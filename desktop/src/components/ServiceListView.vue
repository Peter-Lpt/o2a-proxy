<template>
  <div class="slv">
    <div class="slv-toolbar">
      <input ref="searchInput" v-model="query" type="text" class="slv-search" placeholder="搜索：名称 / 账号 / 端口 / 模型" spellcheck="false" />
      <SelectBox v-model="statusFilter" :options="statusOptions" size="sm" style="width: 96px" />
      <SelectBox v-model="sortKey" :options="sortOptions" size="sm" style="width: 104px" />
      <button class="icon-btn" :title="sortDir === 'asc' ? '升序' : '降序'" @click="flipSort">
        <Icon :name="sortDir === 'asc' ? 'arrow-up' : 'arrow-down'" :size="12" />
      </button>
      <button class="icon-btn" :class="{ spinning: usageLoading }" title="刷新今日用量" @click="loadUsage">
        <Icon name="refresh" :size="12" />
      </button>
    </div>

    <div class="slv-batch" v-if="selectedIds.size">
      <span class="slv-sel">已选 {{ selectedIds.size }}</span>
      <button class="btn btn-sm" @click="$emit('batchStart', ids)">启动</button>
      <button class="btn btn-sm" @click="$emit('batchStop', ids)">停止</button>
      <button class="btn btn-sm btn-danger" @click="$emit('batchRemove', ids)">删除</button>
      <button class="btn btn-sm" @click="selectedIds.clear()">取消选择</button>
    </div>

    <div class="slv-rows">
      <div v-for="r in rows" :key="r.id" class="slv-row" :class="{ off: !r.running }" @click="$emit('open', r.id)">
        <label class="slv-check" @click.stop><input type="checkbox" :checked="selectedIds.has(r.id)" @change="toggleSel(r.id)" /></label>
        <span class="dot" :class="{ on: r.running, busy: r.busy }"></span>
        <span class="slv-name" :title="r.comment">{{ r.comment }}</span>
        <span class="slv-meta">{{ r.accountLabel }}</span>
        <span class="slv-meta mono">:{{ r.port }}</span>
        <span class="slv-model" :title="r.model">{{ r.model }}</span>
        <span class="slv-usage">{{ usageText(r.id) }}</span>
        <span class="slv-acts" @click.stop>
          <button class="icon-btn" :title="r.running ? '停止' : '启动'" @click="$emit('toggle', r.id)">
            <Icon :name="r.running ? 'stop' : 'play'" :size="11" />
          </button>
          <button class="icon-btn" title="克隆" @click="$emit('clone', r.id)"><Icon name="copy" :size="11" /></button>
          <button class="icon-btn" title="删除" @click="$emit('remove', r.id)"><Icon name="trash" :size="11" /></button>
        </span>
      </div>
      <div v-if="!rows.length" class="slv-empty">
        {{ services.length ? "没有匹配的服务" : "尚未配置服务" }}
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
// §5.2A 服务列表视图：搜索 / 状态筛选 / 排序 / 批量启停删除 / 行内操作。
// 数据 = config services × status（运行态）；今日用量按需拉取（带 TTL 缓存，不进轮询）。
import { computed, ref, watch } from "vue";
import Icon from "./Icon.vue";
import SelectBox from "./SelectBox.vue";

// §10.4 键盘流：Ctrl+K 跳转服务时由父组件调用聚焦搜索
defineExpose({
  focusSearch: () => searchInput.value?.focus(),
});

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
  (e: "batchStart", ids: string[]): void;
  (e: "batchStop", ids: string[]): void;
  (e: "batchRemove", ids: string[]): void;
  (e: "usage", ids: string[], done: (usage: Record<string, { requests: number; cost: number }>) => void): void;
}>();

const query = ref("");
const searchInput = ref<HTMLInputElement | null>(null);
const statusFilter = ref<"all" | "running" | "stopped">("all");
const sortKey = ref<"name" | "port" | "status">("name");
const sortDir = ref<"asc" | "desc">("asc");
const selectedIds = ref(new Set<string>());
const usageLoading = ref(false);
const usage = ref<Record<string, { requests: number; cost: number }>>({});

