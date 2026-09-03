//! 公共转换原语：入口识别 / 模式解析 / SSE / 文本提取 / thinking 基元（对齐 `convert.py` 顶部公共函数）。

use o2a_config::{AccountKind, ClientKind, DispatchMode, Service, ThinkingMode};
use serde_json::{Map, Value};

/// Python truthiness：None/False/0/空串/空容器 → false。
pub(crate) fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(false),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// best-effort int 强转（对齐 Python `_to_int` / `int()` 对常见 JSON 类型的行为）。
/// 字符串只接受整数字面量（Python `int("12.5")` 抛异常 → 按失败处理）。
pub(crate) fn coerce_int(v: &Value, default: i64) -> i64 {
    match v {
        Value::Number(n) => n
            .as_i64()
            .or_else(|| n.as_f64().map(|f| f.trunc() as i64))
            .unwrap_or(default),
        Value::Bool(b) => {
            if *b {
                1
            } else {
                0
            }
        }
        Value::String(s) => s.trim().parse::<i64>().unwrap_or(default),
        _ => default,
    }
}

/// dict.get 键存在语义：键存在（含 null）返回 Some；缺失 None。
pub(crate) fn key<'a>(m: &'a Map<String, Value>, k: &str) -> Option<&'a Value> {
    m.get(k)
}

/// `(obj or {})` 语义：truthy 且为 object → 该对象；否则视为空（Python 对 truthy
/// 非 dict 会 AttributeError，此处安全化为空 dict）。
pub(crate) fn obj_or_empty(v: Option<&Value>) -> Option<&Map<String, Value>> {
    v.and_then(|x| x.as_object())
}

// ---------------------------------------------------------------------------
// 入口识别与模式解析
// ---------------------------------------------------------------------------

/// 自动识别入口协议（对齐 `detect_client`）。返回 "anthropic" 或 "openai"。
pub fn detect_client(path: &str, payload: Option<&Value>) -> &'static str {
    let p = path.to_lowercase();
    if p.contains("/v1/messages") {
        return "anthropic";
    }
    if p.contains("/responses") || p.contains("/chat/completions") || p.contains("/completions") {
        return "openai";
    }
    if let Some(Value::Object(obj)) = payload {
        if obj.contains_key("input") && !obj.contains_key("messages") {
            return "openai"; // OpenAI Responses
        }
        if obj.contains_key("max_tokens") && obj.contains_key("system") {
            return "anthropic"; // Anthropic Messages
        }
        if obj.contains_key("messages") {
            let msgs = obj.get("messages").and_then(|v| v.as_array());
            if let Some([first, ..]) = msgs.map(|a| a.as_slice()) {
                // Anthropic 的 content 是 block 列表（text/tool_use/tool_result）
                if first.get("content").map(|c| c.is_array()).unwrap_or(false) {
                    return "anthropic";
                }
                return "openai";
            }
            return "openai";
        }
        if obj.contains_key("max_tokens") {
            return "anthropic";
        }
    }
    "openai" // 默认
}

/// 确定一次请求的分派模式（对齐 `resolve_mode`）。
/// 返回 None 表示该组合不支持（OpenAI 客户端 + 无 OpenAI 端点的账号）。
pub fn resolve_mode(
    service: &Service,
    path: &str,
    payload: Option<&Value>,
) -> Option<DispatchMode> {
    if service.api.is_some() {
        // api 已显式声明：直接采用推导结果，不再按请求体识别
        return Some(service.mode());
    }
    match service.client {
        ClientKind::Auto => {
            if detect_client(path, payload) == "anthropic" {
                Some(direct_or_claude(service))
            } else if service.kind() != AccountKind::Anthropic {
                Some(DispatchMode::Codex)
            } else {
                None
            }
        }
        ClientKind::Openai => {
            if service.kind() != AccountKind::Anthropic {
                Some(DispatchMode::Codex)
            } else {
                None
            }
        }
        ClientKind::Anthropic => Some(direct_or_claude(service)),
    }
}

fn direct_or_claude(service: &Service) -> DispatchMode {
    if matches!(service.kind(), AccountKind::Anthropic | AccountKind::Both) {
        DispatchMode::Direct
    } else {
        DispatchMode::Claude
    }
}

