/**
 * 统计域状态与轮询逻辑（§10.1 stores 拆分）。
 * stats / liveRecords 为引擎统计的响应式镜像；区间（range）与偏好持久化、
 * 模型过滤、额度卡可见性在此集中，PanelApp 心跳只负责触发。
 */
import { computed, reactive, ref } from "vue";
import { api } from "../api";
import { ALL, selected } from "./config";
import { activeSvc } from "./services";

export const stats = reactive<any>({});
export const statsService = computed(() => (selected.value === ALL ? "" : selected.value));  // 当前服务（id 直传后端）
// 当前所选服务集合是否展示费用（订阅制如 opencode token/code plan 不计价）
export const showCost = computed(() => stats.showCost !== false);
export const liveRecords = ref<any[]>([]);

type RangeKey = "today" | "yesterday" | "week" | "lastweek" | "month" | "lastmonth" | "custom";
// 区间档位（历史档由日历点选自定义区间替代，仅今日/本月暴露为主档）
// 区间档位：下拉可选档 = 主档 + 快捷预设（近7天/近30天）；lastweek/lastmonth
// 仅保留 label 兼容（原 rangeOptions 死代码，§10.2-2 统一取值集合）
export const rangeOptions: { value: RangeKey; label: string }[] = [
  { value: "today", label: "今日" },
  { value: "yesterday", label: "昨日" },
  { value: "week", label: "本周" },
  { value: "month", label: "本月" },
];
// 持久化偏好（§10.2-2）：下拉全集 today/yesterday/week/month/7d/30d 都可记忆，
// 重启后按同一语义恢复（7d/30d 重算相对日期）；自定义区间不记忆（原行为）
export function readRangePref(): { range: RangeKey; preset: string | null } {
  try {
    const v = localStorage.getItem("o2a-stats-range");
    if (v === "today" || v === "yesterday" || v === "week" || v === "month") {
      return { range: v as RangeKey, preset: null };
    }
    if (v === "7d" || v === "30d") return { range: "custom", preset: v };
  } catch (_) {}
  return { range: "today", preset: null };
}
const _rangePref = readRangePref();
export const range = ref<RangeKey>(_rangePref.range);
export const presetKey = ref<string | null>(_rangePref.preset);
export function persistRangePref() {
  try {
    if (range.value === "custom") {
      if (presetKey.value) localStorage.setItem("o2a-stats-range", presetKey.value);
      else localStorage.removeItem("o2a-stats-range");
    } else {
      localStorage.setItem("o2a-stats-range", range.value);
    }
  } catch (_) {}
}
export const calOpen = ref(false);
export const customRange = ref<{ start: string; end: string } | null>(null);
// 启动时恢复「近7天/近30天」预设：按当天重算区间（不触发 loadStats，由 onMounted 统一拉取）
if (presetKey.value) {
  const now = new Date();
  const p = (n: number) => String(n).padStart(2, "0");
  const iso = (d: Date) => `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`;
  const days = presetKey.value === "7d" ? 6 : 29;
  customRange.value = { start: iso(new Date(now.getTime() - days * 86400000)), end: iso(now) };
}
export const modelFilter = ref<string>("");

export const statsLoading = ref(false);
export const statsError = ref("");
export const chartResetKey = ref(0);

export const rangeLabel = computed(() => {
  if (range.value === "custom") {
    const s = String(stats.rangeStart || "").slice(5);
    const e = String(stats.rangeEnd || "").slice(5);
    return s && e ? `${s} ~ ${e}` : "自定义";
  }
  const r = String(stats.range || range.value);
  return rangeOptions.find((o) => o.value === r)?.label || "今日";
});


export async function loadStats() {
  statsLoading.value = true;
  try {
    const c = customRange.value;
    const isCustom = range.value === "custom";
    const s = await api.getStats(
      statsService.value,
      range.value,
      isCustom && c ? c.start : undefined,
      isCustom && c ? c.end : undefined,
      modelFilter.value || undefined
    );
    Object.keys(stats).forEach((k) => delete (stats as any)[k]);
    Object.assign(stats, s || {});
    statsError.value = "";
  } catch (e: any) {
    // 统计目录缺失/未启用统计时给出可操作提示，不再静默
    statsError.value = "统计读取失败：" + (e || "未知错误") + "。请确认 config.json 已启用 cache_stats_enabled 且统计目录存在";
  } finally {
    statsLoading.value = false;
  }
  if (quotaVisible.value) loadQuota();
}

