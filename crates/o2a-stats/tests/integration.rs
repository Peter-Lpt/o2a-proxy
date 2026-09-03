//! o2a-stats 集成测试（对照 tests/test_cache_stats.py 用例语义 + 格式快照）。

use std::fs;
use std::path::{Path, PathBuf};

use chrono::Local;
use filetime::{set_file_mtime, FileTime};
use serde_json::{json, Value};

use o2a_stats::meta::{build_meta, ReqTiming};
use o2a_stats::query::{get_account_summary, ServiceSummaryRef, StatsEnv};
use o2a_stats::registry::{is_cache_stats_enabled_with, StatsRegistry};
use o2a_stats::stats::{CacheStats, CacheStatsConfig, Usage};

fn tmp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("o2a-stats-test-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn cfg(dir: &Path, service: &str, service_id: &str, account: &str) -> CacheStatsConfig {
    CacheStatsConfig {
        stats_dir: dir.to_path_buf(),
        retention_days: 30,
        service: service.into(),
        service_id: service_id.into(),
        account: account.into(),
        account_name: format!("{account}-name"),
        no_cost: false,
        pricing_path: Some(dir.join("pricing.json")),
    }
}

fn usage(in_: i64, read: i64, write: i64, out: i64) -> Usage {
    Usage {
        input_tokens: in_,
        cache_read_input_tokens: read,
        cache_creation_input_tokens: write,
        output_tokens: out,
    }
}

fn read_jsonl(dir: &Path) -> Vec<Value> {
    let date = Local::now().format("%Y-%m-%d").to_string();
    let content = fs::read_to_string(dir.join(format!("{date}.jsonl"))).unwrap();
    content.lines().map(|l| serde_json::from_str(l).unwrap()).collect()
}

/// Python test_basic_record：基本记录 + JSONL 字段。
#[test]
fn basic_record_fields() {
    let dir = tmp_dir("basic");
    let stats = CacheStats::new(cfg(&dir, "", "", "acc-1"));
    stats.record("qwen-plus", &usage(1000, 5000, 200, 500), None, None, None, false);

    let recs = read_jsonl(&dir);
    assert_eq!(recs.len(), 1);
    let rec = &recs[0];
    assert_eq!(rec["model"], "qwen-plus");
    assert_eq!(rec["status"], "ok");
    assert_eq!(rec["input_tokens"], 1000);
    assert_eq!(rec["cache_read_tokens"], 5000);
    assert_eq!(rec["cache_write_tokens"], 200);
    assert_eq!(rec["output_tokens"], 500);
    assert!((rec["cache_hit_rate"].as_f64().unwrap() - 5000.0 / 6000.0).abs() < 0.0001);
    assert!((rec["cache_coverage"].as_f64().unwrap() - 5000.0 / 6200.0).abs() < 0.0001);
    // 必有字段
    for k in ["timestamp", "service", "account", "cost", "cache_hit_rate", "cache_coverage"] {
        assert!(rec.get(k).is_some(), "missing {k}");
    }
    // 无 service_id（空）→ 不写
    assert!(rec.get("service_id").is_none());
    // 空计价目录 → cost 0
    assert_eq!(rec["cost"], 0.0);
}

/// 条件字段：service_id / batch / upstream_model / error / meta。
#[test]
fn conditional_fields() {
    let dir = tmp_dir("cond");
    let mut c = cfg(&dir, "svc-a", "svc-aaa", "acc-1");
    c.no_cost = true;
    let stats = CacheStats::new(c);

    // 错误记录：usage 空 + error
    stats.record("m", &Usage::default(), Some("upstream HTTP 500"), None, None, false);
    // batch + upstream_model + meta
    let meta = build_meta(&ReqTiming::new(0.0, Some(1.0), 2.5), 1000);
    stats.record("alias", &usage(10, 20, 0, 1000), None, Some(&meta), Some("m1"), true);

    let recs = read_jsonl(&dir);
    assert_eq!(recs[0]["status"], "error");
    assert_eq!(recs[0]["error"], "upstream HTTP 500");
    assert_eq!(recs[0]["input_tokens"], 0);

    let r = &recs[1];
    assert_eq!(r["status"], "ok");
    assert_eq!(r["service_id"], "svc-aaa");
    assert_eq!(r["batch"], true);
    assert_eq!(r["upstream_model"], "m1");
    assert_eq!(r["model"], "alias");
    // duration_ms = 2500, first_token_ms = 1000（round2）
    assert_eq!(r["duration_ms"], 2500.0);
    assert_eq!(r["first_token_ms"], 1000.0);
    // 速度 = 1000 / (2.5-1.0) = 666.666.. → 666.67
    assert_eq!(r["output_tokens_per_sec"], 666.67);
    // no_cost → cost 0
    assert_eq!(r["cost"], 0.0);
}

