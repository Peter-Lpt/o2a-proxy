//! local 适配器：从本地 JSONL 聚合日/周/月窗口用量（兜底，任何 provider 都能用）。

use serde_json::{Value};
use o2a_config::Account;

use crate::base::{empty_window, make_snapshot, truthy, window_start, QuotaContext, QuotaError};
use crate::registry::QuotaAdapter;
use crate::stats_util::count_requests;

/// 按日/周/月窗口聚合本地统计。limit 可由 accounts[].quota 配置。
pub struct LocalQuotaAdapter;

fn quota_map(account: &Account) -> serde_json::Map<String, Value> {
    account.quota.clone().unwrap_or_default()
}

#[async_trait::async_trait]
impl QuotaAdapter for LocalQuotaAdapter {
    fn name(&self) -> &'static str {
        "local"
    }

    async fn fetch(&self, ctx: &QuotaContext) -> Result<Option<Value>, QuotaError> {
        let now = ctx.now();
        let cfg = quota_map(&ctx.account);
        let period = cfg
            .get("period")
            .and_then(Value::as_str)
            .unwrap_or("month")
            .to_string();
        let limit_raw = cfg.get("limit").cloned().unwrap_or(Value::Null);
        let unit = cfg
            .get("unit")
            .and_then(Value::as_str)
            .unwrap_or("requests")
            .to_string();
        let kinds: Vec<String> = if ["day", "week", "month"].contains(&period.as_str()) {
            vec![period.clone()]
        } else {
            vec!["month".to_string()]
        };
        let limit = if truthy(&limit_raw) {
            limit_raw.as_f64()
        } else {
            None
        };
        let mut windows = Vec::new();
        for kind in &kinds {
            let start = window_start(&now, kind);
            let used = count_requests(&ctx.stats_dir, &ctx.account.id, &start) as f64;
            let reset_at = if kind == "day" {
                Some(ctx.iso(&window_start(&now, "day")))
            } else {
                None
            };
            // 注意：窗口 unit 固定 "requests"，limit 仅在配置 unit=requests 时生效（Python 语义）
            windows.push(empty_window(
                kind,
                "requests",
                used,
                if unit == "requests" { limit } else { None },
                reset_at,
            ));
        }
        let snap = make_snapshot(self.name(), windows, self.source(), None, false, &now);
        Ok(Some(snap))
    }
}
