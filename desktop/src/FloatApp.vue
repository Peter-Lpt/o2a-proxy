<template>
  <div class="float" :class="{ on: anyRunning }" data-tauri-drag-region="deep">
    <div class="head">
      <span class="dot" :class="{ on: anyRunning }"></span>
      <!-- 全平台统一：单悬浮窗，下拉在窗口内直接切换服务（保持打开） -->
      <div class="svc-sel" data-tauri-drag-region="false" title="切换服务（窗口内直接切换）">
        <SelectBox v-model="selValue" :options="floatOptions" size="sm" placeholder="全部" />
      </div>
      <span class="sub">{{ statusText }}</span>
      <button class="x" title="关闭悬浮看板" data-tauri-drag-region="false" @click="closeFloat"><Icon name="x" :size="12" /></button>
    </div>

    <div class="stats">
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

    <div class="spark-box">
      <Spark :points="spark" :height="36" class="spark-canvas" />
      <div class="spark-meta">
        <span class="sm">{{ sparkRange[0] }}</span>
        <span class="sm-lbl">缓存命中 · {{ spark.length }} 次</span>
        <span class="sm">{{ sparkRange[1] }}</span>
      </div>
    </div>

    <div class="feed-wrap" data-tauri-drag-region="false">
      <div class="feed">
        <div v-if="!liveFeed.length" class="empty">等待请求…</div>
        <div v-for="(r, i) in liveFeed" :key="r.key" class="row" :class="{ flash: i === 0 }">
          <span class="t">{{ r.time }}</span>
          <span v-if="r.service" class="svc">{{ r.service }}</span>
          <span class="k">↑{{ fmtNum(r.total) }} · 读{{ fmtNum(r.cacheRead) }} · ↓{{ fmtNum(r.output) }}</span>
          <span class="h" :class="r.hitCls">{{ r.hitPct }}%</span>
        </div>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref, watch } from "vue";
import { listen } from "@tauri-apps/api/event";
import { api, fmtNum, fmtPct } from "./api";
import Icon from "./components/Icon.vue";
import SelectBox from "./components/SelectBox.vue";
import Spark from "./components/Spark.vue";
import { applyTheme, getTheme } from "./theme";

const status = ref<any>({ services: [] });
const records = ref<any[]>([]);
const theme = ref<"dark" | "light">(getTheme());
let timers: any[] = [];

// 初始服务从 URL 解析（#/float?service=xxx；共享窗口初始为全部）；
// 由 Rust 发 float-switch 事件在共享窗口内切换服务。
const floatService = ref<string>(
  (() => {
    const m = (window.location.hash || "").match(/[?&]service=([^&]+)/);
    return m ? decodeURIComponent(m[1]) : "";
  })()
);
// 与 Rust 端窗口 label 一致（统一为 float），由创建时 URL 参数传入，
// 用于过滤各自窗口的事件。
const floatLabel = (() => {
  const m = (window.location.hash || "").match(/[?&]label=([^&]+)/);
  return m ? decodeURIComponent(m[1]) : "float";
})();
// 可见性守卫：隐藏时暂停轮询（Rust 发 float-visible 事件 + document.hidden 双保险）
const floatVisible = ref(!document.hidden);
let unlistenFloat: (() => void) | null = null;
let unlistenSwitch: (() => void) | null = null;

function onFloatVisible(visible: boolean) {
  const was = floatVisible.value;
  floatVisible.value = visible;
  if (visible && !was) {
    refresh();
  }
}

// 全平台统一：单共享悬浮窗，标题栏下拉在窗口内直接切换服务（保持打开，不先关再开）。
const selValue = ref(floatService.value);
watch(floatService, (v) => {
  selValue.value = v;
});
// 切换服务：纯切换语义（选不同服务保持打开只换内容；选当前服务无操作），
// 本地立即更新并刷新（Rust float-switch 事件兜底同步）
watch(selValue, async (v) => {
  if (v === floatService.value) return;
  try {
    await api.toggleFloatFor(v);
    floatService.value = v;
    refresh();
  } catch (_) {
    selValue.value = floatService.value; // 失败回弹
  }
});
const floatOptions = computed(() => [
  { value: "", label: "全部" },
  ...(status.value.services || []).map((s: any) => ({ value: s.name, label: s.name })),
]);

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

