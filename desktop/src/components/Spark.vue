<template>
  <canvas ref="el"></canvas>
</template>

<script setup lang="ts">
import { onMounted, ref, watch } from "vue";

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
  const bw = Math.max(3, Math.min(9, (w - gap * rows.length) / rows.length));
  const totalW = rows.length * (bw + gap) - gap;
  let x = w - totalW;
  const COLOR: Record<string, string> = { good: "#1fab6b", mid: "#f5a623", bad: "#c3cad6" };
  rows.forEach((rate) => {
    const v = Math.max(0, Math.min(1, Number(rate) || 0));
    const bh = Math.max(2, v * (h - 4));
    ctx.fillStyle = COLOR[hitCls(v)];
    ctx.beginPath();
    ctx.roundRect(x, h - bh, bw, bh, 1.5);
    ctx.fill();
    x += bw + gap;
  });
}

onMounted(draw);
watch(() => props.points, draw, { deep: true });
</script>