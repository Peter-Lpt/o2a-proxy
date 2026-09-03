//! convert_request / responses_to_chat 请求方向转换测试
//!（对齐 convert.py 语义；`detect_client` / `normalize_roles` / `sse_event` 单元边界）。

use o2a_config::{ClientKind, Service, ThinkingMode};
use o2a_convert::{
    convert_request, detect_client, normalize_roles, responses_to_chat, sse_event,
    strip_cache_control,
};
use serde_json::json;

mod common;
use common::make_service;

fn claude_service() -> Service {
    make_service("https://api.example.com/v1", "qwen-plus", ThinkingMode::None)
}

// ---------- system 提取 ----------

#[test]
fn system_string_and_blocks() {
    let svc = claude_service();
    let req = json!({
        "system": "be nice",
        "messages": [{"role": "user", "content": "hi"}],
    });
    let out = convert_request(&req, &svc);
    assert_eq!(
        out["messages"][0],
        json!({"role": "system", "content": "be nice"})
    );

    let req = json!({
        "system": [{"type": "text", "text": "a"}, "b"],
        "messages": [{"role": "user", "content": "hi"}],
    });
    let out = convert_request(&req, &svc);
    assert_eq!(out["messages"][0]["content"], json!("a\nb"));

    // 空串 system：falsy 不插入
    let req = json!({
        "system": "",
        "messages": [{"role": "user", "content": "hi"}],
    });
    let out = convert_request(&req, &svc);
    assert_eq!(out["messages"][0]["role"], json!("user"));
}

// ---------- tool_result 交错冲刷 ----------

