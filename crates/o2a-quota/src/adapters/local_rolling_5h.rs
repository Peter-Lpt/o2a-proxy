//! local-rolling-5h 适配器：5 小时滚动窗（Claude Pro/Max 式）。
//!
//! 窗口起点 = 当前 5h 窗内最早一条记录（近似 provider 的滚动窗重置时刻）；
//! limit 来自 accounts[].quota.limit（requests），未配置时只展示用量不显示百分比。

use chrono::Duration;
use serde_json::Value;

use crate::base::{empty_window, make_snapshot, truthy, QuotaContext, QuotaError};
use crate::registry::QuotaAdapter;
use crate::stats_util::iter_records;

pub struct LocalRolling5hAdapter;

#[async_trait::async_trait]
impl QuotaAdapter for LocalRolling5hAdapter {
    fn name(&self) -> &'static str {
        "local-rolling-5h"
    }

    async fn fetch(&self, ctx: &QuotaContext) -> Result<Option<Value>, QuotaError> {
        let now = ctx.now();
        let start = now - Duration::hours(5);
        let records = iter_records(&ctx.stats_dir, &ctx.account.id, &start);
        let used = records.len() as f64;
        let oldest = records.iter().map(|(ts, _)| *ts).min();
        let reset_at = oldest.map(|o| ctx.iso(&(o + Duration::hours(5))));
        let limit_raw = ctx
            .account
            .quota
            .as_ref()
            .and_then(|q| q.get("limit").cloned())
            .unwrap_or(Value::Null);
        let limit = if truthy(&limit_raw) {
            limit_raw.as_f64()
        } else {
            None
        };
        let windows = vec![empty_window("rolling", "requests", used, limit, reset_at)];
        let snap = make_snapshot(self.name(), windows, self.source(), None, false, &now);
        Ok(Some(snap))
    }
}
