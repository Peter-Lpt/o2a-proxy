//! 路由与 GET 端点（对齐 Python engine.py `handle_request` 前半段 + `handle_get` +
//! `_model_entries` / `_model_entry`）。
//!
//! M2 范围：鉴权 → /_reload / /pricing-reload → 503 reloading → GET 端点；
//! POST 代理分发（claude/codex/direct）为 501 占位，M3/M4 填充。

use std::sync::Arc;

use axum::extract::{DefaultBodyLimit, Request, State};
use axum::http::{header, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::Router;
use serde_json::{json, Value};

use crate::auth;
use crate::reload;
use crate::state::{ServiceState, MAX_BODY_SIZE};

pub fn build_router(st: Arc<ServiceState>) -> Router {
    Router::new()
        .route("/", any(handle_any))
        .route("/{*tail}", any(handle_any))
        .layer(DefaultBodyLimit::max(MAX_BODY_SIZE))
        .with_state(st)
}

pub fn json_response(v: &Value, status: StatusCode) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "application/json")],
        serde_json::to_vec(v).unwrap_or_default(),
    )
        .into_response()
}

/// Anthropic 风格错误体（对齐 `error_response`；M3 流式/上游错误使用）。
#[allow(dead_code)]
pub fn error_response(status: StatusCode, message: &str) -> Response {
    json_response(
        &json!({"type": "error", "error": {"type": "api_error", "message": message}}),
        status,
    )
}

/// OpenAI 风格错误体（对齐 `openai_error_response`）。
pub fn openai_error_response(status: StatusCode, message: &str) -> Response {
    json_response(&json!({"error": {"message": message, "type": "api_error"}}), status)
}

async fn handle_any(State(st): State<Arc<ServiceState>>, req: Request) -> Response {
    let path = req.uri().path().to_string();
    let method = req.method().clone();

    // 1) 鉴权（对齐 Python 顺序：鉴权先于 /_reload / 503）
    if !auth::check(&st, req.headers(), &path) {
        return auth::error_response();
    }
    // 2) POST /_reload：触发热重载（异步执行，立即返回）
    if method == Method::POST && path == "/_reload" {
        return match st.engine.as_ref().and_then(|w| w.upgrade()) {
            Some(engine) => {
                tokio::spawn(reload::do_reload(engine));
                json_response(&json!({"status": "reloading"}), StatusCode::OK)
            }
            None => json_response(&json!({"error": "reload not supported"}), StatusCode::OK),
        };
    }
    // 3) POST /pricing-reload：清定价缓存（o2a-stats 接入后生效，M4/M5）
    if method == Method::POST && path == "/pricing-reload" {
        tracing::info!("[pricing] pricing-reload 收到（缓存清理在 stats 接入后生效）");
        return json_response(&json!({"status": "pricing reloaded"}), StatusCode::OK);
    }
    // 4) 重载期间请求明确 503 + Retry-After（/health 保持探活）。
    //    引擎级标记（对齐 Python 进程级 `_O2A_RELOADING`；测试直连 Router 无引擎时视为未重载）。
    let engine_reloading = st
        .engine
        .as_ref()
        .and_then(|w| w.upgrade())
        .map(|e| e.reloading.active())
        .unwrap_or(false);
    if engine_reloading && path != "/health" {
        let mut resp = json_response(
            &json!({"error": {"type": "api_error", "message": "service is reloading, retry shortly"}}),
            StatusCode::SERVICE_UNAVAILABLE,
        );
        resp.headers_mut()
            .insert(header::RETRY_AFTER, HeaderValue::from_static("2"));
        return resp;
    }
    // 5) GET 端点
    if method == Method::GET {
        return handle_get(&st, &path);
    }
    // 6) POST 代理分发：M3/M4 填充（claude / codex / direct）
    openai_error_response(
        StatusCode::NOT_IMPLEMENTED,
        "proxy dispatch not implemented yet (M3/M4)",
    )
}

fn client_str(c: o2a_config::ClientKind) -> &'static str {
    match c {
        o2a_config::ClientKind::Anthropic => "anthropic",
        o2a_config::ClientKind::Openai => "openai",
        o2a_config::ClientKind::Auto => "auto",
    }
}

/// GET 端点分发（对齐 `handle_get`）。
///
/// 与 Python 的差异：/stats、/quota、/pricing-meta 属 M4/M5（统计与额度接入），
/// 当前显式 501；Python 中未知的 GET 路径回退根摘要，此处保持一致。
fn handle_get(st: &ServiceState, path: &str) -> Response {
    let svc = st.service.read().unwrap();
    match path {
        "/models" | "/v1/models" => json_response(
            &json!({"object": "list", "data": model_entries(&svc)}),
            StatusCode::OK,
        ),
        "/status" => {
            let t = st.task.lock().unwrap();
            json_response(
                &json!({
                    "active": t.active(),
                    "active_streams": t.active_streams,
                    "last_finish": t.last_finish,
                    "last_activity": t.last_activity,
                    "service": svc.name,
                    "port": svc.port,
                    "mode": svc.mode().as_str(),
                }),
                StatusCode::OK,
            )
        }
        "/stats" | "/quota" | "/pricing-meta" => openai_error_response(
            StatusCode::NOT_IMPLEMENTED,
            "not implemented yet (M4/M5: stats / quota / pricing-meta)",
        ),
        _ => json_response(
            &json!({
                "status": "ok",
                "mode": svc.mode().as_str(),
                "client": client_str(svc.client),
                "account": svc.account.name,
                "target": svc.target_url(),
                "endpoints": [
                    "/stats?period=hour|day|all",
                    "/stats?account=<账号id>&period=day",
                    "/quota?account=<账号id>",
                    "/health",
                    "/status",
                ],
            }),
            StatusCode::OK,
        ),
    }
}

/// 单条模型条目（对齐 `_model_entry`）：无 default 键（override=true 时列表固定一条）。
pub fn model_entry(svc: &o2a_config::Service) -> Value {
    json!({
        "id": svc.model,
        "object": "model",
        "created": 0,
        "owned_by": svc.account.name,
        "context": svc.max_tokens,
        "required": svc.override_model,
    })
}

/// /v1/models 输出全集（对齐 `_model_entries` 白名单/别名矩阵）。
///
/// - 白名单为空 → 单条（无 default 键）
/// - 非空 → 白名单全集 + 别名键（上游名不暴露），主模型恒在列（不在白名单且
///   不是别名目标时补入首条）；required 仅 override=true 且为主模型时标记
pub fn model_entries(svc: &o2a_config::Service) -> Vec<Value> {
    let mut names: Vec<String> = svc.models.clone();
    for k in svc.models_map.keys() {
        if !names.contains(k) {
            names.push(k.clone());
        }
    }
    if names.is_empty() {
        return vec![model_entry(svc)];
    }
    let main = svc.model.clone();
    let upstream_values: std::collections::HashSet<&String> =
        svc.models_map.0.iter().map(|(_, v)| v).collect();
    if !main.is_empty() && !names.contains(&main) && !upstream_values.contains(&main) {
        names.insert(0, main.clone());
    }
    names
        .iter()
        .map(|m| {
            json!({
                "id": m,
                "object": "model",
                "created": 0,
                "owned_by": svc.account.name,
                "context": svc.max_tokens,
                "required": svc.override_model && m == &main,
                "default": m == &main,
            })
        })
        .collect()
}
