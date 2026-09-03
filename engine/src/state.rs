//! 任务状态与引擎运行时状态（对齐 Python engine.py 的 _task_* / 热重载状态机）。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use o2a_config::Service;

/// 上游连接池上限（对齐 `UPSTREAM_POOL_LIMIT`）。
pub const UPSTREAM_POOL_LIMIT: usize = 200;
/// 请求体上限：1M 上下文场景请求可能很大（对齐 `MAX_BODY_SIZE`）。
pub const MAX_BODY_SIZE: usize = 128 * 1024 * 1024;
/// 上游建连超时（对齐 `CONNECT_TIMEOUT = 120`；reqwest 无 per-request connect
/// timeout，统一设在 Client 上）。
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(120);
/// 流式响应总/读间隔超时（对齐 `STREAM_TIMEOUT = 600`）。
pub const STREAM_TIMEOUT: Duration = Duration::from_secs(600);

/// 重载中标记的引擎级实现（对齐 Python 模块级 `_O2A_RELOADING`：双职，
/// 既作 503 语义判定，也作重载并发去重）。
///
/// Python 中一个进程只有一个引擎，全局即引擎级；Rust 同理把标记挂在
/// `EngineState` 上（同引擎所有服务共享；测试多引擎实例互不干扰）。
#[derive(Default)]
pub struct ReloadFlag(pub(crate) AtomicBool);

impl ReloadFlag {
    /// 当前是否在重载中。
    pub fn active(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }

    /// 原子置位：未在重载时置为 true 并返回守卫（Drop 时自动复位，
    /// 对齐 Python `finally: _O2A_RELOADING = False`）；已在重载中返回 None。
    pub fn try_begin(&self) -> Option<ReloadGuard<'_>> {
        match self.0.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst) {
            Ok(_) => Some(ReloadGuard(self)),
            Err(_) => None,
        }
    }
}

/// 重载守卫：存活期间标记为 true，Drop（含 panic 展开栈）时复位。
pub struct ReloadGuard<'a>(&'a ReloadFlag);

impl Drop for ReloadGuard<'_> {
    fn drop(&mut self) {
        self.0 .0.store(false, Ordering::SeqCst);
    }
}

/// 单服务任务状态（对齐 `_task` / `_task_begin` / `_task_end` / `_task_finish`）。
///
/// 无会话 id 的启发式：只看“最后到最后”的全局信号：
/// active = 有在途流 或 最近一次 finish 是 continue（长链中间）。
#[derive(Debug)]
pub struct TaskState {
    pub active_streams: i64,
    pub last_finish: String, // continue | final | none
    pub last_activity: f64,
}

impl Default for TaskState {
    fn default() -> Self {
        Self {
            active_streams: 0,
            last_finish: "none".to_string(),
            last_activity: 0.0,
        }
    }
}

impl TaskState {
    pub fn begin(&mut self) {
        self.active_streams += 1;
    }

    pub fn end(&mut self) {
        self.active_streams = (self.active_streams - 1).max(0);
    }

    pub fn finish(&mut self, is_final: bool) {
        self.last_finish = if is_final { "final" } else { "continue" }.to_string();
        self.last_activity = unix_now();
    }

    /// active 判定（对齐 `_task_snapshot`）。
    pub fn active(&self) -> bool {
        self.active_streams > 0 || self.last_finish == "continue"
    }
}

#[allow(dead_code)]
fn unix_now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// finish_reason -> 是否“最终答复”（对齐 `_classify`）。
///
/// tool_calls/tool_use 与 length/max_tokens 均为长链中间（continue）；
/// stop / end_turn / None / 其它 → final。
pub fn classify(finish_reason: Option<&str>, has_tool_call: bool) -> bool {
    match finish_reason {
        Some(fr) if has_tool_call || fr == "tool_calls" || fr == "tool_use" => false,
        Some(fr) if fr == "length" || fr == "max_tokens" => false,
        _ => true,
    }
}

/// Python truthiness：None/False/0/空串/空容器 → false（对齐 o2a-config 内部同名语义）。
pub fn py_truthy(v: &serde_json::Value) -> bool {
    match v {
        serde_json::Value::Null => false,
        serde_json::Value::Bool(b) => *b,
        serde_json::Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(false),
        serde_json::Value::String(s) => !s.is_empty(),
        serde_json::Value::Array(a) => !a.is_empty(),
        serde_json::Value::Object(o) => !o.is_empty(),
    }
}

/// 任务状态只读快照（/status 与测试断言用，对齐 `_task_snapshot` 字段）。
#[derive(Debug, Clone)]
pub struct TaskSnapshot {
    pub active: bool,
    pub active_streams: i64,
    pub last_finish: String,
    pub last_activity: f64,
}

