mod proxy;
mod stats;

use std::collections::HashMap;
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use tauri::menu::{Menu, MenuItem, Submenu};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{Emitter, Manager, RunEvent, State, WebviewUrl, WebviewWindowBuilder, WindowEvent};

#[cfg(target_os = "windows")]
mod ffi {
    #[repr(C)]
    pub struct POINT {
        pub x: i32,
        pub y: i32,
    }

    #[link(name = "user32")]
    extern "system" {
        pub fn GetCursorPos(lpPoint: *mut POINT) -> i32;
    }
}

pub struct AppState {
    pub root: PathBuf,
    pub python: String,
    pub default_stats_dir: PathBuf,
    pub children: Mutex<HashMap<String, std::process::Child>>,
    /// 共享悬浮窗当前显示的服务（空串 = 全部视图）。
    /// 用于区分"切到不同服务"（保持打开只换内容）与"切到同一服务"（开关）。
    pub shared_float: Mutex<String>,
}

fn find_root() -> PathBuf {
    if let Ok(p) = std::env::var("O2A_ROOT") {
        if Path::new(&p).join("proxy.py").exists() {
            return PathBuf::from(p);
        }
    }
    let mut dir = std::env::current_dir().unwrap_or_default();
    for _ in 0..6 {
        if dir.join("proxy.py").exists() {
            return dir;
        }
        if !dir.pop() {
            break;
        }
    }
    std::env::current_dir().unwrap_or_default()
}

fn find_python() -> String {
    if let Ok(p) = std::env::var("O2A_PYTHON") {
        if Path::new(&p).exists() {
            return p;
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        let shims = Path::new(&home).join(".pyenv").join("shims").join("python3");
        if shims.exists() {
            return shims.to_string_lossy().to_string();
        }
    }
    for c in [
        "/opt/homebrew/bin/python3",
        "/usr/local/bin/python3",
        "/usr/bin/python3",
    ] {
        if Path::new(c).exists() {
            return c.to_string();
        }
    }
    if cfg!(windows) {
        "python".to_string()
    } else {
        "python3".to_string()
    }
}

fn config_path(state: &AppState) -> PathBuf {
    state.root.join("config.json")
}

fn auth_path(state: &AppState) -> PathBuf {
    state.root.join("auth.json")
}

fn read_auth(state: &AppState) -> serde_json::Value {
    let p = auth_path(state);
    std::fs::read_to_string(p)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| serde_json::json!({}))
}

/// 从 auth.json 按账号 id → name 顺序取 key（值支持 {type, key} 或纯字符串）。
fn auth_key_for(auth: &serde_json::Value, id: &str, name: &str) -> Option<String> {
    let obj = auth.as_object()?;
    for k in [id, name] {
        if k.is_empty() {
            continue;
        }
        if let Some(v) = obj.get(k) {
            let key = if v.is_string() {
                v.as_str().map(String::from)
            } else {
                v.get("key").and_then(|x| x.as_str()).map(String::from)
            };
            if let Some(k2) = key {
                if !k2.is_empty() {
                    return Some(k2);
                }
            }
        }
    }
    None
}

/// 统计目录解析：配置里显式指定（相对路径基于项目根）则用之，否则用系统用户数据目录。
fn stats_dir(state: &AppState) -> PathBuf {
    match read_config_value(state)
        .ok()
        .and_then(|c| c.get("cache_stats_dir").and_then(|v| v.as_str()).map(String::from))
    {
        Some(dir) if !dir.trim().is_empty() => {
            let p = PathBuf::from(dir.trim());
            if p.is_absolute() {
                p
            } else {
                state.root.join(p)
            }
        }
        _ => state.default_stats_dir.clone(),
    }
}

fn primary_service(state: &AppState) -> String {
    read_config_value(state)
        .ok()
        .and_then(|c| c.get("services").cloned())
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .find(|s| {
            let mode = s.get("mode").and_then(|m| m.as_str()).unwrap_or("claude");
            mode == "claude" || mode == "codex" || mode == "direct"
        })
        .and_then(|s| s.get("comment").and_then(|c| c.as_str()).map(String::from))
        .unwrap_or_default()
}

