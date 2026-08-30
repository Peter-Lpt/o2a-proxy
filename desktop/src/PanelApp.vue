<template>
  <div class="popover">
    <header class="head">
      <div class="brand">
        <span class="logo"><Icon name="swap" :size="15" /></span>
        <div class="brand-txt">
          <div class="title">o2a-proxy</div>
          <div class="sub" id="headSub">{{ headSubText }}</div>
        </div>
        <span class="orb" :class="headOrbCls" :title="headOrbTitle"></span>
      </div>
      <div class="head-actions">
        <button class="icon-btn" :title="themeTitle" @click="onToggleTheme">
          <Icon :name="themeIcon" :size="14" />
        </button>
        <button class="btn btn-sm" title="添加服务" @click="addService"><Icon name="plus" :size="12" /> 添加服务</button>
        <button class="float-btn" :title="floatService ? '为「' + floatService + '」开启悬浮看板' : '开启全部悬浮看板'" @click="onToggleFloat"><Icon name="float" :size="12" /> 悬浮</button>
      </div>
    </header>

    <div class="svc-bar">
      <div class="svc-tabs-zone">
        <Transition name="swapfade">
          <div v-if="!useListView" class="svc-tabs" ref="svcTabs" @mousedown="onTabsDown" @wheel.prevent="onTabsWheel" @scroll="onTabsScroll">
            <button class="svc-tab" :class="{ active: selected === '__all__' }" @click="selectAll">
              <span class="dot" :class="{ on: anyRunning, busy: anyBusy }"></span>全部
            </button>
            <button
              v-for="s in serviceList"
              :key="s.id || s.comment"
              class="svc-tab"
              :class="{ active: selected === (s.id || s.comment) }"
              @click="selectService(s)"
              :title="s.comment + ' · ' + (s.client || 'auto') + ' · :' + (s.listen_address ?? '?')"
            >
              <span class="dot" :class="{ on: runningMap[s.id], busy: busyMap[s.id] }"></span>{{ s.comment }}
              <span class="power" @click.stop="toggleSvc(s.id)" :title="runningMap[s.id] ? '停止' : '启动'">
                <Icon :name="runningMap[s.id] ? 'stop' : 'play'" :size="10" />
              </span>
            </button>
          </div>
          <span v-else class="svc-mode-hint" aria-hidden="true">
            <Icon name="grip" :size="11" />拖动调整服务顺序 · 松手自动保存
          </span>
        </Transition>
        <span v-if="tabEdgeL && !useListView" class="tab-edge left" aria-hidden="true"></span>
        <span v-if="tabEdgeR && !useListView" class="tab-edge right" aria-hidden="true"></span>
      </div>
      <button class="icon-btn svc-toggle" :class="{ active: useListView }"
              :title="useListView ? '切换为标签栏' : '切换为服务列表（拖动排序）'"
              @click="toggleListView">
        <Icon :name="useListView ? 'panel' : 'chevron-down'" :size="12" />
      </button>
    </div>

    <!-- §5.2A 服务列表视图：服务 >6 自动启用，或手动切换；拖动行 ⠿ 调整顺序（松手自动保存） -->
    <Transition name="slvroll">
      <div v-if="useListView" class="slv-roll">
        <div class="slv-roll-clip">
          <div class="card slv-card">
            <ServiceListView ref="listViewRef" :services="listRows" @open="openServiceFromList" @toggle="toggleSvc"
                             @clone="cloneById" @remove="removeById"
                             @reorder="reorderServices" @reorder-end="onReorderEnd" />
          </div>
        </div>
      </div>
    </Transition>

    <div v-if="!anyRunning" class="bar off-bar">
      <span class="dot off"></span>
      <span>{{ offMeta }}</span>
      <span v-if="offError" class="off-err">{{ offError }}</span>
    </div>
    <div v-else class="bar on-bar">
      <span class="dot on"></span>
      <span>{{ onMeta }}</span>
      <span v-if="selected !== ALL && activeSvc?.context_1m" class="on-tag">1M</span>
    </div>

    <nav class="tabs">
      <button class="tab" :class="{ active: page === 'stats' }" @click="goPage('stats')">统计</button>
      <button class="tab" :class="{ active: page === 'config' }" @click="goPage('config')">配置</button>
      <button class="tab" :class="{ active: page === 'accounts' }" @click="goPage('accounts')">账号</button>
    </nav>

    <main>
      <!-- 统计（§10.1 拆分为 views/StatsView.vue） -->
      <section v-show="page === 'stats'" class="panel stats-panel" :class="{ active: page === 'stats' }">
        <StatsView :theme="theme" @add-service="addService" @go-accounts="goAccounts" />
      </section>
      <!-- 配置（§10.1 拆分为 views/ServicesView.vue） -->
      <section v-show="page === 'config'" class="panel" :class="{ active: page === 'config' }">
        <ServicesView
          @save="saveConfig()"
          @save-and-restart="saveAndRestart"
          @clone="cloneService()"
          @remove="removeSvc()"
          @refresh-models="refreshModels(true)"
          @reload="onLocationReload"
        />
      </section>
      <!-- 账号（§10.1 拆分为 views/AccountsView.vue） -->
      <section v-show="page === 'accounts'" class="panel" :class="{ active: page === 'accounts' }">
        <AccountsView :acc-stats="accStats" :acc-stats-state="accStatsState" @save="saveConfig()" />
      </section>
    </main>

    <footer class="foot">
      <span>o2a-proxy · v0.1.0</span>
      <button class="link-btn" @click="quitApp">退出应用</button>
    </footer>
    <div id="toast" class="toast" :class="{ show: !!toast, [toastType]: !!toast }">
      <span class="toast-msg">{{ toast }}</span>
      <button v-if="toastAction" class="toast-act" @click="onToastAction">{{ toastAction.label }}</button>
      <button v-if="toast && toastAction" class="toast-x" @click="dismissToast">×</button>
    </div>
    <ConfirmDialog
      :open="!!confirmBox"
      :title="confirmBox?.title || ''"
      :message="confirmBox?.message || ''"
      :ok-text="confirmBox?.okText || '确认'"
      @confirm="onConfirmOk"
      @cancel="confirmBox = null"
    />
  </div>
