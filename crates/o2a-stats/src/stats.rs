//! CacheStats：记录（JSONL）、小时聚合、费用、清理。
//!
//! 行为基准：Python o2a/stats.py（逐字段/逐分支对齐，差异见各处注释与
//! docs/rust-rewrite.md §8）。

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::{DateTime, Local, Timelike};
use serde_json::{json, Map, Value};

use o2a_pricing::resolve_cost;

use crate::pyround::{py_round, thousands};

/// 单次请求 usage 输入（Anthropic 语义字段，缺省 0）。
#[derive(Debug, Clone, Copy, Default)]
pub struct Usage {
    pub input_tokens: i64,
    pub cache_read_input_tokens: i64,
    pub cache_creation_input_tokens: i64,
    pub output_tokens: i64,
}

impl Usage {
    /// 从 serde_json usage 对象构造（引擎侧 _convert_usage 产物；缺省 0）。
    pub fn from_value(v: &Value) -> Self {
        let get = |k: &str| v.get(k).and_then(|x| x.as_i64()).unwrap_or(0);
        Usage {
            input_tokens: get("input_tokens"),
            cache_read_input_tokens: get("cache_read_input_tokens"),
            cache_creation_input_tokens: get("cache_creation_input_tokens"),
            output_tokens: get("output_tokens"),
        }
    }
    fn is_empty(&self) -> bool {
        self.input_tokens == 0
            && self.cache_read_input_tokens == 0
            && self.cache_creation_input_tokens == 0
            && self.output_tokens == 0
    }
}

/// 构造参数（对齐 Python CacheStats.__init__；account_name 与 pricing_path 为
/// Rust 侧显式传入，Python 分别经 load_config 反查与 PROJECT_ROOT 定位）。
#[derive(Debug, Clone)]
pub struct CacheStatsConfig {
    pub stats_dir: PathBuf,
    pub retention_days: i64,
    pub service: String,
    pub service_id: String,
    /// 账号 id（计费 account_keys 首位）
    pub account: String,
    /// 账号显示名（计费 account_keys 次位回退）
    pub account_name: String,
    /// pricing_mode != "token" 时 true（订阅制/免费不记价格）
    pub no_cost: bool,
    /// pricing.json 路径；None → O2A_PRICING env → cwd/pricing.json
    pub pricing_path: Option<PathBuf>,
}

pub struct CacheStats {
    cfg: CacheStatsConfig,
    /// 对应 Python self._lock（record / get_summary 串行化）
    lock: Mutex<()>,
    pricing_cache: Mutex<Option<(PricingSig, Value)>>,
    month_cum_cache: Mutex<Option<MonthCumCache>>,
    last_hour: Mutex<Option<String>>,
}

#[derive(Clone, Copy, PartialEq)]
struct PricingSig(u128, u64);

struct MonthCumCache {
    /// (月, 各文件 (路径, mtime 纳秒, size) 签名)
    key: (String, Vec<(PathBuf, i128, u64)>),
    value: i64,
}

fn pricing_path_resolve(explicit: Option<&Path>) -> PathBuf {
    if let Some(p) = explicit {
        return p.to_path_buf();
    }
    if let Ok(env) = std::env::var("O2A_PRICING") {
        let p = PathBuf::from(env.trim());
        if !p.as_os_str().is_empty() {
            return p;
        }
    }
    PathBuf::from("pricing.json")
}

fn now_local() -> DateTime<Local> {
    Local::now()
}

fn fmt_ts(dt: DateTime<Local>) -> String {
    dt.format("%Y-%m-%dT%H:%M:%S").to_string()
}

impl CacheStats {
    pub fn new(cfg: CacheStatsConfig) -> Self {
        let s = Self {
            cfg,
            lock: Mutex::new(()),
            pricing_cache: Mutex::new(None),
            month_cum_cache: Mutex::new(None),
            last_hour: Mutex::new(None),
        };
        let _ = fs::create_dir_all(s.summary_root());
        s.cleanup_old_files();
        s
    }

    pub fn config(&self) -> &CacheStatsConfig {
        &self.cfg
    }

    /// summary 写入目录：优先按服务 id，无 id 时按服务名（历史行为）。
    pub fn summary_root(&self) -> PathBuf {
        let root = self.cfg.stats_dir.join("summary");
        if !self.cfg.service_id.is_empty() {
            return root.join(&self.cfg.service_id);
        }
        if !self.cfg.service.is_empty() {
            return root.join(&self.cfg.service);
        }
        root
    }

