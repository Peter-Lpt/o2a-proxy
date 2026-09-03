//! POST 代理分发主链路（对齐 Python engine.py `handle_request` 的 POST 段 +
//! `_apply_model_policy` / `record_stats` 占位 / `build_target` / 上游请求头）。
//!
//! M3 范围：claude 模式全量实现（convert_request → 流式/非流式 handler）；
//! codex / direct 分支保持 501 占位（M4 填充），但分发顺序、JSON 解析错误风格、
//! 模式解析与模型策略已完全对齐 Python。

use std::sync::Arc;

use axum::extract::Request;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{json, Value};

use o2a_config::{DispatchMode, ModelPolicy, OpenaiApi, Service, UpstreamApi};
use o2a_convert::{
    convert_request, normalize_roles, resolve_mode, responses_to_chat, strip_cache_control,
};

use crate::claude;
use crate::codex;
use crate::direct;
use crate::handlers::{error_response, openai_error_response};
use crate::state::{ServiceState, MAX_BODY_SIZE};

// 超时常量定义在 state.rs（引擎级共享，ServiceState 构造客户端时使用）
pub use crate::state::STREAM_TIMEOUT;

// ---------------------------------------------------------------------------
// 统计挂点（M4 由 o2a-stats 实现；本任务 no-op）
// ---------------------------------------------------------------------------

/// 请求级统计元信息（对齐 `record_stats` 的 meta 字段）。
#[derive(Debug, Default, Clone)]
pub struct StatsMeta {
    pub duration_ms: Option<f64>,
    pub first_token_ms: Option<f64>,
    pub output_tokens_per_sec: Option<f64>,
}

/// 统计 meta 构造（对齐 record_stats 的 duration/first_token/速度计算；
/// 供 claude/codex/direct 三处流式泵共用）。
pub fn stats_meta(
    req_start: std::time::Instant,
    first_chunk_ts: Option<std::time::Instant>,
    output_tokens: i64,
) -> StatsMeta {
    let duration_ms = Some(req_start.elapsed().as_secs_f64() * 1000.0);
    let first_token_ms = first_chunk_ts
        .map(|t| (t - req_start).as_secs_f64() * 1000.0);
    // 对齐 record_stats：有首 chunk 时间 → 输出速度 = output / 生成秒数；
    // 否则回退总耗时
    let output_tokens_per_sec = match first_chunk_ts {
        Some(t) => {
            let gen = t.elapsed().as_secs_f64();
            (output_tokens > 0 && gen > 0.0).then(|| output_tokens as f64 / gen)
        }
        None => {
            let total = req_start.elapsed().as_secs_f64();
            (output_tokens > 0 && total > 0.0).then(|| output_tokens as f64 / total)
        }
    };
    StatsMeta { duration_ms, first_token_ms, output_tokens_per_sec }
}

/// 统计接收端（对齐 `record_stats`；同步签名，实现内部自行 offload）。
///
/// `model` 是调用点看到的原始模型名（上游名）：对外名/upstream_model 的别名
/// 反查由实现内部完成（对齐 Python record_stats 单一入口）。
pub trait StatsSink: Send + Sync {
    fn record(&self, svc: &Service, model: &str, usage: &Value, error: Option<&str>, meta: StatsMeta);
}

/// no-op 实现（测试注入用；生产路径见 `stats_sink::O2aStatsSink`）。
pub struct NoopSink;

impl StatsSink for NoopSink {
    fn record(&self, _svc: &Service, _model: &str, _usage: &Value, _error: Option<&str>, _meta: StatsMeta) {}
}

/// 统计用的模型别名反查（对齐 `record_stats` 里 `service.reverse_models_map.get`）：
/// 上游名 → 对外名（用户认知名）；未命中保持原样。
pub fn display_model(service: &Service, model: &str) -> String {
    for (alias, upstream) in &service.models_map.0 {
        if upstream == model && !alias.is_empty() {
            return alias.clone();
        }
    }
    model.to_string()
}