</template>

<script setup lang="ts">
import { computed, nextTick, onMounted, onUnmounted, reactive, ref, watch } from "vue";
import { emit, listen } from "@tauri-apps/api/event";
import { api } from "./api";
import ConfirmDialog from "./components/ConfirmDialog.vue";
import Icon from "./components/Icon.vue";
import ServiceListView, { type ServiceRow } from "./components/ServiceListView.vue";
import AccountsView from "./views/AccountsView.vue";
import ServicesView from "./views/ServicesView.vue";
import { applyTheme, getTheme, toggleTheme, watchSystemTheme, type Theme } from "./theme";
import {
  MODEL_CACHE_TTL,
  cacheKey,
  fmtModelTime,
  modelCache,
  performFetchModels,
  setModelHint,
} from "./composables/useModels";
import {
  loadLive,
  loadStats,
  modelFilter,
  stats,
} from "./stores/stats";
import StatsView from "./views/StatsView.vue";
import {
  accountById,
  activeSvc,
  anyBusy,
  anyRunning,
  busyMap,
  commitComment,
  commitModelsMap,
  floatService,
  runningMap,
  serviceList,
} from "./stores/services";
import { dirty, ensureSvcIds, newSvcId, page, selected, selectedSvc, snapCfg, status, cfg, ALL } from "./stores/config";
import {
  confirmBox,
  toast,
  toastType,
  toastAction,
  askConfirm,
  dismissToast,
  onConfirmOk,
  onToastAction,
  showToast,
} from "./stores/ui";

const svcTabs = ref<HTMLElement | null>(null);
let tabsDrag: { startX: number; startScroll: number; moved: boolean } | null = null;

// 服务标签栏：鼠标拖拽横向滚动 + 滚轮横向滚动（服务多时避免难用的窄滚动条）
function onTabsDown(e: MouseEvent) {
  // 启停按钮上的按下不触发拖动，保留点击语义
  if ((e.target as HTMLElement).closest(".power")) return;
  tabsDrag = { startX: e.clientX, startScroll: svcTabs.value?.scrollLeft || 0, moved: false };
  // 注意：不在 mousedown 时加 dragging 类（pointer-events:none 会吞掉 mouseup/click，导致标签无法切换）
  const onMove = (ev: MouseEvent) => {
    const d = tabsDrag;
    if (!d || !svcTabs.value) return;
    const dx = ev.clientX - d.startX;
    // 超过阈值才算拖动：此时才加 dragging 类（防 hover 闪烁）并开始滚动
    if (!d.moved && Math.abs(dx) > 4) {
      d.moved = true;
      svcTabs.value.classList.add("dragging");
    }
    if (d.moved) svcTabs.value.scrollLeft = d.startScroll - dx;
  };
  const onUp = () => {
    const wasDrag = tabsDrag?.moved;
    tabsDrag = null;
    svcTabs.value?.classList.remove("dragging");
    window.removeEventListener("mousemove", onMove);
    window.removeEventListener("mouseup", onUp);
    if (wasDrag) {
      // 拖动结束不选中服务：拦截本次 click
      document.addEventListener(
        "click",
        (ev) => {
          ev.stopPropagation();
          ev.preventDefault();
        },
        { capture: true, once: true }
      );
    }
  };
  window.addEventListener("mousemove", onMove);
  window.addEventListener("mouseup", onUp);
}

function onTabsWheel(e: WheelEvent) {
  const el = svcTabs.value;
  if (!el) return;
  const dx = Math.abs(e.deltaX) > Math.abs(e.deltaY) ? e.deltaX : e.deltaY;
  el.scrollLeft += dx;
}
const offError = ref("");
const theme = ref<Theme>(getTheme());
const tabEdgeL = ref(false);
const tabEdgeR = ref(false);

// 模型缓存子系统移至 composables/useModels.ts（与账号页连通性测试共享）
const accStats = reactive<Record<string, any>>({});
const accStatsState = reactive<Record<string, "loading" | "ok" | "err">>({});
let warmSeq = 0;

// ---------- 确认弹层（状态移至 stores/ui.ts） ----------

// ---------- 主题（深/浅/跟随系统） ----------
const themeIcon = computed(() => (theme.value === "system" ? "auto" : theme.value === "dark" ? "sun" : "moon"));
const themeTitle = computed(() =>
  theme.value === "system"
    ? "跟随系统（点击切为深色）"
    : theme.value === "dark"
      ? "切换到浅色"
      : "切换到深色"
);
function emitTheme() {
  emit("o2a-theme", theme.value).catch(() => {});
}
function onToggleTheme() {
  theme.value = toggleTheme();
  emitTheme();
}

// ---------- 服务标签栏：溢出渐隐提示 ----------
function onTabsScroll() {
  const el = svcTabs.value;
  if (!el) {
    tabEdgeL.value = tabEdgeR.value = false;
    return;
  }
  tabEdgeL.value = el.scrollLeft > 2;
  tabEdgeR.value = el.scrollLeft < el.scrollWidth - el.clientWidth - 2;
}

