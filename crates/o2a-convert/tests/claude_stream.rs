//! claude 模式流式状态机（`ChunkTranslator`）事件序列快照测试。
//!
//! 对齐 `engine.handle_claude_stream` 的关键边界：
//! - 块切换（thinking → text → tool_use）与 index 递增
//! - thinking 块不因空 reasoning_content 提前关闭
//! - tool_calls orphan id 首块缓冲
//! - finish_reason 只关块；[DONE] / EOF / usage 尾块三路收尾幂等
//! - timeout 终止形态（stop_reason=max_tokens、message_stop 无条件发送）

use o2a_convert::{classify_final, ChunkTranslator};
use serde_json::{json, Value};

fn chunk(delta: Value, finish: Option<&str>, usage: Option<Value>) -> Value {
    let mut c = json!({
        "id": "chatcmpl-1", "object": "chat.completion.chunk", "model": "gpt-x",
        "choices": [{"index": 0, "delta": delta}],
    });
    if let Some(f) = finish {
        c["choices"][0]["finish_reason"] = json!(f);
    }
    if let Some(u) = usage {
        c["usage"] = u;
    }
    c
}

fn types(events: &[Value]) -> Vec<String> {
    events
        .iter()
        .map(|e| e["type"].as_str().unwrap_or("").to_string())
        .collect()
}

/// usage 尾块（真实上游形态：choices 为空 + usage）。对齐 Python
/// `handle_claude_stream` 中 `if not choices:` 分支的输入形状。
fn usage_tail_chunk(usage: Value) -> Value {
    json!({
        "id": "chatcmpl-1", "object": "chat.completion.chunk", "model": "gpt-x",
        "choices": [], "usage": usage,
    })
}

#[test]
fn pure_text_stream_with_done() {
    let mut t = ChunkTranslator::new("gpt-x");
    let e1 = t.on_chunk(&chunk(json!({"content": "你"}), None, None));
    let e2 = t.on_chunk(&chunk(json!({"content": "好"}), None, None));
    let e3 = t.on_done();

    // message_start 首事件
    let start = &e1[0];
    assert_eq!(start["type"], json!("message_start"));
    assert_eq!(start["message"]["id"], json!("chatcmpl-1"));
    assert_eq!(start["message"]["model"], json!("gpt-x"));
    assert_eq!(start["message"]["usage"]["output_tokens"], json!(0));
    assert_eq!(e1[1], json!({
        "type": "content_block_start",
        "index": 0,
        "content_block": {"type": "text", "text": ""},
    }));
    assert_eq!(
        e1[2],
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "你"}})
    );
    assert_eq!(
        e2[0],
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "好"}})
    );
    // [DONE]：关块 → message_delta(end_turn) → message_stop
    assert_eq!(
        types(&e3),
        vec!["content_block_stop", "message_delta", "message_stop"]
    );
    assert_eq!(e3[1]["delta"]["stop_reason"], json!("end_turn"));
    assert_eq!(e3[1]["delta"]["stop_sequence"], json!(null));
    assert!(t.is_finished());
}

#[test]
fn finish_reason_then_usage_tail_then_eof() {
    // I1 回归：内容 → finish_reason 块 → usage 尾块（choices 空）→ EOF 无 [DONE]
    let mut t = ChunkTranslator::new("gpt-x");
    let _ = t.on_chunk(&chunk(json!({"content": "你"}), None, None));
    // finish_reason：只关块，不发 message_delta
    let e2 = t.on_chunk(&chunk(json!({}), Some("stop"), None));
    assert_eq!(types(&e2), vec!["content_block_stop"]);
    assert!(!t.is_finished());
    // usage 尾块（choices 空）+ pending_finish_reason → message_delta + message_stop
    let e3 = t.on_chunk(&usage_tail_chunk(json!({"prompt_tokens": 100, "completion_tokens": 20})));
    assert_eq!(types(&e3), vec!["message_delta", "message_stop"]);
    assert_eq!(e3[0]["usage"]["input_tokens"], json!(100));
    assert_eq!(e3[0]["usage"]["output_tokens"], json!(20));
    assert!(t.is_finished());
    // EOF：已完成，无重复
    assert!(t.on_eof().is_empty());
}

