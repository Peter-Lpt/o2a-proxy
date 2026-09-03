//! OpenCode Go 适配器：Cookie 工作区用量页优先，Bearer usage endpoint 兜底。
//!
//! 优先级（对齐 Python OpenCodeGoAdapter.fetch）：
//! 1. {base}/usage（v2 订阅窗口：rolling/weekly/monthly 百分比）→ {base}/v1/usage（base 非 /v1 时）
//!    同体旧形态（usage/limit 标量）兜底
//! 2. Cookie + workspace 工作区 SSR 页面（data-slot=usage-item 内联用量）
//! 3. 旧形态 /v1/usage、/usage 兜底
//!
//! 网络失败抛 QuotaError → 注册表降级 local 并标 stale（断网可用）。

use std::time::Duration;

use chrono::{NaiveDateTime};
use serde_json::Value;

use crate::adapters::openrouter::or_first;
use crate::base::{
    empty_window, make_snapshot, parse_resets_at, truthy, QuotaContext, QuotaError,
    UPSTREAM_TIMEOUT_S,
};
use crate::registry::QuotaAdapter;

pub struct OpenCodeGoAdapter;

/// 优先 quota.url；否则由账号 openai_url 去掉 /chat/completions 推导；缺省 opencode.ai。
fn resolve_base(ctx: &QuotaContext, cfg: &serde_json::Map<String, Value>) -> String {
    if let Some(u) = cfg.get("url").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        return u.trim_end_matches('/').to_string();
    }
    let oa = ctx.account.openai_url.trim().to_string();
    if let Some(stripped) = oa.strip_suffix("/chat/completions") {
        return stripped.trim_end_matches('/').to_string();
    }
    if oa.is_empty() {
        "https://api.opencode.ai".into()
    } else {
        oa.trim_end_matches('/').to_string()
    }
}

async fn get_text(
    session: &reqwest::Client,
    url: &str,
    headers: &[(&str, String)],
) -> Result<String, QuotaError> {
    let mut req = session.get(url).timeout(Duration::from_secs_f64(UPSTREAM_TIMEOUT_S));
    for (k, v) in headers {
        req = req.header(*k, v);
    }
    let resp = req
        .send()
        .await
        .map_err(|e| QuotaError::new(format!("HTTP error for {}: {}", url, e)))?;
    let status = resp.status().as_u16();
    if status != 200 {
        return Err(QuotaError::new(format!("HTTP {} for {}", status, url)));
    }
    resp.text()
        .await
        .map_err(|e| QuotaError::new(format!("read body failed for {}: {}", url, e)))
}

/// 解析 v2 订阅窗口：{"usage": {"rolling": {"percent":10,"resetsAt":"..."}, ...}}
fn extract_v2(data: &Value, now: &NaiveDateTime) -> Vec<Value> {
    let obj = match data.get("data") {
        Some(d) if d.is_object() => d.clone(),
        _ => data.clone(),
    };
    let Some(usage) = obj.get("usage").filter(|u| u.is_object()) else {
        return Vec::new();
    };
    let mut windows = Vec::new();
    for kind in ["rolling", "weekly", "monthly"] {
        let Some(w) = usage.get(kind).filter(|w| w.is_object()) else {
            continue;
        };
        let Some(percent) = w.get("percent") else {
            continue;
        };
        if percent.is_null() {
            continue;
        }
        let percent = percent.as_f64().unwrap_or(0.0).clamp(0.0, 100.0);
        let reset_raw = or_first(vec![
            w.get("resetsAt").cloned().unwrap_or(Value::Null),
            w.get("reset_at").cloned().unwrap_or(Value::Null),
        ]);
        let reset_at = parse_resets_at(&reset_raw).map(|dt| dt.format("%Y-%m-%dT%H:%M:%S").to_string());
        windows.push(empty_window(kind, "percent", percent, Some(100.0), reset_at));
    }
    let _ = now;
    windows
}

/// 解析旧形态 usage / limit 标量（data 包裹兼容，or 链 + dict 解包）。
fn extract(data: &Value) -> (Value, Value) {
    let obj = match data.get("data") {
        Some(d) if d.is_object() => d.clone(),
        _ => data.clone(),
    };
    let mut usage = obj.get("usage").cloned().unwrap_or(Value::Null);
    let mut limit = obj.get("limit").cloned().unwrap_or(Value::Null);
    if usage.is_null() {
        if let Some(inner) = obj.get("data").filter(|d| d.is_object()) {
            usage = inner.get("usage").cloned().unwrap_or(Value::Null);
            limit = inner.get("limit").cloned().unwrap_or(Value::Null);
        }
    }
    if usage.is_object() {
        usage = or_first(vec![
            usage.get("total").cloned().unwrap_or(Value::Null),
            usage.get("used").cloned().unwrap_or(Value::Null),
            usage.get("credits").cloned().unwrap_or(Value::Null),
        ]);
    }
    if limit.is_object() {
        limit = or_first(vec![
            limit.get("total").cloned().unwrap_or(Value::Null),
            limit.get("limit").cloned().unwrap_or(Value::Null),
        ]);
    }
    (usage, limit)
}

