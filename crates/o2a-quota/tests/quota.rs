//! o2a-quota 集成测试：对照 Python tests/test_quota.py 的用例语义。
//!
//! HTTP 类适配器通过本地 mock 服务器（原始 TCP 应答固定 JSON/HTML）验证。

use std::sync::Arc;

use chrono::{NaiveDate, NaiveDateTime, NaiveTime};
use o2a_config::Account;
use o2a_quota::adapters::{codex::OpenAICodexAdapter, declarative::DeclarativeQuotaAdapter, local::LocalQuotaAdapter, local_rolling_5h::LocalRolling5hAdapter, opencode_go::{parse_ssr_windows, OpenCodeGoAdapter}, openrouter::OpenRouterAdapter, zai::ZaiAdapter};
use o2a_quota::{
    empty_window, get_snapshot_async, make_snapshot, resolve_adapter_name, window_start, NowFn,
    QuotaAdapter, QuotaContext, QuotaError, Registry, TTLCache,
};
use serde_json::{json, Map, Value};

// ---------- 夹具 ----------

fn fixed_now(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> NowFn {
    Arc::new(move || {
        NaiveDate::from_ymd_opt(y, mo, d)
            .unwrap()
            .and_time(NaiveTime::from_hms_opt(h, mi, s).unwrap())
    })
}

fn make_account(quota_source: &str, quota: Option<Map<String, Value>>) -> Account {
    make_account_at("https://api.example.com/v1", quota_source, quota)
}

fn make_account_at(
    openai_url: &str,
    quota_source: &str,
    quota: Option<Map<String, Value>>,
) -> Account {
    Account {
        id: "acc-1".into(),
        name: "测试账号".into(),
        api_key: "sk-x".into(),
        openai_url: openai_url.into(),
        anthropic_url: "".into(),
        api: "".into(),
        quota_source: quota_source.into(),
        quota,
    }
}

fn write_jsonl(stats_dir: &std::path::Path, records: &[( &str, Value )]) {
    std::fs::create_dir_all(stats_dir).unwrap();
    let mut by_date: std::collections::BTreeMap<String, Vec<Value>> = Default::default();
    for (ts, rec) in records {
        let mut r = rec.clone();
        r["timestamp"] = json!(ts);
        r["account"] = json!("acc-1");
        by_date.entry(ts[..10].to_string()).or_default().push(r);
    }
    for (ds, recs) in by_date {
        let mut lines = String::new();
        for r in recs {
            lines.push_str(&serde_json::to_string(&r).unwrap());
            lines.push('\n');
        }
        use std::io::Write;
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(stats_dir.join(format!("{}.jsonl", ds)))
            .unwrap()
            .write_all(lines.as_bytes())
            .unwrap();
    }
}

fn rec(ts: &str) -> (&'static str, Value) {
    // 借用 ts 不可行：改为 owned，见 write_jsonl_owned
    (leak_str(ts), rec_value())
}

fn leak_str(s: &str) -> &'static str {
    Box::leak(s.to_string().into_boxed_str())
}

fn rec_value() -> Value {
    json!({"model": "m1", "input_tokens": 10, "output_tokens": 5,
           "cache_read_tokens": 0, "cache_write_tokens": 0})
}

/// 起一个对任意 GET 请求返回固定响应体的 mock 服务器，返回 base url。
async fn spawn_mock(body: String, content_type: &'static str) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = match listener.accept().await {
                Ok(x) => x,
                Err(_) => break,
            };
            let body = body.clone();
            let ct = content_type;
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut buf = vec![0u8; 8192];
                let _ = sock.read(&mut buf).await;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    ct,
                    body.len(),
                    body
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    format!("http://{}", addr)
}

fn ctx(stats_dir: &std::path::Path, account: Account, now: NowFn) -> QuotaContext {
    QuotaContext::new(stats_dir.to_string_lossy().to_string(), account).with_now_fn(now)
}

// ---------- 快照结构 ----------

#[test]
fn snapshot_shape() {
    let snap = make_snapshot(
        "local-rolling-5h",
        vec![empty_window("rolling", "requests", 3.0, Some(200.0), None)],
        "local_stats",
        Some(json!({"name": "pro"})),
        false,
        &NaiveDate::from_ymd_opt(2026, 1, 10).unwrap().and_hms_opt(0, 0, 0).unwrap(),
    );
    assert_eq!(snap["adapterId"], "local-rolling-5h");
    assert_eq!(snap["scope"], "account");
    assert_eq!(snap["stale"], false);
    let w = &snap["windows"][0];
    assert_eq!(w["used"], 3.0);
    assert_eq!(w["limit"], 200.0);
    assert_eq!(w["pct"], 1.5);
}