fn read_config_value(state: &AppState) -> Result<serde_json::Value, String> {
    let p = config_path(state);
    let mut cfg = if !p.exists() {
        serde_json::json!({})
    } else {
        let s = std::fs::read_to_string(&p).map_err(|e| e.to_string())?;
        serde_json::from_str(&s).map_err(|e| e.to_string())?
    };
    // 合并 auth.json 的 Key（供界面显示）；config.json 本身不含 Key
    let auth = read_auth(state);
    if let Some(accounts) = cfg.get_mut("accounts").and_then(|a| a.as_array_mut()) {
        for acc in accounts {
            let id = acc
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let name = acc
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if let Some(key) = auth_key_for(&auth, &id, &name) {
                if let Some(o) = acc.as_object_mut() {
                    o.insert("api_key".to_string(), serde_json::json!(key));
                }
            }
        }
    }
    Ok(cfg)
}

fn open_path(p: &Path) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("cmd")
            .arg("/C")
            .arg("start")
            .arg("")
            .arg(p)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(p)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open")
            .arg(p)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub(crate) fn port_open(host: &str, port: u16) -> bool {
    let addr = (host, port).to_socket_addrs().ok().and_then(|mut it| it.next());
    let Some(addr) = addr else {
        return false;
    };
    TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_ok()
}

/// 探测某服务端点的 /status，返回任务状态（active 等）。失败返回 None。
fn fetch_task_status(host: &str, port: u16) -> Option<serde_json::Value> {
    let url = format!("http://{host}:{port}/status");
    let resp = ureq::get(&url).timeout(Duration::from_millis(800)).call().ok()?;
    let body = resp.into_string().ok()?;
    serde_json::from_str(&body).ok()
}

