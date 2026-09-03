//! OpenAI Codex / ChatGPT 订阅额度适配器：chatgpt.com/backend-api/wham/usage。
//!
//! 真正可用的 usage 端点是未公开的 `chatgpt.com/backend-api/wham/usage`，返回：
//! - `rate_limit.primary_window` / `secondary_window`：5 小时滚动、每周配额
//!   （OpenAI 会按 plan 切换这两个字段的顺序，因此按 `limit_window_seconds` 归类更稳）
//! - `credits`：按量余额 / unlimited
//!
//! 配置：
//!     accounts[].quota_source = "codex" | "gpt" | "openai-codex"  # 或 chatgpt.com 自动嗅探
//!     accounts[].quota = {
//!       "access_token": "…",                       # 方式一：直接给 access token
//!       "refresh_token": "…",                      # 可选，过期自动刷新（尽力写回原文件）
//!       "token_file": "~/.codex/auth.json",        # 方式二：从 Codex / pi / OpenCode auth 文件读取
//!       "usage_url": "https://chatgpt.com/backend-api/wham/usage"  # 可选覆盖
//!     }
//!
//! 网络失败抛 QuotaError → 注册表降级 local 并标 stale（断网可用）。

use std::path::PathBuf;
use std::time::Duration;

use serde_json::{json, Value};

use crate::base::{
    empty_window, make_snapshot, truthy, ts_to_local_naive, QuotaContext, QuotaError,
    UPSTREAM_TIMEOUT_S,
};
use crate::registry::QuotaAdapter;

/// OAuth 客户端 id（pi-codex-usage 中公开使用的 Codex CLI 客户端）
pub const DEFAULT_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
pub const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
pub const DEFAULT_USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";

pub struct OpenAICodexAdapter;

fn or_first(vals: Vec<Value>) -> Value {
    vals.into_iter()
        .find(truthy)
        .unwrap_or(Value::Null)
}

fn str_or_none(v: &Value) -> Option<String> {
    v.as_str().filter(|s| !s.is_empty()).map(|s| s.to_string())
}

/// 按优先级取 access/refresh/token_file：quota 显式 > Codex/pi/OpenCode auth 文件 > 账号 key。
/// 对齐 Python `_resolve_token`。
fn resolve_token(
    api_key: &str,
    cfg: &serde_json::Map<String, Value>,
) -> (String, Option<String>, String) {
    let direct = or_first(vec![
        cfg.get("access_token").cloned().unwrap_or(Value::Null),
        cfg.get("access").cloned().unwrap_or(Value::Null),
    ]);
    let refresh = or_first(vec![
        cfg.get("refresh_token").cloned().unwrap_or(Value::Null),
        cfg.get("refresh").cloned().unwrap_or(Value::Null),
    ]);
    let token_file = or_first(vec![
        cfg.get("token_file").cloned().unwrap_or(Value::Null),
        cfg.get("auth_file").cloned().unwrap_or(Value::Null),
    ]);
    let token_file = token_file.as_str().unwrap_or("").to_string();
    if let Some(d) = str_or_none(&direct) {
        return (d, str_or_none(&refresh), token_file);
    }
    if !token_file.is_empty() {
        if let Some(info) = token_from_file(&token_file) {
            if let Some(access) = info.access {
                return (access, info.refresh, token_file);
            }
        }
    }
    // 显式让账号 key 充当订阅 access token（适合手动粘贴短期 token 的场景）
    if truthy(cfg.get("use_api_key").unwrap_or(&Value::Null)) && !api_key.is_empty() {
        return (api_key.to_string(), None, token_file);
    }
    (api_key.to_string(), None, token_file)
}

struct TokenInfo {
    access: Option<String>,
    refresh: Option<String>,
}

fn norm_token(v: &Value) -> Option<TokenInfo> {
    let obj = v.as_object()?;
    let access = or_first(vec![
        obj.get("access_token").cloned().unwrap_or(Value::Null),
        obj.get("access").cloned().unwrap_or(Value::Null),
        obj.get("accessToken").cloned().unwrap_or(Value::Null),
    ]);
    let access = str_or_none(&access)?;
    let refresh = or_first(vec![
        obj.get("refresh_token").cloned().unwrap_or(Value::Null),
        obj.get("refresh").cloned().unwrap_or(Value::Null),
        obj.get("refreshToken").cloned().unwrap_or(Value::Null),
    ]);
    Some(TokenInfo {
        access: Some(access),
        refresh: str_or_none(&refresh),
    })
}

