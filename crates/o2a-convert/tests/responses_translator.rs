//! `_ResponsesStreamTranslator` 事件序列快照测试（对齐 `_ResponsesStreamTranslator` 逐事件语义）。
//!
//! 覆盖：纯文本流、reasoning+text 交错、tool_calls 分片含无 id 首块、
//! finish_reason 后 usage 尾块（I3 回归）、EOF 无 [DONE] 兜底、completed 幂等。

use o2a_convert::ResponsesStreamTranslator;
use serde_json::{json, Value};

/// 构造 chat chunk。
fn chunk(delta: Value, finish: Option<&str>, usage: Option<Value>) -> Value {
    let mut c = json!({
        "id": "s1", "object": "chat.completion.chunk", "model": "gpt-x",
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

#[test]
fn pure_text_stream_full_sequence() {
    let mut t = ResponsesStreamTranslator::with_identity("gpt-x", "resp_test", 1000);
    let mut all = Vec::new();
    all.extend(t.translate(&chunk(json!({"content": "你"}), None, None)));
    all.extend(t.translate(&chunk(json!({"content": "好"}), None, None)));
    // usage 尾块（choices 空）+ [DONE] → finish
    all.extend(t.translate(&chunk(json!({}), None, Some(json!({"prompt_tokens": 100, "completion_tokens": 20})))));
    all.extend(t.finish());

    assert_eq!(
        types(&all),
        vec![
            "response.created",
            "response.output_item.added",     // message
            "response.content_part.added",
            "response.output_text.delta",     // 你
            "response.output_text.delta",     // 好
            "response.output_text.done",
            "response.content_part.done",
            "response.output_item.done",
            "response.completed",
        ]
    );
    // created 事件形状
    let created = &all[0];
    assert_eq!(created["response"]["id"], json!("resp_test"));
    assert_eq!(created["response"]["created_at"], json!(1000));
    assert_eq!(created["response"]["status"], json!("in_progress"));
    // delta 事件内容
    assert_eq!(all[3]["delta"], json!("你"));
    assert_eq!(all[3]["item_id"], json!("msg_0"));
    assert_eq!(all[3]["output_index"], json!(0));
    // completed：usage 尾块已到达（I3 关键语义）
    let completed = all.last().unwrap();
    assert_eq!(completed["response"]["status"], json!("completed"));
    assert_eq!(completed["response"]["usage"]["input_tokens"], json!(100));
    assert_eq!(completed["response"]["usage"]["output_tokens"], json!(20));
    // output 按交付顺序组装
    let output = completed["response"]["output"].as_array().unwrap();
    assert_eq!(output.len(), 1);
    assert_eq!(output[0]["type"], json!("message"));
    assert_eq!(output[0]["content"][0]["text"], json!("你好"));
}

#[test]
fn reasoning_and_text_interleaved() {
    let mut t = ResponsesStreamTranslator::with_identity("gpt-x", "resp_r", 0);
    let mut all = Vec::new();
    all.extend(t.translate(&chunk(json!({"reasoning_content": "think"}), None, None)));
    all.extend(t.translate(&chunk(json!({"reasoning_content": " more"}), None, None)));
    all.extend(t.translate(&chunk(json!({"content": "answer"}), None, None)));
    all.extend(t.finish());

    assert_eq!(
        types(&all),
        vec![
            "response.created",
            "response.output_item.added",              // reasoning (rs_0)
            "response.reasoning_summary_text.delta",   // think
            "response.reasoning_summary_text.delta",   //  more
            "response.output_item.added",              // message (msg_1)
            "response.content_part.added",
            "response.output_text.delta",
            "response.reasoning_summary_text.done",
            "response.output_item.done",               // reasoning done
            "response.output_text.done",
            "response.content_part.done",
            "response.output_item.done",               // message done
            "response.completed",
        ]
    );
    assert_eq!(all[1]["item"]["id"], json!("rs_0"));
    assert_eq!(all[4]["item"]["id"], json!("msg_1"));
    assert_eq!(all[4]["output_index"], json!(1));
    // completed.output 顺序：reasoning 先于 message
    let completed = all.last().unwrap();
    let output = completed["response"]["output"].as_array().unwrap();
    assert_eq!(output[0]["type"], json!("reasoning"));
    assert_eq!(output[1]["type"], json!("message"));
}

#[test]
fn tool_calls_fragmented_with_orphan_first_block() {
    // 首 chunk 无 id 有 args（orphan）；次 chunk 带 id+name 续块
    let mut t = ResponsesStreamTranslator::with_identity("gpt-x", "resp_t", 0);
    let mut all = Vec::new();
    // 无 id 首块：state 创建 + args 交付（deliver 不要求 id；与 claude 翻译器不同）
    all.extend(t.translate(&chunk(
        json!({"tool_calls": [{"index": 0, "function": {"arguments": "{\"a"}}]}),
        None,
        None,
    )));
    all.extend(t.translate(&chunk(
        // OpenAI 格式：name 在 function 内（Python 读 tc["function"]["name"]）
        json!({"tool_calls": [{"index": 0, "id": "call_1", "function": {"name": "f", "arguments": "\":1}"}}]}),
        None,
        None,
    )));
    all.extend(t.translate(&chunk(
        json!({}),
        Some("tool_calls"),
        Some(json!({"prompt_tokens": 1, "completion_tokens": 2})),
    )));
    all.extend(t.finish());

    let ty = types(&all);
    assert!(ty.contains(&"response.function_call_arguments.delta".to_string()));
    // open 后 delta 两次 + done
    let added_pos = ty.iter().position(|t| *t == "response.output_item.added").unwrap();
    assert_eq!(all[added_pos]["item"]["type"], json!("function_call"));
    let done_positions: Vec<usize> = ty
        .iter()
        .enumerate()
        .filter(|(_, t)| **t == "response.function_call_arguments.done")
        .map(|(i, _)| i)
        .collect();
    assert_eq!(done_positions.len(), 1);
    // 累积参数完整
    assert_eq!(
        all[done_positions[0]]["arguments"],
        json!("{\"a\":1}")
    );
    // finish_reason 触发 close（done 事件先于 completed）
    let completed_pos = ty.iter().position(|t| *t == "response.completed").unwrap();
    assert!(done_positions[0] < completed_pos);
    // completed.usage 非空（finish_reason 后 usage 尾块，I3）
    let completed = &all[completed_pos];
    // Responses 格式 usage 键是 output_tokens（_chat_usage_to_responses 输出，非 completion_tokens）
    assert_eq!(completed["response"]["usage"]["output_tokens"], json!(2));
    // completed.output 仅含 function_call
    let output = completed["response"]["output"].as_array().unwrap();
    assert_eq!(output.len(), 1);
    assert_eq!(output[0]["type"], json!("function_call"));
    assert_eq!(output[0]["name"], json!("f"));
}

#[test]
fn eof_without_done_still_completes() {
    // 仅内容 + EOF（无 finish_reason 无 [DONE]）→ completed 仍存在（I2 极端）
    let mut t = ResponsesStreamTranslator::with_identity("gpt-x", "resp_e", 0);
    let mut all = Vec::new();
    all.extend(t.translate(&chunk(json!({"content": "hi"}), None, None)));
    all.extend(t.finish());
    let ty = types(&all);
    assert!(ty.contains(&"response.completed".to_string()));
    // completed 无 usage 时为 null（Python assemble usage=None → null）
    let completed = all.last().unwrap();
    assert!(completed["response"]["usage"].is_null());
}

#[test]
fn complete_idempotent_and_close_idempotent() {
    let mut t = ResponsesStreamTranslator::with_identity("gpt-x", "resp_i", 0);
    let _ = t.translate(&chunk(json!({"content": "x"}), None, None));
    // finish_reason 触发 close
    let _ = t.translate(&chunk(json!({}), Some("stop"), None));
    let first = t.finish();
    assert_eq!(first.len(), 1); // 仅 completed（done 事件已被 close 发过）
    let second = t.finish();
    assert!(second.is_empty()); // 幂等：无重复 completed
}

#[test]
fn done_event_content_matches_python_shape() {
    // output_text.done / content_part.done / output_item.done 的形状逐字段对齐
    let mut t = ResponsesStreamTranslator::with_identity("gpt-x", "resp_s", 0);
    let _ = t.translate(&chunk(json!({"content": "abc"}), None, None));
    let all = t.finish();
    let ty = types(&all);
    let text_done = all[ty.iter().position(|t| *t == "response.output_text.done").unwrap()].clone();
    assert_eq!(text_done["text"], json!("abc"));
    assert_eq!(text_done["content_index"], json!(0));
    let part_done = all[ty.iter().position(|t| *t == "response.content_part.done").unwrap()].clone();
    assert_eq!(part_done["part"]["text"], json!("abc"));
    let item_done = all[ty.iter().position(|t| *t == "response.output_item.done").unwrap()].clone();
    assert_eq!(item_done["item"]["status"], json!("completed"));
    assert_eq!(item_done["item"]["role"], json!("assistant"));
}

#[test]
fn usage_tail_after_finish_reason_lands_in_completed() {
    // I3 回归：finish_reason 块之后 usage 尾块（choices 空）→ completed.usage 非空
    let mut t = ResponsesStreamTranslator::with_identity("gpt-x", "resp_u", 0);
    let _ = t.translate(&chunk(json!({"content": "x"}), None, None));
    let _ = t.translate(&chunk(json!({}), Some("stop"), None));
    // usage 尾块（choices 空）：仅更新 usage，无事件
    let evs = t.translate(&chunk(json!({}), None, Some(json!({"prompt_tokens": 100, "completion_tokens": 20}))));
    assert!(evs.is_empty());
    let all = t.finish();
    let completed = all.last().unwrap();
    assert_eq!(completed["response"]["usage"]["input_tokens"], json!(100));
    assert_eq!(completed["response"]["usage"]["output_tokens"], json!(20));
}

#[test]
fn multi_tool_calls_ordering() {
    // 多工具并行分片：按 output_index 交付，completed.output 按交付顺序
    let mut t = ResponsesStreamTranslator::with_identity("gpt-x", "resp_m", 0);
    let mut all = Vec::new();
    all.extend(t.translate(&chunk(
        json!({"tool_calls": [
            {"index": 0, "id": "c1", "name": "f1", "function": {"arguments": "{}"}},
            {"index": 1, "id": "c2", "name": "f2", "function": {"arguments": "{}"}},
        ]}),
        None,
        None,
    )));
    all.extend(t.finish());
    let completed = all.last().unwrap();
    let output = completed["response"]["output"].as_array().unwrap();
    assert_eq!(output.len(), 2);
    assert_eq!(output[0]["call_id"], json!("c1"));
    assert_eq!(output[1]["call_id"], json!("c2"));
    // output_index 不重叠
    let added: Vec<Value> = all
        .iter()
        .filter(|e| e["type"] == json!("response.output_item.added"))
        .cloned()
        .collect();
    assert_eq!(added[0]["output_index"], json!(0));
    assert_eq!(added[1]["output_index"], json!(1));
}
