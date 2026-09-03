//! 单条目归一化（元组形态）：从 desktop/src-tauri/src/pricing.rs 逐字提取。
//!
//! 供 `evaluate` 使用；返回 (components, modifiers)。
//! Python 侧 dict 形态归一化见 `schema` 模块（fingerprint 用）。

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

/// v1 模型级字段 → modifiers（discount；多档 range → context_tier）。
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
    // v1 free_quota（模型级数字，按月 tokens 额度）接入，冲抵最后一步
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
pub fn parse_range(text: &str) -> Option<(i64, Option<i64>)> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v1_tier_bakes_cache_ratios() {
        let entry = serde_json::json!({"tiers": [{"input": 2.0, "output": 8.0}]});
        let (comps, mods) = entry_to_v2(&entry).unwrap();
        assert_eq!(comps["cache_read"], 0.4);
        assert_eq!(comps["cache_write"], 2.0);
        assert!(mods.is_empty());
    }

    #[test]
    fn v1_explicit_cache_hit_miss() {
        let entry = serde_json::json!({"tiers": [{"input": 2.0, "cache_hit": 0.5, "cache_miss": 2.5}]});
        let (comps, _) = entry_to_v2(&entry).unwrap();
        assert_eq!(comps["cache_read"], 0.5);
        assert_eq!(comps["cache_write"], 2.5);
    }

    #[test]
    fn empty_tiers_cost_zero() {
        let (comps, mods) = entry_to_v2(&serde_json::json!({"tiers": []})).unwrap();
        assert!(comps.is_null());
        assert!(mods.is_empty());
    }

    #[test]
    fn range_parsing() {
        assert_eq!(parse_range("0-256K"), Some((0, Some(262144))));
        assert_eq!(parse_range("256K-1M"), Some((262144, Some(1048576))));
        assert_eq!(parse_range("unlimited"), Some((0, None)));
        assert_eq!(parse_range("∞"), Some((0, None)));
        assert_eq!(parse_range("*"), Some((0, None)));
        assert_eq!(parse_range("bad"), None);
    }

    #[test]
    fn output_thinking_carried() {
        let entry = serde_json::json!({"tiers": [{"input": 1.0, "output": 4.0, "output_thinking": 8.0}]});
        let (comps, _) = entry_to_v2(&entry).unwrap();
        assert_eq!(comps["output_thinking"], 8.0);
    }
}
