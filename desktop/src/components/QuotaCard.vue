<template>
  <div class="quota-card">
    <div class="quota-head">
      <span class="quota-title">额度</span>
      <span class="quota-adapter">{{ snap.adapterId }}</span>
      <span v-if="snap.stale" class="quota-stale">额度数据可能滞后</span>
    </div>

    <!-- 纯订阅：有 plan 无 windows → 「已包含在你的套餐内」 -->
    <div v-if="!windows.length" class="quota-included">
      {{ snap.plan ? "已包含在你的套餐内" : "暂无额度数据" }}
    </div>

    <div v-for="(w, i) in windows" :key="i" class="quota-window">
      <div class="quota-row">
        <span class="quota-kind">{{ kindLabel(w) }}</span>
        <span class="quota-pct" :class="pctClass(w)">{{ pctText(w) }}</span>
      </div>
      <div class="quota-bar">
        <div class="quota-bar-in" :class="pctClass(w)" :style="{ width: barWidth(w) }"></div>
      </div>
      <div class="quota-row quota-sub">
        <span>{{ usedText(w) }}</span>
        <span v-if="w.reset_at" :title="w.reset_at">重置 {{ shortTime(w.reset_at) }}</span>
      </div>
    </div>

    <div v-if="snap.plan" class="quota-plan">
      {{ snap.plan.name || "套餐" }}
      <template v-if="snap.plan.included">
        · 包含 {{ snap.plan.included.amount }} {{ unitLabel(snap.plan.included.unit) }}
      </template>
    </div>
  </div>
</template>

<script setup lang="ts">
//  前端隔离：本组件只认 QuotaSnapshot，不知道任何供应商细节。
// 多窗口时进度条并列展示；最紧一档（pct 最大）>80% 变琥珀色（）。
import { computed } from "vue";

interface QuotaWindow {
  kind: string;
  unit: string;
  used: number;
  limit: number | null;
  reset_at: string | null;
  pct: number | null;
  value_label?: string;
}
interface QuotaSnapshot {
  adapterId: string;
  scope: string;
  source: string;
  fetched_at: string;
  stale: boolean;
  windows: QuotaWindow[];
  plan: { name?: string; included?: { unit: string; amount: number } } | null;
}

const props = defineProps<{ snapshot: QuotaSnapshot | null }>();

const snap = computed<QuotaSnapshot>(() =>
  props.snapshot || {
    adapterId: "", scope: "account", source: "", fetched_at: "",
    stale: false, windows: [], plan: null,
  }
);
const windows = computed(() => snap.value.windows);

function kindLabel(w: QuotaWindow): string {
  return {
    rolling: "5h 滚动",
    weekly: "本周",
    monthly: "本月",
    day: "今日",
    week: "本周",
    month: "本月",
    credits: "余额",
  }[w.kind] || w.kind;
}
function unitLabel(unit: string): string {
  return { requests: "次", tokens: "tokens", percent: "%", usd: "USD" }[unit] || unit;
}
function pctText(w: QuotaWindow): string {
  if (w.pct == null) return w.value_label || "";
  return Math.round(w.pct) + "%";
}
function usedText(w: QuotaWindow): string {
  if (w.value_label) return w.unit === "usd" && w.kind === "credits" ? `${w.value_label} 可用` : w.value_label;
  const used = fmtN(w.used);
  if (w.unit === "percent") {
    if (w.limit != null) return `${fmtN(w.used)}% / ${fmtN(w.limit)}%`;
    return `已用 ${used}%`;
  }
  if (w.limit != null) return `${used} / ${fmtN(w.limit)} ${unitLabel(w.unit)}`;
  return `已用 ${used} ${unitLabel(w.unit)}`;
}
function fmtN(n: number): string {
  return n >= 1_000_000 ? (n / 1_000_000).toFixed(1) + "M" : n >= 1000 ? (n / 1000).toFixed(1) + "K" : String(Math.round(n));
}
function shortTime(ts: string): string {
  return ts ? ts.slice(5, 16).replace("T", " ") : "";
}
function barWidth(w: QuotaWindow): string {
  if (w.pct == null) return w.used > 0 ? "8%" : "0%";
  return Math.min(100, Math.max(2, w.pct)) + "%";
}
function pctClass(w: QuotaWindow): string {
  if (w.pct == null) return "";
  if (w.pct > 90) return "crit";
  if (w.pct > 80) return "warn";
  return "";
}
</script>

<style scoped>
.quota-card {
  background: var(--bg3);
  border: 1px solid var(--border-soft);
  border-radius: 10px;
  padding: 9px 11px;
  display: flex;
  flex-direction: column;
  gap: 6px;
}
.quota-head {
  display: flex;
  align-items: center;
  gap: 7px;
}
.quota-title {
  font-weight: 600;
  font-size: 12px;
}
.quota-adapter {
  font-size: 10px;
  color: var(--muted);
  background: var(--bg);
  border: 1px solid var(--border-soft);
  border-radius: 7px;
  padding: 0 6px;
  line-height: 15px;
}
.quota-stale {
  font-size: 10px;
  color: var(--amber);
}
.quota-included {
  font-size: 11.5px;
  color: var(--muted-2);
}
.quota-window {
  display: flex;
  flex-direction: column;
  gap: 3px;
}
.quota-row {
  display: flex;
  justify-content: space-between;
  align-items: baseline;
  font-size: 11px;
}
.quota-kind {
  color: var(--muted-2);
}
.quota-pct {
  font-variant-numeric: tabular-nums;
  color: var(--text);
}
.quota-pct.warn {
  color: var(--amber);
}
.quota-pct.crit {
  color: var(--red);
}
.quota-bar {
  height: 6px;
  border-radius: 4px;
  background: var(--bg);
  border: 1px solid var(--border-soft);
  overflow: hidden;
}
.quota-bar-in {
  height: 100%;
  border-radius: 4px;
  background: var(--blue);
  transition: width 0.4s ease;
}
.quota-bar-in.warn {
  background: var(--amber);
}
.quota-bar-in.crit {
  background: var(--red);
}
.quota-sub {
  font-size: 10px;
  color: var(--muted-2);
}
.quota-plan {
  font-size: 10.5px;
  color: var(--muted);
  border-top: 1px dashed var(--border-soft);
  padding-top: 5px;
}
</style>