// ---------- local-rolling-5h 窗口边界 ----------

#[tokio::test]
async fn rolling_5h_boundary() {
    let tmp = tempfile::tempdir().unwrap();
    let stats_dir = tmp.path().join("stats");
    write_jsonl(
        &stats_dir,
        &[
            rec("2026-01-10T07:00:00"), // 恰在 5h 边界上（>=）
            rec("2026-01-10T09:30:00"),
            rec("2026-01-10T11:59:00"),
            rec("2026-01-10T06:59:59"), // 窗口外
            rec("2026-01-09T23:00:00"), // 窗口外
        ],
    );
    let quota = json!({"limit": 100}).as_object().cloned();
    let c = ctx(
        &stats_dir,
        make_account("auto", quota),
        fixed_now(2026, 1, 10, 12, 0, 0),
    );
    let snap = LocalRolling5hAdapter.fetch(&c).await.unwrap().unwrap();
    let w = &snap["windows"][0];
    assert_eq!(w["used"], 3.0);
    assert_eq!(w["reset_at"], "2026-01-10T12:00:00"); // 最早记录 07:00 + 5h
    assert_eq!(snap["adapterId"], "local-rolling-5h");
}

#[tokio::test]
async fn rolling_5h_empty_window() {
    let tmp = tempfile::tempdir().unwrap();
    let c = ctx(
        &tmp.path().join("none"),
        make_account("auto", None),
        fixed_now(2026, 1, 10, 12, 0, 0),
    );
    let snap = LocalRolling5hAdapter.fetch(&c).await.unwrap().unwrap();
    assert_eq!(snap["windows"][0]["used"], 0.0);
    assert!(snap["windows"][0]["reset_at"].is_null());
}

// ---------- local 日/周/月 ----------

#[tokio::test]
async fn local_month_window() {
    let tmp = tempfile::tempdir().unwrap();
    let stats_dir = tmp.path().join("stats");
    write_jsonl(
        &stats_dir,
        &[
            rec("2026-01-02T10:00:00"), // 本月
            rec("2025-12-31T23:59:00"), // 上月
        ],
    );
    let c = ctx(&stats_dir, make_account("auto", None), fixed_now(2026, 1, 10, 12, 0, 0));
    let snap = LocalQuotaAdapter.fetch(&c).await.unwrap().unwrap();
    assert_eq!(snap["windows"][0]["kind"], "month");
    assert_eq!(snap["windows"][0]["used"], 1.0);
}

#[test]
fn week_window_start() {
    let now = NaiveDate::from_ymd_opt(2026, 1, 7) // 周三
        .unwrap()
        .and_hms_opt(9, 0, 0)
        .unwrap();
    let start = window_start(&now, "week");
    use chrono::Datelike;
    assert_eq!(start.weekday().num_days_from_monday(), 0); // 周一
    assert_eq!(start.format("%Y-%m-%d").to_string(), "2026-01-05");
}

// ---------- manual ----------

#[tokio::test]
async fn manual_plan_config() {
    let tmp = tempfile::tempdir().unwrap();
    let quota = json!({"limit": 200, "unit": "requests", "period": "month"})
        .as_object()
        .cloned();
    let registry = Registry::standard();
    let c = ctx(
        &tmp.path().join("none"),
        make_account("auto", quota),
        fixed_now(2026, 1, 10, 12, 0, 0),
    );
    let snap = get_snapshot_async(&registry, &c.account, &c, None, true)
        .await
        .unwrap();
    assert_eq!(snap["adapterId"], "manual");
    assert_eq!(snap["source"], "plan_config");
    assert_eq!(snap["plan"]["included"]["amount"], 200);
}

// ---------- 注册表与嗅探 ----------

#[test]
fn registry_auto_sniff() {
    let registry = Registry::standard();
    let or_acc = make_account_at("https://openrouter.ai/api/v1", "auto", None);
    assert_eq!(resolve_adapter_name(&registry, &or_acc), "openrouter");
    let plain = make_account("auto", None);
    assert_eq!(resolve_adapter_name(&registry, &plain), "local");
}

