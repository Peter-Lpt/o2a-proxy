//! auth.json / config.json 加载、旧结构迁移、service_ids 惰性写回（对齐 Python `o2a/config.py` 加载逻辑）。

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::model::{
    coerce_int, normalize_pricing_value, truthy, Account, ClientKind, ModelsMap, ModelPolicy,
    OpenaiApi, PricingMode, Service, ThinkingMode, UpstreamApi,
};
use crate::paths::{normalize_openai_url, resolve_auth_path, resolve_config_path};

/// 生成稳定服务 id：svc-<8 位十六进制随机>（对齐 `new_service_id`）。
pub fn new_service_id() -> String {
    use rand::Rng;
    let n: u32 = rand::thread_rng().gen();
    format!("svc-{:08x}", n)
}

/// 从 auth.json 读取账号密钥（对齐 `load_auth`）。
///
/// 格式（键可为账号 id 或 name，兼容 dict {"type","key"} 与纯字符串两形态）；
/// `_` 前缀元键（如 _readme）跳过。文件不存在或解析失败返回空 map。
pub fn load_auth_from(path: &Path) -> HashMap<String, String> {
    let raw = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(_) => return HashMap::new(),
    };
    let data: Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return HashMap::new(),
    };
    let mut out = HashMap::new();
    if let Value::Object(obj) = data {
        for (k, v) in obj {
            if k.starts_with('_') {
                continue;
            }
            match v {
                Value::Object(inner) => {
                    let key = inner.get("key").and_then(Value::as_str).unwrap_or("");
                    out.insert(k, key.to_string());
                }
                Value::String(s) => {
                    out.insert(k, s);
                }
                _ => {}
            }
        }
    }
    out
}

/// 解析账号 api_key：auth.json 优先（按 id，再按 name），回退配置内嵌（对齐 `_resolve_api_key`）。
pub fn resolve_api_key(
    auth: &HashMap<String, String>,
    acc_id: &str,
    acc_name: &str,
    embedded: &str,
) -> String {
    if !acc_id.is_empty() {
        if let Some(k) = auth.get(acc_id) {
            if !k.is_empty() {
                return k.clone();
            }
        }
    }
    if !acc_name.is_empty() {
        if let Some(k) = auth.get(acc_name) {
            if !k.is_empty() {
                return k.clone();
            }
        }
    }
    embedded.to_string()
}

fn service_id_registry_path(config_path: &Path) -> PathBuf {
    config_path
        .parent()
        .unwrap_or(Path::new("."))
        .join("service_ids.json")
}

fn load_service_id_registry(config_path: &Path) -> Map<String, Value> {
    let raw = match fs::read_to_string(service_id_registry_path(config_path)) {
        Ok(s) => s,
        Err(_) => return Map::new(),
    };
    match serde_json::from_str::<Value>(&raw) {
        Ok(Value::Object(m)) => m,
        _ => Map::new(),
    }
}

fn save_service_id_registry(config_path: &Path, reg: &Map<String, Value>) {
    let Ok(mut f) = fs::File::create(service_id_registry_path(config_path)) else {
        return;
    };
    // 对齐 Python json.dump(..., ensure_ascii=False, indent=2)
    if let Ok(s) = serde_json::to_string_pretty(&Value::Object(reg.clone())) {
        let _ = f.write_all(s.as_bytes());
    }
}

