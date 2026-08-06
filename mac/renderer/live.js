/* o2a-proxy —— 实时调用共享逻辑（主面板 / 悬浮窗共用，保证显示一致）
   单一事实来源：两处渲染均调用本模块，避免重复实现导致逻辑漂移、显示不一致。 */
(function () {
  const GOOD = "#1fab6b", MID = "#f5a623", BAD = "#c3cad6";

  function esc(s) {
    return String(s == null ? "" : s)
      .replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
  }

  function fmt(n) {
    n = Number(n) || 0;
    if (n >= 1e9) return (n / 1e9).toFixed(2) + "B";
    if (n >= 1e6) return (n / 1e6).toFixed(2) + "M";
    if (n >= 1e3) return (n / 1e3).toFixed(1) + "K";
    return String(Math.round(n));
  }

  function hitCls(rate) {
    rate = Number(rate) || 0;
    return rate >= 0.6 ? "good" : rate > 0.15 ? "mid" : "bad";
  }

  // 近5分钟汇总（命中率用近5分钟 token 加权，反映当下瞬时命中，而非当日平均）
  function summarize(records) {
    const now = Date.now();
    let n = 0, inp = 0, rd = 0, wr = 0, out = 0;
    for (const r of records || []) {
      const ts = Date.parse(r.timestamp);
      if (isNaN(ts) || now - ts > 300000) continue;
      n++;
      inp += r.input_tokens || 0; rd += r.cache_read_tokens || 0;
      wr += r.cache_write_tokens || 0; out += r.output_tokens || 0;
    }
    const hr = inp + rd > 0 ? rd / (inp + rd) : 0;
    return { n, tokens: inp + rd + wr + out, hitRate: hr, input: inp, read: rd, write: wr, output: out };
  }

  // 单条流水行（时间 + 可选服务标签 + token + 命中率）
  function rowHtml(r, showSvc) {
    const t = String(r.timestamp || "").slice(11, 19) || "--:--:--";
    const total = (r.input_tokens || 0) + (r.cache_read_tokens || 0) + (r.cache_write_tokens || 0);
    const rate = Number(r.cache_hit_rate) || 0;
    const svcTag = showSvc && r.service
      ? `<span class="live-svc">${esc(r.service)}</span>`
      : "";
    return (
      `<span class="live-time">${t}</span>` +
      svcTag +
      `<span class="live-tok">↑${fmt(total)} · 读${fmt(r.cache_read_tokens || 0)} · ↓${fmt(r.output_tokens || 0)}</span>` +
      `<span class="live-hit ${hitCls(rate)}">${(rate * 100).toFixed(0)}%</span>`
    );
  }

  // 迷你条形图：高度 = 命中率百分比
  function drawSpark(canvas, records, opts) {
    if (!canvas) return;
    opts = opts || {};
    const rect = canvas.getBoundingClientRect();
    const dpr = window.devicePixelRatio || 1;
    const W = Math.max(1, Math.round(rect.width)) || (opts.width || 360);
    const H = Math.max(1, Math.round(rect.height)) || (opts.height || 44);
    canvas.width = W * dpr; canvas.height = H * dpr;
    const ctx = canvas.getContext("2d");
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, W, H);
    const rows = (records || []).slice(-(opts.maxBars || 40));
    if (!rows.length) {
      ctx.fillStyle = "#c2c8d2";
      ctx.textAlign = "center"; ctx.textBaseline = "middle";
      ctx.font = (opts.font || "11px") + " -apple-system, sans-serif";
      ctx.fillText(opts.emptyText || "暂无请求", W / 2, H / 2);
      return;
    }
    const gap = 2;
    const bw = Math.max(3, Math.min(opts.maxBarWidth || 9, (W - gap * rows.length) / rows.length));
    const totalW = rows.length * (bw + gap) - gap;
    let x = W - totalW;
    const COLOR = { good: GOOD, mid: MID, bad: BAD };
    const pad = opts.bottomPad || 4;
    rows.forEach((r) => {
      const rate = Number(r.cache_hit_rate) || 0;
      const h = Math.max(2, rate * (H - pad));
      ctx.fillStyle = COLOR[hitCls(rate)];
      ctx.beginPath();
      ctx.roundRect(x, H - h, bw, h, 1.5);
      ctx.fill();
      x += bw + gap;
    });
  }

  window.Live = { fmt, hitCls, summarize, rowHtml, drawSpark };
})();