#!/usr/bin/env node
const { app, Tray, Menu, BrowserWindow, ipcMain, shell, screen } = require("electron");
const path = require("path");
const fs = require("fs");
const { spawn } = require("child_process");
const Stats = require("./stats");

// 项目根目录（o2a-proxy，包含 proxy.py / config.json）
// 解析顺序：环境变量 O2A_ROOT → 开发模式(mac/ 的上一级) → userData/root.json 记录 → 默认安装路径
function resolveRoot() {
  const candidates = [];
  if (process.env.O2A_ROOT) candidates.push(process.env.O2A_ROOT);
  candidates.push(path.resolve(__dirname, ".."));
  try {
    const saved = JSON.parse(
      fs.readFileSync(path.join(app.getPath("userData"), "root.json"), "utf-8")
    );
    if (saved && saved.root) candidates.push(saved.root);
  } catch (_) {}
  for (const c of candidates) {
    try {
      if (fs.existsSync(path.join(c, "proxy.py"))) return c;
    } catch (_) {}
  }
  return candidates[1]; // 兜底：保持旧行为
}
const ROOT = resolveRoot();

// python3 绝对路径解析（Finder 启动的 .app 里 PATH 很短，找不到 shell 里的 python3）
function resolvePython() {
  if (process.env.O2A_PYTHON && fs.existsSync(process.env.O2A_PYTHON)) return process.env.O2A_PYTHON;
  const candidates = [
    `${process.env.HOME}/.pyenv/shims/python3`,
    "/opt/homebrew/bin/python3",
    "/usr/local/bin/python3",
    "/usr/bin/python3",
  ];
  for (const c of candidates) {
    try { if (fs.existsSync(c)) return c; } catch (_) {}
  }
  return "python3";
}
const CONFIG_PATH = path.join(ROOT, "config.json");
const PROXY_SCRIPT = path.join(ROOT, "proxy.py");
const ICON_PATH = path.join(__dirname, "assets", "trayTemplate.png");

const PANEL_W = 400;
const PANEL_H = 660;

// ---------- 配置读写 ----------
function defaultConfig() {
  return {
    auth_token: "cc-qs",
    cache_stats_enabled: true,
    cache_stats_dir: "cache_stats",
    cache_stats_retention_days: 30,
    services: [
      {
        comment: "service-1",
        mode: "claude",
        model: "qwen-plus",
        sub_model: "qwen-plus",
        listen_address: "11011",
        openai_base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1/chat/completions",
        openai_api_key: "",
        context_1m: false,
      },
    ],
  };
}

function readConfig() {
  let cfg;
  try {
    cfg = JSON.parse(fs.readFileSync(CONFIG_PATH, "utf-8"));
  } catch (_) {
    cfg = defaultConfig();
  }
  const def = defaultConfig();
  cfg.auth_token = cfg.auth_token ?? def.auth_token;
  cfg.cache_stats_enabled = cfg.cache_stats_enabled !== false;
  cfg.cache_stats_dir = cfg.cache_stats_dir || def.cache_stats_dir;
  cfg.cache_stats_retention_days = cfg.cache_stats_retention_days || def.cache_stats_retention_days;
  if (!Array.isArray(cfg.services) || cfg.services.length === 0) {
    cfg.services = def.services;
  }
  for (const s of cfg.services) {
    s.comment = s.comment || "service";
    s.mode = s.mode || "claude"; // claude | codex
    s.model = s.model || "qwen-plus";
    if (s.mode === "claude") {
      s.sub_model = s.sub_model || s.model;
      s.context_1m = !!s.context_1m;
    }
    if (s.mode === "claude" || s.mode === "codex") {
      s.listen_address = String(s.listen_address || "11011");
    }
    s.openai_base_url = s.openai_base_url || def.services[0].openai_base_url;
    s.openai_api_key = s.openai_api_key || "";
  }
  return cfg;
}