/// 相对路径按当前工作目录解析（Python 按项目根 proxy.py 定位——已知差异，见 lib.rs）。
fn resolve_token_path(token_file: &str) -> PathBuf {
    if let Some(stripped) = token_file.strip_prefix("~/") {
        if let Ok(home) = std::env::var(if cfg!(windows) { "USERPROFILE" } else { "HOME" }) {
            return PathBuf::from(home).join(stripped);
        }
    }
    let p = PathBuf::from(token_file);
    if p.is_absolute() {
        p
    } else {
        std::env::current_dir().unwrap_or_default().join(p)
    }
}

/// 从 Codex / pi / OpenCode auth 文件读取 token（三种常见形态）。
fn token_from_file(token_file: &str) -> Option<TokenInfo> {
    let path = resolve_token_path(token_file);
    let data: Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    let obj = data.as_object()?;
    // 常见形态：~/.codex/auth.json → {tokens: {access_token, refresh_token}}
    if let Some(info) = obj.get("tokens").and_then(norm_token) {
        return Some(info);
    }
    // ~/.pi/agent/auth.json → {openai-codex: {access, refresh, ...}}
    if let Some(info) = obj.get("openai-codex").and_then(norm_token) {
        return Some(info);
    }
    // OpenCode auth.json → {providers: {codex: {tokens: {access_token, ...}}}}
    if let Some(providers) = obj.get("providers").and_then(|p| p.as_object()) {
        for provider in ["codex", "openai-codex", "openai"] {
            let Some(pv) = providers.get(provider).and_then(|v| v.as_object()) else {
                continue;
            };
            let source = pv
                .get("tokens")
                .filter(|t| t.is_object())
                .cloned()
                .unwrap_or(Value::Object(pv.clone()));
            if let Some(info) = norm_token(&source) {
                return Some(info);
            }
        }
    }
    None
}

/// 尽力把刷新后的 token 写回原 auth 文件（refresh token 一次性，必须回写）。
/// 失败不影响额度查询；下次可能需重新登录。
fn write_back_token(token_file: &str, access_token: &str, refresh_token: &str) {
    let path = resolve_token_path(token_file);
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return;
    };
    let Ok(mut data) = serde_json::from_str::<Value>(&raw) else {
        return;
    };
    let Some(obj) = data.as_object_mut() else {
        return;
    };
    // Codex CLI ~/.codex/auth.json
    if let Some(tokens) = obj.get_mut("tokens").filter(|t| t.is_object()) {
        tokens["access_token"] = json!(access_token);
        tokens["refresh_token"] = json!(refresh_token);
    }
    // pi ~/.pi/agent/auth.json
    else if let Some(pi) = obj.get_mut("openai-codex").filter(|t| t.is_object()) {
        pi["access"] = json!(access_token);
        pi["refresh"] = json!(refresh_token);
    }
    // OpenCode auth.json providers
    else if let Some(providers) = obj.get_mut("providers").and_then(|p| p.as_object_mut()) {
        let mut done = false;
        for provider in ["codex", "openai-codex", "openai"] {
            let Some(pv) = providers.get_mut(provider).and_then(|v| v.as_object_mut()) else {
                continue;
            };
            if pv.get("tokens").map(|t| t.is_object()).unwrap_or(false) {
                pv["tokens"]["access_token"] = json!(access_token);
                pv["tokens"]["refresh_token"] = json!(refresh_token);
            } else {
                pv.insert("access_token".into(), json!(access_token));
                pv.insert("refresh_token".into(), json!(refresh_token));
            }
            done = true;
            break;
        }
        if !done {
            return;
        }
    } else {
        return;
    }
    let tmp = path.with_extension(format!(
        "{}.tmp",
        path.extension()
            .map(|e| e.to_string_lossy().to_string())
            .unwrap_or_default()
    ));
    let Ok(serialized) = serde_json::to_string_pretty(&data) else {
        return;
    };
    if std::fs::write(&tmp, serialized).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

