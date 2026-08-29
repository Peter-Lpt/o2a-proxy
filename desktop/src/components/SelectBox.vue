<template>
  <div ref="rootEl" class="sb" :class="[{ open, disabled }, sizeClass]">
    <div v-if="allowCustom" class="sb-input">
      <input
        :value="modelValue"
        type="text"
        spellcheck="false"
        autocomplete="off"
        :placeholder="placeholder"
        :disabled="disabled"
        @focus="openMenu"
        @input="onInput"
        @keydown.esc="close"
        @keydown.down.prevent="moveHighlight(1)"
        @keydown.up.prevent="moveHighlight(-1)"
        @keydown.enter.prevent="pickHighlighted"
      />
      <button type="button" class="sb-chevron-btn" :title="open ? '收起' : '展开'" :disabled="disabled" @click="toggle">
        <Icon name="chevron-down" :size="12" :class="{ flip: open }" />
      </button>
    </div>
    <button
      v-else
      type="button"
      class="sb-btn"
      :disabled="disabled"
      :aria-expanded="open"
      aria-haspopup="listbox"
      @click="toggle"
      @keydown.esc="close"
      @keydown.down.prevent="moveHighlight(1)"
      @keydown.up.prevent="moveHighlight(-1)"
      @keydown.enter.prevent="pickHighlighted"
    >
      <span class="sb-label">
        {{ currentLabel }}
      </span>
      <Icon name="chevron-down" :size="12" :class="{ flip: open }" />
    </button>

    <div v-if="open" class="sb-mask" data-tauri-drag-region="false" @mousedown="close"></div>

    <div v-if="open" class="sb-menu" :style="menuStyle" role="listbox">
      <!-- §10.2-6：非 custom 模式同样提供搜索（模型一多不用滚动查找） -->
      <div v-if="!allowCustom && (opts.length > 8 || query)" class="sb-search">
        <input
          ref="searchEl"
          v-model="query"
          type="text"
          spellcheck="false"
          autocomplete="off"
          placeholder="搜索…"
          @keydown.esc.stop="close"
          @keydown.down.prevent="moveHighlight(1)"
          @keydown.up.prevent="moveHighlight(-1)"
          @keydown.enter.prevent="pickHighlighted"
        />
      </div>
      <button
        v-for="(opt, i) in visibleOptions"
        :key="opt.value"
        type="button"
        class="sb-opt"
        :class="{ active: opt.value === modelValue, highlighted: i === highlightIndex }"
        role="option"
        :aria-selected="opt.value === modelValue"
        @mouseenter="highlightIndex = i"
        @click="pick(opt.value)"
      >
        {{ opt.label }}
      </button>
      <div v-if="!visibleOptions.length" class="sb-empty">无匹配项</div>
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, ref, watch } from "vue";
import Icon from "./Icon.vue";

const props = defineProps<{
  modelValue: string;
  options?: (string | { value: string; label?: string })[];
  placeholder?: string;
  allowCustom?: boolean;
  size?: "sm" | "md";
  disabled?: boolean;
}>();

const emit = defineEmits<{ (e: "update:modelValue", v: string): void }>();

const rootEl = ref<HTMLElement | null>(null);
const searchEl = ref<HTMLInputElement | null>(null);
const open = ref(false);
const query = ref("");
const menuStyle = ref<Record<string, string>>({});
const highlightIndex = ref(-1);

const opts = computed(() =>
  (props.options || []).map((o) =>
    typeof o === "string"
      ? { value: o, label: o }
      : { value: o.value, label: o.label || o.value }
  )
);

const currentLabel = computed(
  () =>
    opts.value.find((o) => o.value === props.modelValue)?.label ||
    props.modelValue ||
    props.placeholder ||
    ""
);

const visibleOptions = computed(() => {
  if (!query.value) return opts.value;
  const q = query.value.toLowerCase();
  return opts.value.filter(
    (o) => o.value.toLowerCase().includes(q) || (o.label || "").toLowerCase().includes(q)
  );
});

const sizeClass = computed(() => (props.size === "sm" ? "sb-sm" : "sb-md"));