/// 工作区 id 仅做安全转义（对齐 Python `_quote`，避免引入额外依赖）。
fn quote(value: &str) -> String {
    value.replace('/', "%2F").replace('?', "%3F").replace('#', "%23")
}

fn label_to_kind(label: &str) -> Option<&'static str> {
    let lower = label.to_lowercase();
    if lower.starts_with("rolling") || lower.starts_with("滚动") {
        Some("rolling")
    } else if lower.starts_with("weekly") || lower.starts_with("每周") {
        Some("weekly")
    } else if lower.starts_with("monthly") || lower.starts_with("每月") {
        Some("monthly")
    } else {
        None
    }
}

/// 把 "2 hours 29 minutes" / "2 小时 29 分钟" 解析为秒（粗略展示用）。
fn parse_duration_to_sec(phrase: &str) -> i64 {
    if phrase.is_empty() {
        return 0;
    }
    let cleaned = regex::Regex::new(r"<!--[\s\S]*?-->")
        .map(|re| re.replace_all(phrase, " ").to_lowercase())
        .unwrap_or_else(|_| phrase.to_lowercase());
    let re = regex::Regex::new(
        r"(\d+)\s*(?:个\s*)?(second|minute|hour|day|week|month|year|秒|分钟|小时|天|周|月|年)s?",
    )
    .expect("static regex");
    let mut total = 0i64;
    for cap in re.captures_iter(&cleaned) {
        let n: i64 = cap[1].parse().unwrap_or(0);
        let unit = &cap[2];
        let secs = match unit {
            "second" | "秒" => 1,
            "minute" | "分钟" => 60,
            "hour" | "小时" => 3600,
            "day" | "天" => 86400,
            "week" | "周" => 604800,
            "month" | "月" => 2592000,
            "year" | "年" => 31536000,
            _ => 0,
        };
        total += secs * n;
    }
    total
}

fn ctx_iso(dt: &NaiveDateTime) -> String {
    dt.format("%Y-%m-%dT%H:%M:%S").to_string()
}

