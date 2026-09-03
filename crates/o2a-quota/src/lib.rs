//! o2a-quota：订阅额度适配器（对齐 Python `o2a/quota/`，契约见 docs/rust-rewrite.md §9）。
//!
//! 隔离原则（与 Python 版一致）：
//! 1. 目录隔离：base + registry + 每适配器一文件；新增供应商 = 新文件 + 注册一行
//! 2. 依赖隔离：适配器只通过 [`base::QuotaContext`] 取数，禁止读 config/全局状态
//! 3. 失败隔离：适配器抛错/超时 → 返回 None（上层降级 local 并标 stale），绝不外泄
//! 4. 端隔离：HTTP 端点属于 engine（M5 接线），本 crate 只提供快照 API
//!
//! 与 Python 的已知差异（有意为之）：
//! - 只提供 async 入口 [`registry::get_snapshot_async`]（engine 是 tokio 单线程异步；
//!   Python 的同步 get_snapshot/run_until_complete 包装不再需要）
//! - `iter_records` 的文件扫描范围按 since 日期..=今天 全覆盖；Python 用
//!   `(now - since).days + 1` 推算天数，跨日滚动窗边缘（since 在昨日深夜、真实
//!   now 在今日凌晨）会漏扫昨日文件导致用量少计——Rust 修正该边界
//! - token_file 相对路径按当前工作目录解析（Python 按项目根 proxy.py 定位）

pub mod adapters;
pub mod base;
pub mod registry;
pub mod stats_util;

pub use base::{
    empty_window, make_snapshot, py_round1, window_start, NowFn, QuotaContext, QuotaError,
    TTLCache, UPSTREAM_TIMEOUT_S,
};
pub use registry::{
    get_snapshot_async, registered_adapters, resolve_adapter_name, QuotaAdapter, Registry,
};
