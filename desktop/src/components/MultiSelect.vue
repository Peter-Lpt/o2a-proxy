<template>
  <div ref="rootEl" class="ms2" :class="{ open, disabled }">
    <button
      type="button"
      class="ms2-btn"
      :disabled="disabled"
      :aria-expanded="open"
      aria-haspopup="listbox"
      @click="toggle"
    >
      <span class="ms2-label">
        <template v-if="!selected.length">{{ placeholder || "未选择（不限制）" }}</template>
        <template v-else-if="selected.length <= 2">{{ selected.join("、") }}</template>
        <template v-else>{{ selected[0] }} 等 {{ selected.length }} 个</template>
      </span>
      <span v-if="selected.length" class="ms2-count">{{ selected.length }}</span>
      <Icon name="chevron-down" :size="12" :class="{ flip: open }" />
    </button>

    <div v-if="open" class="sb-mask" @mousedown="close"></div>

    <div v-if="open" class="ms2-menu" :style="menuStyle" role="listbox" aria-multiselectable="true">
      <div class="ms2-search">
        <input v-model="query" type="text" placeholder="搜索模型…" spellcheck="false" autocomplete="off" />
      </div>
      <div class="ms2-actions">
        <button type="button" class="link-btn" @click="selectAll">全选</button>
        <button type="button" class="link-btn" @click="clearAll">清空</button>
      </div>
      <button
        v-for="opt in visibleOptions"
        :key="opt"
        type="button"
        class="ms2-opt"
        :class="{ checked: isChecked(opt), locked: opt === locked }"
        role="option"
        :aria-selected="isChecked(opt)"
        @click="toggleOpt(opt)"
      >
        <span class="ms2-box">{{ isChecked(opt) ? "✓" : "" }}</span>
        <span class="ms2-name">{{ opt }}</span>
        <span v-if="opt === locked" class="ms2-lock">主模型</span>
      </button>
      <div v-if="!visibleOptions.length" class="ms2-empty">
        {{ (options || []).length ? "无匹配模型" : "↻ 从账号拉取模型列表后再选择" }}
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed, onBeforeUnmount, ref } from "vue";
import Icon from "./Icon.vue";

//  可见模型白名单多选器：搜索 + 复选 + 全选/清空 + 主模型锁定不可移除。
// 数据源 = 服务页模型缓存（与主模型下拉同一体系）。
const props = defineProps<{
  modelValue: string[];
  options?: string[];
  placeholder?: string;
  locked?: string; // 主模型：始终在列且不可移除
  disabled?: boolean;
}>();

const emit = defineEmits<{ (e: "update:modelValue", v: string[]): void }>();

const rootEl = ref<HTMLElement | null>(null);
const open = ref(false);
const query = ref("");
const menuStyle = ref<Record<string, string>>({});

const opts = computed(() => {
  const set = new Set(props.options || []);
  if (props.locked) set.add(props.locked);
  for (const v of props.modelValue || []) set.add(v);
  // 主模型置顶，其余按字母序
  return Array.from(set).sort((a, b) => {
    if (a === props.locked) return -1;
    if (b === props.locked) return 1;
    return a.localeCompare(b);
  });
});

const visibleOptions = computed(() => {
  const q = query.value.trim().toLowerCase();
  if (!q) return opts.value;
  // 支持前缀通配（如 qwen-*）
  if (q.endsWith("*")) {
    const p = q.slice(0, -1);
    return opts.value.filter((o) => o.toLowerCase().startsWith(p));
  }
  return opts.value.filter((o) => o.toLowerCase().includes(q));
});

const selected = computed(() => props.modelValue || []);

function isChecked(v: string): boolean {
  return selected.value.includes(v) || v === props.locked;
}

function toggleOpt(v: string) {
  if (v === props.locked) return; // 主模型不可移除
  const next = isChecked(v)
    ? selected.value.filter((x) => x !== v)
    : [...selected.value, v];
  emit("update:modelValue", next);
}

function selectAll() {
  emit("update:modelValue", [...opts.value]);
}

function clearAll() {
  emit("update:modelValue", props.locked ? [props.locked] : []);
}

const MENU_MAX_H = 220;
const MENU_MIN_H = 96;
const MENU_GAP = 4;
const VIEWPORT_MARGIN = 8;

