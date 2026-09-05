//! 上游重试 —— 厂商无关通用核心:错误分类(`Retry`/`ErrorInfo`)、退避(`Backoff`)、
//! 重放循环(`retry_upstream`)、透传钩子(日志 + 标准 Retry-After 头)。
//! 厂商判据表以分类函数注入(千问见 [`crate::qianwen`])。
//! 不变量:流式首字节下发后绝不重试;判定不可重试的错误不消耗重试预算。

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use o2a_config::RetrySettings;
use serde_json::Value;

// ---------------------------------------------------------------------------
// 判定结果
// ---------------------------------------------------------------------------

/// 重试判定结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Retry {
    /// 建议重试(自愈型限流/服务端瞬态),附限流类别供日志与指标使用。
    Retryable(Category),
    /// 不建议重试(计费/资源/客户端类错误),附原因。
    Permanent(&'static str),
}

/// 限流类别(厂商分类表可复用;`#[non_exhaustive]` 允许增量)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Category {
    /// RPS/RPM 频率限流
    Rps,
    /// TPS/TPM Token 消耗限流
    Tpm,
    /// 频率骤增触发稳定性保护
    BurstRate,
    /// 并发请求超限
    Concurrency,
    /// 其他自愈型配额/容量限流
    Quota,
    /// 服务端瞬态(408 / 5xx)
    ServerError,
}

impl Retry {
    /// 是否建议重试。
    pub fn retryable(&self) -> bool {
        matches!(self, Retry::Retryable(_))
    }

    /// 类别描述(指标上报/日志用)。
    #[allow(dead_code)]
    pub fn reason(&self) -> String {
        match self {
            Retry::Retryable(cat) => format!("retryable({cat:?})"),
            Retry::Permanent(r) => (*r).to_string(),
        }
    }
}

// ---------------------------------------------------------------------------
// 错误信息提取(兼容 DashScope / OpenAI / Anthropic 三种错误体形状)
// ---------------------------------------------------------------------------

/// 上游错误信息:HTTP 状态 + 提取出的 code / message。
#[derive(Debug, Clone)]
pub struct ErrorInfo {
    pub status: StatusCode,
    /// 错误码(如 API 的 `code`,或 OpenAI 风格的 `error.code` / `error.type`)。
    pub code: Option<String>,
    /// 错误消息(缺失时回退为原始 body 文本)。
    pub message: String,
}

impl ErrorInfo {
    /// 从原始响应解析。非 JSON body 时 code=None、message=原文。
    pub fn parse(status: StatusCode, body: &str) -> ErrorInfo {
        let (code, message) = extract_code_message(body);
        ErrorInfo {
            status,
            code,
            message: message.unwrap_or_else(|| body.to_string()),
        }
    }

    /// 用通用规则判定(HTTP 层面;无厂商错误码表)。
    #[allow(dead_code)] // 公开 API:厂商分类表未覆盖时调用方可直接使用
    pub fn classify(&self) -> Retry {
        classify(self.status, self.code.as_deref().unwrap_or(""), &self.message)
    }

    /// 用注入的厂商分类函数判定(如 `o2a_retry::qianwen::classify`)。
    pub fn classify_with(&self, classify: fn(StatusCode, &str, &str) -> Retry) -> Retry {
        classify(self.status, self.code.as_deref().unwrap_or(""), &self.message)
    }
}

/// 提取 code/message:
/// - DashScope:`{"code": "...", "message": "..."}`
/// - OpenAI:`{"error": {"code"|"type": "...", "message": "..."}}`
/// - Anthropic:`{"type": "error", "error": {"type": "...", "message": "..."}}`
fn extract_code_message(body: &str) -> (Option<String>, Option<String>) {
    let Ok(v) = serde_json::from_str::<Value>(body) else {
        return (None, None);
    };
    let code = v
        .get("code")
        .and_then(Value::as_str)
        .or_else(|| {
            v.get("error")
                .and_then(|e| e.get("code").and_then(Value::as_str))
        })
        .or_else(|| {
            v.get("error")
                .and_then(|e| e.get("type").and_then(Value::as_str))
        })
        .map(String::from);
    let message = v
        .get("message")
        .and_then(Value::as_str)
        .or_else(|| {
            v.get("error")
                .and_then(|e| e.get("message").and_then(Value::as_str))
        })
        .map(String::from);
    (code, message)
}