/// 解析工作区 SSR HTML 中的用量窗口（unit="percent"，0-100 整数）。
pub fn parse_ssr_windows(html: &str, now: &NaiveDateTime) -> Vec<Value> {
    let item_re = regex::Regex::new(r#"<div[^>]*data-slot="usage-item""#).expect("static regex");
    let label_re = regex::Regex::new(r#"data-slot="usage-label"[^>]*>([^<]+)<"#).expect("static regex");
    let value_re = regex::Regex::new(r#"data-slot="usage-value"[\s\S]*?<!--\$-->\s*(\d+)\s*<!--/-->"#)
        .expect("static regex");
    let reset_re = regex::Regex::new(
        r#"data-slot="reset-time"[\s\S]*?(?:Resets in|重置于)(?:<!--/-->\s*)?([\s\S]*?)(?:<!--/-->|</span>|</div>)"#,
    )
    .expect("static regex");
    let starts: Vec<usize> = item_re.find_iter(html).map(|m| m.start()).collect();
    let mut windows = Vec::new();
    for (i, &start) in starts.iter().enumerate() {
        let end = starts.get(i + 1).copied().unwrap_or(html.len());
        let block = &html[start..end];
        let (Some(label_cap), Some(value_cap)) = (label_re.captures(block), value_re.captures(block))
        else {
            continue;
        };
        let Some(kind) = label_to_kind(label_cap[1].trim()) else {
            continue;
        };
        let percent = value_cap[1]
            .parse::<f64>()
            .unwrap_or(0.0)
            .clamp(0.0, 100.0);
        let reset_sec = reset_re
            .captures(block)
            .map(|c| parse_duration_to_sec(c[1].as_ref()))
            .unwrap_or(0);
        let reset_at = if reset_sec > 0 {
            let dt = *now + chrono::Duration::seconds(reset_sec);
            Some(ctx_iso(&dt))
        } else {
            None
        };
        windows.push(empty_window(kind, "percent", percent, Some(100.0), reset_at));
    }
    windows
}

#[async_trait::async_trait]
impl QuotaAdapter for OpenCodeGoAdapter {
    fn name(&self) -> &'static str {
        "opencode-go"
    }

    fn source(&self) -> &'static str {
        "provider_api"
    }

    async fn fetch(&self, ctx: &QuotaContext) -> Result<Option<Value>, QuotaError> {
        let Some(session) = &ctx.session else {
            return Ok(None);
        };
        let cfg = ctx.account.quota.clone().unwrap_or_default();
        let base = resolve_base(ctx, &cfg);
        let mut last_err: Option<QuotaError> = None;
        let now = ctx.now();

        // 1) 从账号 OpenAI 兼容端点直接读 usage（v2 订阅窗口，旧形态兜底）
        if !ctx.account.api_key.is_empty() {
            let mut paths = vec!["/usage"];
            if !base.ends_with("/v1") {
                paths.push("/v1/usage");
            }
            for path in paths {
                let url = format!("{}{}", base, path);
                let resp = session
                    .get(&url)
                    .header("Authorization", format!("Bearer {}", ctx.account.api_key))
                    .timeout(Duration::from_secs_f64(UPSTREAM_TIMEOUT_S))
                    .send()
                    .await;
                let resp = match resp {
                    Ok(r) => r,
                    Err(e) => {
                        last_err = Some(QuotaError::new(format!(
                            "opencode-go {} request failed: {}",
                            path, e
                        )));
                        continue;
                    }
                };
                let status = resp.status().as_u16();
                if status != 200 {
                    last_err = Some(QuotaError::new(format!(
                        "opencode-go {} status {}",
                        path, status
                    )));
                    continue;
                }
                let data: Value = match resp.json().await {
                    Ok(d) => d,
                    Err(e) => {
                        last_err = Some(QuotaError::new(format!(
                            "opencode-go {} request failed: {}",
                            path, e
                        )));
                        continue;
                    }
                };
                let v2 = extract_v2(&data, &now);
                if !v2.is_empty() {
                    let snap =
                        make_snapshot(self.name(), v2, self.source(), None, false, &now);
                    return Ok(Some(snap));
                }
                let (usage, limit) = extract(&data);
                if !truthy(&usage) && !truthy(&limit) {
                    last_err = Some(QuotaError::new(format!(
                        "opencode-go {} no usage payload",
                        path
                    )));
                    continue;
                }
                let used = if truthy(&usage) {
                    usage.as_f64().unwrap_or(0.0)
                } else {
                    0.0
                };
                let limit_f = if truthy(&limit) {
                    limit.as_f64()
                } else {
                    None
                };
                let windows = vec![empty_window("month", "usd", used, limit_f, None)];
                let snap =
                    make_snapshot(self.name(), windows, self.source(), None, false, &now);
                return Ok(Some(snap));
            }
        }

        // 2) Cookie + workspace 路径：读取 OpenCode Go 控制台 SSR 页面
        let cookie = cfg.get("cookie").and_then(Value::as_str).unwrap_or("");
        let workspace_id = or_first(vec![
            cfg.get("workspace_id").cloned().unwrap_or(Value::Null),
            cfg.get("workspaceID").cloned().unwrap_or(Value::Null),
        ]);
        let workspace_id = workspace_id.as_str().unwrap_or("");
        if !cookie.is_empty() && !workspace_id.is_empty() {
            let url = format!("{}/workspace/{}/go", base, quote(workspace_id));
            match get_text(
                session,
                &url,
                &[
                    ("Cookie", cookie.to_string()),
                    ("Accept", "text/html".to_string()),
                ],
            )
            .await
            {
                Ok(html) => {
                    let windows = parse_ssr_windows(&html, &now);
                    if !windows.is_empty() {
                        let snap =
                            make_snapshot(self.name(), windows, self.source(), None, false, &now);
                        return Ok(Some(snap));
                    }
                    last_err = Some(QuotaError::new(
                        "opencode-go workspace page parsed empty (cookie expired or invalid?)",
                    ));
                }
                Err(e) => {
                    last_err = Some(QuotaError::new(format!(
                        "opencode-go workspace request failed: {}",
                        e
                    )));
                }
            }
        }

        // 3) 兼容兜底：Bearer usage endpoint（旧形态 OpenAI-style usage / limit）
        if !ctx.account.api_key.is_empty() {
            for path in ["/v1/usage", "/usage"] {
                let url = format!("{}{}", base, path);
                let resp = session
                    .get(&url)
                    .header("Authorization", format!("Bearer {}", ctx.account.api_key))
                    .timeout(Duration::from_secs_f64(UPSTREAM_TIMEOUT_S))
                    .send()
                    .await;
                let resp = match resp {
                    Ok(r) => r,
                    Err(e) => {
                        last_err = Some(QuotaError::new(format!(
                            "opencode-go {} request failed: {}",
                            path, e
                        )));
                        continue;
                    }
                };
                let status = resp.status().as_u16();
                if status != 200 {
                    last_err = Some(QuotaError::new(format!(
                        "opencode-go {} status {}",
                        path, status
                    )));
                    continue;
                }
                let data: Value = match resp.json().await {
                    Ok(d) => d,
                    Err(e) => {
                        last_err = Some(QuotaError::new(format!(
                            "opencode-go {} request failed: {}",
                            path, e
                        )));
                        continue;
                    }
                };
                let (usage, limit) = extract(&data);
                if !truthy(&usage) && !truthy(&limit) {
                    last_err = Some(QuotaError::new(format!(
                        "opencode-go {} no usage payload",
                        path
                    )));
                    continue;
                }
                let used = if truthy(&usage) {
                    usage.as_f64().unwrap_or(0.0)
                } else {
                    0.0
                };
                let limit_f = if truthy(&limit) {
                    limit.as_f64()
                } else {
                    None
                };
                let windows = vec![empty_window("month", "usd", used, limit_f, None)];
                let snap =
                    make_snapshot(self.name(), windows, self.source(), None, false, &now);
                return Ok(Some(snap));
            }
        }

        Err(last_err.unwrap_or_else(|| QuotaError::new("opencode-go unavailable")))
    }
}
