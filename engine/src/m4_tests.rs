//! M4 契约测试：codex（responses→chat / chat 透传 / passthrough 错误）与
//! direct（流式透传 + usage 抓取）+ stats 接线。
//! 行为基准：Python engine.py handle_passthrough / handle_openai_* / handle_direct_*。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::http::StatusCode;
use axum::response::Response;
use serde_json::{json, Value};
use tokio::sync::mpsc;

use crate::handlers::build_router;
use crate::proxy::StatsMeta;
use crate::state::ServiceState;
use o2a_config::{ClientKind, ModelsMap, OpenaiApi, PricingMode, UpstreamApi};

// ---------------------------------------------------------------------------
// 夹具：mock 上游（chat / responses / anthropic 三路由）+ 测试服务
// ---------------------------------------------------------------------------

struct MockUpstream {
    addr: String,
    captured: Arc<Mutex<Vec<(String, Value)>>>, // (uri, body)
}

async fn spawn_mock(
    status: u16,
    lines: Vec<String>,
    raw_body: Option<Vec<u8>>,
    delay: Duration,
) -> MockUpstream {
    let captured: Arc<Mutex<Vec<(String, Value)>>> = Arc::new(Mutex::new(Vec::new()));
    let captured_h = captured.clone();

    let handler = move |req: axum::extract::Request| async move {
        let uri = req.uri().to_string();
        let body = axum::body::to_bytes(req.into_body(), 1 << 20)
            .await
            .unwrap_or_default();
        let payload: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        captured_h.lock().unwrap().push((uri, payload));

        if let Some(raw) = &raw_body {
            return Response::builder()
                .status(StatusCode::from_u16(status).unwrap())
                .header("content-type", "application/json")
                .body(axum::body::Body::from(raw.clone()))
                .unwrap();
        }
        if status != 200 {
            return Response::builder()
                .status(StatusCode::from_u16(status).unwrap())
                .body(axum::body::Body::from("boom"))
                .unwrap();
        }
        let (tx, rx) = mpsc::channel::<Result<Vec<u8>, std::io::Error>>(4);
        tokio::spawn(async move {
            for line in lines {
                if tx.send(Ok(line.into_bytes())).await.is_err() {
                    return;
                }
                tokio::time::sleep(delay).await;
            }
        });
        Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "text/event-stream")
            .body(axum::body::Body::from_stream(
                tokio_stream::wrappers::ReceiverStream::new(rx),
            ))
            .unwrap()
    };

    let app = axum::Router::new()
        .route(
            "/v1/chat/completions",
            axum::routing::post(handler.clone()),
        )
        .route("/v1/responses", axum::routing::post(handler.clone()))
        .route("/v1/messages", axum::routing::post(handler))
        .with_state(());

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    MockUpstream { addr: format!("http://{addr}"), captured }
}

fn service(
    id: &str,
    client: ClientKind,
    api: Option<OpenaiApi>,
    openai_url: String,
    anthropic_url: String,
) -> o2a_config::Service {
    o2a_config::Service {
        id: id.to_string(),
        name: id.to_string(),
        account: o2a_config::Account {
            id: "acc-1".to_string(),
            name: "账号A".to_string(),
            api_key: "sk-test".to_string(),
            openai_url,
            anthropic_url,
            api: String::new(),
            quota_source: "auto".to_string(),
            quota: None,
        },
        client,
        host: "127.0.0.1".into(),
        port: 0,
        model: "m-main".to_string(),
        override_model: true,
        max_tokens: 4096,
        proxy: String::new(),
        api,
        upstream_api: UpstreamApi::OpenaiCompletions,
        thinking_mode: o2a_config::ThinkingMode::Auto,
        pricing_mode: PricingMode::Token,
        pricing_extra: None,
        pricing_raw: serde_json::Value::String(String::new()),
        auth_token: String::new(),
        order: 0,
        enabled: true,
        autostart: false,
        models: Vec::new(),
        models_map: ModelsMap::default(),
        model_policy: o2a_config::ModelPolicy::Clamp,
        mode_override: None,
    }
}

