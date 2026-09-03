//! direct 模式 handler（对齐 Python engine.py `handle_direct_stream` /
//! `handle_direct_non_stream` / `upstream_direct_headers`）。
//!
//! Anthropic 原生透传：请求体除 model/max_tokens 契约注入外原样转发，
//! 响应原样返回，旁路抓 usage 记统计（message_start / message_delta 的
//! Anthropic usage、其余事件的 OpenAI usage、prompt_tokens 存在则转换）。

use std::sync::Arc;
use std::time::Instant;

use axum::body::Body;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{json, Value};
use tokio::sync::mpsc;

use o2a_config::Service;
use o2a_convert::{convert_usage, sse_event};

use crate::handlers::error_response;
use crate::proxy::{build_target, stats_meta, STREAM_TIMEOUT};
use crate::state::{classify, ServiceState};

/// 上游请求头（对齐 `upstream_direct_headers`）：
/// Bearer + x-api-key + anthropic-version + 转发客户端 anthropic-beta。
pub fn upstream_direct_headers(svc: &Service, client: Option<&axum::http::HeaderMap>) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
    if let Ok(v) = HeaderValue::from_str(&format!("Bearer {}", svc.api_key())) {
        headers.insert(header::AUTHORIZATION, v);
    }
    if let Ok(v) = HeaderValue::from_str(svc.api_key()) {
        headers.insert(header::HeaderName::from_static("x-api-key"), v);
    }
    headers.insert(
        header::HeaderName::from_static("anthropic-version"),
        HeaderValue::from_static("2023-06-01"),
    );
    // 转发客户端 anthropic-beta（prompt caching / web search 等门控特性）
    if let Some(h) = client {
        if let Some(beta) = h.get("anthropic-beta") {
            headers.insert(header::HeaderName::from_static("anthropic-beta"), beta.clone());
        }
    }
    headers
}

fn merge_usage(latest: &mut Value, u: &Value) {
    if !u.is_object() || u.as_object().is_some_and(|m| m.is_empty()) {
        return;
    }
    if !latest.is_object() {
        *latest = json!({});
    }
    if let (Some(dst), Some(src)) = (latest.as_object_mut(), u.as_object()) {
        for (k, v) in src {
            dst.insert(k.clone(), v.clone());
        }
    }
}

