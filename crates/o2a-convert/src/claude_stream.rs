//! claude 模式流式状态机（对齐 `engine.handle_claude_stream` 的纯函数抽取）。
//!
//! 输入：上游 Chat Completions SSE 的 `data:` 载荷（单个 chunk JSON）。
//! 输出：Anthropic Messages SSE 事件 JSON 列表（engine 逐个经 [`crate::sse_event`] 写出）。
//!
//! 事件骨架：message_start → content_block_start/delta/stop（thinking/text/tool_use，
//! index 递增）→ message_delta{stop_reason, usage} → message_stop。
//!
//! 关键边界行为（全部复刻）：
//! - thinking 块不因空/null reasoning_content 提前关闭（分段思考间空块保持同一块）
//! - tool_calls 按 index 聚合；无 id 首块参数缓冲（orphan args），id 块到达时合并开块
//! - finish_reason 只关块不发 message_delta（等 usage 尾块）
//! - [DONE] / EOF / timeout 多路幂等收尾，防重复 message_stop

use serde_json::{json, Value};
use std::collections::BTreeMap;

use crate::common::{anthropic_stop_reason, coerce_int, truthy};
use crate::usage::convert_usage;

/// finish_reason → 是否"最终答复"（对齐 `engine._classify`）。
pub fn classify_final(finish_reason: Option<&str>, has_tool_call: bool) -> bool {
    if has_tool_call || matches!(finish_reason, Some("tool_calls") | Some("tool_use")) {
        return false;
    }
    if matches!(finish_reason, Some("length") | Some("max_tokens")) {
        return false;
    }
    true // stop / end_turn / None / 其它
}

#[derive(Clone)]
struct ToolBuf {
    /// Python 缓冲里的 id 字段从不回读（开块时直接用 tc_id）；保留以对齐源结构
    #[allow(dead_code)]
    id: String,
    name: String,
    input_str: String,
}

/// claude 模式流式翻译器：把 Chat chunk 流翻译为 Anthropic SSE 事件序列。
pub struct ChunkTranslator {
    /// 初始/当前模型（message_start 时取 chunk 的 model 覆盖）
    model: String,
    message_id: String,
    started: bool,
    finished: bool,
    latest_usage: Value,
    content_block_idx: i64,
    content_block_open: bool,
    thinking_block_open: bool,
    pending_finish_reason: Option<String>,
    /// block_idx → 工具块缓冲（BTreeMap：块号严格递增，插入序 == 键序）
    tool_input_buf: BTreeMap<i64, ToolBuf>,
    /// 上游 tool_call index → 内容块 index
    tool_index_to_block_index: BTreeMap<i64, i64>,
    /// 无 id 首块的参数缓冲（等 id 块到达再开块）
    pending_orphan_args: BTreeMap<i64, String>,
    had_tool_calls: bool,
}

impl ChunkTranslator {
    pub fn new(service_model: &str) -> Self {
        Self {
            model: service_model.to_string(),
            message_id: "proxy-msg-stream".to_string(),
            started: false,
            finished: false,
            latest_usage: json!({
                "input_tokens": 0,
                "output_tokens": 0,
                "cache_creation_input_tokens": 0,
                "cache_read_input_tokens": 0,
                "reasoning_tokens": 0,
            }),
            content_block_idx: 0,
            content_block_open: false,
            thinking_block_open: false,
            pending_finish_reason: None,
            tool_input_buf: BTreeMap::new(),
            tool_index_to_block_index: BTreeMap::new(),
            pending_orphan_args: BTreeMap::new(),
            had_tool_calls: false,
        }
    }

    pub fn is_finished(&self) -> bool {
        self.finished
    }

    pub fn is_started(&self) -> bool {
        self.started
    }

    /// 当前模型（message_start 时被 chunk 的 model 更新；统计/日志用）。
    pub fn model(&self) -> &str {
        &self.model
    }

    /// 最新 usage（统计用）。
    pub fn usage(&self) -> &Value {
        &self.latest_usage
    }

    pub fn had_tool_calls(&self) -> bool {
        self.had_tool_calls
    }

    /// 是否"最终答复"（`_task_finish` 用：`_classify(pending_finish_reason, had_tool_calls)`）。
    pub fn is_final_answer(&self) -> bool {
        classify_final(self.pending_finish_reason.as_deref(), self.had_tool_calls)
    }