// ---------- §8.5 订阅额度展示（订阅制服务：费用卡位置展示额度卡；§2.3 对象形式兼容） ----------
export const quotaVisible = computed(() => {
  if (selected.value === ALL) return false;
  const p: any = activeSvc.value?.pricing;
  if (p === "none") return true;
  return !!p && typeof p === "object" && p.mode === "subscription";
});
export const quotaSnapshot = ref<any>(null);
export async function loadQuota() {
  const acc = activeSvc.value?.account;
  if (!acc) return;
  try {
    quotaSnapshot.value = await api.getQuota(acc);
  } catch {
    // 引擎未运行 / 端口不可达：隐藏额度卡，不影响统计页其余渲染（§8.4-3）
    quotaSnapshot.value = null;
  }
}

export async function loadLive() {
  try {
    const d = await api.getLive(statsService.value);
    liveRecords.value = d?.records || [];
  } catch (e) {
    liveRecords.value = [];
  }
}

export function setRange(r: RangeKey) {
  range.value = r;
  presetKey.value = null;
  persistRangePref();
  modelFilter.value = "";
  calOpen.value = false;
  loadStats();
}

// 时间区间下拉的当前值（§10.2-1 修复）：主档直接映射；预设（近7天/近30天）保持
// 预设键显示"近 7 天"而非跳成"自定义"；仅日历手选的区间才显示"自定义"
export const rangeSelectValue = computed(() => {
  if (presetKey.value) return presetKey.value;
  if (["today", "yesterday", "week", "month"].includes(range.value)) return range.value;
  return "custom";
});

// 时间区间下拉选项（与模型过滤等下拉统一用 SelectBox 组件）；自定义区间时追加动态 label
export const rangeSelectOptions = computed(() => {
  const opts = [
    { value: "today", label: "今日" },
    { value: "yesterday", label: "昨日" },
    { value: "week", label: "本周" },
    { value: "month", label: "本月" },
    { value: "7d", label: "近 7 天" },
    { value: "30d", label: "近 30 天" },
  ];
  if (!presetKey.value && range.value === "custom") {
    opts.push({ value: "custom", label: `自定义 ${rangeLabel.value}` });
  }
  return opts;
});

// 时间区间下拉切换（SelectBox 直接传所选 value）
export function onRangeSelect(v: string) {
  if (["today", "yesterday", "week", "month"].includes(v)) {
    setRange(v as RangeKey);
  } else if (v === "7d" || v === "30d") {
    presetKey.value = v;
    onQuickRange(v);
  }
  // v === "custom"：当前已是自定义，无需处理
}

// 自定义区间：起止日期（YYYY-MM-DD）；保持日历展开，便于用户看到选中区间并可微调。
// fromPreset：来自"近7天/近30天"快捷（保持预设键以正确显示下拉文案并持久化）
export function setCustomRange(start: string, end: string, fromPreset = false) {
  customRange.value = { start, end };
  range.value = "custom";
  if (!fromPreset) presetKey.value = null;
  persistRangePref();
  modelFilter.value = "";
  loadStats();
}

export function onCalSelect(start: string, end: string) {
  setCustomRange(start, end);
}

// 日历底部快捷：昨日 / 近7天 / 近30天（今日/本月由外部主档位提供，避免重复）
export function onCalQuick(key: string) {
  const now = new Date();
  const p = (n: number) => String(n).padStart(2, "0");
  const iso = (d: Date) => `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`;
  if (key === "yesterday") {
    const y = new Date(now.getTime() - 86400000);
    const s = iso(y);
    setCustomRange(s, s); // 日历手选性质：清除预设键
    return;
  }
  const days = key === "7d" ? 6 : 29;
  const start = iso(new Date(now.getTime() - days * 86400000));
  setCustomRange(start, iso(now), true);
}

// 图表头部快捷区间：近7天 / 近30天（复用日历快捷逻辑）
function onQuickRange(key: string) {
  onCalQuick(key);
}


