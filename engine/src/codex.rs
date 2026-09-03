//! codex 模式 handler（对齐 Python engine.py `handle_passthrough` /
//! `handle_openai_stream` / `handle_openai_non_stream`）。
//!
//! 三条路径：
//! - Chat 整包透传（api=openai-completions）：不重建请求体，仅 model 注入
//! - Responses 整包透传（api=responses + upstream=responses）：usage 从
//!   response.completed 事件提取
//! - Responses → Chat 转换（含 Chat 入直通分支）：流式按入参方向分派
//!   （Responses 入走 ResponsesStreamTranslator / Chat 入 SSE 原样透传）

use std::sync::Arc;
use std::time::Instant;

use axum::body::Body;
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{json, Value};
use tokio::sync::mpsc;

use o2a_config::Service;
use o2a_convert::{chat_to_responses_json, convert_usage, ResponsesStreamTranslator, sse_event};

use crate::handlers::openai_error_response;
use crate::proxy::{build_target, stats_meta, upstream_headers, STREAM_TIMEOUT};
use crate::state::{classify, py_truthy, ServiceState};

/// Python truthiness 局部实现（bool(req.get("input")) 等）。
fn truthy(v: Option<&Value>) -> bool {
    v.map(py_truthy).unwrap_or(false)
}

/// codex 请求的统计模型名（对齐 Python：override ? svc.model : req.model or svc.model）。
fn stats_model(svc: &Service, req: &Value) -> String {
    if svc.override_model {
        return svc.model.clone();
    }
    req.get("model")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(&svc.model)
        .to_string()
}