// ---------------------------------------------------------------------------
// 通用判定(HTTP 层面)
// ---------------------------------------------------------------------------

/// HTTP 层通用规则:408/5xx 可重试,429 默认可重试,其余 4xx 不可重试。
pub fn classify(status: StatusCode, _code: &str, _message: &str) -> Retry {
    if status == StatusCode::REQUEST_TIMEOUT || status.is_server_error() {
        return Retry::Retryable(Category::ServerError);
    }
    if status == StatusCode::TOO_MANY_REQUESTS {
        return Retry::Retryable(Category::Rps);
    }
    Retry::Permanent("non-retryable HTTP status (4xx or other)")
}

// ---------------------------------------------------------------------------
// 退避策略
// ---------------------------------------------------------------------------

/// 指数退避 + 抖动策略。`Retry-After` 优先于本地退避。
#[derive(Debug, Clone)]
pub struct Backoff {
    /// 最大重试次数;0 = 不限制。
    pub max_attempts: usize,
    /// 指数退避基数。
    pub base: Duration,
    /// 单次等待上限。
    pub max: Duration,
    /// 抖动比例(0.2 = 单次等待在 base*2^n 的 ±20% 内随机)。
    pub jitter_ratio: f64,
    /// 抖动 PRNG 种子(内部用,测试注入固定值以保持确定性)。
    pub seed: u64,
}

impl Default for Backoff {
    fn default() -> Self {
        Backoff::from_settings(&RetrySettings::default())
    }
}

impl Backoff {
    /// 由配置(`retry` 块)构造;抖动比与种子为内部常量。
    pub fn from_settings(s: &RetrySettings) -> Self {
        Backoff {
            max_attempts: s.max_attempts,
            base: Duration::from_millis(s.base_ms.max(1)),
            max: Duration::from_millis(s.max_ms.max(1)),
            jitter_ratio: 0.2,
            seed: Self::new_seed(),
        }
    }

    fn new_seed() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() ^ d.subsec_nanos() as u64)
            .unwrap_or(0x9E37_79B9_7F4A_7C15)
    }

    /// 确定性策略(测试用)。
    #[allow(dead_code)]
    pub fn sealed(max_attempts: usize, base: Duration, max: Duration, jitter_ratio: f64, seed: u64) -> Self {
        Backoff { max_attempts, base, max, jitter_ratio, seed }
    }

    /// 第 `attempt` 次(1-based)重试前应等待的时长。
    /// - 上游给了 `retry_after` → 直接采纳并封顶 `max`;
    /// - 否则 `base * 2^(attempt-1)` + 抖动,封顶 `max`,最低 1ms。
    pub fn delay(&self, attempt: usize, retry_after: Option<Duration>) -> Duration {
        if let Some(ra) = retry_after {
            return ra.min(self.max);
        }
        let max_ms = self.max.as_millis().max(1) as i64;
        let base_ms = self.base.as_millis().max(1) as i64;
        // 指数增长带哨兵,防溢出
        let factor = (attempt.saturating_sub(1)).min(20);
        let mut ms = base_ms.saturating_mul(1i64 << factor);
        if self.jitter_ratio > 0.0 {
            let span = (ms as f64 * self.jitter_ratio).round() as i64;
            let unit = unit_random(self.seed, attempt as u64); // [-1, 1)
            ms += (span as f64 * unit) as i64;
        }
        Duration::from_millis(ms.clamp(1, max_ms) as u64)
    }

    /// 重试次数是否已耗尽(0 = 不限制)。
    #[allow(dead_code)]
    pub fn exhausted(&self, attempt: usize) -> bool {
        self.max_attempts > 0 && attempt > self.max_attempts
    }
}

