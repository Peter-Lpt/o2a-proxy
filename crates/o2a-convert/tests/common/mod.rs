//! 测试公共构造器（对齐 Python tests 的 `make_service`）。

use o2a_config::{Account, Service, ThinkingMode};

pub fn make_service(url: &str, model: &str, mode: ThinkingMode) -> Service {
    let acc = Account {
        id: "a1".into(),
        name: "a1".into(),
        api_key: "k".into(),
        openai_url: url.into(),
        anthropic_url: String::new(),
        api: String::new(),
        quota_source: "auto".into(),
        quota: None,
    };
    Service {
        id: "svc-test".into(),
        name: "s1".into(),
        account: acc,
        client: o2a_config::ClientKind::Auto,
        host: "127.0.0.1".into(),
        port: 18000,
        model: model.into(),
        override_model: true,
        max_tokens: 4096,
        proxy: String::new(),
        api: None,
        upstream_api: Default::default(),
        thinking_mode: mode,
        pricing_mode: o2a_config::PricingMode::Token,
        pricing_extra: None,
        pricing_raw: serde_json::json!(""),
        auth_token: String::new(),
        order: 0,
        enabled: true,
        autostart: false,
        models: vec![],
        models_map: Default::default(),
        model_policy: Default::default(),
        mode_override: None,
    }
}
