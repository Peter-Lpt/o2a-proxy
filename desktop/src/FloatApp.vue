<template>
  <div class="float" :class="{ on: anyActive }" data-tauri-drag-region="deep">
    <div class="head">
      <span class="hdot" :class="[headInner, { ring: headRing }]" :title="headDotTitle"></span>
      <!-- 全平台统一：单悬浮窗，下拉在窗口内直接切换服务（保持打开） -->
      <div class="svc-sel" data-tauri-drag-region="false" title="切换服务（窗口内直接切换）">
        <SelectBox v-model="selValue" :options="floatOptions" size="sm" placeholder="全部" />
      </div>
      <span class="sub">{{ statusText }}</span>
      <button class="x" title="打开主面板" data-tauri-drag-region="false" @click="openPanel"><Icon name="panel" :size="12" /></button>
      <button class="x" title="关闭悬浮看板" data-tauri-drag-region="false" @click="closeFloat"><Icon name="x" :size="12" /></button>
    </div>

    <div class="stats">
      <div class="num" :title="numReqTitle">
        <b>{{ fmtNum(min1.requests) }}</b><span>近5min 请求</span>
      </div>
      <div class="num" :title="numTokTitle">
        <b>{{ fmtNum(min1.tokens) }}</b><span>近5min Token</span>
      </div>
      <div class="num" :title="numHitTitle">
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
        <div v-for="(r, i) in liveFeed" :key="r.key" class="row" :class="{ flash: i === 0, err: r.isErr }">
          <span class="t">{{ r.time }}</span>
          <span v-if="r.service" class="svc">{{ r.service }}</span>
          <span v-if="r.isErr" class="err-lbl" :title="r.err">{{ r.err }}</span>
          <span v-else class="k">↑{{ fmtNum(r.total) }} · 读{{ fmtNum(r.cacheRead) }} · ↓{{ fmtNum(r.output) }}<span v-if="r.speed > 0" class="spd" :title="'输出 ' + r.speed + ' tok/s'">·{{ fmtSpeed(r.speed) }}</span></span>
          <span class="h" :class="r.hitCls">{{ r.hitPct }}%</span>
        </div>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref, watch } from "vue";
import { emit, listen } from "@tauri-apps/api/event";
import { api, fmtNum, fmtPct } from "./api";
import { hitTier, todayLiveRecords } from "./format";
import Icon from "./components/Icon.vue";
import SelectBox from "./components/SelectBox.vue";
import Spark from "./components/Spark.vue";
import { applyTheme, getTheme, watchSystemTheme, type Theme } from "./theme";

const status = ref<any>({ services: [] });
const records = ref<any[]>([]);
const theme = ref<Theme>(getTheme());
let timers: any[] = [];
let resizeTimer: any = null;

function fmtSpeed(s: number): string {
  if (!s || isNaN(s) || s <= 0) return "";
  return s >= 1000 ? (s / 1000).toFixed(1) + "k/s" : s.toFixed(0) + "/s";
}

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
  ...(status.value.services || []).map((s: any) => ({
    value: s.name,
    label: s.name,
  })),
]);

// 服务状态：灰=未运行，绿=运行空闲，busy(琥珀闪烁)=正在处理
function svcState(s: any): "gray" | "green" | "busy" {
  if (!s.running) return "gray";
  return s.task?.active ? "busy" : "green";
}

// 全局：任一服务有任务在跑则闪烁
const anyActive = computed(() =>
  (status.value.services || []).some((s: any) => s.task?.active)
);
const anyRunning = computed(() => (status.value.services || []).some((s: any) => s.running));

// 头部状态点：随当前视图变化。全部视图=全局聚合；单服务视图=该服务自身状态
const headInner = computed<"gray" | "green" | "busy">(() => {
  const svc = floatService.value;
  const svcs = status.value.services || [];
  if (svc) {
    const s = svcs.find((x: any) => x.name === svc);
    return s ? svcState(s) : "gray";
  }
  if (anyActive.value) return "busy";
  if (anyRunning.value) return "green";
  return "gray";
});
// 外环（天蓝）：仅单服务视图，且存在其他 running 服务时给出提示
const headRing = computed<boolean>(() => {
  const svc = floatService.value;
  if (!svc) return false;
  return (status.value.services || []).some((x: any) => x.name !== svc && x.running);
});
const headDotTitle = computed(() => {
  const svc = floatService.value;
  const name =
    (status.value.services || []).find((x: any) => x.name === svc)?.name || "全部";
  const stateTxt =
    headInner.value === "busy"
      ? `${name}：正在处理`
      : headInner.value === "green"
        ? `${name}：运行中`
        : `${name}：未运行`;
  return headRing.value ? `${stateTxt}；其他服务有运行` : stateTxt;
});
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

// 数字 hover 明细（近 5 分钟窗口分解）
const numBreakdown = computed(() => {
  const list = now1min.value;
  let input = 0, read = 0, write = 0, output = 0;
  list.forEach((r: any) => {
    input += Number(r.input_tokens || 0);
    read += Number(r.cache_read_tokens || 0);
    write += Number(r.cache_write_tokens || 0);
    output += Number(r.output_tokens || 0);
  });
  return { input, read, write, output };
});
const numReqTitle = computed(() => `近5min 请求：${min1.value.requests} 次`);
const numTokTitle = computed(
  () =>
    `近5min Token（${numBreakdown.value.output} 输出）\n输入 ${numBreakdown.value.input} · 缓存读 ${numBreakdown.value.read} · 缓存写 ${numBreakdown.value.write}`
);
const numHitTitle = computed(() =>
  now1min.value.length
    ? `近5min 命中率：${fmtPct(min1.value.hitRate)}（缓存读 ${numBreakdown.value.read} / 输入+读 ${numBreakdown.value.read + numBreakdown.value.input}）`
    : "近5min 暂无请求"
);

