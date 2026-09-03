//! `_convert_usage` / `_chat_usage_to_responses` 混合 fallback 语义表驱动测试。
//!
//! 核心区分（docs/rust-rewrite.md §5.1）：
//! - 键存在即命中（dict.get 链）：显式 null/0 不穿透
//! - falsy(0) 穿透（or 链）：DeepSeek 顶层命中字段、reasoning_tokens

use o2a_convert::{chat_usage_to_responses, convert_usage};
use serde_json::json;

#[test]
fn openai_standard_fields() {
    let out = convert_usage(Some(&json!({
        "prompt_tokens": 100, "completion_tokens": 20, "total_tokens": 120,
    })));
    assert_eq!(out["input_tokens"], json!(100));
    assert_eq!(out["output_tokens"], json!(20));
    assert_eq!(out["prompt_total"], json!(100));
    assert_eq!(out["cache_read_input_tokens"], json!(0));
}

#[test]
fn input_tokens_variant_fallback() {
    // prompt_tokens 键缺失 → 回退 input_tokens（键存在语义）
    let out = convert_usage(Some(&json!({"input_tokens": 50, "output_tokens": 5})));
    assert_eq!(out["prompt_total"], json!(50));
    assert_eq!(out["input_tokens"], json!(50));
    assert_eq!(out["output_tokens"], json!(5));
}

#[test]
fn prompt_tokens_null_does_not_penetrate() {
    // 键存在但为 null：Python usage.get("prompt_tokens", ...) 返回 None → _to_int(None)=0
    // 不穿透到 input_tokens
    let out = convert_usage(Some(&json!({
        "prompt_tokens": null, "input_tokens": 50,
    })));
    assert_eq!(out["prompt_total"], json!(0));
    assert_eq!(out["input_tokens"], json!(0));
}

#[test]
fn deepseek_cache_hit_subtracts_from_input() {
    let out = convert_usage(Some(&json!({
        "prompt_tokens": 10, "prompt_cache_hit_tokens": 7,
    })));
    assert_eq!(out["cache_read_input_tokens"], json!(7));
    assert_eq!(out["input_tokens"], json!(3)); // max(0, 10 - 7 - 0)
}

#[test]
fn deepseek_zero_hit_penetrates_to_details() {
    // falsy(0) 穿透：命中 0 → 继续 prompt_tokens_details.cached_tokens
    let out = convert_usage(Some(&json!({
        "prompt_tokens": 10,
        "prompt_cache_hit_tokens": 0,
        "prompt_tokens_details": {"cached_tokens": 5},
    })));
    assert_eq!(out["cache_read_input_tokens"], json!(5));
}

#[test]
fn deepseek_nonzero_hit_wins_over_details() {
    let out = convert_usage(Some(&json!({
        "prompt_tokens": 10,
        "prompt_cache_hit_tokens": 7,
        "prompt_tokens_details": {"cached_tokens": 5},
    })));
    assert_eq!(out["cache_read_input_tokens"], json!(7));
}

#[test]
fn details_null_does_not_penetrate_deeper() {
    // 键存在但为 null：不穿透到 input_details（Python .get 返回 None → _to_int(None)=0）
    let out = convert_usage(Some(&json!({
        "prompt_tokens_details": {"cached_tokens": null},
        "input_tokens_details": {"cached_tokens": 3},
    })));
    assert_eq!(out["cache_read_input_tokens"], json!(0));
}

#[test]
fn details_missing_penetrates_deeper() {
    // 键缺失才回退下一层
    let out = convert_usage(Some(&json!({
        "input_tokens_details": {"cached_tokens": 3},
    })));
    assert_eq!(out["cache_read_input_tokens"], json!(3));
}

#[test]
fn cache_write_precedence_chain() {
    // prompt_details.cache_creation > prompt_details.cache_write >
    // input_details.cache_creation > input_details.cache_write > 顶层
    let out = convert_usage(Some(&json!({
        "prompt_tokens_details": {"cache_write_tokens": 11},
        "input_tokens_details": {"cache_creation_input_tokens": 12},
        "cache_creation_input_tokens": 13,
    })));
    assert_eq!(out["cache_creation_input_tokens"], json!(11));

    let out = convert_usage(Some(&json!({
        "input_tokens_details": {"cache_creation_input_tokens": 12},
        "cache_creation_input_tokens": 13,
    })));
    assert_eq!(out["cache_creation_input_tokens"], json!(12));

    let out = convert_usage(Some(&json!({"cache_creation_input_tokens": 13})));
    assert_eq!(out["cache_creation_input_tokens"], json!(13));

    let out = convert_usage(Some(&json!({"cache_write_tokens": 14})));
    assert_eq!(out["cache_creation_input_tokens"], json!(14));
}

