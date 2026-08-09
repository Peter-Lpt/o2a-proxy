<template>
  <div class="linechart">
    <canvas ref="el"></canvas>
    <div class="lc-legend">
      <span><i class="dot input"></i>输入</span>
      <span><i class="dot read"></i>缓存读</span>
      <span><i class="dot output"></i>输出</span>
      <span><i class="dot hit"></i>命中率</span>
    </div>
  </div>
</template>

<script setup lang="ts">
import { onMounted, onUnmounted, ref, watch } from "vue";

const props = defineProps<{
  labels: string[];
  input: number[];
  read: number[];
  output: number[];
  hitRate: number[];
  theme?: string;
}>();

const el = ref<HTMLCanvasElement | null>(null);
let DPR = 1;
let W = 0;
let H = 0;
const PADL = 52;
const PADR = 42;
const PADT = 14;
const PADB = 26;

const vp = { zoom: 1, pan: 0 };
let dataLen = 0;

function cssVar(name: string, fb: string): string {
  const v = getComputedStyle(document.documentElement).getPropertyValue(name).trim();
  return v || fb;
}

function fmtNum(n: number): string {
  n = Number(n) || 0;
  if (n >= 1e9) return (n / 1e9).toFixed(2) + "B";
  if (n >= 1e6) return (n / 1e6).toFixed(2) + "M";
  if (n >= 1e3) return (n / 1e3).toFixed(1) + "K";
  return String(Math.round(n));
}
function fmtComma(n: number): string {
  return Math.round(Number(n) || 0).toLocaleString("en-US");
}
function fmtPct(n: number): string {
  return (Number(n) * 100).toFixed(1) + "%";
}

function niceMax(v: number): number {
  if (v <= 0) return 1;
  const pow = Math.pow(10, Math.floor(Math.log10(v)));
  const f = v / pow;
  let nf: number;
  if (f <= 1) nf = 1;
  else if (f <= 1.5) nf = 1.5;
  else if (f <= 2) nf = 2;
  else if (f <= 2.5) nf = 2.5;
  else if (f <= 3) nf = 3;
  else if (f <= 4) nf = 4;
  else if (f <= 5) nf = 5;
  else if (f <= 6) nf = 6;
  else if (f <= 8) nf = 8;
  else nf = 10;
  return nf * pow;
}

function hexA(hex: string, a: number): string {
  if (hex[0] !== "#") return hex;
  const r = parseInt(hex.slice(1, 3), 16);
  const g = parseInt(hex.slice(3, 5), 16);
  const b = parseInt(hex.slice(5, 7), 16);
  return `rgba(${r},${g},${b},${a})`;
}

function tracePath(g: CanvasRenderingContext2D, pts: { x: number; y: number }[]) {
  g.moveTo(pts[0].x, pts[0].y);
  if (pts.length < 3) {
    for (let i = 1; i < pts.length; i++) g.lineTo(pts[i].x, pts[i].y);
    return;
  }
  for (let i = 0; i < pts.length - 1; i++) {
    const p0 = pts[Math.max(0, i - 1)];
    const p1 = pts[i];
    const p2 = pts[i + 1];
    const p3 = pts[Math.min(pts.length - 1, i + 2)];
    const c1x = p1.x + (p2.x - p0.x) / 6;
    const c1y = p1.y + (p2.y - p0.y) / 6;
    const c2x = p2.x - (p3.x - p1.x) / 6;
    const c2y = p2.y - (p3.y - p1.y) / 6;
    g.bezierCurveTo(c1x, c1y, c2x, c2y, p2.x, p2.y);
  }
}

function setup(): CanvasRenderingContext2D | null {
  const c = el.value;
  if (!c) return null;
  DPR = window.devicePixelRatio || 1;
  W = c.clientWidth || 340;
  H = 170;
  c.width = W * DPR;
  c.height = H * DPR;
  const g = c.getContext("2d");
  if (!g) return null;
  g.scale(DPR, DPR);
  return g;
}