pub async fn direct_stream(
    st: Arc<ServiceState>,
    svc: &Service,
    body: Vec<u8>,
    query: Option<&str>,
    req_start: Instant,
    client_headers: &axum::http::HeaderMap,
) -> Response {
    let target = build_target(svc, query);
    tracing::info!("[FWD][direct] forwarding stream url={target} bytes={}", body.len());

    let up = st
        .client
        .post(&target)
        .headers(upstream_direct_headers(svc, Some(client_headers)))
        .body(body)
        .send()
        .await;
    let mut up = match up {
        Ok(r) => r,
        Err(e) => {
            let err = e.to_string();
            tracing::error!("[direct] upstream failed: {}", &err[..err.len().min(500)]);
            st.stats.record(
                svc,
                &svc.model,
                &json!({}),
                Some("upstream request failed"),
                stats_meta(req_start, None, 0),
            );
            return error_response(
                StatusCode::BAD_GATEWAY,
                &format!("upstream error: {}", &err[..err.len().min(300)]),
            );
        }
    };
    if up.status() != StatusCode::OK {
        let status = up.status();
        // 原样透传上游错误体
        let err = up.text().await.unwrap_or_default();
        tracing::error!("[direct] upstream error {status}: {err}");
        st.stats.record(
            svc,
            &svc.model,
            &json!({}),
            Some(&format!("upstream HTTP {status}")),
            stats_meta(req_start, None, 0),
        );
        return error_response(
            StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY),
            &format!("upstream error: {err}"),
        );
    }

    let (tx, rx) = mpsc::channel::<Result<axum::body::Bytes, std::io::Error>>(32);
    let svc_c = svc.clone();
    let stats = st.stats.clone();
    let task_st = st.clone();

    tokio::spawn(async move {
        {
            let mut t = task_st.task.lock().unwrap();
            t.begin();
        }
        let mut latest_usage: Option<Value> = None;
        let mut stop_reason: Option<String> = None;
        let mut first_chunk_ts: Option<Instant> = None;

        let mut line_buf: Vec<u8> = Vec::new();
        'pump: loop {
            let chunk = match tokio::time::timeout(STREAM_TIMEOUT, up.chunk()).await {
                Err(_) => {
                    let msg = "upstream read timeout".to_string();
                    tracing::error!("[direct] stream error: {msg}");
                    let _ = tx
                        .send(Ok(axum::body::Bytes::from(sse_event(&json!({
                            "type": "error",
                            "error": {"type": "api_error", "message": msg},
                        })))))
                        .await;
                    break 'pump;
                }
                Ok(Ok(None)) => break 'pump,
                Ok(Ok(Some(bytes))) => bytes,
                Ok(Err(e)) => {
                    let msg = e.to_string();
                    tracing::error!("[direct] stream error: {msg}");
                    let _ = tx
                        .send(Ok(axum::body::Bytes::from(sse_event(&json!({
                            "type": "error",
                            "error": {"type": "api_error", "message": msg},
                        })))))
                        .await;
                    break 'pump;
                }
            };
            line_buf.extend_from_slice(&chunk);
            while let Some(pos) = line_buf.iter().position(|&b| b == b'\n') {
                let mut line: Vec<u8> = line_buf.drain(..=pos).collect();
                line.pop();
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                // 原样透传（对齐 Python stream_write(resp, line)）
                let mut out = line.clone();
                out.push(b'\n');
                if tx.send(Ok(axum::body::Bytes::from(out))).await.is_err() {
                    break 'pump; // ClientGone：静默
                }
                let Ok(line) = std::str::from_utf8(&line) else { continue };
                let line = line.trim();
                if !line.starts_with("data:") {
                    continue;
                }
                if first_chunk_ts.is_none() {
                    first_chunk_ts = Some(Instant::now());
                }
                let data = line[5..].trim();
                if data == "[DONE]" {
                    break 'pump;
                }
                let Ok(ev) = serde_json::from_str::<Value>(data) else { continue };
                // Anthropic 全量 usage 在 message_start / message_delta
                match ev.get("type").and_then(|v| v.as_str()) {
                    Some("message_start") => {
                        let u = ev.get("message").cloned().unwrap_or(json!({}));
                        let u = u.get("usage").cloned().unwrap_or(Value::Null);
                        if u.is_object() && !u.as_object().unwrap().is_empty() {
                            latest_usage = Some(u);
                        }
                    }
                    Some("message_delta") => {
                        let u = ev.get("usage").cloned().unwrap_or(Value::Null);
                        if u.is_object() && !u.as_object().unwrap().is_empty() {
                            latest_usage.get_or_insert_with(|| json!({}));
                            if let Some(l) = latest_usage.as_mut() {
                                merge_usage(l, &u);
                            }
                        }
                        if let Some(sr) = ev
                            .get("delta")
                            .and_then(|d| d.get("stop_reason"))
                            .and_then(|v| v.as_str())
                        {
                            stop_reason = Some(sr.to_string());
                        }
                    }
                    // OpenAI 兼容流式：usage 通常出现在最后一个 chunk
                    _ => {
                        if let Some(u) = ev.get("usage") {
                            if u.is_object() && !u.as_object().unwrap().is_empty() {
                                latest_usage.get_or_insert_with(|| json!({}));
                                if let Some(l) = latest_usage.as_mut() {
                                    merge_usage(l, u);
                                }
                            }
                        }
                    }
                }
            }
        }

        // finally：usage 兼容两种格式（prompt_tokens 存在 → 转换）；None → 不记
        if let Some(mut u) = latest_usage {
            let is_openai_style = u.get("prompt_tokens").is_some() || u.get("completion_tokens").is_some();
            if is_openai_style {
                u = convert_usage(Some(&u));
            }
            let out_tokens = u.get("output_tokens").and_then(|v| v.as_i64()).unwrap_or(0);
            stats.record(
                &svc_c,
                &svc_c.model,
                &u,
                None,
                stats_meta(req_start, first_chunk_ts, out_tokens),
            );
        }
        {
            let mut t = task_st.task.lock().unwrap();
            t.finish(classify(stop_reason.as_deref(), false));
            t.end();
        }
    });

    let stream_body = Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(rx));
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, HeaderValue::from_static("text/event-stream; charset=utf-8")),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-cache")),
            (header::HeaderName::from_static("x-accel-buffering"), HeaderValue::from_static("no")),
        ],
        stream_body,
    )
        .into_response()
}

