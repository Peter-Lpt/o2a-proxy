use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::AppState;

/// 事件出口：引擎子进程「启动失败 / 运行后停止」异步通知前端。
/// lib.rs setup 注入 Tauri emit；测试注入记录器。
type EventCallback = Box<dyn Fn(&str, &str, &str) + Send + Sync>;
static EVENT_CB: std::sync::OnceLock<EventCallback> = std::sync::OnceLock::new();

/// 注入事件出口（App 启动时调用一次；重复调用忽略）。
/// kind: "proxy-start-failed" | "proxy-stopped"；payload = {id, message}。
pub fn set_event_callback(cb: EventCallback) {
    let _ = EVENT_CB.set(cb);
}

fn emit_proxy_event(kind: &str, name: &str, message: &str) {
    if let Some(cb) = EVENT_CB.get() {
        cb(kind, name, message);
    }
}

/// 测试夹具：递归复制目录。
#[cfg(test)]
fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let e = entry?;
        let from = e.path();
        let to = dst.join(e.file_name());
        if e.file_type()?.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// 测试夹具：确保 engine 二进制存在并拷贝到目标目录（root/o2a-engine）。
/// 二进制来源：workspace target/debug（cargo test 不构建 workspace crate，
/// 缺失时临时 cargo build -p o2a-engine 产出）。
#[cfg(test)]
fn ensure_engine_binary(root: &std::path::Path) -> PathBuf {
    let bin_name = if cfg!(target_os = "windows") {
        "o2a-engine.exe"
    } else {
        "o2a-engine"
    };
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let built = workspace.join("target").join("debug").join(bin_name);
    if !built.is_file() {
        let status = std::process::Command::new("cargo")
            .args(["build", "-p", "o2a-engine", "-q"])
            .current_dir(&workspace)
            .status()
            .expect("cargo 不可用，无法构建测试用引擎二进制");
        assert!(status.success(), "cargo build -p o2a-engine 失败");
    }
    let dest = root.join(bin_name);
    std::fs::copy(&built, &dest).unwrap();
    dest
}

fn config_services(state: &AppState) -> Vec<serde_json::Value> {
    crate::read_config_value(state)
        .ok()
        .and_then(|c| c.get("services").cloned())
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
}

fn find_service<'a>(services: &'a [serde_json::Value], name: &str) -> Option<&'a serde_json::Value> {
    services.iter().find(|s| {
        let mode = s.get("mode").and_then(|m| m.as_str()).unwrap_or("claude");
        if mode != "claude" && mode != "codex" && mode != "direct" && mode != "auto" {
            return false;
        }
        // 同时接受 id（ 服务身份 id 化）与 comment（老配置/老脚本兼容），端口亦可用
        let sid = s.get("id").and_then(|v| v.as_str()).unwrap_or("");
        let comment = s.get("comment").and_then(|c| c.as_str()).unwrap_or("");
        let port = s.get("listen_address").and_then(|p| p.as_str()).unwrap_or("");
        sid == name || comment == name || port == name
    })
}

fn is_service_enabled(svc: &serde_json::Value) -> bool {
    svc.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true)
}

fn is_alive(child: &mut std::process::Child) -> bool {
    child.try_wait().ok().flatten().is_none()
}

/// 行级 tee：逐行写日志文件（面板查看）+ 终端（dev/前台运行）；
/// `on_line` 回调用于识别引擎就绪标记（见 spawn_monitor）。
/// 逐行而非分块：保证就绪标记在打印后立即被识别，不被缓冲滞后。
fn tee_lines<R, F>(reader: R, file: Arc<Mutex<File>>, mut term: Box<dyn Write + Send>, mut on_line: F)
where
    R: Read,
    F: FnMut(&str),
{
    let mut reader = BufReader::new(reader);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        on_line(line.trim_end_matches(['\r', '\n']));
        let bytes = line.as_bytes();
        if let Ok(mut f) = file.lock() {
            let _ = f.write_all(bytes);
            let _ = f.flush();
        }
        let _ = term.write_all(bytes);
        let _ = term.flush();
    }
}

fn log_path(state: &AppState, name: &str) -> PathBuf {
    let safe: String = name
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    // 日志统一收拢到项目根 logs/（目录不存在时自动创建）
    let dir = state.root.join("logs");
    let _ = std::fs::create_dir_all(&dir);
    dir.join(format!("proxy_{}.log", safe))
}