/// 把面板定位到托盘图标附近（macOS 用托盘坐标，Windows 用鼠标位置 + 工作区）。
#[allow(unused_variables)]
fn position_panel(app: &tauri::AppHandle, win: &tauri::WebviewWindow) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        if let Some(tray) = app.tray_by_id("main") {
            if let Ok(Some(rect)) = tray.rect() {
                let size = win.outer_size().map_err(|e| e.to_string())?;
                let pos = match rect.position {
                    tauri::Position::Physical(p) => (p.x as f64, p.y as f64),
                    tauri::Position::Logical(l) => {
                        let scale = win.scale_factor().map_err(|e| e.to_string())?;
                        (l.x * scale, l.y * scale)
                    }
                };
                win.set_position(tauri::Position::Physical(tauri::PhysicalPosition {
                    x: (pos.0 - size.width as f64 / 2.0) as i32,
                    y: (pos.1 + 12.0) as i32,
                }))
                .map_err(|e| e.to_string())?;
                return Ok(());
            }
        }
    }
    #[cfg(target_os = "windows")]
    {
        let mut pt = ffi::POINT { x: 0, y: 0 };
        unsafe {
            ffi::GetCursorPos(&mut pt);
        }
        let wsize = win.outer_size().map_err(|e| e.to_string())?;
        if let Some(m) = win.current_monitor().map_err(|e| e.to_string())? {
            let wa = m.work_area();
            let wa_right = wa.position.x + wa.size.width as i32;
            let wa_bottom = wa.position.y + wa.size.height as i32;
            let x = (pt.x as i32 - wsize.width as i32 + 18)
                .clamp(wa.position.x, wa_right - wsize.width as i32);
            let y = (wa_bottom - wsize.height as i32 - 10).max(wa.position.y);
            win.set_position(tauri::Position::Physical(tauri::PhysicalPosition { x, y }))
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

#[tauri::command]
fn resolve_root(state: State<'_, AppState>) -> String {
    state.root.to_string_lossy().to_string()
}

#[tauri::command]
fn resolve_python(state: State<'_, AppState>) -> String {
    state.python.clone()
}

#[tauri::command]
fn read_config(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    read_config_value(&state)
}

/// 将 cfg 中 accounts[].api_key 抽取到 auth 映射（按账号 id），并从 cfg 移除。
/// key 为空时同步删除 auth 中对应条目（含 name 键），保证清空后不残留旧 Key。
fn split_account_keys(
    cfg: &mut serde_json::Value,
    auth: &mut serde_json::Map<String, serde_json::Value>,
) {
    if let Some(accounts) = cfg.get_mut("accounts").and_then(|a| a.as_array_mut()) {
        for acc in accounts.iter_mut() {
            let id = acc
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let key = acc
                .get("api_key")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            // 从 config 移除 api_key（新配置不存 Key）
            if let Some(o) = acc.as_object_mut() {
                o.remove("api_key");
            }
            if id.is_empty() {
                continue;
            }
            if key.trim().is_empty() {
                // 清空 Key：同步移除 auth.json 对应条目（按 id 与 name）
                auth.remove(&id);
                if let Some(name) = acc.get("name").and_then(|v| v.as_str()) {
                    if !name.is_empty() {
                        auth.remove(name);
                    }
                }
            } else {
                auth.insert(
                    id.clone(),
                    serde_json::json!({ "type": "api_key", "key": key }),
                );
            }
        }
    }
}

#[tauri::command]
fn save_config(state: State<'_, AppState>, mut cfg: serde_json::Value) -> Result<(), String> {
    // Key 分流：accounts[].api_key → auth.json；config.json 保持不含 Key（新版本默认）
    let mut auth_obj = read_auth(&state)
        .as_object()
        .cloned()
        .unwrap_or_default();
    split_account_keys(&mut cfg, &mut auth_obj);
    let s = serde_json::to_string_pretty(&cfg).map_err(|e| e.to_string())?;
    std::fs::write(config_path(&state), s).map_err(|e| e.to_string())?;
    let auth_json = serde_json::Value::Object(auth_obj);
    let s2 = serde_json::to_string_pretty(&auth_json).map_err(|e| e.to_string())?;
    std::fs::write(auth_path(&state), s2).map_err(|e| e.to_string())
}

#[tauri::command]
async fn get_status(app: tauri::AppHandle) -> Result<serde_json::Value, String> {
    // 异步命令跑在 Tauri 线程池；内部含端口探测（200ms/服务超时），
    // 用 spawn_blocking 挪到阻塞线程池，避免占住异步运行时线程。
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        get_status_impl(&state)
    })
    .await
    .map_err(|e| e.to_string())?
}

fn get_status_impl(state: &AppState) -> Result<serde_json::Value, String> {
    let cfg = read_config_value(state)?;
    let services = cfg.get("services").cloned().unwrap_or_else(|| serde_json::json!([]));
    let mut children = state.children.lock().unwrap();
    let mut out = Vec::new();
    if let Some(arr) = services.as_array() {
        for s in arr {
            let mode = s.get("mode").and_then(|m| m.as_str()).unwrap_or("claude");
            if mode != "claude" && mode != "codex" && mode != "direct" && mode != "auto" {
                continue;
            }
            let name = s
                .get("comment")
                .and_then(|c| c.as_str())
                .unwrap_or("")
                .to_string();
            let port: u16 = s
                .get("listen_address")
                .and_then(|v| v.as_u64().map(|n| n as u16))
                .or_else(|| {
                    s.get("listen_address")
                        .and_then(|v| v.as_str())
                        .and_then(|x| x.parse().ok())
                })
                .unwrap_or(0);
            let host = s
                .get("listen_host")
                .and_then(|v| v.as_str())
                .unwrap_or("127.0.0.1")
                .to_string();
            let child_alive = children
                .get_mut(&name)
                .map(|c| c.try_wait().ok().flatten().is_none())
                .unwrap_or(false);
            let running = child_alive || (port > 0 && port_open(&host, port));
            let mut svc = serde_json::json!({
                "name": name,
                "running": running,
                "port": port,
                "host": host,
                "mode": mode,
                "model": s.get("model").cloned().unwrap_or(serde_json::Value::Null),
                "context_1m": s.get("context_1m").cloned().unwrap_or(serde_json::Value::Bool(false)),
            });
            // 服务在跑时探 /status 拿实时任务状态（active/last_finish 等）
            if running && port > 0 {
                if let Some(task) = fetch_task_status(&host, port) {
                    svc["task"] = task;
                }
            }
            out.push(svc);
        }
    }
    Ok(serde_json::json!({
        "root": state.root.to_string_lossy().to_string(),
        "python": state.python,
        "statsDir": stats_dir(state).to_string_lossy().to_string(),
        "services": out,
    }))
}

