//! v1/v2/v3 pricing 归一化：移植自 Python `o2a/pricing/schema.py`。
//!
//! 供 `fingerprint` 使用（canonical 化前的归一化层）。求值路径用的是
//! `entry` 模块的元组形态（desktop 版），两者对 v1 tier 的 components
//! 推导一致（tiers[0] + 缓存回退比例 0.2/1.0 烘焙）。

use serde_json::{json, Map, Value};

use crate::DEFAULT_CURRENCY;

const COMPONENT_KEYS: [&str; 5] = ["input", "output", "cache_read", "cache_write", "request"];

/// v1 多档 tiers（含 range）→ context_tier modifier。
/// 无 range 的档位跳过；cache_hit/miss/output_thinking 映射进 override。
/// 无可解析 range 时返回 []（维持旧单档行为）。
fn v1_context_tier(tiers: &[Value]) -> Vec<Value> {
    let mut specs: Vec<Value> = Vec::new();
    for t in tiers {
        let Some(range) = t.get("range").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(parsed) = parse_range_py(range) else { continue };
        let mut spec = Map::new();
        spec.insert("upto".into(), match parsed.1 {
            Some(u) => json!(u),
            None => Value::Null,
        });
        let mut ov = Map::new();
        for (src, dst) in [
            ("input", "input"),
            ("output", "output"),
            ("cache_hit", "cache_read"),
            ("cache_miss", "cache_write"),
            ("output_thinking", "output_thinking"),
        ] {
            if let Some(v) = t.get(src).and_then(|v| v.as_f64()) {
                ov.insert(dst.into(), json!(v));
            }
        }
        if !ov.is_empty() {
            spec.insert("override".into(), Value::Object(ov));
        }
        specs.push(Value::Object(spec));
    }
    if specs.is_empty() {
        return Vec::new();
    }
    vec![json!({"type": "context_tier", "by": "context_tokens", "tiers": specs})]
}

/// Python %g（6 位有效数字）格式的 Rust 等价，用于 discount note 文案。
/// 例：0.35 → "0.35"，2.0 → "2"，0.123456789 → "0.123457"，1234567 → "1.23457e+06"。
fn format_g(v: f64) -> String {
    if v == 0.0 {
        return if v.is_sign_negative() { "-0".into() } else { "0".into() };
    }
    let exp = v.abs().log10().floor() as i32;
    if (-4..6).contains(&exp) {
        let decimals = (5 - exp).max(0) as usize;
        let mut s = format!("{:.*}", decimals, v);
        if s.contains('.') {
            while s.ends_with('0') {
                s.pop();
            }
            if s.ends_with('.') {
                s.pop();
            }
        }
        s
    } else {
        // 科学计数法：5 位小数 + 去尾零，指数至少 2 位（Python 风格 e+06 / e-07）
        let mantissa_exp = format!("{:.*e}", 5, v);
        let (m, e) = mantissa_exp.split_once('e').unwrap();
        let mut m = m.to_string();
        if m.contains('.') {
            while m.ends_with('0') {
                m.pop();
            }
            if m.ends_with('.') {
                m.pop();
            }
        }
        let exp_val: i32 = e.parse().unwrap_or(0);
        if exp_val >= 0 {
            format!("{}e+{:02}", m, exp_val)
        } else {
            format!("{}e-{:02}", m, -exp_val)
        }
    }
}

/// 解析 v1 range（Python schema.parse_range 语义）："0-256K" / "unlimited"
/// → (low, upto)；upto=None 表示无上限；解析失败返回 None。
pub fn parse_range_py(text: &str) -> Option<(i64, Option<i64>)> {
    let t = text.trim().to_lowercase();
    if t == "unlimited" || t == "∞" || t == "*" {
        return Some((0, None));
    }
    if !t.contains('-') {
        return None;
    }
    let num = |part: &str| -> Option<i64> {
        let p = part.trim();
        if p.is_empty() {
            return None;
        }
        let last = p.chars().last()?;
        let mult: i64 = match last {
            'k' | 'K' => 1024,
            'm' | 'M' => 1024 * 1024,
            'g' | 'G' => 1024 * 1024 * 1024,
            _ => 1,
        };
        let head = if mult > 1 && p.len() > 1 { &p[..p.len() - 1] } else { p };
        head.trim().parse::<f64>().ok().map(|v| (v * mult as f64) as i64)
    };
    let (low_s, high_s) = t.split_once('-')?;
    let low = num(low_s).unwrap_or(0);
    let high = num(high_s)?;
    Some((low, Some(high)))
}

