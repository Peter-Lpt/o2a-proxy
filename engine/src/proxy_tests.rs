//! M3 claude 模式契约测试：mock 上游（可编程 SSE 序列）→ 端到端断言
//! 客户端收到的事件序列 / usage 尾块时序 / 错误响应 / 客户端断连取消上游。
//! 行为基准：Python engine.py handle_claude_stream / handle_claude_non_stream。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::http::StatusCode;
use axum::response::Response;
use serde_json::{json, Value};
use tokio::sync::mpsc;

use crate::handlers::build_router;
use crate::state::ServiceState;
use o2a_config::{ClientKind, ModelPolicy, ModelsMap};

// ---------------------------------------------------------------------------
// 夹具：mock 上游 + 测试服务
// ---------------------------------------------------------------------------

struct MockUpstream {
    base: String, // 以 /v1/chat/completions 结尾的完整 openai_url
    cancelled: Arc<AtomicBool>,
    captured: Arc<Mutex<Vec<(String, Value)>>>, // (uri, body)
}

/// 起 mock 上游：POST /v1/chat/completions。
/// status=200 时按 `lines`（完整 SSE 行，如 "data: {...}\n\n"）逐行下发，
/// 每行间隔 `delay`；客户端断连（body 流被 drop）时置位 cancelled。
async fn spawn_mock(status: u16, lines: Vec<String>, delay: Duration) -> MockUpstream {
    let cancelled = Arc::new(AtomicBool::new(false));
    let captured: Arc<Mutex<Vec<(String, Value)>>> = Arc::new(Mutex::new(Vec::new()));
    let cancelled_h = cancelled.clone();
    let captured_h = captured.clone();

    let app = axum::Router::new()
        .route(
            "/v1/chat/completions",
            axum::routing::post(
                move |req: axum::extract::Request| async move {
                    let uri = req.uri().to_string();
                    let body = axum::body::to_bytes(req.into_body(), 1 << 20)
                        .await
                        .unwrap_or_default();
                    let payload: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                    captured_h.lock().unwrap().push((uri, payload));

                    if status != 200 {
                        return Response::builder()
                            .status(StatusCode::from_u16(status).unwrap())
                            .body(axum::body::Body::from("boom"))
                            .unwrap();
                    }
                    let (tx, rx) = mpsc::channel::<Result<Vec<u8>, std::io::Error>>(4);
                    let cancelled_t = cancelled_h.clone();
                    tokio::spawn(async move {
                        for line in lines {
                            if tx.send(Ok(line.into_bytes())).await.is_err() {
                                // 接收端被 drop：客户端断连，上游被取消
                                cancelled_t.store(true, Ordering::SeqCst);
                                return;
                            }
                            tokio::time::sleep(delay).await;
                        }
                        // 正常结束不置位 cancelled
                    });
                    Response::builder()
                        .status(StatusCode::OK)
                        .header("content-type", "text/event-stream")
                        .body(axum::body::Body::from_stream(
                            tokio_stream::wrappers::ReceiverStream::new(rx),
                        ))
                        .unwrap()
                },
            ),
        )
        .with_state(());

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    MockUpstream {
        base: format!("http://{addr}/v1/chat/completions"),
        cancelled,
        captured,
    }
}

fn claude_service(id: &str, openai_url: String) -> o2a_config::Service {
    o2a_config::Service {
        id: id.to_string(),
        name: id.to_string(),
        account: o2a_config::Account {
            id: "acc-1".to_string(),
            name: "账号A".to_string(),
            api_key: "sk-test".to_string(),
            openai_url,
            anthropic_url: String::new(),
            api: String::new(),
            quota_source: "auto".to_string(),
            quota: None,
        },
        client: ClientKind::Anthropic, // anthropic 客户端 + openai 端点 → claude 模式
        host: "127.0.0.1".to_string(),
        port: 0,
        model: "m-main".to_string(),
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

/// 起被测引擎服务（随机端口）。
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

/// POST /v1/messages（Anthropic Messages 载荷）。
async fn post_messages(
    port: u16,
    query: &str,
    payload: &Value,
) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("http://127.0.0.1:{port}/v1/messages{query}"))
        .json(payload)
        .send()
        .await
        .unwrap()
}