#[tauri::command]
async fn start_service(app: tauri::AppHandle, name: String) -> Result<(), String> {
    // start_service 内部 sleep(1.2s) 验证子进程存活，属阻塞操作，
    // spawn_blocking 挪到阻塞线程池，避免拖住主线程/异步运行时。
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        proxy::start_service(&state, &name)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn stop_service(app: tauri::AppHandle, name: String) -> Result<(), String> {
    // child.wait() 等待子进程退出，同样放阻塞线程池
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        proxy::stop_service(&state, &name)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn toggle_service(app: tauri::AppHandle, name: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        proxy::toggle_service(&state, &name)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn start_all(app: tauri::AppHandle) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        proxy::start_all(&state)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn stop_all(app: tauri::AppHandle) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        proxy::stop_all(&state)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn get_stats(app: tauri::AppHandle, service: String) -> Result<serde_json::Value, String> {
    // 全量读 jsonl + 按月重算费用较重，异步执行 + stats.rs 内部 TTL 缓存
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        stats::get_stats(&stats_dir(&state), &service, &primary_service(&state))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn get_live(app: tauri::AppHandle, service: String) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        stats::get_live(&stats_dir(&state), &service, &primary_service(&state))
    })
    .await
    .map_err(|e| e.to_string())?
}

fn models_url(base: &str) -> Option<String> {
    // 与旧版 Electron 客户端一致：去掉 /chat/completions 后请求 /models
    let mut u = base.trim().trim_end_matches('/').to_string();
    if u.is_empty() {
        return None;
    }
    if let Some(stripped) = u.strip_suffix("/chat/completions") {
        u = stripped.to_string();
    }
    Some(format!("{u}/models"))
}

fn fetch_models_impl(base_url: &str, api_key: &str) -> Result<serde_json::Value, String> {
    let Some(url) = models_url(base_url) else {
        return Ok(serde_json::json!({"ok": false, "error": "API 地址为空"}));
    };
    let mut req = ureq::get(&url).timeout(Duration::from_secs(8));
    let key = api_key.trim();
    if !key.is_empty() {
        req = req.set("Authorization", &format!("Bearer {key}"));
    }
    match req.call() {
        Ok(resp) => {
            let s = resp.into_string().map_err(|e| e.to_string())?;
            let j: serde_json::Value = serde_json::from_str(&s).map_err(|e| e.to_string())?;
            let arr = if j.is_array() {
                j.as_array().cloned().unwrap_or_default()
            } else if let Some(d) = j.get("data").and_then(|v| v.as_array()) {
                d.clone()
            } else if let Some(m) = j.get("models").and_then(|v| v.as_array()) {
                m.clone()
            } else {
                Vec::new()
            };
            let mut seen = std::collections::HashSet::new();
            let mut ids: Vec<String> = Vec::new();
            for m in arr {
                let id = if m.is_string() {
                    m.as_str().map(String::from)
                } else {
                    m.get("id")
                        .and_then(|v| v.as_str())
                        .map(String::from)
                        .or_else(|| m.get("name").and_then(|v| v.as_str()).map(String::from))
                };
                if let Some(id) = id {
                    let id = id.trim().to_string();
                    if !id.is_empty() && seen.insert(id.clone()) {
                        ids.push(id);
                    }
                }
            }
            ids.sort();
            if ids.is_empty() {
                return Ok(serde_json::json!({
                    "ok": false,
                    "error": "接口返回了空模型列表",
                    "url": url
                }));
            }
            Ok(serde_json::json!({"ok": true, "models": ids, "url": url}))
        }
        Err(ureq::Error::Status(code, _)) => {
            Ok(serde_json::json!({"ok": false, "error": format!("HTTP {code}"), "url": url}))
        }
        Err(e) => Ok(serde_json::json!({"ok": false, "error": e.to_string(), "url": url})),
    }
}

#[tauri::command]
async fn fetch_models(base_url: String, api_key: String) -> Result<serde_json::Value, String> {
    // ureq 同步 HTTP（8s 超时）放阻塞线程池，端点不可达时不再冻结 UI
    tauri::async_runtime::spawn_blocking(move || fetch_models_impl(&base_url, &api_key))
        .await
        .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    #[test]
    fn split_account_keys_moves_and_keeps() {
        let mut cfg = serde_json::json!({
            "accounts": [
                {"id": "acc-1", "name": "A", "api_key": "sk-1", "openai_url": "x"},
                {"id": "acc-2", "name": "B", "api_key": ""}
            ]
        });
        let mut auth = serde_json::Map::new();
        auth.insert(
            "acc-0".to_string(),
            serde_json::json!({ "type": "api_key", "key": "old" }),
        );
        split_account_keys(&mut cfg, &mut auth);
        // config 不再含 api_key
        assert!(cfg["accounts"][0].get("api_key").is_none());
        assert!(cfg["accounts"][1].get("api_key").is_none());
        // 有 key 的账号写入 auth（按 id）
        assert_eq!(auth["acc-1"]["key"], "sk-1");
        assert_eq!(auth["acc-1"]["type"], "api_key");
        // 空 key 不产生条目
        assert!(auth.get("acc-2").is_none());
        // 已有条目保留
        assert_eq!(auth["acc-0"]["key"], "old");
    }

    #[test]
    fn split_account_keys_clears_removed_key() {
        let mut cfg = serde_json::json!({
            "accounts": [{"id": "acc-1", "name": "A", "api_key": ""}]
        });
        let mut auth = serde_json::Map::new();
        auth.insert(
            "acc-1".to_string(),
            serde_json::json!({ "type": "api_key", "key": "sk-old" }),
        );
        auth.insert(
            "A".to_string(),
            serde_json::json!({ "type": "api_key", "key": "sk-old-name" }),
        );
        split_account_keys(&mut cfg, &mut auth);
        // 清空 key 后，auth 中 id 与 name 键一并移除
        assert!(auth.get("acc-1").is_none());
        assert!(auth.get("A").is_none());
    }

    #[test]
    fn models_url_derivation() {
        assert_eq!(
            models_url("https://dashscope.aliyuncs.com/compatible-mode/v1/chat/completions").unwrap(),
            "https://dashscope.aliyuncs.com/compatible-mode/v1/models"
        );
        assert_eq!(
            models_url("https://api.deepseek.com/v1/chat/completions/").unwrap(),
            "https://api.deepseek.com/v1/models"
        );
        assert_eq!(
            models_url("https://api.example.com/v1/").unwrap(),
            "https://api.example.com/v1/models"
        );
        assert!(models_url("   ").is_none());
    }

    #[test]
    fn fetch_models_parses_list() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                // 先读掉请求头，避免未读数据导致 Windows 关闭时发 RST
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let body = "{\"data\":[{\"id\":\"b-model\"},{\"id\":\"a-model\"},{\"id\":\"a-model\"}]}";
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(resp.as_bytes());
            }
        });
        let res = fetch_models_impl(
            &format!("http://127.0.0.1:{port}/v1/chat/completions"),
            "sk-test",
        )
        .unwrap();
        assert_eq!(res["ok"], true, "res = {res}");
        assert_eq!(res["models"], serde_json::json!(["a-model", "b-model"]));
    }
}

