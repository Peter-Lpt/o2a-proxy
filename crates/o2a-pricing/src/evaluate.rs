//! 求值：从 desktop/src-tauri/src/pricing.rs 逐字提取。
//!
//! modifier 管道（discount / batch / schedule / context_tier / free_quota /
//! cumulative_tier）+ components × 用量 → total。

use serde_json::Value;

use chrono::Datelike;

use crate::entry::entry_to_v2;
use crate::resolve::{resolve_entry_at, resolve_v1_v2};

/// modifier 管道 + 求值。返回 total（breakdown 供后续面板展开用）。
/// ctx 携带 timestamp（记录本地时间）与 meta（batch 等标记），schedule 判定用；
/// timestamp 缺失时 schedule 不生效（与 Python 一致）。
pub fn evaluate(
    entry: &Value,
    input: i64,
    read: i64,
    write: i64,
    output: i64,
    requests: i64,
    ctx: Option<&Value>,
) -> f64 {
    let Some((comps_raw, modifiers)) = entry_to_v2(entry) else {
        return 0.0;
    };
    let get = |k: &str| -> f64 {
        comps_raw.get(k).and_then(|v| v.as_f64()).unwrap_or(0.0)
    };
    let mut input_p = get("input");
    let mut output_p = get("output");
    let mut cache_read_p = get("cache_read");
    let mut cache_write_p = get("cache_write");
    let mut request_p = get("request");
    // modifier 管道（当前注册：discount / batch / schedule；与 Python modifiers/ 注册表对应）
    for m in &modifiers {
        let ty = m.get("type").and_then(|v| v.as_str()).unwrap_or("");
        match ty {
            "discount" => {
                let f = m.get("factor").and_then(|v| v.as_f64()).unwrap_or(1.0);
                input_p *= f;
                output_p *= f;
                cache_read_p *= f;
                cache_write_p *= f;
                request_p *= f;
            }
            "batch" => {
                let mut hit = true;
                if let Some(when) = m.get("when").and_then(|w| w.as_object()) {
                    if let Some(meta) = ctx.and_then(|c| c.get("meta")).and_then(|v| v.as_object())
                    {
                        for (k, v) in when {
                            if meta.get(k) != Some(v) {
                                hit = false;
                                break;
                            }
                        }
                    } else if !when.is_empty() {
                        hit = false;
                    }
                }
                if hit {
                    let f = m.get("factor").and_then(|v| v.as_f64()).unwrap_or(1.0);
                    input_p *= f;
                    output_p *= f;
                    cache_read_p *= f;
                    cache_write_p *= f;
                    request_p *= f;
                }
            }
            "schedule" => {
                let ts = ctx
                    .and_then(|c| c.get("timestamp"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if ts.len() >= 16 {
                    if let Some((wd, hhmm)) = parse_local(ts) {
                        if let Some(windows) = m.get("windows").and_then(|w| w.as_array()) {
                            let mut matched: Option<&Value> = None;
                            for w in windows {
                                if window_matches(w, wd, hhmm) {
                                    matched = Some(w);
                                    break;
                                }
                            }
                            if let Some(w) = matched {
                                if let Some(ov) = w.get("override").and_then(|v| v.as_object()) {
                                    for (k, v) in ov {
                                        if let Some(f) = v.as_f64() {
                                            match k.as_str() {
                                                "input" => input_p = f,
                                                "output" => output_p = f,
                                                "cache_read" => cache_read_p = f,
                                                "cache_write" => cache_write_p = f,
                                                "request" => request_p = f,
                                                "output_thinking" => {}
                                                _ => {}
                                            }
                                        }
                                    }
                                } else if let Some(f) =
                                    w.get("factor").and_then(|v| v.as_f64())
                                {
                                    input_p *= f;
                                    output_p *= f;
                                    cache_read_p *= f;
                                    cache_write_p *= f;
                                    request_p *= f;
                                }
                            } else if let Some(fb) =
                                m.get("fallback").and_then(|v| v.as_object())
                            {
                                for (k, v) in fb {
                                    if let Some(f) = v.as_f64() {
                                        match k.as_str() {
                                            "input" => input_p = f,
                                            "output" => output_p = f,
                                            "cache_read" => cache_read_p = f,
                                            "cache_write" => cache_write_p = f,
                                            "request" => request_p = f,
                                            _ => {}
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            "context_tier" => {
                // ctx.meta.context_tokens 命中第一档 value <= upto（upto=null 无上限）
                let value = ctx
                    .and_then(|c| c.get("meta"))
                    .and_then(|m| m.get("context_tokens"))
                    .and_then(|v| v.as_f64());
                if let Some(value) = value {
                    if let Some(tiers) = m.get("tiers").and_then(|w| w.as_array()) {
                        for t in tiers {
                            let upto = t.get("upto").and_then(|v| v.as_f64());
                            if let Some(u) = upto {
                                if value > u {
                                    continue;
                                }
                            }
                            if let Some(ov) = t.get("override").and_then(|v| v.as_object()) {
                                for (k, v) in ov {
                                    if let Some(f) = v.as_f64() {
                                        match k.as_str() {
                                            "input" => input_p = f,
                                            "output" => output_p = f,
                                            "cache_read" => cache_read_p = f,
                                            "cache_write" => cache_write_p = f,
                                            "request" => request_p = f,
                                            _ => {}
                                        }
                                    }
                                }
                            } else if let Some(f) = t.get("factor").and_then(|v| v.as_f64()) {
                                input_p *= f;
                                output_p *= f;
                                cache_read_p *= f;
                                cache_write_p *= f;
                                request_p *= f;
                            }
                            break;
                        }
                    }
                }
            }
            "free_quota" => {
                // 剩余额度冲抵 —— ratio = min(1, max(0, amount-cum)/req_tokens)
                // applied to 全部分量；cumulative 未知时不生效（与 Python 一致）
                let used = ctx
                    .and_then(|c| c.get("cumulative"))
                    .and_then(|c| c.get("tokens"))
                    .and_then(|v| v.as_f64());
                if let Some(used) = used {
                    let amount = m.get("amount").and_then(|v| v.as_f64()).unwrap_or(0.0);
                    let req = (input + read + write + output) as f64;
                    if amount > 0.0 && req > 0.0 {
                        let remaining = (amount - used).max(0.0);
                        let ratio = (remaining / req).min(1.0);
                        input_p *= ratio;
                        output_p *= ratio;
                        cache_read_p *= ratio;
                        cache_write_p *= ratio;
                        request_p *= ratio;
                    }
                }
            }
            "cumulative_tier" => {
                // 阶梯之二：按周期累计用量分档（ctx.cumulative.tokens）
                let value = ctx
                    .and_then(|c| c.get("cumulative"))
                    .and_then(|c| c.get("tokens"))
                    .and_then(|v| v.as_f64());
                if let Some(value) = value {
                    if let Some(tiers) = m.get("tiers").and_then(|w| w.as_array()) {
                        for t in tiers {
                            let upto = t.get("upto").and_then(|v| v.as_f64());
                            if let Some(u) = upto {
                                if value > u {
                                    continue;
                                }
                            }
                            if let Some(ov) = t.get("override").and_then(|v| v.as_object()) {
                                for (k, v) in ov {
                                    if let Some(f) = v.as_f64() {
                                        match k.as_str() {
                                            "input" => input_p = f,
                                            "output" => output_p = f,
                                            "cache_read" => cache_read_p = f,
                                            "cache_write" => cache_write_p = f,
                                            "request" => request_p = f,
                                            _ => {}
                                        }
                                    }
                                }
                            } else if let Some(f) = t.get("factor").and_then(|v| v.as_f64()) {
                                input_p *= f;
                                output_p *= f;
                                cache_read_p *= f;
                                cache_write_p *= f;
                                request_p *= f;
                            }
                            break;
                        }
                    }
                }
            }
            _ => {}
        }
    }
    input as f64 * input_p / 1_000_000.0
        + output as f64 * output_p / 1_000_000.0
        + read as f64 * cache_read_p / 1_000_000.0
        + write as f64 * cache_write_p / 1_000_000.0
        + requests as f64 * request_p
}

/// 解析本地时间戳 "YYYY-MM-DDTHH:MM:SS" → (星期名, "HH:MM")，与 Python 一致。
fn parse_local(ts: &str) -> Option<(&'static str, &str)> {
    let date = chrono::NaiveDate::parse_from_str(&ts[..10], "%Y-%m-%d").ok()?;
    let wd = match date.weekday() {
        chrono::Weekday::Mon => "mon",
        chrono::Weekday::Tue => "tue",
        chrono::Weekday::Wed => "wed",
        chrono::Weekday::Thu => "thu",
        chrono::Weekday::Fri => "fri",
        chrono::Weekday::Sat => "sat",
        chrono::Weekday::Sun => "sun",
    };
    Some((wd, &ts[11..16]))
}

fn window_matches(w: &Value, wd: &str, hhmm: &str) -> bool {
    if let Some(days) = w.get("days").and_then(|d| d.as_array()) {
        if !days.iter().any(|d| d.as_str() == Some(wd)) {
            return false;
        }
    }
    let from = w.get("from").and_then(|v| v.as_str());
    let to = w.get("to").and_then(|v| v.as_str());
    match (from, to) {
        (None, _) | (_, None) => true, // 无时间区间 = 全天
        (Some(f), Some(t)) => {
            if f <= t {
                hhmm >= f && hhmm < t
            } else {
                // 跨天区间（22:00→08:00）
                hhmm >= f || hhmm < t
            }
        }
    }
}

/// 一步到位：resolve + evaluate。未命中返回 0.0。
/// ctx 携带 timestamp（记录本地时间）与 meta，供 schedule / batch 判定。
// 参数与 Python resolve_cost 一一对应，保持签名对齐不合并。
#[allow(clippy::too_many_arguments)]
pub fn resolve_cost(
    pricing: &Value,
    model: &str,
    input: i64,
    read: i64,
    write: i64,
    output: i64,
    account_keys: &[String],
    service_id: &str,
    ctx: Option<&Value>,
) -> f64 {
    let ts = ctx.and_then(|c| c.get("timestamp")).and_then(|v| v.as_str());
    if let Some(entry) = resolve_entry_at(pricing, model, account_keys, service_id, ts) {
        return evaluate(entry, input, read, write, output, 0, ctx);
    }
    // v3 有 rules 但未命中：与 Python 一致回退 v1/v2 当前价兜底
    if let Some(obj) = pricing.as_object() {
        if obj
            .get("rules")
            .and_then(|r| r.as_array())
            .map(|r| !r.is_empty())
            .unwrap_or(false)
        {
            if let Some(entry) = resolve_v1_v2(obj, model, account_keys, service_id) {
                return evaluate(entry, input, read, write, output, 0, ctx);
            }
        }
    }
    0.0
}
