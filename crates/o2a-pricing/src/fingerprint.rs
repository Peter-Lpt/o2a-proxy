//! 价格目录指纹：移植自 Python `o2a/pricing/fingerprint.py`。
//!
//! 指纹用于缓存失效：引擎统计读取以重算为主，指纹改变时丢弃聚合缓存。
//! canonical JSON 语义与 Python `json.dumps(sort_keys=True, separators=(",", ":"),
//! ensure_ascii=False)` 一致（serde_json Map 默认 BTreeMap = 键排序）。

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::schema::normalize_pricing;
use crate::DEFAULT_CURRENCY;

/// 计算价格目录指纹。对归一化后的 v1/v2/v3 结构做 canonical JSON，
/// 并叠加 provider/route 身份。返回 64 位 hex（SHA-256）。
///
/// v3 rules 区间重叠时归一化失败 → Err（与 Python ValueError 冒泡一致）。
pub fn pricing_fingerprint(raw: &Value, provider_identity: &str) -> Result<String, String> {
    let normalized = if raw.is_object() {
        normalize_pricing(raw)?
    } else {
        normalize_pricing(&Value::Null)?
    };
    let meta = normalized.get("_meta").and_then(|v| v.as_object());
    let get_or = |key: &str, d: Value| -> Value {
        // Python .get(key, default)：仅键缺失时回退；键存在为 null 则保留 null
        match meta.and_then(|m| m.get(key)) {
            Some(v) => v.clone(),
            None => d,
        }
    };
    let mut payload = Map::new();
    payload.insert(
        "schema".into(),
        get_or("schema", Value::Null),
    );
    payload.insert(
        "version".into(),
        get_or("version", Value::Null),
    );
    payload.insert(
        "currency".into(),
        get_or("currency", json!(DEFAULT_CURRENCY)),
    );
    payload.insert(
        "models".into(),
        normalized.get("models").cloned().unwrap_or(Value::Null),
    );
    payload.insert(
        "accounts".into(),
        normalized.get("accounts").cloned().unwrap_or(Value::Null),
    );
    payload.insert(
        "services".into(),
        normalized.get("services").cloned().unwrap_or(Value::Null),
    );
    payload.insert(
        "rules".into(),
        normalized.get("rules").cloned().unwrap_or(Value::Null),
    );
    let canonical = serde_json::to_string(&Value::Object(payload))
        .map_err(|e| format!("canonical json failed: {e}"))?;
    let mut h = Sha256::new();
    h.update(canonical.as_bytes());
    if !provider_identity.is_empty() {
        h.update(b"|");
        h.update(provider_identity.as_bytes());
    }
    Ok(hex_encode(&h.finalize()))
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// fixture 指纹由 Python o2a/pricing/fingerprint.py 预先计算并固化：
    /// 见 docs 内评审记录（双端一致性测试）。
    const FIXTURE_A: &str = r#"{
        "dashscope": {"models": {"qwen-plus": {"tiers": [{"input": 0.8, "output": 2.0, "cache_hit": 0.2}]}}},
        "accounts": {"acc-1": {"models": {"qwen-plus": {"tiers": [{"input": 0.4, "output": 1.0}]}}}},
        "currency": "CNY", "version": 2,
        "deepseek": {"models": {"ds": {"tiers": [{"input": 1.0, "output": 4.0}], "discount": 0.35, "discount_note": "promo"}}}
    }"#;
    const EXPECTED_A: &str = "9785def6a9df74078a536b3ee4e7bbd1828f931003a93033058b97c442fc93ab";
    const EXPECTED_B: &str = "587b70e41efa8b719cd1ec5613c88d898634f5bdae66e157f0ad65dfa0912d39";

    #[test]
    fn matches_python_fingerprint() {
        let raw: Value = serde_json::from_str(FIXTURE_A).unwrap();
        assert_eq!(pricing_fingerprint(&raw, "").unwrap(), EXPECTED_A);
    }

    #[test]
    fn provider_identity_changes_hash() {
        let raw: Value = serde_json::from_str(FIXTURE_A).unwrap();
        assert_eq!(
            pricing_fingerprint(&raw, "test-route").unwrap(),
            EXPECTED_B
        );
    }

    #[test]
    fn fingerprint_stable_and_price_sensitive() {
        let raw: Value = serde_json::from_str(FIXTURE_A).unwrap();
        let fp1 = pricing_fingerprint(&raw, "").unwrap();
        let fp2 = pricing_fingerprint(&raw, "").unwrap();
        assert_eq!(fp1, fp2);
        // 改价格 → 指纹变（缓存失效语义）
        let mut changed = raw.clone();
        changed["dashscope"]["models"]["qwen-plus"]["tiers"][0]["input"] = serde_json::json!(0.9);
        assert_ne!(pricing_fingerprint(&changed, "").unwrap(), fp1);
    }

    #[test]
    fn non_dict_input_handled() {
        // Python: normalize_pricing(raw) if isinstance(raw, dict) else {}
        // → payload 各字段缺失 → null；不 panic
        let fp = pricing_fingerprint(&Value::Null, "").unwrap();
        assert_eq!(fp.len(), 64);
    }

    #[test]
    fn overlapping_rules_propagate_error() {
        let raw = json!({"rules": [
            {"id": "a", "effective_from": "2025-01-01T00:00:00", "effective_to": "2025-06-01T00:00:00", "components": {}},
            {"id": "b", "effective_from": "2025-05-01T00:00:00", "effective_to": null, "components": {}}
        ]});
        assert!(pricing_fingerprint(&raw, "").is_err());
    }
}
