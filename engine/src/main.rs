//! o2a-engine：o2a-proxy 的 Rust 引擎二进制（M2 骨架）。
//!
//! 行为基准：Python `o2a/engine.py` 的 serve / _start_service_app / handle_get /
//! _check_auth / _task_* / 热重载 / 父进程 watchdog（见 docs/rust-rewrite.md §3/§4/§11）。
//! M3/M4 将填充 POST 代理分发（claude/codex/direct，当前 501 占位）。
//!
//! CLI（对齐 Python `python -m o2a`）：
//!   o2a-engine [--service <id|comment|port>] [--config <路径|目录>] [--auth <路径|目录>]

mod auth;
mod claude;
mod codex;
mod direct;
mod handlers;
mod proxy;
mod reload;
mod state;
mod stats_sink;
#[cfg(test)]
mod m4_tests;
mod m5_quota;
#[cfg(test)]
mod proxy_tests;
#[cfg(test)]
mod tests;

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use o2a_config::{load_config, Service};
use state::{select_services, EngineState};

#[derive(Default)]
struct Args {
    service: Option<String>,
    config: Option<String>,
    auth: Option<String>,
}

/// 逐对扫描参数（对齐 Python main() 的 argv 索引扫描；未知参数忽略）。
fn parse_args(argv: Vec<String>) -> Args {
    let mut args = Args::default();
    let mut it = argv.into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--service" => args.service = it.next(),
            "--config" => args.config = it.next(),
            "--auth" => args.auth = it.next(),
            _ => {}
        }
    }
    args
}

fn init_logging() {
    use tracing_subscriber::{fmt, EnvFilter};
    use tracing_subscriber::fmt::format::Writer as FmtWriter;
    use tracing_subscriber::fmt::FormatFields;
    // 日志行格式对齐 Python logging："2026-09-03 21:40:09 [INFO] 消息"
    // （本地时间、无 ANSI 色码、等级加方括号；tracing 的 WARN 映射为 WARNING）
    struct PyFormat;
    impl<S, N> fmt::FormatEvent<S, N> for PyFormat
    where
        S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
        N: for<'a> fmt::FormatFields<'a> + 'static,
    {
        fn format_event(
            &self,
            ctx: &fmt::FmtContext<'_, S, N>,
            mut w: FmtWriter<'_>,
            event: &tracing::Event<'_>,
        ) -> std::fmt::Result {
            let level = match *event.metadata().level() {
                tracing::Level::WARN => "WARNING".to_string(),
                other => other.to_string(),
            };
            write!(w, "{} [{}] ", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"), level)?;
            ctx.format_fields(w.by_ref(), event)?;
            writeln!(w)
        }
    }
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with_target(false)
        .with_ansi(false)
        .event_format(PyFormat)
        .init();
}

fn main() -> ExitCode {
    let args = parse_args(std::env::args().skip(1).collect());
    // --config / --auth 写等价 env（对齐 Python main() 的 os.environ 方式，
    // load_config / 路径解析统一走 env 语义）
    if let Some(c) = &args.config {
        std::env::set_var("O2A_CONFIG", c);
    }
    if let Some(a) = &args.auth {
        std::env::set_var("O2A_AUTH", a);
    }
    init_logging();
    let services = load_config();
    let selected = select_services(services, args.service.as_deref());
    if selected.is_empty() {
        tracing::error!("没有可用的服务（请检查 config.json 的 services 与 API key）");
        return ExitCode::from(1);
    }
    match run_engine(selected, args.service) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("{e:#}");
            ExitCode::from(1)
        }
    }
}

fn run_engine(services: Vec<Service>, filter: Option<String>) -> anyhow::Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(serve_all(services, filter))
}

async fn serve_all(services: Vec<Service>, filter: Option<String>) -> anyhow::Result<()> {
    let state = Arc::new(EngineState::new(o2a_config::resolve_config_path(), filter)?);
    let mut started: Vec<(String, state::RunnerHandle)> = Vec::new();
    for svc in services {
        match state.start_service(svc.clone()).await {
            Ok(handle) => {
                tracing::info!(
                    "代理启动: http://{}:{} mode={} target={} model={}",
                    svc.host,
                    svc.port,
                    svc.mode().as_str(),
                    svc.target_url(),
                    svc.model
                );
                if svc.auth_token.is_empty() {
                    tracing::warn!(
                        "[安全] 服务 {} 未配置 auth_token（services[].auth_token 与顶层均未配置），端口 {} 无接入鉴权（可在 config.json 中设置）",
                        svc.name,
                        svc.port
                    );
                }
                started.push((svc.id.clone(), handle));
            }
            Err(e) => {
                tracing::error!("启动失败: {e:#}");
                for (_, h) in started {
                    h.shutdown().await;
                }
                return Err(e);
            }
        }
    }
    state
        .runners
        .write()
        .unwrap()
        .extend(started);

    spawn_watchdog();
    #[cfg(unix)]
    spawn_sighup_handler(state.clone());

    tokio::signal::ctrl_c().await.ok();
    tracing::info!("收到中断信号，关闭代理");
    state.shutdown_all().await;
    Ok(())
}

/// 父进程退出后自动关闭（对齐 `_parent_watchdog`：线程 2s 检查 getppid 变化）。
/// Windows 无 getppid 等价（Python os.getppid 在 Windows 可用但语义弱），
/// 桌面端本就通过子进程句柄管理生命周期，此处 cfg 跳过。
fn spawn_watchdog() {
    #[cfg(unix)]
    {
        let parent = unsafe { libc::getppid() };
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_secs(2));
            if unsafe { libc::getppid() } != parent {
                tracing::info!("检测到父进程已退出，代理自动关闭");
                std::process::exit(0);
            }
        });
    }
}

/// SIGHUP 热重载（仅 POSIX；Windows 走 POST /_reload，对齐 Python 注册逻辑）。
#[cfg(unix)]
fn spawn_sighup_handler(state: Arc<EngineState>) {
    use tokio::signal::unix::{signal, SignalKind};
    match signal(SignalKind::hangup()) {
        Ok(mut hup) => {
            tokio::spawn(async move {
                while hup.recv().await.is_some() {
                    tracing::info!("[reload] SIGHUP 触发热重载");
                    tokio::spawn(reload::do_reload(state.clone()));
                }
            });
        }
        Err(e) => tracing::debug!("[reload] SIGHUP 注册失败（不影响运行）: {e}"),
    }
}
