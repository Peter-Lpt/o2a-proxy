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

fn list_summary_sources(stats_dir: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let summary_dir = stats_dir.join("summary");
    let Ok(entries) = std::fs::read_dir(&summary_dir) else {
        return out;
    };
    let entries: Vec<_> = entries.flatten().collect();
    for e in &entries {
        let name = e.file_name().to_string_lossy().to_string();
        if e.path().is_file() && name.ends_with(".json") {
            out.push(("".into(), name.trim_end_matches(".json").to_string()));
        }
    }
    for e in &entries {
        if !e.path().is_dir() {
            continue;
        }
        let svc = e.file_name().to_string_lossy().to_string();
        if let Ok(files) = std::fs::read_dir(e.path()) {
            for f in files.flatten() {
                let n = f.file_name().to_string_lossy().to_string();
                if f.path().is_file() && n.ends_with(".json") {
                    out.push((svc.clone(), n.trim_end_matches(".json").to_string()));
                }
            }
        }
    }
    out
}

fn load_day(stats_dir: &Path, date_str: &str, service: &str, primary: &str) -> Option<Value> {
    let include_legacy = !service.is_empty() && service == primary;
    let sources = list_summary_sources(stats_dir);
    let mut acc: BTreeMap<String, Agg> = BTreeMap::new();
    for (svc, ds) in &sources {
        if ds != date_str {
            continue;
        }
        if !service.is_empty() && svc != service && !(include_legacy && svc.is_empty()) {
            continue;
        }
        let p = if svc.is_empty() {
            stats_dir.join("summary").join(format!("{date_str}.json"))
        } else {
            stats_dir.join("summary").join(svc).join(format!("{date_str}.json"))
        };
        let Some(raw) = read_json(&p) else { continue };
        let Some(hours) = raw.get("hours").and_then(|h| h.as_object()) else {
            continue;
        };
        for (hh, h) in hours {
            let g = acc.entry(hh.clone()).or_default();
            g.requests += h["requests"].as_i64().unwrap_or(0);
            g.input += h["total_input_tokens"].as_i64().unwrap_or(0);
            g.read += h["total_cache_read_tokens"].as_i64().unwrap_or(0);
            g.write += h["total_cache_write_tokens"].as_i64().unwrap_or(0);
            g.output += h["total_output_tokens"].as_i64().unwrap_or(0);
            g.cost += h["total_cost"].as_f64().unwrap_or(0.0);
        }
    }
    if acc.is_empty() {
        return None;
    }
    let mut hours_map = Map::new();
    let mut day = Agg::default();
    for (hh, g) in &acc {
        hours_map.insert(hh.clone(), g.to_json());
        day.add_agg(g);
    }
    Some(json!({"date": date_str, "day": day.to_json(), "hours": hours_map}))
}

fn read_records(stats_dir: &Path, date_str: &str) -> Vec<Value> {
    let p = stats_dir.join(format!("{date_str}.jsonl"));
    let Ok(s) = std::fs::read_to_string(&p) else {
        return Vec::new();
    };
    s.lines()
        .filter_map(|l| serde_json::from_str(l.trim()).ok())
        .collect()
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
    let now = chrono::Local::now();
    let today_str = now.format("%Y-%m-%d").to_string();
    let month_prefix = now.format("%Y-%m").to_string();
    let cur_hour = now.format("%H").to_string();
    let today = load_day(&dir, &today_str, service, &primary);
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

    let records = read_records(&dir, &today_str);
    let today_minute = aggregate_minutes(&records, service, &primary);
    let today_minute_by_model = aggregate_minutes_by_model(&records, service, &primary);
    let by_model = sum_by_model(&records, service, &primary);

    let day_files: Vec<String> = list_summary_sources(&dir)
        .into_iter()
        .map(|(_, d)| d)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter(|d| d.starts_with(&month_prefix))
        .collect();

    let mut month_total = Agg::default();
    let mut days_map: BTreeMap<String, Value> = BTreeMap::new();
    for ds in &day_files {
        if let Some(dd) = load_day(&dir, ds, service, &primary) {
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
        let recs = read_records(&dir, ds);
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
    let today_str = chrono::Local::now().format("%Y-%m-%d").to_string();
    let records: Vec<Value> = read_records(&dir, &today_str)
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

        let rec = json!({
            "timestamp": format!("{date}T{hour}:12:00"),
            "service": "svc1",
            "model": "m1",
            "input_tokens": 1000,
            "cache_read_tokens": 400,
            "cache_write_tokens": 100,
            "output_tokens": 200,
            "cache_hit_rate": 0.2857,
            "cache_coverage": 0.2666,
            "cost": 0.05
        });
        fs::write(
            dir.join("cache_stats").join(format!("{date}.jsonl")),
            format!("{}\n", serde_json::to_string(&rec).unwrap()),
        )
        .unwrap();
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
        assert_eq!(out["byModel"][0]["requests"], 1);
        assert_eq!(out["monthByModel"][0]["requests"], 1);
        assert!(out["monthDaily"].as_array().unwrap().len() >= 1);
        assert_eq!(out["todayMinute"][0]["requests"], 1);

        let live = get_live(&dir.join("cache_stats"), "svc1", "svc1").unwrap();
        assert_eq!(live["records"].as_array().unwrap().len(), 1);

        let _ = fs::remove_dir_all(&dir);
    }
}
