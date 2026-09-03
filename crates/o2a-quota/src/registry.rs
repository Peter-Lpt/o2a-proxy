//! 额度适配器注册表 + auto 域名嗅探（对齐 Python `o2a/quota/registry.py`）。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use o2a_config::Account;
use serde_json::Value;
use url::Url;

use crate::adapters::{self, codex::OpenAICodexAdapter, declarative::DeclarativeQuotaAdapter, local::LocalQuotaAdapter, local_rolling_5h::LocalRolling5hAdapter, manual::ManualQuotaAdapter, opencode_go::OpenCodeGoAdapter, openrouter::OpenRouterAdapter, zai::ZaiAdapter};
use crate::base::{make_snapshot, QuotaContext, TTLCache, UPSTREAM_TIMEOUT_S};

/// 适配器协议：fetch(ctx) -> snapshot | None。
///
/// fetch 内允许抛 QuotaError / 任意错误 —— 注册表统一捕获降级。
/// Python 的同步 fetch 与 async fetch 双轨在此统一为 async（engine 为 tokio 异步）。
#[async_trait::async_trait]
pub trait QuotaAdapter: Send + Sync {
    fn name(&self) -> &'static str;
    fn source(&self) -> &'static str {
        "local_stats"
    }
    async fn fetch(&self, ctx: &QuotaContext) -> Result<Option<Value>, crate::base::QuotaError>;
}

/// 注册表：适配器按名字索引（新增一个供应商 = 新文件 + register 一行）。
pub struct Registry {
    adapters: HashMap<&'static str, Arc<dyn QuotaAdapter>>,
}

impl Default for Registry {
    fn default() -> Self {
        Registry::standard()
    }
}

impl Registry {
    /// 全部 8 个内置适配器（对齐 Python 模块级 register 调用）。
    pub fn standard() -> Self {
        let mut r = Registry {
            adapters: HashMap::new(),
        };
        r.register(Arc::new(LocalQuotaAdapter));
        r.register(Arc::new(LocalRolling5hAdapter));
        r.register(Arc::new(ManualQuotaAdapter));
        r.register(Arc::new(OpenRouterAdapter));
        r.register(Arc::new(DeclarativeQuotaAdapter));
        r.register(Arc::new(OpenCodeGoAdapter));
        r.register(Arc::new(ZaiAdapter));
        r.register(Arc::new(OpenAICodexAdapter));
        r
    }