/// 惰性写回：为缺失 id 的服务生成稳定 id 并写回 config.json（对齐 `_ensure_service_ids`）。
///
/// 生成前优先按显示名从登记表（service_ids.json）找回历史 id；写回前备份 config.json.bak。
/// 仅在解析成功且确有缺失（含重复 id）时写一次。
pub fn ensure_service_ids(config_path: &Path, config: &mut Value) {
    let Some(services) = config.get_mut("services").and_then(Value::as_array_mut) else {
        return;
    };
    let mut seen = std::collections::HashSet::new();
    let mut missing = false;
    for svc in services.iter_mut() {
        let Some(obj) = svc.as_object_mut() else {
            continue;
        };
        let sid = obj
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        if sid.is_empty() {
            missing = true;
        } else if !seen.insert(sid) {
            missing = true; // 重复 id：保留首个，后续重新生成
        }
    }
    if !missing {
        return;
    }

    let mut reg = load_service_id_registry(config_path);
    let mut reg_changed = false;
    let mut assigned = std::collections::HashSet::new();
    for svc in services.iter_mut() {
        let Some(obj) = svc.as_object_mut() else {
            continue;
        };
        let comment = obj
            .get("comment")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let sid = obj
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        if !sid.is_empty() && assigned.insert(sid.clone()) {
            continue;
        }
        // 缺失或重复：优先按显示名从登记表找回同一 id，找不到才重新生成
        let mut new_sid = String::new();
        if !comment.is_empty() {
            if let Some(reg_id) = reg.get(&comment).and_then(Value::as_str) {
                if !reg_id.is_empty() && assigned.insert(reg_id.to_string()) {
                    new_sid = reg_id.to_string();
                }
            }
        }
        if new_sid.is_empty() {
            loop {
                let candidate = new_service_id();
                if assigned.insert(candidate.clone()) {
                    new_sid = candidate;
                    break;
                }
            }
        }
        obj.insert("id".to_string(), Value::String(new_sid.clone()));
        if !comment.is_empty() {
            let reg_same = reg.get(&comment).and_then(Value::as_str) == Some(new_sid.as_str());
            if !reg_same {
                reg.insert(comment, Value::String(new_sid));
                reg_changed = true;
            }
        }
    }
    if reg_changed {
        save_service_id_registry(config_path, &reg);
    }

    // 备份原始内容后写回（2 空格缩进、非 ASCII 不转义）
    if let Ok(raw) = fs::read_to_string(config_path) {
        let _ = fs::write(config_path.with_extension("json.bak"), raw);
    }
    if let Ok(pretty) = serde_json::to_string_pretty(config) {
        if fs::write(config_path, pretty).is_ok() {
            tracing::info!(
                "[config] 已为缺失 id 的服务生成稳定 id 并写回 config.json（备份: {}）",
                config_path.with_extension("json.bak").display()
            );
        }
    }
}

/// 统计设置（Python 经环境变量 setdefault 传递；Rust 显式解析，env 优先）。
#[derive(Debug, Clone, PartialEq)]
pub struct StatsSettings {
    pub enabled: bool,
    pub dir: PathBuf,
    pub retention_days: i64,
}

/// 从 config 顶层字段 + 环境变量解析统计设置（env 已设置时优先，等价 setdefault 语义）。
pub fn resolve_stats_settings(config: &Value) -> StatsSettings {
    let enabled = crate::paths::env_value("CACHE_STATS_ENABLED")
        .map(|v| matches!(v.to_lowercase().as_str(), "true" | "1" | "yes"))
        .unwrap_or_else(|| config.get("cache_stats_enabled").map(truthy).unwrap_or(true));
    let dir = crate::paths::env_value("CACHE_STATS_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            config
                .get("cache_stats_dir")
                .and_then(Value::as_str)
                .map(|s| {
                    if s.is_empty() {
                        PathBuf::from("data/cache_stats")
                    } else {
                        PathBuf::from(s)
                    }
                })
        })
        .unwrap_or_else(|| PathBuf::from("data/cache_stats"));
    let retention_days = crate::paths::env_value("CACHE_STATS_RETENTION_DAYS")
        .and_then(|v| v.parse::<i64>().ok())
        .or_else(|| {
            config
                .get("cache_stats_retention_days")
                .map(|v| coerce_int(v, 30))
        })
        .unwrap_or(30);
    StatsSettings {
        enabled,
        dir,
        retention_days,
    }
}

