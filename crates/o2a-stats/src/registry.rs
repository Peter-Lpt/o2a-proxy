//! 全局统计实例注册表（对齐 Python stats.get_stats / clear_pricing_cache）。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::query::StatsEnv;
use crate::stats::{CacheStats, CacheStatsConfig};

pub struct StatsRegistry {
    env: StatsEnv,
    instances: Mutex<HashMap<String, Arc<CacheStats>>>,
}

impl StatsRegistry {
    pub fn new(stats_dir: PathBuf, retention_days: i64, pricing_path: Option<PathBuf>) -> Self {
        Self {
            env: StatsEnv { stats_dir, retention_days, pricing_path },
            instances: Mutex::new(HashMap::new()),
        }
    }

    pub fn env(&self) -> &StatsEnv {
        &self.env
    }

    /// 按服务 id / 名字取实例（线程安全懒初始化；键规则对齐 Python get_stats）。
    pub fn get(
        &self,
        service: &str,
        service_id: &str,
        account: &str,
        account_name: &str,
        no_cost: bool,
    ) -> Arc<CacheStats> {
        let key = if !service_id.is_empty() { service_id } else if !service.is_empty() { service } else { "default" };
        let mut map = self.instances.lock().unwrap();
        map.entry(key.to_string()).or_insert_with(|| {
            Arc::new(CacheStats::new(CacheStatsConfig {
                stats_dir: self.env.stats_dir.clone(),
                retention_days: self.env.retention_days,
                service: service.to_string(),
                service_id: service_id.to_string(),
                account: account.to_string(),
                account_name: account_name.to_string(),
                no_cost,
                pricing_path: self.env.pricing_path.clone(),
            }))
        }).clone()
    }

    /// POST /pricing-reload：清空所有实例的定价/月累计缓存。
    pub fn clear_pricing_cache(&self) {
        for s in self.instances.lock().unwrap().values() {
            s.clear_pricing_cache();
        }
    }
}

/// 统计是否启用（CACHE_STATS_ENABLED env，默认开启；true/1/yes 任意大小写）。
pub fn is_cache_stats_enabled() -> bool {
    is_cache_stats_enabled_with(std::env::var("CACHE_STATS_ENABLED").ok().as_deref())
}

/// 供测试注入的纯函数版本（与 Python is_cache_stats_enabled 同规则）。
pub fn is_cache_stats_enabled_with(v: Option<&str>) -> bool {
    match v {
        Some(v) => matches!(v.to_ascii_lowercase().as_str(), "true" | "1" | "yes"),
        None => true,
    }
}