#[test]
fn reasoning_tokens_or_chain_penetrates_zero() {
    // falsy 穿透：completion details 的 0 → 继续 output details
    let out = convert_usage(Some(&json!({
        "completion_tokens_details": {"reasoning_tokens": 0},
        "output_tokens_details": {"reasoning_tokens": 9},
    })));
    assert_eq!(out["reasoning_tokens"], json!(9));

    // 键存在为 null → 穿透
    let out = convert_usage(Some(&json!({
        "completion_tokens_details": {"reasoning_tokens": null},
        "output_tokens_details": {"reasoning_tokens": 9},
    })));
    assert_eq!(out["reasoning_tokens"], json!(9));

    // 键缺失 → 回退
    let out = convert_usage(Some(&json!({
        "output_tokens_details": {"reasoning_tokens": 9},
    })));
    assert_eq!(out["reasoning_tokens"], json!(9));

    // 双方都无 → 0
    let out = convert_usage(Some(&json!({"prompt_tokens": 1})));
    assert_eq!(out["reasoning_tokens"], json!(0));
}

#[test]
fn string_coercion_best_effort() {
    // Python int("42") 可解析；int("42.5") 抛异常 → 0
    let out = convert_usage(Some(&json!({"prompt_tokens": "42", "completion_tokens": "7"})));
    assert_eq!(out["prompt_total"], json!(42));
    assert_eq!(out["output_tokens"], json!(7));

    let out = convert_usage(Some(&json!({"prompt_tokens": "42.5"})));
    assert_eq!(out["prompt_total"], json!(0));
}

#[test]
fn missing_usage_yields_zeros() {
    let out = convert_usage(None);
    assert_eq!(out["input_tokens"], json!(0));
    assert_eq!(out["output_tokens"], json!(0));

    let out = convert_usage(Some(&json!({})));
    assert_eq!(out["prompt_total"], json!(0));
}

#[test]
fn chat_usage_to_responses_mapping() {
    let out = chat_usage_to_responses(Some(&json!({
        "prompt_tokens": 100, "completion_tokens": 20, "total_tokens": 120,
    })));
    assert_eq!(out["input_tokens"], json!(100));
    assert_eq!(out["output_tokens"], json!(20));
    assert_eq!(out["total_tokens"], json!(120));
    assert_eq!(out["input_tokens_details"], json!({"cached_tokens": 0}));
    assert_eq!(out["output_tokens_details"], json!({"reasoning_tokens": 0}));
}

#[test]
fn chat_usage_cached_or_chain() {
    // DeepSeek 顶层命中（非零）优先
    let out = chat_usage_to_responses(Some(&json!({
        "prompt_tokens": 100,
        "prompt_cache_hit_tokens": 7,
        "prompt_tokens_details": {"cached_tokens": 5},
    })));
    assert_eq!(out["input_tokens_details"]["cached_tokens"], json!(7));
    // 顶层 0 → details 穿透
    let out = chat_usage_to_responses(Some(&json!({
        "prompt_tokens": 100,
        "prompt_cache_hit_tokens": 0,
        "prompt_tokens_details": {"cached_tokens": 5},
    })));
    assert_eq!(out["input_tokens_details"]["cached_tokens"], json!(5));
}

#[test]
fn chat_usage_input_tokens_variant() {
    let out = chat_usage_to_responses(Some(&json!({"input_tokens": 30, "output_tokens": 4})));
    assert_eq!(out["input_tokens"], json!(30));
    assert_eq!(out["output_tokens"], json!(4));
    assert_eq!(out["total_tokens"], json!(34));
}

#[test]
fn chat_usage_reasoning_or_chain() {
    let out = chat_usage_to_responses(Some(&json!({
        "completion_tokens_details": {"reasoning_tokens": 0},
        "output_tokens_details": {"reasoning_tokens": 3},
    })));
    assert_eq!(out["output_tokens_details"]["reasoning_tokens"], json!(3));
}
