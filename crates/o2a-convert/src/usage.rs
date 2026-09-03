//! usage 语义转换（对齐 `convert.py` 的 `_convert_usage` / `_chat_usage_to_responses` / `_to_int`）。
//!
//! Python 源实现是**混合语义**，必须区分（docs/rust-rewrite.md §5.1）：
//! - **键存在即命中**（dict.get 链，仅键缺失才回退；显式 null/0 不穿透）
//! - **falsy(0) 穿透**（`or` 链，值为 0/None 时继续回退）

use serde_json::{json, Map, Value};

use crate::common::{coerce_int, key, obj_or_empty, truthy};

/// OpenAI usage → Anthropic 语义（对齐 `_convert_usage`）。
pub fn convert_usage(usage: Option<&Value>) -> Value {
    let usage = match usage {
        Some(v) if v.is_object() => v,
        _ => return empty_converted(),
    };
    let u = usage.as_object().unwrap();
    let prompt_details = obj_or_empty(u.get("prompt_tokens_details"));
    let input_details = obj_or_empty(u.get("input_tokens_details"));

    // usage.get("prompt_tokens", usage.get("input_tokens", 0))：键存在即命中
    let prompt_total = match key(u, "prompt_tokens") {
        Some(v) => coerce_int(v, 0),
        None => key(u, "input_tokens").map(|v| coerce_int(v, 0)).unwrap_or(0),
    };
    let output_tokens = match key(u, "completion_tokens") {
        Some(v) => coerce_int(v, 0),
        None => key(u, "output_tokens").map(|v| coerce_int(v, 0)).unwrap_or(0),
    };

    // DeepSeek 顶层命中字段（键存在即命中，无 or 链）
    let ds_cache_hit = key(u, "prompt_cache_hit_tokens")
        .map(|v| coerce_int(v, 0))
        .unwrap_or(0);

    // cached：ds 非零直接用；否则嵌套 get 链（键存在即命中）
    let cached_tokens = if ds_cache_hit != 0 {
        ds_cache_hit
    } else {
        let chain = key_in(prompt_details, "cached_tokens")
            .or_else(|| key_in(prompt_details, "cache_read_input_tokens"))
            .or_else(|| key_in(input_details, "cached_tokens"))
            .or_else(|| key_in(input_details, "cache_read_input_tokens"))
            .or_else(|| key(u, "cache_read_input_tokens"))
            .or_else(|| key(u, "cached_tokens"));
        chain.map(|v| coerce_int(v, 0)).unwrap_or(0)
    };

    // cache write：纯嵌套 get 链（键存在即命中）
    let cache_write_tokens = key_in(prompt_details, "cache_creation_input_tokens")
        .or_else(|| key_in(prompt_details, "cache_write_tokens"))
        .or_else(|| key_in(input_details, "cache_creation_input_tokens"))
        .or_else(|| key_in(input_details, "cache_write_tokens"))
        .or_else(|| key(u, "cache_creation_input_tokens"))
        .or_else(|| key(u, "cache_write_tokens"))
        .map(|v| coerce_int(v, 0))
        .unwrap_or(0);

    // Anthropic 把 cache write 单独报，普通输入要扣除
    let input_tokens = (prompt_total - cached_tokens - cache_write_tokens).max(0);

    // reasoning：or 链（falsy 穿透）
    let reasoning_tokens = match obj_or_empty(u.get("completion_tokens_details"))
        .and_then(|m| key(m, "reasoning_tokens"))
    {
        Some(v) if truthy(v) => coerce_int(v, 0),
        _ => obj_or_empty(u.get("output_tokens_details"))
            .and_then(|m| key(m, "reasoning_tokens"))
            .map(|v| coerce_int(v, 0))
            .unwrap_or(0),
    };

    json!({
        "input_tokens": input_tokens,
        "output_tokens": output_tokens,
        "cache_creation_input_tokens": cache_write_tokens,
        "cache_read_input_tokens": cached_tokens,
        "reasoning_tokens": reasoning_tokens,
        "prompt_total": prompt_total,
    })
}

fn empty_converted() -> Value {
    json!({
        "input_tokens": 0,
        "output_tokens": 0,
        "cache_creation_input_tokens": 0,
        "cache_read_input_tokens": 0,
        "reasoning_tokens": 0,
        "prompt_total": 0,
    })
}

fn key_in<'a>(m: Option<&'a Map<String, Value>>, k: &str) -> Option<&'a Value> {
    m.and_then(|m| m.get(k))
}

/// Chat usage → Responses API usage 格式（对齐 `_chat_usage_to_responses`）。
pub fn chat_usage_to_responses(usage: Option<&Value>) -> Value {
    let usage = match usage {
        Some(v) if v.is_object() => v,
        _ => return empty_responses_usage(0, 0, 0, 0),
    };
    let u = usage.as_object().unwrap();
    // usage.get("prompt_tokens", usage.get("input_tokens", 0))：键存在即命中
    let prompt = match key(u, "prompt_tokens") {
        Some(v) => coerce_int(v, 0),
        None => key(u, "input_tokens").map(|v| coerce_int(v, 0)).unwrap_or(0),
    };
    let completion = match key(u, "completion_tokens") {
        Some(v) => coerce_int(v, 0),
        None => key(u, "output_tokens").map(|v| coerce_int(v, 0)).unwrap_or(0),
    };
    // or 链（falsy 穿透）：DeepSeek 顶层 → prompt details → input details
    let cached = or_chain_int(&[
        key(u, "prompt_cache_hit_tokens"),
        obj_or_empty(u.get("prompt_tokens_details")).and_then(|m| key(m, "cached_tokens")),
        obj_or_empty(u.get("input_tokens_details")).and_then(|m| key(m, "cached_tokens")),
    ]);
    // or 链（falsy 穿透）
    let reasoning = or_chain_int(&[
        obj_or_empty(u.get("completion_tokens_details")).and_then(|m| key(m, "reasoning_tokens")),
        obj_or_empty(u.get("output_tokens_details")).and_then(|m| key(m, "reasoning_tokens")),
    ]);
    json!({
        "input_tokens": prompt,
        "input_tokens_details": {"cached_tokens": cached},
        "output_tokens": completion,
        "output_tokens_details": {"reasoning_tokens": reasoning},
        "total_tokens": prompt + completion,
    })
}

/// `a or b or c` 整数版：第一个 truthy 值的 coerce_int；全 falsy/缺失 → 0。
fn or_chain_int(candidates: &[Option<&Value>]) -> i64 {
    for v in candidates.iter().flatten() {
        if truthy(v) {
            return coerce_int(v, 0);
        }
    }
    0
}

fn empty_responses_usage(prompt: i64, completion: i64, cached: i64, reasoning: i64) -> Value {
    json!({
        "input_tokens": prompt,
        "input_tokens_details": {"cached_tokens": cached},
        "output_tokens": completion,
        "output_tokens_details": {"reasoning_tokens": reasoning},
        "total_tokens": prompt + completion,
    })
}