async fn spawn_service(mut svc: o2a_config::Service) -> (u16, Arc<ServiceState>) {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    svc.host = "127.0.0.1".into();
    svc.port = port as i64;
    let st = Arc::new(ServiceState::new(svc, None));
    let router = build_router(st.clone());
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (port, st)
}

fn data_line(v: &Value) -> String {
    format!("data: {v}\n\n")
}

/// 捕获型统计 sink（验证 record_stats 接线语义）。
/// (model, upstream_model 占位, usage, error)
type CapturedRecord = (String, String, Value, Option<String>);

struct CapturingSink(Mutex<Vec<CapturedRecord>>);

impl crate::proxy::StatsSink for CapturingSink {
    fn record(
        &self,
        _svc: &o2a_config::Service,
        model: &str,
        usage: &Value,
        error: Option<&str>,
        _meta: StatsMeta,
    ) {
        self.0.lock().unwrap().push((
            model.to_string(),
            String::new(), // upstream_model 由 sink 内部处理；此处验证 model 实参
            usage.clone(),
            error.map(String::from),
        ));
    }
}

async fn spawn_service_with_sink(
    mut svc: o2a_config::Service,
    sink: Arc<dyn crate::proxy::StatsSink>,
) -> (u16, Arc<ServiceState>) {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    svc.host = "127.0.0.1".into();
    svc.port = port as i64;
    let st = Arc::new(ServiceState::with_sink(svc, None, sink));
    let router = build_router(st.clone());
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (port, st)
}

fn parse_sse(text: &str) -> Vec<(String, Value)> {
    let mut out = Vec::new();
    let mut cur_type: Option<String> = None;
    for line in text.lines() {
        if let Some(t) = line.strip_prefix("event: ") {
            cur_type = Some(t.to_string());
        } else if let Some(d) = line.strip_prefix("data: ") {
            let v: Value = serde_json::from_str(d.trim()).unwrap_or(Value::Null);
            let ty = cur_type
                .clone()
                .or_else(|| v.get("type").and_then(|t| t.as_str()).map(String::from))
                .unwrap_or_default();
            out.push((ty, v));
            cur_type = None;
        }
    }
    out
}

// ---------------------------------------------------------------------------
// codex：responses → chat 转换流式
// ---------------------------------------------------------------------------

