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
  candidates.push("/Users/macos/workspace/self/ai-work/o2a-proxy");
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
    s.model = s.model || "qwen-plus";
    s.sub_model = s.sub_model || s.model;
    s.listen_address = String(s.listen_address || "11011");
    s.openai_base_url = s.openai_base_url || def.services[0].openai_base_url;
    s.openai_api_key = s.openai_api_key || "";
    s.context_1m = !!s.context_1m;
  }
  return cfg;
}

function writeConfig(cfg) {
  fs.writeFileSync(CONFIG_PATH, JSON.stringify(cfg, null, 2) + "\n", "utf-8");
}

// ---------- 代理进程管理 ----------
let child = null;
let running = false;
let panel = null;
let floatWin = null;
let tray = null;
let lastError = "";

// 向所有窗口（面板 + 悬浮窗）广播
function sendAll(channel, payload) {
  for (const w of [panel, floatWin]) {
    if (w && !w.isDestroyed()) w.webContents.send(channel, payload);
  }
}

function buildEnv(cfg) {
  const svc = cfg.services[0];
  return {
    ...process.env,
    PROXY_HOST: "127.0.0.1",
    PROXY_PORT: String(svc.listen_address),
    DASHSCOPE_URL: svc.openai_base_url,
    DASHSCOPE_API_KEY: svc.openai_api_key,
    PROXY_MODEL: svc.model,
    SUB_PROXY_MODEL: svc.sub_model || svc.model,
    CACHE_STATS_ENABLED: String(cfg.cache_stats_enabled),
    CACHE_STATS_DIR: cfg.cache_stats_dir,
    CACHE_STATS_RETENTION_DAYS: String(cfg.cache_stats_retention_days),
    PROXY_MAX_TOKENS: svc.context_1m ? "1000000" : "4096",
  };
}

function startProxy() {
  if (running && child) return { ok: true, running: true };
  const cfg = readConfig();
  if (!fs.existsSync(PROXY_SCRIPT)) {
    lastError = `未找到代理脚本: ${PROXY_SCRIPT}`;
    return { ok: false, error: lastError };
  }
  const svc = cfg.services[0];
  if (!svc.openai_api_key) {
    lastError = "API Key 未配置，请先在「配置」中填写";
    return { ok: false, error: lastError };
  }
  try {
    lastError = "";
    child = spawn(resolvePython(), [PROXY_SCRIPT], {
      env: buildEnv(cfg),
      cwd: ROOT,
      stdio: ["ignore", "pipe", "pipe"],
    });
    running = true;
    let stderrTail = "";
    child.stdout.on("data", (d) => { try { console.log(`[proxy] ${d.toString().trim()}`); } catch (_) {} });
    child.stderr.on("data", (d) => {
      const s = d.toString();
      stderrTail = (stderrTail + s).slice(-500);
      try { console.error(`[proxy] ${s.trim()}`); } catch (_) {}
    });
    child.on("exit", (code, signal) => {
      running = false;
      child = null;
      if (code && code !== 0) {
        // 提炼常见错误
        if (/Address already in use|EADDRINUSE|address in use/i.test(stderrTail)) {
          lastError = `端口 :${svc.listen_address} 已被占用（可能有其他代理进程在运行）`;
        } else {
          lastError = `代理异常退出 (code=${code})`;
        }
      }
      console.log(`[proxy] exited code=${code} signal=${signal}`);
      stopLiveWatch();
      pushStatus();
    });
    child.on("error", (e) => {
      running = false;
      child = null;
      lastError = "启动失败: " + e.message;
      pushStatus();
    });
    startLiveWatch();
    pushStatus();
    return { ok: true, running: true, port: svc.listen_address, model: svc.model };
  } catch (e) {
    running = false;
    child = null;
    lastError = e.message;
    return { ok: false, error: e.message };
  }
}

function stopProxy() {
  if (child) {
    try {
      child.kill("SIGINT");
    } catch (_) {}
    setTimeout(() => {
      if (child) {
        try { child.kill("SIGKILL"); } catch (_) {}
      }
    }, 1500);
  }
  running = false;
  lastError = "";
  stopLiveWatch();
  pushStatus();
  return { ok: true, running: false };
}

