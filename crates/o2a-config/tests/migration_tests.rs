//! 配置迁移与 id 化回归测试（对照 Python tests/test_config_migration.py 用例语义）。

use std::fs;
use std::path::Path;

use serde_json::{json, Value};

use o2a_config::load_config_at;

fn write_cfg(dir: &Path, cfg: &Value) -> std::path::PathBuf {
    let p = dir.join("config.json");
    fs::write(&p, serde_json::to_string(cfg).unwrap()).unwrap();
    p
}

fn base_cfg() -> Value {
    json!({
        "cache_stats_enabled": true,
        "accounts": [
            {"id": "acc-1", "name": "a", "api_key": "k",
             "openai_url": "https://api.example.com/v1"}
        ]
    })
}


/// base config + services 合并（json! 不支持 `..` 展开语法）
fn cfg_with_services(services: Value) -> Value {
    let mut cfg = base_cfg();
    cfg["services"] = services;
    cfg
}

fn svc_min(i: i32) -> Value {
    json!({
        "comment": format!("t{}", i),
        "account": "acc-1",
        "client": "openai",
        "listen_address": 18000 + i,
        "model": "m"
    })
}

// ---------- load_config：id 惰性生成 + 写回 ----------

#[test]
fn test_missing_ids_generated_and_written_back() {
    let tmp = tempfile::tempdir().unwrap();
    let p = write_cfg(
        tmp.path(),
        &cfg_with_services(json!([svc_min(1), svc_min(2)])),
    );
    let services = load_config_at(&p);
    assert_eq!(services.len(), 2);
    for s in &services {
        assert!(s.id.starts_with("svc-") && s.id.len() == 12, "id={}", s.id);
    }
    let ids: std::collections::HashSet<_> = services.iter().map(|s| s.id.clone()).collect();
    assert_eq!(ids.len(), 2); // 去重

    let cfg: Value = serde_json::from_str(&fs::read_to_string(&p).unwrap()).unwrap();
    for s in cfg["services"].as_array().unwrap() {
        assert!(s["id"].as_str().unwrap().starts_with("svc-"));
    }
    assert!(Path::new(&format!("{}.bak", p.display())).exists()); // 写回前备份
}

#[test]
fn test_existing_ids_preserved() {
    let tmp = tempfile::tempdir().unwrap();
    let mut svc = svc_min(1);
    svc["id"] = json!("svc-abcdef01");
    let p = write_cfg(tmp.path(), &cfg_with_services(json!([svc])));
    let services = load_config_at(&p);
    assert_eq!(services[0].id, "svc-abcdef01");
    let cfg: Value = serde_json::from_str(&fs::read_to_string(&p).unwrap()).unwrap();
    assert_eq!(cfg["services"][0]["id"], "svc-abcdef01");
}

#[test]
fn test_duplicate_ids_regenerated() {
    let tmp = tempfile::tempdir().unwrap();
    let mut s1 = svc_min(1);
    let mut s2 = svc_min(2);
    s1["id"] = json!("svc-dup11111");
    s2["id"] = json!("svc-dup11111");
    let p = write_cfg(tmp.path(), &cfg_with_services(json!([s1, s2])));
    let services = load_config_at(&p);
    assert_eq!(services[0].id, "svc-dup11111"); // 首个保留
    assert_ne!(services[1].id, "svc-dup11111");
}

#[test]
fn test_registry_recovery_when_id_missing() {
    // comment → id 登记表找回：config 被旧快照覆盖丢 id 时身份不漂移
    let tmp = tempfile::tempdir().unwrap();
    let p = write_cfg(tmp.path(), &cfg_with_services(json!([svc_min(1)])));
    fs::write(
        tmp.path().join("service_ids.json"),
        json!({"t1": "svc-abc12345"}).to_string(),
    )
    .unwrap();
    let services = load_config_at(&p);
    assert_eq!(services[0].id, "svc-abc12345");
}