#[tokio::test]
async fn codex_responses_to_chat_stream_event_sequence() {
    let lines = vec![
        data_line(&json!({"id": "chat-1", "model": "m-main",
                          "choices": [{"delta": {"content": "Hi"}, "finish_reason": null}]})),
        data_line(&json!({"choices": [{"delta": {}, "finish_reason": "stop"}]})),
        // usage 尾块（choices 空）：completed 的 usage 必须取到它
        data_line(&json!({"choices": [], "usage": {"prompt_tokens": 10, "completion_tokens": 5}})),
        "data: [DONE]\n\n".to_string(),
    ];
    let mock = spawn_mock(200, lines, None, Duration::from_millis(5)).await;
    let mut svc = service(
        "svc-cx",
        ClientKind::Openai,
        Some(OpenaiApi::OpenaiResponses),
        format!("{}/v1/chat/completions", mock.addr),
        String::new(),
    );
    svc.upstream_api = UpstreamApi::OpenaiCompletions;
    let (port, _st) = spawn_service(svc).await;

    let resp = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{port}/v1/responses"))
        .json(&json!({"model": "x", "input": "hi", "stream": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let events = parse_sse(&resp.text().await.unwrap());
    let types: Vec<&str> = events.iter().map(|(t, _)| t.as_str()).collect();
    assert_eq!(
        types,
        vec![
            "response.created",
            "response.output_item.added",     // message item
            "response.content_part.added",
            "response.output_text.delta",     // "Hi"
            // finish_reason：done 事件（不发射 completed）
            "response.output_text.done",
            "response.content_part.done",
            "response.output_item.done",
            // [DONE]：response.completed（usage 尾块已到达）
            "response.completed",
        ]
    );
    let completed = events.iter().find(|(t, _)| t == "response.completed").unwrap();
    assert_eq!(completed.1["response"]["status"], "completed");
    assert_eq!(completed.1["response"]["usage"]["input_tokens"], 10);
    assert_eq!(completed.1["response"]["usage"]["output_tokens"], 5);
    // 上游请求体：model 覆盖 + input 转 messages
    let captured = mock.captured.lock().unwrap();
    let (_, body) = &captured[0];
    assert_eq!(body["model"], "m-main");
    assert_eq!(body["messages"][0]["role"], "user");
    assert_eq!(body["messages"][0]["content"], "hi");
    assert!(body["stream_options"]["include_usage"].as_bool().unwrap());
}

#[tokio::test]
async fn codex_chat_input_stream_passthrough_raw() {
    let lines = vec![
        "event: ping\n\n".to_string(), // 非 data 行原样透传
        data_line(&json!({"id": "c1", "choices": [{"delta": {"content": "hey"}, "finish_reason": null}]})),
        "data: [DONE]\n\n".to_string(),
    ];
    let mock = spawn_mock(200, lines, None, Duration::from_millis(5)).await;
    // legacy 路径：api 未声明 + client=openai → codex → Chat 入走 _responses_to_chat 直通分支
    let svc = service(
        "svc-legacy",
        ClientKind::Openai,
        None,
        format!("{}/v1/chat/completions", mock.addr),
        String::new(),
    );
    let (port, _st) = spawn_service(svc).await;

    let resp = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{port}/chat/completions"))
        .json(&json!({"model": "x", "messages": [{"role": "user", "content": "yo"}], "stream": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let text = resp.text().await.unwrap();
    // 原样透传（含非 data 行 event: ping）
    assert!(text.contains("event: ping"));
    assert!(text.contains("\"content\":\"hey\""));
    assert!(text.ends_with('\n'));
    // legacy Chat 入：max_tokens 缺省补默认（4096）+ model 覆盖
    let captured = mock.captured.lock().unwrap();
    let (_, body) = &captured[0];
    assert_eq!(body["model"], "m-main");
    assert_eq!(body["max_tokens"], 4096);
    assert_eq!(body["messages"][0]["content"], "yo");
}

#[tokio::test]
async fn passthrough_upstream_500_body_wrapped() {
    let mock = spawn_mock(500, vec![], Some(b"{\"e\":1}".to_vec()), Duration::from_millis(1)).await;
    let svc = service(
        "svc-pt",
        ClientKind::Openai,
        Some(OpenaiApi::OpenaiCompletions),
        format!("{}/v1/chat/completions", mock.addr),
        String::new(),
    );
    let (port, _st) = spawn_service(svc).await;

    let resp = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{port}/chat/completions"))
        .json(&json!({"model": "x", "messages": []}))
        .send()
        .await
        .unwrap();
    // 错误体包进 openai_error_response 外壳（status 保留）
    assert_eq!(resp.status().as_u16(), 500);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["type"], "api_error");
    assert_eq!(body["error"]["message"], "upstream error: {\"e\":1}");
}

#[tokio::test]
async fn codex_chat_passthrough_normalizes_developer_role() {
    let mock = spawn_mock(200, vec!["data: [DONE]\n\n".to_string()], Some(b"{}".to_vec()), Duration::from_millis(1)).await;
    let svc = service(
        "svc-nr",
        ClientKind::Openai,
        Some(OpenaiApi::OpenaiCompletions),
        format!("{}/v1/chat/completions", mock.addr),
        String::new(),
    );
    let (port, _st) = spawn_service(svc).await;

    let resp = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{port}/chat/completions"))
        .json(&json!({"model": "x", "messages": [
            {"role": "developer", "content": "sys"},
            {"role": "user", "content": "u"},
        ]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let captured = mock.captured.lock().unwrap();
    let (_, body) = &captured[0];
    // normalize_roles：developer → system（修改才重序列化）
    assert_eq!(body["messages"][0]["role"], "system");
    assert_eq!(body["messages"][1]["role"], "user");
    // 整包透传：不补 max_tokens（对齐 Python：passthrough 不重建、不补字段）
    assert!(body.get("max_tokens").is_none());
}

// ---------------------------------------------------------------------------
// direct：透传 + usage 抓取
// ---------------------------------------------------------------------------

#[tokio::test]
async fn direct_stream_passthrough_and_usage_capture() {
    let lines = vec![
        data_line(&json!({"type": "message_start", "message": {"id": "msg_1",
                          "usage": {"input_tokens": 100, "output_tokens": 0}}})),
        data_line(&json!({"type": "content_block_delta", "index": 0,
                          "delta": {"type": "text_delta", "text": "hello"}})),
        data_line(&json!({"type": "message_delta",
                          "delta": {"stop_reason": "end_turn"},
                          "usage": {"output_tokens": 5}})),
        data_line(&json!({"type": "message_stop"})),
    ];
    let mock = spawn_mock(200, lines, None, Duration::from_millis(5)).await;
    let sink = Arc::new(CapturingSink(Mutex::new(Vec::new())));
    let sink_c = sink.clone();
    // anthropic 端点账号 + api=anthropic-messages → direct
    let mut svc = service(
        "svc-dir",
        ClientKind::Anthropic,
        Some(OpenaiApi::AnthropicMessages),
        String::new(),
        format!("{}/v1/messages", mock.addr),
    );
    svc.override_model = false; // 验证请求体不被强改（max_tokens 缺省补 4096）
    let (port, st) = spawn_service_with_sink(svc, sink_c).await;

    let resp = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{port}/v1/messages"))
        .header("anthropic-beta", "prompt-caching-2024-07-31")
        .json(&json!({"model": "claude-x", "max_tokens": 64, "stream": true,
                      "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let text = resp.text().await.unwrap();
    // 事件原样透传（类型齐全、顺序一致）
    assert!(text.contains("\"type\":\"message_start\""));
    assert!(!text.contains("prompt-caching-2024-07-31")); // beta 头只进上游头，不进 body
    assert!(text.contains("\"stop_reason\":\"end_turn\"") || text.contains("\"stop_reason\": \"end_turn\""));
    assert!(text.contains("\"type\":\"message_stop\""));
    // 请求体：override=false 不改 model；max_tokens 已有则保留
    {
        let captured = mock.captured.lock().unwrap();
        let (_, body) = &captured[0];
        assert_eq!(body["model"], "claude-x");
        assert_eq!(body["max_tokens"], 64);
    }
    // usage 抓取：message_start(input=100) + message_delta(output=5) 合并
    tokio::time::sleep(Duration::from_millis(80)).await;
    let recs = sink.0.lock().unwrap();
    assert_eq!(recs.len(), 1, "direct 流式应记一次统计");
    let (model, _, usage, err) = &recs[0];
    assert_eq!(model, "m-main"); // direct 用 service.model 记账
    assert!(err.is_none());
    assert_eq!(usage["input_tokens"], 100);
    assert_eq!(usage["output_tokens"], 5);
    // 任务状态：end_turn → final
    let snap = st.task_snapshot();
    assert_eq!(snap.last_finish, "final");
    assert!(!snap.active);
}

// ---------------------------------------------------------------------------
// stats 接线：别名反查 + upstream_model
// ---------------------------------------------------------------------------

#[tokio::test]
async fn stats_sink_alias_and_upstream_model() {
    let lines = vec![
        data_line(&json!({"id": "chat-a", "model": "m-main",
                          "choices": [{"delta": {"content": "ok"}, "finish_reason": null}]})),
        data_line(&json!({"choices": [], "usage": {"prompt_tokens": 7, "completion_tokens": 3}})),
        "data: [DONE]\n\n".to_string(),
    ];
    let mock = spawn_mock(200, lines, None, Duration::from_millis(5)).await;
    // 别名：对外名 my-alias → 上游 m-main；统计应记 my-alias
    let mut svc = service(
        "svc-alias",
        ClientKind::Anthropic,
        None,
        format!("{}/v1/chat/completions", mock.addr),
        String::new(),
    );
    svc.models_map = ModelsMap(vec![("my-alias".to_string(), "m-main".to_string())]);
    let sink = Arc::new(CapturingSink(Mutex::new(Vec::new())));
    let sink_c = sink.clone();
    let (port, _st) = spawn_service_with_sink(svc, sink_c).await;

    let resp = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{port}/v1/messages"))
        .json(&json!({"model": "my-alias", "max_tokens": 10, "stream": true,
                      "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    tokio::time::sleep(Duration::from_millis(80)).await;

    let recs = sink.0.lock().unwrap();
    assert_eq!(recs.len(), 1);
    let (model, _, usage, _) = &recs[0];
    // sink 收到的是原始模型名（别名反查在 O2aStatsSink 内部做；NoopSink 透传）
    assert_eq!(model, "m-main");
    assert_eq!(usage["input_tokens"], 7);
    assert_eq!(usage["output_tokens"], 3);
}

// ---------------------------------------------------------------------------
// O2aStatsSink：别名反查 + upstream_model + batch（真实注册表 + 临时目录）
// ---------------------------------------------------------------------------

#[test]
fn o2a_stats_sink_writes_alias_and_upstream_model() {
    // CACHE_STATS_ENABLED 为进程级 env，m5 的 env 测试可能并发改写，串行化
    let _env = crate::m5_quota::tests::env_lock();
    let tmp = tempfile::tempdir().unwrap();
    let registry = Arc::new(o2a_stats::StatsRegistry::new(
        tmp.path().to_path_buf(),
        30,
        None,
    ));
    let sink = crate::stats_sink::O2aStatsSink { registry };
    use crate::proxy::StatsSink;

    let mut svc = service(
        "svc-o2a",
        ClientKind::Anthropic,
        None,
        String::new(),
        String::new(),
    );
    svc.account.id = "acc-o2a".into();
    svc.pricing_mode = PricingMode::Subscription; // no_cost 路径
    svc.pricing_extra = Some(serde_json::from_str(r#"{"batch": true}"#).unwrap());
    svc.models_map = ModelsMap(vec![("my-alias".to_string(), "m-main".to_string())]);

    sink.record(
        &svc,
        "m-main",
        &json!({"input_tokens": 7, "output_tokens": 3,
                "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0}),
        None,
        StatsMeta { duration_ms: Some(12.0), first_token_ms: Some(5.0), output_tokens_per_sec: None },
    );
    // usage/error 皆空 → 短路（不写文件）
    sink.record(&svc, "m-main", &json!({}), None, StatsMeta::default());

    // 找到 JSONL 文件并校验记录
    let mut found = false;
    for entry in std::fs::read_dir(tmp.path()).unwrap().flatten() {
        let p = entry.path();
        if p.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            let content = std::fs::read_to_string(&p).unwrap();
            let rec: Value = serde_json::from_str(content.lines().next().unwrap()).unwrap();
            assert_eq!(rec["model"], "my-alias", "model 记对外名（别名反查）");
            assert_eq!(rec["upstream_model"], "m-main", "upstream_model 记上游名");
            assert_eq!(rec["service_id"], "svc-o2a");
            assert_eq!(rec["duration_ms"], 12.0);
            assert_eq!(rec["first_token_ms"], 5.0);
            assert_eq!(rec["batch"], true);
            assert_eq!(rec["input_tokens"], 7);
            found = true;
        }
    }
    assert!(found, "应写入一条 JSONL 记录");
}
