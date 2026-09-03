//! 路径解析与 URL 归一化（对齐 Python `o2a/base.py`）。

use std::env;
use std::path::{Path, PathBuf};

/// 读取环境变量并 trim；空串视为未设置（Python `.strip()` 后 truthy 判断）。
pub fn env_value(key: &str) -> Option<String> {
    env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// 环境变量值 → 具体路径：指向已存在目录或以分隔符结尾 → 目录下 filename；否则当作文件。
/// 对齐 Python `_resolve_config_path`。
pub fn resolve_env_path(env_val: &str, filename: &str) -> PathBuf {
    let p = Path::new(env_val);
    if p.is_dir() || env_val.ends_with('/') || env_val.ends_with('\\') {
        p.join(filename)
    } else {
        p.to_path_buf()
    }
}

/// config.json 路径：O2A_CONFIG（文件或目录），未设置时回退当前工作目录。
/// （Python 缺省回退 PROJECT_ROOT——由 proxy.py 标记定位；Rust 无该标记，
/// 桌面端总是显式传 O2A_CONFIG，CLI 单独使用时回退 cwd。）
pub fn resolve_config_path() -> PathBuf {
    match env_value("O2A_CONFIG") {
        Some(v) => resolve_env_path(&v, "config.json"),
        None => PathBuf::from("config.json"),
    }
}

/// auth.json 路径：O2A_AUTH（文件或目录）优先；缺省跟随 config.json 所在目录。
pub fn resolve_auth_path(config_path: &Path) -> PathBuf {
    match env_value("O2A_AUTH") {
        Some(v) => resolve_env_path(&v, "auth.json"),
        None => config_path
            .parent()
            .unwrap_or(Path::new("."))
            .join("auth.json"),
    }
}

/// 数据文件路径通用解析：env（文件或目录）> config.json 同目录 > 当前工作目录。
/// pricing.json / plans.json 依赖此规则（docs/rust-rewrite.md §3.1）。
pub fn resolve_data_file_path(env_key: &str, filename: &str, config_path: Option<&Path>) -> PathBuf {
    if let Some(v) = env_value(env_key) {
        return resolve_env_path(&v, filename);
    }
    if let Some(cp) = config_path {
        if let Some(dir) = cp.parent() {
            return dir.join(filename);
        }
    }
    PathBuf::from(filename)
}

/// pricing.json 路径：O2A_PRICING > config 同目录 > cwd。
pub fn resolve_pricing_path(config_path: Option<&Path>) -> PathBuf {
    resolve_data_file_path("O2A_PRICING", "pricing.json", config_path)
}

/// plans.json 路径：O2A_PLANS > config 同目录 > cwd。
pub fn resolve_plans_path(config_path: Option<&Path>) -> PathBuf {
    resolve_data_file_path("O2A_PLANS", "plans.json", config_path)
}

/// OpenAI 端点归一化为完整 chat/completions 地址（对齐 `_normalize_openai_url`）。
///
/// - "https://api.deepseek.com"        -> "https://api.deepseek.com/chat/completions"
/// - "https://.../compatible-mode/v1"  -> "https://.../compatible-mode/v1/chat/completions"
/// - "https://.../v1/chat/completions" -> 原样
pub fn normalize_openai_url(url: &str) -> String {
    let u = url.trim().trim_end_matches('/');
    if u.is_empty() || u.ends_with("/chat/completions") {
        u.to_string()
    } else {
        format!("{u}/chat/completions")
    }
}

/// 从归一化的 chat/completions 地址推导同基座 responses 端点（对齐 `_responses_url`）。
///
/// - https://api.deepseek.com/chat/completions -> https://api.deepseek.com/v1/responses
/// - https://x.com/v1/chat/completions         -> https://x.com/v1/responses
pub fn responses_url(chat_url: &str) -> String {
    let base = chat_url
        .strip_suffix("/chat/completions")
        .unwrap_or_else(|| chat_url.trim_end_matches('/'));
    if base.ends_with("/v1") {
        format!("{base}/responses")
    } else {
        format!("{base}/v1/responses")
    }
}