// ---------------------------------------------------------------------------
// 上游请求构造（对齐 build_target / upstream_headers / upstream_kwargs）
// ---------------------------------------------------------------------------

/// 拼接上游地址与客户端查询参数（如 ?beta=true）。
pub fn build_target(service: &Service, query: Option<&str>) -> String {
    let target = service.target_url().to_string();
    match query {
        Some(q) if !q.is_empty() => {
            let sep = if target.contains('?') { "&" } else { "?" };
            format!("{target}{sep}{q}")
        }
        _ => target,
    }
}

/// 上游 JSON 请求头（对齐 `upstream_headers`：Content-Type + Bearer）。
pub fn upstream_headers(service: &Service) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    if let Ok(v) = HeaderValue::from_str(&format!("Bearer {}", service.api_key())) {
        headers.insert(header::AUTHORIZATION, v);
    }
    headers
}

/// 请求元信息日志（对齐 `_payload_summary`：只记元数据，不输出对话内容）。
pub(crate) fn payload_summary(openai_request: &Value, body_len: usize) -> String {
    let msgs = openai_request
        .get("messages")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    let tools = openai_request
        .get("tools")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    format!(
        "model={} messages={msgs} tools={tools} has_thinking={} bytes={body_len}",
        openai_request.get("model").and_then(|v| v.as_str()).unwrap_or(""),
        openai_request.get("thinking").is_some(),
    )
}

// ---------------------------------------------------------------------------
// 模型白名单 / 别名映射（对齐 `_apply_model_policy`）
// ---------------------------------------------------------------------------

/// 白名单与别名映射（转换/透传前施加）。返回 Some(resp) 表示直接回该响应。
///
/// - 别名命中（对外名 → 上游名）：重写 payload["model"] 供转发（白名单内外均映射）
/// - 白名单（对外名）非空时：白名单外请求按 model_policy 处理
///   clamp=强转主模型（默认）/ reject=400 列出可用模型 / passthrough=照旧
/// - 主模型恒放行（allowed 集合补入主模型）
/// - 白名单为空：仅做别名映射，其余照旧
pub fn apply_model_policy(service: &Service, payload: &mut Value) -> Option<Response> {
    let req = payload.get("model").and_then(|v| v.as_str()).unwrap_or("");
    let mut allowed: Vec<String> = service.models.clone();
    for k in service.models_map.0.iter().map(|(k, _)| k) {
        if !allowed.contains(k) {
            allowed.push(k.clone());
        }
    }
    let has_whitelist = !allowed.is_empty();
    if has_whitelist && !service.model.is_empty() && !allowed.contains(&service.model) {
        allowed.push(service.model.clone());
    }

    // 别名命中（Python 先于白名单判断）
    if !req.is_empty() {
        if let Some((_, upstream)) = service
            .models_map
            .0
            .iter()
            .find(|(k, _)| k == req)
        {
            if !has_whitelist || allowed.iter().any(|a| a == req) {
                if let Some(obj) = payload.as_object_mut() {
                    obj.insert("model".into(), Value::String(upstream.clone()));
                }
                return None;
            }
        }
    }
    if !has_whitelist {
        return None;
    }
    if allowed.iter().any(|a| a == req) {
        return None;
    }
    // 白名单外
    match service.model_policy {
        ModelPolicy::Reject => {
            let mut sorted = allowed.clone();
            sorted.sort();
            let sample = sorted.into_iter().take(10).collect::<Vec<_>>().join(", ");
            Some(json_response(
                json!({
                    "type": "error",
                    "error": {
                        "type": "invalid_request_error",
                        "message": format!("model '{req}' is not allowed for this service. available models: {sample}"),
                    },
                }),
                StatusCode::BAD_REQUEST,
            ))
        }
        ModelPolicy::Clamp => {
            if let Some(obj) = payload.as_object_mut() {
                obj.insert("model".into(), Value::String(service.model.clone()));
            }
            None
        }
        ModelPolicy::Passthrough => None,
    }
}

