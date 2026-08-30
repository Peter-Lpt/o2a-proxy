mod pricing;
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
    /// UI 保存的配置文件位置覆盖（settings.json，位于系统用户配置目录，win/mac 各自的标准位置）
    pub settings_file: PathBuf,
    /// 绿色版（root 来自打包资源目录）：配置文件默认落到持久用户目录，
    /// 避免解压临时目录被清理导致配置丢失
    pub persistent_config: bool,
    pub children: Mutex<HashMap<String, std::process::Child>>,
    /// 共享悬浮窗当前显示的服务（空串 = 全部视图）。
    /// 用于区分"切到不同服务"（保持打开只换内容）与"切到同一服务"（开关）。
    pub shared_float: Mutex<String>,
}

/// 定位引擎根目录（含 proxy.py 的目录）。优先级：
/// 1. O2A_ROOT 环境变量（显式指定，含 proxy.py 才用）
/// 2. 开发模式：从 cwd 向上最多 6 层找含 proxy.py 的目录
/// 3. 打包资源目录（绿色版内嵌引擎）——仅当 cwd 向上找不到时兜底，
///    避免 dev 模式下 resource_dir（target/debug）混入引擎文件时被误判为打包版
fn find_root(app: &tauri::App) -> PathBuf {
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
    if let Ok(dir) = app.path().resource_dir() {
        if dir.join("proxy.py").exists() {
            return dir;
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

/// 把可能是目录的路径归一化为具体文件：已存在目录或以分隔符结尾 → 目录下 filename；否则当作文件。
fn to_file_if_dir(p: &Path, filename: &str) -> PathBuf {
    let s = p.to_string_lossy();
    if p.is_dir() || s.ends_with('/') || s.ends_with('\\') {
        p.join(filename)
    } else {
        p.to_path_buf()
    }
}

fn env_resolved_path_opt(env: &str, filename: &str) -> Option<PathBuf> {
    let raw = std::env::var_os(env)
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())?;
    Some(to_file_if_dir(&raw, filename))
}

/// UI 保存的配置位置（settings.json 的 "config" 字段）。
/// 位置不能存进 config.json 自身（鸡生蛋），故放系统用户配置目录：
/// Windows `%APPDATA%\com.o2aproxy.desktop\settings.json`，
/// macOS `~/Library/Application Support/com.o2aproxy.desktop/settings.json`。
fn settings_path(app: &tauri::App) -> PathBuf {
    app.path()
        .app_config_dir()
        .ok()
        .map(|d| d.join("settings.json"))
        .unwrap_or_else(|| {
            std::env::current_dir()
                .unwrap_or_default()
                .join("o2a-settings.json")
        })
}

fn saved_config_override(settings_file: &Path) -> Option<PathBuf> {
    let raw = std::fs::read_to_string(settings_file).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let s = v.get("config")?.as_str()?.trim();
    if s.is_empty() {
        return None;
    }
    Some(PathBuf::from(s))
}

/// config.json 位置优先级：O2A_CONFIG 环境变量 > UI 保存的设置 > 默认项目根
/// （proxy.py 所在目录，Windows/macOS 一致）。
fn config_path(state: &AppState) -> PathBuf {
    if let Some(p) = env_resolved_path_opt("O2A_CONFIG", "config.json") {
        return p;
    }
    if let Some(p) = saved_config_override(&state.settings_file) {
        return to_file_if_dir(&p, "config.json");
    }
    if state.persistent_config {
        // 绿色版：配置文件放持久用户目录（与 settings.json 同目录），
        // 避免资源解压临时目录被清理导致配置丢失
        return state
            .settings_file
            .parent()
            .map(|d| d.join("config.json"))
            .unwrap_or_else(|| state.root.join("config.json"));
    }
    state.root.join("config.json")
}

/// auth.json 位置：优先 O2A_AUTH 环境变量；未指定时跟随 config.json 所在目录，
/// 保证整套配置一起迁移（与 proxy.py 的解析逻辑一致）。
fn auth_path(state: &AppState) -> PathBuf {
    if let Some(p) = env_resolved_path_opt("O2A_AUTH", "auth.json") {
        return p;
    }
    config_path(state)
        .parent()
        .map(|d| d.join("auth.json"))
        .unwrap_or_else(|| state.root.join("auth.json"))
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

/// 服务 id → 当前显示名（comment）翻译。
/// 统计记录按 service 名匹配；前端以 id 为身份（§2 id 化）后，命令层先把 id
/// 翻译为当前名再进 stats 层；带 service_id 的新记录在 stats.rs 读取层
/// 归一化为当前名，改名后的历史统计不丢。
fn resolve_service_name(state: &AppState, service: &str) -> String {
    if service.is_empty() {
        return String::new();
    }
    read_config_value(state)
        .ok()
        .and_then(|c| c.get("services").cloned())
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .find(|s| s.get("id").and_then(|v| v.as_str()) == Some(service))
        .and_then(|s| s.get("comment").and_then(|c| c.as_str()).map(String::from))
        .unwrap_or_else(|| service.to_string())
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
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW：避免打开资源管理器时闪烁 cmd 窗口
        std::process::Command::new("cmd")
            .arg("/C")
            .arg("start")
            .arg("")
            .arg(p)
            .creation_flags(0x08000000)
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
    let mut cfg = read_config_value(&state)?;
    ensure_service_ids_with_registry(&state, &mut cfg);
    Ok(cfg)
}

/// 服务 id 登记表（comment → id），与 config.json 同目录（service_ids.json）。
/// config 被旧快照覆盖保存而丢失 id 时，按显示名找回**同一个** id，
/// 而不是重新随机生成 —— 否则每次覆盖保存都会让全部服务换身份，历史统计失联。
fn service_id_map_path(state: &AppState) -> PathBuf {
    let mut p = config_path(state);
    p.set_file_name("service_ids.json");
    p
}

fn read_service_id_map(state: &AppState) -> serde_json::Map<String, serde_json::Value> {
    std::fs::read_to_string(service_id_map_path(state))
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

fn write_service_id_map(state: &AppState, map: &serde_json::Map<String, serde_json::Value>) {
    if let Ok(s) = serde_json::to_string_pretty(map) {
        let _ = std::fs::write(service_id_map_path(state), s);
    }
}

fn new_random_service_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut x = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
        ^ COUNTER.fetch_add(0x9E37_79B9_7F4A_7C15, Ordering::Relaxed);
    // splitmix64 终混
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = x;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    format!("svc-{:08x}", (z & 0xFFFF_FFFF) as u32)
}

/// 缺失/重复 id 的补齐：优先按显示名从登记表找回，找不到才生成新 id；
/// 最终 (comment, id) 对齐回登记表（有变化才写盘）。与引擎侧 o2a/config.py 共用同一份登记表。
fn ensure_service_ids_with_registry(state: &AppState, cfg: &mut serde_json::Value) {
    let Some(services) = cfg.get_mut("services").and_then(|v| v.as_array_mut()) else {
        return;
    };
    let mut registry = read_service_id_map(state);
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut changed = false;
    for s in services.iter_mut() {
        let comment = s
            .get("comment")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let id = s
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if !id.is_empty() && !seen.contains(&id) {
            seen.insert(id);
            continue;
        }
        // 缺失或重复：先按显示名找回，登记 id 未被占用才可用
        let recovered = registry
            .get(&comment)
            .and_then(|v| v.as_str())
            .filter(|x| !x.is_empty() && !seen.contains(*x))
            .map(|x| x.to_string());
        let new_id = recovered.unwrap_or_else(|| {
            let mut id = new_random_service_id();
            while seen.contains(&id) {
                id = new_random_service_id();
            }
            id
        });
        seen.insert(new_id.clone());
        if let Some(o) = s.as_object_mut() {
            o.insert("id".to_string(), serde_json::json!(new_id));
        }
        changed = true;
    }
    // (comment, id) 对齐回登记表；改名后旧键保留（回滚旧快照时仍可找回）
    for s in services.iter() {
        let comment = s.get("comment").and_then(|v| v.as_str()).unwrap_or("");
        let id = s.get("id").and_then(|v| v.as_str()).unwrap_or("");
        if comment.is_empty() || id.is_empty() {
            continue;
        }
        if registry.get(comment).and_then(|v| v.as_str()) != Some(id) {
            registry.insert(comment.to_string(), serde_json::json!(id));
            changed = true;
        }
    }
    if changed {
        write_service_id_map(state, &registry);
    }
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
    // 服务 id 稳定化：保存前补齐缺失/重复 id，并把 (comment, id) 对齐到
    // service_ids.json 登记表。避免面板内存/旧快照保存时把“临时生成的 id”写入
    // config 却不同步登记表，导致下次 id 丢失后引擎又随机生成新 id、历史统计失联。
    ensure_service_ids_with_registry(&state, &mut cfg);
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
fn is_port_open(host: String, port: u16) -> bool {
    crate::port_open(&host, port)
}

/// §10.4 托盘逐服务启停：按当前配置重建托盘菜单（含每服务 启动/停止 项）。
/// 在 get_status 轮询与 save_config 后调用 —— 面板打开期间保持菜单与运行态同步。
fn rebuild_tray_menu(app: &tauri::AppHandle) -> tauri::Result<()> {
    use tauri::menu::{MenuItem, PredefinedMenuItem, Submenu};
    let state = app.state::<AppState>();
    let cfg = read_config_value(&state).unwrap_or(serde_json::json!({}));
    let services = cfg
        .get("services")
        .and_then(|s| s.as_array())
        .cloned()
        .unwrap_or_default();
    let top_token = String::new();

    let panel_i = MenuItem::with_id(app, "panel", "打开面板", true, None::<&str>)?;
    let float_i = MenuItem::with_id(app, "float", "悬浮看板", true, None::<&str>)?;
    let start_all_i = MenuItem::with_id(app, "start_all", "启动全部代理", true, None::<&str>)?;
    let stop_all_i = MenuItem::with_id(app, "stop_all", "停止全部代理", true, None::<&str>)?;
    let proxy_sub = Submenu::with_id(app, "proxy", "代理", true)?;
    proxy_sub.append(&start_all_i)?;
    proxy_sub.append(&stop_all_i)?;
    proxy_sub.append(&PredefinedMenuItem::separator(app)?)?;
    for s in &services {
        let enabled = s.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);
        if !enabled {
            continue;
        }
        let sid = s.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let name = s
            .get("comment")
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .to_string();
        if sid.is_empty() {
            continue;
        }
        let running = {
            let mut children = state.children.lock().unwrap();
            children
                .get_mut(&sid)
                .map(|c| c.try_wait().ok().flatten().is_none())
                .unwrap_or(false)
        };
        let label = if running {
            format!("● 停止 {}", if name.is_empty() { &sid } else { &name })
        } else {
            format!("○ 启动 {}", if name.is_empty() { &sid } else { &name })
        };
        let id = format!("svc:toggle:{sid}");
        let item = MenuItem::with_id(app, id.as_str(), label.as_str(), true, None::<&str>)?;
        proxy_sub.append(&item)?;
    }
    let open_cfg_i = MenuItem::with_id(app, "open_cfg", "打开 config.json", true, None::<&str>)?;
    let quit_i = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
    let menu = tauri::menu::Menu::with_items(app, &[&panel_i, &float_i, &proxy_sub, &open_cfg_i, &quit_i])?;
    if let Some(tray) = app.tray_by_id("main") {
        tray.set_menu(Some(menu))?;
    }
    Ok(())
}

/// §9 热重载触发：对每个监听中的服务端口 POST /_reload（带接入凭证）。
/// 引擎收到后按 id diff 重载（原地生效/换端口重启）；失败保持旧配置。
fn reload_engine_impl(state: &AppState) -> Result<serde_json::Value, String> {
    let cfg = read_config_value(state)?;
    let top_token = cfg
        .get("auth_token")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let services = cfg.get("services").and_then(|s| s.as_array()).cloned().unwrap_or_default();
    let mut reloaded = 0usize;
    let mut skipped = 0usize;
    let mut errors: Vec<String> = Vec::new();
    for s in &services {
        let host = s.get("listen_host").and_then(|v| v.as_str()).unwrap_or("127.0.0.1").to_string();
        let port = s
            .get("listen_address")
            .and_then(|v| v.as_u64().map(|n| n as u16))
            .or_else(|| {
                s.get("listen_address")
                    .and_then(|v| v.as_str())
                    .and_then(|x| x.parse().ok())
            })
            .unwrap_or(0);
        if port == 0 || !crate::port_open(&host, port) {
            continue; // 未运行的服务无需重载
        }
        let name = s.get("comment").and_then(|c| c.as_str()).unwrap_or("").to_string();
        let mut token = s
            .get("auth_token")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if token.is_empty() {
            token = top_token.clone();
        }
        let url = format!("http://{host}:{port}/_reload");
        let mut req = ureq::post(&url).timeout(Duration::from_secs(3));
        if !token.is_empty() {
            req = req.set("Authorization", &format!("Bearer {token}"));
        }
        match req.call() {
            Ok(resp) if resp.status() == 200 => reloaded += 1,
            Ok(resp) => {
                let label = if name.is_empty() {
                    format!(":{port}")
                } else {
                    name.clone()
                };
                errors.push(format!("{}: HTTP {}", label, resp.status()));
            }
            Err(e) => {
                skipped += 1;
                errors.push(format!(":{} {e}", port));
            }
        }
    }
    Ok(serde_json::json!({"reloaded": reloaded, "skipped": skipped, "errors": errors}))
}

#[tauri::command]
async fn reload_engine(app: tauri::AppHandle) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        reload_engine_impl(&state)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn get_status(app: tauri::AppHandle) -> Result<serde_json::Value, String> {
    // 异步命令跑在 Tauri 线程池；内部含端口探测（200ms/服务超时），
    // 用 spawn_blocking 挪到阻塞线程池，避免占住异步运行时线程。
    let app_for_tray = app.clone();
    let out = tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        get_status_impl(&state)
    })
    .await
    .map_err(|e| e.to_string())?;
    let out = out?;
    // §10.4：轮询后同步托盘逐服务菜单（运行态变化时托盘文案随之更新）
    let _ = rebuild_tray_menu(&app_for_tray);
    Ok(out)
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
            let sid = s
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let enabled = s.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);
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
            // 子进程表 key：桌面端以 id 启停（阶段1 id 化）；保留按 name 兼容查找一个版本周期
            let child_key: Option<String> = if !sid.is_empty() && children.contains_key(&sid) {
                Some(sid.clone())
            } else if children.contains_key(&name) {
                Some(name.clone())
            } else {
                None
            };
            let child_alive = child_key
                .and_then(|k| children.get_mut(&k))
                .map(|c| c.try_wait().ok().flatten().is_none())
                .unwrap_or(false);
            let running = child_alive || (port > 0 && port_open(&host, port));
            let mut svc = serde_json::json!({
                "id": sid,
                "name": name,
                "running": running,
                "enabled": enabled,
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
    let app2 = app.clone();
    let res = tauri::async_runtime::spawn_blocking(move || {
        let state = app2.state::<AppState>();
        proxy::start_service(&state, &name)
    })
    .await
    .map_err(|e| e.to_string())?;
    res?;
    update_tray_tooltip(&app);
    Ok(())
}

#[tauri::command]
async fn stop_service(app: tauri::AppHandle, name: String) -> Result<(), String> {
    // child.wait() 等待子进程退出，同样放阻塞线程池
    let app2 = app.clone();
    let res = tauri::async_runtime::spawn_blocking(move || {
        let state = app2.state::<AppState>();
        proxy::stop_service(&state, &name)
    })
    .await
    .map_err(|e| e.to_string())?;
    res?;
    update_tray_tooltip(&app);
    Ok(())
}

#[tauri::command]
async fn toggle_service(app: tauri::AppHandle, name: String) -> Result<(), String> {
    let app2 = app.clone();
    let res = tauri::async_runtime::spawn_blocking(move || {
        let state = app2.state::<AppState>();
        proxy::toggle_service(&state, &name)
    })
    .await
    .map_err(|e| e.to_string())?;
    res?;
    update_tray_tooltip(&app);
    Ok(())
}

#[tauri::command]
async fn start_all(app: tauri::AppHandle) -> Result<(), String> {
    let app2 = app.clone();
    let res = tauri::async_runtime::spawn_blocking(move || {
        let state = app2.state::<AppState>();
        proxy::start_all(&state)
    })
    .await
    .map_err(|e| e.to_string())?;
    res?;
    update_tray_tooltip(&app);
    Ok(())
}

#[tauri::command]
async fn stop_all(app: tauri::AppHandle) -> Result<(), String> {
    let app2 = app.clone();
    let res = tauri::async_runtime::spawn_blocking(move || {
        let state = app2.state::<AppState>();
        proxy::stop_all(&state)
    })
    .await
    .map_err(|e| e.to_string())?;
    res?;
    update_tray_tooltip(&app);
    Ok(())
}

#[tauri::command]
async fn get_stats(
    app: tauri::AppHandle,
    service: String,
    range: Option<String>,
    start: Option<String>,
    end: Option<String>,
    model: Option<String>,
) -> Result<serde_json::Value, String> {
    // 全量读 jsonl + 按月重算费用较重，异步执行 + stats.rs 内部 TTL 缓存
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let svc_name = resolve_service_name(&state, &service);
        let r = range.as_deref().unwrap_or("today");
        let out = stats::get_stats(
            &stats_dir(&state),
            &svc_name,
            &service,
            &primary_service(&state),
            r,
            start.as_deref(),
            end.as_deref(),
            model.as_deref().unwrap_or(""),
        );
        // §临时诊断：定位“选中服务无数据”——打印运行时查询三要素（用后即删）
        if let Ok(ref v) = out {
            eprintln!(
                "[stats-diag] raw={:?} resolved={:?} range={:?} dir={} today.requests={} month.requests={}",
                service,
                svc_name,
                r,
                stats_dir(&state).display(),
                v["today"]["requests"],
                v["month"]["requests"]
            );
        }
        out
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn get_daily(
    app: tauri::AppHandle,
    service: String,
    start: String,
    end: String,
) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let svc_name = resolve_service_name(&state, &service);
        stats::get_daily(&stats_dir(&state), &svc_name, &service, &primary_service(&state), &start, &end)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn get_live(app: tauri::AppHandle, service: String) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let svc_name = resolve_service_name(&state, &service);
        stats::get_live(&stats_dir(&state), &svc_name, &service, &primary_service(&state))
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

// ---------------------------------------------------------------------------
// §8.4-4 端隔离：额度只存在引擎一份实现，桌面端仅转发 + 缓存（不重写适配逻辑）。
// 引擎侧自带 60s TTL；桌面端再加一层多槽缓存（key = account），避免多窗口
// 交替轮询互相顶掉（对齐 §10.1 多槽缓存方向）。
// ---------------------------------------------------------------------------
static QUOTA_CACHE: std::sync::Mutex<Option<std::collections::HashMap<String, (std::time::Instant, serde_json::Value)>>> =
    std::sync::Mutex::new(None);
const QUOTA_CACHE_TTL_SECS: u64 = 60;
const QUOTA_CACHE_MAX: usize = 32;

fn quota_cache_get(key: &str) -> Option<serde_json::Value> {
    let guard = QUOTA_CACHE.lock().unwrap();
    let map = guard.as_ref()?;
    let (ts, value) = map.get(key)?;
    if ts.elapsed() < Duration::from_secs(QUOTA_CACHE_TTL_SECS) {
        return Some(value.clone());
    }
    None
}

fn quota_cache_stale(key: &str) -> Option<serde_json::Value> {
    let guard = QUOTA_CACHE.lock().unwrap();
    let map = guard.as_ref()?;
    let (_, value) = map.get(key)?;
    let mut v = value.clone();
    v["stale"] = serde_json::json!(true);
    Some(v)
}

fn quota_cache_set(key: &str, value: serde_json::Value) {
    let mut guard = QUOTA_CACHE.lock().unwrap();
    let map = guard.get_or_insert_with(std::collections::HashMap::new);
    if map.len() >= QUOTA_CACHE_MAX && !map.contains_key(key) {
        // 淘汰最旧条目
        if let Some(oldest) = map
            .iter()
            .min_by_key(|(_, (t, _))| *t)
            .map(|(k, _)| k.clone())
        {
            map.remove(&oldest);
        }
    }
    map.insert(key.to_string(), (std::time::Instant::now(), value));
}

/// 取某账号的额度快照：找该账号绑定的运行中服务的端口 → 引擎 GET /quota。
/// 无运行中的引擎 → 返回错误（前端隐藏额度卡，不影响其他渲染）。
fn get_quota_impl(state: &AppState, account: &str) -> Result<serde_json::Value, String> {
    if account.is_empty() {
        return Err("账号未指定".into());
    }
    if let Some(v) = quota_cache_get(account) {
        return Ok(v);
    }
    let cfg = read_config_value(state)?;
    let services = cfg.get("services").and_then(|s| s.as_array()).cloned().unwrap_or_default();
    // 目标账号绑定的服务优先，其次任意运行中的服务（引擎进程可查任意账号）
    let mut candidates: Vec<(String, u16)> = Vec::new();
    for s in &services {
        let acc = s.get("account").and_then(|v| v.as_str()).unwrap_or("");
        let sid = s.get("id").and_then(|v| v.as_str()).unwrap_or("");
        let bound = acc == account || sid == account;
        let host = s.get("listen_host").and_then(|v| v.as_str()).unwrap_or("127.0.0.1").to_string();
        let port = s
            .get("listen_address")
            .and_then(|v| v.as_u64().map(|n| n as u16))
            .or_else(|| {
                s.get("listen_address")
                    .and_then(|v| v.as_str())
                    .and_then(|x| x.parse().ok())
            })
            .unwrap_or(0);
        if port == 0 {
            continue;
        }
        if bound {
            candidates.insert(0, (host, port));
        } else {
            candidates.push((host, port));
        }
    }
    for (host, port) in candidates {
        if !crate::port_open(&host, port) {
            continue;
        }
        let url = format!("http://{host}:{port}/quota?account={account}");
        let req = ureq::get(&url).timeout(Duration::from_secs(2));
        if let Ok(resp) = req.call() {
            if let Ok(s) = resp.into_string() {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&s) {
                    if v.get("error").is_none() {
                        quota_cache_set(account, v.clone());
                        return Ok(v);
                    }
                }
            }
        }
    }
    // 全部失败：返回过期缓存并标 stale（§8.3 降级展示），否则报错
    if let Some(v) = quota_cache_stale(account) {
        return Ok(v);
    }
    Err("额度不可用（引擎未运行或端口不可达）".into())
}

#[tauri::command]
async fn get_quota(app: tauri::AppHandle, account: String) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        get_quota_impl(&state, &account)
    })
    .await
    .map_err(|e| e.to_string())?
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