#[tauri::command]
fn open_config_file(state: State<'_, AppState>) -> Result<(), String> {
    let p = config_path(&state);
    if !p.exists() {
        std::fs::write(&p, "{}\n").map_err(|e| e.to_string())?;
    }
    open_path(&p)
}

/// 通知前端面板可见性：隐藏时前端暂停轮询，避免空转与卡顿。
fn emit_panel_visible(app: &tauri::AppHandle, visible: bool) {
    let _ = app.emit("panel-visible", visible);
}

/// 通知前端悬浮窗可见性（按窗口 label）：隐藏时暂停轮询。
fn emit_float_visible(app: &tauri::AppHandle, label: &str, visible: bool) {
    let _ = app.emit(
        "float-visible",
        serde_json::json!({ "label": label, "visible": visible }),
    );
}

#[tauri::command]
fn toggle_panel(app: tauri::AppHandle) -> Result<bool, String> {
    if let Some(w) = app.get_webview_window("panel") {
        let visible = w.is_visible().map_err(|e| e.to_string())?;
        if visible {
            w.hide().map_err(|e| e.to_string())?;
        } else {
            position_panel(&app, &w)?;
            w.show().map_err(|e| e.to_string())?;
            w.set_focus().map_err(|e| e.to_string())?;
        }
        emit_panel_visible(&app, !visible);
        Ok(!visible)
    } else {
        Err("panel window missing".into())
    }
}

