use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use chrono::Datelike;
use serde_json::{json, Map, Value};

#[derive(Default, Clone)]
struct Agg {
    requests: i64,
    input: i64,
    read: i64,
    write: i64,
    output: i64,
    cost: f64,
}

impl Agg {
    fn add_record(&mut self, rec: &Value) {
        self.requests += 1;
        self.input += rec["input_tokens"].as_i64().unwrap_or(0);
        self.read += rec["cache_read_tokens"].as_i64().unwrap_or(0);
        self.write += rec["cache_write_tokens"].as_i64().unwrap_or(0);
        self.output += rec["output_tokens"].as_i64().unwrap_or(0);
        self.cost += rec["cost"].as_f64().unwrap_or(0.0);
    }

    fn add_agg(&mut self, other: &Agg) {
        self.requests += other.requests;
        self.input += other.input;
        self.read += other.read;
        self.write += other.write;
        self.output += other.output;
        self.cost += other.cost;
    }

    fn to_json(&self) -> Value {
        let denom_hit = self.read + self.input;
        let denom_cov = denom_hit + self.write;
        json!({
            "requests": self.requests,
            "input": self.input,
            "read": self.read,
            "write": self.write,
            "output": self.output,
            "cost": (self.cost * 10000.0).round() / 10000.0,
            "hitRate": if denom_hit > 0 { self.read as f64 / denom_hit as f64 } else { 0.0 },
            "coverage": if denom_cov > 0 { self.read as f64 / denom_cov as f64 } else { 0.0 },
        })
    }
}

fn read_json(path: &Path) -> Option<Value> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
}

fn load_pricing(stats_dir: &Path) -> Value {
    let p = stats_dir
        .parent()
        .map(|d| d.join("pricing.json"))
        .unwrap_or_else(|| Path::new("pricing.json").to_path_buf());
    read_json(&p).unwrap_or(Value::Null)
}

/// 参考 proxy.py CacheStats._calc_cost：按当前 pricing.json 重算单次请求费用。
/// 历史记录写入时可能缺少 cost 或定价表后补，读取时统一重算，保证口径一致。
fn recalc_cost(
    model: &str,
    input: i64,
    read: i64,
    write: i64,
    output: i64,
    pricing: &Value,
) -> f64 {
    let Some(providers) = pricing.as_object() else {
        return 0.0;
    };
    let mut price = None;
    for (pname, pdata) in providers {
        if pname.starts_with('_') {
            continue;
        }
        if let Some(models) = pdata.get("models").and_then(|m| m.as_object()) {
            if let Some(m) = models.get(model) {
                price = Some(m);
                break;
            }
        }
    }
    let Some(price) = price else { return 0.0 };
    let Some(tier) = price
        .get("tiers")
        .and_then(|t| t.as_array())
        .and_then(|a| a.first())
    else {
        return 0.0;
    };
    let input_price = tier.get("input").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let output_price = tier.get("output").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let input_cost = input as f64 * input_price / 1_000_000.0;
    let output_cost = output as f64 * output_price / 1_000_000.0;
    let cache_read_cost = if let Some(c) = tier.get("cache_hit").and_then(|v| v.as_f64()) {
        read as f64 * c / 1_000_000.0
    } else {
        read as f64 * input_price * 0.2 / 1_000_000.0
    };
    let cache_write_cost = if let Some(c) = tier.get("cache_miss").and_then(|v| v.as_f64()) {
        write as f64 * c / 1_000_000.0
    } else {
        write as f64 * input_price / 1_000_000.0
    };
    input_cost + output_cost + cache_read_cost + cache_write_cost
}

fn list_record_dates(stats_dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(stats_dir) else {
        return out;
    };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if e.path().is_file() && name.ends_with(".jsonl") {
            out.push(name.trim_end_matches(".jsonl").to_string());
        }
    }
    out
}

/// 读取某天原始记录，并统一按当前定价重算 cost 字段。
fn read_records(stats_dir: &Path, date_str: &str, pricing: &Value) -> Vec<Value> {
    let p = stats_dir.join(format!("{date_str}.jsonl"));
    let Ok(s) = std::fs::read_to_string(&p) else {
        return Vec::new();
    };
    s.lines()
        .filter_map(|l| serde_json::from_str(l.trim()).ok())
        .map(|mut rec: Value| {
            let model = rec["model"].as_str().unwrap_or("").to_string();
            let input = rec["input_tokens"].as_i64().unwrap_or(0);
            let read = rec["cache_read_tokens"].as_i64().unwrap_or(0);
            let write = rec["cache_write_tokens"].as_i64().unwrap_or(0);
            let output = rec["output_tokens"].as_i64().unwrap_or(0);
            rec["cost"] = json!(recalc_cost(&model, input, read, write, output, pricing));
            rec
        })
        .collect()
}

