/* o2a-proxy 菜单栏面板 —— 渲染层逻辑 */
(function () {
  const $ = (id) => document.getElementById(id);
  const fmt = (n) => (window.Charts ? Charts.fmtNum(n) : String(n));
  const pct = (n) => (window.Charts ? Charts.fmtPct(n) : (Number(n) * 100).toFixed(1) + "%");

  let status = { running: false, port: "", model: "", context1m: false, error: "" };
  let configCache = null;
  let statsCache = null;
  let range = "today"; // today | month
  let busy = false;

  // ---------- 标签页 ----------
  document.querySelectorAll(".tab").forEach((t) => {
    t.addEventListener("click", () => {
      document.querySelectorAll(".tab").forEach((x) => x.classList.remove("active"));
      document.querySelectorAll(".panel").forEach((x) => x.classList.remove("active"));
      t.classList.add("active");
      $(t.dataset.tab).classList.add("active");
      if (t.dataset.tab === "dashboard") drawCharts();
    });
  });

  // ---------- 状态渲染 ----------
  function renderStatus() {
    const on = !!status.running;
    $("powerSwitch").checked = on;
    $("logo").classList.toggle("on", on);
    const sub = $("headSub");
    sub.textContent = on ? `运行中 · 127.0.0.1:${status.port}` : "代理已停止";
    sub.classList.toggle("on", on);

    $("offHero").style.display = on ? "none" : "";
    $("onBar").style.display = on ? "" : "none";
    if (on) {
      $("onBarText").textContent = `运行中 · 127.0.0.1:${status.port} · ${status.model}`;
      $("onTag1m").style.display = status.context1m ? "" : "none";
    } else {
      $("offMeta").textContent = `代理未启动 · 端口 :${status.port} · ${status.model}${status.context1m ? " · 1M" : ""}`;
      $("offErr").textContent = status.error || "";
    }
    $("footStatus").textContent = on ? `代理运行中 · 端口 ${status.port}` : "点击右上角开关启动代理";

    // 实时区块：仅运行中显示
    const wasHidden = $("liveCard").style.display === "none";
    $("liveCard").style.display = on ? "" : "none";
    if (on && wasHidden) requestAnimationFrame(() => requestAnimationFrame(drawSpark));
    // 配置锁定：运行中禁止编辑
    $("cfgLock").style.display = on ? "" : "none";
    $("configForm").classList.toggle("locked", on);
  }

  async function refreshStatus() {
    try { status = await window.api.getStatus(); } catch (_) {}
    renderStatus();
  }

  // ---------- 启停 ----------
  async function doToggle(targetOn) {
    if (busy) return;
    busy = true;
    try {
      const res = targetOn ? await window.api.startProxy() : await window.api.stopProxy();
      if (res && res.ok === false) toast(res.error || "操作失败");
      // 启动后短暂延迟再取一次状态（进程可能立即退出，如端口占用）
      setTimeout(async () => { await refreshStatus(); }, 1200);
    } finally {
      await refreshStatus();
      busy = false;
    }
  }

  $("powerSwitch").addEventListener("change", (e) => doToggle(e.target.checked));
  $("refreshBtn").addEventListener("click", () => loadStats());
  $("quitBtn").addEventListener("click", () => window.api.quitApp());

  // ---------- 悬浮看板 ----------
  function renderFloatBtn(open) {
    $("floatBtn").classList.toggle("on", !!open);
    $("floatBtn").textContent = open ? "◱ 悬浮中" : "◱ 悬浮";
  }
  $("floatBtn").addEventListener("click", async () => {
    const res = await window.api.toggleFloat();
    renderFloatBtn(res && res.open);
  });
  window.api.onFloatState((s) => renderFloatBtn(s && s.open));
  window.api.getFloatState().then((s) => renderFloatBtn(s && s.open)).catch(() => {});

  // ---------- 统计 ----------
  async function loadStats() {
    let s;
    try { s = await window.api.getStats(); } catch (e) { s = { error: e.message }; }
    if (!s || s.error) {
      $("updatedAt").textContent = "统计读取失败：" + (s && s.error ? s.error : "未知");
      return;
    }
    statsCache = s;
    const cols = [s.current, s.today, s.month];
    cols.forEach((v, i) => {
      $("req" + i).textContent = fmt(v.requests);
      $("in" + i).textContent = fmt(v.input);
      $("rd" + i).textContent = fmt(v.read);
      $("out" + i).textContent = fmt(v.output);
      const total = (v.input || 0) + (v.read || 0) + (v.output || 0);
      $("total" + i).textContent = fmt(total);
      $("hit" + i).textContent = pct(v.hitRate);
      $("cost" + i).textContent = "¥" + (v.cost || 0).toFixed(2);
    });
    drawCharts();
    const t = new Date(s.updatedAt);
    $("updatedAt").textContent = "更新于 " + t.toLocaleTimeString("zh-CN");
  }

  // ---------- 图表 ----------
  document.querySelectorAll(".seg-btn").forEach((b) => {
    b.addEventListener("click", () => {
      document.querySelectorAll(".seg-btn").forEach((x) => x.classList.remove("active"));
      b.classList.add("active");
      range = b.dataset.range;
      drawCharts();
    });
  });

  function drawCharts() {
    if (!statsCache) return;
    const s = statsCache;
    const isDay = range === "today";
    const rows = isDay ? s.todayHourly : s.monthDaily;
    const labels = isDay ? rows.map((x) => x.hour) : rows.map((x) => x.date.slice(5));
    const title = isDay ? "今日缓存命中率 & Token 消耗（逐小时）" : "本月缓存命中率 & Token 消耗（逐日）";
    $("chartTitle").textContent = title;

    const COL = { read: "#2d7ff9", input: "#9aa3b2", output: "#1fab6b" };
    const cv = $("chartCombined");
    cv._dataLen = labels.length;
    Charts.drawCombinedChart(cv, {
      labels,
      series: [
        { name: "输入", color: COL.input, data: rows.map((x) => x.input) },
        { name: "缓存读", color: COL.read, data: rows.map((x) => x.read) },
        { name: "输出", color: COL.output, data: rows.map((x) => x.output) },
      ],
      hitData: rows.map((x) => x.hitRate),
      yFmt: fmt,
    });
    cv._onViewportChange = () => drawCharts();
    Charts.setupChartZoomPan(cv);
  }

  // ---------- 配置表单 ----------
  async function loadConfig() {
    configCache = await window.api.getConfig();
    const f = $("configForm");
    const svc = configCache.services[0];
    f.openai_base_url.value = svc.openai_base_url ?? "";
    f.openai_api_key.value = svc.openai_api_key ?? "";
    f.model.value = svc.model ?? "";
    f.sub_model.value = svc.sub_model ?? "";
    f.listen_address.value = svc.listen_address ?? "";
    f.comment.value = svc.comment ?? "";
    f.context_1m.checked = !!svc.context_1m;
    f.auth_token.value = configCache.auth_token ?? "";
    f.cache_stats_enabled.checked = configCache.cache_stats_enabled !== false;
    f.cache_stats_retention_days.value = configCache.cache_stats_retention_days ?? 30;
    f.cache_stats_dir.value = configCache.cache_stats_dir ?? "cache_stats";
  }

  $("configForm").addEventListener("submit", async (e) => {
    e.preventDefault();
    const f = $("configForm");
    const svc = {
      ...configCache.services[0],
      comment: f.comment.value || "service-1",
      model: f.model.value || "qwen-plus",
      sub_model: f.sub_model.value || f.model.value || "qwen-plus",
      listen_address: f.listen_address.value || "11011",
      openai_base_url: f.openai_base_url.value,
      openai_api_key: f.openai_api_key.value,
      context_1m: f.context_1m.checked,
    };
    const cfg = {
      auth_token: f.auth_token.value,
      cache_stats_enabled: f.cache_stats_enabled.checked,
      cache_stats_retention_days: Number(f.cache_stats_retention_days.value) || 30,
      cache_stats_dir: f.cache_stats_dir.value || "cache_stats",
      services: [svc, ...configCache.services.slice(1)],
    };
    const res = await window.api.saveConfig(cfg);
    if (res.ok) {
      configCache = cfg;
      toast("配置已保存");
      refreshStatus();
    } else {
      toast(res.locked ? "⚠️ 代理运行中，请先停止代理再保存" : "保存失败：" + res.error);
    }
  });

  $("openCfgBtn").addEventListener("click", () => window.api.openConfigFile());

  // ---------- API Key 显示/隐藏 ----------
  $("toggleKeyBtn").addEventListener("click", () => {
    const inp = $("configForm").openai_api_key;
    const show = inp.type === "password";
    inp.type = show ? "text" : "password";
    $("toggleKeyBtn").textContent = show ? "隐藏" : "显示";
  });

  // ---------- 模型列表拉取 + 自定义下拉 ----------
  let modelsKey = ""; // baseUrl|apiKey 缓存键，避免重复拉取
  let fetchingModels = false;
  let modelsList = [];

  function setupCombo(input, listEl) {
    let idx = -1;
    const close = () => { listEl.classList.remove("open"); idx = -1; };
    const render = (q) => {
      const ql = (q || "").toLowerCase();
      const items = (ql ? modelsList.filter((m) => m.toLowerCase().includes(ql)) : modelsList).slice(0, 300);
      listEl.innerHTML = "";
      if (!items.length) { close(); return; }
      for (const m of items) {
        const d = document.createElement("div");
        d.className = "combo-item";
        d.textContent = m;
        // mousedown 先于 blur，preventDefault 避免输入框失焦导致列表提前关闭
        d.addEventListener("mousedown", (e) => { e.preventDefault(); input.value = m; close(); });
        listEl.appendChild(d);
      }
      listEl.classList.add("open");
      listEl.scrollTop = 0;
      idx = -1;
    };
    input.addEventListener("focus", () => render(input.value.trim()));
    input.addEventListener("click", () => render(input.value.trim()));
    input.addEventListener("input", () => render(input.value.trim()));
    input.addEventListener("blur", () => setTimeout(close, 120));
    input.addEventListener("keydown", (e) => {
      if (e.key === "Escape") {
        if (listEl.classList.contains("open")) { e.stopPropagation(); close(); }
        return;
      }
      const items = listEl.querySelectorAll(".combo-item");
      if (!items.length || !listEl.classList.contains("open")) return;
      if (e.key === "ArrowDown" || e.key === "ArrowUp") {
        e.preventDefault();
        idx = e.key === "ArrowDown" ? Math.min(idx + 1, items.length - 1) : Math.max(idx - 1, 0);
        items.forEach((x, i) => x.classList.toggle("sel", i === idx));
        items[idx].scrollIntoView({ block: "nearest" });
      } else if (e.key === "Enter" && idx >= 0) {
        e.preventDefault();
        input.value = items[idx].textContent;
        close();
      }
    });
  }
  setupCombo($("configForm").model, $("comboModel"));
  setupCombo($("configForm").sub_model, $("comboSubModel"));

  function setModelHint(text, isErr) {
    const h = $("modelHint");
    h.textContent = text || "";
    h.classList.toggle("err", !!isErr);
  }

  async function fetchModels(force) {
    const f = $("configForm");
    const baseUrl = f.openai_base_url.value.trim();
    const apiKey = f.openai_api_key.value.trim();
    if (!baseUrl) { setModelHint("填写 API 地址后可拉取模型列表", false); return; }
    const key = baseUrl + "|" + apiKey;
    if (!force && key === modelsKey) return; // 已拉过同样的地址
    if (fetchingModels) return;
    fetchingModels = true;
    $("fetchModelsBtn").classList.add("loading");
    setModelHint("正在拉取模型列表…", false);
    try {
      const res = await window.api.fetchModels({ baseUrl, apiKey });
      if (res.ok) {
        modelsKey = key;
        modelsList = res.models;
        setModelHint(`已加载 ${res.models.length} 个模型，点击输入框下拉选择，输入可过滤`, false);
      } else {
        setModelHint("拉取失败：" + res.error, true);
      }
    } catch (e) {
      setModelHint("拉取失败：" + e.message, true);
    } finally {
      fetchingModels = false;
      $("fetchModelsBtn").classList.remove("loading");
    }
  }

  $("fetchModelsBtn").addEventListener("click", () => fetchModels(true));
  // 地址 / Key 修改后自动重新拉取（防抖）
  let mdTimer = null;
  ["openai_base_url", "openai_api_key"].forEach((name) => {
    $("configForm")[name].addEventListener("input", () => {
      clearTimeout(mdTimer);
      mdTimer = setTimeout(() => fetchModels(false), 900);
    });
  });
  // 切到配置页时自动拉取一次
  document.querySelector('.tab[data-tab="config"]').addEventListener("click", () => fetchModels(false));

  // ---------- 实时调用 ----------
  const LIVE_KEEP = 80;
  let liveRecords = [];

  function liveHitCls(r) {
    const rate = Number(r.cache_hit_rate) || 0;
    return rate >= 0.6 ? "good" : rate > 0.15 ? "mid" : "bad";
  }

  function liveRowHtml(r) {
    const t = String(r.timestamp || "").slice(11, 19) || "--:--:--";
    const total = (r.input_tokens || 0) + (r.cache_read_tokens || 0) + (r.cache_write_tokens || 0);
    const rate = Number(r.cache_hit_rate) || 0;
    return (
      `<span class="live-time">${t}</span>` +
      `<span class="live-tok">↑${fmt(total)} · 读${fmt(r.cache_read_tokens || 0)} · ↓${fmt(r.output_tokens || 0)}</span>` +
      `<span class="live-hit ${liveHitCls(r)}">${(rate * 100).toFixed(0)}%</span>`
    );
  }

  function renderLive(newCount) {
    const feed = $("liveFeed");
    if (!liveRecords.length) {
      feed.innerHTML = '<div class="live-empty">等待请求…（发起调用后此处实时滚动）</div>';
      $("liveSum").textContent = "";
      drawSpark();
      return;
    }
    // 最新在最上，最多渲染 30 行
    const rows = liveRecords.slice(-30).reverse();
    feed.innerHTML = rows
      .map((r, i) => `<div class="live-row${i < (newCount || 0) ? " flash" : ""}">${liveRowHtml(r)}</div>`)
      .join("");
    feed.scrollTop = 0;

    // 汇总：最近 60 秒
    const now = Date.now();
    let n = 0, inp = 0, rd = 0, wr = 0, out = 0;
    for (const r of liveRecords) {
      const ts = Date.parse(r.timestamp);
      if (isNaN(ts) || now - ts > 300000) continue;
      n++;
      inp += r.input_tokens || 0; rd += r.cache_read_tokens || 0;
      wr += r.cache_write_tokens || 0; out += r.output_tokens || 0;
    }
    if (n > 0) {
      const hr = rd + inp > 0 ? rd / (rd + inp) : 0;
      $("liveSum").textContent = `近5min：${n} 次 · ${fmt(inp + rd + wr + out)} tok · 命中 ${(hr * 100).toFixed(0)}%`;
    } else {
      const last = liveRecords[liveRecords.length - 1];
      $("liveSum").textContent = `最近一次 ${String(last.timestamp || "").slice(11, 19)}`;
    }
    drawSpark();
  }

  // 迷你条形图：最近 40 次请求，高度=token 量(对数)，颜色=命中率档位
  function drawSpark() {
    const cv = $("liveSpark");
    const rect = cv.getBoundingClientRect();
    const dpr = window.devicePixelRatio || 1;
    const W = Math.max(1, Math.round(rect.width)) || 360;
    const H = Math.max(1, Math.round(rect.height)) || 44;
    cv.width = W * dpr; cv.height = H * dpr;
    const ctx = cv.getContext("2d");
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, W, H);
    const rows = liveRecords.slice(-40);
    if (!rows.length) {
      ctx.fillStyle = "#c2c8d2";
      ctx.textAlign = "center";
      ctx.textBaseline = "middle";
      ctx.font = "11px -apple-system, sans-serif";
      ctx.fillText("暂无请求", W / 2, H / 2);
      return;
    }
    const vals = rows.map((r) => (r.input_tokens || 0) + (r.cache_read_tokens || 0) + (r.cache_write_tokens || 0) + (r.output_tokens || 0));
    const logs = vals.map((v) => Math.log10(Math.max(v, 1)));
    const maxL = Math.max(...logs, 1);
    const gap = 2;
    const bw = Math.max(3, Math.min(9, (W - gap * rows.length) / rows.length));
    const totalW = rows.length * (bw + gap) - gap;
    let x = W - totalW; // 靠右排列，新数据在右侧
    const COLOR = { good: "#1fab6b", mid: "#f5a623", bad: "#c3cad6" };
    rows.forEach((r, i) => {
      const h = Math.max(2, (logs[i] / maxL) * (H - 4));
      ctx.fillStyle = COLOR[liveHitCls(r)];
      ctx.beginPath();
      ctx.roundRect(x, H - h, bw, h, 1.5);
      ctx.fill();
      x += bw + gap;
    });
  }

  async function seedLive() {
    try {
      const res = await window.api.getLive();
      liveRecords = (res && res.records) || [];
      if (liveRecords.length > LIVE_KEEP) liveRecords = liveRecords.slice(-LIVE_KEEP);
      renderLive(0);
    } catch (_) {}
  }

  window.api.onLiveRecords((recs) => {
    if (!Array.isArray(recs) || !recs.length) return;
    liveRecords.push(...recs);
    if (liveRecords.length > LIVE_KEEP) liveRecords = liveRecords.slice(-LIVE_KEEP);
    renderLive(recs.length);
    loadStats(); // 有新请求时顺带刷新汇总统计
  });

  $("cfgLockStopBtn").addEventListener("click", async () => {
    await doToggle(false);
    toast("代理已停止，现在可以编辑配置");
  });

  // ---------- 工具 ----------
  function toast(text) {
    const t = $("toast");
    t.textContent = text;
    t.classList.add("show");
    setTimeout(() => t.classList.remove("show"), 2200);
  }

  window.api.onStatus((s) => {
    const wasRunning = status.running;
    status = s;
    renderStatus();
    if (wasRunning && !s.running && s.error) toast(s.error);
    if (!wasRunning && s.running) seedLive(); // 刚启动：加载实时缓冲
  });
  // 每次面板弹出时刷新
  window.api.onPanelShown(() => {
    refreshStatus(); loadStats(); seedLive(); if (!status.running) loadConfig();
    // 面板从隐藏切到显示后，canvas 尺寸可能还没 layout 完，等一帧再重绘
    requestAnimationFrame(() => requestAnimationFrame(() => { drawCharts(); drawSpark(); }));
  });

  // Esc 收起面板
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape") window.api.hidePanel();
  });

  // ---------- 启动 ----------
  async function init() {
    await refreshStatus();
    await loadConfig();
    fetchModels(false); // 预拉模型列表（不阻塞）
    await loadStats();
    seedLive();
    setInterval(() => { if (document.visibilityState === "visible") loadStats(); }, 5000);
    setInterval(() => { if (document.visibilityState === "visible") refreshStatus(); }, 3000);
  }
  init();
})();