function writeConfig(cfg) {
  fs.writeFileSync(CONFIG_PATH, JSON.stringify(cfg, null, 2) + "\n", "utf-8");
}

// ---------- 代理进程管理（按服务独立进程） ----------
const children = {}; // service.comment -> child process
let panel = null;
let tray = null;
let lastError = "";

function runningServices() { return Object.keys(children); }
function anyRunning() { return runningServices().length > 0; }
function isRunning(name) { return !!children[name]; }

// 向所有窗口（面板 + 各服务悬浮窗）广播
function sendAll(channel, payload) {
  for (const w of [panel, ...Object.values(floatWins)]) {
    if (w && !w.isDestroyed()) w.webContents.send(channel, payload);
  }
}

// proxy.py 直接读取 config.json，这里仅透传缓存统计相关环境变量兜底
function buildEnv(cfg) {
  return {
    ...process.env,
    CACHE_STATS_ENABLED: String(cfg.cache_stats_enabled),
    CACHE_STATS_DIR: cfg.cache_stats_dir,
    CACHE_STATS_RETENTION_DAYS: String(cfg.cache_stats_retention_days),
  };
}

function startService(name) {
  if (children[name]) return { ok: true, running: true };
  const cfg = readConfig();
  const svc = (cfg.services || []).find((s) => s.comment === name);
  if (!svc) return { ok: false, error: "服务不存在" };
  if (!svc.openai_api_key) return { ok: false, error: `${name} 的 API Key 未配置` };
  if (!fs.existsSync(PROXY_SCRIPT)) return { ok: false, error: `未找到代理脚本: ${PROXY_SCRIPT}` };
  try {
    lastError = "";
    const child = spawn(resolvePython(), [PROXY_SCRIPT, "--service", name], {
      env: buildEnv(cfg),
      cwd: ROOT,
      stdio: ["ignore", "pipe", "pipe"],
    });
    children[name] = child;
    let stderrTail = "";
    child.stdout.on("data", (d) => { try { console.log(`[${name}] ${d.toString().trim()}`); } catch (_) {} });
    child.stderr.on("data", (d) => {
      const s = d.toString();
      stderrTail = (stderrTail + s).slice(-500);
      try { console.error(`[${name}] ${s.trim()}`); } catch (_) {}
    });
    child.on("exit", (code, signal) => {
      delete children[name];
      if (code && code !== 0) {
        if (/Address already in use|EADDRINUSE|address in use/i.test(stderrTail)) {
          lastError = `${name} 端口 :${svc.listen_address} 已被占用`;
        } else {
          lastError = `${name} 异常退出 (code=${code})`;
        }
      }
      console.log(`[${name}] exited code=${code} signal=${signal}`);
      if (!anyRunning()) stopLiveWatch();
      pushStatus();
    });
    child.on("error", (e) => {
      delete children[name];
      lastError = `启动失败: ${e.message}`;
      pushStatus();
    });
    if (Object.keys(children).length === 1) startLiveWatch();
    pushStatus();
    return { ok: true, running: true };
  } catch (e) {
    delete children[name];
    lastError = e.message;
    return { ok: false, error: e.message };
  }
}

function stopService(name) {
  const child = children[name];
  if (child) {
    try { child.kill("SIGINT"); } catch (_) {}
    setTimeout(() => {
      if (children[name]) { try { children[name].kill("SIGKILL"); } catch (_) {} }
    }, 1500);
  }
  delete children[name];
  if (!anyRunning()) stopLiveWatch();
  lastError = "";
  pushStatus();
  return { ok: true, running: false };
}

function toggleService(name) { return isRunning(name) ? stopService(name) : startService(name); }

function startProxy() {
  const cfg = readConfig();
  const enabled = (cfg.services || []).filter((s) => (s.mode === "claude" || s.mode === "codex") && s.openai_api_key);
  if (!enabled.length) {
    lastError = "没有可代理的服务，或 API Key 未配置";
    pushStatus();
    return { ok: false, error: lastError };
  }
  for (const s of enabled) startService(s.comment);
  return { ok: true };
}