#[test]
fn content_only_eof_no_finish() {
    // I1 极端：仅内容 + EOF，无 finish_reason 无 [DONE] → message_stop 存在
    let mut t = ChunkTranslator::new("gpt-x");
    let _ = t.on_chunk(&chunk(json!({"content": "你"}), None, None));
    let _ = t.on_chunk(&chunk(json!({"content": "好"}), None, None));
    let e = t.on_eof();
    assert_eq!(
        types(&e),
        vec!["content_block_stop", "message_delta", "message_stop"]
    );
    assert_eq!(e[1]["delta"]["stop_reason"], json!("end_turn"));
    assert!(t.is_finished());
}

#[test]
fn reasoning_then_text_block_switch() {
    let mut t = ChunkTranslator::new("gpt-x");
    let mut all = Vec::new();
    all.extend(t.on_chunk(&chunk(json!({"reasoning_content": "think"}), None, None)));
    all.extend(t.on_chunk(&chunk(json!({"content": "answer"}), None, None)));
    all.extend(t.on_done());

    assert_eq!(all[1], json!({
        "type": "content_block_start",
        "index": 0,
        "content_block": {"type": "thinking", "thinking": ""},
    }));
    // thinking delta
    assert_eq!(
        all[2]["delta"],
        json!({"type": "thinking_delta", "thinking": "think"})
    );
    // text 到来：关 thinking(idx 0) → 开 text(idx 1)
    assert_eq!(all[3], json!({"type": "content_block_stop", "index": 0}));
    assert_eq!(all[4], json!({
        "type": "content_block_start",
        "index": 1,
        "content_block": {"type": "text", "text": ""},
    }));
    // 收尾：关 text 块 → delta → stop
    let tail_types = types(&all[all.len() - 3..]);
    assert_eq!(tail_types, vec!["content_block_stop", "message_delta", "message_stop"]);
}

#[test]
fn empty_reasoning_does_not_split_thinking_block() {
    // 分段思考间的空/null reasoning_content 不拆块（不提前关闭）
    let mut t = ChunkTranslator::new("gpt-x");
    let mut all = Vec::new();
    all.extend(t.on_chunk(&chunk(json!({"reasoning_content": "part1"}), None, None)));
    all.extend(t.on_chunk(&chunk(json!({"reasoning_content": ""}), None, None)));
    all.extend(t.on_chunk(&chunk(json!({"reasoning_content": "part2"}), None, None)));
    all.extend(t.on_done());

    let starts: Vec<&Value> = all
        .iter()
        .filter(|e| e["type"] == json!("content_block_start"))
        .collect();
    assert_eq!(starts.len(), 1, "thinking 块只开一次");
    let deltas: Vec<&Value> = all
        .iter()
        .filter(|e| e["type"] == json!("content_block_delta"))
        .collect();
    assert_eq!(deltas.len(), 2); // part1 + part2
}

#[test]
fn tool_calls_orphan_args_buffered_until_id() {
    // 无 id 首块：参数缓冲不发事件；id 块到达：合并开块并一次性发 delta
    let mut t = ChunkTranslator::new("gpt-x");
    let e1 = t.on_chunk(&chunk(
        json!({"tool_calls": [{"index": 0, "function": {"arguments": "{\"a"}}]}),
        None,
        None,
    ));
    // Python：首个带非空 choices 的 chunk 无条件发 message_start（无论有无内容），
    // 之后才处理 tool_calls。无 id 首块不应产生 content_block 事件。
    assert_eq!(types(&e1), vec!["message_start"], "无 id 首块只发 message_start，不开块");

    let e2 = t.on_chunk(&chunk(
        // OpenAI 格式：name 在 function 内（Python 读 tc["function"]["name"]）
        json!({"tool_calls": [{"index": 0, "id": "t1", "function": {"name": "f", "arguments": "\":1}"}}]}),
        None,
        None,
    ));
    assert_eq!(
        types(&e2),
        vec!["content_block_start", "content_block_delta"]
    );
    assert_eq!(e2[0]["content_block"]["type"], json!("tool_use"));
    assert_eq!(e2[0]["content_block"]["id"], json!("t1"));
    assert_eq!(e2[0]["content_block"]["name"], json!("f"));
    // 合并 orphan 参数
    assert_eq!(e2[1]["delta"]["partial_json"], json!("{\"a\":1}"));
    assert!(t.had_tool_calls());

    // [DONE] 收尾：stop_reason = tool_use
    let e3 = t.on_done();
    assert_eq!(e3[1]["delta"]["stop_reason"], json!("tool_use"));
}