// ---------------------------------------------------------------------------
// 整包透传（Chat / Responses 共用；对齐 handle_passthrough）
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
pub async fn passthrough(
    st: Arc<ServiceState>,
    svc: &Service,
    body: Vec<u8>,
    stream: bool,
    req_start: Instant,
    target_override: Option<String>,
    responses_usage: bool,
    query: Option<&str>,
) -> Response {
    // model 注入（对齐 Python：解析 → 注入 → 仅缺失/需覆盖时重序列化）
    let mut payload: Value = match serde_json::from_slice(&body) {
        Ok(p) => p,
        Err(_) => return openai_error_response(StatusCode::BAD_REQUEST, "invalid json"),
    };
    let model: String;
    let mut body = body;
    if svc.override_model {
        let cur = payload.get("model").and_then(|v| v.as_str()).unwrap_or("");
        if cur != svc.model {
            if let Some(obj) = payload.as_object_mut() {
                obj.insert("model".into(), json!(svc.model));
            }
            body = serde_json::to_vec(&payload).unwrap_or(body);
        }
        model = svc.model.clone();
    } else {
        let has_model = payload
            .get("model")
            .and_then(|v| v.as_str())
            .map(|s| !s.is_empty())
            .unwrap_or(false);
        if !has_model && !svc.model.is_empty() {
            if let Some(obj) = payload.as_object_mut() {
                obj.insert("model".into(), json!(svc.model));
            }
            body = serde_json::to_vec(&payload).unwrap_or(body);
        }
        model = payload
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or(&svc.model)
            .to_string();
    }

    let target = target_override.unwrap_or_else(|| build_target(svc, query));
    tracing::info!(
        "[FWD][passthrough] stream={stream} model={model} url={target} payload_bytes={}",
        body.len()
    );

    let up = st
        .client
        .post(&target)
        .headers(upstream_headers(svc))
        .body(body)
        .send()
        .await;
    let up = match up {
        Ok(r) => r,
        Err(e) => {
            let err = e.to_string();
            tracing::error!("[passthrough] upstream failed: {}", &err[..err.len().min(500)]);
            st.stats.record(
                svc,
                &model,
                &json!({}),
                Some("upstream request failed"),
                stats_meta(req_start, None, 0),
            );
            return openai_error_response(
                StatusCode::BAD_GATEWAY,
                &format!("upstream error: {}", &err[..err.len().min(300)]),
            );
        }
    };
    if up.status() != StatusCode::OK {
        let status = up.status();
        // 原样透传上游错误体，避免丢失 type/code/param（如 429 Retry-After）
        let err = up.text().await.unwrap_or_default();
        st.stats.record(
            svc,
            &model,
            &json!({}),
            Some(&format!("upstream HTTP {status}")),
            stats_meta(req_start, None, 0),
        );
        return openai_error_response(
            StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY),
            &format!("upstream error: {err}"),
        );
    }

    if !stream {
        let raw = up.bytes().await.unwrap_or_default();
        // 旁路提取 usage / 任务状态（解析失败不影响原样返回）
        if let Ok(data) = serde_json::from_slice::<Value>(&raw) {
            let converted = convert_usage(data.get("usage"));
            if converted.get("input_tokens").and_then(|v| v.as_i64()).unwrap_or(0) > 0 {
                st.stats.record(
                    svc,
                    data.get("model").and_then(|v| v.as_str()).unwrap_or(&model),
                    &converted,
                    None,
                    stats_meta(req_start, None, 0),
                );
            }
            let choice = data
                .get("choices")
                .and_then(|v| v.as_array())
                .and_then(|a| a.first())
                .cloned()
                .unwrap_or(json!({}));
            let fr = choice.get("finish_reason").and_then(|v| v.as_str());
            let has_tool = choice
                .get("message")
                .and_then(|m| m.get("tool_calls"))
                .and_then(|v| v.as_array())
                .is_some_and(|a| !a.is_empty());
            let mut t = st.task.lock().unwrap();
            t.finish(classify(fr, has_tool));
        }
        tracing::info!("[passthrough] completed bytes={}", raw.len());
        return (
            StatusCode::OK,
            [(header::CONTENT_TYPE, HeaderValue::from_static("application/json"))],
            raw,
        )
            .into_response();
    }

    // 流式：逐行原样透传（含非 data 行），旁路抓 usage / 任务状态
    let (tx, rx) = mpsc::channel::<Result<axum::body::Bytes, std::io::Error>>(32);
    let svc_c = svc.clone();
    let stats = st.stats.clone();
    let task_st = st.clone();
    let model_c = model;

    tokio::spawn(async move {
        {
            let mut t = task_st.task.lock().unwrap();
            t.begin();
        }
        let mut latest_usage: Option<Value> = None;
        let mut finish_reason: Option<String> = None;
        let mut first_chunk_ts: Option<Instant> = None;
        pump_passthrough(
            up,
            &tx,
            responses_usage,
            &mut latest_usage,
            &mut finish_reason,
            &mut first_chunk_ts,
            &task_st,
        )
        .await;
        // 对齐 Python finally：ClientGone / Error / Done 三路都记统计
        stats.record(
            &svc_c,
            &model_c,
            latest_usage.as_ref().unwrap_or(&json!({})),
            None,
            stats_meta(
                req_start,
                first_chunk_ts,
                latest_usage
                    .as_ref()
                    .and_then(|u| u.get("output_tokens"))
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0),
            ),
        );
        // responses_usage 的任务状态在 response.completed 事件内判定（对齐 Python）
        if !responses_usage {
            let mut t = task_st.task.lock().unwrap();
            t.finish(classify(finish_reason.as_deref(), false));
        }
        let mut t = task_st.task.lock().unwrap();
        t.end();
    });

    sse_response(rx)
}