/// banker's rounding tie：13/20000 = 0.00065 → 0.0006（Python 同值已实测）。
#[test]
fn banker_rounding_tie_in_record() {
    let dir = tmp_dir("tie");
    let stats = CacheStats::new(cfg(&dir, "", "", "acc-1"));
    stats.record("m", &usage(19987, 13, 0, 0), None, None, None, false);
    let rec = &read_jsonl(&dir)[0];
    assert_eq!(rec["cache_hit_rate"], 0.0006);
}

/// Python test_summary_query：多条记录的 day/hour 查询。
#[test]
fn summary_query_day_hour() {
    let dir = tmp_dir("summary");
    let stats = CacheStats::new(cfg(&dir, "", "", "acc-1"));
    for i in 0..3 {
        stats.record("qwen-plus", &usage(1000 + i * 100, 5000 + i * 500, 200, 500), None, None, None, false);
    }
    let s = stats.get_summary("day");
    assert_eq!(s["period"], "day");
    let dt = &s["daily_total"];
    assert_eq!(dt["requests"], 3);
    assert_eq!(dt["total_input_tokens"], 1000 + 1100 + 1200);
    assert_eq!(dt["total_cache_read_tokens"], 5000 + 5500 + 6000);
    // hours：单小时一条，hour 标签 = dateT{HH}:00:00
    let hours = s["hours"].as_array().unwrap();
    assert!(!hours.is_empty());
    let today = Local::now().format("%Y-%m-%d").to_string();
    assert!(hours.iter().all(|h| h["hour"].as_str().unwrap().starts_with(&today)));
    // 空 summary JSONL 回退场景不适用（有 JSONL），daily avg 字段存在
    assert!(dt.get("avg_cache_hit_rate").is_some());

    let hs = stats.get_summary("hour");
    assert_eq!(hs["period"], "hour");
    assert_eq!(hs["requests"], 3);

    // all：日期来自 summary 目录文件
    let all = stats.get_summary("all");
    assert_eq!(all["period"], "all");
    assert_eq!(all["days"].as_array().unwrap().len(), 1);
    assert_eq!(all["total"]["requests"], 3);
}

/// 无数据时的 day/hour 形状。
#[test]
fn empty_summary_shapes() {
    let dir = tmp_dir("empty");
    let stats = CacheStats::new(cfg(&dir, "", "", "acc-1"));
    let today = Local::now().format("%Y-%m-%d").to_string();
    let hour = Local::now().format("%H").to_string();
    assert_eq!(stats.get_summary("day"), json!({"period":"day","date":today,"requests":0}));
    assert_eq!(
        stats.get_summary("hour"),
        json!({"period":"hour","hour":format!("{today}T{hour}"),"requests":0})
    );
    assert_eq!(
        stats.get_summary("week"),
        json!({"error":"unknown period: week"})
    );
}

/// summary JSON 回退：无 JSONL、有 summary 文件（清内部字段、cost 用文件值）。
#[test]
fn summary_file_fallback() {
    let dir = tmp_dir("fallback");
    let mut c = cfg(&dir, "svc-a", "svc-aaa", "acc-1");
    c.no_cost = true;
    let stats = CacheStats::new(c);
    let today = Local::now().format("%Y-%m-%d").to_string();
    let path = dir.join("summary").join("svc-aaa").join(format!("{today}.json"));
    fs::write(
        &path,
        json!({
            "date": today,
            "hours": {
                "09": {"requests": 2, "total_input_tokens": 100, "total_cache_read_tokens": 200,
                        "total_cache_write_tokens": 0, "total_output_tokens": 50,
                        "total_cost": 1.5, "_hit_rate_sum": 1.3333, "_coverage_sum": 1.3333}
            }
        })
        .to_string(),
    )
    .unwrap();
    // JSONL 不存在 → 走 summary 回退
    let s = stats.get_summary("day");
    let hours = s["hours"].as_array().unwrap();
    assert_eq!(hours.len(), 1);
    assert_eq!(hours[0]["hour"], format!("{today}T09:00:00"));
    assert_eq!(hours[0]["requests"], 2);
    assert_eq!(hours[0]["total_cost"], 1.5);
    // round(1.3333/2, 4) = round(0.66665, 4)（Python banker 语义）
    assert!((hours[0]["avg_cache_hit_rate"].as_f64().unwrap() - 1.3333 / 2.0).abs() < 1e-4);
    assert_eq!(s["daily_total"]["total_cost"], 1.5);
    assert_eq!(s["daily_total"]["requests"], 2);
}

