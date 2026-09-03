//! StatsSink 的 o2a-stats 实现（对齐 Python engine.py `record_stats` 语义）。
//!
//! - 统计禁用（CACHE_STATS_ENABLED）短路
//! - usage 与 error 皆空短路（对齐 `if not usage and not error: return`）
//! - 别名反查：model 记对外名（display_model），upstream_model 传上游名（计价用）
//! - batch：services[].pricing_extra.batch 真值
//! - no_cost：pricing_mode != token
//! - 写盘 offload 到 blocking 线程池（对齐 Python asyncio.to_thread）

use std::sync::Arc;

use serde_json::{json, Value};

use o2a_config::{PricingMode, Service};
use o2a_stats::stats::Usage;
use o2a_stats::StatsRegistry;

use crate::proxy::{display_model, StatsMeta, StatsSink};

pub struct O2aStatsSink {
    pub registry: Arc<StatsRegistry>,
}

impl O2aStatsSink {
    fn stats_for(&self, svc: &Service) -> Arc<o2a_stats::CacheStats> {
        let no_cost = svc.pricing_mode != PricingMode::Token;
        self.registry.get(
            &svc.name,
            &svc.id,
            &svc.account.id,
            &svc.account.name,
            no_cost,
        )
    }
}

impl StatsSink for O2aStatsSink {
    fn record(&self, svc: &Service, model: &str, usage: &Value, error: Option<&str>, meta: StatsMeta) {
        if !o2a_stats::is_cache_stats_enabled() {
            return;
        }
        let has_usage = usage.as_object().is_some_and(|m| !m.is_empty());
        if !has_usage && error.is_none() {
            return;
        }
        let display = display_model(svc, model);
        let upstream_model = (display != model).then(|| model.to_string());
        let batch = svc
            .pricing_extra
            .as_ref()
            .and_then(|m| m.get("batch"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let stats = self.stats_for(svc);

        let mut meta_obj = serde_json::Map::new();
        if let Some(d) = meta.duration_ms {
            meta_obj.insert("duration_ms".into(), json!(d));
        }
        if let Some(d) = meta.first_token_ms {
            meta_obj.insert("first_token_ms".into(), json!(d));
        }
        if let Some(d) = meta.output_tokens_per_sec {
            meta_obj.insert("output_tokens_per_sec".into(), json!(d));
        }
        let meta_val = (!meta_obj.is_empty()).then_some(Value::Object(meta_obj));

        let usage = usage.clone();
        let error = error.map(str::to_string);
        let display_c = display;
        let job = move || {
            stats.record(
                &display_c,
                &Usage::from_value(&usage),
                error.as_deref(),
                meta_val.as_ref(),
                upstream_model.as_deref(),
                batch,
            );
        };
        // 异步上下文 → blocking 线程池（对齐 asyncio.to_thread）；同步测试上下文内联执行
        match tokio::runtime::Handle::try_current() {
            Ok(h) => {
                h.spawn_blocking(job);
            }
            Err(_) => job(),
        }
    }
}