// ---- 视口感知定位 ----
// 下拉菜单是 position:fixed，但会被 webview 窗口边界裁剪：小窗口（如悬浮窗
// 默认高度 ~230px）里菜单超出窗口底部的选项点不到。因此：
// 1. maxHeight 按窗口剩余空间收敛（不再固定 220px），菜单内部滚动选择；
// 2. 下方空间不足且上方更宽裕时向上翻转（bottom 锚定，不依赖渲染后高度）。
const MENU_MAX_H = 220;
const MENU_MIN_H = 96; // 最小可视高度：保证至少能滚出几个选项
const MENU_GAP = 4;
const VIEWPORT_MARGIN = 8;

function computeMenuStyle(): Record<string, string> {
  const root = rootEl.value;
  if (!root) return {};
  const rect = root.getBoundingClientRect();
  const vh = window.innerHeight;
  const below = vh - rect.bottom - VIEWPORT_MARGIN;
  const above = rect.top - VIEWPORT_MARGIN;
  const base = {
    left: `${rect.left}px`,
    width: `${rect.width}px`,
  };
  // 下方放得下（或上下都不够时优先向下，符合直觉）
  if (below >= MENU_MIN_H || below >= above) {
    return {
      ...base,
      top: `${rect.bottom + MENU_GAP}px`,
      maxHeight: `${Math.max(MENU_MIN_H, Math.min(MENU_MAX_H, below))}px`,
    };
  }
  // 向上翻转
  return {
    ...base,
    bottom: `${vh - rect.top + MENU_GAP}px`,
    maxHeight: `${Math.max(MENU_MIN_H, Math.min(MENU_MAX_H, above))}px`,
  };
}

function openMenu() {
  if (!rootEl.value) return;
  menuStyle.value = computeMenuStyle();
  open.value = true;
  // 高亮当前值，便于键盘直接确认
  const idx = visibleOptions.value.findIndex((o) => o.value === props.modelValue);
  highlightIndex.value = idx >= 0 ? idx : 0;
  // 非 custom 模式打开后聚焦搜索框（下一帧等渲染）
  if (!props.allowCustom) {
    nextTick(() => searchEl.value?.focus());
  }
}

function close() {
  open.value = false;
  highlightIndex.value = -1;
  if (!props.allowCustom) query.value = ""; // custom 模式的 query 即输入文本，不清
}

function toggle() {
  if (open.value) close();
  else openMenu();
}

function pick(v: string) {
  emit("update:modelValue", v);
  close();
}

// 键盘导航：↑↓ 移动高亮，Enter 确认，Esc 收起
function moveHighlight(delta: number) {
  if (!open.value) {
    openMenu();
    return;
  }
  const n = visibleOptions.value.length;
  if (!n) return;
  const cur = highlightIndex.value < 0 ? 0 : highlightIndex.value;
  highlightIndex.value = (cur + delta + n) % n;
  const menu = rootEl.value?.querySelector<HTMLElement>(".sb-menu");
  const opt = menu?.querySelector<HTMLElement>(`.sb-opt:nth-child(${highlightIndex.value + 1})`);
  opt?.scrollIntoView({ block: "nearest" });
}

function pickHighlighted() {
  if (!open.value) {
    openMenu();
    return;
  }
  const list = visibleOptions.value;
  const idx = highlightIndex.value >= 0 ? highlightIndex.value : 0;
  if (list[idx]) pick(list[idx].value);
}

function onInput(e: Event) {
  query.value = (e.target as HTMLInputElement).value;
  emit("update:modelValue", query.value);
  if (!open.value) openMenu();
}

function onViewportChange() {
  if (open.value) {
    // 滚动/缩放时按当前视口重新定位（含向下/向上翻转与限高），保持对齐输入框
    const root = rootEl.value;
    if (!root) return close();
    menuStyle.value = computeMenuStyle();
  }
}

// 点击组件外部任意位置收起下拉（更可靠，不依赖 fixed 遮罩层）
function onDocDown(e: MouseEvent) {
  if (!open.value) return;
  const root = rootEl.value;
  if (root && !root.contains(e.target as Node)) close();
}

watch(
  () => props.modelValue,
  (v) => {
    if (props.allowCustom) query.value = v;
  }
);

// 非 custom 模式搜索时高亮复位，避免指向被过滤掉的项
watch(query, () => {
  if (!props.allowCustom && open.value) highlightIndex.value = 0;
});