#[test]
fn tool_calls_args_before_id_then_id_block_merges() {
    // 多段 orphan 参数累计
    let mut t = ChunkTranslator::new("gpt-x");
    let _ = t.on_chunk(&chunk(
        json!({"tool_calls": [{"index": 0, "function": {"arguments": "{\"a"}}]}),
        None, None,
    ));
    let _ = t.on_chunk(&chunk(
        json!({"tool_calls": [{"index": 0, "function": {"arguments": ":2,"}}]}),
        None, None,
    ));
    let e = t.on_chunk(&chunk(
        json!({"tool_calls": [{"index": 0, "id": "t1", "name": "f", "function": {"arguments": "\"b\":3}"}}]}),
        None, None,
    ));
    let delta = e.iter().find(|e| e["type"] == json!("content_block_delta")).unwrap();
    assert_eq!(delta["delta"]["partial_json"], json!("{\"a:2,\"b\":3}"));
}

#[test]
fn tool_fragmented_stream_appends_args() {
    // 带 id 开块后续块追加参数，不重复开块
    let mut t = ChunkTranslator::new("gpt-x");
    let _ = t.on_chunk(&chunk(
        json!({"tool_calls": [{"index": 0, "id": "t1", "name": "f", "function": {"arguments": "{\"x\":"}}]}),
        None, None,
    ));
    let e2 = t.on_chunk(&chunk(
        json!({"tool_calls": [{"index": 0, "function": {"arguments": "1}"}}]}),
        None, None,
    ));
    assert_eq!(types(&e2), vec!["content_block_delta"]);
    assert_eq!(e2[0]["index"], json!(0));
    assert_eq!(e2[0]["delta"]["partial_json"], json!("1}"));

    // 重复带 id 的续块也不重开
    let e3 = t.on_chunk(&chunk(
        json!({"tool_calls": [{"index": 0, "id": "t1", "name": "f", "function": {"arguments": ""}}]}),
        None, None,
    ));
    assert!(e3.is_empty());

    let e4 = t.on_done();
    assert_eq!(e4[1]["delta"]["stop_reason"], json!("tool_use"));
}

#[test]
fn reasoning_text_tool_block_index_progression() {
    // thinking(0) → text(1) → tool_use(2)：每类块都正确递增
    let mut t = ChunkTranslator::new("gpt-x");
    let mut all = Vec::new();
    all.extend(t.on_chunk(&chunk(json!({"reasoning_content": "r"}), None, None)));
    all.extend(t.on_chunk(&chunk(json!({"content": "c"}), None, None)));
    all.extend(t.on_chunk(&chunk(
        json!({"tool_calls": [{"index": 0, "id": "t1", "name": "f", "function": {"arguments": "{}"}}]}),
        None, None,
    )));
    all.extend(t.on_done());
    let starts: Vec<&Value> = all
        .iter()
        .filter(|e| e["type"] == json!("content_block_start"))
        .collect();
    assert_eq!(starts[0]["index"], json!(0));
    assert_eq!(starts[0]["content_block"]["type"], json!("thinking"));
    assert_eq!(starts[1]["index"], json!(1));
    assert_eq!(starts[1]["content_block"]["type"], json!("text"));
    assert_eq!(starts[2]["index"], json!(2));
    assert_eq!(starts[2]["content_block"]["type"], json!("tool_use"));
}

#[test]
fn message_start_usage_prefilled() {
    // 首 chunk 自带 usage：message_start 的 usage 已填充
    let mut t = ChunkTranslator::new("gpt-x");
    let e = t.on_chunk(&chunk(
        json!({"content": "x"}),
        None,
        Some(json!({"prompt_tokens": 50, "completion_tokens": 0,
                     "prompt_tokens_details": {"cached_tokens": 30}})),
    ));
    let start = &e[0];
    assert_eq!(start["message"]["usage"]["input_tokens"], json!(20)); // 50-30
    assert_eq!(start["message"]["usage"]["cache_read_input_tokens"], json!(30));
}

