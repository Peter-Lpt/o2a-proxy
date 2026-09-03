//! 接入层鉴权（对齐 Python engine.py `_check_auth` / `_extract_client_token` /
//! `auth_error_response`）。
//!
//! - service.auth_token 为空 → 放行（历史行为；引擎启动时另行打安全警告）
//! - `/health` path 精确豁免（不分 HTTP 方法）
//! - 凭证提取：`Authorization: Bearer <token>` 优先，其次 `x-api-key`
//! - 401 错误体同时兼容 Anthropic / OpenAI 两类客户端解析（error.message）

use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::json;

use crate::state::ServiceState;

pub const AUTH_EXEMPT_PATHS: [&str; 1] = ["/health"];

/// 从请求头提取客户端凭证：Bearer 前缀优先（大小写不敏感），其次 x-api-key。
pub fn extract_client_token(headers: &HeaderMap) -> String {
    if let Some(v) = headers.get(axum::http::header::AUTHORIZATION).and_then(|v| v.to_str().ok()) {
        if v.len() >= 7 && v[..7].to_lowercase() == "bearer " {
            return v[7..].trim().to_string();
        }
    }
    headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .trim()
        .to_string()
}

/// 鉴权判定（对齐 `_check_auth`）。token 未配置 → 放行；/health 恒放行。
pub fn check(st: &ServiceState, headers: &HeaderMap, path: &str) -> bool {
    let token = st.service.read().unwrap().auth_token.clone();
    if token.is_empty() {
        return true;
    }
    if AUTH_EXEMPT_PATHS.contains(&path) {
        return true;
    }
    let supplied = extract_client_token(headers);
    !supplied.is_empty() && supplied == token
}

/// 401 凭证错误响应（对齐 `auth_error_response`）。
pub fn error_response() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(
            axum::http::header::CONTENT_TYPE,
            "application/json",
        )],
        serde_json::to_vec(&json!({
            "type": "error",
            "error": {
                "type": "authentication_error",
                "message": "invalid or missing credentials: set Authorization: Bearer <auth_token> or x-api-key header",
            },
        }))
        .unwrap_or_default(),
    )
        .into_response()
}
