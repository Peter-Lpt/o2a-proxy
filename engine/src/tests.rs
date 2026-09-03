//! M2 引擎骨架契约测试：随机端口起 axum 测试服务器，覆盖
//! /health 豁免、鉴权、/models 矩阵、/status、503 reloading、/_reload swap、
//! POST 501 占位、--service 过滤、diff_services。

use std::sync::Arc;

use axum::Router;
use serde_json::Value;

use crate::handlers::build_router;
use crate::state::{EngineState, ServiceState};

/// 起单个测试服务：直接 bind :0 拿真实端口。
async fn spawn(svc: o2a_config::Service) -> (u16, Arc<ServiceState>) {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let mut svc = svc;
    svc.host = "127.0.0.1".into();
    svc.port = port as i64;
    let st = Arc::new(ServiceState::new(svc, None));
    let router: Router = build_router(st.clone());
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (port, st)
}

async fn get(port: u16, path: &str, token: Option<&str>) -> (u16, Value) {
    let client = reqwest::Client::new();
    let mut req = client.get(format!("http://127.0.0.1:{port}{path}"));
    if let Some(t) = token {
        req = req.bearer_auth(t);
    }
    let resp = req.send().await.unwrap();
    let status = resp.status().as_u16();
    (status, resp.json::<Value>().await.unwrap_or(Value::Null))
}

async fn get_raw(port: u16, path: &str) -> reqwest::Response {
    reqwest::Client::new()
        .get(format!("http://127.0.0.1:{port}{path}"))
        .send()
        .await
        .unwrap()
}

async fn post(port: u16, path: &str) -> (u16, Value) {
    let resp = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{port}{path}"))
        .send()
        .await
        .unwrap();
    let status = resp.status().as_u16();
    (status, resp.json::<Value>().await.unwrap_or(Value::Null))
}

use o2a_config::{ClientKind, ModelPolicy, ModelsMap};

fn base_service(id: &str, name: &str, model: &str) -> o2a_config::Service {
    o2a_config::Service {
        id: id.to_string(),
        name: name.to_string(),
        account: o2a_config::Account {
            id: "acc-1".to_string(),
            name: "账号A".to_string(),
            api_key: "sk-test".to_string(),
            openai_url: "https://upstream.example/v1/chat/completions".to_string(),
            anthropic_url: String::new(),
            api: String::new(),
            quota_source: "auto".to_string(),
            quota: None,
        },
        client: ClientKind::Openai,
        host: "127.0.0.1".to_string(),
        port: 0,
        model: model.to_string(),
        override_model: true,
        max_tokens: 4096,
        proxy: String::new(),
        api: None,
        upstream_api: o2a_config::UpstreamApi::OpenaiCompletions,
        thinking_mode: o2a_config::ThinkingMode::Auto,
        pricing_mode: o2a_config::PricingMode::Token,
        pricing_extra: None,
        pricing_raw: serde_json::Value::String(String::new()),
        auth_token: String::new(),
        order: 0,
        enabled: true,
        autostart: false,
        models: Vec::new(),
        models_map: ModelsMap::default(),
        model_policy: ModelPolicy::Clamp,
        mode_override: None,
    }
}

#[tokio::test]
async fn health_exempt_any_method_and_auth() {
    let mut svc = base_service("svc-h", "health-svc", "m1");
    svc.auth_token = "tok-1".into();
    let (port, _st) = spawn(svc).await;
    // GET /health 无凭证放行
    let (code, body) = get(port, "/health", None).await;
    assert_eq!(code, 200);
    assert_eq!(body["status"], "ok");
    // POST /health 同样豁免鉴权（path 精确豁免，不分方法）：
    // 豁免后落入代理分发 → 501 占位（若豁免失效这里会是 401）
    let (code, _) = post(port, "/health").await;
    assert_eq!(code, 501);
    // /health/ 不豁免（精确匹配）
    let resp = get_raw(port, "/health/").await;
    assert_eq!(resp.status().as_u16(), 401);
}

#[tokio::test]
async fn auth_401_bearer_and_x_api_key() {
    let mut svc = base_service("svc-a", "auth-svc", "m1");
    svc.auth_token = "tok-1".into();
    let (port, _st) = spawn(svc).await;

    // 无凭证 / 错凭证 → 401 双协议错误体
    let client = reqwest::Client::new();
    for prepare in [
        |r: reqwest::RequestBuilder| r,
        |r: reqwest::RequestBuilder| r.bearer_auth("wrong"),
    ] {
        let resp = prepare(client.get(format!("http://127.0.0.1:{port}/models")))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 401);
        let body: Value = resp.json().await.unwrap();
        assert_eq!(body["type"], "error");
        assert_eq!(body["error"]["type"], "authentication_error");
        assert!(body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("x-api-key"));
    }
    // Bearer 正确 → 200
    let (code, _) = get(port, "/models", Some("tok-1")).await;
    assert_eq!(code, 200);
    // x-api-key 正确 → 200
    let resp = client
        .get(format!("http://127.0.0.1:{port}/models"))
        .header("x-api-key", "tok-1")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    // 空 token 头不通过（Python: bool(supplied)）
    let resp = client
        .get(format!("http://127.0.0.1:{port}/models"))
        .header("x-api-key", "  ")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 401);
}