/// passthrough 泵：逐行转发 + 旁路提取。返回 ()（统计语义在调用方 finally 处理）。
async fn pump_passthrough(
    mut up: reqwest::Response,
    tx: &mpsc::Sender<Result<axum::body::Bytes, std::io::Error>>,
    responses_usage: bool,
    latest_usage: &mut Option<Value>,
    finish_reason: &mut Option<String>,
    first_chunk_ts: &mut Option<Instant>,
    task_st: &Arc<ServiceState>,
) {
    let mut line_buf: Vec<u8> = Vec::new();
    macro_rules! write_raw {
        ($line:expr) => {
            let mut out = $line.to_vec();
            out.push(b'\n');
            if tx.send(Ok(axum::body::Bytes::from(out))).await.is_err() {
                return; // ClientGone：静默退出，取消上游
            }
        };
    }
    loop {
        let chunk = match tokio::time::timeout(STREAM_TIMEOUT, up.chunk()).await {
            Err(_) => {
                let msg = "upstream read timeout".to_string();
                tracing::error!("[passthrough] stream error: {msg}");
                let _ = tx
                    .send(Ok(axum::body::Bytes::from(sse_event(&json!({
                        "type": "error",
                        "error": {"type": "api_error", "message": msg},
                    })))))
                    .await;
                return;
            }
            Ok(Ok(None)) => return,
            Ok(Ok(Some(bytes))) => bytes,
            Ok(Err(e)) => {
                let msg = e.to_string();
                tracing::error!("[passthrough] stream error: {msg}");
                let _ = tx
                    .send(Ok(axum::body::Bytes::from(sse_event(&json!({
                        "type": "error",
                        "error": {"type": "api_error", "message": msg},
                    })))))
                    .await;
                return;
            }
        };
        line_buf.extend_from_slice(&chunk);
        while let Some(pos) = line_buf.iter().position(|&b| b == b'\n') {
            let mut line: Vec<u8> = line_buf.drain(..=pos).collect();
            line.pop(); // \n
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            // 逐行原样转发（含非 data 行），对齐 Python stream_write(resp, line)
            write_raw!(&line);
            let Ok(line) = std::str::from_utf8(&line) else { continue };
            let line = line.trim();
            if !line.starts_with("data:") {
                continue;
            }
            if first_chunk_ts.is_none() {
                *first_chunk_ts = Some(Instant::now());
            }
            let piece = line[5..].trim();
            if piece == "[DONE]" {
                continue;
            }
            if piece.is_empty() {
                continue;
            }
            let Ok(chunk) = serde_json::from_str::<Value>(piece) else { continue };
            if responses_usage {
                // Responses：usage 在 response.completed 事件中
                if chunk.get("type").and_then(|v| v.as_str()) == Some("response.completed") {
                    let resp = chunk.get("response").cloned().unwrap_or(json!({}));
                    let u = resp.get("usage").cloned().unwrap_or(Value::Null);
                    if u.is_object() && !u.as_object().unwrap().is_empty() {
                        *latest_usage = Some(convert_usage(Some(&u)));
                    }
                    // 任务状态：output 无 function_call 视为最终答复
                    let has_tool = resp
                        .get("output")
                        .and_then(|v| v.as_array())
                        .map(|a| a.iter().any(|i| i.get("type").and_then(|t| t.as_str()) == Some("function_call")))
                        .unwrap_or(false);
                    let mut t = task_st.task.lock().unwrap();
                    t.finish(classify(None, has_tool));
                }
            } else {
                if let Some(u) = chunk.get("usage") {
                    if u.is_object() && !u.as_object().unwrap().is_empty() {
                        *latest_usage = Some(convert_usage(Some(u)));
                    }
                }
                // Chat 流式：finish_reason 在最后一个 delta 块出现
                if let Some(fr) = chunk
                    .get("choices")
                    .and_then(|v| v.as_array())
                    .and_then(|a| a.first())
                    .and_then(|c| c.get("finish_reason"))
                    .and_then(|v| v.as_str())
                {
                    *finish_reason = Some(fr.to_string());
                }
            }
        }
    }
}