#[test]
fn tool_result_interleaved_text_flush() {
    let svc = claude_service();
    let req = json!({
        "max_tokens": 100,
        "messages": [{
            "role": "user",
            "content": [
                {"type": "text", "text": "before"},
                {"type": "tool_result", "tool_use_id": "t1", "content": "res1"},
                {"type": "text", "text": "mid"},
                {"type": "tool_result", "tool_use_id": "t2", "content": [{"type": "text", "text": "res2"}]},
                {"type": "text", "text": "after"},
            ],
        }],
    });
    let out = convert_request(&req, &svc);
    let msgs = out["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 5);
    assert_eq!(msgs[0], json!({"role": "user", "content": "before"}));
    assert_eq!(msgs[1], json!({"role": "tool", "tool_call_id": "t1", "content": "res1"}));
    assert_eq!(msgs[2], json!({"role": "user", "content": "mid"}));
    assert_eq!(msgs[3], json!({"role": "tool", "tool_call_id": "t2", "content": "res2"}));
    assert_eq!(msgs[4], json!({"role": "user", "content": "after"}));
}

#[test]
fn tool_result_missing_id_skipped_with_id_fallback() {
    let svc = claude_service();
    // tool_use_id 缺失 → 回退 block.id；两者皆无 → 跳过
    let req = json!({
        "max_tokens": 100,
        "messages": [{
            "role": "user",
            "content": [
                {"type": "tool_result", "id": "fb1", "content": "ok"},
                {"type": "tool_result", "content": "bad"},
            ],
        }],
    });
    let out = convert_request(&req, &svc);
    let msgs = out["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0]["tool_call_id"], json!("fb1"));
}

// ---------- assistant tool_use 合并 ----------

#[test]
fn assistant_tool_use_merge() {
    let svc = claude_service();
    let req = json!({
        "max_tokens": 100,
        "messages": [{
            "role": "assistant",
            "content": [
                {"type": "text", "text": "let me check"},
                {"type": "tool_use", "id": "t1", "name": "get_weather", "input": {"city": "BJ"}},
            ],
        }],
    });
    let out = convert_request(&req, &svc);
    let m = &out["messages"][0];
    assert_eq!(m["role"], json!("assistant"));
    assert_eq!(m["content"], json!("let me check"));
    assert_eq!(m["tool_calls"][0]["id"], json!("t1"));
    assert_eq!(m["tool_calls"][0]["function"]["name"], json!("get_weather"));
    assert_eq!(m["tool_calls"][0]["function"]["arguments"], json!(r#"{"city":"BJ"}"#));
}

#[test]
fn assistant_tool_use_content_null_when_no_text() {
    let svc = claude_service();
    let req = json!({
        "max_tokens": 100,
        "messages": [{
            "role": "assistant",
            "content": [{"type": "tool_use", "id": "t1", "name": "f", "input": {}}],
        }],
    });
    let out = convert_request(&req, &svc);
    assert_eq!(out["messages"][0]["content"], json!(null));
    assert_eq!(out["messages"][0]["tool_calls"][0]["function"]["arguments"], json!("{}"));
}

// ---------- 空 content 跳过 / 纯 thinking 消息 ----------

#[test]
fn empty_content_skipped() {
    let svc = claude_service();
    let req = json!({
        "max_tokens": 100,
        "messages": [
            {"role": "assistant", "content": [{"type": "thinking", "thinking": "hmm"}]},
            {"role": "user", "content": "hi"},
        ],
    });
    let out = convert_request(&req, &svc);
    let msgs = out["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0]["role"], json!("user"));
}

// ---------- 模型覆盖 / stream_options / 采样参数 ----------

#[test]
fn model_override_and_passthrough() {
    let svc = claude_service(); // override_model = true
    let req = json!({"model": "client-model", "max_tokens": 1, "messages": [{"role": "user", "content": "x"}]});
    assert_eq!(convert_request(&req, &svc)["model"], json!("qwen-plus"));

    let mut svc = claude_service();
    svc.override_model = false;
    assert_eq!(convert_request(&req, &svc)["model"], json!("client-model"));
    // 客户端缺模型 → 回退服务配置
    let req2 = json!({"max_tokens": 1, "messages": [{"role": "user", "content": "x"}]});
    assert_eq!(convert_request(&req2, &svc)["model"], json!("qwen-plus"));
}

#[test]
fn stream_options_and_sampling() {
    let svc = claude_service();
    let req = json!({
        "max_tokens": 1, "stream": true, "temperature": 0.5, "top_p": 0.9,
        "messages": [{"role": "user", "content": "x"}],
    });
    let out = convert_request(&req, &svc);
    assert_eq!(out["stream_options"], json!({"include_usage": true}));
    assert_eq!(out["temperature"], json!(0.5));
    assert_eq!(out["top_p"], json!(0.9));

    let req = json!({"max_tokens": 1, "messages": [{"role": "user", "content": "x"}]});
    let out = convert_request(&req, &svc);
    assert!(out.get("stream_options").is_none());
    assert!(out.get("temperature").is_none());
}

#[test]
fn max_tokens_defaults_to_service() {
    let svc = claude_service();
    let req = json!({"messages": [{"role": "user", "content": "x"}]});
    let out = convert_request(&req, &svc);
    assert_eq!(out["max_tokens"], json!(4096));
}

// ---------- tools / tool_choice ----------

#[test]
fn tools_and_tool_choice_conversion() {
    let svc = claude_service();
    let req = json!({
        "max_tokens": 1,
        "messages": [{"role": "user", "content": "x"}],
        "tools": [
            {"name": "f1", "description": "d1", "input_schema": {"properties": {}}},
            {"name": "f2", "description": "d2", "input_schema": {"type": "string"}},
        ],
        "tool_choice": {"type": "tool", "name": "f1"},
    });
    let out = convert_request(&req, &svc);
    assert_eq!(out["tools"][0]["type"], json!("function"));
    assert_eq!(out["tools"][0]["function"]["parameters"], json!({"type": "object", "properties": {}}));
    assert_eq!(out["tools"][1]["function"]["parameters"], json!({"type": "string"}));
    assert_eq!(
        out["tool_choice"],
        json!({"type": "function", "function": {"name": "f1"}})
    );
}

#[test]
fn tool_choice_any_single_and_multi() {
    let svc = claude_service();
    let base = json!({"max_tokens": 1, "messages": [{"role": "user", "content": "x"}]});

    let mut req = base.clone();
    req["tools"] = json!([{"name": "only"}]);
    req["tool_choice"] = json!("any");
    let out = convert_request(&req, &svc);
    assert_eq!(out["tool_choice"], json!({"type": "function", "function": {"name": "only"}}));

    let mut req = base.clone();
    req["tools"] = json!([{"name": "a"}, {"name": "b"}]);
    req["tool_choice"] = json!({"type": "any"});
    let out = convert_request(&req, &svc);
    assert_eq!(out["tool_choice"], json!("required"));

    // auto / none 原样
    let mut req = base.clone();
    req["tool_choice"] = json!("auto");
    assert_eq!(convert_request(&req, &svc)["tool_choice"], json!("auto"));
}

#[test]
fn cache_control_stripped_recursively() {
    let svc = claude_service();
    let req = json!({
        "max_tokens": 1,
        "messages": [{"role": "user", "content": [{"type": "text", "text": "hi", "cache_control": {"type": "ephemeral"}}]}],
    });
    let out = convert_request(&req, &svc);
    let out = strip_cache_control(&out);
    let s = serde_json::to_string(&out).unwrap();
    assert!(!s.contains("cache_control"));
}

// ---------- normalize_roles / detect_client / sse_event ----------

#[test]
fn normalize_roles_unit() {
    let mut payload = json!({"model": "m", "messages": [
        {"role": "developer", "content": "a"},
        {"role": "user", "content": "b"},
        {"role": "assistant", "content": "c"},
    ]});
    assert!(normalize_roles(&mut payload));
    assert_eq!(payload["messages"][0]["role"], json!("system"));

    // responses input 字符串形态：不改
    let mut p2 = json!({"model": "m", "input": "hello"});
    assert!(!normalize_roles(&mut p2));
    assert_eq!(p2["input"], json!("hello"));

    // 无 developer：零修改
    let mut p3 = json!({"model": "m", "messages": [{"role": "user", "content": "x"}]});
    assert!(!normalize_roles(&mut p3));

    // input 非列表（None）
    let mut p4 = json!({"model": "m", "input": null});
    assert!(!normalize_roles(&mut p4));
}

#[test]
fn detect_client_paths_and_payloads() {
    assert_eq!(detect_client("/v1/messages", None), "anthropic");
    assert_eq!(detect_client("/v1/responses", None), "openai");
    assert_eq!(detect_client("/chat/completions", None), "openai");
    assert_eq!(
        detect_client("/", Some(&json!({"input": "x"}))),
        "openai"
    );
    assert_eq!(
        detect_client("/", Some(&json!({"max_tokens": 1, "system": "s"}))),
        "anthropic"
    );
    assert_eq!(
        detect_client(
            "/",
            Some(&json!({"messages": [{"role": "user", "content": [{"type": "text"}]}]}))
        ),
        "anthropic"
    );
    assert_eq!(
        detect_client("/", Some(&json!({"messages": [{"role": "user", "content": "hi"}]}))),
        "openai"
    );
    assert_eq!(detect_client("/", None), "openai");
}

#[test]
fn sse_event_format_with_ascii_escaping() {
    let ev = json!({"type": "content_block_delta", "text": "你好"});
    let s = sse_event(&ev);
    assert!(s.starts_with("event: content_block_delta\n"));
    assert!(s.contains("\"text\": \"\\u4f60\\u597d\"")); // ensure_ascii 等价
    assert!(s.ends_with("\n\n"));

    // 无 type 键 → 无 event 行
    let s = sse_event(&json!({"foo": 1}));
    assert!(!s.starts_with("event:"));
    assert!(s.starts_with("data: "));
}

// ---------- responses_to_chat 补充 ----------

#[test]
fn responses_input_string_and_instructions_merge() {
    let svc = make_service("https://api.example.com/v1", "gpt-x", ThinkingMode::None);
    let req = json!({"input": "hello", "instructions": "sys prompt", "max_output_tokens": 77});
    let out = responses_to_chat(&req, &svc);
    assert_eq!(out["model"], json!("gpt-x"));
    assert_eq!(out["max_tokens"], json!(77)); // max_output_tokens 重命名
    assert_eq!(
        out["messages"],
        json!([{"role": "system", "content": "sys prompt"}, {"role": "user", "content": "hello"}])
    );
}

#[test]
fn responses_instructions_merges_into_existing_system() {
    let svc = make_service("https://api.example.com/v1", "gpt-x", ThinkingMode::None);
    let req = json!({
        "input": [
            {"role": "system", "content": "existing"},
            {"role": "user", "content": "hi"},
        ],
        "instructions": "instr",
    });
    let out = responses_to_chat(&req, &svc);
    assert_eq!(out["messages"][0]["content"], json!("instr\n\nexisting"));
    assert_eq!(out["messages"].as_array().unwrap().len(), 2);
}

#[test]
fn responses_function_call_aggregation() {
    let svc = make_service("https://api.example.com/v1", "gpt-x", ThinkingMode::None);
    let req = json!({
        "input": [
            {"type": "function_call", "call_id": "c1", "name": "f1", "arguments": "{\"a\":1}"},
            {"type": "function_call", "call_id": "c2", "name": "f2", "arguments": "{}"},
            {"type": "function_call_output", "call_id": "c1", "output": "r1"},
            {"role": "user", "content": [{"type": "input_text", "text": "go"}]},
        ],
    });
    let out = responses_to_chat(&req, &svc);
    let msgs = out["messages"].as_array().unwrap();
    // 连续 function_call 合并为一条 assistant；随后 tool 消息冲刷；user 最后
    assert_eq!(msgs.len(), 3);
    assert_eq!(msgs[0]["role"], json!("assistant"));
    assert_eq!(msgs[0]["tool_calls"].as_array().unwrap().len(), 2);
    assert_eq!(msgs[0]["tool_calls"][0]["id"], json!("c1"));
    assert_eq!(msgs[0]["content"], json!(null));
    assert_eq!(msgs[1]["role"], json!("tool"));
    assert_eq!(msgs[1]["tool_call_id"], json!("c1"));
    assert_eq!(msgs[2], json!({"role": "user", "content": "go"}));
}

#[test]
fn responses_chat_passthrough_branch() {
    // 无 input（Chat messages 入）：整包透传仅换 model + developer 降级 + max_tokens 兜底
    let svc = make_service("https://api.example.com/v1", "gpt-x", ThinkingMode::None);
    let req = json!({
        "model": "client-m",
        "messages": [{"role": "developer", "content": "sys"}, {"role": "user", "content": "hi"}],
        "stream": true,
        "stop": ["END"],
    });
    let out = responses_to_chat(&req, &svc);
    assert_eq!(out["model"], json!("gpt-x")); // override_model=true
    assert_eq!(out["messages"][0]["role"], json!("system"));
    assert_eq!(out["stop"], json!(["END"])); // 整包字段保留
    assert_eq!(out["max_tokens"], json!(4096)); // 缺省补服务默认
    // Python 的 Chat 直通分支不补 stream_options（仅 input 分支才补）；
    // 客户端自带的字段经整包保留
    assert_eq!(out.get("stream_options"), None);
}

#[test]
fn responses_tools_and_tool_choice() {
    let svc = make_service("https://api.example.com/v1", "gpt-x", ThinkingMode::None);
    let req = json!({
        "input": [{"role": "user", "content": "hi"}],
        "tools": [{"type": "function", "name": "f", "description": "d", "parameters": {"type": "object"}, "strict": true}],
        "tool_choice": {"type": "function", "name": "f"},
    });
    let out = responses_to_chat(&req, &svc);
    assert_eq!(out["tools"][0]["function"]["name"], json!("f"));
    assert_eq!(out["tools"][0]["function"]["strict"], json!(true));
    assert_eq!(out["tool_choice"]["function"]["name"], json!("f"));
}

// ---------- resolve_mode ----------

#[test]
fn resolve_mode_matrix() {
    use o2a_config::DispatchMode as D;

    // api 显式：直接按声明推导
    let mut svc = claude_service();
    svc.api = Some(o2a_config::OpenaiApi::AnthropicMessages);
    assert_eq!(o2a_convert::resolve_mode(&svc, "/v1/messages", None), Some(D::Claude));

    // client=auto：按路径识别 + 账号端点判定
    let mut svc = claude_service();
    svc.client = ClientKind::Auto;
    assert_eq!(
        o2a_convert::resolve_mode(&svc, "/v1/messages", None),
        Some(D::Claude) // 无 anthropic 端点 → 转换
    );
    // anthropic 端点 → direct
    let mut svc2 = claude_service();
    svc2.account.anthropic_url = "https://api.anthropic.com/v1/messages".into();
    assert_eq!(o2a_convert::resolve_mode(&svc2, "/v1/messages", None), Some(D::Direct));
    // openai 客户端 + 纯 anthropic 账号 → None（不支持）
    let mut svc3 = claude_service();
    svc3.account.openai_url = String::new();
    svc3.account.anthropic_url = "https://api.anthropic.com/v1/messages".into();
    assert_eq!(o2a_convert::resolve_mode(&svc3, "/chat/completions", None), None);
}

// ---------- chat_to_responses_json ----------

#[test]
fn chat_to_responses_conversion() {
    let data = json!({
        "id": "chatcmpl-1", "model": "gpt-x",
        "choices": [{
            "index": 0, "finish_reason": "stop",
            "message": {
                "role": "assistant",
                "content": "你好",
                "reasoning_content": "thinking...",
                "tool_calls": [{"id": "c1", "function": {"name": "f", "arguments": "{\"a\":1}"}}],
            },
        }],
        "usage": {"prompt_tokens": 100, "completion_tokens": 20, "total_tokens": 120},
    });
    let out = o2a_convert::chat_to_responses_json(&data, "gpt-x");
    assert_eq!(out["object"], json!("response"));
    assert_eq!(out["status"], json!("completed"));
    assert!(out["id"].as_str().unwrap().starts_with("resp_"));
    let output = out["output"].as_array().unwrap();
    assert_eq!(output.len(), 3);
    assert_eq!(output[0]["type"], json!("message"));
    assert_eq!(output[0]["content"][0]["text"], json!("你好"));
    assert_eq!(output[1]["type"], json!("reasoning"));
    assert_eq!(output[2]["type"], json!("function_call"));
    assert_eq!(output[2]["call_id"], json!("c1"));
    assert_eq!(out["usage"]["input_tokens"], json!(100));
    assert_eq!(out["usage"]["output_tokens"], json!(20));
    assert_eq!(out["usage"]["total_tokens"], json!(120));
    // 非字符串 id 不可用时回退 data.model
    let out2 = o2a_convert::chat_to_responses_json(&data, "");
    assert_eq!(out2["model"], json!("gpt-x"));
}

#[test]
fn chat_to_responses_content_block_list() {
    let data = json!({
        "model": "m",
        "choices": [{"finish_reason": "stop", "message": {"role": "assistant", "content": [
            {"type": "text", "text": "a"}, {"type": "text", "text": "b"},
        ]}}],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1},
    });
    let out = o2a_convert::chat_to_responses_json(&data, "m");
    assert_eq!(out["output"][0]["content"][0]["text"], json!("a\nb"));
}
