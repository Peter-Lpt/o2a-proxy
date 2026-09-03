//! 鉴权配置、mode 推导、URL/路径解析测试（对照 Python tests/test_auth.py 的 config 侧语义
//! 与 o2a/base.py 的 URL 归一化行为）。

use std::fs;
use std::path::Path;
use std::sync::Mutex;

use serde_json::json;

use o2a_config::{
    load_auth_from, load_config_at, new_service_id, normalize_openai_url, normalize_pricing_value,
    resolve_api_key, resolve_auth_path, resolve_plans_path, resolve_pricing_path, responses_url,
    Account, ClientKind, DispatchMode, ModelPolicy, OpenaiApi, PricingMode, Service, ThinkingMode,
    UpstreamApi,
};

fn make_account(openai_url: &str, anthropic_url: &str) -> Account {
    Account {
        id: "a1".to_string(),
        name: "a1".to_string(),
        api_key: "k".to_string(),
        openai_url: normalize_openai_url(openai_url),
        anthropic_url: anthropic_url.to_string(),
        api: String::new(),
        quota_source: "auto".to_string(),
        quota: None,
    }
}

fn make_service(account: Account, client: ClientKind) -> Service {
    Service {
        id: new_service_id(),
        name: "s1".to_string(),
        account,
        client,
        host: "127.0.0.1".to_string(),
        port: 18000,
        model: "m1".to_string(),
        override_model: true,
        max_tokens: 4096,
        proxy: String::new(),
        api: None,
        upstream_api: UpstreamApi::OpenaiCompletions,
        thinking_mode: ThinkingMode::Auto,
        pricing_mode: PricingMode::Token,
        pricing_extra: None,
        pricing_raw: json!(""),
        auth_token: String::new(),
        order: 0,
        enabled: true,
        autostart: false,
        models: Vec::new(),
        models_map: Default::default(),
        model_policy: ModelPolicy::Clamp,
        mode_override: None,
    }
}

// ---------- Account.kind / valid 推导 ----------

#[test]
fn test_account_kind_matrix() {
    let both = make_account("https://o.example.com", "https://a.example.com");
    assert_eq!(both.kind(), o2a_config::AccountKind::Both);
    assert!(both.valid());

    let openai_only = make_account("https://o.example.com", "");
    assert_eq!(openai_only.kind(), o2a_config::AccountKind::Openai);

    let anthropic_only = make_account("", "https://a.example.com");
    assert_eq!(anthropic_only.kind(), o2a_config::AccountKind::Anthropic);

    let invalid = make_account("", "");
    assert_eq!(invalid.kind(), o2a_config::AccountKind::Invalid);
    assert!(!invalid.valid());

    let no_key = make_account("https://o.example.com", "");
    let mut no_key = no_key;
    no_key.api_key = String::new();
    assert!(!no_key.valid());
}

// ---------- Service.mode 推导矩阵（api × kind × client 回退链） ----------

#[test]
fn test_mode_api_anthropic_messages_by_kind() {
    // api=anthropic-messages：kind=anthropic/both → direct；kind=openai → claude
    let mut svc = make_service(make_account("", "https://a.example.com"), ClientKind::Auto);
    svc.api = Some(OpenaiApi::AnthropicMessages);
    assert_eq!(svc.mode(), DispatchMode::Direct);

    let mut both = make_service(make_account("https://o.example.com", "https://a.example.com"), ClientKind::Auto);
    both.api = Some(OpenaiApi::AnthropicMessages);
    assert_eq!(both.mode(), DispatchMode::Direct);

    let mut openai_kind = make_service(make_account("https://o.example.com", ""), ClientKind::Auto);
    openai_kind.api = Some(OpenaiApi::AnthropicMessages);
    assert_eq!(openai_kind.mode(), DispatchMode::Claude);
}

#[test]
fn test_mode_api_openai_is_codex() {
    for api in [OpenaiApi::OpenaiCompletions, OpenaiApi::OpenaiResponses] {
        let mut svc = make_service(make_account("https://o.example.com", ""), ClientKind::Auto);
        svc.api = Some(api);
        assert_eq!(svc.mode(), DispatchMode::Codex);
    }
}