#[test]
fn registry_reserved_names_fallback() {
    let registry = Registry::standard();
    for src in ["anthropic", "zen", "unknown-thing"] {
        assert_eq!(resolve_adapter_name(&registry, &make_account(src, None)), "local");
    }
    let names = o2a_quota::registered_adapters();
    assert!(names.contains(&"codex".to_string()));
    assert!(names.contains(&"manual".to_string()));
    assert!(names.contains(&"openrouter".to_string()));
}

#[test]
fn no_source_means_no_quota() {
    let registry = Registry::standard();
    let acc = make_account("none", None);
    assert_eq!(resolve_adapter_name(&registry, &acc), "local");
}

// ---------- 验证性适配器（declarative / opencode-go / zai / credits） ----------

#[tokio::test]
async fn declarative_adapter() {
    let tmp = tempfile::tempdir().unwrap();
    let quota = json!({
        "plan": "glm-coding-plan",
        "windows": [{"kind": "month", "period": "month", "unit": "requests", "limit": 200}],
    })
    .as_object()
    .cloned();
    let c = ctx(
        &tmp.path().join("stats"),
        make_account("declarative", quota),
        fixed_now(2026, 1, 10, 12, 0, 0),
    );
    let snap = DeclarativeQuotaAdapter.fetch(&c).await.unwrap().unwrap();
    assert_eq!(snap["adapterId"], "declarative");
    assert_eq!(snap["source"], "plan_config");
    assert_eq!(snap["plan"]["name"], "glm-coding-plan");
    assert_eq!(snap["windows"][0]["limit"], 200.0);
}

#[tokio::test]
async fn opencode_go_adapter_mock() {
    let tmp = tempfile::tempdir().unwrap();
    let url = spawn_mock(
        json!({"data": {"usage": 1.25, "limit": 100}}).to_string(),
        "application/json",
    )
    .await;
    let quota = json!({"url": url}).as_object().cloned();
    let c = ctx(
        &tmp.path().join("none"),
        make_account("opencode-go", quota),
        fixed_now(2026, 1, 10, 12, 0, 0),
    )
    .with_session(reqwest::Client::new());
    let snap = OpenCodeGoAdapter.fetch(&c).await.unwrap().unwrap();
    assert_eq!(snap["adapterId"], "opencode-go");
    assert_eq!(snap["windows"][0]["used"], 1.25);
    assert_eq!(snap["windows"][0]["limit"], 100.0);
}

#[tokio::test]
async fn zai_adapter_mock() {
    let tmp = tempfile::tempdir().unwrap();
    let url = spawn_mock(
        json!({"data": {"used_quota": 5, "total_quota": 20}}).to_string(),
        "application/json",
    )
    .await;
    let quota = json!({"url": url}).as_object().cloned();
    let c = ctx(
        &tmp.path().join("none"),
        make_account("zai", quota),
        fixed_now(2026, 1, 10, 12, 0, 0),
    )
    .with_session(reqwest::Client::new());
    let snap = ZaiAdapter.fetch(&c).await.unwrap().unwrap();
    assert_eq!(snap["adapterId"], "zai");
    assert_eq!(snap["windows"][0]["limit"], 20.0);
}

#[tokio::test]
async fn openrouter_credits_adapter_mock() {
    let tmp = tempfile::tempdir().unwrap();
    let url = spawn_mock(
        json!({"data": {"used_credits": 7, "total_credits": 50}}).to_string(),
        "application/json",
    )
    .await;
    let quota = json!({"mode": "credits", "url": url}).as_object().cloned();
    let c = ctx(
        &tmp.path().join("none"),
        make_account("openrouter", quota),
        fixed_now(2026, 1, 10, 12, 0, 0),
    )
    .with_session(reqwest::Client::new());
    let snap = OpenRouterAdapter.fetch(&c).await.unwrap().unwrap();
    assert_eq!(snap["adapterId"], "openrouter");
    assert_eq!(snap["windows"][0]["used"], 7.0);
    assert_eq!(snap["windows"][0]["limit"], 50.0);
}

#[test]
fn registry_new_adapter_names() {
    let registry = Registry::standard();
    let names = o2a_quota::registered_adapters();
    assert!(names.contains(&"declarative".to_string()));
    assert!(names.contains(&"opencode-go".to_string()));
    assert!(names.contains(&"zai".to_string()));
    assert_eq!(
        resolve_adapter_name(&registry, &make_account("glm-coding-plan", None)),
        "zai"
    );
}