// 账号聚合统计（前端合并该账号下各服务的 getStats）
function loadAccountStats() {
  (cfg.accounts || []).forEach((acc: any) => {
    const svcs = (cfg.services || []).filter((s: any) => s.account === acc.id);
    if (!svcs.length) {
      accStats[acc.id] = { today: { cost: 0, requests: 0 }, month: { cost: 0, requests: 0 } };
      accStatsState[acc.id] = "ok";
      return;
    }
    // 已有数据时静默刷新，避免每 10s 轮询闪烁
    if (!accStats[acc.id]) accStatsState[acc.id] = "loading";
    Promise.all(svcs.map((s: any) => api.getStats(s.id || s.comment)))
      .then((res: any[]) => {
        let cost = 0, req = 0, mcost = 0, mreq = 0;
        res.forEach((r: any) => {
          cost += Number(r.today?.cost || 0);
          req += Number(r.today?.requests || 0);
          mcost += Number(r.month?.cost || 0);
          mreq += Number(r.month?.requests || 0);
        });
        accStats[acc.id] = { today: { cost, requests: req }, month: { cost: mcost, requests: mreq } };
        accStatsState[acc.id] = "ok";
      })
      .catch(() => {
        accStats[acc.id] = { today: { cost: 0, requests: 0 }, month: { cost: 0, requests: 0 } };
        accStatsState[acc.id] = "err";
      });
  });
}
// （accStatsText / testAccount / debounceTestAccount / addAccount / removeAccount
//   已移至 views/AccountsView.vue）

async function fetchModels(force = false) {
  if (selected.value === ALL) {
    setModelHint("选择具体服务后可拉取模型列表", false);
    return;
  }
  const s = activeSvc.value;
  if (!s) {
    setModelHint("");
    return;
  }
  const acc = accountById(s.account);
  if (!acc) {
    setModelHint("请先在「账号」页创建并绑定账号", false);
    return;
  }
  const baseUrl = String(acc.openai_url || "").trim();
  const apiKey = String(acc.api_key || "").trim();
  if (!baseUrl) {
    setModelHint("该账号未配置 OpenAI 端点，无法拉取模型列表", false);
    return;
  }
  const key = cacheKey(baseUrl, apiKey);
  const entry = modelCache.get(key);
  const now = Date.now();
  if (!force && entry && now - entry.fetchedAt < MODEL_CACHE_TTL) {
    setModelHint(`已加载 ${entry.models.length} 个模型（更新于 ${fmtModelTime(entry.fetchedAt)}）`, false);
    return;
  }
  if (!force && entry && entry.models.length) {
    if (!apiKey) {
      setModelHint(`模型可能已过期（上次成功 ${fmtModelTime(entry.fetchedAt)}）；该账号未填写 Key，无法刷新`, true);
      return;
    }
    setModelHint(`模型可能已过期（上次成功 ${fmtModelTime(entry.fetchedAt)}），正在后台刷新…`, false);
    void performFetchModels(baseUrl, apiKey).then((models) => {
      const curSvc = activeSvc.value;
      const curAcc = curSvc ? accountById(curSvc.account) : null;
      const curKey = curAcc
        ? cacheKey(String(curAcc.openai_url || "").trim(), String(curAcc.api_key || "").trim())
        : "";
      if (curKey !== key) return; // 用户已切换服务/账号，不污染当前提示
      if (models !== null) {
        setModelHint(`已加载 ${models.length} 个模型（更新于 ${fmtModelTime(Date.now())}）`, false);
      } else if (modelCache.get(key)?.models?.length) {
        setModelHint("后台刷新失败，已保留上次成功列表", true);
      }
    });
    return;
  }
  if (!apiKey) {
    setModelHint("该账号未填写 API Key，拉取模型列表需要 Key", false);
    return;
  }
  setModelHint("正在拉取模型列表…", false);
  const models = await performFetchModels(baseUrl, apiKey);
  const curSvc = activeSvc.value;
  const curAcc = curSvc ? accountById(curSvc.account) : null;
  const curKey = curAcc
    ? cacheKey(String(curAcc.openai_url || "").trim(), String(curAcc.api_key || "").trim())
    : "";
  if (curKey !== key) return; // 等待期间用户切换了服务/账号，不污染当前提示
  if (models !== null) {
    setModelHint(`已加载 ${models.length} 个模型（更新于 ${fmtModelTime(Date.now())}）`, false);
  } else if (modelCache.get(key)?.models?.length) {
    setModelHint("刷新失败，已保留上次成功列表；可点击右侧刷新重试", true);
  } else {
    setModelHint("拉取模型失败", true);
  }
}

async function refreshModels(force = true) {
  await fetchModels(force);
}

async function warmModels() {
  const first = (cfg.accounts || []).find(
    (a: any) => String(a.openai_url || "").trim() && String(a.api_key || "").trim()
  );
  if (!first) return;
  const baseUrl = String(first.openai_url).trim();
  const apiKey = String(first.api_key).trim();
  const entry = modelCache.get(cacheKey(baseUrl, apiKey));
  if (entry && Date.now() - entry.fetchedAt < MODEL_CACHE_TTL) return;
  if (entry?.inflight) return;
  const seq = ++warmSeq;
  try {
    const res = await api.fetchModels(baseUrl, apiKey);
    if (seq === warmSeq && res.ok) {
      modelCache.set(cacheKey(baseUrl, apiKey), { models: res.models, fetchedAt: Date.now() });
    }
  } catch (_) {}
}

// ---------- Toast（状态与逻辑移至 stores/ui.ts） ----------

// 退出应用：确认（运行中的代理会被停止）
function quitApp() {
  askConfirm(
    "退出应用",
    "确定退出 o2a-proxy？\n运行中的代理服务将被停止。",
    () => {
      api.quitApp();
    },
    "退出"
  );
}

