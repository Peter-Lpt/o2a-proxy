//! Python 兼容舍入。
//!
//! Python 内建 `round()` 是 banker's rounding（half-even）；Rust `f64::round`
//! 是 half-away-from-zero，直接用会在 tie 场景产生偏差。
//! `format!("{:.n$}", x)` 的十进制格式化同为 half-even（已用 13/20000、0.125、
//! 0.375、1.0000005 等探针与 Python round 逐一对照），格式化后 parse 回 f64。

pub fn py_round(x: f64, n: usize) -> f64 {
    if !x.is_finite() {
        return x;
    }
    format!("{:.*}", n, x).parse::<f64>().unwrap_or(x)
}

/// 千位分隔（对齐 Python f"{n:,}"，负数同样处理）。
pub fn thousands(n: i64) -> String {
    let neg = n < 0;
    let digits = n.unsigned_abs().to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    if neg {
        format!("-{out}")
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn banker_ties_match_python() {
        // Python: round(13/20000,4)=0.0006 / round(0.125,2)=0.12 / round(0.375,2)=0.38
        assert_eq!(py_round(13.0 / 20000.0, 4), 0.0006);
        assert_eq!(py_round(0.125, 2), 0.12);
        assert_eq!(py_round(0.375, 2), 0.38);
        assert_eq!(py_round(2.5, 0), 2.0);
        assert_eq!(py_round(3.5, 0), 4.0);
        assert_eq!(py_round(5000.0 / 6000.0, 4), 0.8333);
    }

    #[test]
    fn thousands_format() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(1234567), "1,234,567");
        assert_eq!(thousands(-9876), "-9,876");
    }
}