function render() {
  const c = el.value;
  const g = c ? c.getContext("2d") : null;
  if (!c || !g) return;
  const labels = props.labels || [];
  const series = [
    { name: "输入", color: cssVar("--chart-input", "#9aa3b2"), data: props.input || [] },
    { name: "缓存读", color: cssVar("--chart-read", "#4f8cff"), data: props.read || [] },
    { name: "输出", color: cssVar("--chart-output", "#f5a623"), data: props.output || [] },
  ];
  const hitData = props.hitRate || [];
  const n = labels.length;
  const plotW = W - PADL - PADR;
  const plotH = H - PADT - PADB;

  // viewport
  const visCount = Math.max(3, Math.min(n, Math.round(n / vp.zoom)));
  const maxPan = Math.max(0, n - visCount);
  vp.pan = Math.max(0, Math.min(maxPan, Math.round(vp.pan)));
  const si = vp.pan;
  const ei = Math.min(n, si + visCount);
  const visLabels = labels.slice(si, ei);

  let maxTok = 0;
  for (let i = si; i < ei; i++) {
    for (const ser of series) maxTok = Math.max(maxTok, Number(ser.data[i] || 0));
  }
  maxTok = niceMax(maxTok || 1);

  g.clearRect(0, 0, W, H);

  // grid + left axis
  g.strokeStyle = cssVar("--chart-grid", "#eef1f5");
  g.fillStyle = cssVar("--chart-text", "#9aa3b2");
  g.font = "10px monospace";
  g.textAlign = "right";
  g.textBaseline = "middle";
  const ticks = 6;
  for (let i = 0; i <= ticks; i++) {
    const y = PADT + (plotH * i) / ticks;
    g.beginPath();
    g.moveTo(PADL, y);
    g.lineTo(PADL + plotW, y);
    g.stroke();
    g.fillText(fmtNum(maxTok * (1 - i / ticks)), PADL - 6, y);
  }
  // sub-grid
  g.save();
  g.setLineDash([2, 3]);
  g.strokeStyle = cssVar("--chart-grid", "#f0f2f6");
  g.lineWidth = 0.5;
  for (let i = 0; i < ticks; i++) {
    const y = PADT + (plotH * (i + 0.5)) / ticks;
    g.beginPath();
    g.moveTo(PADL, y);
    g.lineTo(PADL + plotW, y);
    g.stroke();
  }
  g.restore();
  // right axis (hit %)
  g.textAlign = "left";
  for (let i = 0; i <= ticks; i++) {
    const y = PADT + (plotH * i) / ticks;
    g.fillText(((1 - i / ticks) * 100).toFixed(0) + "%", PADL + plotW + 6, y);
  }

  if (n === 0) {
    g.fillStyle = cssVar("--chart-text", "#5f6c85");
    g.textAlign = "center";
    g.fillText("暂无数据，等待请求产生统计", W / 2, H / 2);
    return;
  }

  const slot = plotW / Math.max(1, visLabels.length);
  const xCenter = (vi: number) => PADL + slot * (vi - si) + slot / 2;

  // 折线图：各 token 序列独立成线（不堆叠、不填充）
  for (const ser of series) {
    const pts: { x: number; y: number }[] = [];
    for (let vi = si; vi < ei; vi++) {
      const v = Number(ser.data[vi] || 0);
      pts.push({ x: xCenter(vi), y: PADT + plotH * (1 - Math.min(v, maxTok) / maxTok) });
    }
    if (pts.length) {
      g.beginPath();
      tracePath(g, pts);
      g.strokeStyle = ser.color;
      g.lineWidth = 2;
      g.lineJoin = "round";
      g.lineCap = "round";
      g.stroke();
      // 数据点小圆点
      for (const p of pts) {
        g.beginPath();
        g.arc(p.x, p.y, 2.2, 0, Math.PI * 2);
        g.fillStyle = ser.color;
        g.fill();
      }
    }
  }

  // hit rate line
  const hitPts: { x: number; y: number }[] = [];
  for (let vi = si; vi < ei; vi++) {
    const v = hitData[vi];
    if (v != null && v > 0) hitPts.push({ x: xCenter(vi), y: PADT + plotH * (1 - Math.min(v, 1)) });
  }
  if (hitPts.length > 1) {
    g.beginPath();
    tracePath(g, hitPts);
    g.strokeStyle = hexA(cssVar("--chart-hit", "#1fab6b"), 0.55);
    g.lineWidth = 2;
    g.lineJoin = "round";
    g.stroke();
  }
  for (const p of hitPts) {
    g.beginPath();
    g.arc(p.x, p.y, 3, 0, Math.PI * 2);
    g.fillStyle = cssVar("--chart-hit", "#1fab6b");
    g.fill();
    g.strokeStyle = "#fff";
    g.lineWidth = 1.2;
    g.stroke();
  }

  // x labels
  g.fillStyle = cssVar("--chart-text", "#9aa3b2");
  g.textAlign = "center";
  g.textBaseline = "top";
  const step = Math.max(1, Math.ceil(visLabels.length / Math.max(2, Math.floor(plotW / 60))));
  for (let i = 0; i < visLabels.length; i += step) {
    g.fillText(String(visLabels[i]), xCenter(si + i), PADT + plotH + 6);
  }
  if ((visLabels.length - 1) % step !== 0) {
    g.fillText(String(visLabels[visLabels.length - 1]), xCenter(ei - 1), PADT + plotH + 6);
  }
}