// 服务标签以配置为准（添加/删除立即生效），运行态从 status 映射
const headOrbCls = computed(() => (anyBusy.value ? "busy" : anyRunning.value ? "running" : "stopped"));
const headOrbTitle = computed(() =>
  anyBusy.value ? "正在处理请求" : anyRunning.value ? "运行中" : "已停止"
);
const headSubText = computed(() =>
  anyBusy.value ? "代理处理中" : anyRunning.value ? "代理运行中" : "代理已停止"
);
function selectService(s: any) {
  selectedSvc.value = s;
  selected.value = s.id || s.comment;
}
function selectAll() {
  selectedSvc.value = null;
  selected.value = ALL;
}
function goPage(p: "stats" | "config" | "accounts") {
  if (p === page.value) return;
  if (dirty.value) {
    askConfirm(
      "有未保存的改动",
      "当前修改尚未保存，切换页面后将丢失。是否继续？",
      () => {
        snapCfg(); // 丢弃改动（回到快照）
        page.value = p;
      },
      "丢弃并切换"
    );
    return;
  }
  page.value = p;
}

// ---------- §5.2A 克隆服务 ----------
function cloneService(svc?: any) {
  const s = svc || activeSvc.value;
  if (!s) return;
  const used = new Set(
    (cfg.services || [])
      .map((x: any) => Number(x.listen_address))
      .filter((n: any) => Number.isFinite(n))
  );
  let port = Number(s.listen_address) || 11011;
  do {
    port = port >= 65535 ? 11011 : port + 1;
  } while (used.has(port));
  const copy = JSON.parse(JSON.stringify(s));
  copy.id = newSvcId();
  copy.comment = (s.comment || "服务") + "-copy";
  copy.listen_address = port;
  cfg.services.push(copy);
  selectedSvc.value = copy;
  selected.value = copy.id;
  showToast(`已克隆「${s.comment}」→ 端口 :${port}，保存后生效`, "success");
}
function cloneById(id: string) {
  const s = (cfg.services || []).find((x: any) => (x.id || x.comment) === id);
  if (s) cloneService(s);
}
function removeById(id: string) {
  const s = (cfg.services || []).find((x: any) => (x.id || x.comment) === id);
  if (s) removeSvc(s);
}

// ---------- §5.2A 服务列表视图（>6 服务自动启用，或手动切换；拖动行 ⠿ 调整顺序） ----------
const LIST_PREF_KEY = "o2a.panel.listView";
const listMode = ref(localStorage.getItem(LIST_PREF_KEY) === "1");
const useListView = computed(() => listMode.value || serviceList.value.length > 6);
function toggleListView() {
  listMode.value = !listMode.value;
  localStorage.setItem(LIST_PREF_KEY, listMode.value ? "1" : "0");
}
const listRows = computed<ServiceRow[]>(() =>
  (cfg.services || []).map((s: any) => {
    const st = (status.services || []).find((x: any) => x.id === s.id);
    const acc = accountById(s.account);
    return {
      id: s.id || s.comment,
      comment: s.comment || "",
      accountLabel: acc ? acc.name || acc.id : "未绑定账号",
      api: s.api || "auto",
      model: s.model || "",
      port: s.listen_address ?? "?",
      host: String(s.listen_host || "127.0.0.1"),
      running: !!runningMap.value[s.id || s.comment],
      busy: !!busyMap.value[s.id || s.comment],
      ...(st ? {} : {}),
    };
  })
);
function openServiceFromList(id: string) {
  const s = (cfg.services || []).find((x: any) => (x.id || x.comment) === id);
  if (s) {
    selectService(s);
    page.value = "config";
  }
}
// ---------- §5.2A 拖动排序：按列表新顺序重排 cfg.services ----------
// 拖动过程中仅内存态重排（对象引用不变，选中态不受影响）；松手后 onReorderEnd
// 直接复用 saveConfig 完整管线落盘（草稿提交 → 校验 → 写入 → 热加载 → 快照复位），
// 顺序属于配置本体，无需再到配置页手动保存。
function reorderServices(ids: string[]) {
  const byKey = new Map<string, any>(
    (cfg.services || []).map((s: any) => [s.id || s.comment, s])
  );
  const next: any[] = [];
  for (const k of ids) {
    const s = byKey.get(k);
    if (s) {
      next.push(s);
      byKey.delete(k);
    }
  }
  // 兜底：不在列表 id 集合里的服务保持原相对顺序追加
  for (const s of cfg.services || []) {
    if (byKey.has(s.id || s.comment)) next.push(s);
  }
  cfg.services = next;
}
function onReorderEnd() {
  void saveConfig();
}

// ---------- comment 改名 draft 提交（§3.3） ----------
// 改名输入绑本地 draft，@change / 失焦 / 保存时才写回；
// 校验失败 → 输入框下方红字，不写回、不 toast（回绑问题已由 id 身份物理消除）
// 「保存并重启」：引擎启动时一次性读取配置，运行中改的配置需重启该服务生效（§9.1）
async function saveAndRestart() {
  const s = activeSvc.value;
  if (!s) return;
  const key = s.id || s.comment;
  await saveConfig();
  try {
    await api.stopService(key);
    await api.startService(key);
    await loadStatus();
    showToast(`「${s.comment}」已保存并重启，新配置已生效`, "success");
  } catch (e: any) {
    showToast("重启失败: " + e, "error");
  }
}
function goAccounts() {
  page.value = "accounts";
}

// 服务入口提示：api 显式声明优先，回退 client / 自动识别