    fn message_delta_event(&self, delta: Value) -> Value {
        json!({
            "type": "message_delta",
            "delta": delta,
            "usage": self.latest_usage,
        })
    }

    fn stop_reason(&self) -> String {
        anthropic_stop_reason(self.pending_finish_reason.as_deref(), self.had_tool_calls)
    }

    /// 关闭全部开着的块（thinking → tools → text），返回 stop 事件列表。
    fn close_all_blocks(&mut self) -> Vec<Value> {
        let mut events = Vec::new();
        if self.thinking_block_open {
            events.push(json!({
                "type": "content_block_stop",
                "index": self.content_block_idx,
            }));
            self.thinking_block_open = false;
        }
        let block_indices: Vec<i64> = self.tool_input_buf.keys().copied().collect();
        for idx in block_indices {
            events.push(json!({
                "type": "content_block_stop",
                "index": idx,
            }));
        }
        self.tool_input_buf.clear();
        if self.content_block_open {
            events.push(json!({
                "type": "content_block_stop",
                "index": self.content_block_idx,
            }));
            self.content_block_open = false;
        }
        events
    }

    /// 处理一个上游 chunk（`data:` 载荷 JSON），返回要写给客户端的事件列表。
    /// `[DONE]` 哨兵不由本方法处理（引擎检测到后调用 [`Self::on_done`]）。
    pub fn on_chunk(&mut self, chunk: &Value) -> Vec<Value> {
        let mut events: Vec<Value> = Vec::new();
        let Some(obj) = chunk.as_object() else {
            return events;
        };

        // usage 提取（先于 choices 判空；对齐 Python 循环内顺序）
        if let Some(u) = obj.get("usage") {
            if truthy(u) {
                let converted = convert_usage(Some(u));
                self.latest_usage = json!({
                    "input_tokens": converted["input_tokens"],
                    // Python: output_tokens = converted or output_tokens（falsy 保留旧值）
                    "output_tokens": if converted["output_tokens"].as_i64().unwrap_or(0) != 0 {
                        converted["output_tokens"].clone()
                    } else {
                        self.latest_usage["output_tokens"].clone()
                    },
                    "cache_creation_input_tokens": converted["cache_creation_input_tokens"],
                    "cache_read_input_tokens": converted["cache_read_input_tokens"],
                    "reasoning_tokens": converted["reasoning_tokens"],
                });
            }
        }

        let choices = match obj.get("choices").and_then(|c| c.as_array()) {
            Some(c) => c,
            None => {
                // choices 缺失/非数组：Python 走 `if not choices` 分支
                return self.on_choices_empty(events);
            }
        };
        if choices.is_empty() {
            // 最后一个 chunk（choices 为空）带 usage，发送结束事件
            return self.on_choices_empty(events);
        }

        let choice = &choices[0];
        let delta = choice.get("delta").cloned().unwrap_or(json!({}));
        let finish_reason = choice
            .get("finish_reason")
            .filter(|v| !v.is_null())
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        if !self.started {
            self.started = true;
            if let Some(id) = obj.get("id").and_then(|v| v.as_str()) {
                if !id.is_empty() {
                    self.message_id = id.to_string();
                }
            }
            if let Some(m) = obj.get("model").and_then(|v| v.as_str()) {
                if !m.is_empty() {
                    self.model = m.to_string();
                }
            }
            events.push(json!({
                "type": "message_start",
                "message": {
                    "id": self.message_id,
                    "type": "message",
                    "role": "assistant",
                    "content": [],
                    "model": self.model,
                    "stop_reason": null,
                    "stop_sequence": null,
                    "usage": {
                        "input_tokens": self.latest_usage["input_tokens"],
                        "output_tokens": 0,
                        "cache_creation_input_tokens": self.latest_usage["cache_creation_input_tokens"],
                        "cache_read_input_tokens": self.latest_usage["cache_read_input_tokens"],
                        "reasoning_tokens": self.latest_usage["reasoning_tokens"],
                    },
                },
            }));
        }

        // 思考内容（reasoning_content → thinking block）。
        // 注意：不在此处提前关闭 thinking 块——部分网关在分段思考之间会发空/null 的
        // reasoning_content，提前关闭会把一个块拆成多个。
        if let Some(rc) = delta.get("reasoning_content").and_then(|v| v.as_str()) {
            if !rc.is_empty() {
                if self.content_block_open {
                    events.push(json!({
                        "type": "content_block_stop",
                        "index": self.content_block_idx,
                    }));
                    self.content_block_open = false;
                    self.content_block_idx += 1;
                }
                if !self.thinking_block_open {
                    events.push(json!({
                        "type": "content_block_start",
                        "index": self.content_block_idx,
                        "content_block": {"type": "thinking", "thinking": ""},
                    }));
                    self.thinking_block_open = true;
                }
                events.push(json!({
                    "type": "content_block_delta",
                    "index": self.content_block_idx,
                    "delta": {"type": "thinking_delta", "thinking": rc},
                }));
            }
        }

        // 文本内容
        if let Some(c) = delta.get("content").and_then(|v| v.as_str()) {
            if !c.is_empty() {
                if self.thinking_block_open {
                    events.push(json!({
                        "type": "content_block_stop",
                        "index": self.content_block_idx,
                    }));
                    self.thinking_block_open = false;
                    self.content_block_idx += 1;
                }
                if !self.content_block_open {
                    events.push(json!({
                        "type": "content_block_start",
                        "index": self.content_block_idx,
                        "content_block": {"type": "text", "text": ""},
                    }));
                    self.content_block_open = true;
                }
                events.push(json!({
                    "type": "content_block_delta",
                    "index": self.content_block_idx,
                    "delta": {"type": "text_delta", "text": c},
                }));
            }
        }

        // tool_calls 分片
        if let Some(tcs) = delta.get("tool_calls").and_then(|v| v.as_array()) {
            for tc in tcs {
                self.on_tool_call(tc, &mut events);
            }
        }

        if let Some(fr) = finish_reason {
            // 只关块不发 message_delta：等 usage 尾块（标准顺序在 finish_reason 之后）。
            // Python 的 `if finish_reason and not finished:` 是 truthy 检查：空串不触发；
            // 收尾（usage 尾块/[DONE]）后到达的 finish_reason 同样被忽略。
            if !fr.is_empty() && !self.finished {
                self.pending_finish_reason = Some(fr);
                events.extend(self.close_all_blocks());
            }
        }
        events
    }