/// 每服务的请求处理状态（handler 共享；service 可被热重载原地替换）。
pub struct ServiceState {
    pub service: Arc<RwLock<Service>>,
    pub task: std::sync::Mutex<TaskState>,
    /// 引擎级状态（/_reload 触发用）；测试直连 Router 时可为 None
    pub engine: Option<Weak<EngineState>>,
    /// 上游客户端（对齐 app["session"]：连接池复用）
    pub client: reqwest::Client,
    /// 统计挂点（生产为 o2a-stats 实现；测试可注入 NoopSink / 捕获 sink）
    pub stats: Arc<dyn crate::proxy::StatsSink>,
    /// 统计注册表（/stats 端点与 /pricing-reload 用；NoopSink 测试路径为 None）
    pub stats_registry: Option<Arc<o2a_stats::StatsRegistry>>,
}

impl ServiceState {
    pub fn new(service: Service, engine: Option<Weak<EngineState>>) -> Self {
        Self::with_sink(service, engine, Arc::new(crate::proxy::NoopSink))
    }

    // 任务状态便捷封装：调用方不直接触碰 Mutex（锁内仅做单条 TaskState 方法）。
    pub fn task_begin(&self) {
        self.task.lock().unwrap().begin();
    }

    pub fn task_end(&self) {
        self.task.lock().unwrap().end();
    }

    pub fn task_finish(&self, is_final: bool) {
        self.task.lock().unwrap().finish(is_final);
    }

    pub fn task_snapshot(&self) -> TaskSnapshot {
        let t = self.task.lock().unwrap();
        TaskSnapshot {
            active: t.active(),
            active_streams: t.active_streams,
            last_finish: t.last_finish.clone(),
            last_activity: t.last_activity,
        }
    }

    /// 测试/特殊用途：自定义统计接收端（无注册表 → /stats 501）。
    pub fn with_sink(
        service: Service,
        engine: Option<Weak<EngineState>>,
        sink: Arc<dyn crate::proxy::StatsSink>,
    ) -> Self {
        let client = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .pool_max_idle_per_host(UPSTREAM_POOL_LIMIT)
            .use_rustls_tls()
            .build()
            .expect("upstream client build");
        Self {
            service: Arc::new(RwLock::new(service)),
            task: std::sync::Mutex::new(TaskState::default()),
            engine,
            client,
            stats: sink,
            stats_registry: None,
        }
    }

    /// 生产路径：o2a-stats 接收端 + 注册表（/stats /pricing-reload 可用）。
    pub fn with_stats_registry(
        service: Service,
        engine: Option<Weak<EngineState>>,
        registry: Arc<o2a_stats::StatsRegistry>,
    ) -> Self {
        let sink = Arc::new(crate::stats_sink::O2aStatsSink { registry: registry.clone() });
        let mut st = Self::with_sink(service, engine, sink);
        st.stats_registry = Some(registry);
        st
    }
}

/// 单服务监听句柄（对齐 AppRunner：热重载 stop/swap 用）。
pub struct RunnerHandle {
    pub state: Arc<ServiceState>,
    shutdown: tokio::sync::watch::Sender<bool>,
    join: tokio::task::JoinHandle<()>,
}

impl RunnerHandle {
    /// 停止监听并等待退出（对齐 `runner.cleanup()`；5s 超时兜底 abort）。
    pub async fn shutdown(self) {
        let Self { shutdown, mut join, .. } = self;
        let _ = shutdown.send(true);
        tokio::select! {
            _ = &mut join => {}
            _ = tokio::time::sleep(Duration::from_secs(5)) => { join.abort(); }
        }
    }
}

/// 引擎级状态：共享上游客户端 + runner 表（热重载 diff 的操作对象）。
pub struct EngineState {
    #[allow(dead_code)] // 上游请求使用
    pub client: reqwest::Client,
    pub config_path: std::path::PathBuf,
    pub filter: Option<String>,
    pub runners: RwLock<HashMap<String, RunnerHandle>>,
    /// 重载中标记（503 语义 + 并发去重；见 `ReloadFlag`）。
    pub reloading: ReloadFlag,
    /// 统计注册表（按 config/env 解析；统计禁用时为 None → NoopSink）
    pub stats: Option<Arc<o2a_stats::StatsRegistry>>,
    /// 引擎级额度查询缓存（对齐 Python 模块级 `_quota_cache = TTLCache(60)`，
    /// 同引擎所有服务共享，键 = 账号 id）。
    pub quota_cache: o2a_quota::base::TTLCache,
}

