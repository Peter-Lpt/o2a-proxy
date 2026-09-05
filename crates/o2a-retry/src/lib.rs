//! o2a-retry：上游重试机制 —— 厂商无关通用核心 + 厂商判据表实现。
//!
//! 模块划分：
//! - [`retry`]：通用核心 —— 判定框架（`Retry` / `Category` / `ErrorInfo`）、`Backoff` 指数退避、
//!   `Retry-After` 解析、`retry_upstream` 重放循环、透传钩子（日志 + 补标准头）。
//!   顶层 `pub use retry::*` 重导出，常用调用直接 `o2a_retry::retry_upstream(...)`。
//! - [`qianwen`]：千问(公有云平台 / DashScope compatible-mode)特有 429 判据表实现，
//!   以分类函数形式注入通用核心。
//!
//! 接线惯例：engine 侧 codex/claude 分支注入 `o2a_retry::qianwen::classify`；
//! Anthropic 直连(direct)注入 `o2a_retry::classify`（通用 HTTP 规则）。
//! 详细设计见 docs/retry-design.md。

pub mod qianwen;
pub mod retry;

pub use retry::*;