fn read_log_tail(p: &std::path::Path, max: usize) -> String {
    let Ok(mut f) = File::open(p) else {
        return String::new();
    };
    let mut s = String::new();
    if f.read_to_string(&mut s).is_err() {
        return String::new();
    }
    let bytes = s.as_bytes();
    let mut idx = bytes.len().saturating_sub(max);
    while idx < bytes.len() && (bytes[idx] & 0xC0) == 0x80 {
        idx += 1;
    }
    s[idx..].trim().to_string()
}

fn service_host_port(svc: &serde_json::Value) -> (String, u16) {
    let host = svc
        .get("listen_host")
        .and_then(|v| v.as_str())
        .unwrap_or("127.0.0.1")
        .to_string();
    let port: u16 = svc
        .get("listen_address")
        .and_then(|v| v.as_u64().map(|n| n as u16))
        .or_else(|| {
            svc.get("listen_address")
                .and_then(|v| v.as_str())
                .and_then(|x| x.parse().ok())
        })
        .unwrap_or(0);
    (host, port)
}

pub fn start_service(state: &AppState, name: &str) -> Result<(), String> {
    let services = config_services(state);
    let svc = match find_service(&services, name) {
        Some(s) => s,
        None => return Err(format!("未找到服务: {name}")),
    };
    if !is_service_enabled(svc) {
        return Err(format!("服务已停用（enabled=false），请先在面板启用: {name}"));
    }
    // 启动验证用 host/port（引擎将绑定到此地址；在 spawn 前取出，避免借用冲突）
    let (host, port) = service_host_port(svc);
    let already_running = {
        let mut children = state.children.lock().unwrap();
        match children.get_mut(name) {
            Some(child) => {
                if is_alive(child) {
                    true
                } else {
                    children.remove(name);
                    false
                }
            }
            None => false,
        }
    };
    if already_running {
        return Ok(());
    }
    let log = log_path(state, name);
    // 追加写而非覆盖：保留历史日志，便于回溯（旧布局下的根目录 proxy_*.log 历史不会丢失）
    let file = Arc::new(Mutex::new(
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log)
            .map_err(|e| format!("无法创建日志文件: {e}"))?,
    ));
    // 配置位置显式传给子进程：可能来自环境变量或 UI 保存的 settings.json（子进程读不到后者），
    // 保证子进程与桌面端读写同一份 config.json / auth.json。
    let config_path = crate::config_path(state);
    let auth_path = crate::auth_path(state);
    // 配置未显式指定统计目录时，把默认目录传给引擎，保证两端路径一致
    let has_stats_dir = crate::read_config_value(state)
        .ok()
        .and_then(|c| {
            c.get("cache_stats_dir")
                .and_then(|v| v.as_str())
                .map(|s| !s.trim().is_empty())
        })
        .unwrap_or(false);

    let mut cmd = match &state.engine_binary {
        Some(bin) => {
            // Rust 引擎：显式传 CLI 参数（--service/--config/--auth），不依赖 cwd 探测
            let mut cmd = Command::new(bin);
            cmd.arg("--service")
                .arg(name)
                .arg("--config")
                .arg(&config_path)
                .arg("--auth")
                .arg(&auth_path);
            // 显式传桌面端自身 PID：引擎 watchdog 以它为父进程基准。
            // 引擎初始化耗时可能超过桌面端退出窗口，若引擎自己快照 getppid()，
            // 父进程已死时快照到 1（孤儿）永不退出；传 PID 则无此竞态。
            cmd.arg("--parent").arg(std::process::id().to_string());
            // 定价文件：root 下存在时显式传入（引擎默认解析为 env > config 同目录 > cwd）
            let pricing = state.root.join("pricing.json");
            let plans = state.root.join("plans.json");
            if pricing.is_file() {
                cmd.env("O2A_PRICING", &pricing);
            }
            if plans.is_file() {
                cmd.env("O2A_PLANS", &plans);
            }
            cmd
        }
        None => {
            // 过渡期回退：python + proxy_async.py（引擎二进制未找到时；外部 python 引擎仍可用）
            eprintln!("[引擎] 使用 python 引擎回退（未配置 o2a-engine 二进制）");
            let mut cmd = Command::new(&state.python);
            cmd.arg("proxy_async.py").arg("--service").arg(name);
            cmd
        }
    };
    cmd.current_dir(&state.root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Windows 下禁用子进程控制台窗口（CREATE_NO_WINDOW）：
    // 否则从 GUI 桌面端启动引擎子进程时会弹出一个 cmd 黑窗。
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000);
    }
    if !has_stats_dir {
        cmd.env("CACHE_STATS_DIR", &state.default_stats_dir);
    }
    // 配置位置显式传给子进程（python 回退路径走 env；引擎二进制已传 --config/--auth，
    // env 同步传入保持双保险，与旧逻辑一致）
    cmd.env("O2A_CONFIG", &config_path);
    cmd.env("O2A_AUTH", &auth_path);
    let mut child = cmd.spawn().map_err(|e| format!("启动失败: {e}"))?;
    let pid = child.id();
    let stdout = child.stdout.take().map(|r| Box::new(r) as Box<dyn Read + Send>);
    let stderr = child.stderr.take().map(|r| Box::new(r) as Box<dyn Read + Send>);
    state.children.lock().unwrap().insert(name.to_string(), child);
    // spawn 即返回：不再同步等待验证窗口（旧行为固定等 1.2s）。
    // 就绪/失败由监视线程异步检测并事件通知前端：
    // - 就绪：引擎绑定端口后向 stdout 打印「代理启动: http://...」，行级 tee 识别该标记；
    // - 失败：进程退出且从未就绪 → proxy-start-failed 事件（附日志尾部）；
    // - 运行后停止/退出：proxy-stopped 事件（前端刷新状态）。
    spawn_monitor(
        Arc::clone(&state.children),
        name,
        pid,
        file,
        stdout,
        stderr,
        &host,
        port,
        &log,
    );
    Ok(())
}