#[tauri::command]
fn hide_panel(app: tauri::AppHandle) -> Result<(), String> {
    if let Some(w) = app.get_webview_window("panel") {
        w.hide().map_err(|e| e.to_string())?;
    }
    emit_panel_visible(&app, false);
    Ok(())
}

/// 全平台统一：单共享悬浮窗。
/// 切到不同服务 → 保持打开、只换内容（float-switch 事件驱动前端切换）；
/// 切到同一服务 → 开关（toggle）语义。透明 WebView2 无法运行时创建
/// （会卡死事件循环，已验证），只能在 setup 预创建 1 个共享窗口；
/// 统一后 macOS/Linux 也走同一套单窗逻辑。
fn toggle_float_shared(app: &tauri::AppHandle, service: &str) -> Result<bool, String> {
    let label = "float";
    let Some(w) = app.get_webview_window(label) else {
        // 兜底：正常不会走到（setup 已预创建共享悬浮窗）
        let w = create_float_window(app, label, service).map_err(|e| e.to_string())?;
        w.show().map_err(|e| e.to_string())?;
        w.set_focus().map_err(|e| e.to_string())?;
        emit_float_visible(app, label, true);
        return Ok(true);
    };
    let state = app.state::<AppState>();
    let mut cur = state.shared_float.lock().unwrap();
    let is_visible = w.is_visible().map_err(|e| e.to_string())?;
    if service == cur.as_str() {
        // 同一服务：开关
        if is_visible {
            w.hide().map_err(|e| e.to_string())?;
            *cur = String::new(); // 关闭后重置为默认（全部视图），重开不再停留旧服务
            emit_float_visible(app, label, false);
            return Ok(false);
        }
        w.show().map_err(|e| e.to_string())?;
        w.set_focus().map_err(|e| e.to_string())?;
        emit_float_visible(app, label, true);
        return Ok(true);
    }
    // 不同服务：切换显示，保持打开（窗口内直接切换，不先关再开）
    *cur = service.to_string();
    drop(cur);
    let _ = app.emit(
        "float-switch",
        serde_json::json!({ "label": label, "service": service }),
    );
    if !is_visible {
        w.show().map_err(|e| e.to_string())?;
    }
    w.set_focus().map_err(|e| e.to_string())?;
    emit_float_visible(app, label, true);
    Ok(true)
}