/// 用 refresh token 换新 access token，并带回可能轮换的新 refresh token。
async fn refresh_access_token(
    session: &reqwest::Client,
    refresh_token: &str,
) -> Result<(String, Option<String>), QuotaError> {
    let resp = session
        .post(TOKEN_URL)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .timeout(Duration::from_secs_f64(UPSTREAM_TIMEOUT_S))
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", DEFAULT_CLIENT_ID),
        ])
        .send()
        .await
        .map_err(|e| QuotaError::new(format!("codex request failed: {}", e)))?;
    let status = resp.status().as_u16();
    if status != 200 {
        return Err(QuotaError::new(format!(
            "codex token refresh status {}",
            status
        )));
    }
    let data: Value = resp
        .json()
        .await
        .map_err(|e| QuotaError::new(format!("codex request failed: {}", e)))?;
    let Some(access) = str_or_none(data.get("access_token").unwrap_or(&Value::Null)) else {
        return Err(QuotaError::new("codex token refresh missing access_token"));
    };
    Ok((access, str_or_none(data.get("refresh_token").unwrap_or(&Value::Null))))
}

/// 把 wham/usage JSON 归一化为 (windows, plan)。
///
/// 5h 与 weekly 按 `limit_window_seconds` 归类：≤6h 视为 5h 滚动窗，其余按每周配额。
/// 辅助 rate limit 与 credits 尽力透出。
pub fn parse_usage(data: &Value) -> (Vec<Value>, Value) {
    let mut windows = Vec::new();
    let rate_limit = data
        .get("rate_limit")
        .cloned()
        .unwrap_or(Value::Null);
    for w in [
        rate_limit.get("primary_window"),
        rate_limit.get("secondary_window"),
    ]
    .into_iter()
    .flatten()
    {
        let Some(w) = w.as_object() else {
            continue;
        };
        let seconds = w
            .get("limit_window_seconds")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let Some(used_pct) = w.get("used_percent") else {
            continue;
        };
        if used_pct.is_null() {
            continue;
        }
        let kind = if seconds > 0 && seconds <= 21600 {
            "rolling"
        } else {
            "weekly"
        };
        let reset_at = w.get("reset_at").and_then(ts_to_local_naive);
        let reset_at =
            reset_at.map(|dt| dt.format("%Y-%m-%dT%H:%M:%S").to_string());
        windows.push(empty_window(
            kind,
            "percent",
            used_pct.as_f64().unwrap_or(0.0),
            Some(100.0),
            reset_at,
        ));
    }

    // 附加模型级限制（如 Codex Spark），命名 role 前缀避免与主窗混淆。
    let extra = data
        .get("additional_rate_limits")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for (idx, item) in extra.iter().enumerate() {
        let Some(item_obj) = item.as_object() else {
            continue;
        };
        let lim = item_obj
            .get("rate_limit")
            .filter(|l| l.is_object())
            .cloned()
            .unwrap_or_else(|| Value::Object(item_obj.clone()));
        let win = lim
            .get("primary_window")
            .filter(|w| w.is_object())
            .cloned();
        if let Some(win) = win {
            if let Some(used_pct) = win.get("used_percent") {
                if !used_pct.is_null() {
                    let label = or_first(vec![
                        item_obj.get("name").cloned().unwrap_or(Value::Null),
                        item_obj.get("label").cloned().unwrap_or(Value::Null),
                        item_obj.get("id").cloned().unwrap_or(Value::Null),
                        Value::String(format!("extra-{}", idx)),
                    ]);
                    let label = label.as_str().unwrap_or("").to_string();
                    windows.push(empty_window(
                        &label,
                        "percent",
                        used_pct.as_f64().unwrap_or(0.0),
                        Some(100.0),
                        None,
                    ));
                }
            }
        }
    }

    // credits / unlimited
    let credits = data.get("credits").cloned().unwrap_or(Value::Null);
    if credits.is_object() {
        if truthy(credits.get("unlimited").unwrap_or(&Value::Null)) {
            windows.push(json!({
                "kind": "credits", "unit": "usd", "used": 0, "limit": Value::Null,
                "pct": Value::Null, "reset_at": Value::Null, "value_label": "Unlimited",
            }));
        } else {
            let has_credits = credits.get("has_credits").cloned().unwrap_or(Value::Null);
            let balance = credits.get("balance").cloned().unwrap_or(Value::Null);
            if truthy(&has_credits) || !balance.is_null() {
                let balance_f = if truthy(&balance) {
                    balance.as_f64().unwrap_or(0.0)
                } else {
                    0.0
                };
                windows.push(json!({
                    "kind": "credits", "unit": "usd", "used": balance_f, "limit": Value::Null,
                    "pct": Value::Null, "reset_at": Value::Null,
                    "value_label": format!("${}", if truthy(&balance) { balance.to_string().trim_matches('"').to_string() } else { "0".to_string() }),
                }));
            }
        }
    }

    let plan_type = data
        .get("plan_type")
        .and_then(Value::as_str)
        .unwrap_or("ChatGPT");
    let mut plan = json!({"name": plan_type});
    if truthy(rate_limit.get("limit_reached").unwrap_or(&Value::Null)) {
        plan["limit_reached"] = Value::Bool(true);
    }
    (windows, plan)
}