/// Python test_cleanup：retention 外文件清理（jsonl + summary 子目录）。
#[test]
fn retention_cleanup() {
    let dir = tmp_dir("cleanup");
    fs::create_dir_all(dir.join("summary").join("svc-old")).unwrap();
    let old_file = dir.join("2026-06-01.jsonl");
    fs::write(&old_file, "{}\n").unwrap();
    let old_summary = dir.join("summary").join("svc-old").join("2026-06-01.json");
    fs::write(&old_summary, "{}").unwrap();
    let new_file = dir.join("keep.jsonl");
    fs::write(&new_file, "{}\n").unwrap();
    let old = FileTime::from_unix_time(Local::now().timestamp() - 40 * 86400, 0);
    set_file_mtime(&old_file, old).unwrap();
    set_file_mtime(&old_summary, old).unwrap();

    let _ = CacheStats::new(cfg(&dir, "", "", "acc-1"));
    assert!(!old_file.exists(), "old jsonl should be cleaned");
    assert!(!old_summary.exists(), "old summary should be cleaned");
    assert!(new_file.exists(), "fresh file kept");
}

/// 月累计口径：严格早于 before_ts 的同账号 tokens 总和。
#[test]
fn month_cumulative_tokens_window() {
    let dir = tmp_dir("cumul");
    fs::create_dir_all(&dir).unwrap();
    let month = Local::now().format("%Y-%m").to_string();
    // 3 条：同账号早 / 同账号晚 / 他账号早
    let lines = format!(
        "{}\n{}\n{}\n",
        json!({"timestamp": format!("{month}-01T10:00:00"), "account": "acc-1",
               "input_tokens": 10, "cache_read_tokens": 0, "cache_write_tokens": 0, "output_tokens": 5}),
        json!({"timestamp": format!("{month}-20T10:00:00"), "account": "acc-1",
               "input_tokens": 100, "cache_read_tokens": 0, "cache_write_tokens": 0, "output_tokens": 50}),
        json!({"timestamp": format!("{month}-02T10:00:00"), "account": "acc-2",
               "input_tokens": 999, "cache_read_tokens": 0, "cache_write_tokens": 0, "output_tokens": 999})
    );
    fs::write(dir.join(format!("{month}-15.jsonl")), lines).unwrap();
    let stats = CacheStats::new(cfg(&dir, "", "", "acc-1"));
    // 严格早于 01-10：只有 01-01 那条（同秒不计）
    assert_eq!(stats.month_cumulative_tokens(&format!("{month}-10T00:00:00")), 15);
    // Python 行为钉死：缓存键 = (dir, month, 文件签名)，不含 before_ts —— 同实例
    // 在文件未变化时换 before_ts 仍命中旧缓存（15）；新实例重算得 165。
    assert_eq!(stats.month_cumulative_tokens(&format!("{month}-25T00:00:00")), 15);
    let fresh = CacheStats::new(cfg(&dir, "", "", "acc-1"));
    assert_eq!(fresh.month_cumulative_tokens(&format!("{month}-25T00:00:00")), 165);
}

/// 计费写入快照 + 读取重放（upstream_model 优先计价）。
#[test]
fn cost_write_and_replay() {
    let dir = tmp_dir("cost");
    fs::write(
        dir.join("pricing.json"),
        json!({
            "myprov": {
                "models": {
                    "m1": {"tiers": [{"range": "unlimited", "input": 2.0, "output": 6.0,
                                       "cache_hit": 0.4, "cache_miss": 1.0}]}
                }
            }
        })
        .to_string(),
    )
    .unwrap();
    let stats = CacheStats::new(cfg(&dir, "svc-a", "svc-aaa", "acc-1"));
    // input 1M * 2/CNY + output 1M * 6 = 8.0
    stats.record("m1", &usage(1_000_000, 0, 0, 1_000_000), None, None, None, false);
    let rec = &read_jsonl(&dir)[0];
    assert_eq!(rec["cost"], 8.0);

    // 别名：model=alias 计价按 upstream_model=m1
    stats.record("alias", &usage(1_000_000, 0, 0, 1_000_000), None, None, Some("m1"), false);
    let rec = &read_jsonl(&dir)[1];
    assert_eq!(rec["cost"], 8.0);
    assert_eq!(rec["upstream_model"], "m1");

    // 读取端重放（JSONL → 当前 pricing 重算）
    let s = stats.get_summary("day");
    assert_eq!(s["daily_total"]["total_cost"], 16.0);
}

