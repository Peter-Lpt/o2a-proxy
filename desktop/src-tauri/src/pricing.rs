//! pricing 计价：薄转发层。
//!
//! 实现已收敛到 workspace 的 o2a-pricing crate（crates/o2a-pricing/），
//! 此处仅 re-export 保持 `crate::pricing::*` 调用点不变，消除双实现漂移。
//! 双端一致仍由共享 golden fixtures 保证：`pricing/golden/cases.json`
//! 与 pytest 跑同一份文件（见下方 `golden_fixtures_parity` 测试）。

// 模块本身为私有（lib.rs `mod pricing;`），非 resolve_cost 的 re-export 在
// 非 test 构建下无 crate 内使用方；tests 通过 `use super::*` 取用。
#[allow(unused_imports)]
pub use o2a_pricing::{entry_to_v2, evaluate, resolve_cost, resolve_entry, resolve_entry_at};

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

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