/// 确定性伪随机 [-1, 1)(xorshift64*,不引入 rand)。
fn unit_random(seed: u64, attempt: u64) -> f64 {
    let mut x = seed ^ attempt.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    x ^= x >> 30;
    x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x ^= x >> 27;
    x = x.wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^= x >> 31;
    ((x >> 11) as f64 / (1u64 << 53) as f64) * 2.0 - 1.0
}

// ---------------------------------------------------------------------------
// Retry-After 解析(支持秒数与 HTTP-date,按相对偏移)
// ---------------------------------------------------------------------------

/// 解析 `Retry-After` 头:秒数或 HTTP-date(相对偏移);失败返回 None(回退指数退避)。
#[allow(dead_code)]
pub fn parse_retry_after(headers: &HeaderMap) -> Option<Duration> {
    let raw = headers.get(header::RETRY_AFTER)?.to_str().ok()?.trim();
    if raw.is_empty() {
        return None;
    }
    // 秒数(最常见,如 "2" 或 " 5 ")
    if let Ok(secs) = raw.parse::<u64>() {
        return Some(Duration::from_secs(secs));
    }
    // HTTP-date → 未来时刻的等待秒数;过去的日期视为立即(0)。
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let at = chrono::DateTime::parse_from_rfc2822(raw)
        .or_else(|_| chrono::DateTime::parse_from_str(raw, "%a %b %e %H:%M:%S %Y"))
        .ok()?
        .timestamp();
    let secs = (at - now).max(0) as u64;
    Some(Duration::from_secs(secs))
}

// ---------------------------------------------------------------------------
// 引擎侧重放循环(默认关闭,见 o2a_config::RetrySettings)
// ---------------------------------------------------------------------------

/// 对已收到的失败响应做重试判定并(按需)重放上游,返回最终响应:
/// - 未启用重试 / 上游成功 / 判定不可重试 / 预算耗尽 → 原样返回(调用方按现状透传);
/// - 可重试且未耗尽 → 按退避(Retry-After 优先)重放 `send`,直到成功或耗尽。
///
/// 仅允许在「尚未向客户端下发任何字节」时调用(流式场景首字节后绝不可重放)。
///
/// `classify` 为厂商分类函数:千问平台传 `crate::qianwen::classify`,通用上游传
/// `crate::retry::classify`。
/// 重试最终结果:无法通过重试恢复的上游错误(需按现状透传)。
#[derive(Debug)]
pub struct RetryExhausted {
    /// 最后一帧 HTTP 状态。
    pub status: StatusCode,
    /// 最后一帧错误体原文(透传时作为消息)。
    pub body: String,
}

impl RetryExhausted {
    /// 未启用重试时的便捷路径:消费响应构造最终错误(不重放任何请求)。
    pub async fn from_response(resp: reqwest::Response) -> RetryExhausted {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        RetryExhausted { status, body }
    }
}