/// 查询当前生效的配置文件位置（用于 UI 展示与编辑）。
#[tauri::command]
fn get_config_location(state: State<'_, AppState>) -> serde_json::Value {
    let env_set = std::env::var_os("O2A_CONFIG")
        .map(|v| !v.is_empty())
        .unwrap_or(false);
    let saved = saved_config_override(&state.settings_file);
    let source = if env_set {
        "env"
    } else if saved.is_some() {
        "settings"
    } else {
        "default"
    };
    serde_json::json!({
        "config": config_path(&state),
        "auth": auth_path(&state),
        "source": source,
        "saved": saved,
        "settings_file": state.settings_file,
    })
}

/// UI 保存配置文件位置覆盖；path 为空串 = 恢复默认。
/// 返回保存后生效的位置信息（同 get_config_location）。
#[tauri::command]
fn set_config_location(state: State<'_, AppState>, path: String) -> Result<serde_json::Value, String> {
    let raw = path.trim().to_string();

    // 读旧设置（文件可能不存在），保持其他字段
    let mut v = std::fs::read_to_string(&state.settings_file)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .unwrap_or_else(|| serde_json::json!({}));

    if raw.is_empty() {
        // 恢复默认：移除 override，重新走环境变量 / 项目根
        if let Some(obj) = v.as_object_mut() {
            obj.remove("config");
        }
    } else {
        let loc = PathBuf::from(&raw);
        let resolved = to_file_if_dir(&loc, "config.json");
        // 父目录必须存在（配置文件本身可不存在，首次创建场景）；目录已存在亦可
        if let Some(parent) = resolved.parent() {
            let parent = parent.to_path_buf();
            if !parent.as_os_str().is_empty() && !parent.exists() {
                return Err(format!("目录不存在: {}", parent.display()));
            }
        }
        if let Some(obj) = v.as_object_mut() {
            obj.insert(
                "config".into(),
                serde_json::Value::String(loc.to_string_lossy().to_string()),
            );
            obj.insert(
                "_readme".into(),
                serde_json::Value::String(
                    "由 o2a-proxy 桌面端写入：覆盖默认配置文件位置（config.json / auth.json 跟随）".into(),
                ),
            );
        }
    }

    if let Some(parent) = state.settings_file.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(&state.settings_file, serde_json::to_string_pretty(&v).unwrap_or_default())
        .map_err(|e| e.to_string())?;

    Ok(get_config_location(state))
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

/// 恢复悬浮窗尺寸（用户上次缩放记忆，前端 localStorage 持久化）
#[tauri::command]
fn set_float_size(app: tauri::AppHandle, width: f64, height: f64) -> Result<(), String> {
    if let Some(w) = app.get_webview_window("float") {
        w.set_size(tauri::LogicalSize::new(
            width.max(300.0),
            height.max(170.0),
        ))
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// 托盘 tooltip 动态状态：桌面端托管的运行中服务数。
/// children 仅含桌面端启动的子进程；外部启动的引擎进程不统计（仅作提示）。
fn update_tray_tooltip(app: &tauri::AppHandle) {
    if let Some(tray) = app.tray_by_id("main") {
        let state = app.state::<AppState>();
        let running = state.children.lock().unwrap().len();
        let _ = tray.set_tooltip(Some(format!("o2a-proxy · {} 个服务运行中", running)));
    }
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
        // 全局快捷键：Ctrl+Alt+O 唤出面板 / Ctrl+Alt+F 唤出悬浮窗
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, shortcut, event| {
                    if event.state() != tauri_plugin_global_shortcut::ShortcutState::Pressed {
                        return;
                    }
                    let ctrl_alt =
                        tauri_plugin_global_shortcut::Modifiers::CONTROL | tauri_plugin_global_shortcut::Modifiers::ALT;
                    if shortcut.matches(ctrl_alt, tauri_plugin_global_shortcut::Code::KeyO) {
                        let _ = toggle_panel(app.clone());
                    } else if shortcut.matches(ctrl_alt, tauri_plugin_global_shortcut::Code::KeyF) {
                        let _ = toggle_float(app.clone());
                    }
                })
                .build(),
        )
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            resolve_root,
            resolve_python,
            read_config,
            save_config,
            get_status,
            is_port_open,
            get_quota,
            reload_engine,
            start_service,
            stop_service,
            toggle_service,
            start_all,
            stop_all,
            get_stats,
            get_daily,
            get_live,
            fetch_models,
            open_config_file,
            get_config_location,
            set_config_location,
            toggle_panel,
            hide_panel,
            toggle_float,
            toggle_float_for,
            set_float_size,
            quit_app,
        ])
        .setup(|app| {
            // 全局快捷键注册（注册失败不阻塞启动：个别平台/桌面环境可能占用）
            {
                use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut};
                let gs = app.global_shortcut();
                let ctrl_alt = Modifiers::CONTROL | Modifiers::ALT;
                let _ = gs.register(Shortcut::new(Some(ctrl_alt), Code::KeyO));
                let _ = gs.register(Shortcut::new(Some(ctrl_alt), Code::KeyF));
            }
            // macOS：纯菜单栏应用（无 Dock 图标），与原 Electron 行为一致
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let root = find_root(app);
            // 绿色版判定：root 来自打包资源目录（非 O2A_ROOT、非开发目录）
            let resource_root = app
                .path()
                .resource_dir()
                .ok()
                .filter(|d| d.join("proxy.py").exists());
            let persistent_config =
                std::env::var_os("O2A_ROOT").is_none() && resource_root.as_ref() == Some(&root);
            let state = AppState {
                root: root.clone(),
                python: find_python(),
                // 默认统计目录：应用（项目根）下的相对目录（data/cache_stats）
                default_stats_dir: root.join("data").join("cache_stats"),
                settings_file: settings_path(app),
                persistent_config,
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
                    id if id.starts_with("svc:toggle:") => {
                        // §10.4 托盘逐服务启停：id 形如 svc:toggle:<服务id>
                        let sid = id.trim_start_matches("svc:toggle:").to_string();
                        let app = app.clone();
                        tauri::async_runtime::spawn(async move {
                            let state = app.state::<AppState>();
                            let running = {
                                let mut children = state.children.lock().unwrap();
                                children
                                    .get_mut(&sid)
                                    .map(|c| c.try_wait().ok().flatten().is_none())
                                    .unwrap_or(false)
                            };
                            let res = if running {
                                crate::proxy::stop_service(&state, &sid)
                            } else {
                                crate::proxy::start_service(&state, &sid)
                            };
                            if let Err(e) = res {
                                eprintln!("托盘启停服务失败 {sid}: {e}");
                            }
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
                // 可缩放：大屏可拉高查看更多统计；紧凑布局由 CSS 媒体查询适配
                .resizable(true)
                .min_inner_size(430.0, 560.0)
                .decorations(false)
                .transparent(true)
                .skip_taskbar(true)
                .visible(false)
                .build()?;
            panel.on_window_event(panel_events(app.handle().clone()));
            // 启动即显示面板（绿色版/双击启动必须有可见反馈；
            // 设 O2A_SHOW_PANEL=0 可恢复纯托盘模式）
            if std::env::var_os("O2A_SHOW_PANEL").map(|v| v == "0").unwrap_or(false) {
                // 纯托盘模式：保持隐藏
            } else {
                position_panel(app.handle(), &panel)?;
                panel.show()?;
                panel.set_focus()?;
                emit_panel_visible(app.handle(), true);
            }

            // 全平台统一：预创建 1 个共享悬浮窗（隐藏），显示哪个服务由
            // float-switch 事件切换（见 toggle_float_shared）。必须在此阶段
            // 创建：运行时动态创建透明 WebView2 会卡死事件循环（已实测验证），
            // macOS/Linux 也统一预创建，走同一套单窗逻辑。
            create_float_window(app.handle(), "float", "")?;

            // autostart（§2.1/§5.2C）：延迟 1.2s 等窗口/托盘初始化完成后，
            // 自动拉起标记 autostart=true 的服务（阻塞线程池里 sleep + 串行启动）
            let app2 = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let _ = tauri::async_runtime::spawn_blocking(move || {
                    std::thread::sleep(std::time::Duration::from_millis(1200));
                    let state = app2.state::<AppState>();
                    let _ = crate::proxy::start_autostart(&state);
                })
                .await;
            });

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