function computeMenuStyle(): Record<string, string> {
  const root = rootEl.value;
  if (!root) return {};
  const rect = root.getBoundingClientRect();
  const vh = window.innerHeight;
  const below = vh - rect.bottom - VIEWPORT_MARGIN;
  const above = rect.top - VIEWPORT_MARGIN;
  const base = { left: `${rect.left}px`, width: `${Math.max(rect.width, 260)}px` };
  if (below >= MENU_MIN_H || below >= above) {
    return { ...base, top: `${rect.bottom + MENU_GAP}px`, maxHeight: `${Math.max(MENU_MIN_H, Math.min(MENU_MAX_H, below))}px` };
  }
  return { ...base, bottom: `${vh - rect.top + MENU_GAP}px`, maxHeight: `${Math.max(MENU_MIN_H, Math.min(MENU_MAX_H, above))}px` };
}

function openMenu() {
  menuStyle.value = computeMenuStyle();
  open.value = true;
}
function close() {
  open.value = false;
  query.value = "";
}
function toggle() {
  if (open.value) close();
  else openMenu();
}

function onViewportChange() {
  if (open.value) {
    const root = rootEl.value;
    if (!root) return close();
    menuStyle.value = computeMenuStyle();
  }
}
function onDocDown(e: MouseEvent) {
  if (!open.value) return;
  const root = rootEl.value;
  if (root && !root.contains(e.target as Node)) close();
}

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
.ms2 {
  position: relative;
  min-width: 0;
}
.ms2.disabled {
  opacity: 0.55;
  pointer-events: none;
}
.ms2-btn {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 6px;
  width: 100%;
  background: var(--bg);
  border: 1px solid var(--border-soft);
  border-radius: 8px;
  padding: 7px 9px;
  font-size: 12px;
  color: var(--text);
  transition: border-color 0.15s, box-shadow 0.15s;
}
.ms2-btn:hover,
.ms2.open .ms2-btn {
  border-color: var(--blue);
}
.ms2-label {
  flex: 1;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  text-align: left;
}
.ms2-count {
  flex: none;
  background: var(--blue-dim);
  color: var(--blue);
  border-radius: 9px;
  padding: 0 7px;
  font-size: 10.5px;
  line-height: 17px;
}
.ms2-btn > svg {
  flex: none;
  color: var(--muted);
  transition: transform 0.15s;
}
.ms2-btn > svg.flip {
  transform: rotate(180deg);
}
.ms2-menu {
  position: fixed;
  z-index: 1000;
  background: var(--bg3);
  border: 1px solid var(--border);
  border-radius: 10px;
  box-shadow: var(--shadow);
  overflow-y: auto;
  padding: 4px;
  animation: ms2-in 0.12s ease-out;
}
.ms2-menu .sb-mask {
  position: fixed;
  inset: 0;
  z-index: 999;
  cursor: default;
}
@keyframes ms2-in {
  from { opacity: 0; transform: translateY(-3px); }
  to { opacity: 1; transform: translateY(0); }
}
.ms2-search {
  padding: 2px;
}
.ms2-search input {
  width: 100%;
  background: var(--bg);
  border: 1px solid var(--border-soft);
  border-radius: 7px;
  padding: 5px 8px;
  font-size: 11.5px;
  color: var(--text);
  outline: none;
}
.ms2-search input:focus {
  border-color: var(--blue);
}
.ms2-actions {
  display: flex;
  gap: 10px;
  padding: 4px 6px;
  border-bottom: 1px solid var(--border-soft);
  margin-bottom: 3px;
}
.ms2-actions .link-btn {
  font-size: 10.5px;
}
.ms2-opt {
  display: flex;
  align-items: center;
  gap: 7px;
  width: 100%;
  text-align: left;
  padding: 6px 8px;
  border-radius: 7px;
  font-size: 12px;
  color: var(--text);
}
.ms2-opt:hover {
  background: var(--blue-dim);
}
.ms2-box {
  flex: none;
  width: 15px;
  height: 15px;
  border: 1px solid var(--border);
  border-radius: 4px;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  font-size: 10px;
  color: var(--blue);
  background: var(--bg);
}
.ms2-opt.checked .ms2-box {
  border-color: var(--blue);
  background: var(--blue-dim);
}
.ms2-opt.locked {
  cursor: default;
}
.ms2-name {
  flex: 1;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.ms2-lock {
  flex: none;
  font-size: 9.5px;
  color: var(--amber);
  background: var(--amber-dim);
  border-radius: 6px;
  padding: 0 6px;
  line-height: 15px;
}
.ms2-empty {
  padding: 10px;
  font-size: 11px;
  color: var(--muted-2);
  text-align: center;
}
</style>
