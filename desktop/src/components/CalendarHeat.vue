<template>
  <div class="cal">
    <div class="cal-top">
      <button class="cal-nav" title="上一月" @click="shiftMonth(-1)">◀</button>
      <span class="cal-title">{{ view.year }}年{{ view.month + 1 }}月</span>
      <button class="cal-nav" title="下一月" @click="shiftMonth(1)">▶</button>
      <span class="cal-legend"><i class="lg" style="opacity:0.15"></i><i class="lg" style="opacity:0.4"></i><i class="lg" style="opacity:0.65"></i><i class="lg" style="opacity:1"></i><span>少→多</span></span>
    </div>
    <div class="cal-body">
      <div class="cal-ops">
        <button class="cal-q" @click="$emit('quick', 'yesterday')">昨日</button>
        <button class="cal-q" @click="$emit('quick', '7d')">近7天</button>
        <button class="cal-q" @click="$emit('quick', '30d')">近30天</button>
      </div>
      <div class="cal-right">
        <div class="cal-dow">
          <span v-for="w in weeks" :key="w">{{ w }}</span>
        </div>
        <div class="cal-grid">
          <template v-for="(cell, i) in cells" :key="i">
            <span v-if="!cell" class="cal-cell cal-blank"></span>
            <button
              v-else
              class="cal-cell"
              :class="cellCls(cell)"
              :style="cell.style"
              :title="cell.date + (cell.requests > 0 ? ' · ' + cell.requests + ' 次' : ' · 无请求')"
              :disabled="cell.disabled"
              @click="pick(cell.date)"
            ></button>
          </template>
        </div>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
import { onMounted, ref, watch } from "vue";
import { api } from "../api";

const props = defineProps<{ service: string }>();
const emit = defineEmits<{
  select: [start: string, end: string];
  quick: [key: string];
}>();

const BLUE = [79, 140, 255]; // --blue
const DOWS = ["一", "二", "三", "四", "五", "六", "日"];

const daily = ref<Record<string, number>>({});
const start = ref<string | null>(null); // 区间起点
const end = ref<string | null>(null); // 区间终点
const view = ref<{ year: number; month: number }>({
  year: new Date().getFullYear(),
  month: new Date().getMonth(),
});

interface Cell {
  date: string;
  requests: number;
  disabled: boolean;
  style: string;
}
const cells = ref<(Cell | null)[]>([]);
const weeks = DOWS;