const runningList = computed(() =>
  serviceList.value.filter((s: any) => runningMap.value[s.id || s.comment])
);
const onMeta = computed(() =>
  runningList.value.length
    ? "运行中 · " + runningList.value.map((s: any) => "127.0.0.1:" + s.listen_address).join(" / ")
    : "运行中"
);
const offMeta = computed(() => {
  if (selected.value === ALL) {
    return "代理未启动 · 共 " + serviceList.value.length + " 个服务";
  }
  const s = activeSvc.value;
  return s
    ? "代理未启动 · 端口 " + (s.listen_address ?? "?") + " · " + (s.model || "")
    : "代理未启动";
});
// ---------- 服务身份 id（§2 id 化；生成/补齐移至 stores/config.ts） ----------

async function loadConfig() {
  try {
    const prev = selectedSvc.value;
    const prevId = prev?.id || null;
    const prevName = prev?.comment || (selected.value === ALL ? null : selected.value);
    const prevPort = prev?.listen_address;
    const c = await api.getConfig();
    Object.keys(cfg).forEach((k) => delete (cfg as any)[k]);
    Object.assign(cfg, c || {});
    migrateAccounts(cfg);
    ensureSvcIds(cfg);
    // 配置重载后重新关联选中服务：优先按 id（稳定身份，改名不丢选中），
    // 其次按名称 / 端口（兼容）；找不到则回到“全部”，不静默跳到第一个服务。
    if (prevId || prevName) {
      const svcs: any[] = cfg.services || [];
      const next =
        (prevId ? svcs.find((s: any) => s.id === prevId) : null) ||
        (prevName ? svcs.find((s: any) => s.comment === prevName) : null) ||
        (prevPort
          ? svcs.find((s: any) => String(s.listen_address) === String(prevPort))
          : null) ||
        null;
      if (next) {
        selectedSvc.value = next;
        selected.value = String(next.id || next.comment || selected.value);
      } else {
        selectedSvc.value = null;
        selected.value = ALL;
      }
    } else {
      selectedSvc.value = null;
    }
  } catch (e: any) {
    showToast("读取配置失败: " + e, "error");
  }
  snapCfg();
}

// 配置位置应用/恢复后（ServicesView）重新加载配置与状态
async function toggleSvc(id: string) {
  const svc = (cfg.services || []).find((s: any) => (s.id || s.comment) === id);
  if (!svc) {
    showToast("服务不存在或尚未添加", "error");
    return;
  }
  // 状态还没拉回来时先刷新一次，避免“服务尚未保存”误报
  if (!(status.services || []).length) {
    await loadStatus();
  }
  // 兼容 id/name/port 三种身份：有些旧配置里服务没有 id，后端状态会按 name 返回
  const st = (status.services || []).find(
    (s: any) =>
      (s.id && s.id === id) ||
      (s.name && s.name === svc.comment) ||
      (s.port && String(s.port) === String(svc.listen_address))
  );
  const key = st?.id || st?.name || (st?.port != null ? String(st.port) : id);
  if (!st || !key || !(key in runningMap.value)) {
    showToast("该服务尚未保存，请先保存配置", "error");
    return;
  }
  const label = svc.comment || key;
  try {
    await api.toggleService(key);
    await loadStatus();
    offError.value = "";
    showToast(label + (runningMap.value[key] ? " 已启动" : " 已停止"), "success");
  } catch (e: any) {
    showToast("操作失败: " + e, "error");
    offError.value = String(e);
  }
}

async function onLocationReload() {
  await loadConfig();
  await loadStatus();
}

// 引擎/桌面端读取 config.json 后可能会补齐/修正服务 id（例如 engine _ensure_service_ids
// 把缺失 id 写入磁盘）。面板内存里的 cfg 不会自动感知，导致 stats/启停仍用旧 id。
// 这里用 status（来自磁盘 config）把同 comment/端口的服务 id 回写到 cfg，自动愈合。
function syncServiceIdsFromStatus() {
  // 记录同步前是否干净：自动回写 id 是后台自愈，不应让“未保存”误报；
  // 但若用户已有未保存改动，则不能覆盖快照把用户的修改误标为已保存。
  const wasClean = !dirty.value;
  const statusServices: any[] = (status as any).services || [];
  let healed = false;
  for (const ss of statusServices) {
    if (!ss.id) continue;
    const svc = (cfg.services || []).find(
      (s: any) =>
        (ss.name && ss.name === s.comment) ||
        (ss.port && String(ss.port) === String(s.listen_address))
    );
    if (svc && svc.id !== ss.id) {
      svc.id = ss.id;
      healed = true;
    }
  }
  if (healed && wasClean) {
    snapCfg(); // 自愈 id 视为配置已同步，不进入脏状态
  }
  if (healed && selected.value !== ALL) {
    const cur = selectedSvc.value;
    const next = (cfg.services || []).find((s: any) => s.comment === cur?.comment);
    if (next) selected.value = String(next.id || next.comment);
  }
}

async function loadStatus() {
  try {
    const s = await api.getStatus();
    Object.keys(status).forEach((k) => delete (status as any)[k]);
    Object.assign(status, s);
    syncServiceIdsFromStatus();
  } catch (e: any) {
    showToast("读取状态失败: " + e, "error");
  }
}


// 悬浮窗开关：切换当前选中服务（或全部）的悬浮窗，失败时给出提示
async function onToggleFloat() {
  try {
    await api.toggleFloatFor(floatService.value);
  } catch (e: any) {
    showToast(String(e), "error");
  }
}