/// 从原始记录按小时聚合某天数据（不再依赖容易过期的 summary 文件，
/// 保证请求数、费用与记录口径一致）。
fn load_day(
    stats_dir: &Path,
    date_str: &str,
    service: &str,
    primary: &str,
    pricing: &Value,
) -> Option<Value> {
    let records = read_records(stats_dir, date_str, pricing);
    let mut hours: BTreeMap<String, Agg> = BTreeMap::new();
    let mut day = Agg::default();
    for rec in &records {
        if !matches_service(rec, service, primary) {
            continue;
        }
        let ts = rec["timestamp"].as_str().unwrap_or("");
        if ts.len() < 13 {
            continue;
        }
        let hh = &ts[11..13];
        hours.entry(hh.to_string()).or_default().add_record(rec);
        day.add_record(rec);
    }
    if day.requests == 0 {
        return None;
    }
    let mut hours_map = Map::new();
    for (hh, g) in &hours {
        hours_map.insert(hh.clone(), g.to_json());
    }
    Some(json!({"date": date_str, "day": day.to_json(), "hours": hours_map}))
}

fn matches_service(rec: &Value, service: &str, primary: &str) -> bool {
    if service.is_empty() {
        return true;
    }
    match rec.get("service").and_then(|v| v.as_str()) {
        Some(s) => s == service,
        None => service == primary,
    }
}

