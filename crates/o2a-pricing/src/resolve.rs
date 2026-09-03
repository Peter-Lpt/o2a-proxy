//! 覆盖链解析：从 desktop/src-tauri/src/pricing.rs 逐字提取。
//!
//! 服务级 > 账号级（键序 id→name）> 模型级；v3 rules 按事件时间 + 最具体 scope。

use serde_json::Value;

use chrono::{DateTime, NaiveDateTime};

/// 覆盖链解析：服务级 > 账号级（键序）> 模型级。未命中返回 None（cost 0）。
pub fn resolve_entry<'a>(
    pricing: &'a Value,
    model: &str,
    account_keys: &[String],
    service_id: &str,
) -> Option<&'a Value> {
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

pub(crate) fn resolve_v1_v2<'a>(
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
        if pname.starts_with('_') || pname == "accounts" || pname == "services" || pname == "rules"
        {
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
    let svc = scope
        .and_then(|s| s.get("service"))
        .and_then(|v| v.as_str())
        .unwrap_or("*");
    let acc = scope
        .and_then(|s| s.get("account"))
        .and_then(|v| v.as_str())
        .unwrap_or("*");
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_level_beats_global() {
        let pricing = serde_json::json!({
            "p": {"models": {"m": {"components": {"input": 1.0}}}},
            "services": {"svc-a": {"models": {"m": {"components": {"input": 9.0}}}}}
        });
        let e = resolve_entry(&pricing, "m", &[], "svc-a").unwrap();
        assert_eq!(e["components"]["input"], 9.0);
        let e2 = resolve_entry(&pricing, "m", &[], "svc-b").unwrap();
        assert_eq!(e2["components"]["input"], 1.0);
    }

    #[test]
    fn account_key_order_id_first() {
        let pricing = serde_json::json!({
            "accounts": {
                "acc-1": {"models": {"m": {"components": {"input": 2.0}}}},
                "我的账号": {"models": {"m": {"components": {"input": 3.0}}}}
            }
        });
        let keys = vec!["acc-1".to_string(), "我的账号".to_string()];
        let e = resolve_entry(&pricing, "m", &keys, "").unwrap();
        assert_eq!(e["components"]["input"], 2.0);
        let keys2 = vec!["我的账号".to_string()];
        let e2 = resolve_entry(&pricing, "m", &keys2, "").unwrap();
        assert_eq!(e2["components"]["input"], 3.0);
    }

    #[test]
    fn v3_rules_time_window() {
        let pricing = serde_json::json!({
            "version": 3,
            "rules": [
                {"id": "old", "model": "m", "scope": {"service": "*", "account": "*"},
                 "effective_from": "2025-01-01T00:00:00", "effective_to": "2025-06-01T00:00:00",
                 "components": {"input": 1.0}},
                {"id": "new", "model": "m", "scope": {"service": "*", "account": "*"},
                 "effective_from": "2025-06-01T00:00:00", "effective_to": null,
                 "components": {"input": 5.0}}
            ]
        });
        let e = resolve_entry_at(&pricing, "m", &[], "", Some("2025-03-01T00:00:00")).unwrap();
        assert_eq!(e["components"]["input"], 1.0);
        let e2 = resolve_entry_at(&pricing, "m", &[], "", Some("2025-07-01T00:00:00")).unwrap();
        assert_eq!(e2["components"]["input"], 5.0);
        // 有 rules 但时间不命中：fail closed，不冒充当前价
        assert!(resolve_entry_at(&pricing, "m", &[], "", Some("2024-01-01T00:00:00")).is_none());
        assert!(resolve_entry_at(&pricing, "m", &[], "", None).is_none());
    }

    #[test]
    fn v3_specificity_wins() {
        let pricing = serde_json::json!({
            "rules": [
                {"model": "m", "components": {"input": 1.0}},
                {"model": "m", "scope": {"service": "svc-a", "account": "*"},
                 "components": {"input": 7.0}}
            ]
        });
        let e = resolve_entry_at(&pricing, "m", &[], "svc-a", Some("2025-01-01T00:00:00")).unwrap();
        assert_eq!(e["components"]["input"], 7.0);
    }
}