/// v1 tier → v2 components（烘焙缺省缓存比例，保持结果一致）。
fn v1_tier_to_components(tier: &Value) -> Map<String, Value> {
    let inp = tier.get("input").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let out = tier.get("output").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let has_hit = tier.get("cache_hit").map(|v| !v.is_null()).unwrap_or(false);
    let has_miss = tier.get("cache_miss").map(|v| !v.is_null()).unwrap_or(false);
    // Python: tier["cache_hit"] if "cache_hit" in tier else inp*ratio；随后 float(cache_read or 0)
    let cache_read = if has_hit {
        tier.get("cache_hit").and_then(|v| v.as_f64()).unwrap_or(0.0)
    } else {
        inp * crate::CACHE_READ_RATIO
    };
    let cache_write = if has_miss {
        tier.get("cache_miss").and_then(|v| v.as_f64()).unwrap_or(0.0)
    } else {
        inp * crate::CACHE_WRITE_RATIO
    };
    let request = tier.get("request").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let mut m = Map::new();
    m.insert("input".into(), json!(inp));
    m.insert("output".into(), json!(out));
    m.insert("cache_read".into(), json!(cache_read));
    m.insert("cache_write".into(), json!(cache_write));
    m.insert("request".into(), json!(request));
    m
}

fn default_components() -> Map<String, Value> {
    let mut m = Map::new();
    for k in COMPONENT_KEYS {
        m.insert(k.into(), json!(0));
    }
    m
}

/// 溯源字段（currency/source/updated_at/rule_id）：存在且非 null/空串才保留。
fn provenance(entry: &Value) -> Map<String, Value> {
    let mut out = Map::new();
    for key in ["currency", "source", "updated_at", "rule_id"] {
        if let Some(v) = entry.get(key) {
            let keep = match v {
                Value::Null => false,
                Value::String(s) => !s.is_empty(),
                _ => true,
            };
            if keep {
                out.insert(key.into(), v.clone());
            }
        }
    }
    out
}

fn sorted_strings(mut v: Vec<String>) -> Vec<Value> {
    v.sort();
    v.into_iter().map(Value::String).collect()
}