// ---------- v0/v1 样本兼容 + 新字段 ----------

#[test]
fn test_v0_legacy_config_loads() {
    // v0：services 内嵌 url/key 的最老结构 + mode 字段
    let tmp = tempfile::tempdir().unwrap();
    let p = write_cfg(
        tmp.path(),
        &json!({"services": [{
            "comment": "legacy", "mode": "claude", "model": "m",
            "listen_address": 18001,
            "openai_base_url": "https://x.example.com",
            "openai_api_key": "sk-x"}]}),
    );
    let services = load_config_at(&p);
    assert_eq!(services.len(), 1);
    assert_eq!(services[0].name, "legacy");
    assert!(services[0].enabled); // 缺省启用
    assert!(!services[0].autostart); // 缺省不自启
    assert_eq!(services[0].order, 0);
    // mode=claude → client=anthropic；账号只有 openai 端点 → mode=claude（转换）
    assert_eq!(services[0].mode(), o2a_config::DispatchMode::Claude);
    assert_eq!(services[0].account.id, "acc-1");
    assert_eq!(services[0].account.api_key, "sk-x");
}

#[test]
fn test_v1_config_new_fields() {
    let tmp = tempfile::tempdir().unwrap();
    let mut svc = svc_min(1);
    svc["order"] = json!(5);
    svc["enabled"] = json!(false);
    svc["autostart"] = json!(true);
    let p = write_cfg(tmp.path(), &cfg_with_services(json!([svc])));
    let services = load_config_at(&p);
    assert_eq!(services[0].order, 5);
    assert!(!services[0].enabled);
    assert!(services[0].autostart);
}

#[test]
fn test_enabled_string_false_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let mut svc = svc_min(1);
    svc["enabled"] = json!("false");
    let p = write_cfg(tmp.path(), &cfg_with_services(json!([svc])));
    assert!(!load_config_at(&p)[0].enabled);
}

#[test]
fn test_invalid_mode_service_skipped() {
    let tmp = tempfile::tempdir().unwrap();
    let mut svc = svc_min(1);
    svc["mode"] = json!("bogus");
    let p = write_cfg(tmp.path(), &cfg_with_services(json!([svc])));
    let services = load_config_at(&p);
    // 非法 mode 的服务被跳过；若环境设置了 DASHSCOPE_API_KEY，
    // 剩余服务只会是 env 回退单服务（name=default），不会是被跳过的 t1
    assert!(services.iter().all(|s| s.name != "t1"));
    assert!(services.len() <= 1);
}

// ---------- pricing 字段升级 ----------

#[test]
fn test_pricing_string_none_maps_to_subscription() {
    let tmp = tempfile::tempdir().unwrap();
    let mut svc = svc_min(1);
    svc["pricing"] = json!("none");
    let p = write_cfg(tmp.path(), &cfg_with_services(json!([svc])));
    let s = &load_config_at(&p)[0];
    assert!(matches!(s.pricing_mode, o2a_config::PricingMode::Subscription));
    assert_eq!(s.pricing_raw, json!("none")); // 兼容别名原样保留
}

#[test]
fn test_pricing_object_subscription() {
    let tmp = tempfile::tempdir().unwrap();
    let obj = json!({"mode": "subscription", "plan": "max-5h"});
    let mut svc = svc_min(1);
    svc["pricing"] = obj.clone();
    let p = write_cfg(tmp.path(), &cfg_with_services(json!([svc])));
    let s = &load_config_at(&p)[0];
    assert!(matches!(s.pricing_mode, o2a_config::PricingMode::Subscription));
    assert_eq!(s.pricing_extra.as_ref().unwrap().get("plan").unwrap(), "max-5h");
    // 对象形式原样保留（保存不丢字段）：id 已存在 → 不写回，文件字节不变
    let cfg: Value = serde_json::from_str(&fs::read_to_string(&p).unwrap()).unwrap();
    assert_eq!(cfg["services"][0]["pricing"], obj);
}