impl EngineState {
    pub fn new(config_path: std::path::PathBuf, filter: Option<String>) -> anyhow::Result<Self> {
        let client = reqwest::Client::builder()
            .pool_max_idle_per_host(UPSTREAM_POOL_LIMIT)
            .use_rustls_tls()
            .build()?;
        // 统计设置（对齐 Python：CACHE_STATS_* env setdefault + config 顶层字段）
        let cfg_raw: serde_json::Value = std::fs::read_to_string(&config_path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or(serde_json::json!({}));
        let settings = o2a_config::resolve_stats_settings(&cfg_raw);
        let stats = if settings.enabled {
            let pricing_path = o2a_config::resolve_pricing_path(Some(&config_path));
            Some(Arc::new(o2a_stats::StatsRegistry::new(
                settings.dir,
                settings.retention_days,
                Some(pricing_path),
            )))
        } else {
            None
        };
        Ok(Self {
            client,
            config_path,
            filter,
            runners: RwLock::new(HashMap::new()),
            reloading: ReloadFlag::default(),
            stats,
            quota_cache: o2a_quota::base::TTLCache::new(60),
        })
    }

    /// 绑定端口并后台起服务（对齐 `_start_service_app`）。
    pub async fn start_service(self: &Arc<Self>, svc: Service) -> anyhow::Result<RunnerHandle> {
        let port = u16::try_from(svc.port)
            .map_err(|_| anyhow::anyhow!("非法端口 {}", svc.port))?;
        let listener = tokio::net::TcpListener::bind((svc.host.as_str(), port)).await?;
        let st = match &self.stats {
            Some(reg) => Arc::new(ServiceState::with_stats_registry(
                svc,
                Some(Arc::downgrade(self)),
                reg.clone(),
            )),
            None => Arc::new(ServiceState::new(svc, Some(Arc::downgrade(self)))),
        };
        let router = crate::handlers::build_router(st.clone());
        let (tx, mut rx) = tokio::sync::watch::channel(false);
        let join = tokio::spawn(async move {
            let _ = axum::serve(listener, router)
                .with_graceful_shutdown(async move {
                    let _ = rx.changed().await;
                })
                .await;
        });
        Ok(RunnerHandle {
            state: st,
            shutdown: tx,
            join,
        })
    }

    pub async fn shutdown_all(&self) {
        let runners: Vec<RunnerHandle> = self
            .runners
            .write()
            .unwrap()
            .drain()
            .map(|(_, v)| v)
            .collect();
        for r in runners {
            r.shutdown().await;
        }
    }
}

/// --service 过滤匹配（对齐 serve 的 filter 判定）：id / comment（显示名）/ 端口三者任一。
pub fn matches_filter(svc: &Service, filter: &str) -> bool {
    svc.id == filter || svc.name == filter || svc.port.to_string() == filter
}

/// 装载选择（对齐 serve 开头的过滤链）：enabled 且 account.valid，再按 --service 过滤。
pub fn select_services(services: Vec<Service>, filter: Option<&str>) -> Vec<Service> {
    services
        .into_iter()
        .filter(|s| s.enabled && s.account.valid())
        .filter(|s| match filter {
            Some(f) => matches_filter(s, f),
            None => true,
        })
        .collect()
}

/// 按 id diff（对齐 `diff_services`）：返回 (start_ids, stop_ids, swap_ids)。
///
/// 迭代序以 id 升序固定（BTreeMap）：Python dict 是插入序，HashMap 迭代序随机，
/// 取确定性序保证 start/stop 序列可测；仅影响绑定时序不影响正确性。
///
/// - start：新增，或 host/port 变化（需换端口绑定 → 先起新后停旧）
/// - stop：被删除，或 host/port 变化的旧实例
/// - swap：id 保留且绑定未变 → 原地替换 Service 即刻生效
pub fn diff_services(
    old: &HashMap<String, Service>,
    new: &HashMap<String, Service>,
) -> (Vec<String>, Vec<String>, Vec<String>) {
    let old: std::collections::BTreeMap<&String, &Service> = old.iter().collect();
    let new: std::collections::BTreeMap<&String, &Service> = new.iter().collect();
    let mut start_ids = Vec::new();
    let mut stop_ids = Vec::new();
    let mut swap_ids = Vec::new();
    for (sid, old_svc) in &old {
        match new.get(sid) {
            None => stop_ids.push((*sid).clone()),
            Some(new_svc) => {
                if (new_svc.host.as_str(), new_svc.port) != (old_svc.host.as_str(), old_svc.port) {
                    start_ids.push((*sid).clone());
                    stop_ids.push((*sid).clone());
                } else {
                    swap_ids.push((*sid).clone());
                }
            }
        }
    }
    for sid in new.keys() {
        if !old.contains_key(sid) {
            start_ids.push((*sid).clone());
        }
    }
    (start_ids, stop_ids, swap_ids)
}

#[cfg(test)]
mod tests {
    use super::*;
    use o2a_config::{Account, ClientKind, ModelPolicy, ModelsMap, PricingMode, ThinkingMode, UpstreamApi};

