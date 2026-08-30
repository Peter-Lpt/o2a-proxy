//! o2a-pricing：定价解析与求值的 Rust 镜像。
//!
//! 与 Python `o2a/pricing/` 同构：v1 pricing.json 在 loader 中归一化为
//! v2 components（tiers[0] + 缓存回退比例 0.2/1.0 烘焙），覆盖链
//! 服务级 > 账号级（键序 id→name）> 模型级，modifier 管道按数组顺序执行。
//!
//! 双端一致性由共享 golden fixtures 保证：`pricing/golden/cases.json`
//! 与 pytest 跑同一份文件（见下方 `golden_fixtures_parity` 测试）。

use chrono::{DateTime, Datelike, NaiveDateTime};
use serde_json::Value;

/// v1 缺省回退比例（与 Python schema.py 一致）
const CACHE_READ_RATIO: f64 = 0.2;
const CACHE_WRITE_RATIO: f64 = 1.0;

/// 单模型条目归一化：v1（tiers）与 v2（components/modifiers）均接受。
/// 返回 components 五项（input/output/cache_read/cache_write/request）与 modifiers。
pub fn entry_to_v2(entry: &Value) -> Option<(Value, Vec<Value>)> {
    let obj = entry.as_object()?;
    if obj.contains_key("components") || obj.contains_key("modifiers") {
        if let Some(c) = obj.get("components") {
            return Some((c.clone(), modifiers_of(entry)));
        }
        // 混合形态（tiers + modifiers，discount/schedule 场景）：从 tiers[0] 派生
        if let Some(tier) = obj
            .get("tiers")
            .and_then(|t| t.as_array())
            .and_then(|a| a.first())
        {
            return Some((tier_to_comps(tier), modifiers_of(entry)));
        }
        return Some((Value::Null, modifiers_of(entry)));
    }
    let tiers = entry.get("tiers").and_then(|t| t.as_array())?;
    let tier = match tiers.first() {
        Some(t) => t,
        None => return Some((Value::Null, Vec::new())), // 旧行为：无 tier → cost 0
    };
    Some((tier_to_comps(tier), v1_modifiers(entry)))
}

/// v1 tier → components（烘焙缓存回退比例 + output_thinking）。
fn tier_to_comps(tier: &Value) -> Value {
    let input = tier.get("input").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let output = tier.get("output").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let cache_read = match tier.get("cache_hit").and_then(|v| v.as_f64()) {
        Some(c) => c,
        None => input * CACHE_READ_RATIO,
    };
    let cache_write = match tier.get("cache_miss").and_then(|v| v.as_f64()) {
        Some(c) => c,
        None => input * CACHE_WRITE_RATIO,
    };
    let mut comps = serde_json::json!({
        "input": input,
        "output": output,
        "cache_read": cache_read,
        "cache_write": cache_write,
        "request": tier.get("request").and_then(|v| v.as_f64()).unwrap_or(0.0),
    });
    if let Some(ot) = tier.get("output_thinking").and_then(|v| v.as_f64()) {
        comps["output_thinking"] = serde_json::json!(ot);
    }
    comps
}

/// v1 模型级字段 → modifiers（：discount；：多档 range → context_tier）。
fn v1_modifiers(entry: &Value) -> Vec<Value> {
    let mut modifiers = Vec::new();
    if let Some(tiers) = entry.get("tiers").and_then(|t| t.as_array()) {
        let mut specs: Vec<Value> = Vec::new();
        for t in tiers {
            let Some(range) = t.get("range").and_then(|v| v.as_str()) else {
                continue;
            };
            let Some(parsed) = parse_range(range) else { continue };
            let mut spec = serde_json::json!({"upto": parsed.1});
            let mut ov = serde_json::Map::new();
            for (src, dst) in [
                ("input", "input"),
                ("output", "output"),
                ("cache_hit", "cache_read"),
                ("cache_miss", "cache_write"),
                ("output_thinking", "output_thinking"),
            ] {
                if let Some(v) = t.get(src).and_then(|v| v.as_f64()) {
                    ov.insert(dst.to_string(), serde_json::json!(v));
                }
            }
            if !ov.is_empty() {
                spec["override"] = serde_json::json!(ov);
            }
            specs.push(spec);
        }
        if !specs.is_empty() {
            modifiers.push(serde_json::json!(
                {"type": "context_tier", "by": "context_tokens", "tiers": specs}
            ));
        }
    }
    if let Some(d) = entry.get("discount").and_then(|v| v.as_f64()) {
        if d != 1.0 {
            let note = entry
                .get("discount_note")
                .and_then(|v| v.as_str())
                .unwrap_or("discount");
            modifiers.push(serde_json::json!({"type": "discount", "factor": d, "note": note}));
        }
    }
    // ：v1 free_quota（模型级数字，按月 tokens 额度）接入，冲抵最后一步
    if let Some(fq) = entry.get("free_quota").and_then(|v| v.as_f64()) {
        if fq > 0.0 {
            modifiers.push(serde_json::json!(
                {"type": "free_quota", "period": "month", "unit": "tokens", "amount": fq}
            ));
        }
    }
    modifiers
}