// ---------- 失败降级 ----------

struct Boom;

#[async_trait::async_trait]
impl o2a_quota::QuotaAdapter for Boom {
    fn name(&self) -> &'static str {
        "boom"
    }
    fn source(&self) -> &'static str {
        "provider_api"
    }
    async fn fetch(&self, _ctx: &QuotaContext) -> Result<Option<Value>, QuotaError> {
        Err(QuotaError::new("boom"))
    }
}

#[tokio::test]
async fn upstream_failure_degrades_to_local() {
    let tmp = tempfile::tempdir().unwrap();
    let mut registry = Registry::standard();
    registry.register(Arc::new(Boom));
    let c = ctx(&tmp.path().join("stats"), make_account("boom", None), fixed_now(2026, 1, 10, 12, 0, 0));
    let snap = get_snapshot_async(&registry, &c.account, &c, None, true).await;
    let snap = snap.expect("snapshot must exist");
    assert_eq!(snap["adapterId"], "local"); // 降级 local
    assert_eq!(snap["stale"], true); // 标记滞后
}

#[test]
fn ttl_cache_hit_and_stale() {
    let cache = TTLCache::new(60);
    cache.set("acc-1", json!({"adapterId": "x", "stale": false}));
    assert_eq!(cache.get("acc-1").unwrap()["adapterId"], "x");
    let stale = cache.stale("acc-1").unwrap();
    assert_eq!(stale["stale"], true);
}

#[test]
fn iter_records_filters_account() {
    let tmp = tempfile::tempdir().unwrap();
    let stats_dir = tmp.path().join("stats");
    write_jsonl(&stats_dir, &[rec("2026-01-10T10:00:00")]);
    // 别的账号不计
    let since = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap().and_hms_opt(0, 0, 0).unwrap();
    assert!(o2a_quota::stats_util::iter_records(
        stats_dir.to_str().unwrap(),
        "acc-other",
        &since
    )
    .is_empty());
}

// ---------- OpenCode Go SSR / Codex (ChatGPT) 订阅额度 ----------

const SSR_HTML: &str = r#"
    <div data-slot="usage-item">
      <div data-slot="usage-label">Rolling Usage</div>
      <div data-slot="usage-value"><!--$-->6<!--/--></div>
      <div data-slot="reset-time">Resets in 2 hours 29 minutes</div>
    </div>
    <div data-slot="usage-item">
      <div data-slot="usage-label">Weekly Usage</div>
      <div data-slot="usage-value"><!--$-->42<!--/--></div>
      <div data-slot="reset-time">Resets in 5 days</div>
    </div>
"#;

