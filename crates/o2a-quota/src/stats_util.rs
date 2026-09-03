//! local 系适配器共用：从 JSONL 记录聚合窗口用量（对齐 Python `adapters/_stats_util.py`）。
//!
//! 与 Python 的差异：文件扫描范围为 since 日期..=今天（真实时钟）全覆盖，
//! 修复 Python `(now - since).days + 1` 推算在跨日滚动窗边缘漏扫昨日文件的问题。

use std::fs;
use std::io::BufRead;

use chrono::{NaiveDate, NaiveDateTime};
use serde_json::Value;

use crate::base::real_now;

/// 遍历统计目录中某账号自 since 起的记录（timestamp 升序无保证）。
pub fn iter_records(
    stats_dir: &str,
    account_id: &str,
    since: &NaiveDateTime,
) -> Vec<(NaiveDateTime, Value)> {
    if stats_dir.is_empty() || account_id.is_empty() {
        return Vec::new();
    }
    let today: NaiveDate = real_now().date();
    let first = since.date();
    let mut out = Vec::new();
    let mut day = first;
    // 全覆盖扫描到今天（含）
    while day <= today {
        let path = format!("{}/{}.jsonl", stats_dir.trim_end_matches('/'), day);
        if let Ok(f) = fs::File::open(&path) {
            let reader = std::io::BufReader::new(f);
            for line in reader.lines().map_while(Result::ok) {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let Ok(rec) = serde_json::from_str::<Value>(line) else {
                    continue;
                };
                if rec.get("account").and_then(Value::as_str) != Some(account_id) {
                    continue;
                }
                let Ok(ts) = NaiveDateTime::parse_from_str(
                    rec.get("timestamp")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .get(..19)
                        .unwrap_or(""),
                    "%Y-%m-%dT%H:%M:%S",
                ) else {
                    continue;
                };
                if ts >= *since {
                    out.push((ts, rec));
                }
            }
        }
        day += chrono::Duration::days(1);
    }
    out
}

/// 窗口内请求数（成功 + 失败）。
pub fn count_requests(stats_dir: &str, account_id: &str, since: &NaiveDateTime) -> i64 {
    iter_records(stats_dir, account_id, since).len() as i64
}

/// 窗口内 token 总量（输入侧 + 输出）。
pub fn count_tokens(stats_dir: &str, account_id: &str, since: &NaiveDateTime) -> i64 {
    let or0 = |v: &Value| v.as_i64().unwrap_or(0);
    iter_records(stats_dir, account_id, since)
        .iter()
        .map(|(_, rec)| {
            or0(&rec["input_tokens"])
                + or0(&rec["cache_read_tokens"])
                + or0(&rec["cache_write_tokens"])
                + or0(&rec["output_tokens"])
        })
        .sum()
}