// macOS/Linux 旧实现（每服务独立窗口）已移除：需求统一为单共享悬浮窗，
// 全平台都走 toggle_float_shared。
#[tauri::command]
fn toggle_float(app: tauri::AppHandle) -> Result<bool, String> {
    // 全部视图（托盘入口）
    toggle_float_shared(&app, "")
}

// 创建悬浮窗（全平台统一预创建 1 个共享窗口，label 固定为 "float"）。
// 必须在 setup 阶段（事件循环启动前）创建：运行时动态创建透明 WebView2
// 窗口，其 wait_with_pump 消息泵会与主事件循环的 GetMessage 竞争，导致
// 初始化回调丢失 —— 表现为窗口空白、应用事件循环卡死（后续命令与退出均
// 失效，已实测验证）。macOS/Linux 的 WKWebView/WebKitGTK 虽无此问题，
// 但统一逻辑后也一并预创建，保持行为一致。
// 创建时 URL 带 service（初始显示的服务）与 label（事件过滤）。
fn create_float_window(
    app: &tauri::AppHandle,
    label: &str,
    service: &str,
) -> Result<tauri::WebviewWindow, tauri::Error> {
    let url = if service.is_empty() {
        format!("index.html#/float?label={}", urlenc(label))
    } else {
        format!(
            "index.html#/float?service={}&label={}",
            urlenc(service),
            urlenc(label)
        )
    };
    let w = WebviewWindowBuilder::new(app, label, WebviewUrl::App(url.into()))
        .title("o2a-proxy 悬浮看板")
        .inner_size(434.0, 234.0)
        .min_inner_size(300.0, 170.0)
        .resizable(true)
        .decorations(false)
        .transparent(true)
        .always_on_top(true)
        .skip_taskbar(true)
        .visible(false)
        // macOS 默认 acceptsFirstMouse=false：首次点击只会激活窗口而不会传递，
        // 导致必须先点一次聚焦才能拖动。设为 true 后首次点击即可直接拖动。
        .accept_first_mouse(true)
        .build()?;
    w.on_window_event(hide_on_close(app.clone(), label.to_string()));
    Ok(w)
}

fn urlenc(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}


#[tauri::command]
fn toggle_float_for(app: tauri::AppHandle, service: String) -> Result<bool, String> {
    // 面板"悬浮"按钮：当前选中服务（空串=全部）
    toggle_float_shared(&app, &service)
}

#[tauri::command]
fn quit_app(app: tauri::AppHandle) {
    app.exit(0);
}

// ---------------------------------------------------------------------------
// App entry
// ---------------------------------------------------------------------------

fn hide_on_close(app: tauri::AppHandle, label: String) -> impl Fn(&WindowEvent) + Send + 'static {
    move |event| {
        if let WindowEvent::CloseRequested { api, .. } = event {
            if let Some(w) = app.get_webview_window(label.as_str()) {
                let _ = w.hide();
            }
            emit_float_visible(&app, &label, false);
            api.prevent_close();
        }
    }
}

