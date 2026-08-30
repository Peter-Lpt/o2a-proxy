<template>
  <div ref="rootEl" class="slv">
    <TransitionGroup tag="div" class="slv-rows" name="slvrow" :class="{ dragging: !!dragId }" appear>
      <div
        v-for="(r, i) in rows"
        :key="r.id"
        class="slv-row"
        :class="{ off: !r.running, dragging: dragId === r.id }"
        :style="{ '--i': Math.min(i, 12) }"
        :data-id="r.id"
        tabindex="0"
        @click="onRowClick(r.id)"
        @keydown.enter.prevent="$emit('open', r.id)"
        @keydown.up.prevent="focusRow(i - 1)"
        @keydown.down.prevent="focusRow(i + 1)"
      >
        <span class="slv-grip" title="拖动调整顺序" @pointerdown="onDragStart($event, r.id)">
          <Icon name="grip" :size="12" />
        </span>
        <span class="dot" :class="{ on: r.running, busy: r.busy }"></span>
        <span class="slv-name" :title="r.comment">{{ r.comment }}</span>
        <span class="slv-meta mono">:{{ r.port }}</span>
        <span class="slv-model" :class="{ empty: !r.model }" :title="r.model || '未配置模型'">{{ r.model || "未配置模型" }}</span>
        <span class="slv-acts" @click.stop>
          <button class="icon-btn" :title="r.running ? '停止' : '启动'" @click="$emit('toggle', r.id)">
            <Icon :name="r.running ? 'stop' : 'play'" :size="11" />
          </button>
          <button class="icon-btn" title="克隆" @click="$emit('clone', r.id)"><Icon name="copy" :size="11" /></button>
          <button class="icon-btn" title="删除" @click="$emit('remove', r.id)"><Icon name="trash" :size="11" /></button>
        </span>
      </div>
    </TransitionGroup>
    <div v-if="!rows.length" class="slv-empty">尚未配置服务</div>
  </div>
</template>

<script setup lang="ts">
// §5.2A 服务列表视图（v2 精简）：去掉搜索 / 状态筛选 / 排序下拉 / 批量勾选 / 今日用量，
// 职责收敛为两件事：
//   1) 展示服务行（状态灯 / 名称 / 端口 / 模型 / 行内启停·克隆·删除），点击行进入配置页；
//   2) 拖动 ⠿ 调整服务顺序：指针拖拽 + TransitionGroup FLIP 平滑换位。
//      每越一格 emit('reorder')，由父组件按新顺序重排 cfg.services；松手 emit('reorderEnd')
//      触发父组件直接保存（顺序属于配置本体，无需再到配置页手动保存）。
import { computed, onUnmounted, ref } from "vue";
import Icon from "./Icon.vue";

export interface ServiceRow {
  id: string;
  comment: string;
  accountLabel: string;
  api: string;
  model: string;
  port: number | string;
  host: string;
  running: boolean;
  busy: boolean;
}

const props = defineProps<{ services: ServiceRow[] }>();
const emit = defineEmits<{
  (e: "open", id: string): void;
  (e: "toggle", id: string): void;
  (e: "clone", id: string): void;
  (e: "remove", id: string): void;
  (e: "reorder", ids: string[]): void;
  (e: "reorderEnd"): void;
}>();

const rows = computed(() => props.services);

// ---------- 键盘可达性 ----------
const rootEl = ref<HTMLElement | null>(null);
// §10.4 Ctrl+K：打开列表后聚焦首行（原聚焦搜索框，搜索已移除）
defineExpose({
  focusList: () => rootEl.value?.querySelector<HTMLElement>(".slv-row")?.focus(),
});

function focusRow(i: number) {
  const items = rootEl.value?.querySelectorAll<HTMLElement>(".slv-row");
  if (!items || !items.length) return;
  const el = items[Math.max(0, Math.min(items.length - 1, i))];
  el?.focus();
  el?.scrollIntoView({ block: "nearest" });
}

// ---------- 拖动排序（指针拖拽，越格即重排，FLIP 负责平滑动画） ----------
// 监听必须挂 window 而不是手柄元素：Vue 重排列表时对已有节点做「先摘除再插入」的
// DOM 移动，元素离开文档树的瞬间其上的指针捕获会被浏览器隐式释放（Pointer Events
// 规范），挂在手柄上时第一次换位后拖动即中断；window 级监听不受 DOM 变动影响，
// 按住即可连续拖过多个位置。
const dragId = ref<string | null>(null);
let dragMoved = false;
let clickGuardUntil = 0;
let stopDrag: (() => void) | null = null;

function onDragStart(e: PointerEvent, id: string) {
  if (e.pointerType === "mouse" && e.button !== 0) return;
  e.preventDefault(); // 不触发文本选择 / 原生拖拽
  stopDrag?.(); // 防御：上一会话未正常结束时先清理
  dragId.value = id;
  dragMoved = false;
  const onMove = (ev: PointerEvent) => onDragMove(ev);
  const onUp = () => {
    clickGuardUntil = Date.now() + 150;
    stopDrag?.();
    if (dragMoved) emit("reorderEnd");
  };
  stopDrag = () => {
    window.removeEventListener("pointermove", onMove);
    window.removeEventListener("pointerup", onUp);
    window.removeEventListener("pointercancel", onUp);
    stopDrag = null;
    dragId.value = null;
  };
  window.addEventListener("pointermove", onMove);
  window.addEventListener("pointerup", onUp);
  window.addEventListener("pointercancel", onUp);
}

