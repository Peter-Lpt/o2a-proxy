//! OpenAI Responses ↔ Chat 转换（对齐 `convert.py` 的 `_responses_to_chat` /
//! `_chat_to_responses_json` / `_ResponsesStreamTranslator` / `_responses_content_to_text`）。

use serde_json::{json, Map, Value};
use std::collections::HashMap;

use o2a_config::Service;

use crate::common::{key, truthy};
use crate::request::apply_reasoning_to_chat;
use crate::usage::chat_usage_to_responses;

/// 将 Responses API 消息 content parts 提取为纯文本（对齐 `_responses_content_to_text`）。
pub fn responses_content_to_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => {
            let mut out: Vec<String> = Vec::new();
            for p in parts {
                match p {
                    Value::String(s) => out.push(s.clone()),
                    Value::Object(obj) => {
                        let t = obj.get("type").and_then(|t| t.as_str()).unwrap_or("");
                        match obj.get("text") {
                            // "text" 键存在且为字符串值即取（对齐 Python `if "text" in p and isinstance(str)`）
                            Some(Value::String(s)) => out.push(s.clone()),
                            _ if t == "input_text" || t == "output_text" => out.push(
                                obj.get("text").and_then(|t| t.as_str()).unwrap_or("").into(),
                            ),
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }
            out.join("\n")
        }
        _ => String::new(),
    }
}

/// 将 OpenAI Responses API 请求转成 Chat Completions 请求（对齐 `_responses_to_chat`）。
pub fn responses_to_chat(req: &Value, service: &Service) -> Value {
    let Some(req_obj) = req.as_object() else {
        return json!({"model": service.model, "messages": [], "stream": false, "max_tokens": service.max_tokens});
    };
    let mut messages: Vec<Value> = Vec::new();
    let mut pending_calls: Vec<Value> = Vec::new(); // 连续 function_call 项合并为一条 assistant 消息

    fn flush_calls(pending: &mut Vec<Value>, messages: &mut Vec<Value>) {
        if !pending.is_empty() {
            messages.push(json!({
                "role": "assistant",
                "content": null,
                "tool_calls": pending,
            }));
            pending.clear();
        }
    }

    // `if not req.get("input")`：falsy（缺失/null/空串/空列表）→ Chat Completions 直通分支
    let input_val = req_obj.get("input");
    if !input_val.map(truthy).unwrap_or(false) {
        // Chat 直通：整包透传（保留 stream/tools/stop 等全部字段），仅替换 model、规范化 role
        let mut chat = Map::new();
        for (k, v) in req_obj {
            if k == "model" {
                continue;
            }
            chat.insert(k.clone(), v.clone());
        }
        let mut msgs: Vec<Value> = Vec::new();
        if let Some(arr) = chat.get("messages").and_then(|m| m.as_array()) {
            for msg in arr {
                let Some(m) = msg.as_object() else {
                    continue; // 非 dict 项丢弃（Python continue）
                };
                let mut m = m.clone();
                if m.get("role").and_then(|r| r.as_str()) == Some("developer") {
                    m.insert("role".into(), json!("system"));
                }
                msgs.push(Value::Object(m));
            }
        }
        chat.insert("messages".into(), Value::Array(msgs));
        // 模型覆盖：默认服务模型；override_model=false 透传客户端名（缺省回退服务配置）
        let model = if service.override_model {
            service.model.clone()
        } else {
            req_obj
                .get("model")
                .and_then(|m| m.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or(&service.model)
                .to_string()
        };
        chat.insert("model".into(), json!(model));
        // 没带 max_tokens 时用服务默认（不做封顶，透传）
        let has_max = chat
            .get("max_tokens")
            .map(truthy)
            .unwrap_or(false)
            || chat
                .get("max_output_tokens")
                .map(truthy)
                .unwrap_or(false);
        if !has_max {
            chat.insert("max_tokens".into(), json!(service.max_tokens));
        }
        return Value::Object(chat);
    }

    let raw_input = req_obj.get("input").unwrap();
    let items: Vec<Value> = match raw_input {
        Value::String(s) => vec![json!({"role": "user", "content": s})],
        Value::Array(arr) => arr.clone(),
        _ => Vec::new(),
    };
    for item in &items {
        let Some(it) = item.as_object() else {
            continue;
        };
        let itype = it.get("type").and_then(|t| t.as_str()).unwrap_or("");
        if itype == "function_call" {
            pending_calls.push(json!({
                "id": it.get("call_id")
                    .or_else(|| it.get("id"))
                    .cloned()
                    .unwrap_or(json!("")),
                "type": "function",
                "function": {
                    "name": it.get("name").cloned().unwrap_or(json!("")),
                    "arguments": it.get("arguments").cloned().unwrap_or(json!("")),
                },
            }));
        } else if itype == "function_call_output" {
            flush_calls(&mut pending_calls, &mut messages);
            messages.push(json!({
                "role": "tool",
                "tool_call_id": it.get("call_id")
                    .or_else(|| it.get("id"))
                    .cloned()
                    .unwrap_or(json!("")),
                "content": it.get("output").cloned().unwrap_or(json!("")),
            }));
        } else if it.contains_key("role") {
            flush_calls(&mut pending_calls, &mut messages);
            let mut role = it.get("role").cloned().unwrap_or(json!(""));
            if role.as_str() == Some("developer") {
                role = json!("system");
            }
            messages.push(json!({
                "role": role,
                "content": responses_content_to_text(it.get("content").unwrap_or(&json!(""))),
            }));
        }
    }
    flush_calls(&mut pending_calls, &mut messages);

    let instructions = req_obj.get("instructions").cloned().unwrap_or(json!(""));
    if truthy(&instructions) {
        let first_is_system = messages
            .first()
            .and_then(|m| m.get("role"))
            .map(|r| r.as_str() == Some("system"))
            .unwrap_or(false);
        if first_is_system {
            // input 已含 system 角色消息时合并，避免产生两条 system
            // Python：messages[0]["content"] = instructions + "\n\n" + prev（纯字符串拼接）
            let prev = messages[0]
                .get("content")
                .cloned()
                .unwrap_or(json!(""));
            let prev_str = prev.as_str().unwrap_or("").to_string();
            let instr_str = instructions.as_str().unwrap_or("").to_string();
            messages[0]["content"] = if !prev_str.is_empty() {
                json!(format!("{instr_str}\n\n{prev_str}"))
            } else {
                json!(instr_str)
            };
        } else {
            messages.insert(0, json!({"role": "system", "content": instructions}));
        }
    }

    let chat_model = if service.override_model {
        service.model.clone()
    } else {
        req_obj
            .get("model")
            .and_then(|m| m.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or(&service.model)
            .to_string()
    };
    let mut chat = json!({
        "model": chat_model,
        "messages": messages,
        "stream": req_obj.get("stream").cloned().unwrap_or(json!(false)),
    });
    // max_output_tokens 存在时重命名为 max_tokens；max_tokens 存在时原样；都缺省用服务默认
    if let Some(v) = req_obj.get("max_output_tokens") {
        chat["max_tokens"] = v.clone();
    } else if let Some(v) = req_obj.get("max_tokens") {
        chat["max_tokens"] = v.clone();
    } else {
        chat["max_tokens"] = json!(service.max_tokens);
    }
    // 白名单透传：temperature/top_p/stream_options/seed/parallel_tool_calls 存在才复制
    for k in ["temperature", "top_p", "stream_options", "seed", "parallel_tool_calls"] {
        if let Some(v) = key(req_obj, k) {
            chat[k] = v.clone();
        }
    }
    let stream = chat.get("stream").map(truthy).unwrap_or(false);
    if stream && chat.get("stream_options").is_none() {
        chat["stream_options"] = json!({"include_usage": true});
    }

    // tools：仅取 type=function 转 OpenAI 结构
    let empty_arr = Vec::new();
    let tools = req_obj
        .get("tools")
        .and_then(|t| t.as_array())
        .unwrap_or(&empty_arr);
    let mut chat_tools: Vec<Value> = Vec::new();
    for t in tools {
        let Some(obj) = t.as_object() else {
            continue;
        };
        if obj.get("type").and_then(|x| x.as_str()) != Some("function") {
            continue;
        }
        chat_tools.push(json!({
            "type": "function",
            "function": {
                "name": obj.get("name").cloned().unwrap_or(json!("")),
                "description": obj.get("description").cloned().unwrap_or(json!("")),
                "parameters": obj
                    .get("parameters")
                    .cloned()
                    .unwrap_or(json!({"type": "object"})),
                "strict": obj.get("strict").cloned().unwrap_or(json!(false)),
            },
        }));
    }
    if !chat_tools.is_empty() {
        chat["tools"] = Value::Array(chat_tools);
    }

    // tool_choice（truthy 检查）
    if let Some(tc) = req_obj.get("tool_choice") {
        if truthy(tc) {
            match tc {
                Value::String(_) => chat["tool_choice"] = tc.clone(),
                Value::Object(obj) => {
                    chat["tool_choice"] = json!({
                        "type": "function",
                        "function": {"name": obj.get("name").cloned().unwrap_or(Value::Null)},
                    });
                }
                _ => {}
            }
        }
    }
    // 思考深度：Responses reasoning（effort 档位）→ 上游 Chat 参数
    apply_reasoning_to_chat(&mut chat, req, service);
    chat
}

/// Chat Completions 非流式响应 → Responses API 响应（对齐 `_chat_to_responses_json`）。
pub fn chat_to_responses_json(data: &Value, model: &str) -> Value {
    let mut output: Vec<Value> = Vec::new();
    let choice = data
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|c| c.first())
        .cloned()
        .unwrap_or(json!({}));
    let message = choice.get("message").cloned().unwrap_or(json!({}));
    // 文本输出
    let text_val = message.get("content").cloned().unwrap_or(json!(""));
    let mut text = text_val.as_str().unwrap_or("").to_string();
    if !text_val.is_null() && text.is_empty() && text_val.is_array() {
        // 上游 content 为 block 列表时转为纯文本，避免 Responses 结构非法
        text = responses_content_to_text(&text_val);
    }
    if !text.is_empty() {
        output.push(json!({
            "id": format!("msg_{}", output.len()),
            "type": "message",
            "role": "assistant",
            "status": "completed",
            "content": [{"type": "output_text", "text": text, "annotations": []}],
        }));
    }
    // 推理内容（reasoning_content → Responses reasoning item）
    let reasoning = message
        .get("reasoning_content")
        .and_then(|r| r.as_str())
        .unwrap_or("");
    if !reasoning.is_empty() {
        output.push(json!({
            "id": format!("reasoning_{}", output.len()),
            "type": "reasoning",
            "status": "completed",
            "summary": [{"type": "summary_text", "text": reasoning}],
            "content": [{"type": "reasoning_text", "text": reasoning}],
        }));
    }
    // 函数调用
    if let Some(tcs) = message.get("tool_calls").and_then(|t| t.as_array()) {
        for tc in tcs {
            let fn_obj = tc.get("function").cloned().unwrap_or(json!({}));
            output.push(json!({
                "id": format!("fc_{}", output.len()),
                "type": "function_call",
                "status": "completed",
                "name": fn_obj.get("name").cloned().unwrap_or(json!("")),
                "call_id": tc.get("id").cloned().unwrap_or(json!("")),
                "arguments": fn_obj.get("arguments").cloned().unwrap_or(json!("")),
            }));
        }
    }
    json!({
        "id": format!("resp_{}", random_hex24()),
        "object": "response",
        "created_at": unix_now(),
        "status": "completed",
        "model": if !model.is_empty() { json!(model) } else { data.get("model").cloned().unwrap_or(json!("")) },
        "output": output,
        "parallel_tool_calls": true,
        "tools": [],
        "usage": chat_usage_to_responses(data.get("usage")),
    })
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 24 位十六进制随机串（对齐 `uuid4().hex[:24]`）。
pub fn random_hex24() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    (0..12)
        .map(|_| format!("{:02x}", rng.gen::<u8>()))
        .collect()
}

// ---------------------------------------------------------------------------
// Responses 流式翻译器
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
struct ToolState {
    id: String,
    name: String,
    arguments: String,
    output_index: i64,
    item_id: String,
}

/// 将 Chat Completions 流式 SSE 翻译为 Responses API 流式 SSE
/// （对齐 `_ResponsesStreamTranslator`，逐事件复刻）。
pub struct ResponsesStreamTranslator {
    model: String,
    response_id: String,
    created_at: i64,
    output_index: i64,
    emitted_created: bool,
    finished: bool,  // 内容 done 事件已发射（幂等）
    completed: bool, // response.completed 已发射（幂等）
    msg_item_id: String,
    msg_output_index: i64,
    msg_delivered: bool,
    text: String,
    tool_states: HashMap<i64, ToolState>,
    tool_order: Vec<i64>,
    delivered_tool: Vec<i64>,
    output_sequence: Vec<(&'static str, Option<i64>)>, // ('message'|'tool'|'reasoning', key)
    reasoning_item_id: String,
    reasoning_output_index: i64,
    reasoning_delivered: bool,
    reasoning_text: String,
    usage: Option<Value>,
}

impl ResponsesStreamTranslator {
    pub fn new(model: &str) -> Self {
        Self::with_identity(model, &format!("resp_{}", random_hex24()), unix_now())
    }

    /// 测试注入口：固定 response_id / created_at 以获得确定性事件序列。
    pub fn with_identity(model: &str, response_id: &str, created_at: i64) -> Self {
        Self {
            model: model.to_string(),
            response_id: response_id.to_string(),
            created_at,
            output_index: 0,
            emitted_created: false,
            finished: false,
            completed: false,
            msg_item_id: String::new(),
            msg_output_index: 0,
            msg_delivered: false,
            text: String::new(),
            tool_states: HashMap::new(),
            tool_order: Vec::new(),
            delivered_tool: Vec::new(),
            output_sequence: Vec::new(),
            reasoning_item_id: String::new(),
            reasoning_output_index: 0,
            reasoning_delivered: false,
            reasoning_text: String::new(),
            usage: None,
        }
    }

    fn base_response(&self) -> Value {
        json!({
            "id": self.response_id,
            "object": "response",
            "created_at": self.created_at,
            "status": "in_progress",
            "model": self.model,
            "output": [],
            "parallel_tool_calls": true,
            "tools": [],
            "usage": self.usage.clone().unwrap_or(Value::Null),
        })
    }

    fn ensure_created(&mut self, events: &mut Vec<Value>) {
        if !self.emitted_created {
            self.emitted_created = true;
            events.push(json!({"type": "response.created", "response": self.base_response()}));
        }
    }

    fn deliver_message(&mut self, events: &mut Vec<Value>) {
        if self.msg_delivered {
            return;
        }
        self.msg_delivered = true;
        self.msg_item_id = format!("msg_{}", self.output_index);
        self.msg_output_index = self.output_index;
        self.output_index += 1;
        self.output_sequence.push(("message", None));
        let item = json!({
            "id": self.msg_item_id,
            "type": "message",
            "role": "assistant",
            "status": "in_progress",
            "content": [],
        });
        events.push(json!({
            "type": "response.output_item.added",
            "output_index": self.msg_output_index,
            "item": item,
        }));
        events.push(json!({
            "type": "response.content_part.added",
            "item_id": self.msg_item_id,
            "output_index": self.msg_output_index,
            "content_index": 0,
            "part": {"type": "output_text", "text": "", "annotations": []},
        }));
    }

    fn deliver_reasoning(&mut self, events: &mut Vec<Value>) {
        if self.reasoning_delivered {
            return;
        }
        self.reasoning_delivered = true;
        self.reasoning_item_id = format!("rs_{}", self.output_index);
        self.reasoning_output_index = self.output_index;
        self.output_index += 1;
        self.output_sequence.push(("reasoning", None));
        let item = json!({
            "id": self.reasoning_item_id,
            "type": "reasoning",
            "status": "in_progress",
            "summary": [],
            "content": [],
        });
        events.push(json!({
            "type": "response.output_item.added",
            "output_index": self.reasoning_output_index,
            "item": item,
        }));
    }

    fn deliver_tool(&mut self, idx: i64, events: &mut Vec<Value>) {
        if self.delivered_tool.contains(&idx) {
            return;
        }
        self.delivered_tool.push(idx);
        let state = self.tool_states.get_mut(&idx).expect("tool state exists");
        state.output_index = self.output_index;
        state.item_id = format!("fc_{}", self.output_index);
        self.output_index += 1;
        self.output_sequence.push(("tool", Some(idx)));
        let item = json!({
            "id": state.item_id,
            "type": "function_call",
            "status": "in_progress",
            "name": state.name,
            "call_id": state.id,
            "arguments": "",
        });
        events.push(json!({
            "type": "response.output_item.added",
            "output_index": state.output_index,
            "item": item,
        }));
    }

    /// 处理一个 chat chunk，返回 Responses 事件列表（对齐 `translate`）。
    pub fn translate(&mut self, data: &Value) -> Vec<Value> {
        let mut events: Vec<Value> = Vec::new();
        let choices = data.get("choices").and_then(|c| c.as_array());
        if let Some(u) = data.get("usage") {
            if truthy(u) {
                self.usage = Some(chat_usage_to_responses(Some(u)));
            }
        }
        let Some(choices) = choices else {
            return events;
        };
        if choices.is_empty() {
            return events;
        }
        let choice = &choices[0];
        let delta = choice.get("delta").cloned().unwrap_or(json!({}));

        let content = delta.get("content").and_then(|c| c.as_str());
        let reasoning = delta.get("reasoning_content").and_then(|r| r.as_str());
        // 推理内容 → reasoning item（保持与文本输出并行）
        if let Some(r) = reasoning {
            if !r.is_empty() {
                self.ensure_created(&mut events);
                self.deliver_reasoning(&mut events);
                self.reasoning_text.push_str(r);
                events.push(json!({
                    "type": "response.reasoning_summary_text.delta",
                    "item_id": self.reasoning_item_id,
                    "output_index": self.reasoning_output_index,
                    "delta": r,
                }));
            }
        }

        if let Some(c) = content {
            if !c.is_empty() {
                self.ensure_created(&mut events);
                self.deliver_message(&mut events);
                self.text.push_str(c);
                events.push(json!({
                    "type": "response.output_text.delta",
                    "item_id": self.msg_item_id,
                    "output_index": self.msg_output_index,
                    "content_index": 0,
                    "delta": c,
                }));
            }
        }

        if let Some(tcs) = delta.get("tool_calls").and_then(|t| t.as_array()) {
            for tc in tcs {
                let idx = tc.get("index").and_then(|v| v.as_i64()).unwrap_or(0);
                let fn_obj = tc.get("function").cloned().unwrap_or(json!({}));
                use std::collections::hash_map::Entry;
                match self.tool_states.entry(idx) {
                    Entry::Vacant(e) => {
                        e.insert(ToolState {
                            id: tc.get("id").and_then(|v| v.as_str()).unwrap_or("").into(),
                            name: fn_obj.get("name").and_then(|v| v.as_str()).unwrap_or("").into(),
                            arguments: String::new(),
                            output_index: 0,
                            item_id: String::new(),
                        });
                        self.tool_order.push(idx);
                    }
                    Entry::Occupied(mut e) => {
                        let state = e.get_mut();
                        if let Some(n) = fn_obj.get("name").and_then(|v| v.as_str()) {
                            if !n.is_empty() {
                                state.name = n.to_string();
                            }
                        }
                        if let Some(id) = tc.get("id").and_then(|v| v.as_str()) {
                            if !id.is_empty() {
                                state.id = id.to_string();
                            }
                        }
                    }
                }
                if let Some(args) = fn_obj.get("arguments").and_then(|a| a.as_str()) {
                    if !args.is_empty() {
                        self.ensure_created(&mut events);
                        self.deliver_tool(idx, &mut events);
                        let state = self.tool_states.get_mut(&idx).unwrap();
                        state.arguments.push_str(args);
                        let (item_id, output_index) =
                            (state.item_id.clone(), state.output_index);
                        events.push(json!({
                            "type": "response.function_call_arguments.delta",
                            "item_id": item_id,
                            "output_index": output_index,
                            "delta": args,
                        }));
                    }
                }
            }
        }

        let finish_reason = choice.get("finish_reason");
        let has_finish = match finish_reason {
            Some(v) => !v.is_null(),
            None => false,
        };
        if has_finish {
            // 只收尾内容块（done 事件）；response.completed 延迟到流结束（[DONE]/EOF）
            // 由 complete() 发射，确保 usage 尾块（标准顺序在 finish_reason 之后）已到达
            self.close_items(&mut events);
        }
        events
    }

    /// 收尾内容块：done 事件（推理/消息/工具），幂等；不发射 response.completed。
    fn close_items(&mut self, events: &mut Vec<Value>) {
        if self.finished {
            return;
        }
        self.finished = true;
        if !self.emitted_created {
            self.ensure_created(events);
        }
        // 关闭推理内容
        if self.reasoning_delivered {
            events.push(json!({
                "type": "response.reasoning_summary_text.done",
                "item_id": self.reasoning_item_id,
                "output_index": self.reasoning_output_index,
                "text": self.reasoning_text,
            }));
            events.push(json!({
                "type": "response.output_item.done",
                "output_index": self.reasoning_output_index,
                "item": {
                    "id": self.reasoning_item_id, "type": "reasoning",
                    "status": "completed",
                    "summary": [{"type": "summary_text", "text": self.reasoning_text}],
                    "content": [{"type": "reasoning_text", "text": self.reasoning_text}],
                },
            }));
        }
        // 关闭文本消息
        if self.msg_delivered {
            events.push(json!({
                "type": "response.output_text.done",
                "item_id": self.msg_item_id,
                "output_index": self.msg_output_index,
                "content_index": 0,
                "text": self.text,
            }));
            events.push(json!({
                "type": "response.content_part.done",
                "item_id": self.msg_item_id,
                "output_index": self.msg_output_index,
                "content_index": 0,
                "part": {"type": "output_text", "text": self.text, "annotations": []},
            }));
            events.push(json!({
                "type": "response.output_item.done",
                "output_index": self.msg_output_index,
                "item": {
                    "id": self.msg_item_id, "type": "message",
                    "role": "assistant", "status": "completed",
                    "content": [{"type": "output_text", "text": self.text, "annotations": []}],
                },
            }));
        }
        // 关闭工具调用
        for idx in self.tool_order.clone() {
            if !self.delivered_tool.contains(&idx) {
                self.deliver_tool(idx, events);
            }
            let state = &self.tool_states[&idx];
            events.push(json!({
                "type": "response.function_call_arguments.done",
                "item_id": state.item_id,
                "output_index": state.output_index,
                "arguments": state.arguments,
            }));
            events.push(json!({
                "type": "response.output_item.done",
                "output_index": state.output_index,
                "item": {
                    "id": state.item_id, "type": "function_call",
                    "status": "completed", "name": state.name,
                    "call_id": state.id, "arguments": state.arguments,
                },
            }));
        }
    }

    /// 发射 response.completed（含最终 usage），幂等。流结束（[DONE]/EOF）时调用。
    pub fn complete(&mut self) -> Vec<Value> {
        let mut events = Vec::new();
        if self.completed {
            return events;
        }
        self.close_items(&mut events);
        self.completed = true;
        events.push(json!({"type": "response.completed", "response": self.assemble()}));
        events
    }

    /// 完整收尾：done 事件 + response.completed（幂等，供流结束统一调用，对齐 `_finish`）。
    pub fn finish(&mut self) -> Vec<Value> {
        self.complete()
    }

    fn assemble(&self) -> Value {
        let mut output: Vec<Value> = Vec::new();
        for (kind, key_idx) in &self.output_sequence {
            match (*kind, key_idx) {
                ("message", _) => output.push(json!({
                    "id": self.msg_item_id, "type": "message", "role": "assistant",
                    "status": "completed",
                    "content": [{"type": "output_text", "text": self.text, "annotations": []}],
                })),
                ("reasoning", _) => output.push(json!({
                    "id": self.reasoning_item_id, "type": "reasoning",
                    "status": "completed",
                    "summary": [{"type": "summary_text", "text": self.reasoning_text}],
                    "content": [{"type": "reasoning_text", "text": self.reasoning_text}],
                })),
                ("tool", Some(idx)) => {
                    let state = &self.tool_states[idx];
                    output.push(json!({
                        "id": state.item_id, "type": "function_call",
                        "status": "completed", "name": state.name,
                        "call_id": state.id, "arguments": state.arguments,
                    }));
                }
                _ => {}
            }
        }
        let mut resp = self.base_response();
        resp["status"] = json!("completed");
        resp["output"] = Value::Array(output);
        resp["usage"] = self.usage.clone().unwrap_or(Value::Null);
        resp
    }
}