#[test]
fn test_pricing_object_free() {
    let tmp = tempfile::tempdir().unwrap();
    let mut svc = svc_min(1);
    svc["pricing"] = json!({"mode": "free"});
    let p = write_cfg(tmp.path(), &cfg_with_services(json!([svc])));
    let s = &load_config_at(&p)[0];
    assert!(matches!(s.pricing_mode, o2a_config::PricingMode::Free));
}

#[test]
fn test_pricing_object_invalid_mode_falls_back() {
    let tmp = tempfile::tempdir().unwrap();
    let mut svc = svc_min(1);
    svc["pricing"] = json!({"mode": "bogus"});
    let p = write_cfg(tmp.path(), &cfg_with_services(json!([svc])));
    let s = &load_config_at(&p)[0];
    assert!(matches!(s.pricing_mode, o2a_config::PricingMode::Token));
}

#[test]
fn test_pricing_default_is_token() {
    let tmp = tempfile::tempdir().unwrap();
    let p = write_cfg(tmp.path(), &cfg_with_services(json!([svc_min(1)])));
    let s = &load_config_at(&p)[0];
    assert!(matches!(s.pricing_mode, o2a_config::PricingMode::Token));
    assert_eq!(s.pricing_raw, json!(""));
}

// ---------- 写回格式（2 空格缩进 + 非 ASCII 不转义 + .bak） ----------

#[test]
fn test_write_back_format_and_backup() {
    let tmp = tempfile::tempdir().unwrap();
    let p = write_cfg(
        tmp.path(),
        &cfg_with_services(json!([{
            "comment": "中文服务名", "account": "acc-1",
            "client": "openai", "listen_address": 18001, "model": "m"
        }])),
    );
    let original = fs::read_to_string(&p).unwrap();
    let _ = load_config_at(&p);
    let written = fs::read_to_string(&p).unwrap();
    let backup = fs::read_to_string(format!("{}.bak", p.display())).unwrap();
    assert_eq!(backup, original); // .bak = 写回前原始内容
    assert!(written.contains("中文服务名")); // ensure_ascii=False 语义：非 ASCII 不转义
    assert!(written.contains("\n  \"services\"")); // 2 空格缩进
    let cfg: Value = serde_json::from_str(&written).unwrap();
    assert!(cfg["services"][0]["id"].as_str().unwrap().starts_with("svc-"));
}

// ---------- round-trip：主 worktree config.example.json 字段完整 ----------