function draw() {
  if (setup()) render();
}

// ---------- tooltip ----------
let tip: HTMLDivElement | null = null;
let dragging = false;
let dragX = 0;
let dragPan = 0;

function ensureTip() {
  if (tip) return tip;
  tip = document.createElement("div");
  tip.className = "chart-tip";
  tip.style.display = "none";
  document.body.appendChild(tip);
  return tip;
}

function onMove(ev: MouseEvent) {
  const c = el.value;
  if (!c) return;
  const t = ensureTip();
  const rect = c.getBoundingClientRect();
  const mx = ev.clientX - rect.left;
  const my = ev.clientY - rect.top;
  if (dragging) {
    if (dataLen) {
      const visCount = Math.max(3, Math.min(dataLen, Math.round(dataLen / vp.zoom)));
      const slot = (rect.width - PADL - PADR) / visCount;
      vp.pan = Math.round(dragPan + (dragX - ev.clientX) / slot);
      vp.pan = Math.max(0, Math.min(dataLen - visCount, vp.pan));
      draw();
    }
    t.style.display = "none";
    return;
  }
  if (mx < PADL || mx > PADL + (rect.width - PADL - PADR) || my < PADT || my > PADT + (170 - PADT - PADB)) {
    t.style.display = "none";
    return;
  }
  const labels = props.labels || [];
  const n = labels.length;
  if (!n) return;
  const visCount = Math.max(3, Math.min(n, Math.round(n / vp.zoom)));
  const si = vp.pan;
  const ei = Math.min(n, si + visCount);
  const slot = (rect.width - PADL - PADR) / visCount;
  const relX = mx - PADL;
  const idx = Math.floor(relX / slot);
  const dataIdx = si + idx;
  if (dataIdx < si || dataIdx >= ei) {
    t.style.display = "none";
    return;
  }
  const input = props.input || [], read = props.read || [], output = props.output || [];
  const hit = props.hitRate || [];
  const i = Number(input[dataIdx] || 0);
  const r = Number(read[dataIdx] || 0);
  const o = Number(output[dataIdx] || 0);
  const h = hit[dataIdx];
  let html = `<div class="tip-label">${labels[dataIdx]}</div>`;
  html += `<div class="tip-row"><i style="background:${cssVar("--chart-input", "#9aa3b2")}"></i>输入<span>${fmtComma(i)}</span></div>`;
  html += `<div class="tip-row"><i style="background:${cssVar("--chart-read", "#4f8cff")}"></i>缓存读<span>${fmtComma(r)}</span></div>`;
  html += `<div class="tip-row"><i style="background:${cssVar("--chart-output", "#f5a623")}"></i>输出<span>${fmtComma(o)}</span></div>`;
  if (h != null && h > 0) {
    html += `<div class="tip-row"><i style="background:${cssVar("--chart-hit", "#1fab6b")}"></i>命中率<span>${fmtPct(h)}</span></div>`;
  }
  html += `<div class="tip-total">总计 <span>${fmtComma(i + r + o)}</span></div>`;
  t.innerHTML = html;
  t.style.display = "block";
  const tr = t.getBoundingClientRect();
  let tx = ev.clientX + 12;
  let ty = ev.clientY - tr.height - 8;
  if (tx + tr.width > window.innerWidth - 8) tx = ev.clientX - tr.width - 12;
  if (ty < 8) ty = ev.clientY + 16;
  t.style.left = tx + "px";
  t.style.top = ty + "px";
}