fn sse_response(rx: mpsc::Receiver<Result<axum::body::Bytes, std::io::Error>>) -> Response {
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

// ---------------------------------------------------------------------------
// Responses → Chat 转换路径（对齐 handle_openai_stream / handle_openai_non_stream）
// ---------------------------------------------------------------------------

pub async fn openai_stream(
    st: Arc<ServiceState>,
    svc: &Service,
    req: &Value,
    chat_body: Vec<u8>,
    query: Option<&str>,
    req_start: Instant,
) -> Response {
    let target = build_target(svc, query);
    tracing::info!(
        "[FWD][codex] forwarding stream model={} url={target} payload_bytes={}",
        svc.model,
        chat_body.len()
    );

    // 请求格式决定响应方向：Responses 入（input 真值）→ 转 Responses 出；
    // Chat Completions 入（messages）→ 上游 SSE 原样透传
    let is_responses = truthy(req.get("input"));
    let model = stats_model(svc, req);

    let up = st
        .client
        .post(&target)
        .headers(upstream_headers(svc))
        .body(chat_body)
        .send()
        .await;
    let up = match up {
        Ok(r) => r,
        Err(e) => {
            let err = e.to_string();
            tracing::error!("[codex] upstream failed: {}", &err[..err.len().min(500)]);
            st.stats.record(
                svc,
                &model,
                &json!({}),
                Some("upstream request failed"),
                stats_meta(req_start, None, 0),
            );
            return openai_error_response(
                StatusCode::BAD_GATEWAY,
                &format!("upstream error: {}", &err[..err.len().min(300)]),
            );
        }
    };
    if up.status() != StatusCode::OK {
        let status = up.status();
        // 原样透传上游错误体，避免丢失 type/code/param（如 429 Retry-After）
        let err = up.text().await.unwrap_or_default();
        st.stats.record(
            svc,
            &model,
            &json!({}),
            Some(&format!("upstream HTTP {status}")),
            stats_meta(req_start, None, 0),
        );
        return openai_error_response(
            StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY),
            &format!("upstream error: {err}"),
        );
    }

    let (tx, rx) = mpsc::channel::<Result<axum::body::Bytes, std::io::Error>>(32);
    let svc_c = svc.clone();
    let stats = st.stats.clone();
    let task_st = st.clone();
    let model_c = model;

    tokio::spawn(async move {
        {
            let mut t = task_st.task.lock().unwrap();
            t.begin();
        }
        let mut translator = is_responses.then(|| ResponsesStreamTranslator::new(&model_c));
        let mut latest_usage: Option<Value> = None;
        let mut finish_reason: Option<String> = None;
        let mut first_chunk_ts: Option<Instant> = None;
        let mut had_tool = false;

        let client_gone = pump_openai(
            up,
            &tx,
            is_responses,
            translator.as_mut(),
            &mut latest_usage,
            &mut finish_reason,
            &mut first_chunk_ts,
            &mut had_tool,
            &task_st,
        )
        .await;

        // finally：统计 + Chat 入的任务状态收尾
        // ClientGone 同样记统计（对齐 Python finally 语义）
        let out_tokens = latest_usage
            .as_ref()
            .and_then(|u| u.get("output_tokens"))
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        stats.record(
            &svc_c,
            &model_c,
            latest_usage.as_ref().unwrap_or(&json!({})),
            None,
            stats_meta(req_start, first_chunk_ts, out_tokens),
        );
        if !is_responses {
            let mut t = task_st.task.lock().unwrap();
            t.finish(classify(finish_reason.as_deref(), false));
        }
        let mut t = task_st.task.lock().unwrap();
        t.end();
        let _ = client_gone;
    });

    sse_response(rx)
}