/// 解析 v1 range："0-256K" / "256K-1M" / "unlimited" → (low, upto=None 表示无上限)。
fn parse_range(text: &str) -> Option<(i64, Option<i64>)> {
    let t = text.trim().to_lowercase();
    if t == "unlimited" || t == "∞" || t == "*" {
        return Some((0, None));
    }
    let (low_s, high_s) = t.split_once('-')?;
    let num = |part: &str| -> Option<i64> {
        let p = part.trim();
        if p.is_empty() {
            return None;
        }
        let (head, mult) = match p.chars().last()? {
            'k' | 'K' => (&p[..p.len() - 1], 1024i64),
            'm' | 'M' => (&p[..p.len() - 1], 1024 * 1024),
            'g' | 'G' => (&p[..p.len() - 1], 1024 * 1024 * 1024),
            _ => (p, 1),
        };
        head.trim().parse::<f64>().ok().map(|v| (v * mult as f64) as i64)
    };
    let low = num(low_s).unwrap_or(0);
    let high = num(high_s)?;
    Some((low, Some(high)))
}

fn modifiers_of(entry: &Value) -> Vec<Value> {
    entry
        .get("modifiers")
        .and_then(|m| m.as_array())
        .cloned()
        .unwrap_or_default()
}

/// 覆盖链解析：服务级 > 账号级（键序）> 模型级。未命中返回 None（cost 0）。
#[allow(dead_code)]
pub fn resolve_entry<'a>(pricing: &'a Value, model: &str, account_keys: &[String], service_id: &str) -> Option<&'a Value> {
    resolve_entry_at(pricing, model, account_keys, service_id, None)
}

/// 带事件时间的覆盖链解析：优先 v3 rules，未配置/未命中回退 v1/v2 覆盖链。
/// timestamp 为 Option<&str>（None 在 v3 规则下不命中任何历史区间）。
pub fn resolve_entry_at<'a>(
    pricing: &'a Value,
    model: &str,
    account_keys: &[String],
    service_id: &str,
    timestamp: Option<&str>,
) -> Option<&'a Value> {
    let obj = pricing.as_object()?;
    // 1) v3 规则：按事件时间 + 最具体 scope 选择
    if let Some(rules) = obj.get("rules").and_then(|r| r.as_array()) {
        if !rules.is_empty() {
            if let Some(rule) = select_rule(rules, model, account_keys, service_id, timestamp) {
                return Some(rule);
            }
            // 有 rules 但未命中：不允许用 v1/v2 冒充历史
            return None;
        }
    }
    resolve_v1_v2(obj, model, account_keys, service_id)
}

fn resolve_v1_v2<'a>(
    obj: &'a serde_json::Map<String, Value>,
    model: &str,
    account_keys: &[String],
    service_id: &str,
) -> Option<&'a Value> {
    // 1) 服务级（v2 services 段）
    if !service_id.is_empty() {
        if let Some(entry) = obj
            .get("services")
            .and_then(|s| s.get(service_id))
            .and_then(|s| s.get("models"))
            .and_then(|m| m.get(model).or_else(|| m.get("*")))
        {
            if entry.is_object() {
                return Some(entry);
            }
        }
    }
    // 2) 账号级（键序：id 优先，name 兜底）
    for key in account_keys {
        if let Some(entry) = obj
            .get("accounts")
            .and_then(|a| a.get(key))
            .and_then(|a| a.get("models"))
            .and_then(|m| m.get(model))
        {
            if entry.is_object() {
                return Some(entry);
            }
        }
    }
    // 3) 全局模型级（跳过 _* 与 accounts —— v1 provider 结构）
    for (pname, pdata) in obj {
        if pname.starts_with('_') || pname == "accounts" || pname == "services" || pname == "rules" {
            continue;
        }
        if let Some(entry) = pdata.get("models").and_then(|m| m.get(model)) {
            if entry.is_object() {
                return Some(entry);
            }
        }
    }
    None
}

