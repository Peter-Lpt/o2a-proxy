<template>
  <canvas ref="el"></canvas>
</template>

<script setup lang="ts">
import { onMounted, onUnmounted, ref, watch } from "vue";

const props = defineProps<{
  points: number[]; // 缓存命中率 0..1
  height?: number;
  color?: string; // 未使用，颜色由命中档位决定
}>();

const el = ref<HTMLCanvasElement | null>(null);

function hitCls(rate: number): string {
  return rate >= 0.6 ? "good" : rate > 0.15 ? "mid" : "bad";
}

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
  const rows = props.points || [];
  if (!rows.length) {
    ctx.fillStyle =
      getComputedStyle(document.documentElement).getPropertyValue("--chart-text").trim() ||
      "#5f6c85";
    ctx.textAlign = "center";
    ctx.textBaseline = "middle";
    ctx.font = "10px sans-serif";
    ctx.fillText("暂无请求", w / 2, h / 2);
    return;
  }
  const gap = 2;
  const n = rows.length;
  const slot = w / n;
  const bw = Math.max(2, Math.min(9, slot - gap));
  const COLOR: Record<string, string> = { good: "#1fab6b", mid: "#f5a623", bad: "#c3cad6" };
  // 按槽位均匀铺满整宽（左旧右新），与下方时间轴两端对齐
  rows.forEach((rate, i) => {
    const v = Math.max(0, Math.min(1, Number(rate) || 0));
    const x = i * slot + (slot - bw) / 2;
    const bh = Math.max(2, v * (h - 4));
    const r = Math.min(1.5, bw / 2, bh / 2);
    ctx.fillStyle = COLOR[hitCls(v)];
    ctx.beginPath();
    ctx.roundRect(x, h - bh, bw, bh, r);
    ctx.fill();
  });
}

let ro: ResizeObserver | null = null;

onMounted(() => {
  draw();
  // 悬浮窗可缩放：窗口尺寸变化时重绘，保证条形图铺满新宽度
  if (el.value) {
    ro = new ResizeObserver(draw);
    ro.observe(el.value);
  }
});
onUnmounted(() => {
  if (ro) { ro.disconnect(); ro = null; }
});
watch(() => props.points, draw, { deep: true });
</script>