#[test]
fn test_roundtrip_example_config() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/config.example.json");
    let tmp = tempfile::tempdir().unwrap();
    // 拷贝到临时目录：example 有无 id 的服务，加载会触发写回
    let p = tmp.path().join("config.json");
    fs::copy(&fixture, &p).unwrap();
    // 同目录放 auth.json，提供账号 key（example 内嵌 key 为空）
    fs::write(
        tmp.path().join("auth.json"),
        json!({"acc-1": {"type": "api_key", "key": "sk-example"},
                "acc-2": "sk-anthropic"})
            .to_string(),
    )
    .unwrap();

    let services = load_config_at(&p);
    assert_eq!(services.len(), 4);

    let by_name = |n: &str| {
        services
            .iter()
            .find(|s| s.name.contains(n))
            .unwrap_or_else(|| panic!("service {} not found", n))
    };

    // 服务 1：Chat Completions 整包透传
    let s1 = by_name("pi / OpenAI");
    assert_eq!(s1.id, "svc-7f3a9c21");
    assert_eq!(s1.port, 11011); // 字符串端口解析
    assert_eq!(s1.api, Some(o2a_config::OpenaiApi::OpenaiCompletions));
    assert_eq!(s1.mode(), o2a_config::DispatchMode::Codex);
    assert!(s1.override_model);
    assert_eq!(s1.max_tokens, 1_000_000); // context_1m
    assert_eq!(s1.account.api_key, "sk-example"); // auth.json dict 形态

    // 服务 2：Responses 整包透传（上游原生）
    let s2 = by_name("Codex → DeepSeek");
    assert!(!s2.id.is_empty() && s2.id.starts_with("svc-")); // 无 id → 惰性生成
    assert_eq!(s2.api, Some(o2a_config::OpenaiApi::OpenaiResponses));
    assert_eq!(s2.upstream_api, o2a_config::UpstreamApi::OpenaiResponses);

    // 服务 3：Claude Code 转换（thinking effort）
    let s3 = by_name("Claude Code 转换");
    assert_eq!(s3.thinking_mode, o2a_config::ThinkingMode::Effort);
    assert_eq!(s3.mode(), o2a_config::DispatchMode::Claude);

    // 服务 4：Anthropic 直连（subscription 计价）
    let s4 = by_name("Claude Code 直连");
    assert_eq!(s4.id, "svc-e8d54f2a");
    assert_eq!(s4.mode(), o2a_config::DispatchMode::Direct);
    assert!(matches!(s4.pricing_mode, o2a_config::PricingMode::Subscription));
    assert_eq!(s4.account.api_key, "sk-anthropic"); // auth.json 字符串形态
    assert_eq!(s4.target_url(), "https://api.anthropic.com/v1/messages");

    // 顶层 auth_token 无（example 有 qs-cc！）
    let s1 = by_name("pi / OpenAI");
    assert_eq!(s1.auth_token, "qs-cc"); // 服务级未配 → 顶层全局兜底
}

// ---------------------------------------------------------------------------
// 顶层 retry 块解析(Phase 2 引擎侧自动重试配置)
// ---------------------------------------------------------------------------

#[test]
fn retry_settings_default_disabled() {
    // 缺省:整体关闭,保持透传语义
    let s = o2a_config::resolve_retry_settings(&json!({}));
    assert!(!s.enabled);
    assert_eq!(s.max_attempts, 5);
    assert_eq!(s.base_ms, 1000);
    assert_eq!(s.max_ms, 30000);

    // retry 非对象(如字符串)→ 默认
    let s = o2a_config::resolve_retry_settings(&json!({"retry": "oops"}));
    assert!(!s.enabled);
}

#[test]
fn retry_settings_parsed() {
    let s = o2a_config::resolve_retry_settings(&json!({
        "retry": {"enabled": true, "max_attempts": 3, "base_ms": 250, "max_ms": 2000}
    }));
    assert!(s.enabled);
    assert_eq!(s.max_attempts, 3);
    assert_eq!(s.base_ms, 250);
    assert_eq!(s.max_ms, 2000);
    // 部分字段:缺省项保持默认
    let s = o2a_config::resolve_retry_settings(&json!({"retry": {"enabled": true}}));
    assert!(s.enabled);
    assert_eq!(s.max_attempts, 5);
}

#[test]
fn retry_settings_invalid_fallback() {
    // 类型非法/非正数 → 逐字段回退默认
    let s = o2a_config::resolve_retry_settings(&json!({
        "retry": {"enabled": "yes", "max_attempts": -1, "base_ms": 0, "max_ms": "x"}
    }));
    assert!(!s.enabled);
    assert_eq!(s.max_attempts, 5);
    assert_eq!(s.base_ms, 1000);
    assert_eq!(s.max_ms, 30000);
}

#[test]
fn retry_settings_max_lt_base_clamped() {
    // max_ms < base_ms → 回退 max_ms = base_ms
    let s = o2a_config::resolve_retry_settings(&json!({
        "retry": {"enabled": true, "base_ms": 5000, "max_ms": 100}
    }));
    assert_eq!(s.max_ms, s.base_ms);
    assert_eq!(s.max_ms, 5000);
}