const hitCls = computed(() => hitTier(min1.value.hitRate, false));

// getLive 返回最新在前；先按当天过滤 + 时间倒序，再取最近 40 条反转为时间正序（旧→新，最新在右）
const pending = computed(() => todayLiveRecords(records.value, 40).reverse());
const spark = computed(() => pending.value.map((r: any) => Number(r.cache_hit_rate || 0)));
const sparkRange = computed(() => {
  if (!pending.value.length) return ["", ""];
  const first = String(pending.value[0].timestamp || "").slice(11, 16);
  const last = String(pending.value[pending.value.length - 1].timestamp || "").slice(11, 16);
  return [first, last];
});

// 严格按当天 + 完整时间戳倒序（最新在前）：跨天残留的旧记录不展示，
// 时间列因此恒为 HH:mm:ss，不会被误读成当天时刻。
const liveFeed = computed(() =>
  todayLiveRecords(records.value, 24).map((r: any) => {
    const rate = Number(r.cache_hit_rate || 0);
    const isErr = !!r.error || r.status === "error";
    const ts = String(r.timestamp || "");
    return {
      key: `${ts}_${r.service}_${r.output_tokens}_${r.error || ""}`,
      time: ts.slice(11, 19),
      service: r.service || "",
      total:
        Number(r.input_tokens || 0) +
        Number(r.cache_read_tokens || 0) +
        Number(r.cache_write_tokens || 0),
      cacheRead: Number(r.cache_read_tokens || 0),
      output: Number(r.output_tokens || 0),
      hitPct: (rate * 100).toFixed(0),
      hitCls: hitTier(rate, true),
      isErr,
      err: isErr ? String(r.error || r.status || "error") : "",
      duration: Number(r.duration_ms || 0),
      firstToken: Number(r.first_token_ms || 0),
      speed: Number(r.output_tokens_per_sec || 0),
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

// 打开主面板（隐藏时唤出）
function openPanel() {
  api.togglePanel().catch(() => {});
}

function onStorage() {
  theme.value = applyTheme();
}

function onVisibility() {
  onFloatVisible(!document.hidden);
}

// 记忆悬浮窗尺寸：用户缩放后写入 localStorage，下次启动恢复
function onWinResize() {
  clearTimeout(resizeTimer);
  resizeTimer = setTimeout(() => {
    try {
      localStorage.setItem(
        "o2a-float-size",
        JSON.stringify({ w: window.innerWidth, h: window.innerHeight })
      );
    } catch (_) {}
  }, 400);
}
function restoreFloatSize() {
  try {
    const saved = JSON.parse(localStorage.getItem("o2a-float-size") || "null");
    if (saved && saved.w > 0 && saved.h > 0) {
      api.setFloatSize(Number(saved.w), Number(saved.h)).catch(() => {});
    }
  } catch (_) {}
}

onMounted(() => {
  theme.value = applyTheme();
  window.addEventListener("storage", onStorage);
  document.addEventListener("visibilitychange", onVisibility);
  window.addEventListener("resize", onWinResize);
  restoreFloatSize();
  // 主题跨窗口同步：面板/悬浮窗任一侧切换，另一侧立即跟随
  listen<string>("o2a-theme", (e) => {
    const t = e.payload as Theme;
    if (t !== theme.value) {
      theme.value = t;
      applyTheme(t);
    }
  }).catch(() => {});
  watchSystemTheme(() => {
    theme.value = applyTheme();
    emit("o2a-theme", theme.value).catch(() => {});
  });
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
    }, 2000)
  );
});
onUnmounted(() => {
  window.removeEventListener("storage", onStorage);
  document.removeEventListener("visibilitychange", onVisibility);
  window.removeEventListener("resize", onWinResize);
  clearTimeout(resizeTimer);
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
/* 头部状态点：内点=当前视图状态（灰停/绿闲/琥珀闪忙），
   外环天蓝=单服务视图下其他服务有在跑 */
.hdot {
  width: 9px; height: 9px; border-radius: 50%;
  background: #3a4250; border: 1.5px solid transparent;
  flex: none;
}
.hdot.green { background: var(--green); }
.hdot.busy { background: var(--amber); animation: blink 1.6s ease-in-out infinite; }
.hdot.ring { border-color: var(--cyan); }
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
.k .spd { color: var(--muted-2); font-size: 9.5px; }
.err-lbl {
  flex: 1; overflow: hidden; text-overflow: ellipsis; color: var(--red);
}
.row.err .t { color: var(--red); }
.row.err .svc { color: var(--red); }
.row.err .h { color: var(--red); }
.h { flex: none; font-weight: 700; min-width: 36px; text-align: right; }
.h.good { color: var(--green); }
.h.mid { color: var(--amber); }
.h.bad { color: var(--muted-2); }
.empty { text-align: center; color: var(--muted); padding: 5px 0; font-size: 10.5px; }
::-webkit-scrollbar { width: 4px; }
::-webkit-scrollbar-thumb { background: var(--scrollbar); border-radius: 2px; }
</style>