fn sum_by_model(records: &[Value], service: &str, primary: &str) -> Vec<Value> {
    let mut map: BTreeMap<String, Agg> = BTreeMap::new();
    for rec in records {
        if !matches_service(rec, service, primary) {
            continue;
        }
        let model = rec["model"].as_str().unwrap_or("unknown").to_string();
        map.entry(model).or_default().add_record(rec);
    }
    let mut out: Vec<Value> = map
        .into_iter()
        .map(|(model, agg)| {
            let mut v = agg.to_json();
            v["model"] = json!(model);
            v
        })
        .collect();
    out.sort_by(|a, b| {
        b["cost"]
            .as_f64()
            .unwrap_or(0.0)
            .partial_cmp(&a["cost"].as_f64().unwrap_or(0.0))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out
}

fn aggregate_minutes(records: &[Value], service: &str, primary: &str) -> Vec<Value> {
    let by_model = aggregate_minutes_by_model(records, service, primary);
    let mut map: BTreeMap<String, Agg> = BTreeMap::new();
    for arr in by_model.values() {
        for rec in arr {
            let minute = rec["minute"].as_str().unwrap_or("").to_string();
            map.entry(minute).or_default().add_agg(&agg_from_json(rec));
        }
    }
    map.into_iter()
        .map(|(minute, agg)| {
            let mut v = agg.to_json();
            v["minute"] = json!(minute);
            v
        })
        .collect()
}

fn agg_from_json(v: &Value) -> Agg {
    Agg {
        requests: v["requests"].as_i64().unwrap_or(0),
        input: v["input"].as_i64().unwrap_or(0),
        read: v["read"].as_i64().unwrap_or(0),
        write: v["write"].as_i64().unwrap_or(0),
        output: v["output"].as_i64().unwrap_or(0),
        cost: v["cost"].as_f64().unwrap_or(0.0),
    }
}

fn aggregate_minutes_by_model(records: &[Value], service: &str, primary: &str) -> BTreeMap<String, Vec<Value>> {
    let mut by_model: BTreeMap<String, BTreeMap<String, Agg>> = BTreeMap::new();
    for rec in records {
        if !matches_service(rec, service, primary) {
            continue;
        }
        let ts = rec["timestamp"].as_str().unwrap_or("");
        if ts.len() < 16 {
            continue;
        }
        let minute = &ts[..16];
        let model = rec["model"].as_str().unwrap_or("unknown").to_string();
        by_model
            .entry(model)
            .or_default()
            .entry(minute.to_string())
            .or_default()
            .add_record(rec);
    }
    by_model
        .into_iter()
        .map(|(model, m)| {
            let arr = m
                .into_iter()
                .map(|(minute, agg)| {
                    let mut v = agg.to_json();
                    v["minute"] = json!(minute);
                    v
                })
                .collect();
            (model, arr)
        })
        .collect()
}

fn zero_day(date: &str) -> Value {
    json!({
        "date": date,
        "requests": 0, "input": 0, "read": 0, "write": 0, "output": 0,
        "cost": 0.0, "hitRate": 0.0, "coverage": 0.0,
    })
}

fn day_to_series(dd: &Value) -> Value {
    let mut v = dd["day"].clone();
    v["date"] = dd["date"].clone();
    v
}

pub fn get_stats(dir: &Path, service: &str, primary: &str) -> Result<Value, String> {
    let pricing = load_pricing(dir);
    let now = chrono::Local::now();
    let today_str = now.format("%Y-%m-%d").to_string();
    let month_prefix = now.format("%Y-%m").to_string();
    let cur_hour = now.format("%H").to_string();
    let today = load_day(&dir, &today_str, service, &primary, &pricing);
    let current = today
        .as_ref()
        .and_then(|t| t.get("hours").and_then(|h| h.get(&cur_hour)))
        .cloned()
        .unwrap_or_else(|| {
            json!({"hour": cur_hour, "requests":0,"input":0,"read":0,"write":0,"output":0,"cost":0,"hitRate":0,"coverage":0})
        });

    let mut today_hourly = Vec::new();
    for h in 0..24 {
        let hh = format!("{h:02}");
        let d = today.as_ref().and_then(|t| t.get("hours").and_then(|m| m.get(&hh)));
        match d {
            Some(d) => {
                let mut v = d.clone();
                v["hour"] = json!(hh);
                today_hourly.push(v);
            }
            None => today_hourly.push(json!({
                "hour": hh, "requests":0,"input":0,"read":0,"write":0,"output":0,"cost":0,"hitRate":0,"coverage":0
            })),
        }
    }

    let records = read_records(&dir, &today_str, &pricing);
    let today_minute = aggregate_minutes(&records, service, &primary);
    let today_minute_by_model = aggregate_minutes_by_model(&records, service, &primary);
    let by_model = sum_by_model(&records, service, &primary);

    let day_files: Vec<String> = list_record_dates(&dir)
        .into_iter()
        .filter(|d| d.starts_with(&month_prefix))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    let mut month_total = Agg::default();
    let mut days_map: BTreeMap<String, Value> = BTreeMap::new();
    for ds in &day_files {
        if let Some(dd) = load_day(&dir, ds, service, &primary, &pricing) {
            if let Some(day) = dd.get("day") {
                month_total.add_agg(&agg_from_json(day));
            }
            days_map.insert(ds.clone(), dd);
        }
    }

    let days_in_month = days_in_month(now.year(), now.month());
    let up_to = now.day().min(days_in_month);
    let mut month_daily = Vec::new();
    for d in 1..=up_to {
        let ds = format!("{month_prefix}-{d:02}");
        match days_map.get(&ds) {
            Some(dd) => month_daily.push(day_to_series(dd)),
            None => month_daily.push(zero_day(&ds)),
        }
    }

    // 本月按模型（累计 + 逐日）
    let mut month_by_model_map: BTreeMap<String, Agg> = BTreeMap::new();
    let mut month_daily_by_model: BTreeMap<String, BTreeMap<String, Value>> = BTreeMap::new();
    for ds in &day_files {
        let recs = read_records(&dir, ds, &pricing);
        for g in sum_by_model(&recs, service, &primary) {
            let m = g["model"].as_str().unwrap_or("unknown").to_string();
            month_by_model_map.entry(m.clone()).or_default().add_agg(&agg_from_json(&g));
            let mut d = g.clone();
            d["date"] = json!(ds);
            month_daily_by_model.entry(m).or_default().insert(ds.clone(), d);
        }
    }
    let month_by_model: Vec<Value> = month_by_model_map
        .into_iter()
        .map(|(model, agg)| {
            let mut v = agg.to_json();
            v["model"] = json!(model);
            v
        })
        .collect();
    let month_daily_by_model: Value = month_daily_by_model
        .into_iter()
        .map(|(model, days)| {
            let mut arr = Vec::new();
            for d in 1..=up_to {
                let ds = format!("{month_prefix}-{d:02}");
                arr.push(
                    days.get(&ds)
                        .cloned()
                        .unwrap_or_else(|| {
                            let mut v = zero_day(&ds);
                            v["model"] = json!(model);
                            v
                        }),
                );
            }
            (model, json!(arr))
        })
        .collect::<serde_json::Map<_, _>>()
        .into();

    let today_obj = today
        .as_ref()
        .map(day_to_series)
        .unwrap_or_else(|| {
            let mut v = zero_day(&today_str);
            v["date"] = json!(today_str);
            v
        });

    let month_hit_rate = {
        let denom = month_total.read + month_total.input;
        if denom > 0 {
            month_total.read as f64 / denom as f64
        } else {
            0.0
        }
    };
    let month_coverage = {
        let denom = month_total.read + month_total.input + month_total.write;
        if denom > 0 {
            month_total.read as f64 / denom as f64
        } else {
            0.0
        }
    };

    Ok(json!({
        "updatedAt": now.to_rfc3339(),
        "current": current,
        "today": today_obj,
        "month": {
            "requests": month_total.requests,
            "input": month_total.input,
            "read": month_total.read,
            "write": month_total.write,
            "output": month_total.output,
            "cost": (month_total.cost * 10000.0).round() / 10000.0,
            "hitRate": month_hit_rate,
            "coverage": month_coverage,
            "days": day_files.len(),
        },
        "todayHourly": today_hourly,
        "todayMinute": today_minute,
        "todayMinuteByModel": today_minute_by_model,
        "monthDaily": month_daily,
        "monthDailyByModel": month_daily_by_model,
        "byModel": by_model,
        "monthByModel": month_by_model,
    }))
}

fn days_in_month(year: i32, month: u32) -> u32 {
    let (y, m) = if month == 12 { (year + 1, 1) } else { (year, month + 1) };
    let first_next = chrono::NaiveDate::from_ymd_opt(y, m, 1).unwrap();
    let first_cur = chrono::NaiveDate::from_ymd_opt(year, month, 1).unwrap();
    (first_next - first_cur).num_days() as u32
}

pub fn get_live(dir: &Path, service: &str, primary: &str) -> Result<Value, String> {
    let pricing = load_pricing(dir);
    let today_str = chrono::Local::now().format("%Y-%m-%d").to_string();
    let records: Vec<Value> = read_records(&dir, &today_str, &pricing)
        .into_iter()
        .filter(|r| matches_service(r, service, &primary))
        .rev()
        .take(80)
        .collect();
    Ok(json!({
        "records": records,
        "updatedAt": chrono::Local::now().to_rfc3339(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_get_stats_aggregation() {
        let dir = std::env::temp_dir().join(format!("o2a_stats_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("cache_stats").join("summary").join("svc1")).unwrap();

        let now = chrono::Local::now();
        let date = now.format("%Y-%m-%d").to_string();
        let hour = now.format("%H").to_string();

        let summary = json!({
            "date": date,
            "hours": {
                hour.clone(): {
                    "requests": 3,
                    "total_input_tokens": 1000,
                    "total_cache_read_tokens": 400,
                    "total_cache_write_tokens": 100,
                    "total_output_tokens": 200,
                    "total_cost": 0.05,
                    "_hit_rate_sum": 0.28,
                    "_coverage_sum": 0.0
                }
            }
        });
        fs::write(
            dir.join("cache_stats").join("summary").join("svc1").join(format!("{date}.json")),
            serde_json::to_string(&summary).unwrap(),
        )
        .unwrap();

        // 3 条同时段记录（聚合进当前小时），成本由读取时按定价重算，不受缺 cost 字段影响
        let mut jsonl = String::new();
        for (i, minute) in ["12", "13", "14"].iter().enumerate() {
            jsonl.push_str(&format!(
                "{}\n",
                serde_json::to_string(&json!({
                    "timestamp": format!("{date}T{hour}:{minute}:00"),
                    "service": "svc1",
                    "model": "m1",
                    "input_tokens": 1000,
                    "cache_read_tokens": 400,
                    "cache_write_tokens": 100,
                    "output_tokens": 200,
                    "cache_hit_rate": 0.2857,
                    "cache_coverage": 0.2666,
                    "cost": 0.05 * (i as f64 + 1.0)
                }))
                .unwrap()
            ));
        }
        fs::write(dir.join("cache_stats").join(format!("{date}.jsonl")), jsonl).unwrap();
        fs::write(
            dir.join("config.json"),
            json!({
                "cache_stats_dir": "cache_stats",
                "services": [{"comment": "svc1", "mode": "claude", "model": "m1"}]
            })
            .to_string(),
        )
        .unwrap();

        let out = get_stats(&dir.join("cache_stats"), "svc1", "svc1").unwrap();
        assert_eq!(out["current"]["requests"], 3);
        assert_eq!(out["current"]["hitRate"], 0.2857142857142857);
        assert_eq!(out["today"]["requests"], 3);
        assert_eq!(out["todayHourly"].as_array().unwrap().len(), 24);
        assert_eq!(out["month"]["requests"], 3);
        assert_eq!(out["byModel"][0]["model"], "m1");
        assert_eq!(out["byModel"][0]["requests"], 3);
        assert_eq!(out["monthByModel"][0]["requests"], 3);
        assert!(out["monthDaily"].as_array().unwrap().len() >= 1);
        assert_eq!(out["todayMinute"].as_array().unwrap().len(), 3);

        let live = get_live(&dir.join("cache_stats"), "svc1", "svc1").unwrap();
        assert_eq!(live["records"].as_array().unwrap().len(), 3);

        let _ = fs::remove_dir_all(&dir);
    }

}
