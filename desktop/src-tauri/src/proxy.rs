use std::fs::File;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::AppState;

/// 递归复制目录（测试准备临时引擎目录用：root/proxy.py + root/o2a/）。
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
        let comment = s.get("comment").and_then(|c| c.as_str()).unwrap_or("");
        let port = s.get("listen_address").and_then(|p| p.as_str()).unwrap_or("");
        comment == name || port == name
    })
}

fn is_alive(child: &mut std::process::Child) -> bool {
    child.try_wait().ok().flatten().is_none()
}

/// 把子进程一路输出同时写到日志文件（供面板查看）和当前终端（dev/前台运行）。
fn tee_stream<R: Read + Send + 'static>(
    mut reader: R,
    file: Arc<Mutex<File>>,
    mut term: Box<dyn Write + Send>,
) {
    thread::spawn(move || {
        let mut buf = [0u8; 1024];
        loop {
            let n = match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(_) => break,
            };
            let _ = term.write_all(&buf[..n]);
            let _ = term.flush();
            if let Ok(mut f) = file.lock() {
                let _ = f.write_all(&buf[..n]);
                let _ = f.flush();
            }
        }
    });
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
    if find_service(&services, name).is_none() {
        return Err(format!("未找到服务: {name}"));
    }
    let mut children = state.children.lock().unwrap();
    if let Some(child) = children.get_mut(name) {
        if is_alive(child) {
            return Ok(());
        }
        children.remove(name);
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
    let mut cmd = Command::new(&state.python);
    cmd.arg("proxy_async.py")
        .arg("--service")
        .arg(name)
        .current_dir(&state.root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Windows 下禁用子进程控制台窗口（CREATE_NO_WINDOW）：
    // 否则从 GUI 桌面端启动 python 引擎时会弹出一个 cmd 黑窗。
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000);
    }
    // 配置未显式指定统计目录时，把默认目录传给代理，保证两端路径一致
    let has_stats_dir = crate::read_config_value(state)
        .ok()
        .and_then(|c| {
            c.get("cache_stats_dir")
                .and_then(|v| v.as_str())
                .map(|s| !s.trim().is_empty())
        })
        .unwrap_or(false);
    if !has_stats_dir {
        cmd.env("CACHE_STATS_DIR", &state.default_stats_dir);
    }
    // 配置位置显式传给子进程：可能来自环境变量或 UI 保存的 settings.json（子进程读不到后者），
    // 保证子进程与桌面端读写同一份 config.json / auth.json。
    cmd.env("O2A_CONFIG", crate::config_path(state));
    cmd.env("O2A_AUTH", crate::auth_path(state));
    let mut child = cmd.spawn().map_err(|e| format!("启动失败: {e}"))?;
    // 代理日志同时输出到当前终端（pnpm tauri dev / 前台运行）和日志文件（面板查看）
    if let (Some(out), Some(err)) = (child.stdout.take(), child.stderr.take()) {
        tee_stream(out, Arc::clone(&file), Box::new(std::io::stdout()));
        tee_stream(err, Arc::clone(&file), Box::new(std::io::stderr()));
    }
    children.insert(name.to_string(), child);
    // 轮询 try_wait 确认进程没有因端口占用/缺 key 等立刻退出：
    // 快速退出的情况能立刻报错（无需等满窗口）；
    // 验证窗口保持 1.2s，兼容 Windows 上 python 首次拉起较慢的环境。
    let deadline = std::time::Instant::now() + Duration::from_millis(1200);
    loop {
        if let Some(ch) = children.get_mut(name) {
            if let Some(status) = ch.try_wait().map_err(|e| e.to_string())? {
                children.remove(name);
                let tail = read_log_tail(&log, 1500);
                let msg = if tail.is_empty() {
                    String::new()
                } else {
                    format!("，日志：{tail}")
                };
                return Err(format!("代理启动后立即退出（code={status}）{msg}"));
            }
        }
        if std::time::Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(())
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

pub fn start_all(state: &AppState) -> Result<(), String> {
    let services = config_services(state);
    let mut last_err = None;
    for s in &services {
        let name = s
            .get("comment")
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .to_string();
        if !name.is_empty() {
            if let Err(e) = start_service(state, &name) {
                last_err = Some(e);
            }
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
            default_stats_dir: root.join("data").join("cache_stats"),
            settings_file: settings.clone(),
            persistent_config: false,
            children: Mutex::new(std::collections::HashMap::new()),
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
    fn start_service_detects_immediate_exit() {
        // 缺 API Key 时代理启动即退出（确定性失败，避免 Windows 端口复用语义差异）
        let root = std::env::temp_dir().join(format!("o2a_proxy_svc_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..");
        std::fs::copy(repo.join("proxy_async.py"), root.join("proxy_async.py")).unwrap();
        std::fs::copy(repo.join("proxy.py"), root.join("proxy.py")).unwrap();
        copy_dir_recursive(&repo.join("o2a"), &root.join("o2a")).unwrap();

        let cfg = serde_json::json!({
            "cache_stats_enabled": false,
            "services": [{
                "comment": "svc1",
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
            default_stats_dir: root.join("data").join("cache_stats"),
            settings_file: root.join("settings.json"),
            persistent_config: false,
            children: Mutex::new(std::collections::HashMap::new()),
            shared_float: Mutex::new(String::new()),
        };
        let res = start_service(&state, "svc1");
        assert!(res.is_err(), "缺少 API Key 时启动应报错");
        let msg = res.unwrap_err();
        assert!(msg.contains("立即退出"), "错误信息应包含启动失败原因，实际: {msg}");

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
            default_stats_dir: root.join("data").join("cache_stats"),
            settings_file: root.join("settings.json"),
            persistent_config: false,
            children: Mutex::new(std::collections::HashMap::new()),
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