/// 引擎子进程监视：就绪标记识别 + 退出检测 + 事件通知。
/// 三个线程：stdout 行级 tee（识别就绪标记）/ stderr 行级 tee / 退出轮询。
/// 只持有 children 的 Arc（不拖住整个 AppState 的借用生命周期，线程可 'static）。
/// 跨平台：BufReader 逐行读、TcpStream 探测、线程模型在 Windows/macOS/Linux
/// 语义一致；Rust 的 stdout 为行缓冲（LineWriter），标记打印后可实时被识别。
fn spawn_monitor(
    children: Arc<Mutex<HashMap<String, Child>>>,
    name: &str,
    pid: u32,
    file: Arc<Mutex<File>>,
    stdout: Option<Box<dyn Read + Send>>,
    stderr: Option<Box<dyn Read + Send>>,
    host: &str,
    port: u16,
    log: &std::path::Path,
) {
    let ready = Arc::new(AtomicBool::new(false));
    if let Some(out) = stdout {
        let file = Arc::clone(&file);
        let ready = Arc::clone(&ready);
        thread::spawn(move || {
            tee_lines(out, file, Box::new(std::io::stdout()), move |line| {
                if !ready.load(Ordering::SeqCst) && line.contains("代理启动") {
                    ready.store(true, Ordering::SeqCst);
                }
            });
        });
    }
    if let Some(err) = stderr {
        thread::spawn(move || tee_lines(err, file, Box::new(std::io::stderr()), |_| {}));
    }
    let name = name.to_string();
    let host = host.to_string();
    let log = log.to_path_buf();
    let ready = Arc::clone(&ready);
    thread::spawn(move || loop {
        thread::sleep(Duration::from_millis(150));
        let mut children = children.lock().unwrap();
        match children.get_mut(&name) {
            // 仍是本实例：检查退出；python 回退引擎可能不打印就绪标记，
            // 端口可达也视为就绪（与 toggle_service 的 port_open 判定一致）
            Some(c) if c.id() == pid => match c.try_wait() {
                Ok(None) => {
                    if !ready.load(Ordering::SeqCst) && port > 0 && crate::port_open(&host, port) {
                        ready.store(true, Ordering::SeqCst);
                    }
                }
                Ok(Some(_)) => {
                    children.remove(&name);
                    drop(children);
                    handle_child_exit(&name, &ready, &log);
                    break;
                }
                Err(_) => break,
            },
            // 已被移除（stop_service 用户停止）或被新实例替换（重启）：
            // 本实例的监视到此结束，不发事件（主动停止不误报启动失败）
            _ => break,
        }
    });
}

