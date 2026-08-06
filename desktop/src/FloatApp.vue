<template>
  <div class="float" :class="{ on: anyRunning }" data-tauri-drag-region="deep">
    <div class="head">
      <span class="dot" :class="{ on: anyRunning }"></span>
      <span class="ttl">{{ floatTitle }}</span>
      <span class="sub">{{ statusText }}</span>
      <button class="x" title="关闭悬浮看板" @click="api.toggleFloatFor(floatService)"><Icon name="x" :size="12" /></button>
    </div>
    <div class="nums">
      <div class="num">
        <b>{{ fmtNum(min1.requests) }}</b><span>近5min 请求</span>
      </div>
      <div class="num">
        <b>{{ fmtNum(min1.tokens) }}</b><span>近5min Token</span>
      </div>
      <div class="num">
        <b :class="hitCls">{{ now1min.length ? fmtPct(min1.hitRate) : "—" }}</b><span>近5min 命中</span>
      </div>
    </div>
    <Spark :points="spark" :height="36" :color="sparkColor" />
    <div class="feed" data-tauri-drag-region="false">
      <div v-if="!liveFeed.length" class="empty">等待请求…</div>
      <div v-for="(r, i) in liveFeed" :key="i" class="row" :class="{ flash: i === 0 }">
        <span class="t">{{ r.time }}</span>
        <span v-if="r.service" class="svc">{{ r.service }}</span>
        <span class="k">↑{{ fmtNum(r.total) }} · 读{{ fmtNum(r.cacheRead) }} · ↓{{ fmtNum(r.output) }}</span>
        <span class="h" :class="r.hitCls">{{ r.hitPct }}%</span>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from "vue";
import { api, fmtNum, fmtPct } from "./api";
import Icon from "./components/Icon.vue";
import Spark from "./components/Spark.vue";
import { applyTheme, getTheme } from "./theme";

const status = ref<any>({ services: [] });
const records = ref<any[]>([]);
const theme = ref<"dark" | "light">(getTheme());
let timers: any[] = [];

// 从 URL 解析本悬浮窗对应的服务（#/float?service=xxx）
const floatService = (() => {
  const m = (window.location.hash || "").match(/[?&]service=([^&]+)/);
  return m ? decodeURIComponent(m[1]) : "";
})();
const floatTitle = computed(() =>
  floatService ? floatService + " · 悬浮看板" : "o2a-proxy · 全部"
);

const anyRunning = computed(() => (status.value.services || []).some((s: any) => s.running));
const now1min = computed(() => {
  const cutoff = Date.now() - 300_000;
  return (records.value || []).filter((r: any) => {
    const ts = new Date(String(r.timestamp || "").replace("T", " ")).getTime();
    return !isNaN(ts) && ts >= cutoff;
  });
});
const runningMeta = computed(() => {
  const list = (status.value.services || []).filter((s: any) => s.running);
  return list.length ? list.map((s: any) => ":" + s.port).join(" ") : "运行中";
});
const sparkColor = computed(() => {
  if (!anyRunning.value) return "#3a4250";
  return theme.value === "light" ? "#0f9d6a" : "#34d399";
});
const statusText = computed(() =>
  anyRunning.value
    ? now1min.value.length
      ? runningMeta.value
      : "运行中 · 等待请求"
    : "已停止"
);

const min1 = computed(() => {
  const list = now1min.value;
  let input = 0, read = 0, output = 0;
  list.forEach((r: any) => {
    input += Number(r.input_tokens || 0);
    read += Number(r.cache_read_tokens || 0);
    output += Number(r.output_tokens || 0);
  });
  const denom = read + input;
  return {
    requests: list.length,
    tokens: output,
    hitRate: denom > 0 ? read / denom : 0,
  };
});

const hitCls = computed(() => (min1.value.hitRate >= 0.3 ? "good" : min1.value.hitRate > 0 ? "mid" : ""));

const spark = computed(() =>
  (records.value || []).slice(-40).map((r: any) => Number(r.cache_hit_rate || 0))
);