#[test]
fn test_mode_client_fallback_chain() {
    // client=openai → codex（Python 不做 kind 检查）
    let svc = make_service(make_account("https://o.example.com", ""), ClientKind::Openai);
    assert_eq!(svc.mode(), DispatchMode::Codex);

    // client=anthropic：账号有 anthropic 端点 → 透传；只有 openai 端点 → 转换
    let direct = make_service(make_account("", "https://a.example.com"), ClientKind::Anthropic);
    assert_eq!(direct.mode(), DispatchMode::Direct);
    let claude = make_service(make_account("https://o.example.com", ""), ClientKind::Anthropic);
    assert_eq!(claude.mode(), DispatchMode::Claude);

    // client=auto 且未声明 api → Auto（请求时识别）
    let auto = make_service(make_account("https://o.example.com", ""), ClientKind::Auto);
    assert_eq!(auto.mode(), DispatchMode::Auto);
}

#[test]
fn test_with_mode_override_semantics() {
    let mut svc = make_service(make_account("https://o.example.com", ""), ClientKind::Auto);
    svc.auth_token = "sk-t".to_string();
    svc.override_model = false;
    let svc2 = svc.with_mode(DispatchMode::Codex);
    assert_eq!(svc2.mode(), DispatchMode::Codex);
    assert_eq!(svc2.auth_token, "sk-t"); // with_mode 保留 auth_token（Python test_with_mode_preserves_auth_token）
    assert!(!svc2.override_model);
    assert!(svc2.mode_override.is_some());
    // 原服务不受影响（不共享状态）
    assert_eq!(svc.mode(), DispatchMode::Auto);
    assert!(svc.mode_override.is_none());
}

#[test]
fn test_target_url_and_reverse_models_map() {
    let mut direct = make_service(make_account("", "https://a.example.com/v1/messages"), ClientKind::Anthropic);
    direct.api = Some(OpenaiApi::AnthropicMessages);
    assert_eq!(direct.target_url(), "https://a.example.com/v1/messages");

    let codex = make_service(make_account("https://o.example.com/v1", ""), ClientKind::Openai);
    assert_eq!(codex.target_url(), "https://o.example.com/v1/chat/completions");

    let mut mapped = make_service(make_account("https://o.example.com/v1", ""), ClientKind::Openai);
    mapped.models_map = o2a_config::ModelsMap(vec![
        ("对外名".to_string(), "upstream-a".to_string()),
        ("alias2".to_string(), "upstream-b".to_string()),
        ("skip-empty".to_string(), String::new()),
    ]);
    let rev = mapped.models_map.reverse();
    assert_eq!(rev.len(), 2); // 空上游名被过滤
    assert!(rev.contains(&("upstream-a".to_string(), "对外名".to_string())));
    assert!(mapped.models_map.get("对外名") == Some("upstream-a"));
}

// ---------- load_config：services[].auth_token 解析 ----------

#[test]
fn test_load_config_reads_auth_token() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = json!({
        "auth_token": "sk-global",
        "accounts": [{"id": "acc-1", "name": "a", "api_key": "k",
                       "openai_url": "https://api.example.com/v1"}],
        "services": [
            {"comment": "t1", "account": "acc-1", "client": "openai",
             "listen_address": 18001, "model": "m1", "auth_token": "  sk-t1  "},
            {"comment": "t2", "account": "acc-1", "client": "openai",
             "listen_address": 18002, "model": "m2"}
        ]
    });
    let p = tmp.path().join("config.json");
    fs::write(&p, cfg.to_string()).unwrap();
    let services = load_config_at(&p);
    let t1 = services.iter().find(|s| s.name == "t1").unwrap();
    let t2 = services.iter().find(|s| s.name == "t2").unwrap();
    assert_eq!(t1.auth_token, "sk-t1"); // 服务级覆盖全局 + strip
    assert_eq!(t2.auth_token, "sk-global"); // 服务级缺省 → 顶层全局兜底
}

// ---------- auth.json 解析（load_auth + resolve_api_key） ----------

