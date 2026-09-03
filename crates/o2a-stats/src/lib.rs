//! o2a-stats：缓存统计（JSONL 记录 + 小时聚合 + 计费重放 + 账号归并）。
//!
//! 行为基准：Python o2a/stats.py。契约见 docs/rust-rewrite.md §8。

pub mod meta;
pub mod pyround;
pub mod query;
pub mod registry;
pub mod stats;

pub use meta::{build_meta, ReqTiming};
pub use pyround::{py_round, thousands};
pub use query::{get_account_summary, ServiceSummaryRef, StatsEnv};
pub use registry::{is_cache_stats_enabled, StatsRegistry};
pub use stats::{CacheStats, CacheStatsConfig, Usage};
