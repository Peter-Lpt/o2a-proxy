//! declarative 适配器：用 accounts[].quota 声明式窗口，接入套餐目录。
//!
//! accounts[].quota = {"plan": "glm-coding-plan"} 或
//! {"windows": [{"kind":"month","unit":"requests","limit":200}], "plan": "my-plan"}
//! 需要本地用量时通过 stats_dir 聚合（与 manual 一致）；不依赖外网。

use serde_json::{json, Map, Value};

use crate::base::{empty_window, make_snapshot, window_start, QuotaContext, QuotaError};
use crate::registry::QuotaAdapter;
use crate::stats_util::{count_requests, count_tokens};

pub struct DeclarativeQuotaAdapter;

#[async_trait::async_trait]
impl QuotaAdapter for DeclarativeQuotaAdapter {
    fn name(&self) -> &'static str {
        "declarative"
    }

    fn source(&self) -> &'static str {
        "plan_config"
    }

    async fn fetch(&self, ctx: &QuotaContext) -> Result<Option<Value>, QuotaError> {
        let cfg = ctx.account.quota.clone().unwrap_or_default();
        let plan_name = cfg
            .get("plan")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        let windows_cfg = cfg.get("windows").and_then(Value::as_array).cloned();
        if plan_name.is_none() && windows_cfg.is_none() {
            return Ok(None);
        }
        let now = ctx.now();
        let mut windows = Vec::new();
        for w in windows_cfg.unwrap_or_default() {
            let Some(w) = w.as_object() else {
                continue;
            };
            let kind = w
                .get("kind")
                .and_then(Value::as_str)
                .or_else(|| w.get("period").and_then(Value::as_str))
                .unwrap_or("month")
                .to_string();
            let unit = w
                .get("unit")
                .and_then(Value::as_str)
                .unwrap_or("requests")
                .to_string();
            let limit = w.get("limit").cloned().unwrap_or(Value::Null).as_f64();
            let mut period = w
                .get("period")
                .and_then(Value::as_str)
                .unwrap_or("month")
                .to_string();
            if !["day", "week", "month"].contains(&period.as_str()) {
                period = "month".into();
            }
            let start = window_start(&now, &period);
            let used = if unit == "requests" {
                count_requests(&ctx.stats_dir, &ctx.account.id, &start) as f64
            } else {
                count_tokens(&ctx.stats_dir, &ctx.account.id, &start) as f64
            };
            windows.push(empty_window(
                &kind,
                &unit,
                used,
                limit,
                Some(ctx.iso(&window_start(&now, "day"))),
            ));
        }
        let plan = plan_name.map(|name| {
            let empty = Map::new();
            json!({
                "name": name,
                "included": cfg.get("included").cloned().unwrap_or(Value::Object(empty.clone())),
                "overage": cfg.get("overage").cloned().unwrap_or(Value::Object(empty.clone())),
                "free_tier": cfg.get("free_tier").cloned().unwrap_or(Value::Object(empty)),
            })
        });
        let snap = make_snapshot(self.name(), windows, self.source(), plan, false, &now);
        Ok(Some(snap))
    }
}