/// openai_stream 泵：Responses 入走翻译器 / Chat 入逐行透传。
/// 返回是否客户端断连（统计语义在调用方 finally 统一处理）。
/// 参数与 Python 循环内局部变量一一对应，聚合结构体反而增加间接层。
#[allow(clippy::too_many_arguments)]
async fn pump_openai(
    mut up: reqwest::Response,
    tx: &mpsc::Sender<Result<axum::body::Bytes, std::io::Error>>,
    is_responses: bool,
    mut translator: Option<&mut ResponsesStreamTranslator>,
    latest_usage: &mut Option<Value>,
    finish_reason: &mut Option<String>,
    first_chunk_ts: &mut Option<Instant>,
    had_tool: &mut bool,
    task_st: &Arc<ServiceState>,
) -> bool {
    async fn try_send(
        tx: &mpsc::Sender<Result<axum::body::Bytes, std::io::Error>>,
        ev: &Value,
    ) -> bool {
        tx.send(Ok(axum::body::Bytes::from(sse_event(ev))))
            .await
            .is_ok()
    }
    async fn send_error(tx: &mpsc::Sender<Result<axum::body::Bytes, std::io::Error>>, msg: &str) {
        let _ = try_send(
            tx,
            &json!({"type": "error", "error": {"type": "api_error", "message": msg}}),
        )
        .await;
    }

    let mut line_buf: Vec<u8> = Vec::new();
    loop {
        let chunk = match tokio::time::timeout(STREAM_TIMEOUT, up.chunk()).await {
            Err(_) => {
                let msg = "upstream read timeout".to_string();
                tracing::error!("[codex] stream error: {msg}");
                send_error(tx, &msg).await;
                return false;
            }
            Ok(Ok(None)) => {
                // EOF 兜底：补发剩余事件 + response.completed，避免客户端挂起
                if let Some(tr) = translator {
                    for ev in tr.finish() {
                        if ev.get("type").and_then(|v| v.as_str()) == Some("response.output_item.added")
                            && ev["item"]["type"] == json!("function_call")
                        {
                            *had_tool = true;
                        }
                        if !try_send(tx, &ev).await {
                            return true;
                        }
                    }
                    let mut t = task_st.task.lock().unwrap();
                    t.finish(classify(None, *had_tool));
                }
                return false;
            }
            Ok(Ok(Some(bytes))) => bytes,
            Ok(Err(e)) => {
                let msg = e.to_string();
                tracing::error!("[codex] stream error: {msg}");
                send_error(tx, &msg).await;
                return false;
            }
        };
        line_buf.extend_from_slice(&chunk);
        while let Some(pos) = line_buf.iter().position(|&b| b == b'\n') {
            let mut line: Vec<u8> = line_buf.drain(..=pos).collect();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            let Ok(line) = std::str::from_utf8(&line) else { continue };
            let line = line.trim();
            if !is_responses {
                // Chat 直通：原样转发每行（含非 data 行，对齐 Python stream_write(resp, line)），
                // 仅旁路抓 usage / finish_reason
                let mut out = line.as_bytes().to_vec();
                out.push(b'\n');
                if tx.send(Ok(axum::body::Bytes::from(out))).await.is_err() {
                    return true;
                }
                if !line.starts_with("data:") {
                    continue;
                }
                if first_chunk_ts.is_none() {
                    *first_chunk_ts = Some(Instant::now());
                }
                let data = line[5..].trim();
                if data == "[DONE]" || data.is_empty() {
                    continue;
                }
                if let Ok(chunk) = serde_json::from_str::<Value>(data) {
                    if let Some(u) = chunk.get("usage") {
                        if u.is_object() && !u.as_object().unwrap().is_empty() {
                            *latest_usage = Some(convert_usage(Some(u)));
                        }
                    }
                    if let Some(fr) = chunk
                        .get("choices")
                        .and_then(|v| v.as_array())
                        .and_then(|a| a.first())
                        .and_then(|c| c.get("finish_reason"))
                        .and_then(|v| v.as_str())
                    {
                        *finish_reason = Some(fr.to_string());
                    }
                }
                continue;
            }
            if !line.starts_with("data:") {
                continue;
            }
            if first_chunk_ts.is_none() {
                *first_chunk_ts = Some(Instant::now());
            }
            let data = line[5..].trim();
            let Some(tr) = translator.as_deref_mut() else { continue };
            if data == "[DONE]" {
                // 流结束：补发 done 事件 + response.completed（幂等，usage 尾块已到达）
                for ev in tr.finish() {
                    if ev.get("type").and_then(|v| v.as_str()) == Some("response.output_item.added")
                        && ev["item"]["type"] == json!("function_call")
                    {
                        *had_tool = true;
                    }
                    if !try_send(tx, &ev).await {
                        return true;
                    }
                }
                let mut t = task_st.task.lock().unwrap();
                t.finish(classify(None, *had_tool));
                return false;
            }
            if data.is_empty() {
                continue;
            }
            let Ok(chunk) = serde_json::from_str::<Value>(data) else { continue };
            if let Some(u) = chunk.get("usage") {
                if u.is_object() && !u.as_object().unwrap().is_empty() {
                    *latest_usage = Some(convert_usage(Some(u)));
                }
            }
            // Responses 流：response.completed 事件判定是否最终
            if chunk.get("type").and_then(|v| v.as_str()) == Some("response.completed") {
                let has_tool = chunk["response"]["output"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .any(|i| i.get("type").and_then(|t| t.as_str()) == Some("function_call"))
                    })
                    .unwrap_or(false);
                let mut t = task_st.task.lock().unwrap();
                t.finish(classify(None, has_tool));
            }
            for ev in tr.translate(&chunk) {
                if ev.get("type").and_then(|v| v.as_str()) == Some("response.output_item.added")
                    && ev["item"]["type"] == json!("function_call")
                {
                    *had_tool = true;
                }
                if !try_send(tx, &ev).await {
                    return true;
                }
            }
        }
    }
}