/// 单模型条目归一化（Python schema._entry_to_v2 语义）：v1（tiers）与
/// v2（components/modifiers）均接受；无有效 tier → components 全 0。
pub fn entry_to_v2_entry(entry: &Value) -> Value {
    let mut out = Map::new();
    let explicit: Vec<String>;
    let comps: Map<String, Value>;
    let mods: Vec<Value>;
    let billing: Value;

    if !entry.get("_explicit_components").is_none_or(|v| v.is_null()) {
        // 幂等：已归一化条目直接保留显式分量集合，避免默认值被误判为显式声明
        let ex: Vec<String> = entry["_explicit_components"]
            .as_array()
            .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
            .unwrap_or_default();
        let mut c = default_components();
        if let Some(src) = entry.get("components").and_then(|v| v.as_object()) {
            for (k, v) in src {
                c.insert(k.clone(), v.clone());
            }
        }
        mods = entry
            .get("modifiers")
            .and_then(|m| m.as_array())
            .cloned()
            .unwrap_or_default();
        explicit = ex;
        comps = c;
        billing = entry.get("billing").cloned().unwrap_or(json!("token"));
    } else if entry.get("components").is_some() || entry.get("modifiers").is_some() {
        // v2 形态（或 v1+modifiers 混合）：components 缺省补齐；
        // 无 components 但有 tiers 时从 tiers[0] 派生（混合形态，discount/schedule 场景）
        let mut c = default_components();
        let mut ex: Vec<String>;
        if let Some(src) = entry.get("components").and_then(|v| v.as_object()) {
            for (k, v) in src {
                c.insert(k.clone(), v.clone());
            }
            ex = src.keys().cloned().collect();
        } else if let Some(t0) = entry
            .get("tiers")
            .and_then(|t| t.as_array())
            .and_then(|a| a.first())
        {
            c = v1_tier_to_components(t0);
            ex = c.keys().cloned().collect();
            if let Some(ot) = t0.get("output_thinking").and_then(|v| v.as_f64()) {
                c.insert("output_thinking".into(), json!(ot));
                ex.push("output_thinking".into());
            }
        } else {
            ex = Vec::new();
        }
        let mut m = entry
            .get("modifiers")
            .and_then(|x| x.as_array())
            .cloned()
            .unwrap_or_default();
        // 混合形态下同样把多档 range 映射为 context_tier（置于声明 modifiers 之前）
        if let Some(tiers) = entry.get("tiers").and_then(|t| t.as_array()) {
            let mut all = v1_context_tier(tiers);
            all.extend(m);
            m = all;
        }
        explicit = ex;
        comps = c;
        mods = m;
        billing = entry.get("billing").cloned().unwrap_or(json!("token"));
    } else {
        let tiers_empty = entry
            .get("tiers")
            .and_then(|t| t.as_array())
            .map(|a| a.is_empty())
            .unwrap_or(true);
        if tiers_empty {
            out.insert("billing".into(), json!("token"));
            out.insert("components".into(), Value::Object(default_components()));
            out.insert("modifiers".into(), Value::Array(Vec::new()));
            out.insert("_explicit_components".into(), Value::Array(Vec::new()));
            for (k, v) in provenance(entry) {
                out.insert(k, v);
            }
            return Value::Object(out);
        }
        let tiers = entry["tiers"].as_array().unwrap();
        let t0 = &tiers[0];
        let mut c = v1_tier_to_components(t0);
        let mut ex: Vec<String> = c.keys().cloned().collect();
        if let Some(ot) = t0.get("output_thinking").and_then(|v| v.as_f64()) {
            c.insert("output_thinking".into(), json!(ot));
            ex.push("output_thinking".into());
        }
        let mut m = v1_context_tier(tiers);
        if let Some(d) = entry.get("discount").and_then(|v| v.as_f64()) {
            if d != 1.0 {
                let note = entry
                    .get("discount_note")
                    .and_then(|v| v.as_str())
                    .map(String::from)
                    .unwrap_or_else(|| format!("discount:{}", format_g(d)));
                m.push(json!({"type": "discount", "factor": d, "note": note}));
            }
        }
        if let Some(fq) = entry.get("free_quota").and_then(|v| v.as_f64()) {
            if fq > 0.0 {
                m.push(json!({"type": "free_quota", "period": "month", "unit": "tokens", "amount": fq}));
            }
        }
        explicit = ex;
        comps = c;
        mods = m;
        billing = json!("token");
    }

    out.insert("billing".into(), billing);
    out.insert("components".into(), Value::Object(comps));
    out.insert("modifiers".into(), Value::Array(mods));
    out.insert("_explicit_components".into(), Value::Array(sorted_strings(explicit)));
    for (k, v) in provenance(entry) {
        out.insert(k, v);
    }
    Value::Object(out)
}

fn normalize_scope(scope: Option<&Value>) -> Value {
    let Some(s) = scope.and_then(|v| v.as_object()) else {
        return json!({"service": "*", "account": "*"});
    };
    let svc = s
        .get("service")
        .and_then(|v| v.as_str())
        .unwrap_or("*");
    let acc = s
        .get("account")
        .and_then(|v| v.as_str())
        .unwrap_or("*");
    json!({"service": if svc.is_empty() { "*" } else { svc },
           "account": if acc.is_empty() { "*" } else { acc }})
}