function stopProxy() {
  for (const n of runningServices()) stopService(n);
  return { ok: true };
}

function statusPayload() {
  const cfg = readConfig();
  const services = (cfg.services || []).map((s) => {
    const proxiable = s.mode === "claude" || s.mode === "codex";
    return {
      name: s.comment,
      mode: s.mode,
      port: s.listen_address || "",
      model: s.model || "",
      running: proxiable && !!children[s.comment],
      proxiable,
    };
  });
  const running = services.some((s) => s.running);
  return {
    running,
    error: lastError,
    services,
    ports: services.filter((s) => s.running).map((s) => ({ mode: s.mode, port: s.port, model: s.model })),
  };
}

function pushStatus() {
  sendAll("status", statusPayload());
  updateTray();
}

// ---------- 实时调用监听（tail cache_stats 的 jsonl 增量） ----------
const LIVE_MAX = 80;
let liveBuf = [];
let liveWatcher = null;
let livePoll = null;
let liveFile = "";
let liveOffset = 0;

function statsDirPath() {
  const cfg = readConfig();
  return path.join(ROOT, cfg.cache_stats_dir || "cache_stats");
}

function todayJsonlPath() {
  const d = new Date();
  const name = `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}.jsonl`;
  return path.join(statsDirPath(), name);
}

function readNewLiveLines(pushToPanel) {
  const file = todayJsonlPath();
  if (file !== liveFile) { liveFile = file; liveOffset = 0; } // 跨天滚动
  let st;
  try { st = fs.statSync(liveFile); } catch (_) { return; }
  if (st.size < liveOffset) liveOffset = 0; // 文件被清理/截断
  if (st.size === liveOffset) return;
  let fd;
  try {
    fd = fs.openSync(liveFile, "r");
    const len = st.size - liveOffset;
    const buf = Buffer.alloc(len);
    fs.readSync(fd, buf, 0, len, liveOffset);
    liveOffset = st.size;
    const recs = [];
    for (const ln of buf.toString("utf-8").split("\n")) {
      const t = ln.trim();
      if (!t) continue;
      try { recs.push(JSON.parse(t)); } catch (_) {}
    }
    if (recs.length) {
      liveBuf.push(...recs);
      if (liveBuf.length > LIVE_MAX) liveBuf = liveBuf.slice(-LIVE_MAX);
      if (pushToPanel) sendAll("live-records", recs);
    }
  } finally {
    if (fd !== undefined) { try { fs.closeSync(fd); } catch (_) {} }
  }
}

function startLiveWatch() {
  stopLiveWatch();
  const dir = statsDirPath();
  try { fs.mkdirSync(dir, { recursive: true }); } catch (_) {}
  // 预载今天已有的最近记录（作为历史上下文，不推送事件）
  liveFile = "";
  liveOffset = 0;
  liveBuf = [];
  readNewLiveLines(false);
  if (liveBuf.length > 30) liveBuf = liveBuf.slice(-30);
  try {
    liveWatcher = fs.watch(dir, (_ev, fname) => {
      if (fname && String(fname).endsWith(".jsonl")) readNewLiveLines(true);
    });
  } catch (_) {}
  // 兜底轮询（fs.watch 偶发丢事件）
  livePoll = setInterval(() => readNewLiveLines(true), 2000);
}

function stopLiveWatch() {
  if (liveWatcher) { try { liveWatcher.close(); } catch (_) {} liveWatcher = null; }
  if (livePoll) { clearInterval(livePoll); livePoll = null; }
}