fn json_response(v: Value, status: StatusCode) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "application/json")],
        serde_json::to_vec(&v).unwrap_or_default(),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// POST 分发主链路
// ---------------------------------------------------------------------------

pub async fn handle_proxy(st: Arc<ServiceState>, req: Request) -> Response {
    let path = req.uri().path().to_string();
    let query = req.uri().query().map(|q| q.to_string());
    // 客户端请求头（direct 模式转发 anthropic-beta 用；body 消费前先克隆）
    let client_headers = req.headers().clone();
    let req_start = std::time::Instant::now();

    let body: Vec<u8> = match axum::body::to_bytes(req.into_body(), MAX_BODY_SIZE).await {
        Ok(b) => b.into(),
        Err(_) => return error_response(StatusCode::BAD_REQUEST, "read error"),
    };

    // JSON 解析失败的错误风格按服务当前 mode 区分（对齐 Python：
    // `if service.mode == "codex"` → OpenAI 风格，否则 Anthropic 风格）
    let payload: Value = match serde_json::from_slice(&body) {
        Ok(p) => p,
        Err(_) => {
            let mode = st.service.read().unwrap().mode();
            return if mode == DispatchMode::Codex {
                openai_error_response(StatusCode::BAD_REQUEST, "invalid json")
            } else {
                error_response(StatusCode::BAD_REQUEST, "invalid json")
            };
        }
    };

    // 分派模式（api 显式时直接采用推导，不再按请求体识别）
    let locked = st.service.read().unwrap().clone();
    let mode = match resolve_mode(&locked, &path, Some(&payload)) {
        Some(m) => m,
        None => {
            return openai_error_response(
                StatusCode::BAD_REQUEST,
                "该账号没有 OpenAI 端点，无法服务 OpenAI 客户端（Codex）。\
                 请为该账号配置 openai_url，或改用 Claude Code / Anthropic 客户端。",
            )
        }
    };
    // auto 服务：本请求用模式确定的拷贝（Python `service.with_mode(mode)`，不回写共享态）
    let svc = if locked.client == o2a_config::ClientKind::Auto && locked.api.is_none() {
        locked.with_mode(mode)
    } else {
        locked
    };

    // 模型白名单 / 别名映射（所有分派模式统一施加）
    let mut payload = payload;
    if let Some(resp) = apply_model_policy(&svc, &mut payload) {
        return resp;
    }

    match mode {
        DispatchMode::Claude => {
            let stream = payload.get("stream").and_then(|v| v.as_bool()).unwrap_or(false);
            tracing::info!(
                "[REQ] received model={} stream={stream} bytes={} messages={} tools={} has_thinking={} elapsed={:.3}s",
                payload.get("model").and_then(|v| v.as_str()).unwrap_or(""),
                body.len(),
                payload.get("messages").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0),
                payload.get("tools").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0),
                payload.get("thinking").is_some(),
                req_start.elapsed().as_secs_f64(),
            );
            let openai_req = strip_cache_control(&convert_request(&payload, &svc));
            if stream {
                claude::handle_stream(st, &svc, &openai_req, query.as_deref(), req_start).await
            } else {
                claude::handle_non_stream(st, &svc, &openai_req, query.as_deref(), req_start).await
            }
        }
        // M4：codex / direct 分支（分发顺序与上游错误语义对齐 Python engine.py handle_request）
        DispatchMode::Codex => {
            if svc.account.openai_url.is_empty() {
                return openai_error_response(
                    StatusCode::BAD_REQUEST,
                    "该账号没有 OpenAI 端点，无法服务 OpenAI 客户端（Codex）。\
                     请为该账号配置 openai_url，或改用 Claude Code / Anthropic 客户端。",
                );
            }
            let stream = payload.get("stream").and_then(|v| v.as_bool()).unwrap_or(false);
            let req_model = payload
                .get("model")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or(&svc.model);
            let upstream_str = match svc.upstream_api {
                UpstreamApi::OpenaiCompletions => "openai-completions",
                UpstreamApi::OpenaiResponses => "openai-responses",
            };
            tracing::info!(
                "[REQ][codex] model={req_model} stream={stream} api={} upstream={upstream_str} bytes={} elapsed={:.3}s",
                svc.api.map(|_| "set").unwrap_or("auto"),
                body.len(),
                req_start.elapsed().as_secs_f64(),
            );
            // api=openai-completions：Chat 整包透传（直连上游，零转换）
            if svc.api == Some(OpenaiApi::OpenaiCompletions) {
                // developer 角色降级为 system（DeepSeek 等上游不接受 developer）；
                // 未修改时透传原始 body 字节（对齐 Python：别名改写因此仅在该分支
                // normalize_roles 命中或 override=false 且角色有降级时随重序列化生效）
                let mut p = payload;
                let body = if normalize_roles(&mut p) {
                    serde_json::to_vec(&p).unwrap_or(body)
                } else {
                    body
                };
                return codex::passthrough(st, &svc, body, stream, req_start, None, false, query.as_deref()).await;
            }
            // api=openai-responses + upstream=responses：Responses 整包透传（如 DeepSeek 官方）
            if svc.api == Some(OpenaiApi::OpenaiResponses)
                && svc.upstream_api == UpstreamApi::OpenaiResponses
            {
                let target = o2a_config::responses_url(&svc.account.openai_url);
                let mut p = payload;
                let body = if normalize_roles(&mut p) {
                    serde_json::to_vec(&p).unwrap_or(body)
                } else {
                    body
                };
                return codex::passthrough(st, &svc, body, stream, req_start, Some(target), true, None).await;
            }
            // 其余：Responses → Chat 转换（含 Chat 入直通分支）
            let chat = responses_to_chat(&payload, &svc);
            let chat_body = serde_json::to_vec(&chat).unwrap_or_default();
            if stream {
                codex::openai_stream(st, &svc, &payload, chat_body, query.as_deref(), req_start).await
            } else {
                codex::openai_non_stream(st, &svc, &payload, chat_body, query.as_deref(), req_start).await
            }
        }
        DispatchMode::Direct => {
            let stream = payload.get("stream").and_then(|v| v.as_bool()).unwrap_or(false);
            tracing::info!(
                "[REQ][direct] model={} stream={stream} bytes={} elapsed={:.3}s",
                payload.get("model").and_then(|v| v.as_str()).unwrap_or(""),
                body.len(),
                req_start.elapsed().as_secs_f64(),
            );
            // direct 透传也遵循 override_model / max_tokens 默认值契约（对齐 Python）
            let mut p = payload;
            let mut modified = false;
            if svc.override_model
                && p.get("model").cloned().unwrap_or(Value::Null) != json!(svc.model)
            {
                if let Some(obj) = p.as_object_mut() {
                    obj.insert("model".into(), json!(svc.model));
                }
                modified = true;
            }
            let max_tokens_missing = match p.get("max_tokens") {
                None | Some(Value::Null) => true,
                Some(v) => !crate::state::py_truthy(v),
            };
            if max_tokens_missing {
                if let Some(obj) = p.as_object_mut() {
                    obj.insert("max_tokens".into(), json!(svc.max_tokens));
                }
                modified = true;
            }
            let body = if modified {
                serde_json::to_vec(&p).unwrap_or(body)
            } else {
                body
            };
            if stream {
                direct::direct_stream(st, &svc, body, query.as_deref(), req_start, &client_headers).await
            } else {
                direct::direct_non_stream(st, &svc, body, query.as_deref(), req_start, &client_headers).await
            }
        }
        DispatchMode::Auto => {
            // resolve_mode 已把 auto 归约为具体模式；防御分支（不可达）
            openai_error_response(StatusCode::BAD_REQUEST, "invalid json")
        }
    }
}