/// v3 单条规则 → 内部 ResolvedEntry（含规则元数据）。
pub fn normalize_rule(rule: &Value) -> Value {
    let mut out = Map::new();
    let mut comps = default_components();
    let mut explicit: Vec<String> = Vec::new();
    if let Some(cs) = rule.get("components").and_then(|v| v.as_object()) {
        for (k, v) in cs {
            if COMPONENT_KEYS.contains(&k.as_str()) {
                comps.insert(k.clone(), v.clone());
            }
        }
        for k in cs.keys() {
            if COMPONENT_KEYS.contains(&k.as_str()) {
                explicit.push(k.clone());
            }
        }
        if let Some(ot) = cs.get("output_thinking").and_then(|v| v.as_f64()) {
            comps.insert("output_thinking".into(), json!(ot));
        }
        if cs.get("output_thinking").is_some_and(|v| !v.is_null()) {
            explicit.push("output_thinking".into());
        }
    }
    let mut mods: Vec<Value> = rule
        .get("modifiers")
        .and_then(|m| m.as_array())
        .cloned()
        .unwrap_or_default();
    if let Some(p) = rule.get("plan") {
        if !p.is_null() {
            mods.push(json!({"type": "plan", "plan": p}));
        }
    }
    out.insert(
        "billing".into(),
        rule.get("billing").cloned().unwrap_or(json!("token")),
    );
    out.insert("components".into(), Value::Object(comps));
    out.insert("modifiers".into(), Value::Array(mods));
    out.insert(
        "_explicit_components".into(),
        Value::Array(sorted_strings(explicit)),
    );
    out.insert("scope".into(), normalize_scope(rule.get("scope")));
    out.insert(
        "effective_from".into(),
        rule.get("effective_from").cloned().unwrap_or(Value::Null),
    );
    out.insert(
        "effective_to".into(),
        rule.get("effective_to").cloned().unwrap_or(Value::Null),
    );
    for key in ["currency", "source", "updated_at"] {
        if let Some(v) = rule.get(key) {
            if !v.is_null() {
                out.insert(key.into(), v.clone());
            }
        }
    }
    let rid = rule
        .get("id")
        .or_else(|| rule.get("rule_id"))
        .filter(|v| !v.is_null());
    if let Some(v) = rid {
        out.insert("rule_id".into(), v.clone());
    }
    Value::Object(out)
}

/// 两条规则生效区间是否重叠（from 含、to 不含；None 表示无界）。
fn intervals_overlap(a: &Value, b: &Value) -> bool {
    let a0 = a.get("effective_from").and_then(|v| v.as_str());
    let a1 = a.get("effective_to").and_then(|v| v.as_str());
    let b0 = b.get("effective_from").and_then(|v| v.as_str());
    let b1 = b.get("effective_to").and_then(|v| v.as_str());
    if let (Some(a0), Some(b1)) = (a0, b1) {
        if a0 >= b1 {
            return false;
        }
    }
    if let (Some(b0), Some(a1)) = (b0, a1) {
        if b0 >= a1 {
            return false;
        }
    }
    true
}

/// 校验 v3 rules：同 (service, account, model, currency) 生效区间不可重叠。
/// 有重叠返回 Err（启动/加载时给出明确错误，不静默 0）。
pub fn validate_pricing_rules(raw: &Value) -> Result<Vec<Value>, String> {
    let Some(rules) = raw.get("rules").and_then(|r| r.as_array()) else {
        return Ok(Vec::new());
    };
    if rules.is_empty() {
        return Ok(Vec::new());
    }
    let norm: Vec<Value> = rules
        .iter()
        .filter(|r| r.is_object())
        .map(normalize_rule)
        .collect();
    // 分桶：(service, account, model, currency)
    type BucketKey = (String, String, String, String);
    let mut buckets: Vec<(BucketKey, Vec<Value>)> = Vec::new();
    for r in &norm {
        let svc = r["scope"]["service"].as_str().unwrap_or("*").to_string();
        let acc = r["scope"]["account"].as_str().unwrap_or("*").to_string();
        let model = r.get("model").and_then(|v| v.as_str()).unwrap_or("*").to_string();
        let cur = r
            .get("currency")
            .and_then(|v| v.as_str())
            .map(String::from)
            .or_else(|| raw.get("currency").and_then(|v| v.as_str()).map(String::from))
            .unwrap_or_else(|| DEFAULT_CURRENCY.to_string());
        let key = (svc, acc, model, cur);
        if let Some(bucket) = buckets.iter_mut().find(|(k, _)| *k == key) {
            bucket.1.push(r.clone());
        } else {
            buckets.push((key, vec![r.clone()]));
        }
    }
    for (key, items) in &buckets {
        for i in 0..items.len() {
            for j in (i + 1)..items.len() {
                if intervals_overlap(&items[i], &items[j]) {
                    let a = &items[i];
                    let b = &items[j];
                    let ts_text = |v: &Value| -> String {
                        v.as_str().map(String::from).unwrap_or_else(|| "∞".into())
                    };
                    return Err(format!(
                        "pricing rules overlap: ({}, {}, {}, {}) rules {:?} and {:?} ({}~{} vs {}~{})",
                        key.0,
                        key.1,
                        key.2,
                        key.3,
                        a.get("rule_id").and_then(|v| v.as_str()).unwrap_or(""),
                        b.get("rule_id").and_then(|v| v.as_str()).unwrap_or(""),
                        ts_text(a.get("effective_from").unwrap_or(&Value::Null)),
                        ts_text(a.get("effective_to").unwrap_or(&Value::Null)),
                        ts_text(b.get("effective_from").unwrap_or(&Value::Null)),
                        ts_text(b.get("effective_to").unwrap_or(&Value::Null)),
                    ));
                }
            }
        }
    }
    Ok(norm)
}

