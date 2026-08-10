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
        @keydown.enter.prevent="pickFirst"
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
      @click="toggle"
      @keydown.esc="close"
    >
      <span class="sb-label">
        <span v-if="currentOpt && currentOpt.dot" :class="['sb-dot', currentOpt.dot]"></span>
        {{ currentLabel }}
      </span>
      <Icon name="chevron-down" :size="12" :class="{ flip: open }" />
    </button>

    <div v-if="open" class="sb-mask" data-tauri-drag-region="false" @mousedown="close"></div>

    <div v-if="open" class="sb-menu" :style="menuStyle">
      <button
        v-for="opt in visibleOptions"
        :key="opt.value"
        type="button"
        class="sb-opt"
        :class="{ active: opt.value === modelValue }"
        @click="pick(opt.value)"
      >
        <span v-if="opt.dot" :class="['sb-dot', opt.dot]"></span>
        {{ opt.label }}
      </button>
      <div v-if="!visibleOptions.length" class="sb-empty">无匹配项</div>
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from "vue";
import Icon from "./Icon.vue";

const props = defineProps<{
  modelValue: string;
  options?: (string | { value: string; label?: string; dot?: string })[];
  placeholder?: string;
  allowCustom?: boolean;
  size?: "sm" | "md";
  disabled?: boolean;
}>();

const emit = defineEmits<{ (e: "update:modelValue", v: string): void }>();

const rootEl = ref<HTMLElement | null>(null);
const open = ref(false);
const query = ref("");
const menuStyle = ref<Record<string, string>>({});

const opts = computed(() =>
  (props.options || []).map((o) =>
    typeof o === "string"
      ? { value: o, label: o }
      : { value: o.value, label: o.label || o.value, dot: o.dot }
  )
);

const currentOpt = computed(() => opts.value.find((o) => o.value === props.modelValue));

const currentLabel = computed(
  () =>
    opts.value.find((o) => o.value === props.modelValue)?.label ||
    props.modelValue ||
    props.placeholder ||
    ""
);

const visibleOptions = computed(() => {
  if (!props.allowCustom || !query.value) return opts.value;
  const q = query.value.toLowerCase();
  return opts.value.filter(
    (o) => o.value.toLowerCase().includes(q) || (o.label || "").toLowerCase().includes(q)
  );
});

const sizeClass = computed(() => (props.size === "sm" ? "sb-sm" : "sb-md"));

function openMenu() {
  const root = rootEl.value;
  if (!root) return;
  const rect = root.getBoundingClientRect();
  menuStyle.value = {
    position: "fixed",
    top: `${rect.bottom + 4}px`,
    left: `${rect.left}px`,
    width: `${rect.width}px`,
    maxHeight: "220px",
  };
  open.value = true;
}

function close() {
  open.value = false;
}

function toggle() {
  if (open.value) close();
  else openMenu();
}

function pick(v: string) {
  emit("update:modelValue", v);
  close();
}

function onInput(e: Event) {
  query.value = (e.target as HTMLInputElement).value;
  emit("update:modelValue", query.value);
  if (!open.value) openMenu();
}

function pickFirst() {
  if (visibleOptions.value.length) pick(visibleOptions.value[0].value);
}

function onViewportChange() {
  if (open.value) {
    // 滚动/缩放时重新定位，保持对齐输入框
    const root = rootEl.value;
    if (!root) return close();
    const rect = root.getBoundingClientRect();
    menuStyle.value = {
      ...menuStyle.value,
      top: `${rect.bottom + 4}px`,
      left: `${rect.left}px`,
      width: `${rect.width}px`,
    };
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
.sb-dot {
  width: 6px;
  height: 6px;
  border-radius: 50%;
  flex: none;
  background: var(--muted-2);
}
.sb-dot.gray { background: #3a4250; }
.sb-dot.green { background: var(--green); }
.sb-dot.amber { background: var(--amber); }
.sb-dot.red { background: var(--red, #ef4444); }
.sb-dot.busy { background: var(--amber); animation: sb-blink 1.2s ease-in-out infinite; }
@keyframes sb-blink { 50% { opacity: 0.3; } }
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
.sb-empty {
  padding: 8px;
  font-size: 11px;
  color: var(--muted-2);
  text-align: center;
}
</style>
