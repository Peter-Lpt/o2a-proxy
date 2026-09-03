//! M5：/quota 与 /pricing-meta 端点（对齐 Python engine.py 的
//! `_quota_snapshot` / `_plan_for_account` / handle_get 对应分支）。
//!
//! - /quota：统计禁用检查 → 账号解析（load_config 现读，热重载即时生效）→
//!   引擎级 TTLCache(60) → 注册表适配器（降级链由 crate 内实现）→ plan 注入
//! - /pricing-meta：pricing.json / plans.json 指纹与概览（§4.2 字段逐一对齐）
//! - 依赖 crates/o2a-quota（Registry/get_snapshot_async/TTLCache）与
//!   crates/o2a-pricing（load_plans/get_plan/plan_windows_to_snapshot/指纹）

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use axum::http::StatusCode;
use axum::response::Response;
use serde_json::{json, Value};

use o2a_quota::base::{QuotaContext, TTLCache};
use o2a_quota::Registry;

use crate::handlers::{json_response, openai_error_response};
use crate::state::{py_truthy, ServiceState};

/// 直连 Router 测试（无 EngineState）回退的进程级缓存实例。
fn fallback_quota_cache() -> &'static TTLCache {
    static CACHE: OnceLock<TTLCache> = OnceLock::new();
    CACHE.get_or_init(|| TTLCache::new(60))
}

/// GET /quota（对齐 handle_get 的 /quota 分支）。
pub async fn handle_quota(st: &ServiceState, query: Option<&str>) -> Response {
    // 统计禁用时明确报错（Python /stats 与 /quota 同规则）
    if !o2a_stats::is_cache_stats_enabled() {
        return json_response(&json!({"error": "cache stats is disabled"}), StatusCode::OK);
    }
    // Python `request.query.get("account") or service.account.id`：空串按缺省回退
    let account_id = {
        let svc = st.service.read().unwrap();
        let qp = crate::handlers::query_params(query);
        qp.get("account")
            .filter(|s| !s.is_empty())
            .cloned()
            .unwrap_or_else(|| svc.account.id.clone())
    };
    let engine = st.engine.as_ref().and_then(|w| w.upgrade());
    let config_path = engine.as_ref().map(|e| e.config_path.clone());
    let cache: &TTLCache = engine
        .as_ref()
        .map(|e| &e.quota_cache)
        .unwrap_or_else(|| fallback_quota_cache());
    let snapshot = quota_snapshot(
        &account_id,
        cache,
        config_path.as_deref(),
        Some(&st.client),
        &Registry::standard(),
    )
    .await;
    json_response(&snapshot, StatusCode::OK)
}

/// 快照组装（对齐 `_quota_snapshot`；registry 参数化供测试注入 stub 适配器）。
pub async fn quota_snapshot(
    account_id: &str,
    cache: &TTLCache,
    config_path: Option<&Path>,
    client: Option<&reqwest::Client>,
    registry: &Registry,
) -> Value {
    // Python 每次 load_config() 现读：热重载后新配置即时生效
    let services = o2a_config::load_config();
    let Some(acc) = services
        .iter()
        .find(|s| s.account.id == account_id)
        .map(|s| s.account.clone())
    else {
        return json!({"error": format!("account not found: {account_id}")});
    };
    let mut ctx = QuotaContext::new(effective_stats_dir().to_string_lossy().to_string(), acc.clone());
    if let Some(c) = client {
        ctx = ctx.with_session(c.clone());
    }
    // Python 双保险 try/except → snapshot None；crate 版降级链内部完成，不外泄错误
    let snapshot = o2a_quota::get_snapshot_async(registry, &acc, &ctx, Some(cache), true).await;
    let Some(mut snapshot) = snapshot else {
        return Value::Null; // Python: json_response(None)
    };
    // plan 注入（对齐 `_plan_for_account` + snapshot 补全段）
    let (plan_name, plan) = plan_for_account(account_id, config_path);
    if let (Some(pname), Some(p)) = (plan_name, plan) {
        if let Some(obj) = snapshot.as_object_mut() {
            // {**plan, "name": plan_name}：name 以解析出的 plan_name 为准
            let mut plan_obj = p.as_object().cloned().unwrap_or_default();
            plan_obj.insert("name".into(), json!(pname));
            obj.insert("plan".into(), Value::Object(plan_obj));
            obj.insert("planName".into(), json!(pname));
            // 仅当适配器未产出窗口时用套餐模板补全（Python `if not snapshot.get("windows")`）
            let windows_missing = obj.get("windows").map(|w| !py_truthy(w)).unwrap_or(true);
            if windows_missing {
                obj.insert(
                    "windows".into(),
                    Value::Array(o2a_pricing::plan_windows_to_snapshot(&p)),
                );
            }
        }
    }
    snapshot
}

