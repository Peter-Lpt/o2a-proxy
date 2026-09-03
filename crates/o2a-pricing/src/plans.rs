//! 套餐/计划目录：移植自 Python `o2a/pricing/plans.py`。
//!
//! plans.json 独立于 pricing.json，支持版本化。Plan 是账号能力（不是模型单价），
//! 通过 services[].pricing.plan 与 account.quota_source 关联；QuotaAdapter 只产出
//! 统一快照，本模块负责补全套餐余量与额度定义。
//!
//! 路径解析差异：Python 默认 `PROJECT_ROOT/plans.json`；crate 无法感知项目根，
//! `load_plans` 接受显式 path（引擎按 §3.1 规则解析后传入），None 时回退当前
//! 目录 `plans.json`。

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// 加载 plans.json（或直接传入 raw Value）。
/// 文件不存在/解析失败时返回空目录（调用方按空套餐继续，不视为错误）。
pub fn load_plans(path: Option<&std::path::Path>, raw: Option<&Value>) -> Value {
    let raw = match raw {
        Some(r) => Some(r.clone()),
        None => {
            let p = path
                .map(std::path::Path::to_path_buf)
                .unwrap_or_else(|| std::path::PathBuf::from("plans.json"));
            match std::fs::read_to_string(&p) {
                Ok(text) => serde_json::from_str(&text).ok(),
                Err(_) => None,
            }
        }
    };
    normalize_plans(raw)
}

fn normalize_plans(raw: Option<Value>) -> Value {
    let Some(r) = raw else {
        return json!({"_meta": {"schema": "o2a-plans/v1"}, "plans": {}});
    };
    if !r.is_object() {
        return json!({"_meta": {"schema": "o2a-plans/v1"}, "plans": {}});
    }
    let plans = r
        .get("plans")
        .and_then(|p| p.as_object())
        .cloned()
        .unwrap_or_default();
    let version = r.get("version").cloned().unwrap_or(json!(1));
    json!({
        "_meta": {"schema": "o2a-plans/v1", "version": version},
        "plans": plans,
    })
}

/// 取单个计划。`plans` 可传 load_plans 的返回值或裸 plans dict
/// （与 Python 一致：直接按键查找，调用方决定传哪一层）。
pub fn get_plan(plan_name: &str, plans: Option<&Value>) -> Option<Value> {
    if plan_name.is_empty() {
        return None;
    }
    let obj = plans?.as_object()?;
    let plan = obj.get(plan_name)?;
    let mut out = plan.as_object()?.clone();
    out.entry("name".to_string()).or_insert(json!(plan_name));
    Some(Value::Object(out))
}

/// 便捷入口：从 raw plans.json 内容取计划（对应 Python get_plan(raw_plans=...)）。
pub fn get_plan_from_raw(plan_name: &str, raw: &Value) -> Option<Value> {
    let loaded = normalize_plans(Some(raw.clone()));
    get_plan(plan_name, loaded.get("plans"))
}

/// 把计划声明的 windows 转成额度窗口模板（limit 来自 included）。
/// 供 /quota 补全套餐余量展示；实际 used/reset_at 仍由适配器提供。
pub fn plan_windows_to_snapshot(plan: &Value) -> Vec<Value> {
    let Some(obj) = plan.as_object() else {
        return Vec::new();
    };
    let included = obj.get("included").cloned().unwrap_or(json!({}));
    let mut out = Vec::new();
    for w in obj
        .get("windows")
        .and_then(|w| w.as_array())
        .into_iter()
        .flatten()
    {
        let Some(wobj) = w.as_object() else { continue };
        let unit = wobj
            .get("unit")
            .and_then(|u| u.as_str())
            .unwrap_or("requests");
        // Python: included.get(unit) —— 键缺失与值为 null 都得 None
        let limit = included.get(unit).cloned().unwrap_or(Value::Null);
        out.push(json!({
            "kind": wobj.get("kind").and_then(|k| k.as_str()).unwrap_or("session"),
            "period": wobj.get("period").and_then(|p| p.as_str()).unwrap_or("month"),
            "unit": unit,
            "limit": limit,
            "used": Value::Null,
            "pct": Value::Null,
            "reset_at": Value::Null,
        }));
    }
    out
}