// getLive 返回最新在前；取最近 40 条并反转为时间正序（旧→新，最新在右）
const pending = computed(() => (records.value || []).slice(0, 40).reverse());
const spark = computed(() => pending.value.map((r: any) => Number(r.cache_hit_rate || 0)));
const sparkRange = computed(() => {
  if (!pending.value.length) return ["", ""];
  const first = String(pending.value[0].timestamp || "").slice(11, 16);
  const last = String(pending.value[pending.value.length - 1].timestamp || "").slice(11, 16);
  return [first, last];
});

const liveFeed = computed(() =>
  (records.value || [])
    .slice(0, 24)
    .map((r: any) => {
      const rate = Number(r.cache_hit_rate || 0);
      return {
        key: `${r.timestamp}_${r.service}_${r.output_tokens}`,
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
    const d = await api.getLive(floatService.value);
    records.value = d?.records || [];
  } catch (e) {
    records.value = [];
  }
}

// 关闭本悬浮窗（隐藏共享窗口）
async function closeFloat() {
  try {
    await api.toggleFloatFor(floatService.value);
  } catch (_) {}
}

function onStorage() {
  theme.value = applyTheme();
}

function onVisibility() {
  onFloatVisible(!document.hidden);
}

onMounted(() => {
  theme.value = applyTheme();
  window.addEventListener("storage", onStorage);
  document.addEventListener("visibilitychange", onVisibility);
  listen<any>("float-visible", (e) => {
    const p = e.payload || {};
    if (p.label === floatLabel) onFloatVisible(!!p.visible);
  }).then((f) => (unlistenFloat = f));
  // Windows 共享窗口：Rust 切换服务时更新并立即刷新数据（按 label 过滤）
  listen<any>("float-switch", (e) => {
    const p = e.payload || {};
    if (p.label !== floatLabel) return;
    if (typeof p.service === "string" && p.service !== floatService.value) {
      floatService.value = p.service;
      refresh();
    }
  }).then((f) => (unlistenSwitch = f));
  refresh();
  // 悬浮窗隐藏时暂停轮询，可见时才拉数据，避免空转
  timers.push(
    setInterval(() => {
      if (floatVisible.value) refresh();
    }, 3000)
  );
});
onUnmounted(() => {
  window.removeEventListener("storage", onStorage);
  document.removeEventListener("visibilitychange", onVisibility);
  unlistenFloat?.();
  unlistenSwitch?.();
  timers.forEach(clearInterval);
});
</script>

<style scoped>
* { margin: 0; padding: 0; box-sizing: border-box; }
.float {
  position: absolute;
  inset: 6px; /* 铺满窗口（留 6px 边距，保证 border 完整显示），随窗口缩放自适应 */
  border-radius: 14px; /* 与主面板 .popover 一致 */
  /* 不用 backdrop-filter：透明置顶窗口上每帧采样/模糊桌面是拖动卡顿主因，
     改用接近不透明的实色背景，保证拖动流畅 */
  background: var(--float-bg, rgba(13, 18, 32, 0.97));
  border: 1px solid var(--glass-border);
  /* 不加 box-shadow：透明窗口的 CSS 阴影只能渲染在窗口内 6px 边距里，
     在 Windows WebView2 上会形成一圈半透明的"第二层边"（Mac WKWebView
     合成不明显）；悬浮窗不投真实阴影到桌面，去掉后与主面板一致 */
  overflow: hidden;
  font-family: -apple-system, "Segoe UI", "PingFang SC", "Microsoft YaHei", sans-serif;
  color: var(--text);
  font-size: 12px;
  user-select: none;
  display: flex;
  flex-direction: column;
}
.float.on { border-color: rgba(52, 211, 153, 0.4); }
.head { display: flex; align-items: center; gap: 6px; padding: 8px 10px 4px; flex: none; }
.dot { width: 7px; height: 7px; border-radius: 50%; background: #3a4250; flex: none; }
.dot.on { background: var(--green); animation: blink 1.6s ease-in-out infinite; }
@keyframes blink { 50% { opacity: 0.3; } }
/* 服务切换下拉：融入悬浮窗配色——按钮透明背景 + 浮窗同款边框（--glass-border），
   hover 轻亮；菜单用浮窗底色（--float-bg），避免与面板输入框风格割裂 */
.svc-sel { flex: none; }
.svc-sel :deep(.sb) { min-width: 92px; }
.svc-sel :deep(.sb-btn) {
  background: transparent;
  border: 1px solid var(--glass-border);
  border-radius: 6px;
}
.svc-sel :deep(.sb-btn:hover),
.svc-sel :deep(.sb.open .sb-btn) {
  background: var(--bg3);
  border-color: var(--glass-border);
}
.svc-sel :deep(.sb-menu) {
  background: var(--float-bg);
  border: 1px solid var(--glass-border);
}
.sub { color: var(--muted); font-size: 10.5px; margin-left: 2px; flex: 1; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.x {
  width: 18px; height: 18px; border: none; border-radius: 5px;
  background: transparent; color: var(--muted); cursor: pointer; font-size: 11px; line-height: 1;
}
.x:hover { background: var(--bg3); color: var(--text); }
.stats { display: grid; grid-template-columns: repeat(3, 1fr); padding: 2px 10px 4px; gap: 4px; font-variant-numeric: tabular-nums; flex: none; }
.num b { display: block; font-size: 15px; font-weight: 700; font-family: var(--font-mono, ui-monospace, Consolas, monospace); }
.num b.good { color: var(--green); }
.num b.mid { color: var(--amber); }
.num span { font-size: 10px; color: var(--muted); }
.spark-box { flex: none; padding: 0 10px 2px; }
/* 悬浮窗里没有外层宽度约束（主面板用 .live-spark 的 width:100%），
   必须给 canvas 固定 CSS 尺寸；否则画布会按 dpr 逐次放大，在高分屏下撑爆布局 */
.spark-canvas { display: block; width: 100%; height: 36px; }
.spark-meta { display: flex; justify-content: space-between; align-items: center; font-size: 9.5px; color: var(--muted-2); font-variant-numeric: tabular-nums; margin-top: 1px; }
.sm-lbl { color: var(--muted); }
.feed-wrap {
  flex: 1 1 auto; min-height: 46px;
  overflow: hidden;
  border-top: 1px solid var(--hairline);
  border-radius: 0 0 14px 14px;
}
.feed {
  height: 100%; overflow-y: auto;
  overflow-x: hidden;
  padding: 3px 10px 6px; font-size: 10.5px; font-variant-numeric: tabular-nums;
}
.row { display: flex; gap: 6px; padding: 2px 0; white-space: nowrap; align-items: baseline; }
.row.flash { animation: flash 1.2s ease-out; }
@keyframes flash { 0% { background: rgba(45,127,249,.14); } 100% { background: transparent; } }
.t { color: var(--muted); flex: none; }
.svc { color: var(--muted); flex: none; font-size: 10px; }
.k { flex: 1; overflow: hidden; text-overflow: ellipsis; }
.h { flex: none; font-weight: 700; min-width: 36px; text-align: right; }
.h.good { color: var(--green); }
.h.mid { color: var(--amber); }
.h.bad { color: var(--muted-2); }
.empty { text-align: center; color: var(--muted); padding: 5px 0; font-size: 10.5px; }
::-webkit-scrollbar { width: 4px; }
::-webkit-scrollbar-thumb { background: var(--scrollbar); border-radius: 2px; }
</style>