fn anthropic_request(stream: bool) -> Value {
    json!({
        "model": "client-model",
        "max_tokens": 100,
        "stream": stream,
        "messages": [{"role": "user", "content": "hi"}],
    })
}

/// 解析 SSE 文本为 (event_type, json payload) 列表（event 行缺省时取 data.type）。
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
// 流式：事件序列
// ---------------------------------------------------------------------------

#[tokio::test]
async fn claude_stream_event_sequence_and_usage_tail() {
    let lines = vec![
        data_line(&json!({"id": "chat-1", "model": "up-model",
                          "choices": [{"delta": {"content": "Hello"}, "finish_reason": null}]})),
        data_line(&json!({"choices": [{"delta": {"content": " world"}, "finish_reason": null}]})),
        // finish_reason：只关块，不发 message_delta
        data_line(&json!({"choices": [{"delta": {}, "finish_reason": "stop"}]})),
        // usage 尾块（choices 空）：此刻才发 message_delta + message_stop
        data_line(&json!({"choices": [], "usage": {"prompt_tokens": 10, "completion_tokens": 5,
                                                    "prompt_tokens_details": {"cached_tokens": 4}}})),
        "data: [DONE]\n\n".to_string(),
    ];
    let mock = spawn_mock(200, lines, Duration::from_millis(5)).await;
    let svc = claude_service("svc-e", mock.base.clone());
    let (port, _st) = spawn_service(svc).await;

    let resp = post_messages(port, "", &anthropic_request(true)).await;
    assert_eq!(resp.status().as_u16(), 200);
    assert!(resp
        .headers()["content-type"]
        .to_str()
        .unwrap()
        .starts_with("text/event-stream"));
    let text = resp.text().await.unwrap();
    let events = parse_sse(&text);

    let types: Vec<&str> = events.iter().map(|(t, _)| t.as_str()).collect();
    assert_eq!(
        types,
        vec![
            "message_start",
            "content_block_start",
            "content_block_delta",
            "content_block_delta",
            "content_block_stop",
            "message_delta",
            "message_stop",
        ]
    );
    // message_start：id/model 取首 chunk
    assert_eq!(events[0].1["message"]["id"], "chat-1");
    assert_eq!(events[0].1["message"]["model"], "up-model");
    assert_eq!(events[0].1["message"]["usage"]["input_tokens"], 0);
    // 文本增量
    assert_eq!(events[2].1["delta"]["text"], "Hello");
    assert_eq!(events[3].1["delta"]["text"], " world");
    // message_delta 在 usage 尾块后：stop_reason end_turn + 转换后 usage
    // （prompt 10 = input 6 + cached 4）
    assert_eq!(events[5].1["delta"]["stop_reason"], "end_turn");
    assert_eq!(events[5].1["usage"]["input_tokens"], 6);
    assert_eq!(events[5].1["usage"]["cache_read_input_tokens"], 4);
    assert_eq!(events[5].1["usage"]["output_tokens"], 5);

    // 模型覆盖：上游收到服务配置模型（override_model=true）
    let captured = mock.captured.lock().unwrap();
    assert_eq!(captured[0].1["model"], "m-main");
}

#[tokio::test]
async fn claude_stream_query_string_forwarded() {
    let lines = vec![
        data_line(&json!({"choices": [{"delta": {"content": "x"}, "finish_reason": null}]})),
        data_line(&json!({"choices": [{"delta": {}, "finish_reason": "stop"}]})),
        "data: [DONE]\n\n".to_string(),
    ];
    let mock = spawn_mock(200, lines, Duration::from_millis(1)).await;
    let svc = claude_service("svc-q", mock.base.clone());
    let (port, _st) = spawn_service(svc).await;
    let resp = post_messages(port, "?beta=true", &anthropic_request(true)).await;
    assert_eq!(resp.status().as_u16(), 200);
    let _ = resp.text().await.unwrap();
    let captured = mock.captured.lock().unwrap();
    assert!(captured[0].0.contains("beta=true"), "uri={}", captured[0].0);
}