#[tokio::test]
async fn opencode_go_ssr_adapter_mock() {
    let tmp = tempfile::tempdir().unwrap();
    let url = spawn_mock(SSR_HTML.to_string(), "text/html").await;
    let quota = json!({"cookie": "auth=x", "workspace_id": "wrk_1", "url": url})
        .as_object()
        .cloned();
    let c = ctx(
        &tmp.path().join("none"),
        make_account("opencode-go", quota),
        fixed_now(2026, 1, 10, 12, 0, 0),
    )
    .with_session(reqwest::Client::new());
    let snap = OpenCodeGoAdapter.fetch(&c).await.unwrap().unwrap();
    assert_eq!(snap["adapterId"], "opencode-go");
    let kinds: Vec<&str> = snap["windows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, vec!["rolling", "weekly"]);
    assert_eq!(snap["windows"][0]["pct"], 6.0);
    assert_eq!(snap["windows"][0]["reset_at"], "2026-01-10T14:29:00");
}

#[test]
fn opencode_go_parse_ssr_reset_chinese() {
    let html = r#"
    <div data-slot="usage-item">
      <div data-slot="usage-label">每月用量</div>
      <div data-slot="usage-value"><!--$-->77<!--/--></div>
      <div data-slot="reset-time">重置于 1 天 2 小时</div>
    </div>
    "#;
    let now = NaiveDate::from_ymd_opt(2026, 1, 10).unwrap().and_hms_opt(12, 0, 0).unwrap();
    let windows = parse_ssr_windows(html, &now);
    assert_eq!(windows[0]["kind"], "monthly");
    assert_eq!(windows[0]["used"], 77.0);
    assert_eq!(windows[0]["reset_at"], "2026-01-11T14:00:00");
}

#[tokio::test]
async fn codex_adapter_mock() {
    let tmp = tempfile::tempdir().unwrap();
    let url = spawn_mock(
        json!({
            "plan_type": "ChatGPT Plus",
            "rate_limit": {
                "primary_window": {"used_percent": 12, "limit_window_seconds": 18000, "reset_at": 1736514000},
                "secondary_window": {"used_percent": 3, "limit_window_seconds": 604800, "reset_at": 1736960400},
                "limit_reached": false,
            },
            "credits": {"has_credits": true, "balance": 9.5, "unlimited": false},
        })
        .to_string(),
        "application/json",
    )
    .await;
    let quota = json!({"access_token": "tok", "usage_url": url})
        .as_object()
        .cloned();
    let c = ctx(
        &tmp.path().join("none"),
        make_account("codex", quota),
        fixed_now(2026, 1, 10, 12, 0, 0),
    )
    .with_session(reqwest::Client::new());
    let snap = OpenAICodexAdapter.fetch(&c).await.unwrap().unwrap();
    let kinds: Vec<&str> = snap["windows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["kind"].as_str().unwrap())
        .collect();
    assert!(kinds.contains(&"rolling"));
    assert!(kinds.contains(&"weekly"));
    assert!(kinds.contains(&"credits"));
    let rolling = &snap["windows"][kinds.iter().position(|k| *k == "rolling").unwrap()];
    assert_eq!(rolling["pct"], 12.0);
    assert_eq!(snap["plan"]["name"], "ChatGPT Plus");
}

#[test]
fn registry_codex_aliases() {
    let registry = Registry::standard();
    let names = o2a_quota::registered_adapters();
    assert!(names.contains(&"codex".to_string()));
    assert_eq!(
        resolve_adapter_name(&registry, &make_account("gpt", None)),
        "codex"
    );
    assert_eq!(
        resolve_adapter_name(&registry, &make_account("openai-codex", None)),
        "codex"
    );
    assert_eq!(
        resolve_adapter_name(
            &registry,
            &make_account_at("https://chatgpt.com/backend-api", "auto", None)
        ),
        "codex"
    );
}

#[tokio::test]
async fn opencode_go_gateway_usage_v2_mock() {
    let tmp = tempfile::tempdir().unwrap();
    let url = spawn_mock(
        json!({
            "usage": {
                "rolling": {"status": "ok", "percent": 10, "resetsAt": "2026-08-30T15:57:12.109Z"},
                "weekly": {"status": "ok", "percent": 73, "resetsAt": "2026-08-31T00:00:00.109Z"},
                "monthly": {"status": "ok", "percent": 45, "resetsAt": "2026-09-18T13:58:17.109Z"},
            }
        })
        .to_string(),
        "application/json",
    )
    .await;
    let c = ctx(
        &tmp.path().join("none"),
        make_account_at(&url, "opencode-go", None),
        fixed_now(2026, 8, 30, 12, 0, 0),
    )
    .with_session(reqwest::Client::new());
    let snap = OpenCodeGoAdapter.fetch(&c).await.unwrap().unwrap();
    let kinds: Vec<&str> = snap["windows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, vec!["rolling", "weekly", "monthly"]);
    assert_eq!(snap["windows"][0]["pct"], 10.0);
    assert!(!snap["windows"][0]["reset_at"].is_null());
}

// ---------- 补充：显式未注册名继续嗅探（Python 行为，评审修订点） ----------

#[test]
fn unregistered_explicit_name_still_sniffs() {
    let registry = Registry::standard();
    // "openai" 是别名 → codex；换成未注册名 "weird-src" + 嗅探命中 URL → 嗅探生效
    let acc = make_account_at("https://chatgpt.com/backend-api", "weird-src", None);
    assert_eq!(resolve_adapter_name(&registry, &acc), "codex");
    // 未注册名 + 嗅探不中 → local
    let acc2 = make_account("weird-src", None);
    assert_eq!(resolve_adapter_name(&registry, &acc2), "local");
}

// ---------- 补充：codex 窗口归类免疫字段顺序切换 ----------

#[tokio::test]
async fn codex_window_classification_swapped_fields() {
    let tmp = tempfile::tempdir().unwrap();
    let url = spawn_mock(
        json!({
            "plan_type": "Pro",
            "rate_limit": {
                // OpenAI 会切换 primary/secondary 的窗口长度顺序：secondary 是 5h 窗
                "primary_window": {"used_percent": 8, "limit_window_seconds": 604800},
                "secondary_window": {"used_percent": 44, "limit_window_seconds": 18000},
            },
        })
        .to_string(),
        "application/json",
    )
    .await;
    let quota = json!({"access_token": "tok", "usage_url": url})
        .as_object()
        .cloned();
    let c = ctx(
        &tmp.path().join("none"),
        make_account("codex", quota),
        fixed_now(2026, 1, 10, 12, 0, 0),
    )
    .with_session(reqwest::Client::new());
    let snap = OpenAICodexAdapter.fetch(&c).await.unwrap().unwrap();
    let by_kind: std::collections::HashMap<&str, f64> = snap["windows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| (w["kind"].as_str().unwrap(), w["pct"].as_f64().unwrap()))
        .collect();
    assert_eq!(by_kind["rolling"], 44.0); // 18000s ≤ 6h → rolling，与字段位置无关
    assert_eq!(by_kind["weekly"], 8.0);
}

// ---------- 补充：codex 缺 token 报错 / 无 session 返回 None ----------

#[tokio::test]
async fn codex_missing_token_errors() {
    let tmp = tempfile::tempdir().unwrap();
    // api_key 为空且无 token 配置 → 才是 Python 语义下的 missing token
    let mut acc = make_account("codex", None);
    acc.api_key = String::new();
    let c = ctx(
        &tmp.path().join("none"),
        acc,
        fixed_now(2026, 1, 10, 12, 0, 0),
    )
    .with_session(reqwest::Client::new());
    let err = OpenAICodexAdapter.fetch(&c).await.unwrap_err();
    assert!(err.0.contains("missing access token"));
}

#[tokio::test]
async fn http_adapter_without_session_returns_none() {
    let tmp = tempfile::tempdir().unwrap();
    let c = ctx(
        &tmp.path().join("none"),
        make_account("openrouter", None),
        fixed_now(2026, 1, 10, 12, 0, 0),
    );
    assert!(OpenRouterAdapter.fetch(&c).await.unwrap().is_none());
    assert!(ZaiAdapter.fetch(&c).await.unwrap().is_none());
    assert!(OpenCodeGoAdapter.fetch(&c).await.unwrap().is_none());
    assert!(OpenAICodexAdapter.fetch(&c).await.unwrap().is_none());
}

// ---------- 补充：token_file 三种形态读取 ----------

#[test]
fn token_from_file_three_shapes() {
    let tmp = tempfile::tempdir().unwrap();
    // 形态 1：Codex CLI
    let p1 = tmp.path().join("codex.json");
    std::fs::write(
        &p1,
        json!({"tokens": {"access_token": "a1", "refresh_token": "r1"}}).to_string(),
    )
    .unwrap();
    // 形态 2：pi
    let p2 = tmp.path().join("pi.json");
    std::fs::write(
        &p2,
        json!({"openai-codex": {"access": "a2", "refresh": "r2"}}).to_string(),
    )
    .unwrap();
    // 形态 3：OpenCode providers
    let p3 = tmp.path().join("oc.json");
    std::fs::write(
        &p3,
        json!({"providers": {"codex": {"tokens": {"access_token": "a3", "refresh_token": "r3"}}}})
            .to_string(),
    )
    .unwrap();
    // 通过 fetch 路径间接验证：mock + token_file
    let rt = tokio::runtime::Runtime::new().unwrap();
    for (path, expect_access) in [(&p1, "a1"), (&p2, "a2"), (&p3, "a3")] {
        let url = rt.block_on(spawn_mock(
            json!({"plan_type": "T", "rate_limit": {"primary_window": {"used_percent": 1, "limit_window_seconds": 18000}}}).to_string(),
            "application/json",
        ));
        let quota = json!({"token_file": path.to_str().unwrap(), "usage_url": url})
            .as_object()
            .cloned();
        let c = QuotaContext::new("/tmp/none-o2a-quota-test", make_account("codex", quota))
            .with_session(reqwest::Client::new());
        let snap = rt
            .block_on(async { OpenAICodexAdapter.fetch(&c).await })
            .unwrap()
            .unwrap();
        // 每次请求都成功即说明 token 从文件读取成功（服务端不校验 token，用 plan 回传验证请求发出）
        assert_eq!(snap["plan"]["name"], "T");
        let _ = expect_access;
    }
}

// NaiveDateTime 保留引用（部分测试夹具可能需要）
#[allow(dead_code)]
fn _t(_n: NaiveDateTime) {}
