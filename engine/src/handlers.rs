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
    // 3) POST /pricing-reload：清定价缓存（下次读取按新 pricing.json 重算）
    if method == Method::POST && path == "/pricing-reload" {
        if let Some(reg) = &st.stats_registry {
            reg.clear_pricing_cache();
        }
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
        return handle_get(&st, &path, req.uri().query());
    }
    // 6) POST 代理分发（claude 全量；codex/direct M4）
    crate::proxy::handle_proxy(st, req).await
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
/// /quota 与 /pricing-meta 属 M5；未知 GET 路径回退根摘要（对齐 Python）。
fn handle_get(st: &ServiceState, path: &str, query: Option<&str>) -> Response {
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
        "/stats" => {
            // 对齐 Python：统计禁用时明确报错（不 501）
            if !o2a_stats::is_cache_stats_enabled() {
                return json_response(
                    &json!({"error": "cache stats is disabled"}),
                    StatusCode::OK,
                );
            }
            let Some(registry) = &st.stats_registry else {
                return openai_error_response(
                    StatusCode::NOT_IMPLEMENTED,
                    "stats registry unavailable",
                );
            };
            let qp = query_params(query);
            let period = qp.get("period").map(String::as_str).unwrap_or("day");
            let svc = st.service.read().unwrap();
            match qp.get("account") {
                // 账号级聚合：归并该账号下所有服务的 summary（load_config 反查，对齐 Python）
                Some(account_id) => {
                    let services = o2a_config::load_config();
                    let refs: Vec<o2a_stats::ServiceSummaryRef> = services
                        .iter()
                        .filter(|s| s.account.id == *account_id)
                        .map(|s| o2a_stats::ServiceSummaryRef {
                            name: s.name.clone(),
                            service_id: s.id.clone(),
                        })
                        .collect();
                    json_response(
                        &o2a_stats::get_account_summary(registry.env(), account_id, &refs, period),
                        StatusCode::OK,
                    )
                }
                None => {
                    let no_cost = svc.pricing_mode != o2a_config::PricingMode::Token;
                    let stats = registry.get(
                        &svc.name,
                        &svc.id,
                        &svc.account.id,
                        &svc.account.name,
                        no_cost,
                    );
                    json_response(&stats.get_summary(period), StatusCode::OK)
                }
            }
        }
        "/quota" | "/pricing-meta" => openai_error_response(
            StatusCode::NOT_IMPLEMENTED,
            "not implemented yet (M5: quota / pricing-meta)",
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

/// 最小查询串解析（k=v&…；值做 %XX 与 '+' 反转义，对齐 request.query 语义）。
fn query_params(query: Option<&str>) -> std::collections::BTreeMap<String, String> {
    fn decode(s: &str) -> String {
        let s = s.replace('+', " ");
        let bytes = s.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'%' && i + 2 < bytes.len() {
                let hex = |b: u8| (b as char).to_digit(16).map(|d| d as u8);
                if let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                    out.push(h * 16 + l);
                    i += 3;
                    continue;
                }
            }
            out.push(bytes[i]);
            i += 1;
        }
        String::from_utf8_lossy(&out).into_owned()
    }
    let mut out = std::collections::BTreeMap::new();
    if let Some(q) = query {
        for pair in q.split('&') {
            if pair.is_empty() {
                continue;
            }
            let (k, v) = match pair.split_once('=') {
                Some((k, v)) => (k, v),
                None => (pair, ""),
            };
            out.insert(decode(k), decode(v));
        }
    }
    out
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
