//! o2a-config：配置模型与加载（对齐 Python `o2a/base.py` + `o2a/config.py` 行为）。
//!
//! 模块划分：
//! - [`paths`]：O2A_CONFIG / O2A_AUTH / O2A_PRICING / O2A_PLANS 路径解析、URL 归一化
//! - [`model`]：Account / Service 数据模型与 mode 推导
//! - [`load`]：auth.json / config.json 加载、旧结构迁移、service_ids 惰性写回
//!
//! 与 Python 的已知差异（有意为之，见 docs/rust-rewrite.md）：
//! - 路径缺省不再向上查找 proxy.py 标记（桌面端总是显式传路径），回退 cwd
//! - listen_address / max_tokens 非法数值：Python 抛异常崩溃，Rust 回退默认值并警告
//! - Python load_config 通过环境变量 setdefault 传递统计设置（副作用），
//!   Rust 改为显式返回 [`load::StatsSettings`]（env 优先级不变）

pub mod load;
pub mod model;
pub mod paths;

pub use load::{
    ensure_service_ids, load_auth_from, load_config, load_config_at, new_service_id,
    resolve_api_key, resolve_retry_settings, resolve_stats_settings, RetrySettings,
    StatsSettings,
};
pub use model::{
    normalize_pricing_value, Account, AccountKind, ClientKind, DispatchMode, ModelsMap,
    ModelPolicy, OpenaiApi, PricingMode, Service, ThinkingMode, UpstreamApi,
};
pub use paths::{
    normalize_openai_url, resolve_auth_path, resolve_config_path, resolve_plans_path,
    resolve_pricing_path, responses_url,
};