#[tokio::test]
async fn claude_stream_thinking_and_tool_calls() {
    let lines = vec![
        data_line(&json!({"choices": [{"delta": {"reasoning_content": "thinking..."}, "finish_reason": null}]})),
        data_line(&json!({"choices": [{"delta": {"content": "ans"}, "finish_reason": null}]})),
        data_line(&json!({"choices": [{"delta": {"tool_calls": [
            {"index": 0, "id": "call_1", "function": {"name": "f", "arguments": "{\"a\":"}}
        ]}, "finish_reason": null}]})),
        // 续块（无 id）：追加参数
        data_line(&json!({"choices": [{"delta": {"tool_calls": [
            {"index": 0, "function": {"arguments": "1}"}}
        ]}, "finish_reason": null}]})),
        data_line(&json!({"choices": [{"delta": {}, "finish_reason": "tool_calls"}]})),
        data_line(&json!({"choices": [], "usage": {"prompt_tokens": 3, "completion_tokens": 7}})),
        "data: [DONE]\n\n".to_string(),
    ];
    let mock = spawn_mock(200, lines, Duration::from_millis(2)).await;
    let svc = claude_service("svc-t", mock.base.clone());
    let (port, _st) = spawn_service(svc).await;

    let resp = post_messages(port, "", &anthropic_request(true)).await;
    let text = resp.text().await.unwrap();
    let events = parse_sse(&text);
    let types: Vec<&str> = events.iter().map(|(t, _)| t.as_str()).collect();
    assert_eq!(
        types,
        vec![
            "message_start",
            "content_block_start",        // thinking (idx 0)
            "content_block_delta",
            "content_block_stop",         // thinking 关闭（content 到来）
            "content_block_start",        // text (idx 1)
            "content_block_delta",
            "content_block_stop",         // text 关闭（tool_use 到来）
            "content_block_start",        // tool_use (idx 2)
            "content_block_delta",        // '{"a":'
            "content_block_delta",        // '1}'
            "content_block_stop",         // finish_reason 关块
            "message_delta",
            "message_stop",
        ]
    );
    assert_eq!(events[1].1["content_block"]["type"], "thinking");
    assert_eq!(events[1].1["index"], 0);
    assert_eq!(events[4].1["index"], 1);
    assert_eq!(events[7].1["content_block"]["type"], "tool_use");
    assert_eq!(events[7].1["content_block"]["id"], "call_1");
    assert_eq!(events[8].1["delta"]["partial_json"], "{\"a\":");
    assert_eq!(events[9].1["delta"]["partial_json"], "1}");
    assert_eq!(events[11].1["delta"]["stop_reason"], "tool_use");
    assert_eq!(events[11].1["usage"]["output_tokens"], 7);
}

#[tokio::test]
async fn claude_stream_orphan_args_buffered_until_id() {
    // 无 id 首块：只缓冲参数，不产生任何事件
    let lines = vec![
        data_line(&json!({"choices": [{"delta": {"tool_calls": [
            {"index": 0, "function": {"arguments": "{\"x\""}}
        ]}, "finish_reason": null}]})),
        // id 块到达：开块 + 缓冲参数合并为首条 input_json_delta
        data_line(&json!({"choices": [{"delta": {"tool_calls": [
            {"index": 0, "id": "call_9", "function": {"name": "g", "arguments": ":1}"}}
        ]}, "finish_reason": null}]})),
        data_line(&json!({"choices": [{"delta": {}, "finish_reason": "tool_calls"}]})),
        "data: [DONE]\n\n".to_string(),
    ];
    let mock = spawn_mock(200, lines, Duration::from_millis(5)).await;
    let svc = claude_service("svc-o", mock.base.clone());
    let (port, _st) = spawn_service(svc).await;

    let resp = post_messages(port, "", &anthropic_request(true)).await;
    let text = resp.text().await.unwrap();
    let events = parse_sse(&text);
    let types: Vec<&str> = events.iter().map(|(t, _)| t.as_str()).collect();
    // 第一个 chunk（无 id）不产生事件：首事件就是 message_start
    assert_eq!(types[0], "message_start");
    assert_eq!(types[1], "content_block_start");
    assert_eq!(types[1], "content_block_start");
    assert_eq!(events[2].1["delta"]["partial_json"], "{\"x\":1}");
    assert_eq!(events[1].1["content_block"]["id"], "call_9");
}