fn parse_ts(s: &str) -> Option<NaiveDateTime> {
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Some(dt.naive_utc());
    }
    if s.len() >= 19 {
        if let Ok(dt) = NaiveDateTime::parse_from_str(&s[..19], "%Y-%m-%dT%H:%M:%S") {
            return Some(dt);
        }
    }
    None
}

fn in_interval(ts: &str, eff_from: Option<&str>, eff_to: Option<&str>) -> bool {
    if ts.len() < 19 {
        return false;
    }
    let Ok(dt) = chrono::NaiveDateTime::parse_from_str(&ts[..19], "%Y-%m-%dT%H:%M:%S") else {
        return false;
    };
    if let Some(f) = eff_from {
        let Some(fd) = parse_ts(f) else { return false };
        if dt < fd {
            return false;
        }
    }
    if let Some(t) = eff_to {
        let Some(td) = parse_ts(t) else { return false };
        if dt >= td {
            return false;
        }
    }
    true
}

fn rule_matches(
    rule: &Value,
    model: &str,
    account_keys: &[String],
    service_id: &str,
    timestamp: Option<&str>,
) -> Option<(bool, bool, bool)> {
    let scope = rule.get("scope").and_then(|s| s.as_object());
    let r_model = rule.get("model").and_then(|v| v.as_str()).unwrap_or("*");
    if r_model != "*" && r_model != model {
        return None;
    }
    let svc = scope.and_then(|s| s.get("service")).and_then(|v| v.as_str()).unwrap_or("*");
    let acc = scope.and_then(|s| s.get("account")).and_then(|v| v.as_str()).unwrap_or("*");
    if svc != "*" && svc != service_id {
        return None;
    }
    if acc != "*" && !account_keys.iter().any(|k| k == acc) {
        return None;
    }
    let from = rule.get("effective_from").and_then(|v| v.as_str());
    let to = rule.get("effective_to").and_then(|v| v.as_str());
    let ts = timestamp.unwrap_or("");
    if ts.is_empty() || !in_interval(ts, from, to) {
        return None;
    }
    Some((svc != "*", acc != "*", r_model != "*"))
}