    fn on_choices_empty(&mut self, mut events: Vec<Value>) -> Vec<Value> {
        if self.pending_finish_reason.is_some()
            && self
                .pending_finish_reason
                .as_deref()
                .map(|s| !s.is_empty())
                .unwrap_or(false)
            && !self.finished
        {
            let stop = self.stop_reason();
            events.push(self.message_delta_event(json!({
                "stop_reason": stop,
                "stop_sequence": null,
            })));
            events.push(json!({"type": "message_stop"}));
            self.finished = true;
            // 统计由 engine 在消费完事件后调用（此处返回 finished 状态即可）
        }
        events
    }

    fn on_tool_call(&mut self, tc: &Value, events: &mut Vec<Value>) {
        let Some(tco) = tc.as_object() else {
            return;
        };
        // Python: int(tc.get("index", len(tool_index_to_block_index)))
        let tool_call_index = match tco.get("index") {
            Some(v) if !v.is_null() => coerce_int(v, self.tool_index_to_block_index.len() as i64),
            _ => self.tool_index_to_block_index.len() as i64,
        };
        let tc_id = tco.get("id").and_then(|v| v.as_str()).unwrap_or("");
        let fn_obj = tco.get("function").cloned().unwrap_or(json!({}));
        let tc_name = fn_obj.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let tc_args = fn_obj.get("arguments").and_then(|v| v.as_str()).unwrap_or("");

        match self.tool_index_to_block_index.get(&tool_call_index).copied() {
            None => {
                if !tc_id.is_empty() {
                    // 新的 tool_use 开始（id 首块）
                    if self.thinking_block_open {
                        events.push(json!({
                            "type": "content_block_stop",
                            "index": self.content_block_idx,
                        }));
                        self.thinking_block_open = false;
                        self.content_block_idx += 1;
                    }
                    if self.content_block_open {
                        events.push(json!({
                            "type": "content_block_stop",
                            "index": self.content_block_idx,
                        }));
                        self.content_block_open = false;
                        self.content_block_idx += 1;
                    }
                    let block_idx = self.content_block_idx;
                    self.tool_index_to_block_index
                        .insert(tool_call_index, block_idx);
                    self.had_tool_calls = true;
                    // 无 id 首块可能已缓冲部分参数（pending_orphan_args），合并后开块
                    let buffered = self
                        .pending_orphan_args
                        .remove(&tool_call_index)
                        .unwrap_or_default();
                    let input_str = format!("{buffered}{tc_args}");
                    self.tool_input_buf.insert(
                        block_idx,
                        ToolBuf {
                            id: tc_id.to_string(),
                            name: tc_name.to_string(),
                            input_str: input_str.clone(),
                        },
                    );
                    events.push(json!({
                        "type": "content_block_start",
                        "index": block_idx,
                        "content_block": {
                            "type": "tool_use",
                            "id": tc_id,
                            "name": tc_name,
                            "input": {},
                        },
                    }));
                    self.content_block_idx += 1;
                    if !input_str.is_empty() {
                        events.push(json!({
                            "type": "content_block_delta",
                            "index": block_idx,
                            "delta": {"type": "input_json_delta", "partial_json": input_str},
                        }));
                    }
                } else {
                    // 尚无 id：缓冲参数，等 id 块到达再开块（避免孤儿空 id 块）
                    if !tc_args.is_empty() {
                        let entry = self
                            .pending_orphan_args
                            .entry(tool_call_index)
                            .or_default();
                        entry.push_str(tc_args);
                    }
                }
            }
            Some(idx) => {
                // 续块（无 id，或网关每块重复带 id）：追加参数到已有块，不重复开块
                if !tc_args.is_empty() {
                    if let Some(buf) = self.tool_input_buf.get_mut(&idx) {
                        buf.input_str.push_str(tc_args);
                        self.had_tool_calls = true;
                        events.push(json!({
                            "type": "content_block_delta",
                            "index": idx,
                            "delta": {"type": "input_json_delta", "partial_json": tc_args},
                        }));
                        if buf.name.is_empty() && !tc_name.is_empty() {
                            buf.name = tc_name.to_string();
                        }
                    }
                    // 块缓冲已被收尾清空（finish_reason 后到达的续块）：Python 会 KeyError
                    // 进流内 error 事件；此处静默跳过（见 lib.rs 已知差异）
                } else if let Some(buf) = self.tool_input_buf.get_mut(&idx) {
                    if buf.name.is_empty() && !tc_name.is_empty() {
                        buf.name = tc_name.to_string();
                    }
                }
            }
        }
    }