pub async fn direct_non_stream(
    st: Arc<ServiceState>,
    svc: &Service,
    body: Vec<u8>,
    query: Option<&str>,
    req_start: Instant,
    client_headers: &axum::http::HeaderMap,
) -> Response {
    let target = build_target(svc, query);
    tracing::info!(
        "[FWD][direct] forwarding(non-stream) url={target} bytes={} elapsed={:.3}s",
        body.len(),
        req_start.elapsed().as_secs_f64()
    );

    let up = st
        .client
        .post(&target)
        .headers(upstream_direct_headers(svc, Some(client_headers)))
        .body(body)
        .send()
        .await;
    let up = match up {
        Ok(r) => r,
        Err(e) => {
            let err = e.to_string();
            tracing::error!("[direct] upstream failed: {}", &err[..err.len().min(500)]);
            st.stats.record(
                svc,
                &svc.model,
                &json!({}),
                Some("upstream request failed"),
                stats_meta(req_start, None, 0),
            );
            return error_response(
                StatusCode::BAD_GATEWAY,
                &format!("upstream error: {}", &err[..err.len().min(300)]),
            );
        }
    };
    if up.status() != StatusCode::OK {
        let status = up.status();
        let err = up.text().await.unwrap_or_default();
        tracing::error!("[direct] upstream error {status}: {err}");
        st.stats.record(
            svc,
            &svc.model,
            &json!({}),
            Some(&format!("upstream HTTP {status}")),
            stats_meta(req_start, None, 0),
        );
        return error_response(
            StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY),
            &format!("upstream error: {err}"),
        );
    }
    let raw = up.bytes().await.unwrap_or_default();

    // 旁路提取 usage / 任务状态（解析失败不影响原样返回）
    if let Ok(data) = serde_json::from_slice::<Value>(&raw) {
        let model = data
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or(&svc.model)
            .to_string();
        let mut usage = data.get("usage").cloned().unwrap_or(Value::Null);
        // 兼容 Anthropic(input_tokens) 与 OpenAI(prompt_tokens/completion_tokens) 两种 usage 格式
        let is_openai_style =
            usage.get("prompt_tokens").is_some() || usage.get("completion_tokens").is_some();
        if is_openai_style {
            usage = convert_usage(Some(&usage));
        }
        let has_tokens = usage
            .get("input_tokens")
            .and_then(|v| v.as_i64())
            .unwrap_or(0)
            > 0
            || usage
                .get("output_tokens")
                .and_then(|v| v.as_i64())
                .unwrap_or(0)
                > 0;
        if has_tokens {
            st.stats.record(svc, &model, &usage, None, stats_meta(req_start, None, 0));
        }
        // 任务状态：Anthropic stop_reason == end_turn 视为最终答复
        let stop_reason = data.get("stop_reason").and_then(|v| v.as_str());
        let mut t = st.task.lock().unwrap();
        t.finish(classify(stop_reason, false));
    }

    tracing::info!("[direct][nonstream] completed bytes={}", raw.len());
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, HeaderValue::from_static("application/json"))],
        raw,
    )
        .into_response()
}