function addService() {
  cfg.services = cfg.services || [];
  const used = new Set(
    (cfg.services || [])
      .map((s: any) => Number(s.listen_address))
      .filter((n: any) => Number.isFinite(n))
  );
  let port = 11011;
  while (used.has(port)) port++;
  // §11.6：默认模型留空 + 表单必填高亮（原硬编码 qwen-plus 对非 qwen 账号是错的）；
  // 名字用「新服务-N」；id 即刻生成（稳定身份，未保存也不会与其他服务混淆）
  const svc = {
    id: newSvcId(),
    comment: "新服务-" + (cfg.services.length + 1),
    account: (cfg.accounts || [])[0]?.id || "",
    client: "auto",
    model: "",
    override_model: true,
    listen_address: port,
    context_1m: false,
    max_tokens: 4096,
  };
  cfg.services.push(svc);
  selectedSvc.value = svc;
  selected.value = svc.id;
  page.value = "config";
  showToast("已添加服务，绑定账号并选择模型后保存", "success");
}

function removeSvc(target?: any) {
  const s = target || activeSvc.value;
  if (!s) return;
  const name = s.comment;
  const running = !!runningMap.value[s.id || s.comment];
  askConfirm(
    "删除服务",
    running
      ? `服务「${name}」正在运行，删除将先停止它并从配置中移除。\n保存配置后生效。`
      : `确定删除服务「${name}」？\n保存配置后生效。`,
    () => {
      const idx = (cfg.services || []).indexOf(s);
      if (running) api.stopService(s.id || s.comment).catch(() => {});
      cfg.services = cfg.services.filter((x: any) => x.id !== s.id);
      selectedSvc.value = null;
      selected.value = ALL;
      // §10.2-5 撤销：仅内存态回滚（未保存不落盘）
      showToast(`已删除服务「${name}」，点击保存生效`, "success", {
        label: "撤销",
        fn: () => {
          if (idx >= 0) (cfg.services || []).splice(idx, 0, s);
          else (cfg.services || []).push(s);
          selectedSvc.value = s;
          selected.value = s.id || s.comment;
          showToast(`已恢复「${name}」`, "success");
        },
      });
    },
    "删除"
  );
}

function validateConfig(): string | null {
  const svcs = cfg.services || [];
  const seen = new Set<string>();
  const hostPort = new Map<string, string>(); // host:port → 首个占用服务名（端口冲突检测）
  for (const s of svcs) {
    const comment = String(s.comment || "").trim();
    if (!comment) return "服务备注 comment 不能为空";
    if (seen.has(comment)) return "服务备注重复：" + comment;
    seen.add(comment);
    if (!accountById(s.account)) return `服务 ${comment} 未绑定有效账号`;
    if (!String(s.model || "").trim()) {
      return `服务 ${comment} 未配置主模型 model（必填）`;
    }
    const port = Number(s.listen_address);
    if (!Number.isInteger(port) || port < 1 || port > 65535) {
      return `服务 ${comment} 的监听端口无效（需为 1-65535 整数）`;
    }
    const host = String(s.listen_host || "127.0.0.1").trim() || "127.0.0.1";
    const key = `${host}:${port}`;
    const firstOwner = hostPort.get(key);
    if (firstOwner) {
      return `端口冲突：服务「${firstOwner}」与「${comment}」同时监听 ${key}（第二个绑定时会启动失败），请修改其一的端口`;
    }
    hostPort.set(key, comment);
    if (s.max_tokens !== undefined && s.max_tokens !== null && s.max_tokens !== "") {
      const mt = Number(s.max_tokens);
      if (!Number.isInteger(mt) || mt < 1) return `服务 ${comment} 的 max_tokens 无效`;
    }
  }
  for (const a of cfg.accounts || []) {
    if (!String(a.name || "").trim()) return "账号名称不能为空";
    if (!String(a.openai_url || "").trim() && !String(a.anthropic_url || "").trim()) {
      return `账号 ${a.name || a.id} 未配置任何端点（OpenAI / Anthropic 至少填一个）`;
    }
  }
  return null;
}

// 旧配置（services 内嵌 url/key，无 accounts）自动迁移为新结构
function migrateAccounts(c: any) {
  const modeToClient: Record<string, string> = {
    claude: "anthropic",
    codex: "openai",
    direct: "anthropic",
  };
  c.accounts = c.accounts || [];
  c.services = c.services || [];
  const hasLegacy = (c.services as any[]).some(
    (s: any) => s.openai_base_url || s.openai_api_key
  );
  if (!c.accounts.length && hasLegacy) {
    // §11.7：生成 acc-N 前先查重，避免与存量账号 id 冲突导致 Key 错挂
    const usedIds = new Set(
      (c.accounts as any[]).map((a: any) => String(a.id || "")).filter(Boolean)
    );
    const nextAccId = () => {
      let i = 1;
      while (usedIds.has("acc-" + i)) i++;
      const id = "acc-" + i;
      usedIds.add(id);
      return id;
    };
    const generated = (c.services as any[]).map((s: any, i: number) => ({
      id: nextAccId(),
      name: s.comment || "账号" + (i + 1),
      api_key: s.openai_api_key || "",
      openai_url: s.openai_base_url || "",
      anthropic_url: s.anthropic_base_url || "",
    }));
    c.accounts = generated;
    (c.services as any[]).forEach((s: any, i: number) => {
      s.account = generated[i]?.id || "";
      s.client = modeToClient[s.mode || "claude"] || "auto";
      delete s.openai_base_url;
      delete s.openai_api_key;
      delete s.anthropic_base_url;
    });
  }
  (c.services as any[]).forEach((s: any) => {
    if (!s.client) s.client = modeToClient[s.mode || "claude"] || "auto";
  });
}