#[test]
fn test_load_auth_forms_and_meta_keys() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("auth.json");
    fs::write(
        &p,
        json!({
            "_readme": "元键跳过",
            "acc-1": {"type": "api_key", "key": "sk-dict"},
            "我的账号": "sk-str",
            "empty-dict": {},
            "null-key": {"key": null}
        })
        .to_string(),
    )
    .unwrap();
    let auth = load_auth_from(&p);
    assert_eq!(auth.get("acc-1").map(String::as_str), Some("sk-dict"));
    assert_eq!(auth.get("我的账号").map(String::as_str), Some("sk-str"));
    assert_eq!(auth.get("empty-dict").map(String::as_str), Some("")); // dict 无 key → ""
    assert_eq!(auth.get("null-key").map(String::as_str), Some(""));
    assert!(!auth.contains_key("_readme"));
}

#[test]
fn test_resolve_api_key_priority() {
    let mut auth = std::collections::HashMap::new();
    auth.insert("acc-1".to_string(), "sk-by-id".to_string());
    auth.insert("acc-name".to_string(), "sk-by-name".to_string());
    // id 优先
    assert_eq!(resolve_api_key(&auth, "acc-1", "acc-name", "embedded"), "sk-by-id");
    // name 兜底
    assert_eq!(resolve_api_key(&auth, "acc-x", "acc-name", "embedded"), "sk-by-name");
    // 全缺 → 内嵌
    assert_eq!(resolve_api_key(&auth, "acc-x", "acc-y", "embedded"), "embedded");
    // 空串视为未提供（继续回退）
    auth.insert("acc-empty".to_string(), String::new());
    assert_eq!(resolve_api_key(&auth, "acc-empty", "acc-y", "embedded"), "embedded");
}

#[test]
fn test_auth_path_follows_config_dir() {
    let cfg = Path::new("/some/dir/config.json");
    assert_eq!(resolve_auth_path(cfg), Path::new("/some/dir/auth.json"));
}

// ---------- URL 归一化（base.py 行为） ----------

#[test]
fn test_normalize_openai_url() {
    assert_eq!(
        normalize_openai_url("https://api.deepseek.com"),
        "https://api.deepseek.com/chat/completions"
    );
    assert_eq!(
        normalize_openai_url("https://x.example.com/compatible-mode/v1"),
        "https://x.example.com/compatible-mode/v1/chat/completions"
    );
    assert_eq!(
        normalize_openai_url("https://x.example.com/v1/chat/completions"),
        "https://x.example.com/v1/chat/completions"
    );
    assert_eq!(normalize_openai_url(""), "");
    assert_eq!(normalize_openai_url("  https://y.example.com/  "), "https://y.example.com/chat/completions");
}

#[test]
fn test_responses_url() {
    assert_eq!(
        responses_url("https://api.deepseek.com/chat/completions"),
        "https://api.deepseek.com/v1/responses"
    );
    assert_eq!(
        responses_url("https://x.example.com/v1/chat/completions"),
        "https://x.example.com/v1/responses"
    );
}

// ---------- pricing 路径解析（O2A_PRICING / O2A_PLANS，docs §3.1） ----------

static ENV_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn test_pricing_plans_path_resolution_order() {
    let _guard = ENV_LOCK.lock().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let cfg = tmp.path().join("config.json");

    // 1) config 同目录
    std::env::remove_var("O2A_PRICING");
    std::env::remove_var("O2A_PLANS");
    assert_eq!(
        resolve_pricing_path(Some(&cfg)),
        tmp.path().join("pricing.json")
    );
    assert_eq!(resolve_plans_path(Some(&cfg)), tmp.path().join("plans.json"));

    // 2) env 优先（目录形态 → 目录下文件）
    let env_dir = tempfile::tempdir().unwrap();
    std::env::set_var("O2A_PRICING", env_dir.path());
    std::env::set_var("O2A_PLANS", env_dir.path().join("custom.json"));
    assert_eq!(
        resolve_pricing_path(Some(&cfg)),
        env_dir.path().join("pricing.json")
    );
    assert_eq!(
        resolve_plans_path(Some(&cfg)),
        env_dir.path().join("custom.json")
    );

    // 3) 无 config → cwd 相对路径
    std::env::remove_var("O2A_PRICING");
    std::env::remove_var("O2A_PLANS");
    assert_eq!(resolve_pricing_path(None), Path::new("pricing.json"));

    std::env::remove_var("O2A_PRICING");
    std::env::remove_var("O2A_PLANS");
}