#[tokio::test]
async fn claude_stream_unknown_finish_reason_passthrough() {
    let lines = vec![
        data_line(&json!({"choices": [{"delta": {"content": "z"}, "finish_reason": null}]})),
        data_line(&json!({"choices": [{"delta": {}, "finish_reason": "weird_reason"}]})),
        "data: [DONE]\n\n".to_string(),
    ];
    let mock = spawn_mock(200, lines, Duration::from_millis(2)).await;
    let svc = claude_service("svc-u", mock.base.clone());
    let (port, _st) = spawn_service(svc).await;
    let resp = post_messages(port, "", &anthropic_request(true)).await;
    let events = parse_sse(&resp.text().await.unwrap());
    let delta = events.iter().find(|(t, _)| t == "message_delta").unwrap();
    // 未知 finish_reason 原样透传（对齐 _anthropic_stop_reason 的兜底分支）
    assert_eq!(delta.1["delta"]["stop_reason"], "weird_reason");
}

#[tokio::test]
async fn claude_stream_eof_without_done_still_terminates() {
    // 上游不发 [DONE] 直接断开：客户端仍能收到 message_delta + message_stop
    let lines = vec![
        data_line(&json!({"choices": [{"delta": {"content": "partial"}, "finish_reason": null}]})),
        data_line(&json!({"choices": [{"delta": {}, "finish_reason": "stop"}]})),
    ];
    let mock = spawn_mock(200, lines, Duration::from_millis(2)).await;
    let svc = claude_service("svc-ee", mock.base.clone());
    let (port, _st) = spawn_service(svc).await;
    let resp = post_messages(port, "", &anthropic_request(true)).await;
    let events = parse_sse(&resp.text().await.unwrap());
    let types: Vec<&str> = events.iter().map(|(t, _)| t.as_str()).collect();
    assert_eq!(types.last().unwrap(), &"message_stop");
    assert!(types.contains(&"message_delta"));
}

// ---------------------------------------------------------------------------
// 流式：错误与取消
// ---------------------------------------------------------------------------

#[tokio::test]
async fn claude_stream_upstream_500_error_response() {
    let mock = spawn_mock(500, vec![], Duration::ZERO).await;
    let svc = claude_service("svc-5", mock.base.clone());
    let (port, _st) = spawn_service(svc).await;
    let resp = post_messages(port, "", &anthropic_request(true)).await;
    assert_eq!(resp.status().as_u16(), 500);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["type"], "error");
    assert_eq!(body["error"]["type"], "api_error");
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .starts_with("upstream error: boom"));
}

#[tokio::test]
async fn claude_stream_connect_refused_502() {
    // 指向未监听端口 → 连接失败 → 502（响应头未发出）
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let dead_port = listener.local_addr().unwrap().port();
    drop(listener);
    let svc = claude_service("svc-dead", format!("http://127.0.0.1:{dead_port}/v1/chat/completions"));
    let (port, _st) = spawn_service(svc).await;
    let resp = post_messages(port, "", &anthropic_request(true)).await;
    assert_eq!(resp.status().as_u16(), 502);
    let body: Value = resp.json().await.unwrap();
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .starts_with("upstream error:"));
}

