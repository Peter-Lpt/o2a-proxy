//! OpenRouter 适配器：GET /api/v1/key → usage / limit（provider_api）。
//!
//! 支持两种语义：
//! - 默认：`/api/v1/key`（API Key usage/limit）
//! - accounts[].quota.mode = "credits"：`/api/v1/credits`（Management Key credits）
//!
//! 统一快照仍为 windows[{kind, unit, used, limit}]。

use std::time::Duration;

use serde_json::{json, Value};

use crate::base::{empty_window, make_snapshot, truthy, QuotaContext, QuotaError, UPSTREAM_TIMEOUT_S};
use crate::registry::QuotaAdapter;

pub struct OpenRouterAdapter;

/// `or` 链取第一个 truthy 值（Python falsy 穿透语义），全 falsy → Null。
pub(crate) fn or_first(vals: Vec<Value>) -> Value {
    vals.into_iter()
        .find(truthy)
        .unwrap_or(Value::Null)
}

#[async_trait::async_trait]
impl QuotaAdapter for OpenRouterAdapter {
    fn name(&self) -> &'static str {
        "openrouter"
    }

    fn source(&self) -> &'static str {
        "provider_api"
    }

    async fn fetch(&self, ctx: &QuotaContext) -> Result<Option<Value>, QuotaError> {
        let Some(session) = &ctx.session else {
            return Ok(None);
        };
        if ctx.account.api_key.is_empty() {
            return Ok(None);
        }
        let cfg = ctx.account.quota.clone().unwrap_or_default();
        let mode = cfg
            .get("mode")
            .and_then(Value::as_str)
            .unwrap_or("key")
            .to_lowercase();
        let path = if mode == "credits" {
            "/api/v1/credits"
        } else {
            "/api/v1/key"
        };
        let base = cfg
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or("https://openrouter.ai")
            .trim_end_matches('/')
            .to_string();
        let url = format!("{}{}", base, path);
        let resp = session
            .get(&url)
            .header("Authorization", format!("Bearer {}", ctx.account.api_key))
            .timeout(Duration::from_secs_f64(UPSTREAM_TIMEOUT_S))
            .send()
            .await
            .map_err(|e| QuotaError::new(format!("openrouter request failed: {}", e)))?;
        let status = resp.status().as_u16();
        if status != 200 {
            return Err(QuotaError::new(format!(
                "openrouter {} status {}",
                path, status
            )));
        }
        let body: Value = resp
            .json()
            .await
            .map_err(|e| QuotaError::new(format!("openrouter request failed: {}", e)))?;
        let data = body.get("data").cloned().unwrap_or(json!({}));
        if !data.is_object() {
            // Python: data.get("usage") 在非 dict 上崩溃 → 注册表降级
            return Err(QuotaError::new("openrouter unexpected payload shape"));
        }
        let usage = data.get("usage").cloned().unwrap_or(Value::Null);
        let limit = data.get("limit").cloned().unwrap_or(Value::Null);
        let (mut usage, mut limit) = (usage, limit);
        if !truthy(&usage) && !truthy(&limit) {
            // credits 响应形态：{credits: {...}} 或 {used_credits, total_credits}
            let credits = data.get("credits").cloned().unwrap_or(Value::Null);
            usage = or_first(vec![
                data.get("used_credits").cloned().unwrap_or(Value::Null),
                credits.get("used").cloned().unwrap_or(Value::Null),
            ]);
            limit = or_first(vec![
                data.get("total_credits").cloned().unwrap_or(Value::Null),
                credits.get("total").cloned().unwrap_or(Value::Null),
            ]);
        }
        if !truthy(&usage) && !truthy(&limit) {
            return Ok(None);
        }
        let used = if truthy(&usage) {
            usage.as_f64().unwrap_or(0.0)
        } else {
            0.0
        };
        let limit_f = if truthy(&limit) {
            limit.as_f64()
        } else {
            None
        };
        let windows = vec![empty_window("month", "usd", used, limit_f, None)];
        let snap = make_snapshot(
            self.name(),
            windows,
            self.source(),
            None,
            false,
            &ctx.now(),
        );
        Ok(Some(snap))
    }
}