// ---------- 托盘 ----------
function buildTrayMenu() {
  const label = anyRunning()
    ? `● 代理运行中 · ${runningServices().length} 个服务`
    : "○ 代理已停止";
  return Menu.buildFromTemplate([
    { label, enabled: false },
    { label: anyRunning() ? "停止全部代理" : "启动全部代理", click: () => (anyRunning() ? stopProxy() : startProxy()) },
    { type: "separator" },
    {
      label: "打开悬浮看板（第一服务）",
      click: () => { const s = readConfig().services?.[0]; if (s) toggleFloat(s.comment); },
    },
    { label: "打开配置文件", click: () => shell.openPath(CONFIG_PATH).catch(() => {}) },
    { type: "separator" },
    { label: "退出", click: () => { app.isQuiting = true; stopProxy(); app.quit(); } },
  ]);
}

function updateTray() {
  if (!tray) return;
  tray.setToolTip(anyRunning() ? "o2a-proxy · 运行中" : "o2a-proxy · 已停止");
  // 运行时在图标旁显示端口，一眼可见状态
  tray.setTitle(anyRunning() ? "" : "", { fontType: "monospacedDigit" });
}

// ---------- 弹出面板（menubar popover 风格）----------
function createPanel() {
  panel = new BrowserWindow({
    width: PANEL_W,
    height: PANEL_H,
    show: false,
    frame: false,
    resizable: false,
    movable: false,
    minimizable: false,
    maximizable: false,
    fullscreenable: false,
    skipTaskbar: true,
    transparent: true,
    hasShadow: true,
    alwaysOnTop: true,
    hiddenInMissionControl: true,
    webPreferences: {
      preload: path.join(__dirname, "preload.js"),
      contextIsolation: true,
      nodeIntegration: false,
    },
  });
  panel.setAlwaysOnTop(true, "pop-up-menu");
  panel.setVisibleOnAllWorkspaces(true, { visibleOnFullScreen: true });
  panel.loadFile(path.join(__dirname, "renderer", "index.html"));
  // 点击面板外部自动收起
  panel.on("blur", () => {
    if (panel && !panel.webContents.isDevToolsOpened()) panel.hide();
  });
  panel.on("close", (e) => {
    if (!app.isQuiting) {
      e.preventDefault();
      panel.hide();
    }
  });
  return panel;
}

function panelPosition() {
  // 定位到托盘图标正下方；拿不到 bounds 时退化为屏幕右上角
  const tb = tray ? tray.getBounds() : { x: 0, y: 0, width: 0, height: 0 };
  const display = screen.getDisplayNearestPoint({ x: tb.x || 0, y: tb.y || 0 });
  const wa = display.workArea;
  let x, y;
  if (tb.width > 0) {
    x = Math.round(tb.x + tb.width / 2 - PANEL_W / 2);
    y = Math.round(tb.y + tb.height + 6);
  } else {
    x = wa.x + wa.width - PANEL_W - 8;
    y = wa.y + 8;
  }
  x = Math.min(Math.max(x, wa.x + 8), wa.x + wa.width - PANEL_W - 8);
  return { x, y };
}

function togglePanel() {
  if (!panel || panel.isDestroyed()) createPanel();
  if (panel.isVisible()) {
    panel.hide();
    return;
  }
  const { x, y } = panelPosition();
  panel.setPosition(x, y, false);
  panel.show();
  panel.focus();
  panel.webContents.send("status", statusPayload());
  panel.webContents.send("panel-shown");
}

// ---------- 悬浮看板（每个服务独立小窗） ----------
const FLOAT_W = 300;
const FLOAT_H = 210;
const FLOAT_STATE = path.join(app.getPath("userData"), "float-state.json");
const floatWins = {}; // service.comment -> BrowserWindow

function readFloatState() {
  try { return JSON.parse(fs.readFileSync(FLOAT_STATE, "utf-8")); } catch (_) { return {}; }
}
function saveFloatState(name) {
  const w = floatWins[name];
  if (!w || w.isDestroyed()) return;
  try {
    const st = readFloatState();
    const [x, y] = w.getPosition();
    st[name] = { x, y, open: w.isVisible() };
    fs.writeFileSync(FLOAT_STATE, JSON.stringify(st));
  } catch (_) {}
}

