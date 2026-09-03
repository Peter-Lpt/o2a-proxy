//! golden fixtures 回归：与 pytest（tests/test_pricing_golden.py）跑同一份
//! `pricing/golden/cases.json`，固化 Python / Rust 双实现零漂移。
//!
//! 测试逻辑与 desktop/src-tauri/src/pricing.rs 的 golden_fixtures_parity 一致。

use serde_json::Value;

use o2a_pricing::resolve_cost;

fn golden_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("pricing")
        .join("golden")
        .join("cases.json")
}

/// 从 golden case 字段构造 resolve_cost 的 ctx（与 Python 调用侧一致：
/// timestamp / context_tokens → meta / cumulative → cumulative.tokens）。
fn build_ctx(case: &Value) -> Option<Value> {
    let ts = case.get("timestamp").and_then(|v| v.as_str());
    let ct = case.get("context_tokens").and_then(|v| v.as_i64());
    let cum = case.get("cumulative").and_then(|v| v.as_f64());
    let meta = case.get("meta").cloned().unwrap_or(Value::Null);
    match (ts, ct, cum, meta) {
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
    }
}

#[test]
fn golden_fixtures_parity() {
    let path = golden_path();
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
        let ctx = build_ctx(case);
        let sid = case.get("service_id").and_then(|v| v.as_str()).unwrap_or("");
        let total = resolve_cost(
            &pricing,
            model,
            usage.get("input").and_then(|v| v.as_i64()).unwrap_or(0),
            usage
                .get("cache_read")
                .and_then(|v| v.as_i64())
                .unwrap_or(0),
            usage
                .get("cache_write")
                .and_then(|v| v.as_i64())
                .unwrap_or(0),
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