/// 顶层可选 `retry` 块(引擎侧自动重试配置;缺省全部默认且 `enabled=false` = 保持透传语义)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetrySettings {
    /// 是否启用引擎侧自动重试(缺省 false:下游 CLI 自带退避,代理不改语义)。
    pub enabled: bool,
    /// 最大重试次数(初始请求外的追加次数;0 = 不限制)。
    pub max_attempts: usize,
    /// 指数退避基数(ms)。
    pub base_ms: u64,
    /// 单次等待上限(ms);小于 base_ms 时回退为 base_ms。
    pub max_ms: u64,
}

impl Default for RetrySettings {
    fn default() -> Self {
        Self {
            enabled: false,
            max_attempts: 5,
            base_ms: 1000,
            max_ms: 30000,
        }
    }
}

/// 解析顶层 `retry` 块;字段非法(类型错/越界)一律警告 + 回退默认,对齐本项目「非法字段回退默认」风格。
/// 缺省整体关闭,不改变现有透传行为。
pub fn resolve_retry_settings(config: &Value) -> RetrySettings {
    let Some(obj) = config.get("retry").and_then(Value::as_object) else {
        return RetrySettings::default();
    };

    let enabled = match obj.get("enabled") {
        Some(v) => v.as_bool().unwrap_or_else(|| {
            tracing::warn!("[config] retry.enabled 非法，回退默认 false");
            false
        }),
        None => false,
    };
    let max_attempts = match obj.get("max_attempts") {
        Some(v) => v.as_u64().and_then(|n| usize::try_from(n).ok()).unwrap_or_else(|| {
            tracing::warn!("[config] retry.max_attempts 非法，回退默认 5");
            5
        }),
        None => 5,
    };
    let base_ms = match obj.get("base_ms") {
        Some(v) => v.as_u64().filter(|&n| n > 0).unwrap_or_else(|| {
            tracing::warn!("[config] retry.base_ms 非法，回退默认 1000");
            1000
        }),
        None => 1000,
    };
    let max_ms = match obj.get("max_ms") {
        Some(v) => v.as_u64().filter(|&n| n > 0).unwrap_or_else(|| {
            tracing::warn!("[config] retry.max_ms 非法，回退默认 30000");
            30000
        }),
        None => 30000,
    };
    if max_ms < base_ms {
        tracing::warn!("[config] retry.max_ms({max_ms}) < base_ms({base_ms})，回退 max_ms=base_ms");
        RetrySettings { enabled, max_attempts, base_ms, max_ms: base_ms }
    } else {
        RetrySettings { enabled, max_attempts, base_ms, max_ms }
    }
}

/// 解析入口协议声明（对齐 `_OPENAI_API_VALUES` 校验）。
fn parse_openai_api(s: &str, svc_name: &str) -> Option<OpenaiApi> {
    match s {
        "" => None,
        "anthropic-messages" => Some(OpenaiApi::AnthropicMessages),
        "openai-completions" => Some(OpenaiApi::OpenaiCompletions),
        "openai-responses" => Some(OpenaiApi::OpenaiResponses),
        _ => {
            tracing::warn!(
                "[config] 服务 {} 的 api '{}' 不是已知协议，回退 auto",
                svc_name,
                s
            );
            None
        }
    }
}

fn parse_thinking_mode(v: Option<&Value>, svc_name: &str) -> ThinkingMode {
    let raw = v
        .map(|x| match x {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        })
        .unwrap_or_else(|| "auto".to_string());
    let raw = if raw.is_empty() { "auto".to_string() } else { raw };
    match raw.as_str() {
        "auto" => ThinkingMode::Auto,
        "passthrough" => ThinkingMode::Passthrough,
        "effort" => ThinkingMode::Effort,
        "enable_thinking" => ThinkingMode::EnableThinking,
        "none" => ThinkingMode::None,
        _ => {
            tracing::warn!(
                "[config] 服务 {} 的 thinking_mode '{}' 非法，回退 auto",
                svc_name,
                raw
            );
            ThinkingMode::Auto
        }
    }
}

