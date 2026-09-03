//! thinking 五态映射测试（对齐 tests/test_thinking.py 用例语义）。

use o2a_config::ThinkingMode;
use o2a_convert::budget_to_effort;
use o2a_convert::{
    apply_reasoning_to_chat, apply_thinking_to_chat, convert_request,
    infer_thinking_style, responses_to_chat,
};
use serde_json::json;

mod common;
use common::make_service;

// ---------- budget_tokens → reasoning_effort ----------

#[test]
fn budget_to_effort_thresholds() {
    let b = |v: Option<serde_json::Value>| budget_to_effort(v.as_ref());
    assert_eq!(b(Some(json!(512))), Some("low"));
    assert_eq!(b(Some(json!(2047))), Some("low"));
    assert_eq!(b(Some(json!(2048))), Some("medium"));
    assert_eq!(b(Some(json!(8191))), Some("medium"));
    assert_eq!(b(Some(json!(8192))), Some("high"));
    assert_eq!(b(Some(json!(32000))), Some("high"));
    assert_eq!(b(None), None);
    assert_eq!(b(Some(json!(0))), None);
    assert_eq!(b(Some(json!("abc"))), None); // Python int("abc") 抛异常 → None
    assert_eq!(b(Some(json!(null))), None);
}

// ---------- auto 推断 ----------

#[test]
fn infer_style_by_url() {
    use ThinkingMode::*;
    assert_eq!(
        infer_thinking_style(&make_service(
            "https://dashscope.aliyuncs.com/compatible-mode/v1",
            "gpt-x",
            Auto
        )),
        EnableThinking
    );
    assert_eq!(
        infer_thinking_style(&make_service("https://api.deepseek.com", "gpt-x", Auto)),
        Passthrough
    );
    assert_eq!(
        infer_thinking_style(&make_service("https://api.moonshot.cn/v1", "gpt-x", Auto)),
        Passthrough
    );
    assert_eq!(
        infer_thinking_style(&make_service("https://api.kimi.com/v1", "gpt-x", Auto)),
        Passthrough
    );
    assert_eq!(
        infer_thinking_style(&make_service(
            "https://opencode.ai/zen/v1/chat/completions",
            "gpt-x",
            Auto
        )),
        Effort
    );
    // 模型名推断
    assert_eq!(
        infer_thinking_style(&make_service("https://api.example.com/v1", "qwen3-max", Auto)),
        EnableThinking
    );
    assert_eq!(
        infer_thinking_style(&make_service(
            "https://api.example.com/v1",
            "kimi-k2-thinking",
            Auto
        )),
        Passthrough
    );
}

// ---------- Anthropic thinking → Chat ----------

#[test]
fn thinking_passthrough_keeps_budget() {
    let mut chat = json!({});
    apply_thinking_to_chat(
        &mut chat,
        Some(&json!({"type": "enabled", "budget_tokens": 32000})),
        &make_service("https://api.example.com/v1", "gpt-x", ThinkingMode::Passthrough),
    );
    assert_eq!(chat, json!({"thinking": {"type": "enabled", "budget_tokens": 32000}}));
}

#[test]
fn thinking_passthrough_disabled() {
    let mut chat = json!({});
    apply_thinking_to_chat(
        &mut chat,
        Some(&json!({"type": "disabled"})),
        &make_service("https://api.example.com/v1", "gpt-x", ThinkingMode::Passthrough),
    );
    assert_eq!(chat, json!({"thinking": {"type": "disabled"}}));
}

#[test]
fn thinking_effort_mapping() {
    let svc = || make_service("https://api.example.com/v1", "gpt-x", ThinkingMode::Effort);
    let mut chat = json!({});
    apply_thinking_to_chat(&mut chat, Some(&json!({"type": "enabled", "budget_tokens": 32000})), &svc());
    assert_eq!(chat, json!({"reasoning_effort": "high"}));
    let mut chat = json!({});
    apply_thinking_to_chat(&mut chat, Some(&json!({"type": "enabled", "budget_tokens": 1024})), &svc());
    assert_eq!(chat, json!({"reasoning_effort": "low"}));
    // enabled 无预算 → medium 兜底（显式开启思考）
    let mut chat = json!({});
    apply_thinking_to_chat(&mut chat, Some(&json!({"type": "enabled"})), &svc());
    assert_eq!(chat, json!({"reasoning_effort": "medium"}));
    // disabled → 忽略（OpenAI 系无关闭语义，由模型默认决定）
    let mut chat = json!({});
    apply_thinking_to_chat(
        &mut chat,
        Some(&json!({"type": "disabled", "budget_tokens": 32000})),
        &svc(),
    );
    assert_eq!(chat, json!({}));
}

#[test]
fn thinking_enable_thinking_bool() {
    let svc = || make_service("https://api.example.com/v1", "gpt-x", ThinkingMode::EnableThinking);
    let mut chat = json!({});
    apply_thinking_to_chat(&mut chat, Some(&json!({"type": "enabled", "budget_tokens": 32000})), &svc());
    assert_eq!(chat, json!({"enable_thinking": true}));
    let mut chat = json!({});
    apply_thinking_to_chat(&mut chat, Some(&json!({"type": "disabled"})), &svc());
    assert_eq!(chat, json!({"enable_thinking": false}));
}