pub async fn openai_non_stream(
    st: Arc<ServiceState>,
    svc: &Service,
    req: &Value,
    chat_body: Vec<u8>,
    query: Option<&str>,
    req_start: Instant,
) -> Response {
    let target = build_target(svc, query);
    let model = stats_model(svc, req);
    tracing::info!(
        "[FWD][codex] forwarding(non-stream) model={model} url={target} payload_bytes={}",
        chat_body.len()
    );

    let up = st
        .client
        .post(&target)
        .headers(upstream_headers(svc))
        .body(chat_body)
        .send()
        .await;
    let up = match up {
        Ok(r) => r,
        Err(e) => {
            let err = e.to_string();
            tracing::error!("[codex] upstream failed: {}", &err[..err.len().min(500)]);
            st.stats.record(
                svc,
                &model,
                &json!({}),
                Some("upstream request failed"),
                stats_meta(req_start, None, 0),
            );
            return openai_error_response(
                StatusCode::BAD_GATEWAY,
                &format!("upstream error: {}", &err[..err.len().min(300)]),
            );
        }
    };
    if up.status() != StatusCode::OK {
        let status = up.status();
        let err = up.text().await.unwrap_or_default();
        st.stats.record(
            svc,
            &model,
            &json!({}),
            Some(&format!("upstream HTTP {status}")),
            stats_meta(req_start, None, 0),
        );
        return openai_error_response(
            StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY),
            &format!("upstream error: {err}"),
        );
    }
    let raw = up.bytes().await.unwrap_or_default();

    let is_responses = truthy(req.get("input"));
    let parsed = serde_json::from_slice::<Value>(&raw);
    match parsed {
        Ok(data) => {
            let converted = convert_usage(data.get("usage"));
            if converted.get("input_tokens").and_then(|v| v.as_i64()).unwrap_or(0) > 0 {
                st.stats.record(
                    svc,
                    &model,
                    &converted,
                    None,
                    stats_meta(req_start, None, 0),
                );
            }
            if !is_responses {
                // Chat 直通：上游本就是 chat.completion，原样返回
                let choice = data
                    .get("choices")
                    .and_then(|v| v.as_array())
                    .and_then(|a| a.first())
                    .cloned()
                    .unwrap_or(json!({}));
                let fr = choice.get("finish_reason").and_then(|v| v.as_str());
                let has_tool = choice
                    .get("message")
                    .and_then(|m| m.get("tool_calls"))
                    .and_then(|v| v.as_array())
                    .is_some_and(|a| !a.is_empty());
                let mut t = st.task.lock().unwrap();
                t.finish(classify(fr, has_tool));
                return (
                    StatusCode::OK,
                    [(header::CONTENT_TYPE, HeaderValue::from_static("application/json"))],
                    raw,
                )
                    .into_response();
            }
            let out = chat_to_responses_json(
                &data,
                req.get("model")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .unwrap_or(&svc.model),
            );
            // Responses 转换：output 无 function_call 视为最终答复
            let has_tool = out
                .get("output")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().any(|i| i.get("type").and_then(|t| t.as_str()) == Some("function_call")))
                .unwrap_or(false);
            let mut t = st.task.lock().unwrap();
            t.finish(classify(None, has_tool));
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, HeaderValue::from_static("application/json"))],
                serde_json::to_vec(&out).unwrap_or_default(),
            )
                .into_response()
        }
        Err(e) => {
            if !is_responses {
                // Chat 直通：任何解析异常都原样返回上游响应
                return (
                    StatusCode::OK,
                    [(header::CONTENT_TYPE, HeaderValue::from_static("application/json"))],
                    raw,
                )
                    .into_response();
            }
            // Responses 转换失败：返回明确错误，不能把 Chat 结构错位地当作 Responses 返回
            st.stats.record(
                svc,
                &model,
                &json!({}),
                Some(&format!("response convert failed: {e}")),
                stats_meta(req_start, None, 0),
            );
            openai_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("response convert failed: {e}"),
            )
        }
    }
}

