//! claude 模式 handler（对齐 Python engine.py `handle_claude_stream` /
//! `handle_claude_non_stream`）。
//!
//! 流式：上游 Chat SSE 按行切分 → [`o2a_convert::ChunkTranslator`] 翻译 →
//! 客户端 SSE 逐事件写出。多路收尾（[DONE] / EOF / 总超时）全部幂等。
//! 客户端断连：axum body 接收端被 drop → channel send 失败 → pump 退出并
//! drop 上游响应 → 上游连接取消（对齐 Python ClientGone / CancelledError）。

use std::sync::Arc;
use std::time::Instant;

use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{json, Value};
use tokio::sync::mpsc;

use o2a_config::Service;
use o2a_convert::{anthropic_stop_reason, ChunkTranslator};

use crate::handlers::error_response;
use crate::proxy::{build_target, upstream_headers, StatsMeta, STREAM_TIMEOUT};
use crate::sse_pump::{
    next_chunk, send_stream_error, sse_response, sse_send, split_line, PumpOutcome, SseTx,
};
use crate::state::{classify, ServiceState};

/// 请求截止：总超时按"距请求开始的耗时"判定（对齐 Python `req_elapsed > STREAM_TIMEOUT`），
/// 读间隔另有同值超时兜底（对齐 aiohttp sock_read=STREAM_TIMEOUT）。
struct Clock {
    req_start: Instant,
    first_chunk_ts: Option<Instant>,
}

impl Clock {
    fn new(req_start: Instant) -> Self {
        Self { req_start, first_chunk_ts: None }
    }

    fn note_first_chunk(&mut self) {
        if self.first_chunk_ts.is_none() {
            self.first_chunk_ts = Some(Instant::now());
        }
    }

    fn total_elapsed_exceeded(&self) -> bool {
        self.req_start.elapsed() > STREAM_TIMEOUT
    }

    fn meta(&self, output_tokens: i64) -> StatsMeta {
        crate::proxy::stats_meta(self.req_start, self.first_chunk_ts, output_tokens)
    }
}

/// 流式 handler 主入口。
pub async fn handle_stream(
    st: Arc<ServiceState>,
    svc: &Service,
    openai_req: &Value,
    query: Option<&str>,
    req_start: Instant,
) -> Response {
    let target = build_target(svc, query);
    let body = serde_json::to_vec(openai_req).unwrap_or_default();
    let body_len = body.len();
    tracing::info!(
        "[FWD] forwarding stream model={} url={target} timeout=120s payload_bytes={body_len} elapsed_since_req={:.3}s",
        openai_req.get("model").and_then(|v| v.as_str()).unwrap_or(""),
        req_start.elapsed().as_secs_f64()
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
            tracing::error!("[FWD] Upstream request failed: {}", &err[..err.len().min(500)]);
            tracing::error!("Sent request: {}", crate::proxy::payload_summary(openai_req, body_len));
            st.stats.record(svc,
                openai_req.get("model").and_then(|v| v.as_str()).unwrap_or(&svc.model),
                &json!({}),
                Some("upstream request failed"),
                crate::proxy::stats_meta(req_start, None, 0),
            );
            // 响应头未发出，可正常回 502
            return error_response(
                StatusCode::BAD_GATEWAY,
                &format!("upstream error: {}", &err[..err.len().min(300)]),
            );
        }
    };

    if up.status() != StatusCode::OK {
        let status = up.status();
        let err_body = up.text().await.unwrap_or_default();
        tracing::error!("Upstream error {status}: {err_body}");
        tracing::error!("Sent request: {}", crate::proxy::payload_summary(openai_req, body_len));
        st.stats.record(svc,
            openai_req.get("model").and_then(|v| v.as_str()).unwrap_or(&svc.model),
            &json!({}),
            Some(&format!("upstream HTTP {status}")),
            crate::proxy::stats_meta(req_start, None, 0),
        );
        return error_response(
            StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY),
            &format!("upstream error: {err_body}"),
        );
    }
    tracing::info!("[FWD] upstream connected status=200 connect_time={:.2}s", req_start.elapsed().as_secs_f64());

    // 客户端 SSE 响应通道：接收端被 drop（客户端断连）时 send 失败 → pump 退出
    let (tx, rx) = mpsc::channel::<Result<axum::body::Bytes, std::io::Error>>(32);
    let stats = st.stats.clone();
    let svc_model = svc.model.clone();
    let svc_for_stats = svc.clone();
    let task_st = st.clone();
    let pump_req_start = req_start;

    tokio::spawn(async move {
        {
            task_st.task_begin();
        }
        let mut clock = Clock::new(pump_req_start);
        let mut tr = ChunkTranslator::new(&svc_model);
        let end = pump_loop(up, tx, &mut tr, &mut clock).await;
        // 对齐 Python finally：_task_finish(_classify(pending_finish_reason, had_tool_calls))
        // + _task_end（ClientGone 同样走 finally）
        {
            task_st.task_finish(tr.is_final_answer());
            task_st.task_end();
        }
        match end {
            PumpOutcome::Completed => {
                stats.record(&svc_for_stats, tr.model(), tr.usage(), None, clock.meta(
                    tr.usage().get("output_tokens").and_then(|v| v.as_i64()).unwrap_or(0),
                ));
            }
            PumpOutcome::Error(msg) => {
                stats.record(&svc_for_stats, tr.model(), tr.usage(), Some(&msg), clock.meta(
                    tr.usage().get("output_tokens").and_then(|v| v.as_i64()).unwrap_or(0),
                ));
            }
            PumpOutcome::ClientGone => {}
        }
        tracing::info!(
            "[STREAM] ended finished={} total_elapsed={:.2}s",
            tr.is_finished(),
            pump_req_start.elapsed().as_secs_f64()
        );
    });

    sse_response(rx)
}