    /// summary 读取目录列表：id 目录优先，名字目录兜底（历史数据双查），去重保序。
    pub fn summary_read_dirs(&self) -> Vec<PathBuf> {
        let root = self.cfg.stats_dir.join("summary");
        let mut dirs = Vec::new();
        if !self.cfg.service_id.is_empty() {
            dirs.push(root.join(&self.cfg.service_id));
        }
        if !self.cfg.service.is_empty() {
            dirs.push(root.join(&self.cfg.service));
        }
        if dirs.is_empty() {
            dirs.push(root);
        }
        let mut seen = Vec::new();
        dirs.into_iter().filter(|d| {
            if seen.contains(d) {
                false
            } else {
                seen.push(d.clone());
                true
            }
        }).collect()
    }

    /// 启动时清理超过保留天数的文件（含 summary 子目录）。
    fn cleanup_old_files(&self) {
        let cutoff = std::time::SystemTime::now()
            - std::time::Duration::from_secs((self.cfg.retention_days.max(0) as u64) * 86400);
        let mut dirs = vec![self.cfg.stats_dir.clone(), self.cfg.stats_dir.join("summary")];
        if let Ok(entries) = fs::read_dir(self.cfg.stats_dir.join("summary")) {
            for e in entries.flatten() {
                if e.path().is_dir() {
                    dirs.push(e.path());
                }
            }
        }
        for dir in dirs {
            let Ok(entries) = fs::read_dir(&dir) else { continue };
            for e in entries.flatten() {
                let p = e.path();
                let keep = matches!(p.extension().and_then(|x| x.to_str()), Some("jsonl" | "json"));
                if !keep {
                    continue;
                }
                let Ok(meta) = e.metadata() else { continue };
                let Ok(mtime) = meta.modified() else { continue };
                if mtime < cutoff {
                    let _ = fs::remove_file(&p);
                    tracing::info!("[CACHE] Cleaned up old file: {}", p.display());
                }
            }
        }
    }