onUnmounted(() => stopDrag?.());

// 无指针捕获后，拖拽收尾的 click 可能落在行内任意元素上：短暂窗口内忽略，
// 避免拖完一下直接跳进配置页
function onRowClick(id: string) {
  if (Date.now() < clickGuardUntil) return;
  emit("open", id);
}

function onDragMove(ev: PointerEvent) {
  const id = dragId.value;
  if (!id) return;
  const container = rootEl.value?.querySelector<HTMLElement>(".slv-rows");
  if (!container) return;
  const items = container.querySelectorAll<HTMLElement>(".slv-row");
  const n = items.length;
  if (n < 2) return;
  const from = rows.value.findIndex((r) => r.id === id);
  if (from < 0) return;
  // 行高一致（行内全部单行省略），直接按指针在容器内的内容坐标换算目标槽位。
  // 不依赖动画中的瞬时 DOM 位置，避免 FLIP 位移与命中检测互相反馈导致抖动。
  const first = items[0].getBoundingClientRect();
  const box = container.getBoundingClientRect();
  const slot = first.height + 3; // 3px = .slv-rows gap
  if (slot <= 0) return;
  // 接近上下边缘时自动滚动（列表由外层 main 滚动时同样生效）
  const scroller = container.closest<HTMLElement>("main");
  const EDGE = 18;
  if (scroller) {
    const sb = scroller.getBoundingClientRect();
    if (ev.clientY < sb.top + EDGE) scroller.scrollTop -= 7;
    else if (ev.clientY > sb.bottom - EDGE) scroller.scrollTop += 7;
  }
  const y = ev.clientY - box.top + container.scrollTop;
  const target = Math.max(0, Math.min(n - 1, Math.floor(y / slot)));
  if (target === from) return;
  dragMoved = true;
  const ids = rows.value.map((r) => r.id);
  ids.splice(from, 1);
  ids.splice(target, 0, id);
  emit("reorder", ids);
}
</script>

<style scoped>
.slv {
  display: flex;
  flex-direction: column;
  gap: 6px;
}
.slv-rows {
  display: flex;
  flex-direction: column;
  gap: 3px;
}
.slv-rows.dragging {
  user-select: none;
  cursor: grabbing;
}
.slv-row {
  display: flex;
  align-items: center;
  gap: 7px;
  background: var(--bg);
  border: 1px solid var(--border-soft);
  border-radius: 8px;
  padding: 5px 8px 5px 5px;
  cursor: pointer;
  min-height: 28px;
  transition: border-color 0.15s, background 0.15s, box-shadow 0.15s, opacity 0.15s;
}
.slv-row:hover {
  border-color: var(--blue);
}
.slv-row.off {
  opacity: 0.78;
}
.slv-row.dragging {
  opacity: 0.92;
  background: var(--blue-dim);
  border-color: rgba(79, 140, 255, 0.55);
  box-shadow: 0 5px 16px rgba(0, 0, 0, 0.3);
  position: relative;
  z-index: 2;
}
.slv-grip {
  flex: none;
  display: inline-flex;
  align-items: center;
  padding: 3px 2px;
  border-radius: 5px;
  color: var(--muted-2);
  cursor: grab;
  touch-action: none; /* 触屏拖拽不被页面滚动手势接管 */
}
.slv-grip:hover {
  color: var(--text);
  background: var(--bg3);
}
.slv-rows.dragging .slv-grip {
  cursor: grabbing;
}
.slv-name {
  flex: 1.1;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  font-weight: 600;
  font-size: 12px;
}
.slv-meta {
  flex: none;
  font-size: 10.5px;
  color: var(--muted-2);
  font-family: var(--font-mono);
}
.slv-model {
  flex: 1;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  font-size: 11px;
  color: var(--muted);
}
.slv-model.empty {
  color: var(--muted-2);
}
.slv-acts {
  flex: none;
  display: inline-flex;
  gap: 2px;
}
.slv-empty {
  text-align: center;
  font-size: 11px;
  color: var(--muted-2);
  padding: 14px 0;
}

/* FLIP：拖动换位时其他行平滑滑动 */
.slvrow-move {
  transition: transform 0.22s cubic-bezier(0.22, 0.61, 0.36, 1);
}
/* 展开/新增服务时行级联入场（--i 由行内样式给出，封顶避免长列表延迟过久） */
.slvrow-enter-active {
  transition: opacity 0.2s ease, transform 0.24s cubic-bezier(0.22, 0.61, 0.36, 1);
  transition-delay: calc(var(--i) * 22ms);
}
.slvrow-enter-from {
  opacity: 0;
  transform: translateY(-6px);
}
/* 列表不做筛选，仅在服务被删除时移除行：直接消失，让 move 动画补位 */
.slvrow-leave-active {
  display: none;
}
</style>