    /// 上游发送 `[DONE]`（对齐 Python `[DONE]` 分支：未开始则不收尾）。
    pub fn on_done(&mut self) -> Vec<Value> {
        let mut events = Vec::new();
        if !self.finished && self.started {
            events.extend(self.close_all_blocks());
            let stop = self.stop_reason();
            events.push(self.message_delta_event(json!({
                "stop_reason": stop,
                "stop_sequence": null,
            })));
            events.push(json!({"type": "message_stop"}));
            self.finished = true;
        }
        events
    }

    /// 上游 EOF 且未收到 `[DONE]`：补发终止事件，避免客户端挂起。
    pub fn on_eof(&mut self) -> Vec<Value> {
        let mut events = Vec::new();
        if !self.finished {
            events.extend(self.close_all_blocks());
            if self.started {
                let stop = self.stop_reason();
                events.push(self.message_delta_event(json!({
                    "stop_reason": stop,
                    "stop_sequence": null,
                })));
                events.push(json!({"type": "message_stop"}));
                self.finished = true;
            }
        }
        events
    }

    /// 流式总超时（STREAM_TIMEOUT 无进展）：与 [DONE]/EOF 不同的终止形态——
    /// stop_reason 固定 max_tokens（无 stop_sequence 键），message_stop 无条件发送。
    pub fn on_timeout(&mut self) -> Vec<Value> {
        let mut events = Vec::new();
        events.extend(self.close_all_blocks());
        if self.started {
            events.push(self.message_delta_event(json!({"stop_reason": "max_tokens"})));
        }
        events.push(json!({"type": "message_stop"}));
        self.finished = true;
        events
    }
}