#[test]
fn timeout_termination_shape() {
    // 超时终止：stop_reason=max_tokens（无 stop_sequence 键）、message_stop 无条件发送
    let mut t = ChunkTranslator::new("gpt-x");
    let _ = t.on_chunk(&chunk(json!({"content": "partial"}), None, None));
    let e = t.on_timeout();
    assert_eq!(
        types(&e),
        vec!["content_block_stop", "message_delta", "message_stop"]
    );
    assert_eq!(e[1]["delta"], json!({"stop_reason": "max_tokens"}));
    assert!(t.is_finished());

    // 未开始也发 message_stop（对齐 Python 无条件写 message_stop）
    let mut t2 = ChunkTranslator::new("gpt-x");
    let e2 = t2.on_timeout();
    assert_eq!(types(&e2), vec!["message_stop"]);
}

#[test]
fn done_idempotent() {
    let mut t = ChunkTranslator::new("gpt-x");
    let _ = t.on_chunk(&chunk(json!({"content": "x"}), None, None));
    let first = t.on_done();
    assert!(!first.is_empty());
    assert!(t.on_done().is_empty());
    assert!(t.on_eof().is_empty());
}

#[test]
fn finish_reason_after_tool_stop_reason_tool_use() {
    let mut t = ChunkTranslator::new("gpt-x");
    let _ = t.on_chunk(&chunk(
        json!({"tool_calls": [{"index": 0, "id": "t1", "name": "f", "function": {"arguments": "{}"}}]}),
        None, None,
    ));
    let _ = t.on_chunk(&chunk(json!({}), Some("tool_calls"), None));
    // finish_reason 已关块（实现与 Python 一致）：[DONE] 时不重复关块，
    // 事件序列为 [message_delta, message_stop]（e[0] 即 message_delta）。
    let e = t.on_done();
    assert_eq!(types(&e), vec!["message_delta", "message_stop"]);
    assert_eq!(e[0]["delta"]["stop_reason"], json!("tool_use"));
}

#[test]
fn length_finish_maps_to_max_tokens() {
    let mut t = ChunkTranslator::new("gpt-x");
    let _ = t.on_chunk(&chunk(json!({"content": "x"}), None, None));
    let _ = t.on_chunk(&chunk(json!({}), Some("length"), None));
    // finish_reason 已关块：[DONE] 序列为 [message_delta, message_stop]
    let e = t.on_done();
    assert_eq!(e[0]["delta"]["stop_reason"], json!("max_tokens"));
    assert!(!t.is_final_answer()); // length → 非最终答复
}

#[test]
fn classify_final_matrix() {
    assert!(classify_final(Some("stop"), false));
    assert!(classify_final(None, false));
    assert!(classify_final(Some("end_turn"), false));
    assert!(!classify_final(Some("tool_calls"), false));
    assert!(!classify_final(Some("stop"), true));
    assert!(!classify_final(Some("length"), false));
    assert!(!classify_final(Some("max_tokens"), false));
}

#[test]
fn unknown_finish_reason_passthrough() {
    let mut t = ChunkTranslator::new("gpt-x");
    let _ = t.on_chunk(&chunk(json!({"content": "x"}), None, None));
    let _ = t.on_chunk(&chunk(json!({}), Some("weird_reason"), None));
    // finish_reason 已关块：[DONE] 序列为 [message_delta, message_stop]；
    // 未知 finish_reason 原样透传（_anthropic_stop_reason 兼底分支）
    let e = t.on_done();
    assert_eq!(e[0]["delta"]["stop_reason"], json!("weird_reason"));
}

#[test]
fn model_updated_from_first_chunk() {
    let mut t = ChunkTranslator::new("qwen-plus");
    let _ = t.on_chunk(&chunk(json!({"content": "x"}), None, None));
    assert_eq!(t.model(), "gpt-x"); // chunk 的 model 覆盖服务模型
}

