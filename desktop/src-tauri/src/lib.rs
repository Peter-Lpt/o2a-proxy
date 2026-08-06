mod proxy;
mod stats;

use std::collections::HashMap;
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use tauri::menu::{Menu, MenuItem, Submenu};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{Manager, RunEvent, State, WebviewUrl, WebviewWindowBuilder, WindowEvent};

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
    if !p.exists() {
        return Ok(serde_json::json!({}));
    }
    let s = std::fs::read_to_string(&p).map_err(|e| e.to_string())?;
    serde_json::from_str(&s).map_err(|e| e.to_string())
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

#[tauri::command]
fn save_config(state: State<'_, AppState>, cfg: serde_json::Value) -> Result<(), String> {
    let s = serde_json::to_string_pretty(&cfg).map_err(|e| e.to_string())?;
    std::fs::write(config_path(&state), s).map_err(|e| e.to_string())
}

#[tauri::command]
fn get_status(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let cfg = read_config_value(&state)?;
    let services = cfg.get("services").cloned().unwrap_or_else(|| serde_json::json!([]));
    let mut children = state.children.lock().unwrap();
    let mut out = Vec::new();
    if let Some(arr) = services.as_array() {
        for s in arr {
            let mode = s.get("mode").and_then(|m| m.as_str()).unwrap_or("claude");
            if mode != "claude" && mode != "codex" && mode != "direct" {
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
            out.push(serde_json::json!({
                "name": name,
                "running": running,
                "port": port,
                "host": host,
                "mode": mode,
                "model": s.get("model").cloned().unwrap_or(serde_json::Value::Null),
                "context_1m": s.get("context_1m").cloned().unwrap_or(serde_json::Value::Bool(false)),
            }));
        }
    }
    Ok(serde_json::json!({
        "root": state.root.to_string_lossy().to_string(),
        "python": state.python,
        "statsDir": stats_dir(&state).to_string_lossy().to_string(),
        "services": out,
    }))
}

#[tauri::command]
fn start_service(state: State<'_, AppState>, name: String) -> Result<(), String> {
    proxy::start_service(&state, &name)
}

#[tauri::command]
fn stop_service(state: State<'_, AppState>, name: String) -> Result<(), String> {
    proxy::stop_service(&state, &name)
}

#[tauri::command]
fn toggle_service(state: State<'_, AppState>, name: String) -> Result<(), String> {
    proxy::toggle_service(&state, &name)
}

#[tauri::command]
fn start_all(state: State<'_, AppState>) -> Result<(), String> {
    proxy::start_all(&state)
}

#[tauri::command]
fn stop_all(state: State<'_, AppState>) -> Result<(), String> {
    proxy::stop_all(&state)
}

#[tauri::command]
fn get_stats(state: State<'_, AppState>, service: String) -> Result<serde_json::Value, String> {
    stats::get_stats(&stats_dir(&state), &service, &primary_service(&state))
}

#[tauri::command]
fn get_live(state: State<'_, AppState>, service: String) -> Result<serde_json::Value, String> {
    stats::get_live(&stats_dir(&state), &service, &primary_service(&state))
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
fn fetch_models(base_url: String, api_key: String) -> Result<serde_json::Value, String> {
    fetch_models_impl(&base_url, &api_key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

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
    Ok(())
}

#[tauri::command]
fn toggle_float(app: tauri::AppHandle) -> Result<bool, String> {
    if let Some(w) = app.get_webview_window("float") {
        let visible = w.is_visible().map_err(|e| e.to_string())?;
        if visible {
            w.hide().map_err(|e| e.to_string())?;
        } else {
            w.show().map_err(|e| e.to_string())?;
            w.set_focus().map_err(|e| e.to_string())?;
        }
        Ok(!visible)
    } else {
        Err("float window missing".into())
    }
}

fn float_label(service: &str) -> String {
    if service.is_empty() {
        "float".to_string()
    } else {
        format!("float_{service}")
    }
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
    let label = float_label(&service);
    if let Some(w) = app.get_webview_window(&label) {
        let visible = w.is_visible().map_err(|e| e.to_string())?;
        if visible {
            w.hide().map_err(|e| e.to_string())?;
        } else {
            w.show().map_err(|e| e.to_string())?;
            w.set_focus().map_err(|e| e.to_string())?;
        }
        Ok(!visible)
    } else {
        let url = format!("index.html#/float?service={}", urlenc(&service));
        let w = WebviewWindowBuilder::new(&app, &label, WebviewUrl::App(url.into()))
            .title("o2a-proxy 悬浮看板")
            .inner_size(434.0, 234.0)
            .resizable(false)
            .decorations(false)
            .transparent(true)
            .always_on_top(true)
            .skip_taskbar(true)
            .visible(true)
            .build()
            .map_err(|e| e.to_string())?;
        w.on_window_event(hide_on_close(app.clone(), label));
        Ok(true)
    }
}

#[tauri::command]
fn get_float_state(app: tauri::AppHandle) -> Result<bool, String> {
    Ok(app
        .get_webview_window("float")
        .map(|w| w.is_visible().unwrap_or(false))
        .unwrap_or(false))
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
            api.prevent_close();
        }
        WindowEvent::Focused(false) => {
            // 类 popover 行为：面板失焦即收起
            if let Some(w) = app.get_webview_window("panel") {
                let _ = w.hide();
            }
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
            get_float_state,
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
            };
            app.manage(state);

            let panel = WebviewWindowBuilder::new(app, "panel", WebviewUrl::App("index.html".into()))
                .title("o2a-proxy")
                .inner_size(430.0, 740.0)
                .resizable(false)
                .decorations(false)
                .skip_taskbar(true)
                .visible(false)
                .build()?;
            panel.on_window_event(panel_events(app.handle().clone()));
            // 测试/演示用：O2A_SHOW_PANEL=1 时启动即显示面板
            if std::env::var_os("O2A_SHOW_PANEL").is_some() {
                panel.show()?;
                panel.set_focus()?;
            }

            let float_win = WebviewWindowBuilder::new(
                app,
                "float",
                WebviewUrl::App("index.html#/float".into()),
            )
            .title("o2a-proxy 悬浮看板")
            .inner_size(434.0, 234.0)
            .resizable(false)
            .decorations(false)
            .transparent(true)
            .always_on_top(true)
            .skip_taskbar(true)
            .visible(false)
            .build()?;
            float_win.on_window_event(hide_on_close(app.handle().clone(), "float".to_string()));

            // 托盘菜单
            let panel_i = MenuItem::with_id(app, "panel", "打开面板", true, None::<&str>)?;
            let float_i = MenuItem::with_id(app, "float", "悬浮看板", true, None::<&str>)?;
            let start_all_i = MenuItem::with_id(app, "start_all", "启动全部代理", true, None::<&str>)?;
            let stop_all_i = MenuItem::with_id(app, "stop_all", "停止全部代理", true, None::<&str>)?;
            let open_cfg_i = MenuItem::with_id(app, "open_cfg", "打开 config.json", true, None::<&str>)?;
            let quit_i = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
            let proxy_menu = Submenu::with_items(app, "代理", true, &[&start_all_i, &stop_all_i])?;
            let menu = Menu::with_items(app, &[&panel_i, &float_i, &proxy_menu, &open_cfg_i, &quit_i])?;

            TrayIconBuilder::with_id("main")
                .icon(app.default_window_icon().cloned().ok_or("no default icon")?)
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
                        let state = app.state::<AppState>();
                        let _ = proxy::start_all(&state);
                    }
                    "stop_all" => {
                        let state = app.state::<AppState>();
                        let _ = proxy::stop_all(&state);
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
