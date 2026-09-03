//! manual 适配器：config 手填额度（accounts[].quota），冷启动兜底。
//!
//! quota = {"limit": 200, "unit": "requests" | "tokens" | "usd", "period": "day" | "week" | "month"}
//! 用量来自本地统计（usd 单位暂用请求数近似：limit 只做刻度）。

use serde_json::{json, Value};

use crate::base::{empty_window, make_snapshot, truthy, window_start, QuotaContext, QuotaError};
use crate::registry::QuotaAdapter;
use crate::stats_util::{count_requests, count_tokens};

pub struct ManualQuotaAdapter;

#[async_trait::async_trait]
impl QuotaAdapter for ManualQuotaAdapter {
    fn name(&self) -> &'static str {
        "manual"
    }

    fn source(&self) -> &'static str {
        "plan_config"
    }

    async fn fetch(&self, ctx: &QuotaContext) -> Result<Option<Value>, QuotaError> {
        let cfg = ctx.account.quota.clone().unwrap_or_default();
        let limit_raw = cfg.get("limit").cloned().unwrap_or(Value::Null);
        if !truthy(&limit_raw) {
            return Ok(None);
        }
        let limit = limit_raw.as_f64();
        let unit = cfg
            .get("unit")
            .and_then(Value::as_str)
            .unwrap_or("requests")
            .to_string();
        let mut period = cfg
            .get("period")
            .and_then(Value::as_str)
            .unwrap_or("month")
            .to_string();
        if !["day", "week", "month"].contains(&period.as_str()) {
            period = "month".into();
        }
        let now = ctx.now();
        let start = window_start(&now, &period);
        let used = if unit == "tokens" {
            count_tokens(&ctx.stats_dir, &ctx.account.id, &start) as f64
        } else {
            // 本地暂无逐条费用聚合窗口：以请求数近似展示（limit 只做刻度）
            count_requests(&ctx.stats_dir, &ctx.account.id, &start) as f64
        };
        let windows = vec![empty_window(
            &period,
            &unit,
            used,
            limit,
            Some(ctx.iso(&window_start(&now, "day"))),
        )];
        let plan = json!({
            "name": format!("{} 套餐", ctx.account.name),
            "period": period,
            "included": {"unit": unit, "amount": limit_raw},
        });
        let snap = make_snapshot(self.name(), windows, self.source(), Some(plan), false, &now);
        Ok(Some(snap))
    }
}
