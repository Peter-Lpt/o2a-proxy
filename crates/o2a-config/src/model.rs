//! Account / Service 数据模型与 mode 推导（对齐 Python `o2a/config.py`）。

use serde_json::{Map, Value};

/// 账号端点类型（对齐 `Account.kind` property）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountKind {
    Both,
    Openai,
    Anthropic,
    Invalid,
}

/// 旧 client 字段（api 未声明时的兼容入口）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientKind {
    Anthropic,
    Openai,
    Auto,
}

/// 入口协议显式声明（对齐 `services[].api` 合法值）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenaiApi {
    AnthropicMessages,
    OpenaiCompletions,
    OpenaiResponses,
}

/// 上游原生协议（配合 api=openai-responses 使用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UpstreamApi {
    #[default]
    OpenaiCompletions,
    OpenaiResponses,
}

/// 思考深度透传模式（对齐 `_THINKING_MODES`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThinkingMode {
    #[default]
    Auto,
    Passthrough,
    Effort,
    EnableThinking,
    None,
}

/// 模型白名单外请求处理策略（对齐 `MODEL_POLICIES`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ModelPolicy {
    #[default]
    Clamp,
    Reject,
    Passthrough,
}

/// 分派模式（对齐 `Service.mode`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchMode {
    Claude,
    Codex,
    Direct,
    Auto,
}

impl DispatchMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            DispatchMode::Claude => "claude",
            DispatchMode::Codex => "codex",
            DispatchMode::Direct => "direct",
            DispatchMode::Auto => "auto",
        }
    }
}

/// 计价模式（对齐 `PRICING_MODES`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PricingMode {
    Token,
    Subscription,
    Free,
}

/// 账号：凭证 + 端点（对齐 `Account`）。
#[derive(Debug, Clone)]
pub struct Account {
    pub id: String,
    pub name: String,
    pub api_key: String,
    pub openai_url: String,
    pub anthropic_url: String,
    /// 账号级默认入口协议（可被 services[].api 覆盖），空 = 未声明
    pub api: String,
    /// 额度来源：auto | openrouter | anthropic | codex | zen | local | manual | none
    pub quota_source: String,
    /// manual 适配器手填额度（冷启动兜底）
    pub quota: Option<Map<String, Value>>,
}

impl Account {
    pub fn kind(&self) -> AccountKind {
        let has_o = !self.openai_url.is_empty();
        let has_a = !self.anthropic_url.is_empty();
        match (has_o, has_a) {
            (true, true) => AccountKind::Both,
            (true, false) => AccountKind::Openai,
            (false, true) => AccountKind::Anthropic,
            (false, false) => AccountKind::Invalid,
        }
    }

    /// 账号是否可服务：有 key 且至少一个端点。
    pub fn valid(&self) -> bool {
        self.kind() != AccountKind::Invalid && !self.api_key.is_empty()
    }
}

/// 服务别名映射：保持插入顺序（/models 列表输出顺序依赖），提供查询辅助。
#[derive(Debug, Clone, Default)]
pub struct ModelsMap(pub Vec<(String, String)>);

impl ModelsMap {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.0.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    pub fn contains_key(&self, key: &str) -> bool {
        self.get(key).is_some()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &String)> {
        self.0.iter().map(|(k, v)| (k, v))
    }

    pub fn keys(&self) -> impl Iterator<Item = &String> {
        self.0.iter().map(|(k, _)| k)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// {上游名: 对外名}（统计按对外名记录用），对齐 `reverse_models_map`。
    /// Python dict 推导后同名上游后者覆盖前者，此处保持一致（后者覆盖）。
    pub fn reverse(&self) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = Vec::new();
        for (external, upstream) in &self.0 {
            if upstream.is_empty() {
                continue;
            }
            if let Some(pair) = out.iter_mut().find(|(u, _)| u == upstream) {
                pair.1 = external.clone();
            } else {
                out.push((upstream.clone(), external.clone()));
            }
        }
        out
    }
}