/// matches_record 规则：id 命中 / 名命中 / 双空默认兜底。
#[test]
fn matches_record_rules() {
    let dir = tmp_dir("match");
    let stats = CacheStats::new(cfg(&dir, "svc-a", "svc-aaa", "acc-1"));
    let mk = |sid: Value, svc: Value| json!({"service_id": sid, "service": svc});
    assert!(stats.matches_record(&mk(json!("svc-aaa"), json!("other"))));
    assert!(stats.matches_record(&mk(json!("other"), json!("svc-a"))));
    assert!(!stats.matches_record(&mk(json!("svc-bbb"), json!("svc-b"))));
    // 双空实例只吃双空记录
    let plain = CacheStats::new(cfg(&dir, "", "", "acc-1"));
    assert!(plain.matches_record(&mk(json!(""), json!(""))));
    assert!(!plain.matches_record(&mk(json!("svc-aaa"), json!("svc-a"))));
}

/// 账号归并：day hours 合并 + daily 累加 + all 分支 + 空服务。
#[test]
fn account_summary_merge() {
    let dir = tmp_dir("acct");
    let env = StatsEnv {
        stats_dir: dir.to_path_buf(),
        retention_days: 30,
        pricing_path: Some(dir.join("pricing.json")),
    };
    let mk = |name: &str, id: &str| CacheStats::new(CacheStatsConfig {
        stats_dir: dir.to_path_buf(),
        retention_days: 30,
        service: name.into(),
        service_id: id.into(),
        account: "acc-1".into(),
        account_name: String::new(),
        no_cost: true,
        pricing_path: Some(dir.join("pricing.json")),
    });
    let a = mk("服务A", "svc-aaa");
    let b = mk("服务B", "svc-bbb");
    a.record("m", &usage(100, 0, 0, 10), None, None, None, false);
    a.record("m", &usage(100, 0, 0, 10), None, None, None, false);
    b.record("m", &usage(50, 0, 0, 5), None, None, None, false);

    let services = vec![
        ServiceSummaryRef { name: "服务A".into(), service_id: "svc-aaa".into() },
        ServiceSummaryRef { name: "服务B".into(), service_id: "svc-bbb".into() },
    ];
    let day = get_account_summary(&env, "acc-1", &services, "day");
    assert_eq!(day["daily_total"]["requests"], 3);
    assert_eq!(day["daily_total"]["total_input_tokens"], 250);
    assert_eq!(day["daily_total"]["total_output_tokens"], 25);
    let today = Local::now().format("%Y-%m-%d").to_string();
    let hours = day["hours"].as_array().unwrap();
    assert_eq!(hours.len(), 1);
    assert_eq!(hours[0]["hour"], format!("{today}T{:02}:00:00", Local::now().format("%H")));
    assert_eq!(hours[0]["requests"], 3);
    assert_eq!(day["account"], "acc-1");

    let all = get_account_summary(&env, "acc-1", &services, "all");
    assert_eq!(all["total"]["requests"], 3);
    assert_eq!(all["days"].as_array().unwrap().len(), 2); // 两个服务各自的 day

    // Python 语义：services 由 config 按账号过滤得出，无匹配服务 = 空列表
    let none = get_account_summary(&env, "acc-9", &[], "day");
    assert_eq!(none["requests"], 0);
    assert_eq!(none["account"], "acc-9");

    // hour 分支：daily 取 hour 摘要本身
    let hour = get_account_summary(&env, "acc-1", &services, "hour");
    assert_eq!(hour["daily_total"]["requests"], 3);
}

/// 注册表：同 id 复用实例、clear_pricing_cache 可调用。
#[test]
fn stats_registry() {
    let dir = tmp_dir("registry");
    let reg = StatsRegistry::new(dir.clone(), 30, None);
    let a = reg.get("svc-a", "svc-aaa", "acc-1", "acc-1-name", false);
    let b = reg.get("svc-a", "svc-aaa", "acc-1", "acc-1-name", false);
    assert!(std::sync::Arc::ptr_eq(&a, &b));
    a.record("m", &usage(1, 0, 0, 1), None, None, None, false);
    reg.clear_pricing_cache();
    assert!(dir.join(format!("{}.jsonl", a.today())).exists());
}

#[test]
fn cache_stats_enabled_rules() {
    assert!(is_cache_stats_enabled_with(None));
    for v in ["true", "True", "1", "yes", "YES"] {
        assert!(is_cache_stats_enabled_with(Some(v)), "{v}");
    }
    for v in ["false", "0", "no", "junk"] {
        assert!(!is_cache_stats_enabled_with(Some(v)), "{v}");
    }
}

/// meta 二位小数舍入写入 record（build_meta 产物经 round2 落盘）。
#[test]
fn meta_rounded_to_two_decimals() {
    let dir = tmp_dir("metaround");
    let stats = CacheStats::new(cfg(&dir, "", "", "acc-1"));
    let meta = build_meta(&ReqTiming::new(0.0, None, 1.234567), 0);
    stats.record("m", &usage(1, 0, 0, 0), None, Some(&meta), None, false);
    let rec = &read_jsonl(&dir)[0];
    assert_eq!(rec["duration_ms"], 1234.57);
}
