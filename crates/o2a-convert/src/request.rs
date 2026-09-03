//! Anthropic Messages → OpenAI Chat 请求转换（对齐 `convert.py` 的 `convert_request`
//! 与 thinking 五态映射 `_apply_thinking_to_chat` / `_apply_reasoning_to_chat`）。

use o2a_config::{Service, ThinkingMode};
use serde_json::{json, Map, Value};

use crate::common::{budget_to_effort, extract_text, infer_thinking_style, key, obj_or_empty, tool_choice_any, truthy};

/// Anthropic Messages → OpenAI chat completions（对齐 `convert_request`）。
pub fn convert_request(req: &Value, service: &Service) -> Value {
    let Some(req_obj) = req.as_object() else {
        // Python 对非 dict req 会 AttributeError；引擎恒传 object，此处安全兜底
        return json!({
            "model": service.model,
            "messages": [],
            "max_tokens": service.max_tokens,
            "stream": false,
        });
    };
    let empty = Vec::new();
    let raw_messages = req_obj
        .get("messages")
        .and_then(|m| m.as_array())
        .unwrap_or(&empty);

    let mut messages: Vec<Value> = Vec::new();
    for msg in raw_messages {
        let Some(m) = msg.as_object() else {
            continue; // Python 对非 dict 会 AttributeError；安全化跳过
        };
        // role：键缺失回退 "user"；键存在（含 null）原样保留（Python dict.get 语义）
        let role = m.get("role").cloned().unwrap_or(json!("user"));
        let empty_content = Value::String(String::new());
        let content = m.get("content").unwrap_or(&empty_content);

        if let Some(blocks) = content.as_array() {
            let has_tool_results = blocks.iter().any(|b| {
                b.get("type").and_then(|t| t.as_str()) == Some("tool_result") && b.is_object()
            });
            if has_tool_results {
                // tool_result 块转 role=tool 消息；交错的 text 块按出现顺序冲刷为 user 消息
                let mut orphan_text_parts: Vec<String> = Vec::new();
                for block in blocks {
                    let Some(b) = block.as_object() else {
                        continue;
                    };
                    let btype = b.get("type").and_then(|t| t.as_str()).unwrap_or("");
                    if btype == "tool_result" {
                        if !orphan_text_parts.is_empty() {
                            messages.push(json!({
                                "role": "user",
                                "content": orphan_text_parts.join("\n"),
                            }));
                            orphan_text_parts.clear();
                        }
                        // tool_use_id 缺失时回退 block.id（键存在语义）；两者皆无才跳过
                        let tool_id = match b.get("tool_use_id") {
                            Some(v) => value_as_str_or_empty(v),
                            None => b
                                .get("id")
                                .map(value_as_str_or_empty)
                                .unwrap_or_default(),
                        };
                        if tool_id.is_empty() {
                            // 缺 tool_use_id 无法形成合法 tool 消息（上游会 400）：跳过
                            continue;
                        }
                        let text = extract_text(b.get("content").unwrap_or(&json!("")));
                        messages.push(json!({
                            "role": "tool",
                            "tool_call_id": tool_id,
                            "content": text,
                        }));
                    } else if btype == "text" {
                        // 与 tool_result 同行的文本块没有 tool_use_id，收集后作为 user 消息
                        orphan_text_parts
                            .push(b.get("text").and_then(|t| t.as_str()).unwrap_or("").into());
                    }
                }
                if !orphan_text_parts.is_empty() {
                    messages.push(json!({
                        "role": "user",
                        "content": orphan_text_parts.join("\n"),
                    }));
                }
                continue;
            }
            if role.as_str() == Some("assistant") {
                let has_tool_uses = blocks.iter().any(|b| {
                    b.get("type").and_then(|t| t.as_str()) == Some("tool_use") && b.is_object()
                });
                if has_tool_uses {
                    let mut text_parts: Vec<String> = Vec::new();
                    let mut tool_calls: Vec<Value> = Vec::new();
                    for block in blocks {
                        let Some(b) = block.as_object() else {
                            continue;
                        };
                        match b.get("type").and_then(|t| t.as_str()) {
                            Some("text") => text_parts
                                .push(b.get("text").and_then(|t| t.as_str()).unwrap_or("").into()),
                            Some("tool_use") => tool_calls.push(json!({
                                "id": b.get("id").cloned().unwrap_or(Value::Null),
                                "type": "function",
                                "function": {
                                    "name": b.get("name").cloned().unwrap_or(Value::Null),
                                    "arguments": serde_json::to_string(
                                        b.get("input").unwrap_or(&json!({})),
                                    )
                                    .unwrap_or_else(|_| "{}".into()),
                                },
                            })),
                            _ => {}
                        }
                    }
                    let mut oai_msg = json!({"role": "assistant", "content": null});
                    if !tool_calls.is_empty() {
                        oai_msg["tool_calls"] = Value::Array(tool_calls);
                    }
                    if !text_parts.is_empty() {
                        oai_msg["content"] = Value::String(text_parts.join("\n"));
                    }
                    messages.push(oai_msg);
                    continue;
                }
            }
        }

        // 普通文本消息 - 转为纯文本（DashScope 不支持 content blocks 格式）
        let text = extract_text(content);
        if text.is_empty() {
            // 纯 thinking 块等空 content 消息，跳过（部分上游拒绝空 content）
            continue;
        }
        messages.push(json!({"role": role, "content": text}));
    }

    // system：truthy 检查（空串/空列表不动），提取为纯文本插到首条
    if let Some(system) = key(req_obj, "system") {
        if truthy(system) {
            let system_content = extract_text(system);
            messages.insert(0, json!({"role": "system", "content": system_content}));
        }
    }

    let is_stream = key(req_obj, "stream").map(truthy).unwrap_or(false);

    // 模型覆盖：override_model=true 强转服务模型；false 透传客户端名（缺失/空回退）
    let client_model = req_obj
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("")
        .to_string();
    let model = if service.override_model {
        service.model.clone()
    } else if !client_model.is_empty() {
        client_model
    } else {
        service.model.clone()
    };

    let mut openai_req = json!({
        "model": model,
        "messages": messages,
        // max_tokens：键存在即命中（null 保留），缺失回退服务默认
        "max_tokens": key(req_obj, "max_tokens").cloned().unwrap_or(json!(service.max_tokens)),
        "stream": key(req_obj, "stream").cloned().unwrap_or(json!(false)),
    });
    if is_stream {
        openai_req["stream_options"] = json!({"include_usage": true});
    }

    // 转发采样参数（子 agent 可能设置特定 temperature；键存在即复制，含 null）
    for k in ["temperature", "top_p"] {
        if let Some(v) = key(req_obj, k) {
            openai_req[k] = v.clone();
        }
    }

    // thinking 参数：按服务 thinking_mode 映射到上游（键存在即调用；null 由映射函数忽略）
    if let Some(thinking) = key(req_obj, "thinking") {
        apply_thinking_to_chat(&mut openai_req, Some(thinking), service);
    }

    // tools: Anthropic → OpenAI
    let empty_arr = Vec::new();
    let tools = req_obj
        .get("tools")
        .and_then(|t| t.as_array())
        .unwrap_or(&empty_arr);
    let mut openai_tools: Vec<Value> = Vec::new();
    for tool in tools {
        let Some(t) = tool.as_object() else {
            continue;
        };
        openai_tools.push(json!({
            "type": "function",
            "function": {
                "name": t.get("name").cloned().unwrap_or(json!("")),
                "description": t.get("description").cloned().unwrap_or(json!("")),
                "parameters": convert_tool_input(t.get("input_schema").unwrap_or(&json!({}))),
                "strict": false,
            },
        }));
    }
    if !openai_tools.is_empty() {
        openai_req["tools"] = Value::Array(openai_tools);
    }

    // tool_choice（truthy 检查：null/空串/空对象不动）
    if let Some(tc) = req_obj.get("tool_choice") {
        if truthy(tc) {
            match tc {
                Value::String(s) => match s.as_str() {
                    "any" => openai_req["tool_choice"] = tool_choice_any(openai_req.get("tools").unwrap_or(&Value::Null)),
                    "auto" | "none" => openai_req["tool_choice"] = tc.clone(),
                    _ => {}
                },
                Value::Object(obj) => {
                    match obj.get("type").and_then(|t| t.as_str()).unwrap_or("") {
                        "tool" => {
                            openai_req["tool_choice"] = json!({
                                "type": "function",
                                "function": {"name": obj.get("name").cloned().unwrap_or(Value::Null)},
                            });
                        }
                        "any" => openai_req["tool_choice"] = tool_choice_any(openai_req.get("tools").unwrap_or(&Value::Null)),
                        "auto" | "none" => openai_req["tool_choice"] = tc.clone(),
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    }

    openai_req
}

/// Python dict 值 → 字符串 best-effort（tool_use_id / id 取值用）。
fn value_as_str_or_empty(v: &Value) -> String {
    v.as_str().unwrap_or("").to_string()
}

/// Anthropic input_schema → OpenAI function parameters（对齐 `convert_tool_input`）。
pub fn convert_tool_input(input_schema: &Value) -> Value {
    match input_schema {
        Value::Object(obj) => {
            let mut params = obj.clone();
            if !params.contains_key("type") {
                params.insert("type".into(), Value::String("object".into()));
            }
            Value::Object(params)
        }
        other => other.clone(),
    }
}

/// Anthropic thinking → Chat 请求参数（对齐 `_apply_thinking_to_chat`）。
pub fn apply_thinking_to_chat(chat: &mut Value, thinking: Option<&Value>, service: &Service) {
    if service.thinking_mode == ThinkingMode::None {
        return;
    }
    let Some(Value::Object(t)) = thinking else {
        return; // not thinking or not isinstance(dict)
    };
    let mut mode = service.thinking_mode;
    if mode == ThinkingMode::Auto {
        mode = infer_thinking_style(service);
    }
    let enabled = t.get("type").and_then(|v| v.as_str()) != Some("disabled");
    match mode {
        ThinkingMode::Passthrough => {
            // 原样保留 type 与 budget_tokens（Kimi 支持 budget 控制深度）
            let mut out = Map::new();
            out.insert(
                "type".into(),
                t.get("type").cloned().unwrap_or(json!("enabled")),
            );
            if enabled {
                if let Some(budget) = t.get("budget_tokens") {
                    if truthy(budget) {
                        out.insert("budget_tokens".into(), budget.clone());
                    }
                }
            }
            chat["thinking"] = Value::Object(out);
        }
        ThinkingMode::EnableThinking => {
            // DashScope / Qwen 兼容模式：布尔开关
            chat["enable_thinking"] = json!(enabled);
        }
        ThinkingMode::Effort => {
            // OpenAI 档位：budget → low/medium/high；enabled 无预算时 medium 兜底
            let effort = if enabled {
                budget_to_effort(t.get("budget_tokens")).or(Some("medium"))
            } else {
                None
            };
            if let Some(e) = effort {
                chat["reasoning_effort"] = json!(e);
            }
            // disabled 时 OpenAI 系无关闭语义，忽略（由模型默认决定）
        }
        ThinkingMode::Auto | ThinkingMode::None => {}
    }
}

/// OpenAI Responses reasoning（effort 档位）→ Chat 请求参数（对齐 `_apply_reasoning_to_chat`）。
pub fn apply_reasoning_to_chat(chat: &mut Value, req: &Value, service: &Service) {
    if service.thinking_mode == ThinkingMode::None {
        return;
    }
    // req.get("reasoning") or {}：truthy 但非 dict → Python isinstance 检查后按 None 处理
    let reasoning = obj_or_empty(req.get("reasoning"));
    let mut effort: Option<&Value> = reasoning
        .and_then(|r| key(r, "effort"))
        .filter(|v| truthy(v));
    if effort.is_none() {
        effort = req.get("reasoning_effort").filter(|v| truthy(v));
    }
    let Some(effort) = effort else {
        return;
    };
    let mut mode = service.thinking_mode;
    if mode == ThinkingMode::Auto {
        mode = infer_thinking_style(service);
    }
    match mode {
        ThinkingMode::Effort => chat["reasoning_effort"] = effort.clone(),
        ThinkingMode::Passthrough => {
            // Responses 无 token 预算概念，effort 存在即开启思考（深度由上游默认）
            chat["thinking"] = json!({"type": "enabled"});
        }
        ThinkingMode::EnableThinking => {
            chat["enable_thinking"] = json!(true);
        }
        ThinkingMode::Auto | ThinkingMode::None => {}
    }
}