#[tokio::test]
async fn models_no_whitelist_single_entry() {
    let svc = base_service("svc-m", "m-svc", "main-model");
    let (port, _st) = spawn(svc).await;
    for path in ["/models", "/v1/models"] {
        let (code, body) = get(port, path, None).await;
        assert_eq!(code, 200);
        assert_eq!(body["object"], "list");
        let data = body["data"].as_array().unwrap();
        assert_eq!(data.len(), 1);
        assert_eq!(data[0]["id"], "main-model");
        assert_eq!(data[0]["owned_by"], "账号A");
        assert_eq!(data[0]["context"], 4096);
        assert_eq!(data[0]["required"], true);
        // 单条路径无 default 键（对齐 Python `_model_entry`）
        assert!(data[0].get("default").is_none());
    }
}

#[tokio::test]
async fn models_whitelist_alias_default_matrix() {
    let mut svc = base_service("svc-w", "w-svc", "m-main");
    svc.models = vec!["m-a".into(), "m-b".into()];
    svc.models_map = ModelsMap(vec![("alias-x".into(), "m-c".into())]);
    let (port, _st) = spawn(svc).await;
    let (_, body) = get(port, "/models", None).await;
    let data = body["data"].as_array().unwrap();
    let ids: Vec<&str> = data.iter().map(|e| e["id"].as_str().unwrap()).collect();
    // 主模型补入首条（default=true）；白名单序保持；别名键列入；上游名 m-c 不暴露
    assert_eq!(ids, vec!["m-main", "m-a", "m-b", "alias-x"]);
    assert_eq!(data[0]["default"], true);
    assert_eq!(data[0]["required"], true);
    assert_eq!(data[1]["default"], false);
    assert_eq!(data[1]["required"], false);
    assert_eq!(data[3]["required"], false); // 别名条目 required=false
    assert!(!ids.contains(&"m-c"));
}

#[tokio::test]
async fn status_shape_and_task_state() {
    let svc = base_service("svc-s", "s-svc", "m1");
    let (port, st) = spawn(svc).await;
    let (code, body) = get(port, "/status", None).await;
    assert_eq!(code, 200);
    assert_eq!(body["active"], false);
    assert_eq!(body["active_streams"], 0);
    assert_eq!(body["last_finish"], "none");
    assert_eq!(body["service"], "s-svc");
    assert_eq!(body["mode"], "codex"); // client=openai → codex
    assert!(body["port"].as_i64().unwrap() > 0);
    assert_eq!(body["last_activity"], 0.0);

    // 任务状态语义经 /status 可见
    {
        let mut t = st.task.lock().unwrap();
        t.begin();
        t.finish(false);
    }
    let (_, body) = get(port, "/status", None).await;
    assert_eq!(body["active_streams"], 1);
    assert_eq!(body["last_finish"], "continue");
    assert_eq!(body["active"], true);
    assert!(body["last_activity"].as_f64().unwrap() > 0.0);
}

#[tokio::test]
async fn root_summary_shape() {
    let svc = base_service("svc-r", "r-svc", "m1");
    let (port, _st) = spawn(svc).await;
    let (code, body) = get(port, "/", None).await;
    assert_eq!(code, 200);
    assert_eq!(body["status"], "ok");
    assert_eq!(body["mode"], "codex");
    assert_eq!(body["client"], "openai");
    assert_eq!(body["account"], "账号A");
    assert_eq!(body["target"], "https://upstream.example/v1/chat/completions");
    let endpoints = body["endpoints"].as_array().unwrap();
    assert_eq!(endpoints.len(), 5);
    assert_eq!(endpoints[0], "/stats?period=hour|day|all");
    // 未知 GET 路径回退根摘要（对齐 Python handle_get 兜底）
    let (code, body) = get(port, "/unknown-path", None).await;
    assert_eq!(code, 200);
    assert_eq!(body["status"], "ok");
}