fn select_rule<'a>(
    rules: &'a [Value],
    model: &str,
    account_keys: &[String],
    service_id: &str,
    timestamp: Option<&str>,
) -> Option<&'a Value> {
    let mut best = None;
    let mut best_score = (false, false, false);
    for rule in rules {
        if let Some(score) = rule_matches(rule, model, account_keys, service_id, timestamp) {
            if best.is_none() || score > best_score {
                best = Some(rule);
                best_score = score;
            }
        }
    }
    best
}

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
        comps_raw
            .get(k)
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0)
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
                    if let Some(meta) = ctx.and_then(|c| c.get("meta")).and_then(|v| v.as_object()) {
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
                                } else if let Some(f) = w.get("factor").and_then(|v| v.as_f64()) {
                                    input_p *= f;
                                    output_p *= f;
                                    cache_read_p *= f;
                                    cache_write_p *= f;
                                    request_p *= f;
                                }
                            } else if let Some(fb) = m.get("fallback").and_then(|v| v.as_object()) {
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
                // ：ctx.meta.context_tokens 命中第一档 value <= upto（upto=null 无上限）
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
                // ：剩余额度冲抵 —— ratio = min(1, max(0, amount-cum)/req_tokens)
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
                //  阶梯之二：按周期累计用量分档（ctx.cumulative.tokens）
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
    let ts = ctx
        .and_then(|c| c.get("timestamp"))
        .and_then(|v| v.as_str());
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 与 pytest 跑同一份共享 golden fixtures（pricing/golden/cases.json），
    /// 固化 Python / Rust 双实现零漂移。
    #[test]
    fn golden_fixtures_parity() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("pricing")
            .join("golden")
            .join("cases.json");
        let raw = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("golden fixtures 不存在（{}）: {e}", path.display()));
        let data: Value = serde_json::from_str(&raw).unwrap();
        let cases = data.get("cases").and_then(|c| c.as_array()).unwrap();
        assert!(!cases.is_empty(), "golden 用例为空");
        for case in cases {
            let name = case.get("name").and_then(|v| v.as_str()).unwrap_or("?");
            let pricing = case.get("pricing").cloned().unwrap_or(Value::Null);
            let model = case.get("model").and_then(|v| v.as_str()).unwrap_or("");
            let usage = case.get("usage").cloned().unwrap_or(Value::Null);
            let keys: Vec<String> = case
                .get("account_keys")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            let ts = case.get("timestamp").and_then(|v| v.as_str());
            let ct = case.get("context_tokens").and_then(|v| v.as_i64());
            let cum = case.get("cumulative").and_then(|v| v.as_f64());
            let meta = case.get("meta").cloned().unwrap_or(Value::Null);
            let ctx = match (ts, ct, cum, meta) {
                (ts_opt, ct_opt, Some(c), meta_obj) => {
                    let mut ctx_obj = serde_json::json!({"cumulative": {"tokens": c}});
                    if let Some(t) = ts_opt {
                        ctx_obj["timestamp"] = serde_json::json!(t);
                    }
                    let mut m = serde_json::Map::new();
                    if let Some(n) = ct_opt {
                        m.insert("context_tokens".to_string(), serde_json::json!(n));
                    }
                    if let Some(mo) = meta_obj.as_object() {
                        for (k, v) in mo {
                            m.insert(k.clone(), v.clone());
                        }
                    }
                    if !m.is_empty() {
                        ctx_obj["meta"] = serde_json::json!(m);
                    }
                    Some(ctx_obj)
                }
                (Some(ts), Some(ct), None, meta_obj) => {
                    let mut m = serde_json::Map::new();
                    m.insert("context_tokens".to_string(), serde_json::json!(ct));
                    if let Some(mo) = meta_obj.as_object() {
                        for (k, v) in mo {
                            m.insert(k.clone(), v.clone());
                        }
                    }
                    Some(serde_json::json!({"timestamp": ts, "meta": m}))
                }
                (Some(ts), None, None, meta_obj) => {
                    if let Some(mo) = meta_obj.as_object() {
                        Some(serde_json::json!({"timestamp": ts, "meta": mo}))
                    } else {
                        Some(serde_json::json!({"timestamp": ts}))
                    }
                }
                (None, Some(ct), None, meta_obj) => {
                    let mut m = serde_json::Map::new();
                    m.insert("context_tokens".to_string(), serde_json::json!(ct));
                    if let Some(mo) = meta_obj.as_object() {
                        for (k, v) in mo {
                            m.insert(k.clone(), v.clone());
                        }
                    }
                    Some(serde_json::json!({"meta": m}))
                }
                (None, None, None, meta_obj) => {
                    if meta_obj.is_object() {
                        Some(serde_json::json!({"meta": meta_obj}))
                    } else {
                        None
                    }
                }
            };
            let sid = case.get("service_id").and_then(|v| v.as_str()).unwrap_or("");
            let total = resolve_cost(
                &pricing,
                model,
                usage.get("input").and_then(|v| v.as_i64()).unwrap_or(0),
                usage.get("cache_read").and_then(|v| v.as_i64()).unwrap_or(0),
                usage.get("cache_write").and_then(|v| v.as_i64()).unwrap_or(0),
                usage.get("output").and_then(|v| v.as_i64()).unwrap_or(0),
                &keys,
                sid,
                ctx.as_ref(),
            );
            let expected = case
                .get("expected_total")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            assert!(
                (total - expected).abs() < 1e-12,
                "case `{name}`: rust={total} expected={expected}"
            );
        }
    }

    #[test]
    fn v2_components_and_discount() {
        let pricing = serde_json::json!({"p": {"models": {"m": {
            "components": {"input": 2.0, "output": 8.0, "cache_read": 0.4, "cache_write": 2.0},
            "modifiers": [{"type": "discount", "factor": 0.5}]
        }}}});
        let entry = resolve_entry(&pricing, "m", &[], "").unwrap();
        let total = evaluate(entry, 1_000_000, 0, 0, 1_000_000, 0, None);
        assert!((total - 5.0).abs() < 1e-12);
    }
}
