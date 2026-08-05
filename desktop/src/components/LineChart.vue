<template>
  <div class="linechart">
    <canvas ref="el"></canvas>
    <div class="lc-legend">
      <span><i class="dot tok"></i>Token 消耗</span>
      <span><i class="dot hit"></i>缓存命中率</span>
    </div>
  </div>
</template>

<script setup lang="ts">
import { onMounted, ref, watch } from "vue";

const props = defineProps<{
  labels: string[];
  tokens: number[];
  hitRate: number[];
  theme?: string;
}>();

const el = ref<HTMLCanvasElement | null>(null);
let W = 0;
let H = 170;
let PADL = 34;
let PADB = 18;
let PADT = 8;
let DPR = 1;

function cssVar(name: string, fb: string): string {
  const v = getComputedStyle(document.documentElement).getPropertyValue(name).trim();
  return v || fb;
}

function setup(): CanvasRenderingContext2D | null {
  const c = el.value;
  if (!c) return null;
  DPR = window.devicePixelRatio || 1;
  W = c.clientWidth || 340;
  c.width = W * DPR;
  c.height = H * DPR;
  const g = c.getContext("2d");
  if (!g) return null;
  g.scale(DPR, DPR);
  return g;
}

function localCtx(): CanvasRenderingContext2D | null {
  const c = el.value;
  if (!c) return null;
  const g = c.getContext("2d");
  if (!g) return null;
  return g;
}

function render() {
  const c = el.value;
  const ctx = localCtx();
  if (!c || !ctx) return;
  const labels = props.labels || [];
  const tokens = props.tokens || [];
  const hit = props.hitRate || [];
  const n = labels.length;
  ctx.clearRect(0, 0, W, H);

  if (!n) {
    ctx.fillStyle = cssVar("--chart-text", "#5f6c85");
    ctx.font = "11px sans-serif";
    ctx.textAlign = "left";
    ctx.fillText("暂无数据，等待请求产生统计", PADL + 4, H / 2);
    return;
  }

  const cw = W - PADL - 8;
  const ch = H - PADT - PADB;
  const maxTok = Math.max(...tokens, 1);

  // 横向网格线（命中率 0/50/100%）
  ctx.strokeStyle = cssVar("--chart-grid", "rgba(255,255,255,0.05)");
  ctx.lineWidth = 1;
  ctx.fillStyle = cssVar("--chart-text", "#5f6c85");
  ctx.font = "9px monospace";
  ctx.textAlign = "right";
  for (const frac of [0, 0.5, 1]) {
    const y = PADT + ch - frac * ch;
    ctx.beginPath();
    ctx.moveTo(PADL, y);
    ctx.lineTo(W - 4, y);
    ctx.stroke();
    ctx.fillText(String(Math.round(frac * 100)) + "%", PADL - 4, y + 3);
  }
  ctx.fillText(String(Math.round(maxTok)), PADL - 4, PADT + 9);

  // Token 柱
  const bw = Math.min(14, (cw / n) * 0.55);
  tokens.forEach((t, i) => {
    const x = PADL + (i + 0.5) * (cw / n) - bw / 2;
    const bh = (t / maxTok) * ch;
    ctx.fillStyle = cssVar("--chart-bar", "rgba(79,140,255,0.5)");
    const r = Math.min(3, bw / 2);
    ctx.beginPath();
    ctx.roundRect(x, PADT + ch - bh, bw, bh, [r, r, 0, 0]);
    ctx.fill();
  });

  // 命中率线 + 渐变面积
  const linePts: [number, number][] = [];
  hit.forEach((v, i) => {
    const x = PADL + (i + 0.5) * (cw / n);
    const y = PADT + ch - Math.min(Math.max(v, 0), 1) * ch;
    linePts.push([x, y]);
  });
  if (linePts.length > 1) {
    const grad = ctx.createLinearGradient(0, PADT, 0, PADT + ch);
    grad.addColorStop(0, cssVar("--chart-line-glow", "rgba(52,211,153,0.22)"));
    grad.addColorStop(1, "rgba(52,211,153,0)");
    ctx.beginPath();
    linePts.forEach(([x, y], i) => (i === 0 ? ctx.moveTo(x, y) : ctx.lineTo(x, y)));
    ctx.lineTo(linePts[linePts.length - 1][0], PADT + ch);
    ctx.lineTo(linePts[0][0], PADT + ch);
    ctx.closePath();
    ctx.fillStyle = grad;
    ctx.fill();
    ctx.beginPath();
    linePts.forEach(([x, y], i) => (i === 0 ? ctx.moveTo(x, y) : ctx.lineTo(x, y)));
    ctx.strokeStyle = cssVar("--chart-line", "#34d399");
    ctx.lineWidth = 1.8;
    ctx.lineJoin = "round";
    ctx.stroke();
  }

  // 轴线 + X 标签
  ctx.strokeStyle = cssVar("--chart-grid", "rgba(255,255,255,0.09)");
  ctx.beginPath();
  ctx.moveTo(PADL, PADT);
  ctx.lineTo(PADL, PADT + ch);
  ctx.lineTo(W - 4, PADT + ch);
  ctx.stroke();
  ctx.fillStyle = cssVar("--chart-text", "#5f6c85");
  ctx.font = "9px monospace";
  ctx.textAlign = "center";
  const step = Math.max(1, Math.floor(n / 8));
  labels.forEach((lb, i) => {
    if (i % step === 0 || i === n - 1) {
      ctx.fillText(lb, PADL + (i + 0.5) * (cw / n), H - 5);
    }
  });
}