/// 整份 pricing 归一化（幂等：v2 输入原样通过；v3 rules 保留为规则表）。
pub fn normalize_pricing(raw: &Value) -> Result<Value, String> {
    if !raw.is_object() {
        return Ok(json!({
            "_meta": {"schema": "o2a-pricing/v2", "currency": DEFAULT_CURRENCY},
            "models": {}, "accounts": {}, "services": {}, "rules": []
        }));
    }
    let obj = raw.as_object().unwrap();
    let rules = validate_pricing_rules(raw)?;

    let mut models = Map::new();
    for (provider, pv) in obj {
        if provider.starts_with('_') || provider == "accounts" || provider == "services" || provider == "rules" {
            continue;
        }
        let Some(pdict) = pv.as_object() else { continue };
        let provider_source = pdict.get("source").and_then(|v| v.as_str());
        let provider_updated = pdict
            .get("last_updated")
            .or_else(|| pdict.get("updated_at"))
            .and_then(|v| v.as_str());
        if let Some(pmodels) = pdict.get("models").and_then(|m| m.as_object()) {
            for (model, mv) in pmodels {
                if !mv.is_object() {
                    continue;
                }
                let mut entry = mv.clone();
                if let Some(src) = provider_source {
                    let has = entry
                        .get("source")
                        .map(|v| !v.is_null() && v.as_str() != Some(""))
                        .unwrap_or(false);
                    if !has {
                        entry["source"] = json!(src);
                    }
                }
                if let Some(upd) = provider_updated {
                    let has = entry
                        .get("updated_at")
                        .map(|v| !v.is_null() && v.as_str() != Some(""))
                        .unwrap_or(false);
                    if !has {
                        entry["updated_at"] = json!(upd);
                    }
                }
                models.insert(model.clone(), entry_to_v2_entry(&entry));
            }
        }
    }

    let mut accounts = Map::new();
    if let Some(acc_raw) = obj.get("accounts").and_then(|a| a.as_object()) {
        for (key, av) in acc_raw {
            if !av.is_object() {
                continue;
            }
            let mut m = Map::new();
            if let Some(am) = av.get("models").and_then(|x| x.as_object()) {
                for (name, mv) in am {
                    if mv.is_object() {
                        m.insert(name.clone(), entry_to_v2_entry(mv));
                    }
                }
            }
            accounts.insert(key.clone(), json!({"models": m}));
        }
    }

    let mut meta = Map::new();
    meta.insert("schema".into(), json!("o2a-pricing/v2"));
    meta.insert("currency".into(), json!(DEFAULT_CURRENCY));
    if let Some(raw_meta) = obj.get("_meta").and_then(|m| m.as_object()) {
        for (k, v) in raw_meta {
            meta.insert(k.clone(), v.clone());
        }
    }
    if let Some(c) = obj.get("currency") {
        if !c.is_null() {
            meta.insert("currency".into(), c.clone());
        }
    }
    if let Some(v) = obj.get("version") {
        if !v.is_null() {
            meta.insert("version".into(), v.clone());
        }
    }

    Ok(json!({
        "_meta": Value::Object(meta),
        "models": Value::Object(models),
        "accounts": Value::Object(accounts),
        "services": obj.get("services").cloned().unwrap_or(json!({})),
        "rules": rules,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v1_normalizes_with_baked_ratios() {
        let raw = json!({"dashscope": {"models": {"q": {"tiers": [{"input": 2.0, "output": 8.0}]}}}});
        let norm = normalize_pricing(&raw).unwrap();
        let e = &norm["models"]["q"];
        assert_eq!(e["components"]["input"], 2.0);
        assert_eq!(e["components"]["cache_read"], 0.4);
        assert_eq!(e["components"]["cache_write"], 2.0);
        // v1 烘焙 → explicit 五项齐全
        let ex = e["_explicit_components"].as_array().unwrap();
        assert_eq!(ex.len(), 5);
    }

    #[test]
    fn provenance_inherited_from_provider() {
        let raw = json!({"ds": {"source": "manual", "last_updated": "2025-01-01",
                                "models": {"m": {"tiers": [{"input": 1.0}]}}}});
        let norm = normalize_pricing(&raw).unwrap();
        assert_eq!(norm["models"]["m"]["source"], "manual");
        assert_eq!(norm["models"]["m"]["updated_at"], "2025-01-01");
    }

    #[test]
    fn discount_note_matches_python_g_format() {
        let e = entry_to_v2_entry(&json!({"tiers": [{"input": 1.0, "output": 4.0}], "discount": 0.35}));
        let m = &e["modifiers"][0];
        assert_eq!(m["type"], "discount");
        assert_eq!(m["note"], "discount:0.35");
        let e2 = entry_to_v2_entry(&json!({"tiers": [{"input": 1.0}], "discount": 2.0}));
        let note = e2["modifiers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["type"] == "discount")
            .unwrap()["note"]
            .as_str()
            .unwrap();
        assert_eq!(note, "discount:2");
    }

    #[test]
    fn format_g_matches_python() {
        assert_eq!(format_g(0.35), "0.35");
        assert_eq!(format_g(2.0), "2");
        assert_eq!(format_g(0.123456789), "0.123457");
        assert_eq!(format_g(1234567.0), "1.23457e+06");
        assert_eq!(format_g(0.0001), "0.0001");
        assert_eq!(format_g(0.00001), "1e-05");
    }

    #[test]
    fn v3_rule_normalization_and_overlap() {
        let raw = json!({"rules": [
            {"id": "a", "model": "m", "scope": {"service": "s1", "account": "acc"},
             "effective_from": "2025-01-01T00:00:00", "effective_to": "2025-06-01T00:00:00",
             "components": {"input": 1.0}},
            {"id": "b", "model": "m", "scope": {"service": "s1", "account": "acc"},
             "effective_from": "2025-05-01T00:00:00", "effective_to": null,
             "components": {"input": 2.0}}
        ]});
        assert!(normalize_pricing(&raw).is_err());
        // 相邻不重叠（from 含、to 不含）
        let ok = json!({"rules": [
            {"id": "a", "effective_from": "2025-01-01T00:00:00", "effective_to": "2025-06-01T00:00:00", "components": {}},
            {"id": "b", "effective_from": "2025-06-01T00:00:00", "effective_to": null, "components": {}}
        ]});
        let norm = normalize_pricing(&ok).unwrap();
        assert_eq!(norm["rules"].as_array().unwrap().len(), 2);
        assert_eq!(norm["rules"][0]["rule_id"], "a");
    }

    #[test]
    fn meta_merge_and_currency_override() {
        let raw = json!({"_meta": {"schema": "o2a-pricing/v2", "custom": 1},
                         "currency": "USD", "version": 3, "x": {"models": {}}});
        let norm = normalize_pricing(&raw).unwrap();
        assert_eq!(norm["_meta"]["currency"], "USD");
        assert_eq!(norm["_meta"]["version"], 3);
        assert_eq!(norm["_meta"]["custom"], 1);
    }

    #[test]
    fn entry_level_idempotent() {
        // 条目级幂等（与 Python _entry_to_v2 一致）：归一化输出再入 _explicit 分支原样通过。
        // 注意：顶层 normalize_pricing(归一化输出) 不幂等（"models" 键会被当作 provider 名），
        // Python 同样如此（已实测确认），此处不测。
        let e = json!({"tiers": [{"input": 1.0, "output": 4.0}], "discount": 0.5});
        let once = entry_to_v2_entry(&e);
        let twice = entry_to_v2_entry(&once);
        assert_eq!(once, twice);
        assert_eq!(twice["billing"], "token");
        assert_eq!(twice["components"]["cache_read"], 0.2);
    }
}