function toISO(d: Date): string {
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`;
}
function startOfDay(d: Date): Date {
  return new Date(d.getFullYear(), d.getMonth(), d.getDate());
}

// 当月月历格子（周一开头，不足 7 的倍数用空位补齐），热力色按请求量分档
function buildCells() {
  const { year, month } = view.value;
  const first = new Date(year, month, 1);
  const daysInMonth = new Date(year, month + 1, 0).getDate();
  const lead = (first.getDay() + 6) % 7; // 周一=0 的偏移
  const total = Math.ceil((lead + daysInMonth) / 7) * 7;
  const today = startOfDay(new Date());
  const max = Math.max(1, ...Object.values(daily.value));
  const arr: (Cell | null)[] = [];
  for (let i = 0; i < total; i++) {
    if (i < lead) {
      arr.push(null);
      continue;
    }
    const day = i - lead + 1;
    const d = new Date(year, month, day);
    const date = toISO(d);
    const disabled = d.getTime() > today.getTime();
    const requests = daily.value[date] || 0;
    arr.push({
      date,
      requests,
      disabled,
      style: disabled ? "" : heatStyle(requests, max),
    });
  }
  cells.value = arr;
}

// 热力色：0 → 弱底；>0 按相对最大值分档深浅
function heatStyle(requests: number, max: number): string {
  if (requests <= 0) return `background: rgba(${BLUE[0]},${BLUE[1]},${BLUE[2]},0.07)`;
  const t = max > 0 ? requests / max : 0;
  const a = 0.2 + t * 0.8;
  return `background: rgba(${BLUE[0]},${BLUE[1]},${BLUE[2]},${a.toFixed(2)})`;
}

// 选中样式：今天细描边，起点/终点粗描边，区间内格子内边框高亮
function isInRange(date: string): boolean {
  if (!start.value || !end.value) return false;
  return date >= start.value && date <= end.value;
}
function cellCls(cell: Cell): Record<string, boolean> {
  return {
    "cal-today": !cell.disabled && cell.date === toISO(startOfDay(new Date())),
    "cal-in-range": !cell.disabled && isInRange(cell.date),
    "cal-end": !cell.disabled && (cell.date === start.value || cell.date === end.value),
    "cal-future": cell.disabled,
  };
}

// 点选：无选择或已有完整区间 → 重新开始；仅有起点 → 点终点生效（面板保持打开可微调）
function pick(date: string) {
  if (!start.value || (start.value && end.value)) {
    start.value = date;
    end.value = null;
    return;
  }
  if (date === start.value) {
    start.value = null;
    return;
  }
  const [a, b] = [start.value, date].sort();
  start.value = a;
  end.value = b;
  emit("select", a, b);
}

// 翻月：已有完整区间则清空；只有起点则保留（支持跨月继续选终点）；不越过当前月
function shiftMonth(delta: number) {
  const { year, month } = view.value;
  const nm = month + delta;
  const ny = year + Math.floor(nm / 12);
  const nmonth = ((nm % 12) + 12) % 12;
  const now = new Date();
  if (ny > now.getFullYear() || (ny === now.getFullYear() && nmonth > now.getMonth())) {
    return;
  }
  view.value = { year: ny, month: nmonth };
  if (end.value) {
    start.value = null;
    end.value = null;
  }
  load();
}

async function load() {
  try {
    const { year, month } = view.value;
    const first = toISO(new Date(year, month, 1));
    const last = toISO(new Date(year, month + 1, 0));
    const d = await api.getDaily(props.service, first, last);
    const map: Record<string, number> = {};
    (d?.daily || []).forEach((x: any) => {
      map[x.date] = Number(x.requests || 0);
    });
    daily.value = map;
    buildCells();
  } catch (_) {
    daily.value = {};
  }
}

onMounted(() => {
  load();
});
// 服务切换时刷新热力图并重置选择
watch(
  () => props.service,
  () => {
    start.value = null;
    end.value = null;
    load();
  }
);
</script>

<style scoped>
.cal {
  display: flex;
  flex-direction: column;
  gap: 7px;
  padding: 8px 10px 9px;
  background: var(--bg2);
  border: 1px solid var(--border-soft);
  border-radius: 10px;
  margin: 0 0 9px;
}
.cal-top {
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 5px;
}
.cal-nav {
  width: 16px;
  height: 16px;
  font-size: 8px;
  line-height: 1;
  color: var(--muted);
  background: transparent;
  border: 1px solid var(--border-soft);
  border-radius: 4px;
  transition: all 0.15s;
  padding: 0;
}
.cal-nav:hover {
  border-color: var(--blue);
  color: var(--text);
}
.cal-title {
  font-size: 10.5px;
  font-weight: 600;
  color: var(--text);
  min-width: 62px;
  text-align: center;
}
.cal-legend {
  display: inline-flex;
  align-items: center;
  gap: 2px;
  margin-left: auto;
  font-size: 8.5px;
  color: var(--muted);
}
.cal-legend .lg {
  display: inline-block;
  width: 7px;
  height: 7px;
  border-radius: 1.5px;
  background: rgb(79, 140, 255);
}
/* 左右布局：左侧操作列 + 右侧月历，无背景直接融入卡片 */
.cal-body {
  display: flex;
  gap: 12px;
  align-items: flex-start;
}
.cal-ops {
  flex: none;
  width: 70px;
  display: flex;
  flex-direction: column;
  justify-content: center;
  gap: 5px;
  padding-top: 12px; /* 与星期行底部对齐 */
}
.cal-q {
  padding: 5px 0;
  font-size: 10.5px;
  color: var(--muted);
  background: transparent;
  border: 1px solid var(--border-soft);
  border-radius: 6px;
  transition: all 0.15s;
}
.cal-q:hover {
  border-color: var(--blue);
  color: var(--text);
  background: var(--blue-dim);
}
.cal-right {
  flex: 1;
  display: flex;
  flex-direction: column;
  align-items: center;
}
.cal-dow {
  display: grid;
  grid-template-columns: repeat(7, 14px);
  gap: 2px;
  margin-bottom: 2px;
}
.cal-dow span {
  height: 11px;
  line-height: 11px;
  font-size: 8.5px;
  color: var(--muted-2);
  text-align: center;
}
.cal-grid {
  display: grid;
  grid-template-columns: repeat(7, 14px);
  gap: 2px;
}
.cal-cell {
  width: 14px;
  height: 14px;
  border: none;
  border-radius: 2px;
  padding: 0;
  cursor: pointer;
  transition: filter 0.1s;
}
.cal-cell:hover {
  filter: brightness(1.15);
}
.cal-blank {
  visibility: hidden;
}
.cal-cell.cal-future {
  cursor: default;
  visibility: hidden;
}
.cal-cell.cal-today {
  outline: 1px solid var(--blue);
  outline-offset: 0;
}
/* 选中区间：端点粗描边，区间内格子内边框高亮 */
.cal-cell.cal-in-range {
  box-shadow: inset 0 0 0 1.5px rgba(79, 140, 255, 0.9);
}
.cal-cell.cal-end {
  outline: 2px solid var(--blue);
  outline-offset: 0;
  box-shadow: inset 0 0 0 1.5px rgba(79, 140, 255, 0.9);
}
</style>