function draw() {
  if (setup()) render();
}

function onHover(ev: MouseEvent) {
  const c = el.value;
  const ctx = localCtx();
  if (!c || !ctx) return;
  const labels = props.labels || [];
  const tokens = props.tokens || [];
  const hit = props.hitRate || [];
  const n = labels.length;
  if (!n) return;
  render();
  const rect = c.getBoundingClientRect();
  const mx = ev.clientX - rect.left;
  const cw = W - PADL - 8;
  const idx = Math.min(n - 1, Math.max(0, Math.round((mx - PADL) / (cw / n) - 0.5)));
  const x = PADL + (idx + 0.5) * (cw / n);
  const ch = H - PADT - PADB;
  ctx.strokeStyle = cssVar("--chart-crosshair", "rgba(255,255,255,0.3)");
  ctx.setLineDash([3, 3]);
  ctx.beginPath();
  ctx.moveTo(x, PADT);
  ctx.lineTo(x, PADT + ch);
  ctx.stroke();
  ctx.setLineDash([]);
  const label = labels[idx];
  const tok = tokens[idx];
  const hr = Math.round((hit[idx] || 0) * 1000) / 10;
  const tw = 108;
  const tx = Math.min(W - tw - 4, Math.max(4, x - tw / 2));
  ctx.fillStyle = cssVar("--chart-tooltip-bg", "rgba(16,23,42,0.96)");
  ctx.strokeStyle = cssVar("--chart-tooltip-border", "#2b3a5c");
  ctx.beginPath();
  ctx.roundRect(tx, PADT + 4, tw, 36, 6);
  ctx.fill();
  ctx.stroke();
  ctx.textAlign = "left";
  ctx.fillStyle = cssVar("--text", "#e7edf8");
  ctx.font = "10px monospace";
  ctx.fillText(String(label), tx + 8, PADT + 19);
  ctx.fillStyle = cssVar("--muted", "#93a0b8");
  ctx.font = "9px monospace";
  ctx.fillText("tok " + tok + " · 命中 " + hr + "%", tx + 8, PADT + 32);
}

function onLeave() {
  render();
}

onMounted(() => {
  draw();
  const c = el.value;
  if (c) {
    c.onmousemove = onHover;
    c.onmouseleave = onLeave;
  }
});
watch(() => [props.labels, props.tokens, props.hitRate, props.theme], draw, { deep: true });
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
  gap: 14px;
  font-size: 10.5px;
  color: #5f6c85;
  padding: 3px 2px 0;
}
.lc-legend .dot {
  display: inline-block;
  width: 8px;
  height: 8px;
  border-radius: 2px;
  margin-right: 4px;
}
.dot.tok { background: rgba(79, 140, 255, 0.5); }
.dot.hit { background: #34d399; }
</style>