// ---------------------------------------------------------------------------
// SSE 格式化
// ---------------------------------------------------------------------------

/// ensure_ascii=True 等价：把 serde_json 输出中的非 ASCII 字符转成 \uXXXX
/// （非 ASCII 只可能出现在字符串字面量内，逐字符替换语义等价）。
/// Python `json.dumps`（默认参数）等价序列化：`", "` / `": "` 分隔符 +
/// ensure_ascii（非 ASCII 转 `\uXXXX` 小写十六进制，增补平面用代理对）。
/// 键序为 serde_json Map 序（默认字典序），与 Python 插入序不同——客户端按
/// JSON 解析不依赖键序（docs/rust-rewrite.md §3.4）。浮点极端形式（如 1e30
/// 的 `1e+30`）不逐一复刻；SSE 事件载荷不含此类值。
pub fn py_json_dumps(data: &Value) -> String {
    match data {
        Value::Null => "null".into(),
        Value::Bool(true) => "true".into(),
        Value::Bool(false) => "false".into(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => py_json_string(s),
        Value::Array(items) => {
            let parts: Vec<String> = items.iter().map(py_json_dumps).collect();
            format!("[{}]", parts.join(", "))
        }
        Value::Object(obj) => {
            let parts: Vec<String> = obj
                .iter()
                .map(|(k, v)| format!("{}: {}", py_json_string(k), py_json_dumps(v)))
                .collect();
            format!("{{{}}}", parts.join(", "))
        }
    }
}

/// Python json.dumps 的字符串转义：短转义 + 控制字符 `\u00xx` +
/// 非 ASCII 全量 `\uXXXX`（ensure_ascii=True）。
fn py_json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c if c.is_ascii() => out.push(c),
            c => {
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    out.push_str(&format!("\\u{:04x}", unit));
                }
            }
        }
    }
    out.push('"');
    out
}

/// 格式化 SSE 事件（对齐 `sse_event`）。
/// event 类型默认取 data["type"]；输出 `event: <type>\ndata: <json>\n\n`。
pub fn sse_event(data: &Value) -> String {
    let mut event_type: Option<&str> = None;
    if let Value::Object(obj) = data {
        if let Some(Value::String(t)) = obj.get("type") {
            event_type = Some(t);
        }
    }
    let mut out = String::new();
    if let Some(t) = event_type {
        out.push_str(&format!("event: {t}\n"));
    }
    out.push_str(&format!("data: {}\n\n", py_json_dumps(data)));
    out
}

// ---------------------------------------------------------------------------
// 通用小函数
// ---------------------------------------------------------------------------

/// OpenAI finish_reason → Anthropic stop_reason（对齐 `_anthropic_stop_reason`）。
/// 未知值原样返回。
pub fn anthropic_stop_reason(finish_reason: Option<&str>, has_tool_calls: bool) -> String {
    if has_tool_calls || finish_reason == Some("tool_calls") {
        return "tool_use".into();
    }
    match finish_reason {
        Some("length") => "max_tokens".into(),
        None | Some("") | Some("stop") => "end_turn".into(),
        Some("content_filter") => "stop_sequence".into(),
        Some(other) => other.to_string(),
    }
}

/// 将 Anthropic content blocks 转为纯文本字符串（对齐 `_extract_text`）。
pub fn extract_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => {
            let mut parts: Vec<String> = Vec::new();
            for block in blocks {
                match block {
                    Value::String(s) => parts.push(s.clone()),
                    Value::Object(obj) => match obj.get("type").and_then(|t| t.as_str()) {
                        Some("text") => {
                            parts.push(obj.get("text").and_then(|t| t.as_str()).unwrap_or("").into())
                        }
                        Some("tool_result") => match obj.get("content") {
                            Some(Value::String(s)) => parts.push(s.clone()),
                            Some(Value::Array(cbs)) => {
                                for cb in cbs {
                                    if cb.get("type").and_then(|t| t.as_str()) == Some("text") {
                                        parts.push(
                                            cb.get("text").and_then(|t| t.as_str()).unwrap_or("").into(),
                                        );
                                    }
                                }
                            }
                            _ => {}
                        },
                        // Python：其余类型 dict（thinking 等）不追加任何内容
                        _ => {}
                    },
                    other => parts.push(other.to_string()),
                }
            }
            parts.join("\n")
        }
        other => other.to_string(),
    }
}