/// 按账号找 services[].pricing_extra.plan → get_plan（对齐 `_plan_for_account`）。
///
/// 返回 (plan_name, plan)；无 plan 声明或套餐目录缺失时对应项为 None。
/// Python 对非字符串 plan 值做 str() 强转后查目录，此处仅接受字符串（配置约定为字符串）。
fn plan_for_account(account_id: &str, config_path: Option<&Path>) -> (Option<String>, Option<Value>) {
    let services = o2a_config::load_config();
    let plan_name = services
        .iter()
        .find(|s| s.account.id == account_id)
        .and_then(|s| s.pricing_extra.as_ref())
        .and_then(|e| e.get("plan"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let Some(pname) = plan_name else {
        return (None, None);
    };
    // Python get_plan(plan_name) 无参 → 按缺省路径加载 plans.json；Rust 按 §3.1 解析
    let plans_path = o2a_config::paths::resolve_plans_path(config_path);
    let loaded = o2a_pricing::load_plans(Some(&plans_path), None);
    let plan = o2a_pricing::get_plan(&pname, loaded.get("plans"));
    (Some(pname), plan)
}

/// 额度适配器用的统计目录（Python 有效值 = env，load_config 曾把
/// config.cache_stats_dir setdefault 进 env —— 等价于 env → config → 缺省链）。
fn effective_stats_dir() -> PathBuf {
    if let Some(v) = o2a_config::paths::env_value("CACHE_STATS_DIR") {
        return PathBuf::from(v);
    }
    let cfg_path = o2a_config::paths::resolve_config_path();
    let cfg: Value = std::fs::read_to_string(&cfg_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| json!({}));
    o2a_config::resolve_stats_settings(&cfg).dir
}

/// GET /pricing-meta（对齐 handle_get 的 /pricing-meta 分支，§4.2 字段）。
pub fn handle_pricing_meta(st: &ServiceState) -> Response {
    let engine = st.engine.as_ref().and_then(|w| w.upgrade());
    let config_path = engine.as_ref().map(|e| e.config_path.clone());
    let pricing_path = o2a_config::paths::resolve_pricing_path(config_path.as_deref());
    // Python：文件缺失/解析失败 → raw = {}（继续按空目录产出）
    let raw: Value = std::fs::read_to_string(&pricing_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| json!({}));

    let rules_len = raw
        .get("rules")
        .filter(|v| py_truthy(v))
        .and_then(Value::as_array)
        .map(|a| a.len())
        .unwrap_or(0);
    // version: raw.version（falsy 穿透）→ raw._meta.schema（键存在即命中，null 保留）→ "v2"
    let version = match raw.get("version") {
        Some(v) if py_truthy(v) => v.clone(),
        _ => match raw.get("_meta").and_then(Value::as_object) {
            Some(m) => m.get("schema").cloned().unwrap_or(json!("v2")),
            None => json!("v2"),
        },
    };
    // currency: raw.currency → raw._meta.currency → "CNY"（or 链，falsy 穿透）
    let currency = [raw.get("currency"), raw.get("_meta").and_then(|m| m.get("currency"))]
        .into_iter()
        .flatten()
        .find(|v| py_truthy(v))
        .cloned()
        .unwrap_or(json!("CNY"));
    // Python 归一化失败（v3 rules 重叠）会异常冒泡 → 500
    let fingerprint = match o2a_pricing::pricing_fingerprint(&raw, "") {
        Ok(f) => f,
        Err(e) => return openai_error_response(StatusCode::INTERNAL_SERVER_ERROR, &e),
    };
    let plans_path = o2a_config::paths::resolve_plans_path(config_path.as_deref());
    let loaded_plans = o2a_pricing::load_plans(Some(&plans_path), None);
    // serde_json Map 为 BTreeMap：keys() 已排序，等价 Python sorted(...keys())
    let plans_keys: Vec<String> = loaded_plans
        .get("plans")
        .and_then(Value::as_object)
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default();
    // 指纹对归一化后目录计算（crate 内 normalize，等价 Python fingerprint(load_plans())）；
    // 文件缺失时按空目录（等价 Python load_plans() 失败路径），不回退 cwd
    let raw_plans: Option<Value> = std::fs::read_to_string(&plans_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok());
    let plans_fp = o2a_pricing::plans_fingerprint(Some(raw_plans.as_ref().unwrap_or(&json!({}))));
    json_response(
        &json!({
            "fingerprint": fingerprint,
            "version": version,
            "currency": currency,
            "rules": rules_len,
            "plans": plans_keys,
            "plans_fingerprint": plans_fp,
        }),
        StatusCode::OK,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use crate::handlers::build_router;
    use crate::state::EngineState;
    /// env 触碰测试串行化（O2A_CONFIG / CACHE_STATS_* 为进程级全局）。
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    const ENV_KEYS: [&str; 4] = ["O2A_CONFIG", "O2A_PRICING", "O2A_PLANS", "CACHE_STATS_ENABLED"];

    struct EnvGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        saved: Vec<(&'static str, Option<String>)>,
    }

    impl EnvGuard {
        fn acquire() -> Self {
            let g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let mut saved = Vec::new();
            for k in ENV_KEYS {
                saved.push((k, std::env::var(k).ok()));
            }
            EnvGuard { _lock: g, saved }
        }

        fn set(&self, k: &str, v: &std::path::Path) {
            std::env::set_var(k, v);
        }

        fn set_str(&self, k: &str, v: &str) {
            std::env::set_var(k, v);
        }

        fn remove(&self, k: &str) {
            std::env::remove_var(k);
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (k, v) in &self.saved {
                match v {
                    Some(v) => std::env::set_var(k, v),
                    None => std::env::remove_var(k),
                }
            }
        }
    }

    /// 测试夹具目录：config.json + plans.json + pricing.json + 空 stats 目录。
    struct Fixture {
        _dir: tempfile::TempDir,
        stats_dir: PathBuf,
        config_path: PathBuf,
    }

    fn write_fixture(with_plan: bool) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let config = json!({
            "auth_token": "",
            "cache_stats_dir": "",
            "accounts": [
                {"id": "acc-1", "name": "账号A", "openai_url": "https://upstream.example/v1"},
                {"id": "acc-2", "name": "账号B", "openai_url": "https://upstream.example/v1"}
            ],
            "services": [
                {"id": "svc-1", "comment": "s1", "account": "acc-1",
                 "listen_host": "127.0.0.1", "listen_address": 0, "model": "m1",
                 "pricing": if with_plan { json!({"mode": "token", "plan": "glm"}) } else { json!("") }}
            ]
        });
        std::fs::write(
            dir.path().join("config.json"),
            serde_json::to_string(&config).unwrap(),
        )
        .unwrap();
        std::fs::write(
            dir.path().join("plans.json"),
            json!({
                "version": 1,
                "plans": {
                    "glm": {
                        "mode": "subscription", "currency": "CNY",
                        "windows": [
                            {"kind": "session", "period": "rolling-5h", "unit": "requests"},
                            {"kind": "weekly", "period": "week", "unit": "tokens"}
                        ],
                        "included": {"requests": 500, "tokens": 10000000}
                    }
                }
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            dir.path().join("pricing.json"),
            json!({
                "version": "v9", "currency": "USD",
                "rules": [{"match": "m1"}],
                "models": {"m1": {"input": 1, "output": 2}}
            })
            .to_string(),
        )
        .unwrap();
        let stats_dir = dir.path().join("stats");
        std::fs::create_dir_all(&stats_dir).unwrap();
        let config_path = dir.path().join("config.json");
        Fixture { _dir: dir, stats_dir, config_path }
    }

    /// 起 EngineState + 直连 Router 的服务（真实端口），返回 (端口, ServiceState, EngineState)。
    /// EngineState 由调用方持有：它同时承载引擎级 TTL 缓存与配置路径，
    /// 提前 drop 会使 Weak 升级失败 → 回退进程级静态缓存（测试间串数据）。
    async fn spawn_with_engine(
        fx: &Fixture,
        guard: &EnvGuard,
    ) -> (u16, Arc<ServiceState>, Arc<EngineState>) {
        guard.set("O2A_CONFIG", &fx.config_path);
        guard.set("CACHE_STATS_DIR", &fx.stats_dir);
        let engine = Arc::new(EngineState::new(fx.config_path.clone(), None).unwrap());
        let svc = o2a_config::load_config().into_iter().next().unwrap();
        let st = match &engine.stats {
            Some(registry) => Arc::new(ServiceState::with_stats_registry(
                svc,
                Some(Arc::downgrade(&engine)),
                registry.clone(),
            )),
            None => Arc::new(ServiceState::new(svc, Some(Arc::downgrade(&engine)))),
        };
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let router = build_router(st.clone());
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        (port, st, engine)
    }

    async fn get(port: u16, path: &str) -> (u16, Value) {
        let resp = reqwest::Client::new()
            .get(format!("http://127.0.0.1:{port}{path}"))
            .send()
            .await
            .unwrap();
        let status = resp.status().as_u16();
        (status, resp.json::<Value>().await.unwrap_or(Value::Null))
    }

    fn today_record(account: &str) -> String {
        let ts = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S");
        json!({
            "timestamp": ts.to_string(),
            "service": "s1", "account": account, "model": "m1",
            "status": "ok", "input_tokens": 1, "cache_read_tokens": 0,
            "cache_write_tokens": 0, "output_tokens": 1,
            "cache_hit_rate": 0.0, "cache_coverage": 0.0, "cost": 0.0
        })
        .to_string()
    }

    #[tokio::test]
    async fn quota_plan_injection_and_account_not_found() {
        let guard = EnvGuard::acquire();
        guard.remove("CACHE_STATS_ENABLED");
        let fx = write_fixture(true);
        let (port, _st, _engine) = spawn_with_engine(&fx, &guard).await;
        // plan 注入：local 适配器产出非空窗口 → plan/planName 注入但窗口不替换
        let (code, body) = get(port, "/quota?account=acc-1").await;
        assert_eq!(code, 200);
        assert_eq!(body["adapterId"], "local");
        assert_eq!(body["plan"]["name"], "glm");
        assert_eq!(body["plan"]["mode"], "subscription");
        assert_eq!(body["planName"], "glm");
        assert!(!body["windows"].as_array().unwrap().is_empty());
        assert!(body["windows"][0].get("kind").is_some()); // 适配器窗口（非套餐模板）

        // 账号不存在
        let (code, body) = get(port, "/quota?account=acc-none").await;
        assert_eq!(code, 200);
        assert_eq!(body["error"], "account not found: acc-none");

        // account 参数空串 → 回退当前服务账号（Python `or service.account.id`）
        let (code, body) = get(port, "/quota?account=").await;
        assert_eq!(code, 200);
        assert!(body.get("error").is_none());
        assert_eq!(body["adapterId"], "local");
    }

    #[tokio::test]
    async fn quota_stats_disabled() {
        let guard = EnvGuard::acquire();
        guard.set_str("CACHE_STATS_ENABLED", "false");
        let fx = write_fixture(false);
        let (port, _st, _engine) = spawn_with_engine(&fx, &guard).await;
        let (code, body) = get(port, "/quota").await;
        assert_eq!(code, 200);
        assert_eq!(body["error"], "cache stats is disabled");
        let (code, body) = get(port, "/stats").await;
        assert_eq!(code, 200);
        assert_eq!(body["error"], "cache stats is disabled");
    }

    #[tokio::test]
    async fn quota_ttl_cache_hits_at_engine_level() {
        let guard = EnvGuard::acquire();
        guard.remove("CACHE_STATS_ENABLED");
        let fx = write_fixture(false);
        // 手填额度 → manual 适配器（请求数刻度），窗口内计数来自 stats JSONL
        let config: Value = serde_json::from_str(
            &std::fs::read_to_string(&fx.config_path).unwrap(),
        )
        .unwrap();
        let mut config = config;
        config["accounts"][0]["quota"] = json!({"limit": 10, "unit": "requests", "period": "month"});
        std::fs::write(&fx.config_path, config.to_string()).unwrap();
        std::fs::write(
            fx.stats_dir.join(format!(
                "{}.jsonl",
                chrono::Local::now().format("%Y-%m-%d")
            )),
            format!("{}\n", today_record("acc-1")),
        )
        .unwrap();
        let (port, _st, _engine) = spawn_with_engine(&fx, &guard).await;

        // 第一次：缓存未命中，实测计数 1
        let (code, body) = get(port, "/quota?account=acc-1").await;
        assert_eq!(code, 200);
        assert_eq!(body["adapterId"], "manual");
        assert_eq!(body["windows"][0]["used"].as_f64().unwrap(), 1.0);

        // 追加一条记录后再次请求：引擎级 TTL 缓存命中 → used 仍为 1
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(fx.stats_dir.join(format!(
                "{}.jsonl",
                chrono::Local::now().format("%Y-%m-%d")
            )))
            .unwrap();
        f.write_all(format!("{}\n", today_record("acc-1")).as_bytes()).unwrap();
        let (_, body) = get(port, "/quota?account=acc-1").await;
        assert_eq!(body["windows"][0]["used"].as_f64().unwrap(), 1.0);

        // 对照：全新缓存（无 TTL）下实测计数 2，证明上面确为缓存命中而非计数失灵
        let fresh = TTLCache::new(60);
        let snap = quota_snapshot(
            "acc-1",
            &fresh,
            Some(&fx.config_path),
            None,
            &Registry::standard(),
        )
        .await;
        assert_eq!(snap["windows"][0]["used"].as_f64().unwrap(), 2.0);
    }

    /// 套餐窗口模板注入：适配器产出空窗口时用 plan windows 补全（§4.2 / §9）。
    #[tokio::test]
    async fn quota_plan_windows_injected_when_adapter_windows_empty() {
        let guard = EnvGuard::acquire();
        guard.remove("CACHE_STATS_ENABLED");
        let fx = write_fixture(true);
        let engine = Arc::new(EngineState::new(fx.config_path.clone(), None).unwrap());
        guard.set("O2A_CONFIG", &fx.config_path);
        let svc = o2a_config::load_config().into_iter().next().unwrap();
        struct EmptyWindows;
        #[async_trait::async_trait]
        impl o2a_quota::registry::QuotaAdapter for EmptyWindows {
            fn name(&self) -> &'static str {
                "stub-empty"
            }
            async fn fetch(
                &self,
                _ctx: &QuotaContext,
            ) -> Result<Option<Value>, o2a_quota::base::QuotaError> {
                Ok(Some(o2a_quota::base::make_snapshot(
                    "stub-empty",
                    Vec::new(),
                    "provider_api",
                    None,
                    false,
                    &o2a_quota::base::default_now(),
                )))
            }
        }
        let mut registry = Registry::standard();
        registry.register(Arc::new(EmptyWindows));
        // quota_source 指向 stub（已注册显式名直用）
        let mut svc2 = svc.clone();
        svc2.account.quota_source = "stub-empty".into();
        // load_config 现读 config.json（quota_source 来自配置），直接改内存副本无效；
        // 改为直接以内存 Account 构造 QuotaContext 走 quota_snapshot —— 但 account
        // 必须能在 load_config 中找到（quota_snapshot 内部解析）。因此写回配置文件。
        let mut config: Value =
            serde_json::from_str(&std::fs::read_to_string(&fx.config_path).unwrap()).unwrap();
        config["accounts"][0]["quota_source"] = json!("stub-empty");
        std::fs::write(&fx.config_path, config.to_string()).unwrap();

        let snap = quota_snapshot(
            "acc-1",
            &engine.quota_cache,
            Some(&fx.config_path),
            None,
            &registry,
        )
        .await;
        assert_eq!(snap["adapterId"], "stub-empty");
        assert_eq!(snap["plan"]["name"], "glm");
        assert_eq!(snap["planName"], "glm");
        // 空窗口 → 套餐模板补全：2 个窗口，limit 来自 included
        let windows = snap["windows"].as_array().unwrap();
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0]["kind"], "session");
        assert_eq!(windows[0]["unit"], "requests");
        assert_eq!(windows[0]["limit"], 500);
        assert_eq!(windows[1]["unit"], "tokens");
        assert_eq!(windows[1]["limit"], 10000000);
        let _ = (svc2, svc);
    }

    #[tokio::test]
    async fn pricing_meta_fields_with_fixture() {
        let guard = EnvGuard::acquire();
        guard.remove("CACHE_STATS_ENABLED");
        let fx = write_fixture(false);
        let (port, _st, _engine) = spawn_with_engine(&fx, &guard).await;
        let (code, body) = get(port, "/pricing-meta").await;
        assert_eq!(code, 200);
        assert_eq!(body["version"], "v9");
        assert_eq!(body["currency"], "USD");
        assert_eq!(body["rules"], 1);
        assert_eq!(body["plans"], json!(["glm"]));
        assert_eq!(body["fingerprint"].as_str().unwrap().len(), 64);
        assert_eq!(body["plans_fingerprint"].as_str().unwrap().len(), 64);
        // 指纹与 o2a-pricing crate 自身（已被 Python 实测对齐的 fixture 单测覆盖）一致
        let raw: Value = serde_json::from_str(
            &std::fs::read_to_string(fx.config_path.parent().unwrap().join("pricing.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(body["fingerprint"], o2a_pricing::pricing_fingerprint(&raw, "").unwrap());
    }

    #[tokio::test]
    async fn pricing_meta_defaults_when_file_missing() {
        let guard = EnvGuard::acquire();
        guard.remove("CACHE_STATS_ENABLED");
        let fx = write_fixture(false);
        std::fs::remove_file(fx.config_path.parent().unwrap().join("pricing.json")).unwrap();
        let (port, _st, _engine) = spawn_with_engine(&fx, &guard).await;
        let (code, body) = get(port, "/pricing-meta").await;
        assert_eq!(code, 200);
        assert_eq!(body["version"], "v2");
        assert_eq!(body["currency"], "CNY");
        assert_eq!(body["rules"], 0);
        assert_eq!(body["plans"], json!(["glm"])); // plans.json 仍在：套餐目录不受 pricing.json 缺失影响
        assert_eq!(body["fingerprint"].as_str().unwrap().len(), 64);
    }
}