#[test]
fn thinking_auto_inference() {
    // dashscope → 布尔开关（深度由模型默认）
    let mut chat = json!({});
    apply_thinking_to_chat(
        &mut chat,
        Some(&json!({"type": "enabled", "budget_tokens": 32000})),
        &make_service("https://dashscope.aliyuncs.com/compatible-mode/v1", "gpt-x", ThinkingMode::Auto),
    );
    assert_eq!(chat, json!({"enable_thinking": true}));
    // deepseek → 原样对象（保留 budget，由上游消费）
    let mut chat = json!({});
    apply_thinking_to_chat(
        &mut chat,
        Some(&json!({"type": "enabled", "budget_tokens": 32000})),
        &make_service("https://api.deepseek.com", "gpt-x", ThinkingMode::Auto),
    );
    assert_eq!(chat, json!({"thinking": {"type": "enabled", "budget_tokens": 32000}}));
    // 其他网关 → effort 档位
    let mut chat = json!({});
    apply_thinking_to_chat(
        &mut chat,
        Some(&json!({"type": "enabled", "budget_tokens": 32000})),
        &make_service("https://opencode.ai/zen/v1/chat/completions", "gpt-x", ThinkingMode::Auto),
    );
    assert_eq!(chat, json!({"reasoning_effort": "high"}));
}

#[test]
fn thinking_none_mode() {
    let mut chat = json!({"model": "x"});
    apply_thinking_to_chat(
        &mut chat,
        Some(&json!({"type": "enabled", "budget_tokens": 32000})),
        &make_service("https://api.example.com/v1", "gpt-x", ThinkingMode::None),
    );
    assert_eq!(chat, json!({"model": "x"}));
}

// ---------- Responses reasoning → Chat ----------

#[test]
fn reasoning_to_chat_effort() {
    let svc = || make_service("https://api.example.com/v1", "gpt-x", ThinkingMode::Effort);
    let mut chat = json!({});
    apply_reasoning_to_chat(&mut chat, &json!({"reasoning": {"effort": "high"}}), &svc());
    assert_eq!(chat, json!({"reasoning_effort": "high"}));
    // 顶层标量（部分客户端直接发 reasoning_effort）
    let mut chat = json!({});
    apply_reasoning_to_chat(&mut chat, &json!({"reasoning_effort": "low"}), &svc());
    assert_eq!(chat, json!({"reasoning_effort": "low"}));
}

#[test]
fn reasoning_to_chat_passthrough_and_bool() {
    let mut chat = json!({});
    apply_reasoning_to_chat(
        &mut chat,
        &json!({"reasoning": {"effort": "high"}}),
        &make_service("https://api.example.com/v1", "gpt-x", ThinkingMode::Passthrough),
    );
    assert_eq!(chat, json!({"thinking": {"type": "enabled"}}));
    let mut chat = json!({});
    apply_reasoning_to_chat(
        &mut chat,
        &json!({"reasoning": {"effort": "high"}}),
        &make_service("https://api.example.com/v1", "gpt-x", ThinkingMode::EnableThinking),
    );
    assert_eq!(chat, json!({"enable_thinking": true}));
    // 无 effort → 不动
    let mut chat = json!({"model": "x"});
    apply_reasoning_to_chat(
        &mut chat,
        &json!({"reasoning": {}}),
        &make_service("https://api.example.com/v1", "gpt-x", ThinkingMode::Effort),
    );
    assert_eq!(chat, json!({"model": "x"}));
}

#[test]
fn reasoning_to_chat_none() {
    let mut chat = json!({});
    apply_reasoning_to_chat(
        &mut chat,
        &json!({"reasoning": {"effort": "high"}}),
        &make_service("https://api.example.com/v1", "gpt-x", ThinkingMode::None),
    );
    assert_eq!(chat, json!({}));
}

// ---------- 集成 ----------

#[test]
fn convert_request_with_thinking() {
    let svc = make_service("https://api.example.com/v1", "gpt-x", ThinkingMode::Effort);
    let req = json!({
        "model": "claude-sonnet",
        "max_tokens": 4096,
        "messages": [{"role": "user", "content": "hi"}],
        "thinking": {"type": "enabled", "budget_tokens": 32000},
    });
    let out = convert_request(&req, &svc);
    assert_eq!(out["reasoning_effort"], json!("high"));
    assert_eq!(out["messages"], json!([{"role": "user", "content": "hi"}]));
}

#[test]
fn convert_request_without_thinking() {
    let svc = make_service("https://api.example.com/v1", "gpt-x", ThinkingMode::Effort);
    let req = json!({
        "model": "claude-sonnet",
        "max_tokens": 4096,
        "messages": [{"role": "user", "content": "hi"}],
    });
    let out = convert_request(&req, &svc);
    assert!(out.get("reasoning_effort").is_none());
}

#[test]
fn responses_to_chat_with_reasoning() {
    let svc = make_service("https://api.example.com/v1", "gpt-x", ThinkingMode::Effort);
    let req = json!({
        "model": "gpt-5",
        "input": "hello",
        "reasoning": {"effort": "high"},
    });
    let out = responses_to_chat(&req, &svc);
    assert_eq!(out["reasoning_effort"], json!("high"));
    assert_eq!(out["messages"], json!([{"role": "user", "content": "hello"}]));
}

#[test]
fn chat_direct_passthrough_keeps_reasoning_effort() {
    // api=openai-completions 直通：无 input 的 Chat 格式入参整包透传，reasoning_effort 原样保留
    let svc = make_service("https://api.example.com/v1", "gpt-x", ThinkingMode::None);
    let req = json!({
        "model": "gpt-5",
        "messages": [{"role": "user", "content": "hi"}],
        "reasoning_effort": "high",
    });
    let out = responses_to_chat(&req, &svc);
    assert_eq!(out["reasoning_effort"], json!("high"));
    assert_eq!(out["model"], json!("gpt-x")); // override_model=true 默认用服务模型
}
