/* o2a-proxy 菜单栏面板 —— 渲染层逻辑 */
(function () {
  const $ = (id) => document.getElementById(id);
  const fmt = (n) => (window.Charts ? Charts.fmtNum(n) : String(n));
  const pct = (n) => (window.Charts ? Charts.fmtPct(n) : (Number(n) * 100).toFixed(1) + "%");

  let status = { running: false, error: "", services: [], ports: [] };
  let activeIdx = -1; // -1 = 全部(总览)；否则为服务下标
  let configCache = null;
  let liveService = ""; // 实时调用要显示的服务名；""=全部(总览)
  let statsCache = null;
  let range = "today"; // today | month
  let selectedModel = "__all__"; // 模型过滤，”__all__“ = 全部
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

  // ---------- 状态渲染（按当前选中的服务；-1=全部总览） ----------
  function activeInfo() {
    if (activeIdx === -1) {
      const running = (status.services || []).filter((s) => s.running);
      return {
        name: "", mode: "all", all: true, running: running.length > 0,
        runningCount: running.length,
        runningPorts: running.map((s) => `${s.port}(${s.mode})`).join(" · "),
        port: "", model: "", proxiable: false,
      };
    }
    const list = status.services || [];
    const i = Math.min(activeIdx, Math.max(0, list.length - 1));
    return list[i] || { name: "", mode: "", port: "", model: "", running: false, proxiable: false };
  }

  function renderSvcTabs() {
    const el = $("svcTabs");
    if (!el) return;
    const services = (configCache && configCache.services) || [];
    const runningMap = {};
    (status.services || []).forEach((s) => { runningMap[s.name] = s.running; });
    const tabs = [
      `<button class="svc-tab all-tab${activeIdx === -1 ? " active" : ""}" data-idx="-1">
        <span class="svc-tab-name">总览</span>
      </button>`,
    ];
    services.forEach((s, i) => {
      const running = !!runningMap[s.comment] || !!runningMap[s.name];
      const name = s.comment || s.name;
      const cls = `svc-tab${i === activeIdx ? " active" : ""}${running ? " running" : ""}`;
      tabs.push(`<button class="${cls}" data-idx="${i}" data-name="${esc(name)}">
        <span class="svc-tab-name">${esc(name)}</span>
        <span class="svc-tab-toggle ${running ? "on" : ""}" title="${running ? "停止" : "启动"}"></span>
        <span class="svc-tab-del" title="删除此服务">×</span>
      </button>`);
    });
    el.innerHTML = tabs.join("");
    el.querySelectorAll(".svc-tab").forEach((b) => {
      b.addEventListener("click", (e) => {
        const idx = Number(b.dataset.idx);
        const name = b.dataset.name;
        if (e.target.classList.contains("svc-tab-toggle")) {
          doToggleService(name, !runningMap[name]);
          return;
        }
        if (e.target.classList.contains("svc-tab-del")) {
          doRemoveService(name);
          return;
        }
        activeIdx = idx;
        renderSvcTabs();
        renderStatus();
        loadStats();
        loadConfig();
      });
    });
  }

  function renderStatus() {
    renderSvcTabs();
    refreshFloatBtn();
    const svc = activeInfo();
    const on = !!svc.running;
    $("logo").classList.toggle("on", on);
    const sub = $("headSub");
    if (on) sub.textContent = svc.all ? `运行中 · ${svc.runningCount} 个服务` : `运行中 · 127.0.0.1:${svc.port}`;
    else if (svc.all) sub.textContent = "总览 · 全部服务";
    else if (!svc.name) sub.textContent = "未配置";
    else sub.textContent = "代理未启动";
    sub.classList.toggle("on", on);

    $("offHero").style.display = on ? "none" : "";
    $("onBar").style.display = on ? "" : "none";
    if (on) {
      $("onBarText").textContent = svc.all ? `运行中 · ${svc.runningPorts}` : `运行中 · 127.0.0.1:${svc.port} · ${svc.model}`;
    } else if (svc.all) {
      $("offMeta").textContent = "总览 · 全部服务统计（含历史数据）";
      $("offErr").textContent = "";
    } else if (svc.name) {
      $("offMeta").textContent = `代理未启动 · 端口 :${svc.port} · ${svc.model}`;
      $("offErr").textContent = status.error || "";
    } else {
      $("offMeta").textContent = "未配置服务";
      $("offErr").textContent = status.error || "";
    }
    $("footStatus").textContent = on ? `代理运行中 · ${svc.all ? svc.runningPorts : svc.port}` : "在顶部服务标签上点击开关启动";
    // 统计范围标注：切换服务时同步切换统计
    $("statsScope").textContent = svc.all ? "全部" : (svc.name || "—");
    // 实时调用按当前服务过滤
    const newLiveService = svc.all ? "" : (svc.name || "");
    if (newLiveService !== liveService) { liveService = newLiveService; renderLive(0); }

    // 实时区块：任一服务运行中显示
    const anyOn = !!status.running;
    const wasHidden = $("liveCard").style.display === "none";
    $("liveCard").style.display = anyOn ? "" : "none";
    if (on && wasHidden) requestAnimationFrame(() => requestAnimationFrame(drawSpark));
    // 配置仅当“当前选中服务”运行中时锁定；总览不显示配置
    if (svc.all) {
      $("cfgLock").style.display = "none";
      $("configForm").style.display = "none";
    } else {
      $("configForm").style.display = "";
      const locked = !!svc.running;
      $("cfgLock").style.display = locked ? "" : "none";
      $("cfgLockText").textContent = `🔒 「${svc.name}」运行中，停止后可编辑（其他未运行服务不受影响）`;
      $("cfgLockStopBtn").style.display = locked ? "" : "none";
      $("configForm").classList.toggle("locked", locked);
    }
  }

  async function refreshStatus() {
    try { status = await window.api.getStatus(); } catch (_) {}
    renderStatus();
  }

  // ---------- 启停（按当前选中的服务） ----------
  async function doToggleService(name, targetOn) {
    if (busy) return;
    busy = true;
    try {
      const res = targetOn ? await window.api.startService(name) : await window.api.stopService(name);
      if (res && res.ok === false) toast(res.error || "操作失败");
      // 启动后短暂延迟再取一次状态（进程可能立即退出，如端口占用）
      setTimeout(async () => { await refreshStatus(); }, 1200);
    } finally {
      await refreshStatus();
      busy = false;
    }
  }

  // 每个服务的开关内嵌在顶部服务标签中（svc-tab-toggle）
  $("refreshBtn").addEventListener("click", () => loadStats());
  $("quitBtn").addEventListener("click", () => window.api.quitApp());

  // ---------- 悬浮看板（按当前服务独立小窗） ----------
  function activeName() {
    if (activeIdx === -1) return ""; // 总览无独立悬浮窗
    const services = (configCache && configCache.services) || [];
    const i = Math.min(activeIdx, services.length - 1);
    return services[i] ? (services[i].comment || services[i].name) : "";
  }
  function renderFloatBtn(open) {
    $("floatBtn").classList.toggle("on", !!open);
    $("floatBtn").textContent = open ? "◱ 悬浮中" : "◱ 悬浮";
  }
  function refreshFloatBtn() {
    const name = activeName();
    if (!name) { renderFloatBtn(false); return; }
    window.api.getFloatState(name).then((s) => renderFloatBtn(s && s.open)).catch(() => {});
  }
  $("floatBtn").addEventListener("click", async () => {
    const name = activeName();
    if (!name) return;
    const res = await window.api.toggleFloat(name);
    renderFloatBtn(res && res.open);
  });
  window.api.onFloatState((s) => { if (s && s.name === activeName()) renderFloatBtn(s.open); });
  refreshFloatBtn();

  // ---------- 统计 ----------
  function emptyStats() {
    const z = () => ({ requests: 0, input: 0, read: 0, write: 0, output: 0, cost: 0, hitRate: 0 });
    const pad = (n) => String(n).padStart(2, "0");
    const todayHourly = [];
    for (let h = 0; h < 24; h++) todayHourly.push({ hour: pad(h), ...z() });
    return {
      current: z(), today: z(), month: z(),
      todayHourly, monthDaily: [], todayMinute: [], todayMinuteByModel: {},
      byModel: [], updatedAt: new Date().toISOString(),
    };
  }

  let statsSeq = 0; // 丢弃过期统计响应，避免快速切换服务时旧数据覆盖新数据
  async function loadStats() {
    const seq = ++statsSeq;
    let s;
    const svc = activeInfo();
    const statService = (svc && !svc.all && svc.name) ? svc.name : undefined;
    try { s = await window.api.getStats(statService); } catch (e) { s = { error: e.message }; }
    if (seq !== statsSeq) return; // 已有更新的请求，丢弃过期结果
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
    populateModelFilter();
    drawCharts();
    const t = new Date(s.updatedAt);
    $("updatedAt").textContent = "更新于 " + t.toLocaleTimeString("zh-CN");
  }

  // ---------- 按模型分组统计 ----------
  function hitCls(rate) {
    return rate >= 0.6 ? "good" : rate > 0.15 ? "mid" : "bad";
  }

  function renderModelStats(byModel, title) {
    const card = $("modelStatsCard");
    if (!byModel || !byModel.length) {
      card.style.display = "none";
      return;
    }
    card.style.display = "";
    $("modelStatsTitle").textContent = title || "今日按模型";
    let totalCost = 0, totalReq = 0, totalTok = 0;
    for (const m of byModel) {
      totalCost += m.cost || 0;
      totalReq += m.requests || 0;
      totalTok += (m.input || 0) + (m.read || 0) + (m.write || 0) + (m.output || 0);
    }
    $("modelStatsTotal").textContent = `共 ${byModel.length} 个模型 · ${totalReq} 次 · ${fmt(totalTok)} tok · ¥${totalCost.toFixed(2)}`;

    $("modelStatsList").innerHTML = byModel.map((m) => {
      const rate = m.hitRate || 0;
      const tok = (m.input || 0) + (m.read || 0) + (m.write || 0) + (m.output || 0);
      return `<div class="model-stat-row">
        <div class="model-stat-name">${m.model} <span class="model-stat-badge">${m.requests} 次</span></div>
        <div class="model-stat-cost">¥${(m.cost || 0).toFixed(2)}</div>
        <div class="model-stat-details">
          <span>↑${fmt(tok)}</span>
          <span>读${fmt(m.read || 0)}</span>
          <span>↓${fmt(m.output || 0)}</span>
          <span class="model-stat-hit ${hitCls(rate)}">命中 ${(rate * 100).toFixed(0)}%</span>
        </div>
      </div>`;
    }).join("");
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

  // 模型过滤（今日/本月均生效）
  const modelFilterEl = $("modelFilter");
  if (modelFilterEl) {
    modelFilterEl.addEventListener("change", () => {
      selectedModel = modelFilterEl.value;
      drawCharts();
    });
  }

  function drawCharts() {
    if (!statsCache) return;
    const s = statsCache;
    const isDay = range === "today";
    let rows, labelFn, titlePrefix, gran;
    if (isDay) {
      // 今日：按模型过滤逐分钟数据
      const src = selectedModel === "__all__" ? s.todayMinute : (s.todayMinuteByModel[selectedModel] || []);
      rows = src;
      labelFn = (x) => x.minute.slice(11);
      gran = "逐分钟";
      titlePrefix = "今日" + (selectedModel !== "__all__" ? " · " + selectedModel : "");
    } else {
      // 本月：按模型过滤逐日数据
      const src = selectedModel === "__all__" ? s.monthDaily : (s.monthDailyByModel[selectedModel] || []);
      rows = src;
      labelFn = (x) => x.date.slice(5);
      gran = "逐日";
      titlePrefix = "本月" + (selectedModel !== "__all__" ? " · " + selectedModel : "");
    }
    const labels = rows.map(labelFn);
    const title = `${titlePrefix}缓存命中率 & Token 消耗（${gran}）`;
    $("chartTitle").textContent = title;

    const COL = { read: "#2d7ff9", input: "#9aa3b2", output: "#1fab6b" };
    const series = [
      { name: "输入", color: COL.input, data: rows.map((x) => x.input) },
      { name: "缓存读", color: COL.read, data: rows.map((x) => x.read) },
      { name: "输出", color: COL.output, data: rows.map((x) => x.output) },
    ];
    const hitData = rows.map((x) => x.hitRate);

    // 主图：平滑曲线
    const cv = $("chartCombined");
    cv._dataLen = labels.length;
    Charts.drawCombinedChart(cv, {
      labels, mode: "curve", series, hitData, yFmt: fmt,
    });
    cv._onViewportChange = () => drawCharts();
    Charts.setupChartZoomPan(cv);

    // 按模型分组统计跟随今日/本月切换
    renderModelStats(isDay ? s.byModel : s.monthByModel, isDay ? "今日按模型" : "本月按模型");
  }

  // 模型过滤器（今日/本月均显示，模型来自今日+本月并集）
  function populateModelFilter() {
    const sel = $("modelFilter");
    if (!sel) return;
    const todayModels = statsCache && statsCache.todayMinuteByModel ? Object.keys(statsCache.todayMinuteByModel) : [];
    const monthModels = statsCache && statsCache.monthDailyByModel ? Object.keys(statsCache.monthDailyByModel) : [];
    const models = [...new Set([...todayModels, ...monthModels])];
    const current = selectedModel;
    sel.innerHTML = `<option value="__all__">全部模型</option>` + models
      .map((m) => `<option value="${m}">${m}</option>`)
      .join("");
    sel.value = current;
    // 若当前选中模型已无数据，回退到全部
    if (current !== "__all__" && !models.includes(current)) sel.value = "__all__";
    selectedModel = sel.value;
  }

  // ---------- 配置表单（多服务，模式：claude / codex） ----------
  const MODE_LABEL = {
    claude: "claude（Anthropic 转换）",
    codex: "codex（OpenAI 透传）",
  };

  function esc(s) {
    return String(s == null ? "" : s)
      .replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
  }

  function svcCardHtml(svc, index) {
    const mode = svc.mode || "claude";
    const sub = mode === "claude" ? (svc.sub_model || svc.model || "") : "";
    const port = (mode === "claude" || mode === "codex") ? (svc.listen_address ?? "11011") : "";
    return `
    <div class="svc-card" data-index="${index}">
      <div class="svc-head">
        <span class="svc-title">服务 ${index + 1}</span>
        <span class="svc-mode-wrap">
          <select class="svc-mode">
            ${Object.entries(MODE_LABEL).map(([v, l]) => `<option value="${v}" ${v === mode ? "selected" : ""}>${l}</option>`).join("")}
          </select>
        </span>
      </div>
      <label>备注 <input class="svc-comment" type="text" value="${esc(svc.comment)}" placeholder="如 claude / codex" /></label>
      <label>API 地址 <input class="svc-base" type="text" spellcheck="false" value="${esc(svc.openai_base_url)}" placeholder="https://.../v1/chat/completions" /></label>
      <label class="svc-key-wrap">API Key
        <span class="model-wrap">
          <input class="svc-key" type="password" autocomplete="off" placeholder="sk-..." value="${esc(svc.openai_api_key)}" />
          <button type="button" class="svc-key-toggle mini-btn text">显示</button>
        </span>
      </label>
      <label>模型 <input class="svc-model" type="text" list="modelList" spellcheck="false" autocomplete="off" value="${esc(svc.model || "qwen-plus")}" /></label>
      <label class="svc-submodel-field">子模型 <input class="svc-submodel" type="text" list="modelList" spellcheck="false" autocomplete="off" value="${esc(sub)}" /></label>
      <div class="grid2">
        <label class="svc-port-field">监听端口 <input class="svc-port" type="text" value="${esc(port)}" /></label>
        <label class="svc-ctx-field inline">
          <input class="svc-ctx" type="checkbox" ${svc.context_1m ? "checked" : ""} />
          <span>1M 上下文<small>max_tokens = 1,000,000</small></span>
        </label>
      </div>
    </div>`;
  }

  function applyMode(card) {
    const mode = card.querySelector(".svc-mode").value;
    card.querySelector(".svc-submodel-field").classList.toggle("hidden", mode !== "claude");
    card.querySelector(".svc-ctx-field").classList.toggle("hidden", mode !== "claude");
    card.querySelector(".svc-port-field").classList.remove("hidden");
  }

  function renderActiveConfig() {
    const services = (configCache && configCache.services) || [];
    const box = $("svcSingle");
    if (activeIdx === -1) {
      box.innerHTML = '<div class="hint">总览视图 · 选择顶部某个服务标签以查看/编辑其配置</div>';
      $("activeSvcTitle").textContent = "总览";
      return;
    }
    if (!services.length) {
      box.innerHTML = '<div class="hint">暂无服务，点顶部「＋」添加</div>';
      $("activeSvcTitle").textContent = "";
      return;
    }
    const i = Math.min(activeIdx, services.length - 1);
    activeIdx = i;
    $("activeSvcTitle").textContent = services[i].comment || services[i].name;
    box.innerHTML = svcCardHtml(services[i], i);
    const card = box.querySelector(".svc-card");
    applyMode(card);
    card.querySelector(".svc-mode").addEventListener("change", () => { applyMode(card); fetchModels(false); });
    card.querySelector(".svc-key-toggle").addEventListener("click", (e) => {
      const inp = card.querySelector(".svc-key");
      const show = inp.type === "password";
      inp.type = show ? "text" : "password";
      e.currentTarget.textContent = show ? "隐藏" : "显示";
    });
    card.querySelector(".svc-base").addEventListener("input", debounceFetch);
    card.querySelector(".svc-key").addEventListener("input", debounceFetch);
  }

  function readActiveConfig() {
    const services = (configCache && configCache.services) || [];
    if (!services.length) return;
    const i = Math.min(activeIdx, services.length - 1);
    const card = $("svcSingle").querySelector(".svc-card");
    if (!card) return;
    const mode = card.querySelector(".svc-mode").value;
    const svc = {
      comment: card.querySelector(".svc-comment").value.trim() || `service-${i + 1}`,
      mode: mode,
      openai_base_url: card.querySelector(".svc-base").value.trim(),
      openai_api_key: card.querySelector(".svc-key").value.trim(),
      model: card.querySelector(".svc-model").value.trim() || "qwen-plus",
    };
    if (mode === "claude") {
      svc.sub_model = card.querySelector(".svc-submodel").value.trim() || svc.model;
      svc.context_1m = card.querySelector(".svc-ctx").checked;
    }
    if (mode === "claude" || mode === "codex") {
      svc.listen_address = card.querySelector(".svc-port").value.trim() || "11011";
    }
    services[i] = svc;
    configCache = { ...configCache, services };
  }

  async function loadConfig() {
    configCache = await window.api.getConfig();
    renderActiveConfig();
    renderSvcTabs();
    const f = $("configForm");
    f.auth_token.value = configCache.auth_token ?? "";
    f.cache_stats_enabled.checked = configCache.cache_stats_enabled !== false;
    f.cache_stats_retention_days.value = configCache.cache_stats_retention_days ?? 30;
    f.cache_stats_dir.value = configCache.cache_stats_dir ?? "cache_stats";
    fetchModels(true); // 切到服务时强制按当前服务地址拉取模型列表
  }

  async function persistConfig() {
    await window.api.saveConfig(configCache);
  }

  $("addSvcBtn").addEventListener("click", async () => {
    readActiveConfig();
    const services = (configCache && configCache.services) || [];
    const firstUrl = (services[0] && (services[0].openai_base_url || "").trim()) || "";
    services.push({
      comment: `service-${services.length + 1}`, mode: "claude", model: "qwen-plus",
      sub_model: "qwen-plus", listen_address: "11011", openai_base_url: firstUrl,
      openai_api_key: "", context_1m: false,
    });
    configCache = { ...configCache, services };
    activeIdx = services.length - 1;
    await persistConfig();
    renderActiveConfig();
    renderSvcTabs();
    refreshStatus();
  });

  async function doRemoveService(name) {
    const services = (configCache && configCache.services) || [];
    if (services.length <= 1) { toast("至少保留一个服务"); return; }
    const i = services.findIndex((s) => (s.comment || s.name) === name);
    if (i < 0) { toast("服务不存在"); return; }
    if (status.services && status.services.some((s) => s.name === name && s.running)) {
      toast("该服务正在运行，请先停止再删除");
      return;
    }
    services.splice(i, 1);
    configCache = { ...configCache, services };
    activeIdx = Math.max(-1, Math.min(i, services.length - 1));
    await persistConfig();
    renderActiveConfig();
    renderSvcTabs();
    refreshStatus();
  }

  $("removeSvcBtn").addEventListener("click", () => {
    if (activeIdx === -1) { toast("总览视图，请先选择要删除的服务"); return; }
    const services = (configCache && configCache.services) || [];
    const i = Math.min(activeIdx, services.length - 1);
    doRemoveService(services[i].comment || services[i].name);
  });

  $("configForm").addEventListener("submit", async (e) => {
    e.preventDefault();
    readActiveConfig();
    const f = $("configForm");
    const cfg = {
      auth_token: f.auth_token.value,
      cache_stats_enabled: f.cache_stats_enabled.checked,
      cache_stats_retention_days: Number(f.cache_stats_retention_days.value) || 30,
      cache_stats_dir: f.cache_stats_dir.value || "cache_stats",
      services: (configCache && configCache.services) || [],
    };
    const res = await window.api.saveConfig(cfg);
    if (res.ok) {
      configCache = cfg;
      toast("配置已保存");
      refreshStatus();
      renderSvcTabs();
    } else {
      toast(res.locked ? "⚠️ 代理运行中，请先停止代理再保存" : "保存失败：" + (res.error || ""));
    }
  });

  $("openCfgBtn").addEventListener("click", () => window.api.openConfigFile());

  // ---------- 模型列表拉取（填充全局 datalist） ----------
  const modelCache = {}; // baseUrl -> models[]，跨服务共享（同地址不重复拉取）

  function setModelHint(text, isErr) {
    const h = $("modelHint");
    h.textContent = text || "";
    h.classList.toggle("err", !!isErr);
  }

  function renderModelDatalist(baseUrl) {
    const models = modelCache[baseUrl];
    $("modelList").innerHTML = models.map((m) => `<option value="${esc(m)}"></option>`).join("");
  }

  let modelsSeq = 0; // 丢弃过期模型拉取，避免快速切换服务时旧结果/旧错误覆盖新结果
  async function fetchModels(force) {
    const first = document.querySelector("#svcSingle .svc-card");
    if (!first) return;
    const baseUrl = first.querySelector(".svc-base").value.trim();
    const apiKey = first.querySelector(".svc-key").value.trim();
    if (!baseUrl) { setModelHint("填写当前服务的 API 地址后可拉取模型列表", false); return; }
    // 同地址已有缓存：直接展示（即使该服务未填 Key，也能用同一提供商的模型列表）
    if (modelCache[baseUrl]) {
      renderModelDatalist(baseUrl);
      setModelHint(`已加载 ${modelCache[baseUrl].length} 个模型（${baseUrl}）`, false);
      return;
    }
    if (!apiKey) { setModelHint("填写当前服务的 API Key 后拉取模型列表", false); return; }
    const seq = ++modelsSeq;
    setModelHint("正在拉取模型列表…", false);
    try {
      const res = await window.api.fetchModels({ baseUrl, apiKey });
      if (seq !== modelsSeq) return; // 过期请求
      if (res.ok) {
        modelCache[baseUrl] = res.models;
        renderModelDatalist(baseUrl);
        setModelHint(`已加载 ${res.models.length} 个模型`, false);
      } else {
        setModelHint("该服务拉取模型失败：" + (res.error || ""), true);
      }
    } catch (e) {
      if (seq !== modelsSeq) return;
      setModelHint("该服务拉取模型失败：" + e.message, true);
    }
  }

  // 启动时预热模型缓存：用第一个有 Key 的服务拉取，供同地址的所有服务复用
  async function warmModels() {
    const services = (configCache && configCache.services) || [];
    const first = services.find((s) => (s.openai_base_url || "").trim() && (s.openai_api_key || "").trim());
    if (!first) return;
    const baseUrl = first.openai_base_url.trim();
    if (modelCache[baseUrl]) return;
    const seq = ++modelsSeq;
    try {
      const res = await window.api.fetchModels({ baseUrl, apiKey: first.openai_api_key.trim() });
      if (seq !== modelsSeq) return;
      if (res.ok) modelCache[baseUrl] = res.models;
    } catch (_) {}
  }

  // 地址 / Key 修改后自动重新拉取（防抖）
  let mdTimer = null;
  function debounceFetch() {
    clearTimeout(mdTimer);
    mdTimer = setTimeout(() => fetchModels(false), 900);
  }
  // 切到配置页时自动拉取一次
  document.querySelector('.tab[data-tab="config"]').addEventListener("click", () => fetchModels(false));

  // ---------- 实时调用 ----------
  const LIVE_KEEP = 80;
  let liveRecords = [];

  // 实时记录池：按当前选中服务过滤（总览=全部）
  function livePool() {
    return liveService ? liveRecords.filter((r) => r.service === liveService) : liveRecords;
  }

  function liveHitCls(r) {
    const rate = Number(r.cache_hit_rate) || 0;
    return rate >= 0.6 ? "good" : rate > 0.15 ? "mid" : "bad";
  }

  function liveRowHtml(r) {
    const t = String(r.timestamp || "").slice(11, 19) || "--:--:--";
    const total = (r.input_tokens || 0) + (r.cache_read_tokens || 0) + (r.cache_write_tokens || 0);
    const rate = Number(r.cache_hit_rate) || 0;
    const svcTag = r.service ? `<span class="live-svc">${esc(r.service)}</span>` : "";
    return (
      `<span class="live-time">${t}</span>` +
      svcTag +
      `<span class="live-tok">↑${fmt(total)} · 读${fmt(r.cache_read_tokens || 0)} · ↓${fmt(r.output_tokens || 0)}</span>` +
      `<span class="live-hit ${liveHitCls(r)}">${(rate * 100).toFixed(0)}%</span>`
    );
  }

  function renderLive(newCount) {
    const feed = $("liveFeed");
    const pool = livePool();
    if (!pool.length) {
      feed.innerHTML = liveService
        ? `<div class="live-empty">暂无「${esc(liveService)}」的实时调用</div>`
        : '<div class="live-empty">等待请求…（发起调用后此处实时滚动）</div>';
      $("liveSum").textContent = liveService ? `近5min：0 次` : "";
      drawSpark();
      return;
    }
    // 最新在最上，最多渲染 30 行
    const rows = pool.slice(-30).reverse();
    feed.innerHTML = rows
      .map((r, i) => `<div class="live-row${i < (newCount || 0) ? " flash" : ""}">${liveRowHtml(r)}</div>`)
      .join("");
    feed.scrollTop = 0;

    // 汇总：最近 5 分钟
    const now = Date.now();
    let n = 0, inp = 0, rd = 0, wr = 0, out = 0;
    for (const r of pool) {
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
      const last = pool[pool.length - 1];
      $("liveSum").textContent = `最近一次 ${String(last.timestamp || "").slice(11, 19)}`;
    }
    drawSpark();
  }

  // 迷你条形图：最近 40 次请求，高度=命中率百分比
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
    const rows = livePool().slice(-40);
    if (!rows.length) {
      ctx.fillStyle = "#c2c8d2";
      ctx.textAlign = "center";
      ctx.textBaseline = "middle";
      ctx.font = "11px -apple-system, sans-serif";
      ctx.fillText("暂无请求", W / 2, H / 2);
      return;
    }
    const gap = 2;
    const bw = Math.max(3, Math.min(9, (W - gap * rows.length) / rows.length));
    const totalW = rows.length * (bw + gap) - gap;
    let x = W - totalW;
    const COLOR = { good: "#1fab6b", mid: "#f5a623", bad: "#c3cad6" };
    rows.forEach((r) => {
      const rate = Number(r.cache_hit_rate) || 0;
      const h = Math.max(2, rate * (H - 4));
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
    const name = activeName();
    if (!name) return;
    await window.api.stopService(name);
    setTimeout(async () => { await refreshStatus(); }, 1000);
    toast(`「${name}」已停止，可以编辑配置`);
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
    warmModels(); // 预热模型缓存（不阻塞）
    fetchModels(false); // 预拉模型列表（不阻塞）
    await loadStats();
    seedLive();
    setInterval(() => { if (document.visibilityState === "visible") loadStats(); }, 5000);
    setInterval(() => { if (document.visibilityState === "visible") refreshStatus(); }, 3000);
  }
  init();
})();
