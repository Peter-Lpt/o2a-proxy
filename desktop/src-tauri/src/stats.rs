use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use chrono::{Datelike, NaiveDate};
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

/// 读取 config.json 的 accounts，构建账号 id -> name 别名映射（用于定价按 name 匹配）。
fn load_account_aliases(stats_dir: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let p = stats_dir
        .parent()
        .map(|d| d.join("config.json"))
        .unwrap_or_else(|| Path::new("config.json").to_path_buf());
    if let Some(cfg) = read_json(&p) {
        if let Some(accounts) = cfg.get("accounts").and_then(|a| a.as_array()) {
            for a in accounts {
                if let (Some(id), Some(name)) = (
                    a.get("id").and_then(|v| v.as_str()),
                    a.get("name").and_then(|v| v.as_str()),
                ) {
                    out.insert(id.to_string(), name.to_string());
                }
            }
        }
    }
    out
}

/// 参考 proxy.py CacheStats._calc_cost：按当前 pricing.json 重算单次请求费用。
/// 历史记录写入时可能缺少 cost 或定价表后补，读取时统一重算，保证口径一致。
/// 账号级定价（pricing.json["accounts"]，键可为账号 id 或 name）优先，全局兜底。
fn recalc_cost(
    model: &str,
    input: i64,
    read: i64,
    write: i64,
    output: i64,
    pricing: &Value,
    account: &str,
    aliases: &BTreeMap<String, String>,
) -> f64 {
    let Some(pricing_obj) = pricing.as_object() else {
        return 0.0;
    };
    let mut price = None;
    // 1. 账号级：pricing.json["accounts"]，键可为账号 id 或 name
    if !account.is_empty() {
        if let Some(acc_map) = pricing_obj.get("accounts").and_then(|v| v.as_object()) {
            let mut keys: Vec<String> = vec![account.to_string()];
            if let Some(name) = aliases.get(account) {
                if !name.is_empty() {
                    keys.push(name.clone());
                }
            }
            for k in keys {
                if let Some(models) = acc_map
                    .get(&k)
                    .and_then(|v| v.get("models"))
                    .and_then(|v| v.as_object())
                {
                    if let Some(m) = models.get(model) {
                        price = Some(m);
                        break;
                    }
                }
            }
        }
    }
    // 2. 全局按模型名兜底
    if price.is_none() {
        for (pname, pdata) in pricing_obj {
            if pname.starts_with('_') || pname == "accounts" {
                continue;
            }
            if let Some(models) = pdata.get("models").and_then(|m| m.as_object()) {
                if let Some(m) = models.get(model) {
                    price = Some(m);
                    break;
                }
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
fn read_records(
    stats_dir: &Path,
    date_str: &str,
    pricing: &Value,
    aliases: &BTreeMap<String, String>,
) -> Vec<Value> {
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
            let account = rec["account"].as_str().unwrap_or("").to_string();
            rec["cost"] = json!(recalc_cost(&model, input, read, write, output, pricing, &account, aliases));
            rec
        })
        .collect()
}

/// 从原始记录按小时聚合某天数据（不再依赖容易过期的 summary 文件，
/// 保证请求数、费用与记录口径一致）。纯聚合、不读文件，
/// 由 get_stats 一次性读入当月数据后对每天复用。
fn aggregate_day(
    records: &[Value],
    service: &str,
    primary: &str,
    date_str: &str,
) -> Option<Value> {
    let mut hours: BTreeMap<String, Agg> = BTreeMap::new();
    let mut day = Agg::default();
    for rec in records {
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

/// get_stats 短时缓存（TTL 2.5s）：面板 5s 轮询 + 账号聚合每 10s 按服务各调
/// 一次，全部重读 jsonl 并遍历当月重算费用开销大，同批轮询只扫一遍文件。
/// 键含统计目录，避免不同目录/测试间串缓存。
static STATS_CACHE: Mutex<Option<(String, Instant, Value)>> = Mutex::new(None);

pub fn get_stats(
    dir: &Path,
    service: &str,
    primary: &str,
    range: &str,
    start: Option<&str>,
    end: Option<&str>,
) -> Result<Value, String> {
    let key = format!(
        "{}|{}|{}|{}|{}|{}",
        dir.to_string_lossy(),
        service,
        primary,
        range,
        start.unwrap_or(""),
        end.unwrap_or("")
    );
    if let Some((k, t, v)) = &*STATS_CACHE.lock().unwrap() {
        if t.elapsed() < Duration::from_millis(2500) && k == &key {
            let mut v = v.clone();
            // 命中缓存时刷新时间戳，面板上“更新于”仍保持接近实时
            v["updatedAt"] = json!(chrono::Local::now().to_rfc3339());
            return Ok(v);
        }
    }
    let out = get_stats_impl(dir, service, primary, range, start, end)?;
    *STATS_CACHE.lock().unwrap() = Some((key, Instant::now(), out.clone()));
    Ok(out)
}

fn get_stats_impl(
    dir: &Path,
    service: &str,
    primary: &str,
    range: &str,
    start: Option<&str>,
    end: Option<&str>,
) -> Result<Value, String> {
    let pricing = load_pricing(dir);
    let aliases = load_account_aliases(dir);
    let now = chrono::Local::now();
    let today = now.date_naive();
    let today_str = today.format("%Y-%m-%d").to_string();
    let month_prefix = now.format("%Y-%m").to_string();
    let cur_hour = now.format("%H").to_string();

    // 自定义区间：range="custom" 且 start/end 均可解析、start<=end 时生效
    let rng = if RANGE_KEYS.contains(&range) { range } else { "today" };
    let mut is_custom = false;
    let (cur, prev) = if rng == "custom" {
        let parse = |s: Option<&str>| s.and_then(|v| NaiveDate::parse_from_str(v, "%Y-%m-%d").ok());
        match (parse(start), parse(end)) {
            (Some(s), Some(e)) if s <= e => {
                is_custom = true;
                let one = chrono::Duration::days(1);
                let len = (e - s).num_days() + 1;
                ((s, e), (s - chrono::Duration::days(len), s - one))
            }
            _ => span_for("today", today),
        }
    } else {
        span_for(rng, today)
    };

    // 需要读取的日期：当前区间 + 上一区间 + 当月锚点(顶部 KPI：今日/本月)
    let mut need: BTreeSet<NaiveDate> = BTreeSet::new();
    for d in date_range(cur.0, cur.1) {
        need.insert(d);
    }
    for d in date_range(prev.0, prev.1) {
        need.insert(d);
    }
    let first_month = today.with_day(1).unwrap_or(today);
    for d in date_range(first_month, today) {
        need.insert(d);
    }

    // 逐日读取一次（按需），并按 service 过滤后缓存，供各聚合复用
    let mut by_date: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    for d in &need {
        let ds = d.format("%Y-%m-%d").to_string();
        let recs = read_records(dir, &ds, &pricing, &aliases)
            .into_iter()
            .filter(|r| matches_service(r, service, primary))
            .collect();
        by_date.insert(ds, recs);
    }
    let today_records: Vec<Value> = by_date.get(&today_str).cloned().unwrap_or_default();

    // ---- 锚点 KPI：当前小时 / 今日 / 本月（顶部统计表，固定三列） ----
    let today_day = aggregate_day(&today_records, service, primary, &today_str);
    let current = today_day
        .as_ref()
        .and_then(|t| t.get("hours").and_then(|h| h.get(&cur_hour)))
        .cloned()
        .unwrap_or_else(|| {
            json!({"hour": cur_hour, "requests":0,"input":0,"read":0,"write":0,"output":0,"cost":0,"hitRate":0,"coverage":0})
        });
    let today_obj = today_day
        .as_ref()
        .map(day_to_series)
        .unwrap_or_else(|| {
            let mut v = zero_day(&today_str);
            v["date"] = json!(today_str);
            v
        });
    let mut month_total = Agg::default();
    let mut month_days = 0usize;
    for (ds, recs) in &by_date {
        if !ds.starts_with(&month_prefix) {
            continue;
        }
        if recs.is_empty() {
            continue;
        }
        month_days += 1;
        if let Some(dd) = aggregate_day(recs, service, primary, ds) {
            if let Some(day) = dd.get("day") {
                month_total.add_agg(&agg_from_json(day));
            }
        }
    }
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

    // ---- 所选范围的图表序列 ----
    let range_records = collect_records(&by_date, &cur);
    let custom_len = if is_custom {
        (cur.1 - cur.0).num_days() + 1
    } else {
        0
    };
    let (series_kind, series): (&str, Vec<Value>) = match rng {
        "today" => (
            "minute",
            aggregate_minutes(&today_records, service, primary)
                .into_iter()
                .map(|mut v| {
                    // label 用 "HH:MM"，避免完整时间戳过长导致横轴标签重叠
                    let m = v["minute"].as_str().unwrap_or("");
                    v["label"] = json!(if m.len() >= 16 { &m[11..16] } else { m });
                    v
                })
                .collect(),
        ),
        "yesterday" => ("hour", hourly_series(&by_date, &cur)),
        "custom" if custom_len <= 1 => ("hour", hourly_series(&by_date, &cur)),
        _ => ("day", daily_series(&by_date, &cur)),
    };

    let by_model = sum_by_model(&range_records, service, primary);
    let range_agg = agg_of_records(&range_records);
    let prev_records = collect_records(&by_date, &prev);
    let prev_agg = agg_of_records(&prev_records);

    // 自定义区间的显示文案：区间文本由前端用 rangeStart/End 拼，同比文案“前 N 天”
    let range_label = if is_custom {
        "自定义"
    } else {
        range_label(rng)
    };
    let prev_label = if is_custom {
        format!("前{}天", (cur.1 - cur.0).num_days() + 1)
    } else {
        prev_label(rng).to_string()
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
            "days": month_days,
        },
        "range": rng,
        "rangeLabel": range_label,
        "prevLabel": prev_label,
        "rangeStart": cur.0.format("%Y-%m-%d").to_string(),
        "rangeEnd": cur.1.format("%Y-%m-%d").to_string(),
        "seriesKind": series_kind,
        "series": series,
        "byModel": by_model,
        "rangeAgg": range_agg,
        "prevAgg": prev_agg,
    }))
}

/// 支持的区间 key（custom 由 start/end 参数驱动）
const RANGE_KEYS: [&str; 7] = ["today", "yesterday", "week", "lastweek", "month", "lastmonth", "custom"];

fn range_label(r: &str) -> &str {
    match r {
        "yesterday" => "昨日",
        "week" => "本周",
        "lastweek" => "上周",
        "month" => "本月",
        "lastmonth" => "上月",
        _ => "今日",
    }
}

fn prev_label(r: &str) -> &str {
    match r {
        "today" => "昨日",
        "yesterday" => "前天",
        "week" => "上周",
        "lastweek" => "上上周",
        "month" => "上月",
        "lastmonth" => "上上月",
        _ => "昨日",
    }
}

/// 返回当前区间与上一等长区间（均为含首尾闭区间）
fn span_for(range: &str, today: NaiveDate) -> ((NaiveDate, NaiveDate), (NaiveDate, NaiveDate)) {
    let one = chrono::Duration::days(1);
    match range {
        "yesterday" => ((today - one, today - one), (today - one * 2, today - one * 2)),
        "week" => {
            let mon = monday_of(today);
            ((mon, today), (mon - one * 7, mon - one))
        }
        "lastweek" => {
            let mon = monday_of(today);
            ((mon - one * 7, mon - one), (mon - one * 14, mon - one * 8))
        }
        "month" => {
            let first = today.with_day(1).unwrap_or(today);
            let prev_last = first - one;
            let prev_first = prev_last.with_day(1).unwrap_or(prev_last);
            ((first, today), (prev_first, prev_last))
        }
        "lastmonth" => {
            let first = today.with_day(1).unwrap_or(today);
            let cur_last = first - one;
            let cur_first = cur_last.with_day(1).unwrap_or(cur_last);
            let prev_first = prev_month_start(cur_first);
            ((cur_first, cur_last), (prev_first, cur_first - one))
        }
        _ => ((today, today), (today - one, today - one)),
    }
}

fn monday_of(d: NaiveDate) -> NaiveDate {
    d - chrono::Duration::days(d.weekday().num_days_from_monday() as i64)
}

fn prev_month_start(d: NaiveDate) -> NaiveDate {
    let (y, m) = if d.month() == 1 {
        (d.year() - 1, 12)
    } else {
        (d.year(), d.month() - 1)
    };
    NaiveDate::from_ymd_opt(y, m, 1).unwrap_or(d)
}

fn date_range(start: NaiveDate, end: NaiveDate) -> Vec<NaiveDate> {
    let mut out = Vec::new();
    let mut d = start;
    while d <= end {
        out.push(d);
        d += chrono::Duration::days(1);
    }
    out
}

/// 收集闭区间内的所有记录（记录已按 service 过滤）
fn collect_records(by_date: &BTreeMap<String, Vec<Value>>, span: &(NaiveDate, NaiveDate)) -> Vec<Value> {
    let mut out = Vec::new();
    for d in date_range(span.0, span.1) {
        let ds = d.format("%Y-%m-%d").to_string();
        if let Some(recs) = by_date.get(&ds) {
            out.extend(recs.iter().cloned());
        }
    }
    out
}

fn agg_of_records(records: &[Value]) -> Value {
    let mut agg = Agg::default();
    for r in records {
        agg.add_record(r);
    }
    agg.to_json()
}

/// 单日逐小时序列（00..23 零值补齐），label 为 "HH"
fn hourly_series(by_date: &BTreeMap<String, Vec<Value>>, span: &(NaiveDate, NaiveDate)) -> Vec<Value> {
    let mut hours: BTreeMap<String, Agg> = BTreeMap::new();
    for d in date_range(span.0, span.1) {
        let ds = d.format("%Y-%m-%d").to_string();
        if let Some(recs) = by_date.get(&ds) {
            for rec in recs {
                let ts = rec["timestamp"].as_str().unwrap_or("");
                if ts.len() >= 13 {
                    hours.entry(ts[11..13].to_string()).or_default().add_record(rec);
                }
            }
        }
    }
    let mut out = Vec::new();
    for h in 0..24 {
        let hh = format!("{h:02}");
        match hours.get(&hh) {
            Some(agg) => {
                let mut v = agg.to_json();
                v["label"] = json!(hh);
                out.push(v);
            }
            None => out.push(json!({
                "label": hh, "requests":0,"input":0,"read":0,"write":0,"output":0,"cost":0.0,"hitRate":0.0,"coverage":0.0
            })),
        }
    }
    out
}

/// 多日逐日序列（区间内每天零值补齐），label 为 "MM-DD"
fn daily_series(by_date: &BTreeMap<String, Vec<Value>>, span: &(NaiveDate, NaiveDate)) -> Vec<Value> {
    let mut out = Vec::new();
    for d in date_range(span.0, span.1) {
        let ds = d.format("%Y-%m-%d").to_string();
        let mut v = match by_date.get(&ds) {
            Some(recs) if !recs.is_empty() => aggregate_day(recs, "", "", &ds)
                .map(|dd| day_to_series(&dd))
                .unwrap_or_else(|| zero_day(&ds)),
            _ => zero_day(&ds),
        };
        v["label"] = json!(ds[5..].to_string());
        out.push(v);
    }
    out
}

/// get_live 短时缓存（TTL 1.5s）：面板与各悬浮窗每 3s 各读一次今日 jsonl，
/// 高频轮询下避免重复全量读取。
static LIVE_CACHE: Mutex<Option<(String, Instant, Value)>> = Mutex::new(None);

pub fn get_live(dir: &Path, service: &str, primary: &str) -> Result<Value, String> {
    let key = format!("{}|{}|{}", dir.to_string_lossy(), service, primary);
    if let Some((k, t, v)) = &*LIVE_CACHE.lock().unwrap() {
        if t.elapsed() < Duration::from_millis(1500) && k == &key {
            return Ok(v.clone());
        }
    }
    let out = get_live_impl(dir, service, primary)?;
    *LIVE_CACHE.lock().unwrap() = Some((key, Instant::now(), out.clone()));
    Ok(out)
}

fn get_live_impl(dir: &Path, service: &str, primary: &str) -> Result<Value, String> {
    let pricing = load_pricing(dir);
    let aliases = load_account_aliases(dir);
    let today_str = chrono::Local::now().format("%Y-%m-%d").to_string();
    let records: Vec<Value> = read_records(&dir, &today_str, &pricing, &aliases)
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

/// get_daily 短时缓存（TTL 2.5s）：日历热力图轮询时避免重复全量读取
static DAILY_CACHE: Mutex<Option<(String, Instant, Value)>> = Mutex::new(None);

/// 返回 [start, end] 区间内每日请求数（热力图用），含 0 值补全。
pub fn get_daily(
    dir: &Path,
    service: &str,
    primary: &str,
    start: &str,
    end: &str,
) -> Result<Value, String> {
    let key = format!("{}|{}|{}|{}|{}", dir.to_string_lossy(), service, primary, start, end);
    if let Some((k, t, v)) = &*DAILY_CACHE.lock().unwrap() {
        if t.elapsed() < Duration::from_millis(2500) && k == &key {
            return Ok(v.clone());
        }
    }
    let pricing = load_pricing(dir);
    let aliases = load_account_aliases(dir);
    let s = NaiveDate::parse_from_str(start, "%Y-%m-%d").unwrap_or_else(|_| chrono::Local::now().date_naive());
    let e = NaiveDate::parse_from_str(end, "%Y-%m-%d").unwrap_or(s);
    let (s, e) = if s <= e { (s, e) } else { (e, s) };
    let mut out: Vec<Value> = Vec::new();
    for d in date_range(s, e) {
        let ds = d.format("%Y-%m-%d").to_string();
        let recs = read_records(dir, &ds, &pricing, &aliases);
        let req = recs
            .iter()
            .filter(|r| matches_service(r, service, primary))
            .count();
        out.push(json!({"date": ds, "requests": req}));
    }
    let v = json!({"daily": out});
    *DAILY_CACHE.lock().unwrap() = Some((key, Instant::now(), v.clone()));
    Ok(v)
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

        let out = get_stats(&dir.join("cache_stats"), "svc1", "svc1", "today", None, None).unwrap();
        assert_eq!(out["current"]["requests"], 3);
        assert_eq!(out["current"]["hitRate"], 0.2857142857142857);
        assert_eq!(out["today"]["requests"], 3);
        assert_eq!(out["month"]["requests"], 3);
        assert_eq!(out["monthByModel"], Value::Null);
        assert_eq!(out["range"], "today");
        assert_eq!(out["seriesKind"], "minute");
        assert_eq!(out["series"].as_array().unwrap().len(), 3);
        assert_eq!(out["byModel"][0]["model"], "m1");
        assert_eq!(out["byModel"][0]["requests"], 3);
        assert_eq!(out["rangeAgg"]["requests"], 3);
        assert_eq!(out["prevAgg"]["requests"], 0);

        // 昨日：逐小时序列 24 点
        let y = get_stats(&dir.join("cache_stats"), "svc1", "svc1", "yesterday", None, None).unwrap();
        assert_eq!(y["range"], "yesterday");
        assert_eq!(y["seriesKind"], "hour");
        assert_eq!(y["series"].as_array().unwrap().len(), 24);
        assert_eq!(y["series"][0]["label"], "00");
        assert_eq!(y["rangeAgg"]["requests"], 0);

        // 本月：逐日序列
        let m = get_stats(&dir.join("cache_stats"), "svc1", "svc1", "month", None, None).unwrap();
        assert_eq!(m["seriesKind"], "day");
        assert!(m["series"].as_array().unwrap().len() >= 1);

        // 上周 / 上月：同样为逐日序列，无数据时请求数为 0
        let lw = get_stats(&dir.join("cache_stats"), "svc1", "svc1", "lastweek", None, None).unwrap();
        assert_eq!(lw["seriesKind"], "day");
        assert_eq!(lw["rangeAgg"]["requests"], 0);
        let lm = get_stats(&dir.join("cache_stats"), "svc1", "svc1", "lastmonth", None, None).unwrap();
        assert_eq!(lm["seriesKind"], "day");
        assert_eq!(lm["rangeAgg"]["requests"], 0);

        // 自定义区间：跨多天 → 逐日；单日 → 逐小时
        let d = now.date_naive();
        let ds = d.format("%Y-%m-%d").to_string();
        let prev_ds = (d - chrono::Duration::days(1)).format("%Y-%m-%d").to_string();
        let c = get_stats(
            &dir.join("cache_stats"),
            "svc1",
            "svc1",
            "custom",
            Some(&prev_ds),
            Some(&ds),
        )
        .unwrap();
        assert_eq!(c["range"], "custom");
        assert_eq!(c["seriesKind"], "day");
        assert_eq!(c["rangeAgg"]["requests"], 3);
        assert_eq!(c["prevLabel"], "前2天");
        assert_eq!(c["series"].as_array().unwrap().len(), 2);
        let c1 = get_stats(
            &dir.join("cache_stats"),
            "svc1",
            "svc1",
            "custom",
            Some(&ds),
            Some(&ds),
        )
        .unwrap();
        assert_eq!(c1["seriesKind"], "hour");
        assert_eq!(c1["series"].as_array().unwrap().len(), 24);

        // 热力图：每日请求数
        let dl = get_daily(&dir.join("cache_stats"), "svc1", "svc1", &prev_ds, &ds).unwrap();
        let arr = dl["daily"].as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[1]["requests"], 3);

        let live = get_live(&dir.join("cache_stats"), "svc1", "svc1").unwrap();
        assert_eq!(live["records"].as_array().unwrap().len(), 3);

        let _ = fs::remove_dir_all(&dir);
    }

}