#[tokio::test]
async fn claude_stream_client_disconnect_cancels_upstream() {
    // 50 行 × 100ms：客户端读首事件后断连，上游应被取消
    let lines: Vec<String> = (0..50)
        .map(|i| {
            data_line(&json!({"choices": [{"delta": {"content": format!("c{i}")}, "finish_reason": null}]}))
        })
        .collect();
    let mock = spawn_mock(200, lines, Duration::from_millis(100)).await;
    let svc = claude_service("svc-dc", mock.base.clone());
    let (port, _st) = spawn_service(svc).await;

    let resp = post_messages(port, "", &anthropic_request(true)).await;
    assert_eq!(resp.status().as_u16(), 200);
    // 读到首块即断连
    let mut resp = resp;
    let _ = resp.chunk().await.unwrap();
    drop(resp);

    // 上游在数秒内观察到取消（远小于 50×100ms=5s 的自然结束时间）
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !mock.cancelled.load(Ordering::SeqCst) {
        assert!(std::time::Instant::now() < deadline, "upstream was not cancelled");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

// ---------------------------------------------------------------------------
// 非流式
// ---------------------------------------------------------------------------

#[tokio::test]
async fn claude_non_stream_conversion() {
    let upstream_body = json!({
        "id": "chat-9",
        "model": "up-m",
        "choices": [{
            "message": {
                "role": "assistant",
                "content": "hi",
                "reasoning_content": "deep thought",
                "tool_calls": [{"id": "t1", "function": {"name": "f", "arguments": "{\"k\":1}"}}],
            },
            "finish_reason": "tool_calls",
        }],
        "usage": {"prompt_tokens": 100, "prompt_cache_hit_tokens": 40, "completion_tokens": 20},
    });
    let mock = spawn_mock_raw(200, serde_json::to_vec(&upstream_body).unwrap()).await;
    let svc = claude_service("svc-ns", mock.base.clone());
    let (port, _st) = spawn_service(svc).await;

    let resp = post_messages(port, "", &anthropic_request(false)).await;
    assert_eq!(resp.status().as_u16(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["id"], "chat-9");
    assert_eq!(body["type"], "message");
    assert_eq!(body["role"], "assistant");
    assert_eq!(body["model"], "up-m");
    // content_list：thinking → text → tool_use
    let content = body["content"].as_array().unwrap();
    assert_eq!(content.len(), 3);
    assert_eq!(content[0]["type"], "thinking");
    assert_eq!(content[0]["thinking"], "deep thought");
    assert_eq!(content[1]["type"], "text");
    assert_eq!(content[1]["text"], "hi");
    assert_eq!(content[2]["type"], "tool_use");
    assert_eq!(content[2]["id"], "t1");
    assert_eq!(content[2]["input"], json!({"k": 1}));
    // stop_reason：tool_calls → tool_use
    assert_eq!(body["stop_reason"], "tool_use");
    // usage：DeepSeek 顶层缓存字段语义（prompt 100 = input 60 + cache_read 40）
    assert_eq!(body["usage"]["input_tokens"], 60);
    assert_eq!(body["usage"]["cache_read_input_tokens"], 40);
    assert_eq!(body["usage"]["output_tokens"], 20);
    assert!(body["usage"].get("reasoning_tokens").is_none()); // 响应体无 reasoning 键（对齐 Python）
}

#[tokio::test]
async fn claude_non_stream_bad_arguments_kept_raw() {
    let upstream_body = json!({
        "id": "chat-b",
        "model": "up-m",
        "choices": [{
            "message": {"role": "assistant", "content": "",
                        "tool_calls": [{"id": "t2", "function": {"name": "g", "arguments": "not-json"}}]},
            "finish_reason": "tool_calls",
        }],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1},
    });
    let mock = spawn_mock_raw(200, serde_json::to_vec(&upstream_body).unwrap()).await;
    let svc = claude_service("svc-ba", mock.base.clone());
    let (port, _st) = spawn_service(svc).await;
    let resp = post_messages(port, "", &anthropic_request(false)).await;
    let body: Value = resp.json().await.unwrap();
    // arguments 解析失败 → 保留原始字符串（对齐 Python except 分支）
    assert_eq!(body["content"][0]["input"], json!("not-json"));
}

#[tokio::test]
async fn claude_non_stream_upstream_error() {
    let mock = spawn_mock_raw(429, b"rate limited".to_vec()).await;
    let svc = claude_service("svc-n4", mock.base.clone());
    let (port, _st) = spawn_service(svc).await;
    let resp = post_messages(port, "", &anthropic_request(false)).await;
    assert_eq!(resp.status().as_u16(), 429);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["type"], "error");
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .starts_with("upstream error: rate limited"));
}

// ---------------------------------------------------------------------------
// 模型白名单 / 别名 / 策略
// ---------------------------------------------------------------------------