function statusPayload() {
  const svc = readConfig().services[0];
  return {
    running,
    port: svc.listen_address,
    model: svc.model,
    context1m: !!svc.context_1m,
    error: lastError,
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
  const cfg = readConfig();
  const port = cfg.services[0].listen_address;
  return Menu.buildFromTemplate([
    { label: running ? `● 代理运行中 :${port}` : "○ 代理已停止", enabled: false },
    { label: running ? "停止代理" : "启动代理", click: () => (running ? stopProxy() : startProxy()) },
    { type: "separator" },
    {
      label: (floatWin && !floatWin.isDestroyed() && floatWin.isVisible()) ? "关闭悬浮看板" : "打开悬浮看板",
      click: () => toggleFloat(),
    },
    { label: "打开配置文件", click: () => shell.openPath(CONFIG_PATH).catch(() => {}) },
    { type: "separator" },
    { label: "退出", click: () => { app.isQuiting = true; stopProxy(); app.quit(); } },
  ]);
}

function updateTray() {
  if (!tray) return;
  tray.setToolTip(running ? "o2a-proxy · 运行中" : "o2a-proxy · 已停止");
  // 运行时在图标旁显示端口，一眼可见状态
  tray.setTitle(running ? "" : "", { fontType: "monospacedDigit" });
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

// ---------- 悬浮看板（置顶小窗，实时查看） ----------
const FLOAT_W = 300;
const FLOAT_H = 210;
const FLOAT_STATE = path.join(app.getPath("userData"), "float-state.json");

function readFloatState() {
  try { return JSON.parse(fs.readFileSync(FLOAT_STATE, "utf-8")); } catch (_) { return {}; }
}
function saveFloatState() {
  if (!floatWin || floatWin.isDestroyed()) return;
  try {
    const [x, y] = floatWin.getPosition();
    fs.writeFileSync(FLOAT_STATE, JSON.stringify({ x, y, open: floatWin.isVisible() }));
  } catch (_) {}
}

function createFloatWin() {
  const st = readFloatState();
  const display = screen.getPrimaryDisplay();
  const wa = display.workArea;
  let x = Number.isFinite(st.x) ? st.x : wa.x + wa.width - FLOAT_W - 16;
  let y = Number.isFinite(st.y) ? st.y : wa.y + 44;
  // 防止历史位置在屏幕外
  x = Math.min(Math.max(x, wa.x), wa.x + wa.width - FLOAT_W);
  y = Math.min(Math.max(y, wa.y), wa.y + wa.height - FLOAT_H);

  floatWin = new BrowserWindow({
    width: FLOAT_W,
    height: FLOAT_H,
    x, y,
    show: false,
    frame: false,
    resizable: false,
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
  floatWin.setAlwaysOnTop(true, "floating");
  floatWin.setVisibleOnAllWorkspaces(true, { visibleOnFullScreen: true });
  floatWin.loadFile(path.join(__dirname, "renderer", "float.html"));
  floatWin.on("moved", saveFloatState);
  floatWin.on("close", (e) => {
    if (!app.isQuiting) {
      e.preventDefault();
      floatWin.hide();
      saveFloatState();
      sendAll("float-state", { open: false });
    }
  });
  return floatWin;
}

function toggleFloat(forceOpen) {
  if (!floatWin || floatWin.isDestroyed()) createFloatWin();
  const open = forceOpen === undefined ? !floatWin.isVisible() : !!forceOpen;
  if (open) {
    floatWin.showInactive(); // 不抢焦点，面板不会因 blur 收起
    floatWin.webContents.send("status", statusPayload());
    floatWin.webContents.send("panel-shown");
  } else {
    floatWin.hide();
  }
  saveFloatState();
  sendAll("float-state", { open });
  return { ok: true, open };
}

// ---------- IPC ----------
ipcMain.handle("get-config", () => readConfig());
ipcMain.handle("save-config", (_e, cfg) => {
  // 运行中锁定配置：必须先停止代理才能修改（避免运行参数与配置不一致）
  if (running) {
    return { ok: false, locked: true, error: "代理运行中，请先停止代理再修改配置" };
  }
  try {
    writeConfig(cfg);
    pushStatus();
    return { ok: true };
  } catch (e) {
    return { ok: false, error: e.message };
  }
});
// 实时调用记录：面板打开时取最近缓冲
ipcMain.handle("get-live", () => ({ running, records: liveBuf.slice(-LIVE_MAX) }));
ipcMain.handle("get-stats", () => {
  const cfg = readConfig();
  const dir = path.join(ROOT, cfg.cache_stats_dir || "cache_stats");
  try {
    return new Stats(dir).getStats();
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
ipcMain.handle("toggle-proxy", () => (running ? stopProxy() : startProxy()));
ipcMain.handle("open-config-file", () => {
  shell.openPath(CONFIG_PATH).catch(() => {});
  return { ok: true };
});
ipcMain.handle("hide-panel", () => {
  if (panel && !panel.isDestroyed()) panel.hide();
  return { ok: true };
});
ipcMain.handle("toggle-float", (_e, forceOpen) => toggleFloat(forceOpen));
ipcMain.handle("get-float-state", () => ({
  open: !!(floatWin && !floatWin.isDestroyed() && floatWin.isVisible()),
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
    createFloatWin();
    // 上次退出时悬浮窗是打开的 → 恢复显示
    if (readFloatState().open) toggleFloat(true);
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
