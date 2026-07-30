/* 纯 Canvas 图表库 —— 无第三方依赖，离线可用。 */
(function () {
  function setupCanvas(canvas) {
    const dpr = window.devicePixelRatio || 1;
    const rect = canvas.getBoundingClientRect();
    const w = rect.width || canvas.clientWidth || 400;
    const h = rect.height || canvas.clientHeight || 220;
    canvas.width = Math.round(w * dpr);
    canvas.height = Math.round(h * dpr);
    const ctx = canvas.getContext("2d");
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, w, h);
    return { ctx, w, h };
  }

  function fmtNum(n) {
    n = Number(n) || 0;
    if (n >= 1e9) return (n / 1e9).toFixed(2) + "B";
    if (n >= 1e6) return (n / 1e6).toFixed(2) + "M";
    if (n >= 1e3) return (n / 1e3).toFixed(1) + "K";
    return String(Math.round(n));
  }
  function fmtComma(n) {
    return Math.round(Number(n) || 0).toLocaleString("en-US");
  }
  function fmtPct(n) { return (Number(n) * 100).toFixed(1) + "%"; }

  function niceMax(v) {
    if (v <= 0) return 1;
    const pow = Math.pow(10, Math.floor(Math.log10(v)));
    const f = v / pow;
    let nf;
    if (f <= 1) nf = 1; else if (f <= 2) nf = 2; else if (f <= 5) nf = 5; else nf = 10;
    return nf * pow;
  }

  // 根据可用宽度计算不重叠的 x 轴标签步长
  function labelStep(ctx, labels, plotW, spacing) {
    if (!labels.length) return 1;
    spacing = spacing || 44;
    const avgW = labels.reduce((s, t) => s + ctx.measureText(String(t)).width, 0) / labels.length;
    const maxFit = Math.max(2, Math.floor(plotW / (avgW + spacing)));
    return Math.max(1, Math.ceil(labels.length / maxFit));
  }

  function drawLineChart(canvas, opts) {
    const { ctx, w, h } = setupCanvas(canvas);
    const data = opts.data || [];
    const labels = opts.labels || data.map((_, i) => i);
    const color = opts.color || "#2d7ff9";
    const yFmt = opts.yFmt || fmtNum;
    const padL = 52, padR = 14, padT = 12, padB = 26;
    const plotW = w - padL - padR;
    const plotH = h - padT - padB;

    let maxV = 0;
    for (const d of data) maxV = Math.max(maxV, d);
    maxV = niceMax(maxV || 1);

    // 网格 + y 轴
    ctx.strokeStyle = "#eef1f5";
    ctx.fillStyle = "#9aa3b2";
    ctx.font = "11px -apple-system, sans-serif";
    ctx.textAlign = "right";
    ctx.textBaseline = "middle";
    const ticks = 4;
    for (let i = 0; i <= ticks; i++) {
      const y = padT + (plotH * i) / ticks;
      const val = maxV * (1 - i / ticks);
      ctx.beginPath();
      ctx.moveTo(padL, y);
      ctx.lineTo(padL + plotW, y);
      ctx.stroke();
      ctx.fillText(yFmt(val), padL - 6, y);
    }

    if (data.length === 0) {
      ctx.fillStyle = "#c2c8d2";
      ctx.textAlign = "center";
      ctx.fillText("暂无数据", w / 2, h / 2);
      return;
    }

    const x = (i) => padL + (plotW * i) / Math.max(1, data.length - 1);
    const y = (v) => padT + plotH * (1 - Math.min(v, maxV) / maxV);

    // 面积填充
    ctx.beginPath();
    ctx.moveTo(x(0), y(data[0]));
    for (let i = 1; i < data.length; i++) ctx.lineTo(x(i), y(data[i]));
    ctx.lineTo(x(data.length - 1), padT + plotH);
    ctx.lineTo(x(0), padT + plotH);
    ctx.closePath();
    const grad = ctx.createLinearGradient(0, padT, 0, padT + plotH);
    grad.addColorStop(0, hexA(color, 0.22));
    grad.addColorStop(1, hexA(color, 0.02));
    ctx.fillStyle = grad;
    ctx.fill();

    // 线
    ctx.beginPath();
    ctx.moveTo(x(0), y(data[0]));
    for (let i = 1; i < data.length; i++) ctx.lineTo(x(i), y(data[i]));
    ctx.strokeStyle = color;
    ctx.lineWidth = 2;
    ctx.lineJoin = "round";
    ctx.stroke();

    // x 轴标签（按宽度稀疏，避免重叠）
    ctx.fillStyle = "#9aa3b2";
    ctx.textAlign = "center";
    ctx.textBaseline = "top";
    const step = labelStep(ctx, labels, plotW);
    for (let i = 0; i < data.length; i += step) {
      ctx.fillText(String(labels[i]), x(i), padT + plotH + 6);
    }
    if ((data.length - 1) % step !== 0) {
      ctx.fillText(String(labels[data.length - 1]), x(data.length - 1), padT + plotH + 6);
    }
  }

  function drawStackedBarChart(canvas, opts) {
    const { ctx, w, h } = setupCanvas(canvas);
    const labels = opts.labels || [];
    const series = opts.series || []; // [{name,color,data:[]}]
    const yFmt = opts.yFmt || fmtNum;
    const padL = 52, padR = 14, padT = 12, padB = 26;
    const plotW = w - padL - padR;
    const plotH = h - padT - padB;

    let maxV = 0;
    for (let i = 0; i < labels.length; i++) {
      let s = 0;
      for (const ser of series) s += ser.data[i] || 0;
      maxV = Math.max(maxV, s);
    }
    maxV = niceMax(maxV || 1);

    // 网格 + y 轴
    ctx.strokeStyle = "#eef1f5";
    ctx.fillStyle = "#9aa3b2";
    ctx.font = "11px -apple-system, sans-serif";
    ctx.textAlign = "right";
    ctx.textBaseline = "middle";
    const ticks = 4;
    for (let i = 0; i <= ticks; i++) {
      const y = padT + (plotH * i) / ticks;
      const val = maxV * (1 - i / ticks);
      ctx.beginPath();
      ctx.moveTo(padL, y);
      ctx.lineTo(padL + plotW, y);
      ctx.stroke();
      ctx.fillText(yFmt(val), padL - 6, y);
    }

    if (labels.length === 0) {
      ctx.fillStyle = "#c2c8d2"; ctx.textAlign = "center";
      ctx.fillText("暂无数据", w / 2, h / 2);
      return;
    }

    const n = labels.length;
    const slot = plotW / n;
    const bw = Math.min(20, slot * 0.66);

    for (let i = 0; i < n; i++) {
      const cx = padL + slot * i + slot / 2;
      let base = padT + plotH;
      for (const ser of series) {
        const v = ser.data[i] || 0;
        if (v <= 0) continue;
        const bh = plotH * (v / maxV);
        ctx.fillStyle = ser.color;
        ctx.fillRect(cx - bw / 2, base - bh, bw, bh);
        base -= bh;
      }
      // x 标签（按宽度稀疏，避免重叠）
      ctx.fillStyle = "#9aa3b2";
      ctx.textAlign = "center";
      ctx.textBaseline = "top";
      const step = labelStep(ctx, labels, plotW);
      if (i % step === 0 || i === n - 1) {
        ctx.fillText(String(labels[i]), cx, padT + plotH + 6);
      }
    }

    // 图例
    if (opts.legend !== false) {
      let lx = padL;
      const ly = 2;
      ctx.textAlign = "left";
      ctx.textBaseline = "middle";
      for (const ser of series) {
        ctx.fillStyle = ser.color;
        ctx.fillRect(lx, ly, 10, 10);
        ctx.fillStyle = "#5b6472";
        ctx.fillText(ser.name, lx + 14, ly + 5);
        lx += 14 + ctx.measureText(ser.name).width + 18;
      }
    }
  }

  function hexA(hex, a) {
    if (hex[0] !== "#") return hex;
    const r = parseInt(hex.slice(1, 3), 16);
    const g = parseInt(hex.slice(3, 5), 16);
    const b = parseInt(hex.slice(5, 7), 16);
    return `rgba(${r},${g},${b},${a})`;
  }

  function drawCombinedChart(canvas, opts) {
    const { ctx, w, h } = setupCanvas(canvas);
    const labels = opts.labels || [];
    const series = opts.series || [];
    const hitData = opts.hitData || [];
    const yFmt = opts.yFmt || fmtNum;
    const padL = 52, padR = 42, padT = 14, padB = 26;
    const plotW = w - padL - padR;
    const plotH = h - padT - padB;

    // viewport state (stored on canvas element)
    if (canvas._vp == null) canvas._vp = { zoom: 1, pan: 0 };
    const vp = canvas._vp;
    const n = labels.length;
    const visCount = Math.max(3, Math.min(n, Math.round(n / vp.zoom)));
    const maxPan = Math.max(0, n - visCount);
    vp.pan = Math.max(0, Math.min(maxPan, Math.round(vp.pan)));
    const si = vp.pan;
    const ei = Math.min(n, si + visCount);
    const visLabels = labels.slice(si, ei);

    // y-scale left (tokens)
    let maxTok = 0;
    for (let i = si; i < ei; i++) {
      let s = 0;
      for (const ser of series) s += ser.data[i] || 0;
      maxTok = Math.max(maxTok, s);
    }
    maxTok = niceMax(maxTok || 1);

    // grid + left y-axis
    ctx.strokeStyle = "#eef1f5";
    ctx.fillStyle = "#9aa3b2";
    ctx.font = "11px -apple-system, sans-serif";
    ctx.textAlign = "right";
    ctx.textBaseline = "middle";
    const ticks = 4;
    for (let i = 0; i <= ticks; i++) {
      const y = padT + (plotH * i) / ticks;
      ctx.beginPath();
      ctx.moveTo(padL, y);
      ctx.lineTo(padL + plotW, y);
      ctx.stroke();
      ctx.fillText(yFmt(maxTok * (1 - i / ticks)), padL - 6, y);
    }

    // right y-axis (hit rate %)
    ctx.textAlign = "left";
    for (let i = 0; i <= ticks; i++) {
      const y = padT + (plotH * i) / ticks;
      ctx.fillText(((1 - i / ticks) * 100).toFixed(0) + "%", padL + plotW + 6, y);
    }

    if (n === 0) {
      ctx.fillStyle = "#c2c8d2";
      ctx.textAlign = "center";
      ctx.textBaseline = "middle";
      ctx.fillText("暂无数据", w / 2, h / 2);
      return;
    }

    const slot = plotW / visLabels.length;
    const bw = Math.min(22, slot * 0.6);
    const xCenter = (vi) => padL + slot * (vi - si) + slot / 2;

    // bars
    for (let vi = si; vi < ei; vi++) {
      const cx = xCenter(vi);
      let base = padT + plotH;
      for (const ser of series) {
        const v = ser.data[vi] || 0;
        if (v <= 0) continue;
        const bh = plotH * (v / maxTok);
        ctx.fillStyle = ser.color;
        ctx.fillRect(cx - bw / 2, base - bh, bw, bh);
        base -= bh;
      }
    }

    // hit rate line
    const pts = [];
    for (let vi = si; vi < ei; vi++) {
      if (hitData[vi] != null && hitData[vi] > 0) {
        pts.push({ x: xCenter(vi), y: padT + plotH * (1 - hitData[vi]) });
      }
    }
    if (pts.length > 1) {
      ctx.beginPath();
      ctx.moveTo(pts[0].x, pts[0].y);
      for (let i = 1; i < pts.length; i++) ctx.lineTo(pts[i].x, pts[i].y);
      ctx.strokeStyle = "rgba(31,171,107,0.45)";
      ctx.lineWidth = 2;
      ctx.lineJoin = "round";
      ctx.stroke();
    }
    for (const p of pts) {
      ctx.beginPath();
      ctx.arc(p.x, p.y, 3.5, 0, Math.PI * 2);
      ctx.fillStyle = "#1fab6b";
      ctx.fill();
      ctx.strokeStyle = "#fff";
      ctx.lineWidth = 1.5;
      ctx.stroke();
    }

    // x labels
    ctx.fillStyle = "#9aa3b2";
    ctx.textAlign = "center";
    ctx.textBaseline = "top";
    const lStep = labelStep(ctx, visLabels, plotW);
    for (let i = 0; i < visLabels.length; i += lStep) {
      ctx.fillText(String(visLabels[i]), xCenter(si + i), padT + plotH + 6);
    }
    if ((visLabels.length - 1) % lStep !== 0) {
      ctx.fillText(String(visLabels[visLabels.length - 1]), xCenter(ei - 1), padT + plotH + 6);
    }

    // legend (series + hit rate line)
    let lx = padL;
    const ly = 2;
    ctx.textAlign = "left";
    ctx.textBaseline = "middle";
    for (const ser of series) {
      ctx.fillStyle = ser.color;
      ctx.fillRect(lx, ly, 10, 10);
      ctx.fillStyle = "#5b6472";
      ctx.fillText(ser.name, lx + 14, ly + 5);
      lx += 14 + ctx.measureText(ser.name).width + 14;
    }
    // hit rate legend item
    ctx.beginPath();
    ctx.arc(lx + 4, ly + 5, 3.5, 0, Math.PI * 2);
    ctx.fillStyle = "#1fab6b";
    ctx.fill();
    ctx.fillStyle = "#5b6472";
    ctx.fillText("命中率", lx + 12, ly + 5);

    // store bar metadata for tooltip hit-testing
    canvas._barMeta = { padL, padT, plotW, plotH, slot, si, ei, series, hitData, labels };
  }

  function setupChartZoomPan(canvas) {
    if (canvas._zpBound) return;
    canvas._zpBound = true;

    // create tooltip element
    const tip = document.createElement("div");
    tip.className = "chart-tip";
    tip.style.display = "none";
    document.body.appendChild(tip);

    canvas.addEventListener("wheel", (e) => {
      e.preventDefault();
      if (canvas._vp == null) canvas._vp = { zoom: 1, pan: 0 };
      const vp = canvas._vp;
      const rect = canvas.getBoundingClientRect();
      const mx = e.clientX - rect.left;
      const ratio = Math.max(0, Math.min(1, (mx - 52) / (rect.width - 52 - 42)));
      const oldVis = Math.round(canvas._dataLen / vp.zoom);
      const factor = e.deltaY < 0 ? 1.2 : 1 / 1.2;
      vp.zoom = Math.max(1, Math.min(8, vp.zoom * factor));
      const newVis = Math.round(canvas._dataLen / vp.zoom);
      const center = vp.pan + oldVis * ratio;
      vp.pan = Math.round(center - newVis * ratio);
      if (canvas._onViewportChange) canvas._onViewportChange();
    }, { passive: false });

    let dragging = false, dragX = 0, dragPan = 0;
    canvas.addEventListener("mousedown", (e) => {
      dragging = true;
      dragX = e.clientX;
      dragPan = canvas._vp ? canvas._vp.pan : 0;
      canvas.style.cursor = "grabbing";
      tip.style.display = "none";
    });
    window.addEventListener("mousemove", (e) => {
      if (!dragging) return;
      if (!canvas._vp) return;
      const rect = canvas.getBoundingClientRect();
      const plotW = rect.width - 52 - 42;
      const visCount = Math.max(3, Math.min(canvas._dataLen || 1, Math.round((canvas._dataLen || 1) / canvas._vp.zoom)));
      const slot = plotW / visCount;
      const dx = dragX - e.clientX;
      canvas._vp.pan = Math.round(dragPan + dx / slot);
      if (canvas._onViewportChange) canvas._onViewportChange();
    });
    window.addEventListener("mouseup", () => {
      if (dragging) {
        dragging = false;
        canvas.style.cursor = "";
      }
    });
    canvas.addEventListener("dblclick", () => {
      if (canvas._vp) { canvas._vp.zoom = 1; canvas._vp.pan = 0; }
      if (canvas._onViewportChange) canvas._onViewportChange();
    });

    // tooltip on hover
    canvas.addEventListener("mousemove", (e) => {
      if (dragging) { tip.style.display = "none"; return; }
      const meta = canvas._barMeta;
      if (!meta) { tip.style.display = "none"; return; }
      const rect = canvas.getBoundingClientRect();
      const mx = e.clientX - rect.left;
      const my = e.clientY - rect.top;
      const { padL, padT, plotW, plotH, slot, si, ei, series, hitData, labels } = meta;
      // check if in plot area
      if (mx < padL || mx > padL + plotW || my < padT || my > padT + plotH) {
        tip.style.display = "none";
        return;
      }
      // determine which bar
      const relX = mx - padL;
      const idx = Math.floor(relX / slot);
      const dataIdx = si + idx;
      if (dataIdx < si || dataIdx >= ei) {
        tip.style.display = "none";
        return;
      }
      // build tooltip content
      let html = `<div class="tip-label">${labels[dataIdx]}</div>`;
      let total = 0;
      for (const ser of series) {
        const v = ser.data[dataIdx] || 0;
        total += v;
        html += `<div class="tip-row"><span class="tip-dot" style="background:${ser.color}"></span>${ser.name}<span class="tip-val">${fmtComma(v)}</span></div>`;
      }
      const hit = hitData[dataIdx];
      if (hit != null && hit > 0) {
        html += `<div class="tip-row"><span class="tip-dot" style="background:#1fab6b"></span>命中率<span class="tip-val">${fmtPct(hit)}</span></div>`;
      }
      html += `<div class="tip-total">总计 <span>${fmtComma(total)}</span></div>`;
      tip.innerHTML = html;
      tip.style.display = "block";
      // position tooltip
      const tipRect = tip.getBoundingClientRect();
      let tx = e.clientX + 12;
      let ty = e.clientY - tipRect.height - 8;
      if (tx + tipRect.width > window.innerWidth - 8) tx = e.clientX - tipRect.width - 12;
      if (ty < 8) ty = e.clientY + 16;
      tip.style.left = tx + "px";
      tip.style.top = ty + "px";
    });
    canvas.addEventListener("mouseleave", () => {
      tip.style.display = "none";
    });
  }

  window.Charts = { drawLineChart, drawStackedBarChart, drawCombinedChart, setupChartZoomPan, fmtNum, fmtPct };
})();
