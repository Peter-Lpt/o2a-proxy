//! record_stats 的 meta 计算（调用方语义对齐 Python engine.record_stats）。
//!
//! - duration_ms：请求开始 → 记录时刻（毫秒）；req_start 存在即写入
//! - first_token_ms：请求开始 → 首个上游 chunk（毫秒，流式）；两者存在即写入
//! - output_tokens_per_sec：加权口径 —— output/(now-first_chunk)（首 chunk 存在），
//!   否则 output/(now-req_start)；对应分支分母 <= 0 时不产出、不回退另一口径

use serde_json::{json, Value};

/// 请求计时输入（秒级时间戳，monotonic 或 unix 均可，只用到差值）。
#[derive(Debug, Clone, Copy)]
pub struct ReqTiming {
    pub req_start: f64,
    pub first_chunk: Option<f64>,
    pub now: f64,
}

impl ReqTiming {
    pub fn new(req_start: f64, first_chunk: Option<f64>, now: f64) -> Self {
        Self { req_start, first_chunk, now }
    }
}

pub fn build_meta(t: &ReqTiming, output_tokens: i64) -> Value {
    let mut m = serde_json::Map::new();
    m.insert("duration_ms".into(), json!((t.now - t.req_start) * 1000.0));
    if let Some(fc) = t.first_chunk {
        m.insert("first_token_ms".into(), json!((fc - t.req_start) * 1000.0));
    }
    if output_tokens > 0 {
        let speed = match t.first_chunk {
            Some(fc) => {
                let g = t.now - fc;
                (g > 0.0).then(|| output_tokens as f64 / g)
            }
            None => {
                let tot = t.now - t.req_start;
                (tot > 0.0).then(|| output_tokens as f64 / tot)
            }
        };
        if let Some(s) = speed {
            m.insert("output_tokens_per_sec".into(), json!(s));
        }
    }
    Value::Object(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meta_fields_and_speed_weighting() {
        // 无 first_chunk：速度用总时长
        let m = build_meta(&ReqTiming::new(0.0, None, 4.0), 100);
        assert!((m["duration_ms"].as_f64().unwrap() - 4000.0).abs() < 1e-9);
        assert!(m.get("first_token_ms").is_none());
        assert!((m["output_tokens_per_sec"].as_f64().unwrap() - 25.0).abs() < 1e-9);

        // 有 first_chunk：速度 = output/(now-first_chunk)（加权口径）
        let m = build_meta(&ReqTiming::new(0.0, Some(1.0), 5.0), 100);
        assert!((m["first_token_ms"].as_f64().unwrap() - 1000.0).abs() < 1e-9);
        assert!((m["output_tokens_per_sec"].as_f64().unwrap() - 25.0).abs() < 1e-9);

        // output=0：无速度字段
        let m = build_meta(&ReqTiming::new(0.0, Some(1.0), 5.0), 0);
        assert!(m.get("output_tokens_per_sec").is_none());

        // 有 first_chunk 但分母非正：无速度字段（不回退总时长口径，与 Python 一致）
        let m = build_meta(&ReqTiming::new(5.0, Some(5.0), 5.0), 100);
        assert!(m.get("output_tokens_per_sec").is_none());
        assert!(m.get("duration_ms").is_some());

        // 无 first_chunk 且总时长非正：无速度字段
        let m = build_meta(&ReqTiming::new(5.0, None, 5.0), 100);
        assert!(m.get("output_tokens_per_sec").is_none());
    }
}