#[tokio::test]
async fn model_policy_alias_rewrite_and_clamp_and_reject() {
    let lines = vec![
        data_line(&json!({"choices": [{"delta": {"content": "ok"}, "finish_reason": null}]})),
        data_line(&json!({"choices": [{"delta": {}, "finish_reason": "stop"}]})),
        "data: [DONE]\n\n".to_string(),
    ];

    // 别名命中：对外名 alias-x → 上游名 m-c。
    // 注意：别名改写发生在 payload 上，convert_request 在 override_model=true 时
    // 会强转主模型，因此别名仅在 override_model=false 时生效（Python 同序）
    let mock = spawn_mock(200, lines.clone(), Duration::from_millis(1)).await;
    let mut svc = claude_service("svc-al", mock.base.clone());
    svc.override_model = false;
    svc.models = vec!["alias-x".into()];
    svc.models_map = ModelsMap(vec![("alias-x".into(), "m-c".into())]);
    let (port, _st) = spawn_service(svc).await;
    let mut req = anthropic_request(true);
    req["model"] = json!("alias-x");
    let resp = post_messages(port, "", &req).await;
    assert_eq!(resp.status().as_u16(), 200);
    let _ = resp.text().await.unwrap();
    assert_eq!(mock.captured.lock().unwrap()[0].1["model"], "m-c");

    // clamp（默认策略）：白名单外模型强转主模型
    let mock = spawn_mock(200, lines.clone(), Duration::from_millis(1)).await;
    let mut svc = claude_service("svc-cl", mock.base.clone());
    svc.models = vec!["m-a".into()];
    let (port, _st) = spawn_service(svc).await;
    let mut req = anthropic_request(true);
    req["model"] = json!("unknown-model");
    let resp = post_messages(port, "", &req).await;
    assert_eq!(resp.status().as_u16(), 200);
    let _ = resp.text().await.unwrap();
    assert_eq!(mock.captured.lock().unwrap()[0].1["model"], "m-main");

    // reject：白名单外 → 400 + 可用模型列表（排序）
    let mock = spawn_mock(200, lines, Duration::from_millis(1)).await;
    let mut svc = claude_service("svc-rj", mock.base.clone());
    svc.models = vec!["m-b".into(), "m-a".into()];
    svc.model_policy = ModelPolicy::Reject;
    let (port, _st) = spawn_service(svc).await;
    let mut req = anthropic_request(true);
    req["model"] = json!("nope");
    let resp = post_messages(port, "", &req).await;
    assert_eq!(resp.status().as_u16(), 400);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["type"], "invalid_request_error");
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("available models: m-a, m-b"));
}

// ---------------------------------------------------------------------------
// JSON 解析失败的风格区分
// ---------------------------------------------------------------------------

#[tokio::test]
async fn invalid_json_error_style_by_mode() {
    // claude 模式（client=anthropic）→ Anthropic 风格 400
    let mock = spawn_mock(200, vec![], Duration::ZERO).await;
    let svc = claude_service("svc-ij", mock.base.clone());
    let (port, _st) = spawn_service(svc).await;
    let resp = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{port}/v1/messages"))
        .body(b"not-json".to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 400);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["type"], "error");
    assert_eq!(body["error"]["message"], "invalid json");

    // codex 模式（client=openai）→ OpenAI 风格 400
    let mut svc = claude_service("svc-ij2", mock.base.clone());
    svc.client = ClientKind::Openai;
    let (port, _st) = spawn_service(svc).await;
    let resp = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
        .body(b"not-json".to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 400);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["message"], "invalid json");
}

// ---------------------------------------------------------------------------
// 非 SSE 的 mock（非流式用：返回完整 JSON）
// ---------------------------------------------------------------------------

async fn spawn_mock_raw(status: u16, body: Vec<u8>) -> MockUpstream {
    let cancelled = Arc::new(AtomicBool::new(false));
    let captured: Arc<Mutex<Vec<(String, Value)>>> = Arc::new(Mutex::new(Vec::new()));
    let captured_h = captured.clone();

    let app = axum::Router::new().route(
        "/v1/chat/completions",
        axum::routing::post(move |req: axum::extract::Request| async move {
            let uri = req.uri().to_string();
            let b = axum::body::to_bytes(req.into_body(), 1 << 20)
                .await
                .unwrap_or_default();
            captured_h.lock().unwrap().push((uri, serde_json::from_slice(&b).unwrap_or(Value::Null)));
            Response::builder()
                .status(StatusCode::from_u16(status).unwrap())
                .header("content-type", "application/json")
                .body(axum::body::Body::from(body))
                .unwrap()
        }),
    );
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    MockUpstream {
        base: format!("http://{addr}/v1/chat/completions"),
        cancelled,
        captured,
    }
}