const liveFeed = computed(() =>
  (records.value || [])
    .slice(0, 30)
    .map((r: any) => {
      const rate = Number(r.cache_hit_rate || 0);
      return {
        time: String(r.timestamp || "").slice(11, 19),
        service: r.service || "",
        total:
          Number(r.input_tokens || 0) +
          Number(r.cache_read_tokens || 0) +
          Number(r.cache_write_tokens || 0),
        cacheRead: Number(r.cache_read_tokens || 0),
        output: Number(r.output_tokens || 0),
        hitPct: (rate * 100).toFixed(0),
        hitCls: rate >= 0.6 ? "good" : rate > 0.15 ? "mid" : "bad",
      };
    })
);

async function refresh() {
  try {
    status.value = await api.getStatus();
    const d = await api.getLive(floatService);
    records.value = d?.records || [];
  } catch (e) {
    records.value = [];
  }
}

function onStorage() {
  theme.value = applyTheme();
}

onMounted(() => {
  theme.value = applyTheme();
  window.addEventListener("storage", onStorage);
  refresh();
  timers.push(setInterval(refresh, 3000));
});
onUnmounted(() => {
  window.removeEventListener("storage", onStorage);
  timers.forEach(clearInterval);
});
</script>

<style scoped>
* { margin: 0; padding: 0; box-sizing: border-box; }
.float {
  margin: 6px;
  border-radius: 13px;
  background: var(--glass-bg);
  backdrop-filter: blur(22px);
  -webkit-backdrop-filter: blur(22px);
  border: 1px solid var(--glass-border);
  box-shadow: none;
  transition: border-color 0.3s;
  overflow: hidden;
  font-family: -apple-system, "Segoe UI", "PingFang SC", "Microsoft YaHei", sans-serif;
  color: var(--text);
  font-size: 12px;
  user-select: none;
}
.float.on {
  border-color: rgba(52, 211, 153, 0.35);
  box-shadow: none;
}
.head { display: flex; align-items: center; gap: 6px; padding: 8px 10px 4px; }
.dot { width: 7px; height: 7px; border-radius: 50%; background: #3a4250; flex: none; }
.dot.on { background: var(--green); animation: blink 1.6s ease-in-out infinite; }
@keyframes blink { 50% { opacity: 0.3; } }
.ttl { font-weight: 700; font-size: 12px; }
.sub { color: var(--muted); font-size: 10.5px; margin-left: 2px; flex: 1; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.x {
  width: 18px; height: 18px; border: none; border-radius: 5px;
  background: transparent; color: var(--muted); cursor: pointer; font-size: 11px; line-height: 1;
}
.x:hover { background: var(--bg3); color: var(--text); }
.nums { display: grid; grid-template-columns: repeat(3, 1fr); padding: 2px 10px 4px; gap: 4px; font-variant-numeric: tabular-nums; }
.num b { display: block; font-size: 15px; font-weight: 700; font-family: var(--font-mono, ui-monospace, Consolas, monospace); }
.num b.good { color: var(--green); }
.num b.mid { color: var(--amber); }
.num span { font-size: 10px; color: var(--muted); }
.feed {
  max-height: 62px; overflow-y: auto;
  border-top: 1px solid var(--hairline);
  padding: 3px 10px 6px; font-size: 10.5px; font-variant-numeric: tabular-nums;
}
.row { display: flex; gap: 6px; padding: 2px 0; white-space: nowrap; }
.row.flash { animation: flash 1.2s ease-out; }
@keyframes flash { 0% { background: rgba(45,127,249,.14); } 100% { background: transparent; } }
.t { color: var(--muted); flex: none; }
.k { flex: 1; overflow: hidden; text-overflow: ellipsis; }
.svc { color: var(--muted); flex: none; font-size: 10px; }
.h { flex: none; font-weight: 700; min-width: 36px; text-align: right; }
.h.good { color: var(--green); }
.h.mid { color: var(--amber); }
.empty { text-align: center; color: var(--muted); padding: 5px 0; font-size: 10.5px; }
::-webkit-scrollbar { width: 4px; }
::-webkit-scrollbar-thumb { background: var(--scrollbar); border-radius: 2px; }
</style>