/// 将 Anthropic input_schema 转为 OpenAI function parameters 格式（对齐 `convert_tool_input`）。
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

/// 递归移除 cache_control 字段（对齐 `_strip_cache_control`；DashScope 不支持）。
pub fn strip_cache_control(obj: &Value) -> Value {
    match obj {
        Value::Object(map) => {
            let mut out = Map::new();
            for (k, v) in map {
                if k == "cache_control" {
                    continue;
                }
                out.insert(k.clone(), strip_cache_control(v));
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(strip_cache_control).collect()),
        other => other.clone(),
    }
}

/// 将 OpenAI 特有的 developer 角色规范化为 system（对齐 `normalize_roles`）。
/// 处理 chat messages 与 responses input 两处；返回是否发生修改。
pub fn normalize_roles(payload: &mut Value) -> bool {
    let mut changed = false;
    let Some(obj) = payload.as_object_mut() else {
        return false;
    };
    for key in ["messages", "input"] {
        let Some(Value::Array(items)) = obj.get_mut(key) else {
            continue;
        };
        for item in items.iter_mut() {
            if let Value::Object(m) = item {
                if m.get("role").and_then(|r| r.as_str()) == Some("developer") {
                    m.insert("role".into(), Value::String("system".into()));
                    changed = true;
                }
            }
        }
    }
    changed
}

/// Anthropic tool_choice='any'（必须调用）→ OpenAI：单工具绑定该工具，多工具 required
/// （对齐 `_tool_choice_any`）。
pub fn tool_choice_any(openai_tools: &Value) -> Value {
    let mut names: Vec<String> = Vec::new();
    if let Some(arr) = openai_tools.as_array() {
        for t in arr {
            if let Some(fn_obj) = t.get("function").and_then(|f| f.as_object()) {
                if let Some(n) = fn_obj.get("name").and_then(|n| n.as_str()) {
                    names.push(n.to_string());
                }
            }
        }
    }
    names.retain(|n| !n.is_empty());
    if names.len() == 1 {
        serde_json::json!({"type": "function", "function": {"name": names[0]}})
    } else {
        Value::String("required".into())
    }
}

// ---------------------------------------------------------------------------
// thinking 基元
// ---------------------------------------------------------------------------

/// Anthropic budget_tokens（token 预算）→ OpenAI reasoning_effort 档位（对齐 `_budget_to_effort`）。
/// ≥8192 → high，≥2048 → medium，其余 → low；非正数 / 解析失败 → None。
pub fn budget_to_effort(budget: Option<&Value>) -> Option<&'static str> {
    let v = budget?;
    if !truthy(v) {
        return None; // Python `budget or 0`：falsy 直接 0 → None
    }
    let b = match v {
        Value::Number(n) => n
            .as_i64()
            .or_else(|| n.as_f64().map(|f| f.trunc() as i64))?,
        Value::String(s) => s.trim().parse::<i64>().ok()?, // int("12.5") 在 Python 抛异常
        Value::Bool(true) => 1,
        Value::Bool(false) => return None,
        _ => return None,
    };
    if b <= 0 {
        return None;
    }
    if b >= 8192 {
        Some("high")
    } else if b >= 2048 {
        Some("medium")
    } else {
        Some("low")
    }
}

/// auto 模式下按上游 URL / 模型名推断思考参数风格（对齐 `_infer_thinking_style`）。
pub fn infer_thinking_style(service: &Service) -> ThinkingMode {
    let url = service.account.openai_url.to_lowercase();
    let model = service.model.to_lowercase();
    if url.contains("dashscope") || url.contains("qwen") || model.contains("qwen") {
        ThinkingMode::EnableThinking
    } else if url.contains("deepseek")
        || url.contains("moonshot")
        || url.contains("kimi")
        || model.contains("kimi")
    {
        ThinkingMode::Passthrough
    } else {
        ThinkingMode::Effort
    }
}