const statusOptions = [
  { value: "all", label: "全部状态" },
  { value: "running", label: "运行中" },
  { value: "stopped", label: "已停止" },
];
const sortOptions = [
  { value: "name", label: "按名称" },
  { value: "port", label: "按端口" },
  { value: "status", label: "按状态" },
];

function flipSort() {
  sortDir.value = sortDir.value === "asc" ? "desc" : "asc";
}

const filtered = computed(() => {
  const q = query.value.trim().toLowerCase();
  let list = props.services;
  if (q) {
    list = list.filter((s) =>
      [s.comment, s.accountLabel, String(s.port), s.model, s.api]
        .join(" ")
        .toLowerCase()
        .includes(q)
    );
  }
  if (statusFilter.value === "running") list = list.filter((s) => s.running);
  if (statusFilter.value === "stopped") list = list.filter((s) => !s.running);
  return list;
});

const rows = computed(() => {
  const dir = sortDir.value === "asc" ? 1 : -1;
  const list = [...filtered.value];
  list.sort((a, b) => {
    if (sortKey.value === "port") return (Number(a.port) - Number(b.port)) * dir;
    if (sortKey.value === "status") return (Number(b.running) - Number(a.running)) * dir;
    return a.comment.localeCompare(b.comment) * dir;
  });
  return list;
});

const ids = computed(() => Array.from(selectedIds.value));

function toggleSel(id: string) {
  const next = new Set(selectedIds.value);
  if (next.has(id)) next.delete(id);
  else next.add(id);
  selectedIds.value = next;
}

watch(
  () => props.services.map((s) => s.id).join(","),
  () => {
    // 清理已删除服务的勾选
    const alive = new Set(props.services.map((s) => s.id));
    const next = new Set(Array.from(selectedIds.value).filter((id) => alive.has(id)));
    if (next.size !== selectedIds.value.size) selectedIds.value = next;
  }
);

function usageText(id: string): string {
  const u = usage.value[id];
  if (!u) return "";
  return `今日 ${u.requests} 次` + (u.cost ? ` / ¥${u.cost.toFixed(2)}` : "");
}

// 首次打开批量拉一次今日用量；后续手动刷新
watch(
  () => props.services.length,
  () => loadUsage(),
  { immediate: true }
);

function loadUsage() {
  if (!props.services.length || usageLoading.value) return;
  usageLoading.value = true;
  emit("usage", props.services.map((s) => s.id), (result) => {
    usage.value = { ...usage.value, ...result };
    usageLoading.value = false;
  });
}
</script>

<style scoped>
.slv {
  display: flex;
  flex-direction: column;
  gap: 6px;
  margin-bottom: 10px;
}
.slv-toolbar {
  display: flex;
  gap: 6px;
  align-items: center;
}
.slv-search {
  flex: 1;
  min-width: 0;
  background: var(--bg);
  border: 1px solid var(--border-soft);
  border-radius: 8px;
  padding: 5px 9px;
  font-size: 11.5px;
  color: var(--text);
  outline: none;
}
.slv-search:focus {
  border-color: var(--blue);
}
.slv-batch {
  display: flex;
  gap: 6px;
  align-items: center;
  background: var(--blue-dim);
  border-radius: 8px;
  padding: 5px 8px;
}
.slv-sel {
  font-size: 10.5px;
  color: var(--blue);
  margin-right: 2px;
}
.slv-rows {
  display: flex;
  flex-direction: column;
  gap: 3px;
}
.slv-row {
  display: flex;
  align-items: center;
  gap: 7px;
  background: var(--bg);
  border: 1px solid var(--border-soft);
  border-radius: 8px;
  padding: 6px 8px;
  cursor: pointer;
  min-height: 28px;
}
.slv-row:hover {
  border-color: var(--blue);
}
.slv-row.off {
  opacity: 0.75;
}
.slv-check {
  flex: none;
  display: inline-flex;
  cursor: pointer;
}
.slv-name {
  flex: 1.2;
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
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  max-width: 76px;
}
.slv-meta.mono {
  font-family: var(--font-mono);
}
.slv-model {
  flex: 1.4;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  font-size: 11px;
  color: var(--muted);
}
.slv-usage {
  flex: none;
  font-size: 10.5px;
  color: var(--muted-2);
  font-variant-numeric: tabular-nums;
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
</style>