#[tokio::test]
async fn post_proxy_dispatch_501_placeholder() {
    let svc = base_service("svc-p", "p-svc", "m1");
    let (port, _st) = spawn(svc).await;
    let (code, body) = post(port, "/v1/messages").await;
    assert_eq!(code, 501);
    assert_eq!(body["error"]["type"], "api_error");
    // M4/M5 端点同样 501
    let (code, _) = post(port, "/pricing-reload").await; // 先确认端点存在（见下个测试）
    assert_eq!(code, 200);
    let (code, _) = get(port, "/stats", None).await;
    assert_eq!(code, 501);
    let (code, _) = get(port, "/quota", None).await;
    assert_eq!(code, 501);
    let (code, _) = get(port, "/pricing-meta", None).await;
    assert_eq!(code, 501);
}

/// 重载链路综合测试（共享全局 RELOADING 标志，故合并为一个串行用例）：
/// 503 语义 → /_reload swap（model 原地生效）→ /_reload start（新增服务）→ /pricing-reload。
#[tokio::test]
async fn reloading_flag_and_reload_endpoints() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.json");
    // 预留端口（并行测试的 :0 分配不会撞上）；实际绑定前释放
    let guard1 = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port1 = guard1.local_addr().unwrap().port();
    let guard2 = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port2 = guard2.local_addr().unwrap().port();
    let config = serde_json::json!({
        "accounts": [{"id": "acc-1", "name": "a1", "api_key": "sk-1", "openai_url": "https://x.example/v1"}],
        "services": [
            {"id": "svc-t1", "comment": "t1", "account": "acc-1", "client": "openai",
             "listen_host": "127.0.0.1", "listen_address": port1, "model": "m1"},
            {"id": "svc-t2", "comment": "t2", "account": "acc-1", "client": "openai",
             "listen_host": "127.0.0.1", "listen_address": port2, "model": "m2"}
        ]
    });
    // 初始配置只有 t1
    let mut initial = config.clone();
    initial["services"] = serde_json::json!([config["services"][0].clone()]);
    std::fs::write(&config_path, initial.to_string()).unwrap();

    let engine = Arc::new(
        EngineState::new(config_path.clone(), None).expect("client build"),
    );
    let services = o2a_config::load_config_at(&config_path);
    assert_eq!(services.len(), 1);
    drop(guard1); // 让位给引擎绑定（极小竞态窗口，可接受）
    let handle = engine.start_service(services[0].clone()).await.unwrap();
    engine
        .runners
        .write()
        .unwrap()
        .insert(services[0].id.clone(), handle);

    // --- 503 语义：非 /health 全部 503 + Retry-After: 2 ---
    // 引擎级标记（不再操纵进程级全局，避免与其他并行测试的引擎实例互扰）
    engine.reloading.0.store(true, std::sync::atomic::Ordering::SeqCst);
    let resp = get_raw(port1, "/models").await;
    assert_eq!(resp.status().as_u16(), 503);
    assert_eq!(resp.headers()["Retry-After"], "2");
    let (code, _) = get(port1, "/health", None).await;
    assert_eq!(code, 200);
    engine.reloading.0.store(false, std::sync::atomic::Ordering::SeqCst);

    // --- /pricing-reload 端点契约 ---
    let (code, body) = post(port1, "/pricing-reload").await;
    assert_eq!(code, 200);
    assert_eq!(body["status"], "pricing reloaded");

    // --- /_reload swap：t1 model m1 → m2'，同端口原地生效 ---
    let mut swapped = initial.clone();
    swapped["services"][0]["model"] = serde_json::json!("m1-swapped");
    std::fs::write(&config_path, swapped.to_string()).unwrap();
    let (code, body) = post(port1, "/_reload").await;
    assert_eq!(code, 200);
    assert_eq!(body["status"], "reloading");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if !engine.reloading.active() {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "reload not finished");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let (_, body) = get(port1, "/status", None).await;
    assert_eq!(body["service"], "t1");
    // swap 生效验证走 /models（model 字段在 /status 不展示，models 列表取 service.model）
    let (_, body) = get(port1, "/models", None).await;
    assert_eq!(body["data"][0]["id"], "m1-swapped");

    // --- /_reload start：配置加回 t2 → 新端口可服务 ---
    drop(guard2);
    std::fs::write(&config_path, config.to_string()).unwrap();
    let (code, _) = post(port1, "/_reload").await;
    assert_eq!(code, 200);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let ok = reqwest::Client::new()
            .get(format!("http://127.0.0.1:{port2}/health"))
            .send()
            .await
            .map(|r| r.status().as_u16() == 200)
            .unwrap_or(false);
        if ok || std::time::Instant::now() > deadline {
            assert!(ok, "reload did not start svc-t2 on port {port2}");
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    engine.shutdown_all().await;
}