/// 子进程退出后的通知逻辑：
/// - 已就绪：视为运行后停止/退出（含用户停止——但用户停止会先移除管理表，
///   监视线程因 pid 不匹配提前退出，不会走到这里），通知前端刷新状态；
/// - 未就绪：启动失败，附日志尾部推送错误事件。
fn handle_child_exit(name: &str, ready: &AtomicBool, log: &std::path::Path) {
    if ready.load(Ordering::SeqCst) {
        emit_proxy_event("proxy-stopped", name, "");
        return;
    }
    // 稍等日志 tee 线程把尾部写完再读，保证错误信息完整
    thread::sleep(Duration::from_millis(200));
    let tail = read_log_tail(log, 1500);
    let msg = if tail.is_empty() {
        "代理启动后立即退出（无日志输出，请检查端口占用与 API key）".to_string()
    } else {
        format!("代理启动失败，日志：{tail}")
    };
    emit_proxy_event("proxy-start-failed", name, &msg);
}

pub fn stop_service(state: &AppState, name: &str) -> Result<(), String> {
    let mut children = state.children.lock().unwrap();
    if let Some(mut child) = children.remove(name) {
        let _ = child.kill();
        // 最多等 3s 让子进程退出；超时则再次强杀兜底，
        // 避免挂死的子进程让 wait() 永久占住阻塞线程池线程。
        let deadline = std::time::Instant::now() + Duration::from_millis(3000);
        loop {
            if child.try_wait().map_err(|e| e.to_string())?.is_some() {
                break;
            }
            if std::time::Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    Ok(())
}

pub fn toggle_service(state: &AppState, name: &str) -> Result<(), String> {
    let services = config_services(state);
    let Some(svc) = find_service(&services, name) else {
        return Err(format!("未找到服务: {name}"));
    };
    let svc = svc.clone();

    // 自己管理的子进程还在运行 -> 停止
    let child_alive = {
        let mut children = state.children.lock().unwrap();
        children.get_mut(name).map(is_alive).unwrap_or(false)
    };
    if child_alive {
        return stop_service(state, name);
    }

    // 进程不在本客户端管理内，但端口被监听（可能是外部启动）-> 提示无法停止，而不是误启动
    let (host, port) = service_host_port(&svc);
    if port > 0 && crate::port_open(&host, port) {
        return Err(format!(
            "端口 {port} 已有进程监听（可能由外部启动），客户端无法停止；请手动关闭该进程后重试"
        ));
    }

    start_service(state, name)
}

/// 服务的启停 key：优先 id（稳定身份），老配置回退 comment；无身份返回 None。
fn service_start_key(svc: &serde_json::Value) -> Option<String> {
    let name = svc
        .get("id")
        .and_then(|v| v.as_str())
        .filter(|v| !v.is_empty())
        .or_else(|| svc.get("comment").and_then(|c| c.as_str()))
        .unwrap_or("")
        .to_string();
    if name.is_empty() { None } else { Some(name) }
}

pub fn start_all(state: &AppState) -> Result<(), String> {
    let services = config_services(state);
    let mut last_err = None;
    for s in &services {
        // enabled=false：停用态不参与 start_all
        if !is_service_enabled(s) {
            continue;
        }
        let Some(name) = service_start_key(s) else { continue };
        // start_service 即返回（就绪/失败异步通知），无需并行
        if let Err(e) = start_service(state, &name) {
            last_err = Some(e);
        }
    }
    match last_err {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// 启动 autostart=true 且 enabled 的服务（App 启动时自动拉起， 生命周期）。
pub fn start_autostart(state: &AppState) -> Result<(), String> {
    let services = config_services(state);
    let mut last_err = None;
    for s in &services {
        let auto = s
            .get("autostart")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if !auto || !is_service_enabled(s) {
            continue;
        }
        let Some(name) = service_start_key(s) else { continue };
        if let Err(e) = start_service(state, &name) {
            last_err = Some(e);
        }
    }
    match last_err {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

pub fn stop_all(state: &AppState) -> Result<(), String> {
    let names: Vec<String> = state.children.lock().unwrap().keys().cloned().collect();
    for n in names {
        let _ = stop_service(state, &n);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// 测试用事件记录器：所有 emit_proxy_event 事件追加到全局缓冲，
    /// 供 wait_event 轮询断言（cargo test 并行运行共用同一进程，OnceLock 只注册一次）。
    static RECORDED: Mutex<Vec<(String, String, String)>> = Mutex::new(Vec::new());

    fn ensure_recorder() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            set_event_callback(Box::new(|kind, name, message| {
                RECORDED
                    .lock()
                    .unwrap()
                    .push((kind.to_string(), name.to_string(), message.to_string()));
            }));
        });
    }

    /// 轮询等待某服务的指定事件，返回 message；超时返回 None。
    fn wait_event(kind: &str, name: &str, timeout: Duration) -> Option<String> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if let Some((_, _, m)) = RECORDED
                .lock()
                .unwrap()
                .iter()
                .rev()
                .find(|(k, n, _)| k == kind && n == name)
            {
                return Some(m.clone());
            }
            if std::time::Instant::now() >= deadline {
                return None;
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    #[test]
    fn settings_override_resolves_config_and_auth() {
        // 环境变量优先级高于设置：测试环境若设置了 O2A_CONFIG 则跳过（不保证设置生效）
        if std::env::var_os("O2A_CONFIG").is_some() {
            return;
        }
        let root = std::env::temp_dir().join(format!("o2a_settings_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let settings = root.join("settings.json");
        let state = AppState {
            root: root.clone(),
            python: "python".to_string(),
            engine_binary: None,
            default_stats_dir: root.join("data").join("cache_stats"),
            settings_file: settings.clone(),
            persistent_config: false,
            children: Arc::new(Mutex::new(std::collections::HashMap::new())),
            shared_float: Mutex::new(String::new()),
        };

        // 1) 无设置：默认项目根，auth 跟随
        assert_eq!(crate::config_path(&state), root.join("config.json"));
        assert_eq!(crate::auth_path(&state), root.join("auth.json"));

        // 2) 设置指向目录（已存在）：取目录下 config.json，auth 跟随同目录
        let cfg_dir = root.join("cfg");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::write(
            &settings,
            serde_json::to_string(&serde_json::json!({"config": cfg_dir})).unwrap(),
        )
        .unwrap();
        assert_eq!(crate::config_path(&state), cfg_dir.join("config.json"));
        assert_eq!(crate::auth_path(&state), cfg_dir.join("auth.json"));

        // 3) 设置指向具体文件：config 用该文件，auth 跟随其父目录
        let cfg_file = root.join("conf").join("my-config.json");
        std::fs::create_dir_all(cfg_file.parent().unwrap()).unwrap();
        std::fs::write(
            &settings,
            serde_json::to_string(&serde_json::json!({"config": cfg_file})).unwrap(),
        )
        .unwrap();
        assert_eq!(crate::config_path(&state), cfg_file);
        assert_eq!(
            crate::auth_path(&state),
            root.join("conf").join("auth.json")
        );

        // 4) 尾部分隔符视为目录
        assert_eq!(
            crate::to_file_if_dir(&cfg_dir.join(""), "config.json"),
            cfg_dir.join("config.json")
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn start_service_failure_notified_via_event() {
        // start_service 即返回成功；启动失败由监视线程推 proxy-start-failed 事件。
        // 引擎二进制 + 缺 API Key 配置：select_services 过滤后无可用服务 → 启动即退出
        // （确定性失败，避免 Windows 端口复用语义差异）
        let root = std::env::temp_dir().join(format!("o2a_proxy_svc_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        ensure_engine_binary(&root);
        ensure_recorder();

        let cfg = serde_json::json!({
            "cache_stats_enabled": false,
            "services": [{
                "comment": "svc-fail-exit",
                "mode": "claude",
                "model": "m",
                "listen_address": 18999,
                "openai_base_url": "http://127.0.0.1:1/v1",
                "openai_api_key": ""
            }]
        });
        std::fs::write(root.join("config.json"), serde_json::to_string(&cfg).unwrap()).unwrap();

        let state = AppState {
            root: root.clone(),
            python: "python".to_string(),
            engine_binary: Some(root.join("o2a-engine")),
            default_stats_dir: root.join("data").join("cache_stats"),
            settings_file: root.join("settings.json"),
            persistent_config: false,
            children: Arc::new(Mutex::new(std::collections::HashMap::new())),
            shared_float: Mutex::new(String::new()),
        };
        let res = start_service(&state, "svc-fail-exit");
        assert!(res.is_ok(), "start_service 应即返回成功，失败走异步事件；实际: {res:?}");

        // 等待失败事件（引擎缺 key 立即退出 → 监视线程轮询到退出后推送）
        let msg = wait_event("proxy-start-failed", "svc-fail-exit", Duration::from_secs(15))
            .unwrap_or_else(|| panic!("未收到启动失败事件"));
        assert!(
            msg.contains("启动失败") || msg.contains("立即退出"),
            "失败信息应包含原因，实际: {msg}"
        );
        assert!(
            state.children.lock().unwrap().is_empty(),
            "失败后子进程应从管理表移除"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn start_service_ready_then_self_exit_notifies() {
        // 成功路径：引擎带有效 key 正常绑定端口（stdout 打印就绪标记）→
        // start_service 即返回且子进程存活；外部杀掉进程（模拟自行退出）→
        // 监视线程推 proxy-stopped 事件并从管理表移除。
        let root = std::env::temp_dir().join(format!("o2a_proxy_ready_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        ensure_engine_binary(&root);
        ensure_recorder();

        let cfg = serde_json::json!({
            "cache_stats_enabled": false,
            "services": [{
                "comment": "svc-ready-exit",
                "mode": "claude",
                "model": "m",
                "listen_address": 18998,
                "openai_base_url": "http://127.0.0.1:1/v1",
                "openai_api_key": "sk-test"
            }]
        });
        std::fs::write(root.join("config.json"), serde_json::to_string(&cfg).unwrap()).unwrap();

        let state = AppState {
            root: root.clone(),
            python: "python".to_string(),
            engine_binary: Some(root.join("o2a-engine")),
            default_stats_dir: root.join("data").join("cache_stats"),
            settings_file: root.join("settings.json"),
            persistent_config: false,
            children: Arc::new(Mutex::new(std::collections::HashMap::new())),
            shared_float: Mutex::new(String::new()),
        };
        let res = start_service(&state, "svc-ready-exit");
        assert!(res.is_ok(), "start_service 应即返回成功；实际: {res:?}");
        assert!(
            state.children.lock().unwrap().contains_key("svc-ready-exit"),
            "即返回后子进程应已登记"
        );

        // 等引擎绑定端口（就绪）
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        let mut bound = false;
        while std::time::Instant::now() < deadline {
            if crate::port_open("127.0.0.1", 18998) {
                bound = true;
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }
        assert!(bound, "引擎应在 15s 内绑定端口 18998");

        // 外部杀掉引擎（非 stop_service 路径，模拟自行退出/崩溃）
        let pid = state
            .children
            .lock()
            .unwrap()
            .get("svc-ready-exit")
            .map(|c| c.id())
            .expect("子进程应在管理表中");
        let mut kill = Command::new(if cfg!(windows) { "taskkill" } else { "kill" });
        if cfg!(windows) {
            kill.arg("/F").arg("/PID").arg(pid.to_string());
        } else {
            kill.arg("-9").arg(pid.to_string());
        }
        let _ = kill.output();

        // 监视线程应推 proxy-stopped 事件并移除管理表项
        assert!(
            wait_event("proxy-stopped", "svc-ready-exit", Duration::from_secs(15)).is_some(),
            "未收到 proxy-stopped 事件"
        );
        assert!(
            state.children.lock().unwrap().is_empty(),
            "退出后子进程应从管理表移除"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn toggle_stops_running_child() {
        let root = std::env::temp_dir().join(format!("o2a_proxy_toggle_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let cfg = serde_json::json!({
            "cache_stats_enabled": false,
            "services": [{
                "comment": "svc1",
                "mode": "claude",
                "model": "m",
                "listen_address": 18999,
                "openai_base_url": "http://127.0.0.1:1/v1",
                "openai_api_key": "sk-test"
            }]
        });
        std::fs::write(root.join("config.json"), serde_json::to_string(&cfg).unwrap()).unwrap();
        let state = AppState {
            root: root.clone(),
            python: "python".to_string(),
            engine_binary: None,
            default_stats_dir: root.join("data").join("cache_stats"),
            settings_file: root.join("settings.json"),
            persistent_config: false,
            children: Arc::new(Mutex::new(std::collections::HashMap::new())),
            shared_float: Mutex::new(String::new()),
        };
        // 模拟一个运行中的子进程
        let child = Command::new("python")
            .arg("-c")
            .arg("import time; time.sleep(30)")
            .spawn()
            .unwrap();
        state.children.lock().unwrap().insert("svc1".to_string(), child);

        let res = toggle_service(&state, "svc1");
        assert!(res.is_ok(), "停止运行中的子进程应成功，实际: {res:?}");
        assert!(
            state.children.lock().unwrap().is_empty(),
            "停止后子进程应从管理表移除"
        );

        let _ = std::fs::remove_dir_all(&root);
    }
}