function onLeave() {
  if (tip) tip.style.display = "none";
}

function onWheel(ev: WheelEvent) {
  ev.preventDefault();
  const c = el.value;
  if (!c) return;
  const n = dataLen;
  if (!n) return;
  const rect = c.getBoundingClientRect();
  const mx = ev.clientX - rect.left;
  const ratio = Math.max(0, Math.min(1, (mx - PADL) / (rect.width - PADL - PADR)));
  const oldVis = Math.round(n / vp.zoom);
  const factor = ev.deltaY < 0 ? 1.2 : 1 / 1.2;
  vp.zoom = Math.max(1, Math.min(8, vp.zoom * factor));
  const newVis = Math.round(n / vp.zoom);
  const center = vp.pan + oldVis * ratio;
  vp.pan = Math.round(center - newVis * ratio);
  vp.pan = Math.max(0, Math.min(n - newVis, vp.pan));
  draw();
}

function onDown(ev: MouseEvent) {
  dragging = true;
  dragX = ev.clientX;
  dragPan = vp.pan;
}

function onUp() {
  dragging = false;
}

function onDbl() {
  vp.zoom = 1;
  vp.pan = 0;
  draw();
}

onMounted(() => {
  dataLen = props.labels?.length || 0;
  draw();
  const c = el.value;
  if (c) {
    c.onmousemove = onMove;
    c.onmouseleave = onLeave;
    c.onmousedown = onDown;
    c.onmouseup = onUp;
    c.ondblclick = onDbl;
    c.onwheel = onWheel;
  }
});

onUnmounted(() => {
  if (tip) tip.remove();
});

watch(
  () => [props.labels, props.input, props.read, props.output, props.hitRate, props.theme],
  () => {
    dataLen = props.labels?.length || 0;
    draw();
  },
  { deep: true }
);
</script>

<style scoped>
.linechart canvas {
  width: 100%;
  height: 170px;
  display: block;
  cursor: crosshair;
}
.lc-legend {
  display: flex;
  flex-wrap: wrap;
  gap: 14px;
  font-size: 10.5px;
  color: #5f6c85;
  padding: 3px 2px 0;
}
/* 图例：所有序列都是折线，标记统一为方块（圆角矩形），不再混用圆点 */
.lc-legend .dot {
  display: inline-block;
  width: 8px;
  height: 8px;
  border-radius: 2px;
  margin-right: 4px;
}
.dot.input { background: var(--chart-input, #9aa3b2); }
.dot.read { background: var(--chart-read, #4f8cff); }
.dot.output { background: var(--chart-output, #f5a623); }
.dot.hit { background: var(--chart-hit, #1fab6b); }
</style>