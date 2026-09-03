//! 额度基础设施：上下文、快照结构、适配器协议（对齐 Python `o2a/quota/base.py`）。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{DateTime, Datelike, Local, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use o2a_config::Account;
use serde_json::{json, Value};

/// 上游请求超时（秒）：永不阻塞主流程（对齐 Python `UPSTREAM_TIMEOUT_S = 1.5`）。
pub const UPSTREAM_TIMEOUT_S: f64 = 1.5;

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct QuotaError(pub String);

impl QuotaError {
    pub fn new(msg: impl Into<String>) -> Self {
        QuotaError(msg.into())
    }
}

/// 注入时钟（对齐 Python `now_fn`）：返回 naive 本地时间（Python `datetime.now()` 即 naive）。
pub type NowFn = Arc<dyn Fn() -> NaiveDateTime + Send + Sync>;

pub fn default_now() -> NaiveDateTime {
    Local::now().naive_local()
}

/// 适配器取数上下文（依赖隔离：适配器只允许从这里拿数据）。
pub struct QuotaContext {
    /// JSONL 统计目录（local 系适配器聚合用）
    pub stats_dir: String,
    /// o2a-config::Account（只读使用）
    pub account: Account,
    /// 共享 reqwest Client（upstream 适配器发请求用；None 时 HTTP 适配器返回 None）
    pub session: Option<reqwest::Client>,
    /// 可注入时钟（单测用）
    pub now_fn: NowFn,
}

impl QuotaContext {
    pub fn new(stats_dir: impl Into<String>, account: Account) -> Self {
        QuotaContext {
            stats_dir: stats_dir.into(),
            account,
            session: None,
            now_fn: Arc::new(default_now),
        }
    }

    pub fn with_session(mut self, session: reqwest::Client) -> Self {
        self.session = Some(session);
        self
    }

    pub fn with_now_fn(mut self, now_fn: NowFn) -> Self {
        self.now_fn = now_fn;
        self
    }

    pub fn now(&self) -> NaiveDateTime {
        (self.now_fn)()
    }

    /// naive 本地时间 → "%Y-%m-%dT%H:%M:%S"（对齐 `ctx.iso`）。
    pub fn iso(&self, dt: &NaiveDateTime) -> String {
        dt.format("%Y-%m-%dT%H:%M:%S").to_string()
    }
}

/// Python truthiness（o2a-config 的 `truthy` 未导出，此处独立实现）。
pub(crate) fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(false),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// Python `round(x, 1)` 的 half-even 语义（与 o2a-stats::pyround 同款思路；
/// Rust `{:.1}` 格式化同为 round-half-to-even）。
pub fn py_round1(x: f64) -> f64 {
    format!("{:.1}", x).parse::<f64>().unwrap_or(x)
}

/// 构造一个窗口条目（对齐 `empty_window`）。
pub fn empty_window(
    kind: &str,
    unit: &str,
    used: f64,
    limit: Option<f64>,
    reset_at: Option<String>,
) -> Value {
    let pct = limit.map(|l| py_round1(used / l * 100.0));
    json!({
        "kind": kind,
        "unit": unit,
        "used": used,
        "limit": limit,
        "pct": pct,
        "reset_at": reset_at,
    })
}

/// 构造 QuotaSnapshot（归一化结构，前端 QuotaCard 只认这个形状）。
pub fn make_snapshot(
    adapter_id: &str,
    windows: Vec<Value>,
    source: &str,
    plan: Option<Value>,
    stale: bool,
    now: &NaiveDateTime,
) -> Value {
    json!({
        "adapterId": adapter_id,
        "scope": "account",
        "source": source,
        "fetched_at": now.format("%Y-%m-%dT%H:%M:%S").to_string(),
        "stale": stale,
        "windows": windows,
        "plan": plan,
    })
}

/// 供 local 系适配器共用的窗口起点（对齐 `window_start`）。
///
/// day → 当日零点；week → 本周一零点；month → 本月 1 日零点。
/// 未知 kind：Python 抛 ValueError，此处 panic（调用方均已校验）。
pub fn window_start(now: &NaiveDateTime, kind: &str) -> NaiveDateTime {
    let date = now.date();
    let target = match kind {
        "day" => date,
        "week" => {
            let monday_offset = date.weekday().num_days_from_monday() as i64;
            date - chrono::Duration::days(monday_offset)
        }
        "month" => NaiveDate::from_ymd_opt(date.year(), date.month(), 1)
            .expect("valid month start"),
        _ => panic!("unknown window kind: {}", kind),
    };
    target.and_time(NaiveTime::MIN)
}

/// 额度查询 TTL 缓存（60s；面板隐藏时不刷新由调用方保证）。
pub struct TTLCache {
    ttl: Duration,
    data: Mutex<HashMap<String, (Instant, Value)>>,
}

impl Default for TTLCache {
    fn default() -> Self {
        TTLCache::new(60)
    }
}

impl TTLCache {
    pub fn new(ttl_s: u64) -> Self {
        TTLCache {
            ttl: Duration::from_secs(ttl_s),
            data: Mutex::new(HashMap::new()),
        }
    }

    pub fn get(&self, key: &str) -> Option<Value> {
        let data = self.data.lock().unwrap();
        let (ts, value) = data.get(key)?;
        if ts.elapsed() < self.ttl {
            Some(value.clone())
        } else {
            None
        }
    }

    pub fn set(&self, key: &str, value: Value) {
        self.data
            .lock()
            .unwrap()
            .insert(key.to_string(), (Instant::now(), value));
    }

    /// 过期但尚存的旧值（上游失败时降级展示用）：只要存过就返回并标 stale。
    pub fn stale(&self, key: &str) -> Option<Value> {
        let data = self.data.lock().unwrap();
        let (_, value) = data.get(key)?;
        let mut v = value.clone();
        if let Value::Object(o) = &mut v {
            o.insert("stale".into(), Value::Bool(true));
        }
        Some(v)
    }
}

/// Unix 秒时间戳 → 本地 naive（对齐 Python `datetime.fromtimestamp(int(ts))`，失败返回 None）。
pub(crate) fn ts_to_local_naive(reset_ts: &Value) -> Option<NaiveDateTime> {
    let secs = reset_ts.as_i64()?;
    DateTime::<Utc>::from_timestamp(secs, 0)
        .map(|dt| dt.with_timezone(&Local).naive_local())
}

/// aware 时间字符串（RFC3339 / Z 后缀）→ 本地 naive；naive 字符串按本地时间理解。
/// 对齐 Python `datetime.fromisoformat(value.replace("Z", "+00:00")).astimezone()`。
pub(crate) fn parse_resets_at(value: &Value) -> Option<NaiveDateTime> {
    let s = value.as_str()?.trim();
    if s.is_empty() {
        return None;
    }
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Some(dt.with_timezone(&Local).naive_local());
    }
    // Z 后缀 + 毫秒已被 rfc3339 覆盖；fromisoformat 兼容 "+00:00" 之外的形态再试 naive
    if let Ok(dt) = NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.f") {
        return Some(dt);
    }
    if let Ok(dt) = NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S") {
        return Some(dt);
    }
    None
}

/// `Local::now()` 的 naive 形式（iter_records 文件范围用真实时钟，与 Python 一致）。
pub(crate) fn real_now() -> NaiveDateTime {
    Local::now().naive_local()
}
