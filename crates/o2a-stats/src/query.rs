//! 账号级归并（对齐 Python stats.get_account_summary）。
//!
//! Python 经 load_config 找账号下服务；Rust 无配置访问权，由调用方
//! （engine）传入账号下的服务列表。

use std::path::PathBuf;

use serde_json::{json, Value};

use crate::stats::{CacheStats, CacheStatsConfig};

/// 账号聚合用的环境参数（对应 Python get_stats 读取的 env 缺省值，由调用方解析）。
#[derive(Debug, Clone)]
pub struct StatsEnv {
    pub stats_dir: PathBuf,
    pub retention_days: i64,
    pub pricing_path: Option<PathBuf>,
}

/// 参与归并的服务（name = 显示名，service_id = 稳定 id）。
#[derive(Debug, Clone)]
pub struct ServiceSummaryRef {
    pub name: String,
    pub service_id: String,
}

fn instance(env: &StatsEnv, svc: &ServiceSummaryRef, account_id: &str) -> CacheStats {
    CacheStats::new(CacheStatsConfig {
        stats_dir: env.stats_dir.clone(),
        retention_days: env.retention_days,
        service: svc.name.clone(),
        service_id: svc.service_id.clone(),
        account: account_id.to_string(),
        // 与 Python get_account_summary 一致：默认按 token 计价重放
        account_name: String::new(),
        no_cost: false,
        pricing_path: env.pricing_path.clone(),
    })
}

/// 数值求和并保持整数类型（对齐 Python int+int=int / float 参与即 float）。
fn sum_vals(a: &Value, b: &Value) -> Value {
    match (a.as_i64(), b.as_i64()) {
        (Some(x), Some(y)) => json!(x + y),
        _ => json!(a.as_f64().unwrap_or(0.0) + b.as_f64().unwrap_or(0.0)),
    }
}

const AGG_KEYS: [&str; 6] = [
    "requests",
    "total_input_tokens",
    "total_cache_read_tokens",
    "total_cache_write_tokens",
    "total_output_tokens",
    "total_cost",
];

/// 按账号聚合其下所有服务的统计。
pub fn get_account_summary(
    env: &StatsEnv,
    account_id: &str,
    services: &[ServiceSummaryRef],
    period: &str,
) -> Value {
    if services.is_empty() {
        return json!({ "period": period, "account": account_id, "requests": 0 });
    }
    if period == "all" {
        let mut total: Value = json!({
            "requests": 0, "total_input_tokens": 0, "total_cache_read_tokens": 0,
            "total_cache_write_tokens": 0, "total_output_tokens": 0, "total_cost": 0.0,
        });
        let mut days: Vec<Value> = Vec::new();
        for svc in services {
            let s = instance(env, svc, account_id).get_summary("all");
            for d in s.get("days").and_then(|v| v.as_array()).cloned().unwrap_or_default() {
                if let Some(dt) = d.get("daily_total") {
                    for k in AGG_KEYS {
                        total[k] = sum_vals(&total[k], &dt[k]);
                    }
                }
                days.push(d);
            }
        }
        let denom_hit = total["total_cache_read_tokens"].as_f64().unwrap_or(0.0)
            + total["total_input_tokens"].as_f64().unwrap_or(0.0);
        let denom_cov = denom_hit + total["total_cache_write_tokens"].as_f64().unwrap_or(0.0);
        total["avg_cache_hit_rate"] = json!(if denom_hit > 0.0 {
            total["total_cache_read_tokens"].as_f64().unwrap_or(0.0) / denom_hit
        } else {
            0.0
        });
        total["avg_cache_coverage"] = json!(if denom_cov > 0.0 {
            total["total_cache_read_tokens"].as_f64().unwrap_or(0.0) / denom_cov
        } else {
            0.0
        });
        return json!({ "period": period, "account": account_id, "days": days, "total": total });
    }

    // day / hour：合并 daily_total，hours 按时间排序叠加
    let mut agg_daily: Value = json!({
        "requests": 0, "total_input_tokens": 0, "total_cache_read_tokens": 0,
        "total_cache_write_tokens": 0, "total_output_tokens": 0, "total_cost": 0.0,
    });
    let mut hours: Value = json!({});
    for svc in services {
        let s = instance(env, svc, account_id).get_summary(period);
        let daily = if period == "day" {
            s.get("daily_total").cloned()
        } else {
            Some(s.clone())
        };
        let Some(daily) = daily else { continue };
        for k in AGG_KEYS {
            if daily.get(k).is_some() {
                agg_daily[k] = sum_vals(&agg_daily[k], &daily[k]);
            }
        }
        for h in s.get("hours").and_then(|v| v.as_array()).cloned().unwrap_or_default() {
            let hid = h.get("hour").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let cur = hours.get(&hid).cloned();
            match cur {
                Some(mut c) => {
                    for k in ["requests", "total_input_tokens", "total_cache_read_tokens",
                              "total_cache_write_tokens", "total_output_tokens", "total_cost"] {
                        c[k] = sum_vals(&c[k], &h[k]);
                    }
                    // Python 近似口径：重复小时命中率取两者平均
                    c["avg_cache_hit_rate"] = json!((c["avg_cache_hit_rate"].as_f64().unwrap_or(0.0)
                        + h["avg_cache_hit_rate"].as_f64().unwrap_or(0.0)) / 2.0);
                    c["avg_cache_coverage"] = json!((c["avg_cache_coverage"].as_f64().unwrap_or(0.0)
                        + h["avg_cache_coverage"].as_f64().unwrap_or(0.0)) / 2.0);
                    hours[&hid] = c;
                }
                None => {
                    hours[&hid] = h;
                }
            }
        }
    }
    let denom_hit = agg_daily["total_cache_read_tokens"].as_f64().unwrap_or(0.0)
        + agg_daily["total_input_tokens"].as_f64().unwrap_or(0.0);
    let denom_cov = denom_hit + agg_daily["total_cache_write_tokens"].as_f64().unwrap_or(0.0);
    agg_daily["avg_cache_hit_rate"] = json!(if denom_hit > 0.0 {
        agg_daily["total_cache_read_tokens"].as_f64().unwrap_or(0.0) / denom_hit
    } else {
        0.0
    });
    agg_daily["avg_cache_coverage"] = json!(if denom_cov > 0.0 {
        agg_daily["total_cache_read_tokens"].as_f64().unwrap_or(0.0) / denom_cov
    } else {
        0.0
    });
    let mut hour_list: Vec<(String, Value)> =
        hours.as_object().map(|o| o.iter().map(|(k, v)| (k.clone(), v.clone())).collect()).unwrap_or_default();
    hour_list.sort_by(|a, b| a.0.cmp(&b.0));
    json!({
        "period": period,
        "account": account_id,
        "hours": hour_list.iter().map(|(_, v)| v.clone()).collect::<Vec<_>>(),
        "daily_total": agg_daily,
    })
}