/// 对失败响应做重试判定并重放上游:
/// `Ok(Response)` = 成功/未启用(不重放);`Err(RetryExhausted)` = 不可重试、预算或网络
/// 瞬态耗尽,携带最后一帧真实错误的 status+body 供透传。仅限流式首字节下发前调用。
pub async fn retry_upstream<F, Fut>(
    settings: &RetrySettings,
    classify: fn(StatusCode, &str, &str) -> Retry,
    first: reqwest::Response,
    mut send: F,
) -> Result<reqwest::Response, RetryExhausted>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = reqwest::Result<reqwest::Response>>,
{
    if !settings.enabled || first.status().is_success() {
        return Ok(first);
    }
    let backoff = Backoff::from_settings(settings);
    let mut cur = first;
    let mut retries = 0usize;
    loop {
        // ---- 判定当前响应 ----
        let status = cur.status();
        if status.is_success() {
            return Ok(cur);
        }
        let headers = cur.headers().clone();
        let bytes = match cur.bytes().await {
            Ok(b) => b,
            // 读不到 body(连接中断等):视为无法判定/恢复,交调用方透传(空体)
            Err(_) => return Err(RetryExhausted { status, body: String::new() }),
        };
        let body = String::from_utf8_lossy(&bytes).into_owned();
        let retry_after = parse_retry_after(&headers);
        let decision = ErrorInfo::parse(status, &body).classify_with(classify);

        if !decision.retryable() {
            tracing::debug!(
                status = status.as_u16(),
                code = ErrorInfo::parse(status, &body).code.as_deref().unwrap_or(""),
                msg = body.as_str(),
                reason = decision.reason(),
                "upstream error NOT retryable; no more attempts, passthrough",
            );
            return Err(RetryExhausted { status, body });
        }

        let next_attempt = retries + 1;
        let wait = backoff.delay(next_attempt, retry_after);
        if backoff.exhausted(next_attempt) {
            tracing::warn!(
                status = status.as_u16(),
                retries,
                max = settings.max_attempts,
                msg = body.as_str(),
                "retry budget exhausted; passthrough final upstream error",
            );
            return Err(RetryExhausted { status, body });
        }

        tracing::warn!(
            status = status.as_u16(),
            code = ErrorInfo::parse(status, &body).code.as_deref().unwrap_or(""),
            category = decision.reason(),
            attempt = next_attempt,
            max = settings.max_attempts,
            retry_after = ?retry_after,
            wait_ms = wait.as_millis(),
            "retrying upstream after retryable error",
        );
        tokio::time::sleep(wait).await;
        retries += 1;

        // ---- 内层:重发直到拿到 HTTP 响应(网络瞬态在预算内继续) ----
        loop {
            match send().await {
                Ok(resp) => {
                    cur = resp;
                    break;
                }
                Err(e) => {
                    tracing::warn!(
                        retries,
                        max = settings.max_attempts,
                        error = %e,
                        "upstream send failed during retry; treating as transient",
                    );
                    if backoff.exhausted(retries + 1) {
                        tracing::warn!("retry budget exhausted on send failure; passthrough last HTTP error");
                        return Err(RetryExhausted { status, body });
                    }
                    tokio::time::sleep(backoff.delay(retries + 1, None)).await;
                    retries += 1;
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 透传钩子:日志 + 标准 Retry-After 头(调用方按厂商分类函数注入)
// ---------------------------------------------------------------------------

/// 结构化记录透传错误的重试判定:可重试 → `warn`(含类别/码/建议退避),不可重试 → `debug`。
pub fn record_retry_decision(
    status: StatusCode,
    body: &str,
    classify: fn(StatusCode, &str, &str) -> Retry,
) {
    let info = ErrorInfo::parse(status, body);
    match info.classify_with(classify) {
        Retry::Retryable(cat) => {
            tracing::warn!(
                status = status.as_u16(),
                category = format!("{cat:?}"),
                code = info.code.as_deref().unwrap_or(""),
                msg = info.message.as_str(),
                suggested_backoff = format!("{:?}", Backoff::default().delay(1, None)),
                "upstream error is retryable; passed through with Retry-After if absent, downstream CLI may retry",
            );
        }
        Retry::Permanent(reason) => {
            tracing::debug!(
                status = status.as_u16(),
                code = info.code.as_deref().unwrap_or(""),
                msg = info.message.as_str(),
                reason,
                "upstream error is NOT retryable; passed through as-is",
            );
        }
    }
}

/// 透传补头:错误可重试且缺 `Retry-After` 时按默认退避补标准值(供下游 CLI 遵循)。
pub fn attach_retry_after_if_missing(
    status: StatusCode,
    body: &str,
    headers: &mut HeaderMap,
    classify: fn(StatusCode, &str, &str) -> Retry,
) {
    if headers.contains_key(header::RETRY_AFTER) {
        return;
    }
    let info = ErrorInfo::parse(status, body);
    if !info.classify_with(classify).retryable() {
        return;
    }
    let secs = Backoff::default().delay(1, None).as_secs().max(1);
    if let Ok(v) = HeaderValue::from_str(&secs.to_string()) {
        headers.insert(header::RETRY_AFTER, v);
    }
}

// ---------------------------------------------------------------------------
// 单元测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generic_http_classification() {
        assert!(classify(StatusCode::REQUEST_TIMEOUT, "", "").retryable());
        for s in [
            StatusCode::INTERNAL_SERVER_ERROR,
            StatusCode::BAD_GATEWAY,
            StatusCode::SERVICE_UNAVAILABLE,
            StatusCode::GATEWAY_TIMEOUT,
        ] {
            assert!(classify(s, "", "").retryable(), "5xx should be retryable: {s}");
        }
        // 429 在通用层默认可重试(错误码细判由厂商分类表注入)
        assert!(classify(StatusCode::TOO_MANY_REQUESTS, "", "").retryable());
        for s in [
            StatusCode::BAD_REQUEST,
            StatusCode::UNAUTHORIZED,
            StatusCode::FORBIDDEN,
            StatusCode::NOT_FOUND,
        ] {
            assert!(!classify(s, "", "").retryable(), "4xx should be permanent: {s}");
        }
    }

    #[test]
    fn error_info_parse_shapes() {
        // DashScope 形状(解析层与厂商无关;判定由 qianwen::classify 负责)
        let info = ErrorInfo::parse(
            StatusCode::TOO_MANY_REQUESTS,
            r#"{"code":"SomeProvider.Code","message":"limit"}"#,
        );
        assert_eq!(info.code.as_deref(), Some("SomeProvider.Code"));
        assert!(info.classify().retryable()); // 通用层 429 默认可重试

        // OpenAI 风格
        let info = ErrorInfo::parse(
            StatusCode::TOO_MANY_REQUESTS,
            r#"{"error":{"type":"rate_limit_error","message":"slow down"}}"#,
        );
        assert_eq!(info.code.as_deref(), Some("rate_limit_error"));

        // Anthropic 风格(无 code)
        let info = ErrorInfo::parse(
            StatusCode::SERVICE_UNAVAILABLE,
            r#"{"type":"error","error":{"type":"overloaded_error","message":"overloaded"}}"#,
        );
        assert_eq!(info.code.as_deref(), Some("overloaded_error"));
        assert!(info.classify().retryable()); // 5xx 可重试

        // 非 JSON → code None、message 原文
        let info = ErrorInfo::parse(StatusCode::TOO_MANY_REQUESTS, "plain text body");
        assert!(info.code.is_none());
        assert_eq!(info.message, "plain text body");
        assert!(info.classify().retryable());
    }

    #[test]
    fn backoff_growth_and_cap() {
        // 无抖动:严格指数
        let b = Backoff::sealed(5, Duration::from_millis(100), Duration::from_secs(30), 0.0, 42);
        assert_eq!(b.delay(1, None), Duration::from_millis(100));
        assert_eq!(b.delay(2, None), Duration::from_millis(200));
        assert_eq!(b.delay(3, None), Duration::from_millis(400));
        assert_eq!(b.delay(4, None), Duration::from_millis(800));
        // 指数溢出哨兵 → 封顶
        assert_eq!(b.delay(100, None), Duration::from_secs(30));
    }

    #[test]
    fn backoff_retry_after_wins_and_caps() {
        let b = Backoff::default();
        assert_eq!(b.delay(3, Some(Duration::from_secs(2))), Duration::from_secs(2));
        // 超长 Retry-After 被封顶
        assert_eq!(b.delay(1, Some(Duration::from_secs(3600))), b.max);
    }

    #[test]
    fn backoff_jitter_within_bounds() {
        let b = Backoff::sealed(5, Duration::from_millis(1000), Duration::from_secs(30), 0.2, 7);
        let max_ms = 30000.0;
        for attempt in 1..=20u64 {
            let d = b.delay(attempt as usize, None).as_millis() as f64;
            let raw = 1000.0 * 2f64.powi(attempt as i32 - 1);
            // 抖动后要封顶到 max:上/下界按 clamp(x, 1, max) 计算
            let lo = (raw * 0.8).clamp(1.0, max_ms);
            let hi = (raw * 1.2).clamp(1.0, max_ms);
            assert!(d >= lo - 1.0 && d <= hi + 1.0, "attempt {attempt}: {d} not in [{lo},{hi}]");
        }
    }

    #[test]
    fn backoff_exhaustion() {
        let b = Backoff::sealed(5, Duration::from_secs(1), Duration::from_secs(30), 0.0, 0);
        assert!(!b.exhausted(1));
        assert!(!b.exhausted(5));
        assert!(b.exhausted(6));
        let unlimited = Backoff::sealed(0, Duration::from_secs(1), Duration::from_secs(30), 0.0, 0);
        assert!(!unlimited.exhausted(1000));
    }

    #[test]
    fn backoff_from_settings() {
        let s = RetrySettings {
            enabled: true,
            max_attempts: 3,
            base_ms: 250,
            max_ms: 2000,
        };
        let b = Backoff::from_settings(&s);
        assert_eq!(b.max_attempts, 3);
        assert_eq!(b.base, Duration::from_millis(250));
        assert_eq!(b.max, Duration::from_millis(2000));
        // 无抖动时严格指数
        let b0 = Backoff { jitter_ratio: 0.0, ..b };
        assert_eq!(b0.delay(1, None), Duration::from_millis(250));
        assert_eq!(b0.delay(3, None), Duration::from_millis(1000));
        assert_eq!(b0.delay(10, None), Duration::from_millis(2000)); // 封顶
    }

    #[test]
    fn parse_retry_after_header() {
        let h = HeaderMap::new();
        assert_eq!(parse_retry_after(&h), None);
        for (raw, expect) in [("2", Some(2u64)), (" 5 ", Some(5)), ("abc", None), ("", None)] {
            let mut h = HeaderMap::new();
            if !raw.is_empty() {
                h.insert(header::RETRY_AFTER, HeaderValue::from_str(raw).unwrap());
            }
            assert_eq!(
                parse_retry_after(&h).map(|d| d.as_secs()),
                expect,
                "raw={raw:?}"
            );
        }
        // HTTP-date(未来时刻)
        let mut h = HeaderMap::new();
        h.insert(
            header::RETRY_AFTER,
            HeaderValue::from_str("Fri, 31 Dec 9999 23:59:59 GMT").unwrap(),
        );
        assert!(parse_retry_after(&h).is_some());
    }

    #[test]
    fn attach_header_only_when_retryable() {
        // 429(通用默认可重试)→ 补头
        let mut h = HeaderMap::new();
        attach_retry_after_if_missing(
            StatusCode::TOO_MANY_REQUESTS,
            r#"{"code":"Whatever","message":"limit"}"#,
            &mut h,
            classify,
        );
        assert!(h.contains_key(header::RETRY_AFTER));

        // 已有头 → 不动
        let mut h = HeaderMap::new();
        h.insert(header::RETRY_AFTER, HeaderValue::from_static("30"));
        attach_retry_after_if_missing(
            StatusCode::TOO_MANY_REQUESTS,
            r#"{"code":"Whatever","message":"limit"}"#,
            &mut h,
            classify,
        );
        assert_eq!(h.get(header::RETRY_AFTER).unwrap(), "30");

        // 400 → 不补
        let mut h = HeaderMap::new();
        attach_retry_after_if_missing(StatusCode::BAD_REQUEST, "{\"message\":\"bad\"}", &mut h, classify);
        assert!(!h.contains_key(header::RETRY_AFTER));
    }

    #[test]
    fn record_decision_uses_injected_classifier() {
        // 注入的厂商分类器 dict:qwen 表把 CommodityNotPurchased 判为不可重试
        // (这里仅验证注入路径生效:用永久分类 fn 模拟)
        fn permanent(_s: StatusCode, _c: &str, _m: &str) -> Retry {
            Retry::Permanent("test-permanent")
        }
        // 不 panic 即为通过(日志 hook 接受任意分类函数)
        record_retry_decision(StatusCode::TOO_MANY_REQUESTS, "{}", permanent);
        record_retry_decision(StatusCode::SERVICE_UNAVAILABLE, "{}", classify);
    }
}