//! 千问AI(公有云平台 / DashScope compatible-mode)上游错误的重试判定实现。
//!
//! 判据表依据平台错误文档 429 章节:Throttling.RateQuota / AllocationQuota / BurstRate /
//! Concurrency 等自愈型限流值得退避重试;CommodityNotPurchased / *BillOverdue / 免费额度
//! 耗尽等计费资源型重试必失败。以分类函数形式注入通用核心(`o2a_retry::retry_upstream`)。

use axum::http::StatusCode;

pub use crate::retry::{Category, Retry};

/// 千问 429 判据表 + 非 429 回退通用规则。
/// 注意:计费/资源型关键词必须先于“可重试关键词”匹配(避免消息里 "retry" 误判)。
pub fn classify(status: StatusCode, code: &str, message: &str) -> Retry {
    // 非 429 交给通用规则
    if status != StatusCode::TOO_MANY_REQUESTS {
        return crate::retry::classify(status, code, message);
    }

    // code 去分隔符(limit_requests → limitrequests),message 原样小写
    let c = code.to_ascii_lowercase().replace(['_', '-'], "");
    let m = message.to_ascii_lowercase();

    // --- 计费/资源型(先判,重试无意义) ---
    for k in ["commoditynotpurchased", "prepaidbilloverdue", "postpaidbilloverdue"] {
        if c.contains(k) {
            return Retry::Permanent("billing/order error; fix account before retry");
        }
    }
    if m.contains("free allocated quota exceeded") {
        return Retry::Permanent("free quota exhausted, no pay-as-you-go; switch model");
    }
    if m.contains("voice-clone") || m.contains("voice clone") {
        return Retry::Permanent("voice-clone limit exceeded; delete voices first");
    }
    if m.contains("fine-tune job") || m.contains("fine_tune") || m.contains("fine-tune") {
        return Retry::Permanent("fine-tune/resource limit; remove models or request quota");
    }

    // --- 自愈型限流 ---
    if c.contains("burst") {
        return Retry::Retryable(Category::BurstRate);
    }
    if c.contains("concurrency") || c.contains("concurrent") {
        return Retry::Retryable(Category::Concurrency);
    }
    if c.contains("allocation") || c.contains("insufficientquota") {
        return Retry::Retryable(Category::Tpm);
    }
    if c.contains("ratelimit") || c.contains("limitrequests") || c.contains("resourceexhausted")
        || c.contains("ratequota") || c.contains("throttling") || c.contains("toomanyrequests")
    {
        return Retry::Retryable(Category::Rps);
    }
    if c.contains("quota") || c.contains("capacity") {
        return Retry::Retryable(Category::Quota);
    }
    if m.contains("too many requests") || m.contains("rate limit")
        || m.contains("try again") || m.contains("retry")
        || m.contains("temporarily rate-limited") || m.contains("batch requests")
        || m.contains("increased too quickly")
    {
        return Retry::Retryable(Category::Rps);
    }

    // 兜底:429 且无任何识别特征 → 按限流常态处理
    Retry::Retryable(Category::Rps)
}

// ---------------------------------------------------------------------------
// 测试:429 章节判据表全表
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn retryable(code: &str, msg: &str) -> bool {
        classify(StatusCode::TOO_MANY_REQUESTS, code, msg).retryable()
    }

    #[test]
    fn retryable_429_codes() {
        for code in [
            "Throttling.RateQuota",
            "LimitRequests",
            "limit_requests",
            "ResourceExhausted",
            "Too many requests",
            "Throttling",
            "Throttling.BurstRate",
            "limit_burst_rate",
            "Throttling.AllocationQuota",
            "insufficient_quota",
            "Throttling.Concurrency",
        ] {
            assert!(retryable(code, ""), "code should be retryable: {code}");
        }
    }

    #[test]
    fn retryable_429_messages() {
        for msg in [
            "You have exceeded your request limit.",
            "Requests rate limit exceeded, please try again later.",
            "Request rate increased too quickly. To ensure system stability, please adjust your client logic to scale requests more smoothly over time.",
            "Too many requests in route. Please try again later.",
            "All models are temporarily rate-limited. Please try again in a few minutes.",
            "Too many requests. Batch requests are being throttled due to system capacity limits. Please try again later.",
        ] {
            assert!(retryable("", msg), "message should be retryable: {msg}");
        }
    }

    #[test]
    fn permanent_429_codes_and_messages() {
        for code in ["CommodityNotPurchased", "PrepaidBillOverdue", "PostpaidBillOverdue"] {
            assert!(!retryable(code, ""), "code should be permanent: {code}");
        }
        for msg in [
            "Free allocated quota exceeded.",
            "Maximum voice-clone voice limit exceeded.",
            "Too many fine-tune job in running, please retry later.",
            "Only 20 fine-tune job in running or succeeded allowed per user.",
        ] {
            assert!(!retryable("", msg), "message should be permanent: {msg}");
        }
    }

    #[test]
    fn category_mapping() {
        assert_eq!(
            classify(StatusCode::TOO_MANY_REQUESTS, "Throttling.RateQuota", ""),
            Retry::Retryable(Category::Rps)
        );
        assert_eq!(
            classify(StatusCode::TOO_MANY_REQUESTS, "Throttling.BurstRate", ""),
            Retry::Retryable(Category::BurstRate)
        );
        assert_eq!(
            classify(StatusCode::TOO_MANY_REQUESTS, "Throttling.AllocationQuota", ""),
            Retry::Retryable(Category::Tpm)
        );
        assert_eq!(
            classify(StatusCode::TOO_MANY_REQUESTS, "Throttling.Concurrency", ""),
            Retry::Retryable(Category::Concurrency)
        );
        assert_eq!(
            classify(StatusCode::TOO_MANY_REQUESTS, "insufficient_quota", ""),
            Retry::Retryable(Category::Tpm)
        );
    }

    #[test]
    fn non_429_delegates_to_generic() {
        // 5xx → 可重试(通用规则),即使带千问计费 code 也不影响
        assert!(classify(StatusCode::SERVICE_UNAVAILABLE, "Throttling.RateQuota", "").retryable());
        // 4xx → 不可重试
        assert!(!classify(StatusCode::BAD_REQUEST, "", "").retryable());
        // 408 → 可重试
        assert!(classify(StatusCode::REQUEST_TIMEOUT, "", "").retryable());
    }

    #[test]
    fn pass_through_error_info_shapes() {
        let info = crate::retry::ErrorInfo::parse(
            StatusCode::TOO_MANY_REQUESTS,
            r#"{"code":"Throttling.RateQuota","message":"You have exceeded your request limit."}"#,
        );
        assert!(info.classify_with(classify).retryable());

        // OpenAI 风格 type 字段(经 generic 语义词命中)
        let info = crate::retry::ErrorInfo::parse(
            StatusCode::TOO_MANY_REQUESTS,
            r#"{"error":{"type":"rate_limit_error","message":"slow down"}}"#,
        );
        assert!(info.classify_with(classify).retryable());
    }
}