window.addEventListener("scroll", onViewportChange, true);
window.addEventListener("resize", onViewportChange);
document.addEventListener("mousedown", onDocDown);
onBeforeUnmount(() => {
  window.removeEventListener("scroll", onViewportChange, true);
  window.removeEventListener("resize", onViewportChange);
  document.removeEventListener("mousedown", onDocDown);
});
</script>

<style scoped>
.sb {
  position: relative;
  min-width: 0;
}
.sb.disabled {
  opacity: 0.55;
  pointer-events: none;
}
.sb-input {
  position: relative;
  display: flex;
  align-items: center;
}
.sb-input input {
  width: 100%;
  background: var(--bg);
  border: 1px solid var(--border-soft);
  border-radius: 8px;
  padding: 7px 28px 7px 9px;
  font-size: 12px;
  color: var(--text);
  outline: none;
  transition: border-color 0.15s, box-shadow 0.15s;
}
.sb-input input:focus {
  border-color: var(--blue);
  box-shadow: 0 0 0 3px rgba(79, 140, 255, 0.15);
}
.sb-chevron-btn {
  position: absolute;
  right: 4px;
  top: 50%;
  transform: translateY(-50%);
  width: 22px;
  height: 22px;
  display: flex;
  align-items: center;
  justify-content: center;
  color: var(--muted);
  border-radius: 6px;
}
.sb-chevron-btn:hover {
  color: var(--text);
  background: var(--bg3);
}
.sb-btn {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 6px;
  width: 100%;
  background: var(--bg);
  border: 1px solid var(--border-soft);
  border-radius: 8px;
  padding: 6px 28px 6px 9px;
  font-size: 12px;
  color: var(--text);
  position: relative;
  transition: border-color 0.15s, box-shadow 0.15s;
}
.sb-btn:hover,
.sb.open .sb-btn {
  border-color: var(--blue);
}
.sb-md .sb-btn {
  padding: 7px 28px 7px 9px;
}
.sb-sm .sb-btn {
  padding: 3px 24px 3px 8px;
  font-size: 11px;
}
.sb-label {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  display: inline-flex;
  align-items: center;
  gap: 5px;
}
.sb-chevron-btn svg,
.sb-btn > svg {
  transition: transform 0.15s;
  color: var(--muted);
}
.sb-chevron-btn svg.flip,
.sb-btn > svg.flip {
  transform: rotate(180deg);
}
.sb-menu {
  position: fixed;
  z-index: 1000;
  background: var(--bg3);
  border: 1px solid var(--border);
  border-radius: 10px;
  box-shadow: var(--shadow);
  overflow-y: auto;
  padding: 4px;
  animation: sb-in 0.12s ease-out;
}
/* 展开时的全屏透明遮罩：点击任意位置收起下拉。
   必须标记 data-tauri-drag-region=false，否则悬浮窗的拖拽区会吞掉
   mousedown，导致点击拖拽区域（标题/统计区等）下拉无法收起。 */
.sb-mask {
  position: fixed;
  inset: 0;
  z-index: 999;
  cursor: default;
}
@keyframes sb-in {
  from {
    opacity: 0;
    transform: translateY(-3px);
  }
  to {
    opacity: 1;
    transform: translateY(0);
  }
}
.sb-opt {
  display: block;
  width: 100%;
  text-align: left;
  padding: 7px 9px;
  border-radius: 7px;
  font-size: 12px;
  color: var(--text);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.sb-opt:hover {
  background: var(--blue-dim);
}
.sb-opt.active {
  background: var(--blue-dim);
  color: var(--blue);
}
.sb-opt.highlighted {
  background: var(--blue-dim);
}
.sb-opt.highlighted:not(.active) {
  color: var(--text);
  outline: 1px solid rgba(79, 140, 255, 0.5);
  outline-offset: -1px;
}
.sb-empty {
  padding: 8px;
  font-size: 11px;
  color: var(--muted-2);
  text-align: center;
}
.sb-search {
  padding: 2px 2px 4px;
  border-bottom: 1px solid var(--border-soft);
  margin-bottom: 3px;
}
.sb-search input {
  width: 100%;
  background: var(--bg);
  border: 1px solid var(--border-soft);
  border-radius: 6px;
  padding: 4px 8px;
  font-size: 11.5px;
  color: var(--text);
  outline: none;
}
.sb-search input:focus {
  border-color: var(--blue);
}
</style>