    /// 加载定价数据（mtime+size 签名缓存，热加载；对齐 Python _load_pricing）。
    pub fn load_pricing(&self) -> Value {
        let path = pricing_path_resolve(self.cfg.pricing_path.as_deref());
        let sig = fs::metadata(&path).ok().and_then(|m| {
            m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| {
                PricingSig(d.as_nanos(), m.len())
            })
        });
        let mut cache = self.pricing_cache.lock().unwrap();
        if let (Some(s), Some((cached_sig, value))) = (sig, cache.as_ref()) {
            if *cached_sig == s {
                return value.clone();
            }
        }
        // 读取成功 → 新值；失败 → 从未加载过则落空表，否则沿用旧缓存（对齐 Python）
        let value = fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<Value>(&s).ok());
        match value {
            Some(v) => {
                if let Some(s) = sig {
                    *cache = Some((s, v.clone()));
                }
                v
            }
            None => match cache.as_ref() {
                Some((_, v)) => v.clone(),
                None => Value::Object(Map::new()),
            },
        }
    }

    /// 本月早于 before_ts 的该账号 tokens 总和（(月, 文件签名) 缓存；free_quota /
    /// cumulative_tier 冲抵口径：input+cache_read+cache_write+output，ts 严格早于）。
    pub fn month_cumulative_tokens(&self, before_ts: &str) -> i64 {
        let now = if before_ts.len() >= 10 { &before_ts[..10] } else { "" };
        let month = if now.len() >= 7 { now[..7].to_string() } else { fmt_ts(now_local())[..7].to_string() };
        // 文件集合 + 签名
        let mut files: Vec<PathBuf> = Vec::new();
        if let Ok(entries) = fs::read_dir(&self.cfg.stats_dir) {
            let prefix = format!("{month}-");
            for e in entries.flatten() {
                let p = e.path();
                let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if name.starts_with(&prefix) && name.ends_with(".jsonl") {
                    files.push(p);
                }
            }
        }
        files.sort();
        let mut sig: Vec<(PathBuf, i128, u64)> = Vec::new();
        for f in &files {
            if let Ok(meta) = fs::metadata(f) {
                let ns = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_nanos() as i128)
                    .unwrap_or(0);
                sig.push((f.clone(), ns, meta.len()));
            }
        }
        {
            let cache = self.month_cum_cache.lock().unwrap();
            if let Some(c) = cache.as_ref() {
                if c.key.0 == month && c.key.1 == sig {
                    return c.value;
                }
            }
        }
        let mut total = 0i64;
        for f in &files {
            let Ok(content) = fs::read_to_string(f) else { continue };
            for line in content.lines() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let Ok(rec) = serde_json::from_str::<Value>(line) else { continue };
                let rts = rec.get("timestamp").and_then(|v| v.as_str()).unwrap_or("");
                if rts >= before_ts {
                    continue; // 字典序 = 时间序（严格早于才计入）
                }
                if rec.get("account").and_then(|v| v.as_str()).unwrap_or("") != self.cfg.account {
                    continue; // free_quota 按账号口径冲抵
                }
                let get = |k: &str| rec.get(k).and_then(|v| v.as_i64()).unwrap_or(0);
                total += get("input_tokens")
                    + get("cache_read_tokens")
                    + get("cache_write_tokens")
                    + get("output_tokens");
            }
        }
        *self.month_cum_cache.lock().unwrap() = Some(MonthCumCache { key: (month, sig), value: total });
        total
    }

    /// 计算缓存命中率和覆盖率。
    pub fn compute_rates(input_tokens: i64, cache_read: i64, cache_write: i64) -> (f64, f64) {
        let denom_hit = cache_read + input_tokens;
        let cache_hit_rate = if denom_hit > 0 { cache_read as f64 / denom_hit as f64 } else { 0.0 };
        let denom_cov = cache_read + input_tokens + cache_write;
        let cache_coverage = if denom_cov > 0 { cache_read as f64 / denom_cov as f64 } else { 0.0 };
        (cache_hit_rate, cache_coverage)
    }

    fn calc_cost(&self, model: &str, usage: &Usage, timestamp: &str, batch: bool) -> f64 {
        let pricing = self.load_pricing();
        if pricing.as_object().map(|o| o.is_empty()).unwrap_or(true) {
            return 0.0;
        }
        // account_keys：id 优先，name 兜底（v1 accounts 段语义）
        let mut keys: Vec<String> = Vec::new();
        if !self.cfg.account.is_empty() {
            keys.push(self.cfg.account.clone());
            if !self.cfg.account_name.is_empty() {
                keys.push(self.cfg.account_name.clone());
            }
        }
        let cumulative = self.month_cumulative_tokens(timestamp);
        let mut meta = Map::new();
        meta.insert(
            "context_tokens".into(),
            json!(usage.input_tokens + usage.cache_read_input_tokens + usage.cache_creation_input_tokens),
        );
        if batch {
            meta.insert("batch".into(), json!(true));
        }
        let mut ctx = Map::new();
        if !timestamp.is_empty() {
            ctx.insert("timestamp".into(), json!(timestamp));
        }
        ctx.insert("meta".into(), Value::Object(meta));
        ctx.insert("cumulative".into(), json!({ "tokens": cumulative }));
        resolve_cost(
            &pricing,
            model,
            usage.input_tokens,
            usage.cache_read_input_tokens,
            usage.cache_creation_input_tokens,
            usage.output_tokens,
            &keys,
            &self.cfg.service_id,
            Some(&Value::Object(ctx)),
        )
    }

    /// 构建一条统计记录（字段全集与可选字段条件对齐 Python _build_record）。
    pub fn build_record(
        &self,
        model: &str,
        usage: &Usage,
        error: Option<&str>,
        meta: Option<&Value>,
        upstream_model: Option<&str>,
        batch: bool,
    ) -> Value {
        let (hit_rate, coverage) = Self::compute_rates(
            usage.input_tokens,
            usage.cache_read_input_tokens,
            usage.cache_creation_input_tokens,
        );
        let ts = fmt_ts(now_local());
        // 计价模型：upstream_model 优先（对齐 Python _calc_cost(upstream_model or model)）
        let cost_model = upstream_model
            .filter(|m| !m.is_empty())
            .unwrap_or(model);
        let cost = if self.cfg.no_cost {
            0.0
        } else {
            self.calc_cost(cost_model, usage, &ts, batch)
        };
        let mut rec = Map::new();
        rec.insert("timestamp".into(), json!(ts));
        rec.insert("service".into(), json!(self.cfg.service));
        rec.insert("account".into(), json!(self.cfg.account));
        rec.insert("model".into(), json!(model));
        rec.insert("status".into(), json!(if error.is_some() { "error" } else { "ok" }));
        rec.insert("input_tokens".into(), json!(usage.input_tokens));
        rec.insert("cache_read_tokens".into(), json!(usage.cache_read_input_tokens));
        rec.insert("cache_write_tokens".into(), json!(usage.cache_creation_input_tokens));
        rec.insert("output_tokens".into(), json!(usage.output_tokens));
        rec.insert("cache_hit_rate".into(), json!(py_round(hit_rate, 4)));
        rec.insert("cache_coverage".into(), json!(py_round(coverage, 4)));
        rec.insert("cost".into(), json!(py_round(cost, 6)));
        if !self.cfg.service_id.is_empty() {
            rec.insert("service_id".into(), json!(self.cfg.service_id));
        }
        if batch {
            rec.insert("batch".into(), json!(true));
        }
        if let Some(up) = upstream_model {
            if !up.is_empty() && up != model {
                rec.insert("upstream_model".into(), json!(up));
            }
        }
        if let Some(err) = error {
            rec.insert("error".into(), json!(err));
        }
        if let Some(meta) = meta {
            for key in ["duration_ms", "first_token_ms", "output_tokens_per_sec"] {
                if let Some(v) = meta.get(key).and_then(|x| x.as_f64()) {
                    rec.insert(key.into(), json!(py_round(v, 2)));
                }
            }
        }
        Value::Object(rec)
    }

    fn format_log(rec: &Value) -> String {
        let hit = rec["cache_hit_rate"].as_f64().unwrap_or(0.0) * 100.0;
        let get = |k: &str| rec[k].as_i64().unwrap_or(0);
        format!(
            "[CACHE] {} hit={:.1}% read={} write={} input={} out={}",
            rec["model"].as_str().unwrap_or(""),
            hit,
            thousands(get("cache_read_tokens")),
            thousands(get("cache_write_tokens")),
            thousands(get("input_tokens")),
            thousands(get("output_tokens")),
        )
    }

    /// 记录一次请求的缓存统计（成功或失败）。
    pub fn record(
        &self,
        model: &str,
        usage: &Usage,
        error: Option<&str>,
        meta: Option<&Value>,
        upstream_model: Option<&str>,
        batch: bool,
    ) {
        let _guard = self.lock.lock().unwrap();
        if usage.is_empty() && error.is_none() {
            return;
        }
        let rec = self.build_record(model, usage, error, meta, upstream_model, batch);
        let ts = rec["timestamp"].as_str().unwrap_or("").to_string();

        // 写入 JSONL（追加写单行；不做跨进程文件锁 —— Python 用 fcntl，Rust 引擎为
        // 单进程模型且记录行 < 4KB，主流平台 O_APPEND 小写入具备原子性；
        // 桌面端对 JSONL 只读，无并发写方，差异已确认可接受）
        let filepath = self.cfg.stats_dir.join(format!("{}.jsonl", &ts[..10]));
        match fs::OpenOptions::new().create(true).append(true).open(&filepath) {
            Ok(mut f) => {
                if let Err(e) = writeln!(f, "{rec}") {
                    tracing::warn!("[CACHE] Failed to write record: {e}");
                }
            }
            Err(e) => tracing::warn!("[CACHE] Failed to write record: {e}"),
        }

        // 懒检查：跨小时则打印上一小时汇总
        let current_hour = ts[..13].to_string();
        let prev = {
            let mut last = self.last_hour.lock().unwrap();
            let prev = last.clone().filter(|p| p != &current_hour);
            *last = Some(current_hour);
            prev
        };
        if let Some(prev) = prev {
            self.print_hourly_summary(&prev);
        }

        // 更新小时聚合
        self.update_hourly_summary(&rec);

        tracing::info!("{}", Self::format_log(&rec));
    }

    fn update_hourly_summary(&self, rec: &Value) {
        let ts = rec["timestamp"].as_str().unwrap_or("");
        if ts.len() < 13 {
            return;
        }
        let (date_str, hour_str) = (&ts[..10], &ts[11..13]);
        let summary_path = self.summary_root().join(format!("{date_str}.json"));

        let mut summary: Value = fs::read_to_string(&summary_path)
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_else(|| json!({}));

        if summary.get("hours").is_none() {
            summary["date"] = json!(date_str);
            summary["hours"] = json!({});
        }
        let h = &mut summary["hours"][hour_str];
        if !h.is_object() {
            *h = json!({
                "requests": 0,
                "total_input_tokens": 0,
                "total_cache_read_tokens": 0,
                "total_cache_write_tokens": 0,
                "total_output_tokens": 0,
                "total_cost": 0.0,
                "_hit_rate_sum": 0.0,
                "_coverage_sum": 0.0,
            });
        }
        let get_f = |k: &str| h[k].as_f64().unwrap_or(0.0);
        let get_i = |k: &str| h[k].as_i64().unwrap_or(0);
        *h = json!({
            "requests": get_i("requests") + 1,
            "total_input_tokens": get_i("total_input_tokens") + rec["input_tokens"].as_i64().unwrap_or(0),
            "total_cache_read_tokens": get_i("total_cache_read_tokens") + rec["cache_read_tokens"].as_i64().unwrap_or(0),
            "total_cache_write_tokens": get_i("total_cache_write_tokens") + rec["cache_write_tokens"].as_i64().unwrap_or(0),
            "total_output_tokens": get_i("total_output_tokens") + rec["output_tokens"].as_i64().unwrap_or(0),
            "total_cost": get_f("total_cost") + rec["cost"].as_f64().unwrap_or(0.0),
            "_hit_rate_sum": get_f("_hit_rate_sum") + rec["cache_hit_rate"].as_f64().unwrap_or(0.0),
            "_coverage_sum": get_f("_coverage_sum") + rec["cache_coverage"].as_f64().unwrap_or(0.0),
        });

        if let Ok(mut f) = fs::File::create(&summary_path) {
            if let Err(e) = writeln!(f, "{summary}") {
                tracing::warn!("[CACHE] Failed to write summary: {e}");
            }
        } else {
            tracing::warn!("[CACHE] Failed to write summary: {}", summary_path.display());
        }
    }

    fn print_hourly_summary(&self, hour_str: &str) {
        if hour_str.len() < 13 {
            return;
        }
        let date_str = &hour_str[..10];
        let hour = &hour_str[11..13];
        let mut summary: Option<Value> = None;
        for d in self.summary_read_dirs() {
            let p = d.join(format!("{date_str}.json"));
            if let Ok(raw) = fs::read_to_string(&p) {
                if let Ok(v) = serde_json::from_str::<Value>(&raw) {
                    summary = Some(v);
                    break;
                }
            }
        }
        let Some(summary) = summary else { return };
        let Some(h) = summary.get("hours").and_then(|hs| hs.get(hour)) else { return };
        let req = h["requests"].as_i64().unwrap_or(0);
        if req > 0 {
            let avg_hit = h["_hit_rate_sum"].as_f64().unwrap_or(0.0) / req as f64 * 100.0;
            let get = |k: &str| h[k].as_i64().unwrap_or(0);
            tracing::info!(
                "[CACHE HOURLY {date_str}T{hour}] requests={req} avg_hit={avg_hit:.1}% total_read={} total_write={} total_input={}",
                thousands(get("total_cache_read_tokens")),
                thousands(get("total_cache_write_tokens")),
                thousands(get("total_input_tokens")),
            );
        }
    }

    /// 返回聚合统计（hour | day | all）。
    pub fn get_summary(&self, period: &str) -> Value {
        let _guard = self.lock.lock().unwrap();
        match period {
            "hour" => self.get_last_hour_summary(),
            "day" => self.get_day_summary(),
            "all" => self.get_all_summary(),
            other => json!({ "error": format!("unknown period: {other}") }),
        }
    }

    /// 记录是否属于本实例（服务 id/名/默认兜底；与 Rust 读取端同一规则）。
    pub fn matches_record(&self, rec: &Value) -> bool {
        let sid = rec.get("service_id").and_then(|v| v.as_str()).unwrap_or("");
        let rsvc = rec.get("service").and_then(|v| v.as_str()).unwrap_or("");
        if !self.cfg.service_id.is_empty() && sid == self.cfg.service_id {
            return true;
        }
        if !self.cfg.service.is_empty() && rsvc == self.cfg.service {
            return true;
        }
        if self.cfg.service_id.is_empty() && self.cfg.service.is_empty() {
            return sid.is_empty() && rsvc.is_empty();
        }
        false
    }

    /// 按当前 pricing 重放单条记录费用（写入侧 cost 是快照，读取端以重算为准）。
    fn replay_record_cost(&self, rec: &Value) -> f64 {
        if self.cfg.no_cost {
            return 0.0;
        }
        let model = rec
            .get("upstream_model")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .or_else(|| rec.get("model").and_then(|v| v.as_str()))
            .unwrap_or("");
        let account = if self.cfg.account.is_empty() {
            rec.get("account").and_then(|v| v.as_str()).unwrap_or("")
        } else {
            &self.cfg.account
        };
        let get = |k: &str| rec.get(k).and_then(|v| v.as_i64()).unwrap_or(0);
        let usage = Usage {
            input_tokens: get("input_tokens"),
            cache_read_input_tokens: get("cache_read_tokens"),
            cache_creation_input_tokens: get("cache_write_tokens"),
            output_tokens: get("output_tokens"),
        };
        let ts = rec.get("timestamp").and_then(|v| v.as_str()).unwrap_or("");
        let batch = rec.get("batch").and_then(|v| v.as_bool()).unwrap_or(false);
        // 与 self.calc_cost 相同的 ctx，但 account 可能来自历史记录
        let pricing = self.load_pricing();
        if pricing.as_object().map(|o| o.is_empty()).unwrap_or(true) {
            return 0.0;
        }
        let mut keys: Vec<String> = Vec::new();
        if !account.is_empty() {
            keys.push(account.to_string());
            if account == self.cfg.account && !self.cfg.account_name.is_empty() {
                keys.push(self.cfg.account_name.clone());
            }
        }
        let cumulative = self.month_cumulative_tokens(ts);
        let mut meta = Map::new();
        meta.insert(
            "context_tokens".into(),
            json!(usage.input_tokens + usage.cache_read_input_tokens + usage.cache_creation_input_tokens),
        );
        if batch {
            meta.insert("batch".into(), json!(true));
        }
        let mut ctx = Map::new();
        if !ts.is_empty() {
            ctx.insert("timestamp".into(), json!(ts));
        }
        ctx.insert("meta".into(), Value::Object(meta));
        ctx.insert("cumulative".into(), json!({ "tokens": cumulative }));
        resolve_cost(
            &pricing,
            model,
            usage.input_tokens,
            usage.cache_read_input_tokens,
            usage.cache_creation_input_tokens,
            usage.output_tokens,
            &keys,
            &self.cfg.service_id,
            Some(&Value::Object(ctx)),
        )
    }

    /// 某天的聚合统计（从原始 JSONL 重放费用）。
    pub fn load_day_summary(&self, date_str: &str) -> Option<Value> {
        let path = self.cfg.stats_dir.join(format!("{date_str}.jsonl"));
        let mut records: Vec<Value> = Vec::new();
        let content = match fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => return self.load_day_summary_file(date_str),
        };
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let Ok(rec) = serde_json::from_str::<Value>(line) else { continue };
            if rec.get("status").and_then(|v| v.as_str()) == Some("error") || !self.matches_record(&rec) {
                continue;
            }
            records.push(rec);
        }
        if records.is_empty() {
            return None;
        }

        let mut hours: HashMap<String, [f64; 8]> = HashMap::new(); // req, in, read, write, out, cost, hit_sum, cov_sum
        let mut day = [0f64; 8];
        for rec in &records {
            let ts = rec.get("timestamp").and_then(|v| v.as_str()).unwrap_or("");
            let hour = if ts.len() >= 13 { &ts[11..13] } else { "" };
            let hour_cost = self.replay_record_cost(rec);
            let vals = [
                1.0,
                rec["input_tokens"].as_f64().unwrap_or(0.0),
                rec["cache_read_tokens"].as_f64().unwrap_or(0.0),
                rec["cache_write_tokens"].as_f64().unwrap_or(0.0),
                rec["output_tokens"].as_f64().unwrap_or(0.0),
                hour_cost,
                rec["cache_hit_rate"].as_f64().unwrap_or(0.0),
                rec["cache_coverage"].as_f64().unwrap_or(0.0),
            ];
            let h = hours.entry(hour.to_string()).or_insert([0.0; 8]);
            for i in 0..8 {
                h[i] += vals[i];
                day[i] += vals[i];
            }
        }

        let mut hours_list: Vec<Value> = Vec::new();
        let mut keys: Vec<&String> = hours.keys().collect();
        keys.sort();
        for hour in keys {
            let h = &hours[hour.as_str()];
            let req = h[0] as i64;
            hours_list.push(json!({
                "hour": format!("{date_str}T{hour}:00:00"),
                "requests": req,
                "avg_cache_hit_rate": py_round(h[6] / req as f64, 4),
                "avg_cache_coverage": py_round(h[7] / req as f64, 4),
                "total_cache_read_tokens": h[2] as i64,
                "total_cache_write_tokens": h[3] as i64,
                "total_input_tokens": h[1] as i64,
                "total_output_tokens": h[4] as i64,
                "total_cost": py_round(h[5], 6),
            }));
        }
        let denom_hit = day[2] + day[1];
        let denom_cov = denom_hit + day[3];
        Some(json!({
            "date": date_str,
            "hours": hours_list,
            "daily_total": {
                "requests": day[0] as i64,
                "total_input_tokens": day[1] as i64,
                "total_cache_read_tokens": day[2] as i64,
                "total_cache_write_tokens": day[3] as i64,
                "total_output_tokens": day[4] as i64,
                "total_cost": py_round(day[5], 6),
                "avg_cache_hit_rate": if denom_hit > 0.0 { py_round(day[2] / denom_hit, 4) } else { 0.0 },
                "avg_cache_coverage": if denom_cov > 0.0 { py_round(day[2] / denom_cov, 4) } else { 0.0 },
            }
        }))
    }

    /// 回退：读取旧 summary JSON（id 目录优先，名字目录兜底），清内部字段。
    fn load_day_summary_file(&self, date_str: &str) -> Option<Value> {
        let mut summary: Option<Value> = None;
        for d in self.summary_read_dirs() {
            let summary_path = d.join(format!("{date_str}.json"));
            if let Ok(raw) = fs::read_to_string(&summary_path) {
                if let Ok(v) = serde_json::from_str::<Value>(&raw) {
                    summary = Some(v);
                    break;
                }
            }
        }
        let summary = summary?;

        let mut hours_list: Vec<Value> = Vec::new();
        let mut daily = [0f64; 6]; // req, in, read, write, out, cost
        let hours = summary.get("hours").and_then(|v| v.as_object()).cloned().unwrap_or_default();
        let mut keys: Vec<&String> = hours.keys().collect();
        keys.sort();
        for hour in keys {
            let h = &hours[hour.as_str()];
            let req = h["requests"].as_f64().unwrap_or(0.0);
            let hour_cost = h["total_cost"].as_f64().unwrap_or(0.0);
            let get_i = |k: &str| h[k].as_f64().unwrap_or(0.0);
            hours_list.push(json!({
                "hour": format!("{date_str}T{hour}:00:00"),
                "requests": req as i64,
                "avg_cache_hit_rate": py_round(h["_hit_rate_sum"].as_f64().unwrap_or(0.0) / req, 4),
                "avg_cache_coverage": py_round(h["_coverage_sum"].as_f64().unwrap_or(0.0) / req, 4),
                "total_cache_read_tokens": get_i("total_cache_read_tokens") as i64,
                "total_cache_write_tokens": get_i("total_cache_write_tokens") as i64,
                "total_input_tokens": get_i("total_input_tokens") as i64,
                "total_output_tokens": get_i("total_output_tokens") as i64,
                "total_cost": py_round(hour_cost, 6),
            }));
            daily[0] += req;
            daily[1] += get_i("total_input_tokens");
            daily[2] += get_i("total_cache_read_tokens");
            daily[3] += get_i("total_cache_write_tokens");
            daily[4] += get_i("total_output_tokens");
            daily[5] += hour_cost;
        }
        let denom_hit = daily[2] + daily[1];
        let denom_cov = denom_hit + daily[3];
        Some(json!({
            "date": date_str,
            "hours": hours_list,
            "daily_total": {
                "requests": daily[0] as i64,
                "total_input_tokens": daily[1] as i64,
                "total_cache_read_tokens": daily[2] as i64,
                "total_cache_write_tokens": daily[3] as i64,
                "total_output_tokens": daily[4] as i64,
                "total_cost": py_round(daily[5], 6),
                "avg_cache_hit_rate": if denom_hit > 0.0 { py_round(daily[2] / denom_hit, 4) } else { 0.0 },
                "avg_cache_coverage": if denom_cov > 0.0 { py_round(daily[2] / denom_cov, 4) } else { 0.0 },
            }
        }))
    }

    fn get_last_hour_summary(&self) -> Value {
        let now = now_local();
        let date_str = fmt_ts(now)[..10].to_string();
        let hour_str = format!("{:02}", now.hour());
        let Some(day_data) = self.load_day_summary(&date_str) else {
            return json!({ "period": "hour", "hour": format!("{date_str}T{hour_str}"), "requests": 0 });
        };
        for h in day_data["hours"].as_array().map(|a| a.as_slice()).unwrap_or(&[]) {
            let hour_label = h["hour"].as_str().unwrap_or("");
            if hour_label.len() >= 13 && hour_label[11..13] == *hour_str {
                let mut out = Map::new();
                out.insert("period".into(), json!("hour"));
                if let Some(obj) = h.as_object() {
                    for (k, v) in obj {
                        out.insert(k.clone(), v.clone());
                    }
                }
                return Value::Object(out);
            }
        }
        json!({ "period": "hour", "hour": format!("{date_str}T{hour_str}"), "requests": 0 })
    }

    fn get_day_summary(&self) -> Value {
        let now = now_local();
        let date_str = fmt_ts(now)[..10].to_string();
        match self.load_day_summary(&date_str) {
            None => json!({ "period": "day", "date": date_str, "requests": 0 }),
            Some(day_data) => {
                let mut out = Map::new();
                out.insert("period".into(), json!("day"));
                if let Some(obj) = day_data.as_object() {
                    for (k, v) in obj {
                        out.insert(k.clone(), v.clone());
                    }
                }
                Value::Object(out)
            }
        }
    }

    fn get_all_summary(&self) -> Value {
        // 日期并集：id 目录与名字目录的 *.json 文件名（去重保序后排序）
        let mut dates: Vec<String> = Vec::new();
        for d in self.summary_read_dirs() {
            let Ok(entries) = fs::read_dir(&d) else { continue };
            let mut names: Vec<String> = entries
                .flatten()
                .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("json"))
                .filter_map(|e| {
                    e.file_name().into_string().ok().map(|n| n.trim_end_matches(".json").to_string())
                })
                .collect();
            names.sort();
            for n in names {
                if !dates.contains(&n) {
                    dates.push(n);
                }
            }
        }
        dates.sort();
        let mut days: Vec<Value> = Vec::new();
        let mut total = [0f64; 6];
        for date_str in &dates {
            if let Some(day_data) = self.load_day_summary(date_str) {
                if let Some(dt) = day_data.get("daily_total") {
                    total[0] += dt["requests"].as_f64().unwrap_or(0.0);
                    total[1] += dt["total_input_tokens"].as_f64().unwrap_or(0.0);
                    total[2] += dt["total_cache_read_tokens"].as_f64().unwrap_or(0.0);
                    total[3] += dt["total_cache_write_tokens"].as_f64().unwrap_or(0.0);
                    total[4] += dt["total_output_tokens"].as_f64().unwrap_or(0.0);
                    total[5] += dt["total_cost"].as_f64().unwrap_or(0.0);
                }
                days.push(day_data);
            }
        }
        let denom_hit = total[2] + total[1];
        let denom_cov = denom_hit + total[3];
        json!({
            "period": "all",
            "days": days,
            "total": {
                "requests": total[0] as i64,
                "total_input_tokens": total[1] as i64,
                "total_cache_read_tokens": total[2] as i64,
                "total_cache_write_tokens": total[3] as i64,
                "total_output_tokens": total[4] as i64,
                "total_cost": py_round(total[5], 6),
                "avg_cache_hit_rate": if denom_hit > 0.0 { py_round(total[2] / denom_hit, 4) } else { 0.0 },
                "avg_cache_coverage": if denom_cov > 0.0 { py_round(total[2] / denom_cov, 4) } else { 0.0 },
            }
        })
    }

    /// 当天日期字符串（本地时间，YYYY-MM-DD）。
    pub fn today(&self) -> String {
        fmt_ts(now_local())[..10].to_string()
    }

    /// 当前小时（本地时间，HH）。
    pub fn current_hour(&self) -> String {
        let now = now_local();
        format!("{:02}", now.hour())
    }

    /// 当前完整时间戳（本地时间）。
    pub fn now_ts(&self) -> String {
        fmt_ts(now_local())
    }

    /// 供 /pricing-reload：清空定价与月累计缓存（下次读取自动重算）。
    pub fn clear_pricing_cache(&self) {
        *self.pricing_cache.lock().unwrap() = None;
        *self.month_cum_cache.lock().unwrap() = None;
    }
}

