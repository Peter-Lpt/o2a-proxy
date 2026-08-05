/* o2a-proxy 悬浮看板 —— 实时小窗逻辑（每个服务独立小窗） */
(function () {
  const $ = (id) => document.getElementById(id);
  const KEEP = 80;
  const svc = (new URLSearchParams(location.search).get("svc") || "").trim();
  let records = [];
  let svcStatus = { running: false, port: "", model: "" };

  document.title = svc ? `o2a-proxy · ${svc}` : "o2a-proxy 悬浮看板";
  $("ttl").textContent = svc ? svc : "o2a-proxy";

  function pickStatus(s) {
    const list = (s && s.services) || [];
    const found = list.find((x) => x.name === svc);
    return found || { running: false, port: "", model: "", mode: "" };
  }

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
    $("dot").classList.toggle("on", !!svcStatus.running);
    $("sub").textContent = svcStatus.running
      ? `:${svcStatus.port} · ${svcStatus.model}`
      : "已停止";
  }

  function render(newCount) {
    // 近 5 分钟汇总（命中率用近5分钟 token 加权，反映当下瞬时命中，而非当日平均）
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
    const hr = inp + rd > 0 ? rd / (inp + rd) : 0;
    $("nHit").textContent = n ? (hr * 100).toFixed(0) + "%" : "—";
    $("nHit").className = hr >= 0.6 ? "good" : hr > 0.15 ? "mid" : "bad";

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
    const gap = 2;
    const bw = Math.max(3, Math.min(8, (W - gap * rows.length) / rows.length));
    let x = W - (rows.length * (bw + gap) - gap);
    const COLOR = { good: "#1fab6b", mid: "#f5a623", bad: "#c3cad6" };
    rows.forEach((r) => {
      const rate = Number(r.cache_hit_rate) || 0;
      const h = Math.max(2, rate * (H - 3));
      ctx.fillStyle = COLOR[hitCls(r)];
      ctx.beginPath();
      ctx.roundRect(x, H - h, bw, h, 1.5);
      ctx.fill();
      x += bw + gap;
    });
  }

  async function seed() {
    try {
      const res = await window.api.getLive(svc || undefined);
      records = ((res && res.records) || []).slice(-KEEP);
      render(0);
    } catch (_) {}
  }

  window.api.onLiveRecords((recs) => {
    if (!Array.isArray(recs) || !recs.length) return;
    const mine = svc ? recs.filter((r) => r.service === svc) : recs;
    if (!mine.length) return;
    records.push(...mine);
    if (records.length > KEEP) records = records.slice(-KEEP);
    render(mine.length);
  });

  window.api.onStatus((s) => { svcStatus = pickStatus(s); renderHead(); });
  window.api.onPanelShown(() => { seed(); refreshStatus(); });

  $("closeBtn").addEventListener("click", () => window.api.toggleFloat(svc));

  async function refreshStatus() {
    try { svcStatus = pickStatus(await window.api.getStatus()); } catch (_) {}
    renderHead();
  }

  // 近60s汇总需要随时间衰减，定期重算
  setInterval(() => { if (document.visibilityState === "visible") render(0); }, 5000);
  setInterval(() => { if (document.visibilityState === "visible") refreshStatus(); }, 5000);

  refreshStatus();
  seed();
})();