/// SSE 行缓冲 + chunk 消费循环（协议差异面：ChunkTranslator 翻译 + Clock 总超时）。
async fn pump_loop(
    mut up: reqwest::Response,
    tx: SseTx,
    tr: &mut ChunkTranslator,
    clock: &mut Clock,
) -> PumpOutcome {
    let mut line_buf: Vec<u8> = Vec::new();
    loop {
        let chunk = match next_chunk(&mut up).await {
            Ok(Some(bytes)) => bytes,
            Ok(None) => {
                // 上游 EOF 未收 [DONE]：补发终止事件，避免客户端挂起
                for ev in tr.on_eof() {
                    if !sse_send(&tx, &ev).await {
                        return PumpOutcome::ClientGone;
                    }
                }
                return PumpOutcome::Completed;
            }
            Err(msg) => {
                // 读间隔超时 / 上游读错误（对齐 Python 循环内 except → 流内 error 事件）
                tracing::error!("Stream error: {msg}");
                if tr.is_started() && !send_stream_error(&tx, &msg).await {
                    return PumpOutcome::ClientGone;
                }
                return PumpOutcome::Error(msg);
            }
        };
        clock.note_first_chunk();

        // 总超时：关全部开块 → 已开始则 max_tokens 停止 + message_stop（对齐
        // Python 循环顶部的 req_elapsed 判定，graceful 而非 error）
        if clock.total_elapsed_exceeded() {
            tracing::warn!(
                "[STREAM] timeout after {:.1}s (limit={}s)",
                clock.req_start.elapsed().as_secs_f64(),
                STREAM_TIMEOUT.as_secs()
            );
            for ev in tr.on_timeout() {
                if !sse_send(&tx, &ev).await {
                    return PumpOutcome::ClientGone;
                }
            }
            return PumpOutcome::Completed;
        }

        line_buf.extend_from_slice(&chunk);
        while let Some(line) = split_line(&mut line_buf) {
            let Ok(line) = std::str::from_utf8(&line) else { continue };
            let line = line.trim();
            if !line.starts_with("data:") {
                continue;
            }
            let data = line[5..].trim();
            if data == "[DONE]" {
                for ev in tr.on_done() {
                    if !sse_send(&tx, &ev).await {
                        return PumpOutcome::ClientGone;
                    }
                }
                return PumpOutcome::Completed;
            }
            if data.is_empty() {
                continue;
            }
            let Ok(chunk) = serde_json::from_str::<Value>(data) else { continue };
            for ev in tr.on_chunk(&chunk) {
                if !sse_send(&tx, &ev).await {
                    return PumpOutcome::ClientGone;
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 非流式（对齐 handle_claude_non_stream）
// ---------------------------------------------------------------------------

pub async fn handle_non_stream(
    st: Arc<ServiceState>,
    svc: &Service,
    openai_req: &Value,
    query: Option<&str>,
    req_start: Instant,
) -> Response {
    let target = build_target(svc, query);
    let body = serde_json::to_vec(openai_req).unwrap_or_default();
    let body_len = body.len();
    tracing::info!(
        "[FWD] forwarding(non-stream) model={} url={target} timeout=120s payload_bytes={body_len} elapsed_since_req={:.3}s",
        openai_req.get("model").and_then(|v| v.as_str()).unwrap_or(""),
        req_start.elapsed().as_secs_f64()
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
            tracing::error!("[FWD] Upstream request failed: {}", &err[..err.len().min(500)]);
            tracing::error!("Sent request: {}", crate::proxy::payload_summary(openai_req, body_len));
            st.stats.record(
                svc,
                openai_req.get("model").and_then(|v| v.as_str()).unwrap_or(&svc.model),
                &json!({}),
                Some("upstream request failed"),
                crate::proxy::stats_meta(req_start, None, 0),
            );
            return error_response(
                StatusCode::BAD_GATEWAY,
                &format!("upstream error: {}", &err[..err.len().min(300)]),
            );
        }
    };

    if up.status() != StatusCode::OK {
        let status = up.status();
        let err_body = up.text().await.unwrap_or_default();
        tracing::error!("Upstream error {status}: {err_body}");
        tracing::error!("Sent request: {}", crate::proxy::payload_summary(openai_req, body_len));
        st.stats.record(
            svc,
            openai_req.get("model").and_then(|v| v.as_str()).unwrap_or(&svc.model),
            &json!({}),
            Some(&format!("upstream HTTP {status}")),
            crate::proxy::stats_meta(req_start, None, 0),
        );
        return error_response(
            StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY),
            &format!("upstream error: {err_body}"),
        );
    }
    let raw_text = up.text().await.unwrap_or_default();
    let Ok(raw) = serde_json::from_str::<Value>(&raw_text) else {
        return error_response(StatusCode::BAD_GATEWAY, "upstream error: invalid json body");
    };

    tracing::info!(
        "[NONSTREAM] completed elapsed={:.2}s model={} usage={}",
        req_start.elapsed().as_secs_f64(),
        raw.get("model").and_then(|v| v.as_str()).unwrap_or(""),
        raw.get("usage").map(|v| v.to_string()).unwrap_or_default()
    );

    let mut content = String::new();
    let mut finish_reason = "stop";
    let mut tool_calls: Vec<Value> = Vec::new();
    let mut reasoning_content = String::new();
    if let Some(choice) = raw.get("choices").and_then(|v| v.as_array()).and_then(|a| a.first()) {
        let message = choice.get("message").cloned().unwrap_or(json!({}));
        if let Some(c) = message.get("content").and_then(|v| v.as_str()) {
            content = c.to_string();
        }
        if let Some(fr) = choice.get("finish_reason").and_then(|v| v.as_str()) {
            if !fr.is_empty() {
                finish_reason = fr;
            }
        }
        if let Some(rc) = message.get("reasoning_content").and_then(|v| v.as_str()) {
            reasoning_content = rc.to_string();
        }
        if let Some(tcs) = message.get("tool_calls").and_then(|v| v.as_array()) {
            for tc in tcs {
                let args_raw = tc
                    .get("function")
                    .and_then(|f| f.get("arguments"))
                    .cloned()
                    .unwrap_or(json!("{}"));
                // 对齐 Python：arguments 解析失败时保留原始字符串
                let input = match &args_raw {
                    Value::String(s) => serde_json::from_str::<Value>(s)
                        .unwrap_or_else(|_| Value::String(s.clone())),
                    other => other.clone(),
                };
                tool_calls.push(json!({
                    "type": "tool_use",
                    "id": tc.get("id").and_then(|v| v.as_str()).unwrap_or(""),
                    "name": tc.get("function").and_then(|f| f.get("name")).and_then(|v| v.as_str()).unwrap_or(""),
                    "input": input,
                }));
            }
        }
    }

    let converted = o2a_convert::convert_usage(raw.get("usage"));
    let usage_body = json!({
        "input_tokens": converted["input_tokens"],
        "output_tokens": converted["output_tokens"],
        "cache_creation_input_tokens": converted["cache_creation_input_tokens"],
        "cache_read_input_tokens": converted["cache_read_input_tokens"],
    });

    let mut content_list: Vec<Value> = Vec::new();
    if !reasoning_content.is_empty() {
        content_list.push(json!({"type": "thinking", "thinking": reasoning_content}));
    }
    if !content.is_empty() {
        content_list.push(json!({"type": "text", "text": content}));
    }
    content_list.extend(tool_calls.iter().cloned());

    let has_tool = !tool_calls.is_empty();
    let stop_reason = anthropic_stop_reason(Some(finish_reason), has_tool);
    // 任务状态（对齐 Python：非流式单次响应，finish_reason 判定）
    {
        st.task_finish(classify(Some(finish_reason), has_tool));
    }
    // 统计（对齐 record_stats：usage 全量 + req_start 计时；模型取 raw.model）
    let stats_model = raw
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or(&svc.model)
        .to_string();
    st.stats.record(
        svc,
        &stats_model,
        &json!({
            "input_tokens": converted["input_tokens"],
            "output_tokens": converted["output_tokens"],
            "cache_read_input_tokens": converted["cache_read_input_tokens"],
            "cache_creation_input_tokens": converted["cache_creation_input_tokens"],
            "reasoning_tokens": converted.get("reasoning_tokens").cloned().unwrap_or(json!(0)),
        }),
        None,
        crate::proxy::stats_meta(
            req_start,
            None,
            converted.get("output_tokens").and_then(|v| v.as_i64()).unwrap_or(0),
        ),
    );

    (
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, HeaderValue::from_static("application/json"))],
        serde_json::to_vec(&json!({
            "id": raw.get("id").and_then(|v| v.as_str()).unwrap_or("proxy-msg"),
            "type": "message",
            "role": "assistant",
            "content": content_list,
            "model": raw.get("model").and_then(|v| v.as_str()).unwrap_or(&svc.model),
            "stop_reason": stop_reason,
            "stop_sequence": null,
            "usage": usage_body,
        }))
        .unwrap_or_default(),
    )
        .into_response()
}