function createFloatWin(name) {
  const st = (readFloatState())[name] || {};
  const display = screen.getPrimaryDisplay();
  const wa = display.workArea;
  let x = Number.isFinite(st.x) ? st.x : wa.x + wa.width - FLOAT_W - 16;
  let y = Number.isFinite(st.y) ? st.y : wa.y + 44;
  x = Math.min(Math.max(x, wa.x), wa.x + wa.width - FLOAT_W);
  y = Math.min(Math.max(y, wa.y), wa.y + wa.height - FLOAT_H);

  const w = new BrowserWindow({
    width: FLOAT_W, height: FLOAT_H, x, y,
    show: false, frame: false, resizable: false, minimizable: false, maximizable: false,
    fullscreenable: false, skipTaskbar: true, transparent: true, hasShadow: true,
    alwaysOnTop: true, hiddenInMissionControl: true,
    webPreferences: {
      preload: path.join(__dirname, "preload.js"),
      contextIsolation: true,
      nodeIntegration: false,
    },
  });
  w.setAlwaysOnTop(true, "floating");
  w.setVisibleOnAllWorkspaces(true, { visibleOnFullScreen: true });
  w.loadFile(path.join(__dirname, "renderer", "float.html"), { query: { svc: name } });
  w.on("moved", () => saveFloatState(name));
  w.on("close", (e) => {
    if (!app.isQuiting) {
      e.preventDefault();
      w.hide();
      saveFloatState(name);
    }
  });
  w.on("closed", () => { delete floatWins[name]; });
  floatWins[name] = w;
  return w;
}

function toggleFloat(name, forceOpen) {
  if (!floatWins[name] || floatWins[name].isDestroyed()) createFloatWin(name);
  const w = floatWins[name];
  const open = forceOpen === undefined ? !w.isVisible() : !!forceOpen;
  if (open) {
    w.showInactive(); // 不抢焦点，面板不会因 blur 收起
    w.webContents.send("status", statusPayload());
    w.webContents.send("panel-shown");
  } else {
    w.hide();
  }
  saveFloatState(name);
  sendAll("float-state", { name, open });
  return { ok: true, open };
}

// ---------- IPC ----------
ipcMain.handle("get-config", () => readConfig());
ipcMain.handle("save-config", (_e, cfg) => {
  // 允许运行中保存配置；运行中的代理不会热加载，需重启相应服务后生效
  try {
    writeConfig(cfg);
    pushStatus();
    return { ok: true };
  } catch (e) {
    return { ok: false, error: e.message };
  }
});
// 从最近几天的 jsonl 读取历史实时记录（按服务过滤），用于无运行代理时回退显示
function loadRecentLive(service, limit = LIVE_MAX) {
  const dir = statsDirPath();
  let files;
  try { files = fs.readdirSync(dir).filter((f) => f.endsWith(".jsonl")).sort(); } catch (_) { return []; }
  const recs = [];
  for (const f of files.slice(-4)) { // 最近 4 天
    try {
      const lines = fs.readFileSync(path.join(dir, f), "utf-8").split("\n");
      for (const ln of lines) {
        const t = ln.trim(); if (!t) continue;
        try {
          const r = JSON.parse(t);
          if (service && r.service !== service) continue;
          recs.push(r);
        } catch (_) {}
      }
    } catch (_) {}
  }
  return recs.slice(-limit);
}

