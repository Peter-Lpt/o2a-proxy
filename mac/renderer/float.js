/* o2a-proxy 悬浮看板 —— 实时小窗逻辑 */
(function () {
  const $ = (id) => document.getElementById(id);
  const KEEP = 80;
  let records = [];
  let status = { running: false, port: "", model: "" };

  function fmt(n) {
    n = Number(n) || 0;
    if (n >= 1e9) return (n / 1e9).toFixed(2) + "B";
    if (n >= 1e6) return (n / 1e6).toFixed(2) + "M";
    if (n >= 1e3) return (n / 1e3).toFixed(1) + "K";
    return String(Math.round(n));
  }

  function hitCls(r) {
    const rate = Number(r.cache_hit_rate) || 0;
    return rate >= 0.6 ? "good" : rate > 0.15 ? "mid" : "bad";
  }

  function renderHead() {
    $("dot").classList.toggle("on", !!status.running);
    $("sub").textContent = status.running
      ? `:${status.port} · ${status.model}`
      : "已停止";
  }

  function render(newCount) {
    // 近 5 分钟汇总
    const now = Date.now();
    let n = 0, inp = 0, rd = 0, wr = 0, out = 0;
    for (const r of records) {
      const ts = Date.parse(r.timestamp);
      if (isNaN(ts) || now - ts > 300000) continue;
      n++;
      inp += r.input_tokens || 0; rd += r.cache_read_tokens || 0;
      wr += r.cache_write_tokens || 0; out += r.output_tokens || 0;
    }
    $("nReq").textContent = n ? String(n) : "0";
    $("nTok").textContent = n ? fmt(inp + rd + wr + out) : "0";

    // 流水（最近 8 条）
    const feed = $("feed");
    if (!records.length) {
      feed.innerHTML = '<div class="empty">等待请求…</div>';
    } else {
      feed.innerHTML = records.slice(-8).reverse().map((r, i) => {
        const t = String(r.timestamp || "").slice(11, 19) || "--:--:--";
        const total = (r.input_tokens || 0) + (r.cache_read_tokens || 0) + (r.cache_write_tokens || 0);
        const rate = ((Number(r.cache_hit_rate) || 0) * 100).toFixed(0);
        return `<div class="row${i < (newCount || 0) ? " flash" : ""}">` +
          `<span class="t">${t}</span>` +
          `<span class="k">↑${fmt(total)} · ↓${fmt(r.output_tokens || 0)}</span>` +
          `<span class="h ${hitCls(r)}">${rate}%</span></div>`;
      }).join("");
      feed.scrollTop = 0;
    }
    drawSpark();
  }

  function drawSpark() {
    const cv = $("spark");
    const rect = cv.getBoundingClientRect();
    const dpr = window.devicePixelRatio || 1;
    const W = Math.max(1, Math.round(rect.width)) || 260;
    const H = Math.max(1, Math.round(rect.height)) || 40;
    cv.width = W * dpr; cv.height = H * dpr;
    const ctx = cv.getContext("2d");
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, W, H);
    const rows = records.slice(-36);
    if (!rows.length) {
      ctx.fillStyle = "#c2c8d2";
      ctx.textAlign = "center"; ctx.textBaseline = "middle";
      ctx.font = "10px -apple-system, sans-serif";
      ctx.fillText("暂无请求", W / 2, H / 2);
      return;
    }
    const vals = rows.map((r) => (r.input_tokens || 0) + (r.cache_read_tokens || 0) + (r.cache_write_tokens || 0) + (r.output_tokens || 0));
    const logs = vals.map((v) => Math.log10(Math.max(v, 1)));
    const maxL = Math.max(...logs, 1);
    const gap = 2;
    const bw = Math.max(3, Math.min(8, (W - gap * rows.length) / rows.length));
    let x = W - (rows.length * (bw + gap) - gap);
    const COLOR = { good: "#1fab6b", mid: "#f5a623", bad: "#c3cad6" };
    rows.forEach((r, i) => {
      const h = Math.max(2, (logs[i] / maxL) * (H - 3));
      ctx.fillStyle = COLOR[hitCls(r)];
      ctx.beginPath();
      ctx.roundRect(x, H - h, bw, h, 1.5);
      ctx.fill();
      x += bw + gap;
    });
  }

  async function loadTodayHit() {
    try {
      const s = await window.api.getStats();
      if (s && s.today) $("nHit").textContent = ((s.today.hitRate || 0) * 100).toFixed(1) + "%";
    } catch (_) {}
  }

  async function seed() {
    try {
      const res = await window.api.getLive();
      records = ((res && res.records) || []).slice(-KEEP);
      render(0);
    } catch (_) {}
    loadTodayHit();
  }

  window.api.onLiveRecords((recs) => {
    if (!Array.isArray(recs) || !recs.length) return;
    records.push(...recs);
    if (records.length > KEEP) records = records.slice(-KEEP);
    render(recs.length);
    loadTodayHit();
  });

  window.api.onStatus((s) => { status = s; renderHead(); });
  window.api.onPanelShown(() => { seed(); refreshStatus(); });

  $("closeBtn").addEventListener("click", () => window.api.toggleFloat(false));

  async function refreshStatus() {
    try { status = await window.api.getStatus(); } catch (_) {}
    renderHead();
  }

  // 近60s汇总需要随时间衰减，定期重算
  setInterval(() => { if (document.visibilityState === "visible") render(0); }, 5000);
  setInterval(() => { if (document.visibilityState === "visible") refreshStatus(); }, 5000);

  refreshStatus();
  seed();
})();