function normalizeConfig() {
  (cfg.services || []).forEach((s: any) => {
    s.comment = String(s.comment || "").trim();
    s.listen_address = Number(s.listen_address);
    s.listen_host = String(s.listen_host || "").trim();
    if (!s.listen_host) delete s.listen_host;
    if (s.max_tokens === "" || s.max_tokens === undefined || s.max_tokens === null) {
      delete s.max_tokens;
    } else {
      s.max_tokens = Number(s.max_tokens);
    }
    s.context_1m = !!s.context_1m;
    if (!s.api) delete s.api;
    if (!s.upstream_api || s.upstream_api === "openai-completions") delete s.upstream_api;
    if (!s.thinking_mode || s.thinking_mode === "auto") delete s.thinking_mode;
    // §6 白名单 / 别名 / 策略归一化（空值删除字段，与 normalizeConfig 风格一致）
    if (Array.isArray(s.models)) {
      s.models = s.models.map((m: any) => String(m).trim()).filter(Boolean);
      if (!s.models.length) delete s.models;
    }
    if (s.models_map && typeof s.models_map === "object") {
      const map: Record<string, string> = {};
      for (const [k, v] of Object.entries(s.models_map)) {
        if (String(k).trim() && String(v).trim()) map[String(k).trim()] = String(v).trim();
      }
      if (Object.keys(map).length) s.models_map = map;
      else delete s.models_map;
    }
    if (!s.model_policy || s.model_policy === "clamp") delete s.model_policy;
    s.auth_token = String(s.auth_token || "").trim();
    if (!s.auth_token) delete s.auth_token;
    delete s.openai_base_url;
    delete s.openai_api_key;
    delete s.anthropic_base_url;
  });
  (cfg.accounts || []).forEach((a: any) => {
    a.name = String(a.name || "").trim();
    a.api_key = String(a.api_key || "");
    a.openai_url = String(a.openai_url || "").trim();
    a.anthropic_url = String(a.anthropic_url || "").trim();
  });
  if (cfg.cache_stats_enabled !== undefined) cfg.cache_stats_enabled = !!cfg.cache_stats_enabled;
  if (cfg.cache_stats_retention_days !== undefined) {
    const d = Number(cfg.cache_stats_retention_days);
    cfg.cache_stats_retention_days = Number.isInteger(d) && d >= 1 ? d : 30;
  }
}

function computeRemovedServices(oldList: any[], newList: any[]): string[] {
  // §2 id 化：删除判定按 id 差集 —— 改名（id 不变）不再被误判为删除；
  // 旧状态列表无 id（旧版本）时回退按 comment，改名按端口启发式兜底。
  const newIds = new Set(newList.map((x: any) => x.id).filter(Boolean));
  const newNames = new Set(newList.map((x: any) => x.comment));
  return oldList
    .filter((x: any) => {
      if (x.id && newIds.has(x.id)) return false;
      if (!x.id && newNames.has(x.name)) return false;
      // 无 id 的旧状态：旧名消失但端口被新服务占用视为改名，不停止进程
      if (!x.id) {
        const renamed = newList.some(
          (s: any) => s.comment !== x.name && String(s.listen_address) === String(x.port)
        );
        if (renamed) return false;
      }
      return true;
    })
    .map((x: any) => x.id || x.name);
}

// §5.2B 端口占用预检：未运行服务的端口被占（外部进程）时保存前提醒
async function precheckPorts(): Promise<string[]> {
  const busy: string[] = [];
  for (const s of cfg.services || []) {
    const port = Number(s.listen_address);
    if (!Number.isInteger(port) || port < 1 || port > 65535) continue;
    const host = String(s.listen_host || "127.0.0.1").trim() || "127.0.0.1";
    if (runningMap.value[s.id || s.comment]) continue; // 自己管理的运行中服务跳过
    if (await api.isPortOpen(host, port).catch(() => false)) {
      busy.push(`${host}:${port}（服务「${s.comment}」）`);
    }
  }
  return busy;
}

async function saveConfig() {
  // 保存前先提交 comment draft 与 models_map 草稿；校验失败则阻止保存
  if (!commitComment() || !commitModelsMap()) {
    showToast("表单校验未通过，请修正后再保存", "error");
    return;
  }
  const err = validateConfig();
  if (err) {
    showToast(err, "error");
    return;
  }
  precheckPorts().then((busy) => {
    if (busy.length) {
      showToast("端口已被外部进程占用（启动会失败）：" + busy.join("、"), "error");
    }
  });
  normalizeConfig();
  try {
    const removed = computeRemovedServices(status.services || [], cfg.services || []);
    await api.saveConfig(cfg);
    await loadConfig();
    await loadStatus();
    loadAccountStats();
    for (const n of removed) {
      await api.stopService(n).catch(() => {});
    }
    // §9 热加载：优先触发热重载（原地生效，无需重启）；失败则回退"重启生效"提示
    if (anyRunning.value) {
      try {
        const res: any = await api.reloadEngine();
        const errs: string[] = res?.errors || [];
        if (res?.reloaded) {
          showToast(`配置已写入并热加载（${res.reloaded} 个服务生效）`, "success");
        } else {
          showToast("配置已写入 config.json，运行中的服务重启后生效", "info");
        }
        if (errs.length) showToast("部分服务热重载失败：" + errs.join("；"), "error");
      } catch {
        showToast("配置已写入 config.json，运行中的服务重启后生效", "info");
      }
    } else {
      showToast("配置已保存", "success");
    }
    snapCfg();
  } catch (e: any) {
    showToast("保存失败: " + e, "error");
  }
}

