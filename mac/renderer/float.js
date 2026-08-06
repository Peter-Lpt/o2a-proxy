/* o2a-proxy 悬浮看板 —— 实时小窗逻辑（每个服务独立小窗）
   渲染逻辑复用 live.js（与主面板同一事实来源），保证显示一致。 */
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
    return list.find((x) => x.name === svc) || { running: false, port: "", model: "", mode: "" };
  }

  function renderHead() {
    $("dot").classList.toggle("on", !!svcStatus.running);
    $("sub").textContent = svcStatus.running
      ? `:${svcStatus.port} · ${svcStatus.model}`
      : "已停止";
  }

  function render(newCount) {
    // 近 5 分钟汇总（与主面板同一套逻辑）
    const s = Live.summarize(records);
    $("nReq").textContent = s.n ? String(s.n) : "0";
    $("nTok").textContent = s.n ? Live.fmt(s.tokens) : "0";
    $("nHit").textContent = s.n ? (s.hitRate * 100).toFixed(0) + "%" : "—";
    $("nHit").className = Live.hitCls(s.hitRate);

    // 流水（最近 8 条）
    const feed = $("feed");
    if (!records.length) {
      feed.innerHTML = '<div class="live-empty">等待请求…</div>';
    } else {
      feed.innerHTML = records.slice(-8).reverse().map((r, i) =>
        `<div class="live-row${i < (newCount || 0) ? " flash" : ""}">${Live.rowHtml(r)}</div>`
      ).join("");
      feed.scrollTop = 0;
    }
    Live.drawSpark($("spark"), records, { maxBars: 36, width: 260, height: 40, emptyText: "暂无请求" });
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