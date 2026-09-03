//! o2a-convert：协议转换（对齐 Python `o2a/convert.py` + `engine.handle_claude_stream` 行为）。
//!
//! 模块划分：
//! - [`common`]：入口识别 / 模式解析 / SSE 格式化 / 文本提取 / cache_control 剥离 / thinking 基元
//! - [`usage`]：OpenAI ↔ Anthropic usage 语义转换（键存在 vs falsy 穿透混合语义）
//! - [`request`]：Anthropic Messages → Chat 请求转换（convert_request + thinking 五态映射）
//! - [`responses`]：OpenAI Responses ↔ Chat 转换（请求 / 非流式响应 / 流式翻译器）
//! - [`claude_stream`]：claude 模式流式状态机（chat chunk → Anthropic SSE 事件列表）
//!
//! 与 Python 的已知差异（有意为之，全部为 Python 崩溃路径的安全化）：
//! - JSON 序列化为紧凑格式（Python json.dumps 默认带 `, ` / `: ` 空格）；sse_event 输出
//!   ensure_ascii=True 等价的 \uXXXX 转义，语义与字节兼容客户端解析
//! - messages 数组中的非 dict 项：Python 抛 AttributeError（引擎转 500），Rust 跳过
//! - tool_call index 为 null 时：Python int(None) 崩溃，Rust 按 index 缺失处理
//! - claude 流收尾后继续到达的 tool args（Python KeyError → 流内 error 事件）：
//!   Rust 静默跳过

pub mod claude_stream;
pub mod common;
pub mod request;
pub mod responses;
pub mod usage;

pub use claude_stream::{classify_final, ChunkTranslator};
pub use common::{
    anthropic_stop_reason, budget_to_effort, detect_client, extract_text, infer_thinking_style,
    normalize_roles, resolve_mode, sse_event, strip_cache_control, tool_choice_any,
};
pub use request::{
    apply_reasoning_to_chat, apply_thinking_to_chat, convert_request, convert_tool_input,
};
pub use responses::{chat_to_responses_json, responses_to_chat, ResponsesStreamTranslator};
pub use usage::{chat_usage_to_responses, convert_usage};