// ---------- 环境变量单服务回退 ----------

#[test]
fn test_env_fallback_single_service() {
    let _guard = ENV_LOCK.lock().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("nonexistent.json"); // config 不存在
    std::env::set_var("DASHSCOPE_API_KEY", "sk-env");
    std::env::remove_var("DASHSCOPE_URL");
    std::env::remove_var("PROXY_HOST");
    std::env::remove_var("PROXY_PORT");
    std::env::remove_var("PROXY_MODEL");
    std::env::remove_var("PROXY_MAX_TOKENS");
    let services = load_config_at(&p);
    std::env::remove_var("DASHSCOPE_API_KEY");

    assert_eq!(services.len(), 1);
    let s = &services[0];
    assert_eq!(s.name, "default");
    assert_eq!(s.account.id, "acc-env");
    assert_eq!(s.account.api_key, "sk-env");
    assert_eq!(
        s.account.openai_url,
        "https://dashscope.aliyuncs.com/compatible-mode/v1/chat/completions"
    );
    assert_eq!(s.port, 8317);
    assert_eq!(s.model, "qwen-plus");
    assert_eq!(s.max_tokens, 4096);
    assert_eq!(s.mode(), DispatchMode::Auto);
}

// ---------- StatsSettings（env 优先，等价 setdefault） ----------

#[test]
fn test_stats_settings_resolution() {
    use o2a_config::StatsSettings;
    let _guard = ENV_LOCK.lock().unwrap();
    let cfg = json!({
        "cache_stats_enabled": false,
        "cache_stats_dir": "from/config",
        "cache_stats_retention_days": 7
    });

    // 无 env：config 值生效
    for k in ["CACHE_STATS_ENABLED", "CACHE_STATS_DIR", "CACHE_STATS_RETENTION_DAYS"] {
        std::env::remove_var(k);
    }
    let s = o2a_config::resolve_stats_settings(&cfg);
    assert_eq!(
        s,
        StatsSettings {
            enabled: false,
            dir: "from/config".into(),
            retention_days: 7
        }
    );

    // env 已设置：env 优先（setdefault 语义）
    std::env::set_var("CACHE_STATS_ENABLED", "true");
    std::env::set_var("CACHE_STATS_DIR", "/env/dir");
    std::env::set_var("CACHE_STATS_RETENTION_DAYS", "3");
    let s = o2a_config::resolve_stats_settings(&cfg);
    assert!(s.enabled);
    assert_eq!(s.dir, Path::new("/env/dir"));
    assert_eq!(s.retention_days, 3);

    for k in ["CACHE_STATS_ENABLED", "CACHE_STATS_DIR", "CACHE_STATS_RETENTION_DAYS"] {
        std::env::remove_var(k);
    }
}

// ---------- normalize_pricing_value 单元 ----------

#[test]
fn test_normalize_pricing_value_unit() {
    let (m, e) = normalize_pricing_value(&json!("none"));
    assert!(matches!(m, PricingMode::Subscription));
    assert!(e.is_none());

    let (m, e) = normalize_pricing_value(&json!({"mode": "subscription", "plan": "p1", "quota_source": "codex"}));
    assert!(matches!(m, PricingMode::Subscription));
    assert_eq!(e.unwrap().len(), 2);

    // mode 空 → 默认 token
    let (m, _) = normalize_pricing_value(&json!({"plan": "p1"}));
    assert!(matches!(m, PricingMode::Token));

    // 非法 mode → token 且 extra 丢弃（对齐 Python）
    let (m, e) = normalize_pricing_value(&json!({"mode": "bogus", "plan": "p1"}));
    assert!(matches!(m, PricingMode::Token));
    assert!(e.is_none());

    // 非法字符串 → token
    let (m, _) = normalize_pricing_value(&json!("bogus"));
    assert!(matches!(m, PricingMode::Token));
}