    pub fn register(&mut self, adapter: Arc<dyn QuotaAdapter>) {
        self.adapters.insert(adapter.name(), adapter);
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn QuotaAdapter>> {
        self.adapters.get(name).cloned()
    }

    pub fn contains(&self, name: &str) -> bool {
        self.adapters.contains_key(name)
    }
}

/// 已注册适配器名（排序），对齐 `registered_adapters`。
pub fn registered_adapters() -> Vec<String> {
    Registry::standard()
        .adapters
        .keys()
        .map(|s| s.to_string())
        .collect()
}

// 域名嗅探表：子串 → 适配器名（auto 用）
const SNIFF: [(&str, &str); 6] = [
    ("openrouter.ai", "openrouter"),
    ("opencode.ai", "opencode-go"),
    ("chatgpt.com", "codex"),
    ("openai.com", "codex"),
    ("bigmodel.cn", "zai"),
    ("z.ai", "zai"),
];

// 显式名 → 适配器名别名（支持套餐名等用户友好写法）
const ALIASES: [(&str, &str); 9] = [
    ("glm-coding-plan", "zai"),
    ("glm", "zai"),
    ("codex_zen", "zai"),
    ("opencode_go", "opencode-go"),
    ("openai-codex", "codex"),
    ("openai_codex", "codex"),
    ("gpt", "codex"),
    ("chatgpt", "codex"),
    ("openai", "codex"),
];

// 预留显式名（尚未实现 → 回退 local）
const RESERVED: [&str; 3] = ["anthropic", "zen", "generic"];

/// 选择适配器名（对齐 `resolve_adapter_name`，含显式未注册名继续嗅探的语义）。
pub fn resolve_adapter_name(registry: &Registry, account: &Account) -> String {
    let mut source = account.quota_source.trim().to_string();
    if source.is_empty() {
        source = "auto".into();
    }
    if RESERVED.contains(&source.as_str()) {
        return "local".into();
    }
    let source = ALIASES
        .iter()
        .find(|(a, _)| *a == source)
        .map(|(_, b)| b.to_string())
        .unwrap_or(source);
    if source != "auto" && registry.contains(&source) {
        return source;
    }
    let url = &account.openai_url;
    let host = Url::parse(url)
        .map(|u| {
            let mut netloc = u.host_str().unwrap_or("").to_lowercase();
            if let Some(port) = u.port() {
                netloc.push_str(&format!(":{}", port));
            }
            netloc
        })
        .unwrap_or_default();
    for (frag, name) in SNIFF {
        if host.contains(frag) {
            return name.into();
        }
    }
    // auto 且手填了额度上限（quota.limit）→ manual 套餐（冷启动兜底）
    if source == "auto" {
        if let Some(quota) = &account.quota {
            if quota.get("limit").map(crate::base::truthy).unwrap_or(false) {
                return "manual".into();
            }
        }
    }
    "local".into()
}

/// 取额度快照：优先缓存 → 注册表适配器（1.5s 超时）→ 失败降级 local（标 stale）→
/// local 也失败 → 最小空窗口快照（标 stale）。适配器异常/超时绝不外泄。
///
/// degrade=false（对齐 Python `degrade` 参数）：不降级、不缓存，直接返回 None。
pub async fn get_snapshot_async(
    registry: &Registry,
    account: &Account,
    ctx: &QuotaContext,
    ttl_cache: Option<&TTLCache>,
    degrade: bool,
) -> Option<Value> {
    let key = account.id.clone();
    if let Some(cache) = ttl_cache {
        if let Some(cached) = cache.get(&key) {
            return Some(cached);
        }
    }
    let name = resolve_adapter_name(registry, account);
    let mut snapshot = None;
    if let Some(adapter) = registry.get(&name) {
        let fut = adapter.fetch(ctx);
        if let Ok(Ok(Some(s))) =
            tokio::time::timeout(Duration::from_secs_f64(UPSTREAM_TIMEOUT_S), fut).await
        {
            snapshot = Some(s);
        }
    }
    finalize(registry, snapshot, account, ctx, &name, ttl_cache, degrade).await
}

/// 降级 + 缓存写回（对齐 `_finalize`）。
async fn finalize(
    registry: &Registry,
    snapshot: Option<Value>,
    account: &Account,
    ctx: &QuotaContext,
    name: &str,
    ttl_cache: Option<&TTLCache>,
    degrade: bool,
) -> Option<Value> {
    let mut snapshot = snapshot;
    if snapshot.is_none() && degrade && name != "local" {
        if let Some(local) = registry.get("local") {
            let fut = local.fetch(ctx);
            if let Ok(Ok(Some(mut s))) =
                tokio::time::timeout(Duration::from_secs_f64(UPSTREAM_TIMEOUT_S), fut).await
            {
                if let Value::Object(o) = &mut s {
                    o.insert("stale".into(), Value::Bool(true));
                }
                snapshot = Some(s);
            }
        }
    }
    if snapshot.is_none() && degrade {
        // local 也失败（如统计目录缺失）→ 最小快照，标 stale
        snapshot = Some(make_snapshot(
            "local",
            Vec::new(),
            "local_stats",
            None,
            true,
            &ctx.now(),
        ));
    }
    if let (Some(s), Some(cache)) = (&snapshot, ttl_cache) {
        cache.set(&account.id.clone(), s.clone());
    }
    snapshot
}

// adapters 模块对外再导出，保持与 Python `o2a.quota.adapters` 相同的访问路径
pub use adapters::{
    codex, declarative, local, local_rolling_5h, manual, opencode_go, openrouter, zai,
};