    pub(crate) fn base_service(id: &str, name: &str, model: &str) -> Service {
        Service {
            id: id.to_string(),
            name: name.to_string(),
            account: Account {
                id: "acc-1".to_string(),
                name: "账号A".to_string(),
                api_key: "sk-test".to_string(),
                openai_url: "https://upstream.example/v1/chat/completions".to_string(),
                anthropic_url: String::new(),
                api: String::new(),
                quota_source: "auto".to_string(),
                quota: None,
            },
            client: ClientKind::Openai,
            host: "127.0.0.1".to_string(),
            port: 0,
            model: model.to_string(),
            override_model: true,
            max_tokens: 4096,
            proxy: String::new(),
            api: None,
            upstream_api: UpstreamApi::OpenaiCompletions,
            thinking_mode: ThinkingMode::Auto,
            pricing_mode: PricingMode::Token,
            pricing_extra: None,
            pricing_raw: serde_json::Value::String(String::new()),
            auth_token: String::new(),
            order: 0,
            enabled: true,
            autostart: false,
            models: Vec::new(),
            models_map: ModelsMap::default(),
            model_policy: ModelPolicy::Clamp,
            mode_override: None,
        }
    }

    #[test]
    fn classify_matches_python() {
        assert!(classify(Some("stop"), false));
        assert!(classify(None, false));
        assert!(classify(Some(""), false));
        assert!(!classify(Some("tool_calls"), false));
        assert!(!classify(Some("tool_use"), false));
        assert!(!classify(Some("stop"), true)); // has_tool_call 优先
        assert!(!classify(Some("length"), false));
        assert!(!classify(Some("max_tokens"), false));
        assert!(classify(Some("other"), false));
    }

    #[test]
    fn task_state_semantics() {
        let mut t = TaskState::default();
        assert!(!t.active());
        assert_eq!(t.last_finish, "none");
        t.begin();
        t.begin();
        assert!(t.active());
        assert_eq!(t.active_streams, 2);
        t.end();
        t.end();
        t.end(); // 不下探到负数
        assert_eq!(t.active_streams, 0);
        assert!(!t.active());
        t.finish(true);
        assert_eq!(t.last_finish, "final");
        t.finish(false);
        assert_eq!(t.last_finish, "continue");
        assert!(t.active()); // continue 视为 active（长链中间）
        assert!(t.last_activity > 0.0);
    }

    #[test]
    fn select_services_enabled_valid_and_filter() {
        let mut services = vec![
            base_service("svc-a", "s1", "m1"),
            base_service("svc-b", "s2", "m2"),
        ];
        services[1].enabled = false;
        services[0].account.api_key = String::new(); // invalid
        let mut valid = base_service("svc-c", "s3", "m3");
        valid.port = 12345;
        services.push(valid);

        assert_eq!(select_services(services.clone(), None).len(), 1);
        assert_eq!(select_services(services.clone(), Some("svc-c")).len(), 1);
        assert_eq!(select_services(services.clone(), Some("s3")).len(), 1);
        assert_eq!(select_services(services.clone(), Some("12345")).len(), 1);
        assert!(select_services(services, Some("svc-zz")).is_empty());
    }

    #[test]
    fn diff_services_start_stop_swap() {
        let mut old = HashMap::new();
        old.insert("a".into(), base_service("a", "a", "m"));
        old.insert("b".into(), base_service("b", "b", "m"));
        old.insert("c".into(), base_service("c", "c", "m"));
        let mut new = HashMap::new();
        new.insert("a".into(), base_service("a", "a", "m-new")); // swap
        let mut b2 = base_service("b", "b", "m");
        b2.port = 9999; // 换端口 → start + stop
        new.insert("b".into(), b2);
        new.insert("d".into(), base_service("d", "d", "m")); // 新增

        let (start, stop, swap) = diff_services(&old, &new);
        assert_eq!(start, vec!["b".to_string(), "d".to_string()]);
        assert_eq!(stop, vec!["b".to_string(), "c".to_string()]);
        assert_eq!(swap, vec!["a".to_string()]);
    }
}
