//! Z.ai / GLM Coding Plan 适配器：quota + subscription 接口。
//!
//! - 默认 base 从 accounts[].quota.url，缺省 https://open.bigmodel.cn/api/paas/v4
//! - 先尝试 /balance（OpenAI 风格余额），再尝试 /plan（套餐余量）
//! - 响应支持 {"data": {"usage": ..., "limit": ...}} 或 {"usage": ..., "limit": ...}
//! - 网络失败抛 QuotaError → 注册表降级 local 并标 stale

use std::time::Duration;

use serde_json::Value;

use crate::adapters::openrouter::or_first;
use crate::base::{empty_window, make_snapshot, truthy, QuotaContext, QuotaError, UPSTREAM_TIMEOUT_S};
use crate::registry::QuotaAdapter;

pub struct ZaiAdapter;

/// obj = data.data（若为 dict）否则 data 本身；usage/limit 各自 `or` 链 + dict 解包。
fn extract(data: &Value) -> (Value, Value) {
    let obj = match data.get("data") {
        Some(d) if d.is_object() => d.clone(),
        _ => data.clone(),
    };
    let mut usage = or_first(vec![
        obj.get("usage").cloned().unwrap_or(Value::Null),
        obj.get("used_quota").cloned().unwrap_or(Value::Null),
        obj.get("used").cloned().unwrap_or(Value::Null),
    ]);
    let mut limit = or_first(vec![
        obj.get("limit").cloned().unwrap_or(Value::Null),
        obj.get("total_quota").cloned().unwrap_or(Value::Null),
        obj.get("total").cloned().unwrap_or(Value::Null),
    ]);
    if usage.is_object() {
        usage = or_first(vec![
            usage.get("total").cloned().unwrap_or(Value::Null),
            usage.get("used").cloned().unwrap_or(Value::Null),
            usage.get("credits").cloned().unwrap_or(Value::Null),
        ]);
    }
    if limit.is_object() {
        limit = or_first(vec![
            limit.get("total").cloned().unwrap_or(Value::Null),
            limit.get("limit").cloned().unwrap_or(Value::Null),
        ]);
    }
    (usage, limit)
}

#[async_trait::async_trait]
impl QuotaAdapter for ZaiAdapter {
    fn name(&self) -> &'static str {
        "zai"
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
        let base = cfg
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or("https://open.bigmodel.cn/api/paas/v4")
            .trim_end_matches('/')
            .to_string();
        let mut last_err: Option<QuotaError> = None;
        for path in ["/balance", "/plan"] {
            let url = format!("{}{}", base, path);
            let resp = match session
                .get(&url)
                .header("Authorization", format!("Bearer {}", ctx.account.api_key))
                .timeout(Duration::from_secs_f64(UPSTREAM_TIMEOUT_S))
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    last_err = Some(QuotaError::new(format!("zai request failed: {}", e)));
                    continue;
                }
            };
            let status = resp.status().as_u16();
            if status != 200 {
                last_err = Some(QuotaError::new(format!("zai {} status {}", path, status)));
                continue;
            }
            let data: Value = match resp.json().await {
                Ok(d) => d,
                Err(e) => {
                    last_err = Some(QuotaError::new(format!("zai request failed: {}", e)));
                    continue;
                }
            };
            let (usage, limit) = extract(&data);
            if !truthy(&usage) && !truthy(&limit) {
                last_err = Some(QuotaError::new(format!("zai {} no usage payload", path)));
                continue;
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
            let snap = make_snapshot(self.name(), windows, self.source(), None, false, &ctx.now());
            return Ok(Some(snap));
        }
        Err(last_err.unwrap_or_else(|| QuotaError::new("zai unavailable")))
    }
}
