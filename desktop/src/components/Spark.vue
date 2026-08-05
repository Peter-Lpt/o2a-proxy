<template>
  <canvas ref="el"></canvas>
</template>

<script setup lang="ts">
import { onMounted, ref, watch } from "vue";

const props = defineProps<{
  points: number[];
  height?: number;
  color?: string;
}>();

const el = ref<HTMLCanvasElement | null>(null);

function draw() {
  const c = el.value;
  if (!c) return;
  const dpr = window.devicePixelRatio || 1;
  const w = c.clientWidth || 200;
  const h = props.height || 40;
  c.width = w * dpr;
  c.height = h * dpr;
  const ctx = c.getContext("2d");
  if (!ctx) return;
  ctx.scale(dpr, dpr);
  ctx.clearRect(0, 0, w, h);
  const pts = props.points || [];
  if (pts.length < 2) {
    // 无数据：画一条虚线基线，避免空白画布
    const grid =
      getComputedStyle(document.documentElement)
        .getPropertyValue("--chart-grid")
        .trim() || "rgba(255,255,255,0.12)";
    ctx.strokeStyle = grid;
    ctx.lineWidth = 1;
    ctx.setLineDash([3, 3]);
    ctx.beginPath();
    ctx.moveTo(0, h - 2);
    ctx.lineTo(w, h - 2);
    ctx.stroke();
    ctx.setLineDash([]);
    return;
  }
  const max = Math.max(...pts, 1);
  const color = props.color || "#2d7ff9";
  // 面积渐变填充
  const grad = ctx.createLinearGradient(0, 0, 0, h);
  grad.addColorStop(0, color + "55");
  grad.addColorStop(1, color + "00");
  ctx.beginPath();
  pts.forEach((p, i) => {
    const x = (i / (pts.length - 1)) * w;
    const y = h - 3 - (p / max) * (h - 8);
    if (i === 0) ctx.moveTo(x, y);
    else ctx.lineTo(x, y);
  });
  ctx.lineTo(w, h);
  ctx.lineTo(0, h);
  ctx.closePath();
  ctx.fillStyle = grad;
  ctx.fill();

  ctx.strokeStyle = color;
  ctx.lineWidth = 1.6;
  ctx.beginPath();
  pts.forEach((p, i) => {
    const x = (i / (pts.length - 1)) * w;
    const y = h - 3 - (p / max) * (h - 8);
    if (i === 0) ctx.moveTo(x, y);
    else ctx.lineTo(x, y);
  });
  ctx.stroke();
}

onMounted(draw);
watch(() => props.points, draw, { deep: true });
</script>