// 实时调用记录：面板打开时取最近缓冲；无运行代理时回退到历史实时数据（可按服务过滤）
ipcMain.handle("get-live", (_e, service) => {
  let recs = service ? liveBuf.filter((r) => r.service === service) : liveBuf;
  if (!recs.length) recs = loadRecentLive(service);
  return { running: anyRunning(), records: (recs || []).slice(-LIVE_MAX) };
});
ipcMain.handle("get-stats", (_e, service) => {
  const cfg = readConfig();
  const dir = path.join(ROOT, cfg.cache_stats_dir || "cache_stats");
  try {
    const stats = new Stats(dir);
    stats.migrateLegacy(); // 一次性：把历史数据归入第一个可代理服务
    return stats.getStats(service || undefined);
  } catch (e) {
    return { error: e.message };
  }
});
ipcMain.handle("get-status", () => statusPayload());
// 根据 API 地址拉取模型列表（OpenAI 兼容 /models 端点）
ipcMain.handle("fetch-models", async (_e, { baseUrl, apiKey }) => {
  try {
    let u = String(baseUrl || "").trim().replace(/\/+$/, "");
    if (!u) return { ok: false, error: "API 地址为空" };
    u = u.replace(/\/chat\/completions$/, ""); // 配置里通常填的是 chat/completions 完整地址
    const url = u + "/models";
    const ctrl = new AbortController();
    const timer = setTimeout(() => ctrl.abort(), 8000);
    let res;
    try {
      res = await fetch(url, {
        headers: apiKey ? { Authorization: `Bearer ${apiKey}` } : {},
        signal: ctrl.signal,
      });
    } finally {
      clearTimeout(timer);
    }
    if (!res.ok) return { ok: false, error: `HTTP ${res.status} ${res.statusText}`, url };
    const j = await res.json();
    const arr = Array.isArray(j) ? j : j.data || j.models || [];
    const ids = [...new Set(
      arr.map((m) => (typeof m === "string" ? m : m && (m.id || m.name))).filter(Boolean)
    )].sort();
    if (ids.length === 0) return { ok: false, error: "接口返回了空模型列表", url };
    return { ok: true, models: ids, url };
  } catch (e) {
    const msg = e && e.name === "AbortError" ? "请求超时 (8s)" : (e && e.message) || "未知错误";
    return { ok: false, error: msg };
  }
});
ipcMain.handle("start-proxy", () => startProxy());
ipcMain.handle("stop-proxy", () => stopProxy());
ipcMain.handle("toggle-proxy", () => (anyRunning() ? stopProxy() : startProxy()));
ipcMain.handle("start-service", (_e, name) => startService(name));
ipcMain.handle("stop-service", (_e, name) => stopService(name));
ipcMain.handle("toggle-service", (_e, name) => toggleService(name));
ipcMain.handle("open-config-file", () => {
  shell.openPath(CONFIG_PATH).catch(() => {});
  return { ok: true };
});
ipcMain.handle("hide-panel", () => {
  if (panel && !panel.isDestroyed()) panel.hide();
  return { ok: true };
});
ipcMain.handle("toggle-float", (_e, name) => toggleFloat(name));
ipcMain.handle("get-float-state", (_e, name) => ({
  open: !!(floatWins[name] && !floatWins[name].isDestroyed() && floatWins[name].isVisible()),
}));
ipcMain.handle("quit-app", () => {
  app.isQuiting = true;
  stopProxy();
  app.quit();
  return { ok: true };
});

// ---------- 启动 ----------
if (!app.requestSingleInstanceLock()) {
  app.quit();
} else {
  app.on("second-instance", () => togglePanel());
  app.whenReady().then(() => {
    // 纯菜单栏应用：不显示 Dock 图标
    if (app.dock) app.dock.hide();
    tray = new Tray(ICON_PATH);
    tray.setToolTip("o2a-proxy 菜单栏代理");
    // 左键：弹出/收起面板；右键：快捷菜单
    tray.on("click", () => togglePanel());
    tray.on("right-click", () => tray.popUpContextMenu(buildTrayMenu()));
    createPanel();
    // 恢复上次退出时各服务打开的悬浮窗
    const fst = readFloatState();
    for (const s of readConfig().services || []) {
      if (fst[s.comment] && fst[s.comment].open) toggleFloat(s.comment, true);
    }
    updateTray();
  });
  app.on("before-quit", () => {
    app.isQuiting = true;
    stopProxy();
  });
  app.on("window-all-closed", () => {
    // 保持后台运行（菜单栏应用）
  });
}