/// 单个服务（接入点）：独立端口 + 引用账号 + 客户端类型 + 入口协议（对齐 `Service`）。
#[derive(Debug, Clone)]
pub struct Service {
    /// 稳定身份（svc-<8hex>），生成后终生不变
    pub id: String,
    /// 显示名（comment）
    pub name: String,
    pub account: Account,
    pub client: ClientKind,
    pub host: String,
    pub port: i64,
    pub model: String,
    pub override_model: bool,
    pub max_tokens: i64,
    pub proxy: String,
    /// 入口协议显式声明；None = 未声明（回退 client/auto 识别）
    pub api: Option<OpenaiApi>,
    pub upstream_api: UpstreamApi,
    pub thinking_mode: ThinkingMode,
    /// 计价模式（"" = token；"none" = subscription 别名；对象形式按 mode）
    pub pricing_mode: PricingMode,
    /// 对象形式 pricing 的附加字段（plan / quota_source / batch 等）
    pub pricing_extra: Option<Map<String, Value>>,
    /// 原始 pricing 值（"none" 字符串 / 对象 / 空串原样保留，写回不丢字段）
    pub pricing_raw: Value,
    /// 客户端凭证（接入层鉴权）：非空时校验 Authorization: Bearer / x-api-key
    pub auth_token: String,
    pub order: i64,
    pub enabled: bool,
    pub autostart: bool,
    /// 服务级模型白名单（对外名）；空 = 不限制
    pub models: Vec<String>,
    /// {对外名: 上游名} 别名映射
    pub models_map: ModelsMap,
    pub model_policy: ModelPolicy,
    /// auto 服务每次请求识别后的模式覆盖（对齐 `_mode_override` / `with_mode`）
    pub mode_override: Option<DispatchMode>,
}

impl Service {
    /// api_key 快捷访问（对齐 `Service.api_key` property）。
    pub fn api_key(&self) -> &str {
        &self.account.api_key
    }

    pub fn kind(&self) -> AccountKind {
        self.account.kind()
    }

    /// 推导出的分派模式（对齐 `Service.mode` property）。
    pub fn mode(&self) -> DispatchMode {
        if let Some(m) = self.mode_override {
            return m;
        }
        if let Some(api) = self.api {
            return match api {
                OpenaiApi::AnthropicMessages => {
                    if matches!(self.kind(), AccountKind::Anthropic | AccountKind::Both) {
                        DispatchMode::Direct
                    } else {
                        DispatchMode::Claude
                    }
                }
                OpenaiApi::OpenaiCompletions | OpenaiApi::OpenaiResponses => DispatchMode::Codex,
            };
        }
        match self.client {
            ClientKind::Openai => DispatchMode::Codex,
            ClientKind::Anthropic => {
                if matches!(self.kind(), AccountKind::Anthropic | AccountKind::Both) {
                    DispatchMode::Direct
                } else {
                    DispatchMode::Claude
                }
            }
            ClientKind::Auto => DispatchMode::Auto,
        }
    }

    /// 出口端点（完整 URL）。direct 用 anthropic 端点，其余用 openai 端点。
    pub fn target_url(&self) -> &str {
        if self.mode() == DispatchMode::Direct {
            &self.account.anthropic_url
        } else {
            &self.account.openai_url
        }
    }

    /// 返回模式确定的服务拷贝（auto 服务每个请求用），对齐 `with_mode`。
    /// 其余字段（含 auth_token / 白名单 / 计价）全部保留。
    pub fn with_mode(&self, mode: DispatchMode) -> Service {
        let mut s = self.clone();
        s.mode_override = Some(mode);
        s
    }
}

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

/// best-effort int 强转（对齐 Python `int()` 对常见 JSON 类型的行为）。
pub(crate) fn coerce_int(v: &Value, default: i64) -> i64 {
    match v {
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                i
            } else if let Some(f) = n.as_f64() {
                f.trunc() as i64
            } else {
                default
            }
        }
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

/// 归一化 services[].pricing → (mode, extra)（对齐 `normalize_pricing_value`）。
///
/// - ""（缺省）→ Token；"none" → Subscription 兼容别名
/// - 对象 → mode 必填合法；可附 plan / quota_source / batch 等
/// - 非法值回退 Token 并警告（extra 一并丢弃，与 Python 一致）
pub fn normalize_pricing_value(raw: &Value) -> (PricingMode, Option<Map<String, Value>>) {
    const PRICING_MODES: [&str; 3] = ["token", "subscription", "free"];
    if let Value::Object(obj) = raw {
        let mode = obj.get("mode").and_then(Value::as_str).unwrap_or("token");
        if !PRICING_MODES.contains(&mode) {
            tracing::warn!("[config] pricing.mode '{}' 非法，回退 token", mode);
            return (PricingMode::Token, None);
        }
        let extra: Map<String, Value> = obj
            .iter()
            .filter(|(k, _)| k.as_str() != "mode")
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let pm = match mode {
            "subscription" => PricingMode::Subscription,
            "free" => PricingMode::Free,
            _ => PricingMode::Token,
        };
        return (pm, if extra.is_empty() { None } else { Some(extra) });
    }
    let s = match raw {
        Value::Null => String::new(),
        Value::String(s) => s.trim().to_string(),
        other => other.to_string(),
    };
    if s == "none" {
        return (PricingMode::Subscription, None);
    }
    if s.is_empty() {
        return (PricingMode::Token, None);
    }
    tracing::warn!("[config] pricing '{}' 非法，回退 token（仅支持 none / 对象形式）", s);
    (PricingMode::Token, None)
}