/// 从指定路径加载服务列表（对齐 `load_config` 主体；路径解析差异见 crate 文档）。
///
/// 行为要点：
/// - config.json 不存在/解析失败 → 空服务列表；仅配 DASHSCOPE_API_KEY 时回退单服务
/// - 旧结构（services 内嵌 openai_base_url/key）与账号引用缺失 → 自动迁移生成账号
/// - 字段非法一律警告 + 回退默认；services[].mode 非法 → 该服务整体跳过
pub fn load_config_at(config_path: &Path) -> Vec<Service> {
    let mut services: Vec<Service> = Vec::new();

    let raw = fs::read_to_string(config_path).unwrap_or_default();
    let mut config: Value = serde_json::from_str(&raw).unwrap_or(Value::Object(Map::new()));

    if config.as_object().map(|o| !o.is_empty()).unwrap_or(false) {
        ensure_service_ids(config_path, &mut config);

        let services_raw = config
            .get("services")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let accounts_raw = config
            .get("accounts")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let auth = load_auth_from(&resolve_auth_path(config_path));
        let top_auth_token = config
            .get("auth_token")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();

        // 账号解析
        let mut accounts: HashMap<String, Account> = HashMap::new();
        for (i, a) in accounts_raw.iter().enumerate() {
            let id = a
                .get("id")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("acc-{}", i + 1));
            let name = a
                .get("name")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .unwrap_or_else(|| {
                    a.get("id")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| format!("账号{}", i + 1))
                });
            let embedded = a.get("api_key").and_then(Value::as_str).unwrap_or("");
            accounts.insert(
                id.clone(),
                Account {
                    api_key: resolve_api_key(&auth, &id, &name, embedded),
                    openai_url: normalize_openai_url(a.get("openai_url").and_then(Value::as_str).unwrap_or("")),
                    anthropic_url: a
                        .get("anthropic_url")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .trim()
                        .to_string(),
                    api: a
                        .get("api")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .trim()
                        .to_string(),
                    quota_source: a
                        .get("quota_source")
                        .and_then(Value::as_str)
                        .unwrap_or("auto")
                        .trim()
                        .to_string(),
                    quota: a.get("quota").and_then(Value::as_object).cloned(),
                    id,
                    name,
                },
            );
        }

        let mode_to_client = |mode: &str| match mode {
            "claude" | "direct" => "anthropic",
            "codex" => "openai",
            _ => "auto",
        };

        for (i, svc_raw) in services_raw.iter().enumerate() {
            // mode 字段：仅作旧结构 client 推导与非法跳过判定
            let mode_str = svc_raw.get("mode").and_then(Value::as_str).unwrap_or("claude");
            if !matches!(mode_str, "claude" | "codex" | "direct" | "auto") {
                continue; // 未知模式跳过
            }
            let Some(svc) = svc_raw.as_object() else {
                continue;
            };
            let svc_name_for_warn = svc
                .get("comment")
                .and_then(Value::as_str)
                .unwrap_or("（未命名）")
                .to_string();

            let acc_id = svc.get("account").and_then(Value::as_str).unwrap_or("");
            let account = match accounts.get(acc_id) {
                Some(a) => a.clone(),
                None => {
                    // 自动迁移：旧格式（services 内嵌 url/key）或引用缺失时按服务生成账号
                    let id = format!("acc-{}", i + 1);
                    let name = svc
                        .get("comment")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| format!("账号{}", i + 1));
                    let embedded = svc
                        .get("openai_api_key")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    let acc = Account {
                        api_key: resolve_api_key(&auth, &id, &name, embedded),
                        openai_url: normalize_openai_url(
                            svc.get("openai_base_url").and_then(Value::as_str).unwrap_or(""),
                        ),
                        anthropic_url: svc
                            .get("anthropic_base_url")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                        api: svc.get("api").and_then(Value::as_str).unwrap_or("").to_string(),
                        quota_source: "auto".to_string(),
                        quota: None,
                        id: id.clone(),
                        name,
                    };
                    accounts.insert(id, acc.clone());
                    acc
                }
            };

            // 入口协议：服务级 api 优先，回退账号级 api
            let svc_api = svc.get("api").and_then(Value::as_str).unwrap_or("").trim();
            let api_str = if !svc_api.is_empty() {
                svc_api
            } else {
                account.api.trim()
            };
            let api = parse_openai_api(api_str, &svc_name_for_warn);

            let upstream_api = match svc.get("upstream_api").and_then(Value::as_str).unwrap_or("openai-completions")
            {
                "openai-responses" => UpstreamApi::OpenaiResponses,
                "openai-completions" => UpstreamApi::OpenaiCompletions,
                other => {
                    tracing::warn!(
                        "[config] 服务 {} 的 upstream_api '{}' 非法，回退 openai-completions",
                        svc_name_for_warn,
                        other
                    );
                    UpstreamApi::OpenaiCompletions
                }
            };

            let thinking_mode = parse_thinking_mode(svc.get("thinking_mode"), &svc_name_for_warn);

            let client_raw = svc
                .get("client")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| mode_to_client(mode_str));
            let client = match client_raw {
                "anthropic" => ClientKind::Anthropic,
                "openai" => ClientKind::Openai,
                _ => ClientKind::Auto,
            };

            // pricing：对象形式校验后原样保留；字符串仅 "" / "none" 合法
            let pricing_raw = svc.get("pricing").cloned().unwrap_or(Value::String(String::new()));
            match &pricing_raw {
                Value::Object(_) => {
                    normalize_pricing_value(&pricing_raw); // 校验（非法 mode 回退 token）
                }
                Value::String(s) if s.is_empty() || s == "none" => {}
                Value::Null => {}
                Value::String(s) => {
                    tracing::warn!(
                        "[config] 服务 {} 的 pricing '{}' 非法，忽略（仅支持 none / 对象形式）",
                        svc_name_for_warn,
                        s
                    );
                }
                _ => {}
            }
            let (pricing_mode, pricing_extra) = normalize_pricing_value(&pricing_raw);

            let auth_token = svc
                .get("auth_token")
                .map(|v| match v {
                    Value::String(s) => s.trim().to_string(),
                    Value::Null => String::new(),
                    other => other.to_string().trim().to_string(),
                })
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| top_auth_token.clone());

            let model_policy = match svc.get("model_policy").and_then(Value::as_str).unwrap_or("clamp") {
                "reject" => ModelPolicy::Reject,
                "passthrough" => ModelPolicy::Passthrough,
                "clamp" => ModelPolicy::Clamp,
                other => {
                    tracing::warn!(
                        "[config] 服务 {} 的 model_policy '{}' 非法，回退 clamp",
                        svc_name_for_warn,
                        other
                    );
                    ModelPolicy::Clamp
                }
            };

            let context_1m = svc.get("context_1m").map(truthy).unwrap_or(false);
            let max_tokens = svc
                .get("max_tokens")
                .map(|v| coerce_int(v, if context_1m { 1_000_000 } else { 4096 }))
                .unwrap_or(if context_1m { 1_000_000 } else { 4096 });

            let models = svc
                .get("models")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .map(value_to_string)
                        .filter(|s| !s.trim().is_empty())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let models_map = svc
                .get("models_map")
                .and_then(Value::as_object)
                .map(|obj| {
                    obj.iter()
                        .filter(|(_, v)| !value_to_string(v).trim().is_empty())
                        .map(|(k, v)| (k.clone(), value_to_string(v)))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();

            let name = svc
                .get("comment")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .unwrap_or_else(|| {
                    svc.get("model")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| mode_str.to_string())
                });

            services.push(Service {
                id: svc
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim()
                    .to_string(),
                order: svc.get("order").map(|v| coerce_int(v, i as i64)).unwrap_or(i as i64),
                enabled: svc.get("enabled").map(|v| v != &Value::Bool(false) && v != &Value::String("false".to_string())).unwrap_or(true),
                autostart: svc.get("autostart").map(|v| v == &Value::Bool(true) || v == &Value::String("true".to_string())).unwrap_or(false),
                models,
                models_map: ModelsMap(models_map),
                model_policy,
                name,
                port: svc
                    .get("listen_address")
                    .map(|v| coerce_int(v, 8317))
                    .unwrap_or(8317),
                host: svc
                    .get("listen_host")
                    .and_then(Value::as_str)
                    .unwrap_or("127.0.0.1")
                    .to_string(),
                model: svc
                    .get("model")
                    .and_then(Value::as_str)
                    .unwrap_or("qwen-plus")
                    .to_string(),
                override_model: svc.get("override_model").map(truthy).unwrap_or(true),
                max_tokens,
                proxy: std::env::var("HTTP_PROXY").unwrap_or_default(),
                api,
                upstream_api,
                thinking_mode,
                pricing_mode,
                pricing_extra,
                pricing_raw,
                auth_token,
                account,
                client,
                mode_override: None,
            });
        }
    }

    if services.is_empty() {
        // 回退：环境变量配置（单服务）
        let api_key = crate::paths::env_value("DASHSCOPE_API_KEY").unwrap_or_default();
        if !api_key.is_empty() {
            let target = crate::paths::env_value("DASHSCOPE_URL").unwrap_or_else(|| {
                "https://dashscope.aliyuncs.com/compatible-mode/v1/chat/completions".to_string()
            });
            let host = crate::paths::env_value("PROXY_HOST").unwrap_or_else(|| "127.0.0.1".to_string());
            let port = crate::paths::env_value("PROXY_PORT")
                .and_then(|v| v.parse::<i64>().ok())
                .unwrap_or(8317);
            let model = crate::paths::env_value("PROXY_MODEL").unwrap_or_else(|| "qwen-plus".to_string());
            let max_tokens = crate::paths::env_value("PROXY_MAX_TOKENS")
                .and_then(|v| v.parse::<i64>().ok())
                .unwrap_or(4096);
            services.push(Service {
                id: new_service_id(),
                name: "default".to_string(),
                account: Account {
                    id: "acc-env".to_string(),
                    name: "环境变量账号".to_string(),
                    api_key,
                    openai_url: normalize_openai_url(&target),
                    anthropic_url: String::new(),
                    api: String::new(),
                    quota_source: "auto".to_string(),
                    quota: None,
                },
                client: ClientKind::Auto,
                host,
                port,
                model,
                override_model: true,
                max_tokens,
                proxy: std::env::var("HTTP_PROXY").unwrap_or_default(),
                api: None,
                upstream_api: UpstreamApi::OpenaiCompletions,
                thinking_mode: ThinkingMode::Auto,
                pricing_mode: PricingMode::Token,
                pricing_extra: None,
                pricing_raw: Value::String(String::new()),
                auth_token: String::new(),
                order: 0,
                enabled: true,
                autostart: false,
                models: Vec::new(),
                models_map: ModelsMap::default(),
                model_policy: ModelPolicy::Clamp,
                mode_override: None,
            });
        }
    }
    services
}

fn value_to_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// 生产入口：按 O2A_CONFIG（缺省 cwd/config.json）加载。
pub fn load_config() -> Vec<Service> {
    load_config_at(&resolve_config_path())
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_service_id_shape() {
        let id = new_service_id();
        assert!(id.starts_with("svc-"));
        assert_eq!(id.len(), 12);
        assert!(id[4..].chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn pricing_mode_default_is_token() {
        let (m, e) = normalize_pricing_value(&Value::String(String::new()));
        assert_eq!(m, PricingMode::Token);
        assert!(e.is_none());
    }

    #[test]
    fn load_auth_missing_file_is_empty() {
        let auth = load_auth_from(Path::new("/nonexistent/auth.json"));
        assert!(auth.is_empty());
    }
}