// 配置文件位置来源标签
watch(selected, (v) => {
  if (v === ALL) {
    selectedSvc.value = null;
  } else {
    // id 优先（稳定身份），comment 兼容兜底
    const next =
      (cfg.services || []).find((s: any) => (s.id || s.comment) === v) || null;
    if (next) {
      selectedSvc.value = next;
    } else if ((cfg.services || []).length) {
      // 选中值失效时自动落到第一个服务，避免配置页空白/“尚未配置服务”
      selectedSvc.value = (cfg.services || [])[0];
      selected.value = selectedSvc.value.id || selectedSvc.value.comment;
    } else {
      selectedSvc.value = null;
    }
  }
  modelFilter.value = "";
  loadStats();
  loadLive();
  if (page.value === "config") fetchModels();
});

// 模型过滤切换：整页统计（KPI / 性能条 / 图表 / 按模型 / 按服务）按所选模型
// 重新拉取（后端记录层过滤）；实时调用由 livePool 在前端同步过滤。
watch(modelFilter, () => {
  loadStats();
});

watch(page, (p) => {
  if (p === "config") fetchModels();
});

// 切换服务绑定的账号时，模型列表跟随新账号的端点刷新
watch(
  () => activeSvc.value?.account,
  () => {
    if (page.value === "config") fetchModels();
  }
);

// 服务增删后刷新 tab 溢出遮罩
watch(
  serviceList,
  () => {
    nextTick(onTabsScroll);
  },
  { deep: true }
);

// ---------- §10.1 单一心跳轮询 ----------
// 原 4 个独立 setInterval（3s 状态 / 5s 统计 / 3s 实时 / 10s 账号）收敛为
// 单一 1s tick，按各自周期分发；暂停条件（面板隐藏 / 文档隐藏）只判一处。
// 统计轮询自适应降频：连续无变化时 5s → 15s。
let heartbeat: any = null;
let lastStatus = 0, lastStats = 0, lastLive = 0, lastAcc = 0;
let statsIdleCount = 0;
const statsInterval = () => (statsIdleCount >= 3 ? 15000 : 5000);
function heartbeatTick() {
  if (!panelVisible.value || document.hidden) return;
  const now = Date.now();
  if (now - lastStatus >= 3000) { lastStatus = now; loadStatus(); }
  if (now - lastLive >= 3000) { lastLive = now; loadLive(); }
  if (now - lastStats >= statsInterval()) {
    lastStats = now;
    const before = stats.today;
    loadStats().then(() => {
      // 无变化 → 计数+1（触发降频）；有变化 → 复位
      if (JSON.stringify(stats.today) === JSON.stringify(before)) statsIdleCount = Math.min(statsIdleCount + 1, 5);
      else statsIdleCount = 0;
    });
  }
  if (now - lastAcc >= 10000) { lastAcc = now; loadAccountStats(); }
}
function startHeartbeat() {
  lastStatus = lastStats = lastLive = lastAcc = 0;
  heartbeat = setInterval(heartbeatTick, 1000);
}
// 面板可见性：隐藏时暂停轮询，避免面板长期隐藏期间空转 + 反复全量读统计。
// 托盘点击/失焦收起由 Rust 发 panel-visible 事件通知（可靠），再叠加
// document.hidden（WebView2 窗口隐藏时可见性置位）双保险；
// 变为可见时立即刷新一轮，不等下一个轮询周期。
const panelVisible = ref(!document.hidden);
let unlistenPanel: (() => void) | null = null;
// 模型列表只在面板首次可见时预热一次：避免启动即发 /models 请求
// （端点不可达时白白占用 8s 阻塞线程），面板隐藏期间不预热。
let modelsWarmed = false;
function warmModelsOnce() {
  if (modelsWarmed) return;
  modelsWarmed = true;
  warmModels();
}

function onPanelVisible(visible: boolean) {
  const was = panelVisible.value;
  panelVisible.value = visible;
  if (visible && !was) {
    loadStatus();
    loadStats();
    loadLive();
    loadAccountStats();
    warmModelsOnce();
  }
}
function onVisibilityChange() {
  onPanelVisible(!document.hidden);
}

// ---------- §10.4 键盘流：Ctrl+K 打开服务列表快速跳转；Esc 关面板 ----------
const listViewRef = ref<InstanceType<typeof ServiceListView> | null>(null);
function onGlobalKeydown(e: KeyboardEvent) {
  if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "k") {
    e.preventDefault();
    if (!listMode.value) toggleListView();
    nextTick(() => listViewRef.value?.focusList());
  } else if (e.key === "Escape") {
    if (confirmBox.value || toastAction.value) return; // 弹层优先自行处理
    api.hidePanel().catch(() => {});
  }
}

onMounted(async () => {
  await Promise.all([loadConfig(), loadStatus()]);
  loadStats();
  loadLive();
  loadAccountStats();
  if (panelVisible.value) warmModelsOnce();
  listen<boolean>("panel-visible", (e) => onPanelVisible(!!e.payload)).then(
    (f) => (unlistenPanel = f)
  );
  document.addEventListener("visibilitychange", onVisibilityChange);
  document.addEventListener("keydown", onGlobalKeydown);
  // 主题：跨窗口同步（Tauri event）+ 系统深浅跟随
  listen<string>("o2a-theme", (e) => {
    if (e.payload !== theme.value) {
      theme.value = e.payload as Theme;
      applyTheme(theme.value);
    }
  }).catch(() => {});
  watchSystemTheme(() => emitTheme());
  emitTheme();
  nextTick(onTabsScroll);
  startHeartbeat();
});
onUnmounted(() => {
  unlistenPanel?.();
  document.removeEventListener("visibilitychange", onVisibilityChange);
  document.removeEventListener("keydown", onGlobalKeydown);
  if (heartbeat) clearInterval(heartbeat);
});
</script>