fn panel_events(app: tauri::AppHandle) -> impl Fn(&WindowEvent) + Send + 'static {
    move |event| match event {
        WindowEvent::CloseRequested { api, .. } => {
            if let Some(w) = app.get_webview_window("panel") {
                let _ = w.hide();
            }
            emit_panel_visible(&app, false);
            api.prevent_close();
        }
        WindowEvent::Focused(false) => {
            // 类 popover 行为：面板失焦即收起
            if let Some(w) = app.get_webview_window("panel") {
                let _ = w.hide();
            }
            emit_panel_visible(&app, false);
        }
        _ => {}
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            resolve_root,
            resolve_python,
            read_config,
            save_config,
            get_status,
            start_service,
            stop_service,
            toggle_service,
            start_all,
            stop_all,
            get_stats,
            get_live,
            fetch_models,
            open_config_file,
            toggle_panel,
            hide_panel,
            toggle_float,
            toggle_float_for,
            quit_app,
        ])
        .setup(|app| {
            // macOS：纯菜单栏应用（无 Dock 图标），与原 Electron 行为一致
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let root = find_root();
            let state = AppState {
                root: root.clone(),
                python: find_python(),
                // 默认统计目录：应用（项目根）下的相对目录
                default_stats_dir: root.join("cache_stats"),
                children: Mutex::new(HashMap::new()),
                shared_float: Mutex::new(String::new()),
            };
            app.manage(state);

            // 托盘菜单：先于任何窗口创建，托盘图标立即出现。
            // Windows 上 WebView2 窗口初始化耗时 0.5~2s/个（面板 + 悬浮窗共
            // 6 个），若等全部建完再创建托盘，图标会延迟很久才显示。
            let panel_i = MenuItem::with_id(app, "panel", "打开面板", true, None::<&str>)?;
            let float_i = MenuItem::with_id(app, "float", "悬浮看板", true, None::<&str>)?;
            let start_all_i = MenuItem::with_id(app, "start_all", "启动全部代理", true, None::<&str>)?;
            let stop_all_i = MenuItem::with_id(app, "stop_all", "停止全部代理", true, None::<&str>)?;
            let open_cfg_i = MenuItem::with_id(app, "open_cfg", "打开 config.json", true, None::<&str>)?;
            let quit_i = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
            let proxy_menu = Submenu::with_items(app, "代理", true, &[&start_all_i, &stop_all_i])?;
            let menu = Menu::with_items(app, &[&panel_i, &float_i, &proxy_menu, &open_cfg_i, &quit_i])?;

            TrayIconBuilder::with_id("main")
                // 用与应用图标同款的托盘图标（22x22/@2x），避免默认图标在菜单栏渲染过小失真
                .icon(tauri::include_image!("icons/tray-icon.png"))
                .icon_as_template(false)
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "panel" => {
                        let _ = toggle_panel(app.clone());
                    }
                    "float" => {
                        let _ = toggle_float(app.clone());
                    }
                    "start_all" => {
                        // 每服务串行等待约 1.2s 验证存活，异步执行避免卡住托盘 UI
                        let app = app.clone();
                        tauri::async_runtime::spawn(async move {
                            let _ = start_all(app).await;
                        });
                    }
                    "stop_all" => {
                        let app = app.clone();
                        tauri::async_runtime::spawn(async move {
                            let _ = stop_all(app).await;
                        });
                    }
                    "open_cfg" => {
                        let state = app.state::<AppState>();
                        let _ = open_config_file(state);
                    }
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        let _ = toggle_panel(tray.app_handle().clone());
                    }
                })
                .build(app)?;

            let panel = WebviewWindowBuilder::new(app, "panel", WebviewUrl::App("index.html".into()))
                .title("o2a-proxy")
                .inner_size(430.0, 740.0)
                .resizable(false)
                .decorations(false)
                .transparent(true)
                .skip_taskbar(true)
                .visible(false)
                .build()?;
            panel.on_window_event(panel_events(app.handle().clone()));
            // 测试/演示用：O2A_SHOW_PANEL=1 时启动即显示面板
            if std::env::var_os("O2A_SHOW_PANEL").is_some() {
                panel.show()?;
                panel.set_focus()?;
            }

            // 全平台统一：预创建 1 个共享悬浮窗（隐藏），显示哪个服务由
            // float-switch 事件切换（见 toggle_float_shared）。必须在此阶段
            // 创建：运行时动态创建透明 WebView2 会卡死事件循环（已实测验证），
            // macOS/Linux 也统一预创建，走同一套单窗逻辑。
            create_float_window(app.handle(), "float", "")?;

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            if let RunEvent::Exit = event {
                let state = app.state::<AppState>();
                let mut children = state.children.lock().unwrap();
                for (_, child) in children.iter_mut() {
                    let _ = child.kill();
                }
            }
        });
}