/// 套餐目录指纹（canonical JSON + SHA-256，用于缓存失效）。
pub fn plans_fingerprint(raw: Option<&Value>) -> String {
    let data = load_plans(None, raw);
    // serde_json Map 为 BTreeMap：键排序 + 紧凑分隔，等价 Python
    // json.dumps(data, ensure_ascii=False, sort_keys=True, separators=(",", ":"))
    let canonical = serde_json::to_string(&data).unwrap_or_default();
    let mut h = Sha256::new();
    h.update(canonical.as_bytes());
    let bytes = h.finalize();
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Value {
        serde_json::from_str(
            r#"{"version": 1, "plans": {"glm": {"mode": "subscription", "currency": "CNY",
                "windows": [{"kind": "session", "period": "rolling-5h", "unit": "requests"},
                            {"kind": "weekly", "period": "week", "unit": "tokens"}],
                "included": {"requests": 500, "tokens": 10000000}}}}"#,
        )
        .unwrap()
    }

    /// 期望值由 Python o2a/pricing/plans.py 的 plans_fingerprint 预先计算固化。
    #[test]
    fn fingerprint_matches_python() {
        assert_eq!(
            plans_fingerprint(Some(&fixture())),
            "46268e1c5c3094f3a9ec8713531d75d8945cd625f9932edef925725f95c70ef5"
        );
    }

    #[test]
    fn load_plans_extracts_meta_and_plans() {
        let loaded = load_plans(None, Some(&fixture()));
        assert_eq!(loaded["_meta"]["schema"], "o2a-plans/v1");
        assert_eq!(loaded["_meta"]["version"], 1);
        assert!(loaded["plans"]["glm"].is_object());
    }

    #[test]
    fn load_plans_missing_file_empty_catalog() {
        let loaded = load_plans(Some(std::path::Path::new("/nonexistent/plans.json")), None);
        assert_eq!(loaded["_meta"]["schema"], "o2a-plans/v1");
        assert!(loaded["plans"].as_object().unwrap().is_empty());
        // 注意：加载失败路径无 version 键（与 Python 一致）
        assert!(loaded["_meta"].get("version").is_none());
    }

    #[test]
    fn non_dict_raw_empty_catalog() {
        let loaded = load_plans(None, Some(&json!("not-a-dict")));
        assert!(loaded["plans"].as_object().unwrap().is_empty());
    }

    #[test]
    fn get_plan_injects_name() {
        let loaded = load_plans(None, Some(&fixture()));
        let plan = get_plan_from_raw("glm", &fixture()).unwrap();
        assert_eq!(plan["name"], "glm");
        assert_eq!(plan["mode"], "subscription");
        assert!(loaded["plans"].is_object());
    }

    #[test]
    fn get_plan_missing_returns_none() {
        assert!(get_plan_from_raw("nope", &fixture()).is_none());
        assert!(get_plan_from_raw("", &fixture()).is_none());
    }

    #[test]
    fn windows_snapshot_limits_from_included() {
        let plan = get_plan_from_raw("glm", &fixture()).unwrap();
        let windows = plan_windows_to_snapshot(&plan);
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0]["kind"], "session");
        assert_eq!(windows[0]["unit"], "requests");
        assert_eq!(windows[0]["limit"], 500);
        assert_eq!(windows[1]["unit"], "tokens");
        assert_eq!(windows[1]["limit"], 10000000);
        assert!(windows[0]["used"].is_null());
        assert!(windows[0]["pct"].is_null());
        assert!(windows[0]["reset_at"].is_null());
    }

    #[test]
    fn windows_unit_without_included_limit() {
        let plan = json!({"windows": [{"kind": "day", "unit": "usd"}]});
        let windows = plan_windows_to_snapshot(&plan);
        assert_eq!(windows.len(), 1);
        assert!(windows[0]["limit"].is_null());
    }
}
