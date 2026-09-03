//! o2a-pricing：定价解析与求值（Rust 引擎侧）。
//!
//! 与 Python `o2a/pricing/` 同构，来源分两部分：
//! - `entry` / `resolve` / `evaluate`：从 `desktop/src-tauri/src/pricing.rs` 提取
//!   （实现不动，已由 `pricing/golden/cases.json` 29 用例 golden 固化）
//! - `schema` / `fingerprint` / `plans`：从 Python `o2a/pricing/schema.py` /
//!   `fingerprint.py` / `plans.py` 移植（桌面端 Rust 镜像不含，golden 只覆盖
//!   resolve/evaluate，本 crate 自带 fixture 单测钉死指纹与套餐语义）
//!
//! 模块对应：
//! - `schema`      o2a/pricing/schema.py（v1/v2/v3 归一化，fingerprint 依赖）
//! - `entry`       单条目归一化（元组形态，evaluate 用；desktop 版逐字保留）
//! - `resolve`     覆盖链：服务级 > 账号级 > 模型级 + v3 事件时间规则
//! - `evaluate`    求值：components × 用量 → total（f64）
//! - `fingerprint` 价格目录指纹（缓存失效用）
//! - `plans`       套餐目录（load_plans/get_plan/plan_windows_to_snapshot/指纹）

pub mod entry;
pub mod evaluate;
pub mod fingerprint;
pub mod plans;
pub mod resolve;
pub mod schema;

pub use entry::{entry_to_v2, parse_range};
pub use evaluate::{evaluate, resolve_cost};
pub use fingerprint::pricing_fingerprint;
pub use plans::{get_plan, load_plans, plan_windows_to_snapshot, plans_fingerprint};
pub use resolve::{resolve_entry, resolve_entry_at};
pub use schema::{normalize_pricing, validate_pricing_rules};

/// v1 缺省回退比例（与 Python schema.py 一致）
pub const CACHE_READ_RATIO: f64 = 0.2;
pub const CACHE_WRITE_RATIO: f64 = 1.0;

/// 缺省币种（与 Python schema.DEFAULT_CURRENCY 一致）
pub const DEFAULT_CURRENCY: &str = "CNY";