#[async_trait::async_trait]
impl QuotaAdapter for OpenAICodexAdapter {
    fn name(&self) -> &'static str {
        "codex"
    }

    fn source(&self) -> &'static str {
        "provider_api"
    }

    async fn fetch(&self, ctx: &QuotaContext) -> Result<Option<Value>, QuotaError> {
        let Some(session) = &ctx.session else {
            return Ok(None);
        };
        let cfg = ctx.account.quota.clone().unwrap_or_default();
        let (mut token, refresh_token, token_file) =
            resolve_token(&ctx.account.api_key, &cfg);
        if token.is_empty() {
            return Err(QuotaError::new(
                "codex: missing access token (set quota.access_token / quota.token_file, or login with Codex CLI)",
            ));
        }
        let usage_url = str_or_none(cfg.get("usage_url").unwrap_or(&Value::Null))
            .unwrap_or_else(|| DEFAULT_USAGE_URL.to_string());

        let get_usage = |token: &str| {
            session
                .get(&usage_url)
                .header("Authorization", format!("Bearer {}", token))
                .header("Accept", "application/json")
                .timeout(Duration::from_secs_f64(UPSTREAM_TIMEOUT_S))
        };

        let result: Result<Value, QuotaError> = async {
            let resp = get_usage(&token)
                .send()
                .await
                .map_err(|e| QuotaError::new(format!("codex request failed: {}", e)))?;
            let status = resp.status().as_u16();
            if status == 401 {
                let Some(refresh) = &refresh_token else {
                    return Err(QuotaError::new(format!("codex usage status {}", status)));
                };
                let (new_access, new_refresh) = refresh_access_token(session, refresh).await?;
                if let Some(nr) = &new_refresh {
                    if !token_file.is_empty() {
                        write_back_token(&token_file, &new_access, nr);
                    }
                }
                token = new_access;
                let resp2 = get_usage(&token)
                    .send()
                    .await
                    .map_err(|e| QuotaError::new(format!("codex request failed: {}", e)))?;
                let status2 = resp2.status().as_u16();
                if status2 != 200 {
                    return Err(QuotaError::new(format!("codex usage status {}", status2)));
                }
                resp2.json::<Value>()
                    .await
                    .map_err(|e| QuotaError::new(format!("codex request failed: {}", e)))
            } else if status != 200 {
                Err(QuotaError::new(format!("codex usage status {}", status)))
            } else {
                resp.json::<Value>()
                    .await
                    .map_err(|e| QuotaError::new(format!("codex request failed: {}", e)))
            }
        }
        .await;

        let data = result?;
        let (windows, plan) = parse_usage(&data);
        let snap = make_snapshot(
            self.name(),
            windows,
            self.source(),
            Some(plan),
            false,
            &ctx.now(),
        );
        Ok(Some(snap))
    }
}
