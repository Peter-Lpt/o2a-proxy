<template>
  <div class="popover">
    <header class="head">
      <div class="brand">
        <span class="logo"><Icon name="swap" :size="15" /></span>
        <div class="brand-txt">
          <div class="title">o2a-proxy</div>
          <div class="sub" id="headSub">{{ anyRunning ? "代理运行中" : "代理已停止" }}</div>
        </div>
        <span class="orb" :class="anyRunning ? 'running' : 'stopped'" :title="anyRunning ? '运行中' : '已停止'"></span>
      </div>
      <div class="head-actions">
        <button class="icon-btn" :title="theme === 'dark' ? '切换到浅色' : '切换到深色'" @click="onToggleTheme">
          <Icon :name="theme === 'dark' ? 'sun' : 'moon'" :size="14" />
        </button>
        <button class="btn btn-sm" title="添加服务" :disabled="anyRunning" @click="addService"><Icon name="plus" :size="12" /> 添加服务</button>
        <button class="float-btn" title="悬浮看板：置顶小窗实时查看" @click="api.toggleFloat()"><Icon name="float" :size="12" /> 悬浮</button>
      </div>
    </header>

    <div class="svc-bar">
      <div class="svc-tabs">
        <button class="svc-tab" :class="{ active: selected === '__all__' }" @click="selected = '__all__'">
          <span class="dot" :class="{ on: anyRunning }"></span>全部
        </button>
        <button
          v-for="s in serviceList"
          :key="s.comment"
          class="svc-tab"
          :class="{ active: selected === s.comment }"
          @click="selected = s.comment"
          :title="s.comment + ' · ' + (s.mode || 'claude') + ' · :' + (s.listen_address ?? '?')"
        >
          <span class="dot" :class="{ on: runningMap[s.comment] }"></span>{{ s.comment }}
          <span class="power" @click.stop="toggleSvc(s.comment)" :title="runningMap[s.comment] ? '停止' : '启动'">
            <Icon :name="runningMap[s.comment] ? 'stop' : 'play'" :size="9" />
          </span>
        </button>
      </div>
    </div>

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
      <button class="tab" :class="{ active: page === 'stats' }" @click="page = 'stats'">统计</button>
      <button class="tab" :class="{ active: page === 'config' }" @click="page = 'config'">配置</button>
    </nav>

    <main>
      <!-- 统计 -->
      <section v-show="page === 'stats'" class="panel" :class="{ active: page === 'stats' }">
        <div v-if="anyRunning" class="card live-card">
          <div class="live-head">
            <span class="live-title"><span class="live-dot"></span>实时调用</span>
            <span>{{ liveRecords.length ? fmtNum(liveRecords[liveRecords.length - 1].output_tokens) + " tok / 最新" : "等待请求…" }}</span>
          </div>
          <Spark :points="liveSpark" :height="40" class="live-spark" />
          <div class="live-feed">
            <div v-for="(r, i) in liveFeed" :key="i" class="lf-row">
              <span class="t">{{ r.time }}</span>
              <span class="k">{{ r.model }}</span>
              <span class="h" :class="r.hit >= 0.3 ? 'good' : ''">{{ fmtPct(r.hit) }}</span>
              <span class="h">{{ fmtNum(r.tokens) }}</span>
            </div>
          </div>
        </div>

        <div class="card stat-table">
          <div class="st-row st-head">
            <span>{{ scopeLabel }}</span><span>当前小时</span><span>今日</span><span>本月</span>
          </div>
          <div class="st-row"><span>请求数</span><b>{{ fmtNum(stats.current?.requests) }}</b><b>{{ fmtNum(stats.today?.requests) }}</b><b>{{ fmtNum(stats.month?.requests) }}</b></div>
          <div class="st-row"><span>输入</span><b>{{ fmtNum(stats.current?.input) }}</b><b>{{ fmtNum(stats.today?.input) }}</b><b>{{ fmtNum(stats.month?.input) }}</b></div>
          <div class="st-row"><span>缓存读</span><b>{{ fmtNum(stats.current?.read) }}</b><b>{{ fmtNum(stats.today?.read) }}</b><b>{{ fmtNum(stats.month?.read) }}</b></div>
          <div class="st-row"><span>输出</span><b>{{ fmtNum(stats.current?.output) }}</b><b>{{ fmtNum(stats.today?.output) }}</b><b>{{ fmtNum(stats.month?.output) }}</b></div>
          <div class="st-row"><span>总计</span><b>{{ fmtNum(totalOf(stats.current)) }}</b><b>{{ fmtNum(totalOf(stats.today)) }}</b><b>{{ fmtNum(totalOf(stats.month)) }}</b></div>
          <div class="st-row hit">
            <span>命中率</span>
            <b :class="hitClass(stats.current?.hitRate)">{{ fmtPct(stats.current?.hitRate) }}</b>
            <b :class="hitClass(stats.today?.hitRate)">{{ fmtPct(stats.today?.hitRate) }}</b>
            <b :class="hitClass(stats.month?.hitRate)">{{ fmtPct(stats.month?.hitRate) }}</b>
          </div>
          <div class="st-row cost"><span>费用</span><b>{{ fmtCost(stats.current?.cost) }}</b><b>{{ fmtCost(stats.today?.cost) }}</b><b>{{ fmtCost(stats.month?.cost) }}</b></div>
        </div>

        <div class="chart-head">
          <div class="seg">
            <button class="seg-btn" :class="{ active: range === 'today' }" @click="setRange('today')">今日</button>
            <button class="seg-btn" :class="{ active: range === 'month' }" @click="setRange('month')">本月</button>
          </div>
          <div class="chart-head-right">
            <SelectBox v-model="modelFilter" :options="modelOptions" size="sm" placeholder="全部模型" title="按模型过滤（今日/本月）" />
            <button class="icon-btn" title="刷新" @click="loadStats()"><Icon name="refresh" :size="12" /></button>
          </div>
        </div>

        <div v-if="modelStats.length" class="card model-stats-card">
          <div class="fc-h">
            <span>{{ range === 'today' ? "今日按模型" : "本月按模型" }}</span>
            <span class="model-stats-total">总费用 ¥{{ fmtCost(modelStats.reduce((a, m) => a + (m.cost || 0), 0)) }}</span>
          </div>
          <div class="model-stats-list">
            <div v-for="m in modelStats" :key="m.model" class="ms-row">
              <span class="m">{{ m.model }}</span>
              <span class="n">{{ fmtNum(m.requests) }} 次</span>
              <span class="n">{{ fmtNum(m.input + m.read + m.output) }} tok</span>
              <span class="c">¥{{ fmtCost(m.cost) }}</span>
            </div>
          </div>
        </div>
        <div v-else class="card">
          <div class="empty-tip">还没有按模型的统计数据，代理跑起来后这里会展示各模型的用量与费用。</div>
        </div>

        <div class="card chart-box">
          <div class="chart-title">缓存命中率 & Token 消耗（{{ range === 'today' ? '今日逐小时' : '本月逐日' }}）</div>
          <LineChart :labels="chartLabels" :tokens="chartTokens" :hit-rate="chartHit" :theme="theme" />
          <div class="chart-note">* 命中率 = 缓存读 / (缓存读 + 输入)，对齐 Anthropic 官方口径</div>
        </div>
        <div class="updated">{{ stats.updatedAt ? "更新于 " + stats.updatedAt.replace('T', ' ').slice(0, 19) : "—" }}</div>
      </section>

      <!-- 配置 -->
      <section v-show="page === 'config'" class="panel" :class="{ active: page === 'config' }">
        <div v-if="anyRunning" class="cfg-lock">
          <span class="cfg-lock-msg"><Icon name="lock" :size="13" /> 服务运行中，配置已锁定</span>
          <button class="btn btn-sm" @click="stopAll()">停止代理以编辑</button>
        </div>
        <form @submit.prevent="saveConfig">
          <div class="card form-card">
            <div class="fc-h">
              服务配置
              <span class="fc-sub">{{ selected !== ALL ? (activeSvc?.comment || "未配置") : "全部视图" }}</span>
            </div>
            <div v-if="activeSvc && selected !== ALL">
              <label>备注 comment <input v-model="activeSvc.comment" type="text" :disabled="anyRunning" /></label>
              <label>模式 mode
                <SelectBox v-model="activeSvc.mode" :options="modeOptions" :disabled="anyRunning" />
              </label>
              <label>主模型 model <SelectBox v-model="activeSvc.model" :options="fetchedModels" allow-custom placeholder="选择或输入模型" :disabled="anyRunning" /></label>
              <label>子模型 sub_model <SelectBox v-model="activeSvc.sub_model" :options="fetchedModels" allow-custom placeholder="选择或输入模型" :disabled="anyRunning" /></label>
              <label>监听端口 listen_address <input v-model="activeSvc.listen_address" type="number" min="1" max="65535" :disabled="anyRunning" /></label>
              <label>API 地址 openai_base_url <input v-model="activeSvc.openai_base_url" type="text" spellcheck="false" autocomplete="off" :disabled="anyRunning" @input="debounceFetchModels" /></label>
              <label>API Key
                <div class="field-row">
                  <input v-model="activeSvc.openai_api_key" :type="showKey ? 'text' : 'password'" autocomplete="off" :disabled="anyRunning" @input="debounceFetchModels" />
                  <button type="button" class="icon-btn" @click="showKey = !showKey" :title="showKey ? '隐藏 API Key' : '显示 API Key'">
                    <Icon :name="showKey ? 'eye-off' : 'eye'" :size="13" />
                  </button>
                </div>
              </label>
              <div class="grid2">
                <label>max_tokens <input v-model="activeSvc.max_tokens" type="number" min="1" :disabled="anyRunning" /></label>
                <label class="inline"><input v-model="activeSvc.context_1m" type="checkbox" :disabled="anyRunning" /><span>1M 上下文</span></label>
              </div>
            </div>
            <div v-else class="hint">
              {{ selected === ALL
                ? "当前是「全部」视图，聚合查看所有服务。请点击上方具体服务标签来编辑该服务配置，或点击「添加服务」新建。"
                : "尚未配置服务，点击右上角「添加服务」。" }}
            </div>
            <div class="model-hint" :class="{ err: modelHint.err }">{{ modelHint.text }}</div>
          </div>

          <div class="card form-card">
            <div class="fc-h">全局</div>
            <label>认证令牌 auth_token <input v-model="cfg.auth_token" type="text" :disabled="anyRunning" /></label>
            <label class="inline"><input v-model="cfg.cache_stats_enabled" type="checkbox" :disabled="anyRunning" /><span>启用缓存统计</span></label>
            <div class="grid2">
              <label>保留天数 <input v-model.number="cfg.cache_stats_retention_days" type="number" min="1" max="365" :disabled="anyRunning" /></label>
              <label>统计目录 <input v-model="cfg.cache_stats_dir" type="text" placeholder="留空使用默认" :disabled="anyRunning" /></label>
            </div>
            <div class="model-hint">留空使用默认目录：{{ status.statsDir || "…" }}</div>
          </div>

          <div class="form-actions">
            <button type="submit" class="btn btn-primary" :disabled="anyRunning">保存配置</button>
            <button type="button" class="btn btn-danger" @click="removeSvc" :disabled="anyRunning || !activeSvc || selected === ALL">删除此服务</button>
            <button type="button" class="btn" @click="api.openConfigFile()">打开 config.json</button>
          </div>
          <p class="hint">每个服务监听各自端口：claude 走 Anthropic 转换，codex 走 OpenAI 透传，均记录统计。填写真实 API Key 后点击服务标签上的开关即可启动。</p>
        </form>
      </section>
    </main>

    <footer class="foot">
      <span>o2a-proxy · v0.1.0</span>
      <button class="link-btn" @click="api.quitApp()">退出应用</button>
    </footer>
    <div id="toast" class="toast" :class="{ show: toast }">{{ toast }}</div>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, onUnmounted, reactive, ref, watch } from "vue";
import { api, fmtCost, fmtNum, fmtPct } from "./api";
import Icon from "./components/Icon.vue";
import LineChart from "./components/LineChart.vue";
import SelectBox from "./components/SelectBox.vue";
import Spark from "./components/Spark.vue";
import { getTheme, toggleTheme } from "./theme";

const ALL = "__all__";
const cfg = reactive<any>({});
const status = reactive<any>({ services: [] });
const stats = reactive<any>({});
const liveRecords = ref<any[]>([]);
const selected = ref<string>(ALL);
const page = ref<"stats" | "config">("stats");
const range = ref<"today" | "month">("today");
const modelFilter = ref<string>("");
const toast = ref("");
const offError = ref("");
const showKey = ref(false);
const theme = ref<"dark" | "light">(getTheme());
const fetchedModels = ref<string[]>([]);
const modelHint = ref<{ text: string; err: boolean }>({ text: "", err: false });
const modelCache = new Map<string, string[]>();
const modeOptions = [
  { value: "claude", label: "claude（Anthropic 转换）" },
  { value: "codex", label: "codex（OpenAI 透传）" },
];
let modelsSeq = 0;
let mdTimer: any = null;

function onToggleTheme() {
  theme.value = toggleTheme();
}

function setModelHint(text: string, err = false) {
  modelHint.value = { text, err };
}

async function fetchModels() {
  if (selected.value === ALL) {
    setModelHint("选择具体服务后可拉取模型列表", false);
    return;
  }
  const s = activeSvc.value;
  if (!s) {
    setModelHint("");
    return;
  }
  const baseUrl = String(s.openai_base_url || "").trim();
  const apiKey = String(s.openai_api_key || "").trim();
  if (!baseUrl) {
    setModelHint("填写当前服务的 API 地址后可拉取模型列表", false);
    return;
  }
  // 同一地址已缓存：直接展示（即使该服务未填 Key 也能复用同提供商的模型列表）
  if (modelCache.has(baseUrl)) {
    fetchedModels.value = modelCache.get(baseUrl)!;
    setModelHint(`已加载 ${fetchedModels.value.length} 个模型（${baseUrl}）`, false);
    return;
  }
  if (!apiKey) {
    setModelHint("填写当前服务的 API Key 后拉取模型列表", false);
    return;
  }
  const seq = ++modelsSeq;
  setModelHint("正在拉取模型列表…", false);
  try {
    const res = await api.fetchModels(baseUrl, apiKey);
    if (seq !== modelsSeq) return; // 过期请求
    if (res.ok) {
      modelCache.set(baseUrl, res.models);
      fetchedModels.value = res.models;
      setModelHint(`已加载 ${res.models.length} 个模型`, false);
    } else {
      setModelHint("该服务拉取模型失败：" + (res.error || ""), true);
    }
  } catch (e: any) {
    if (seq !== modelsSeq) return;
    setModelHint("该服务拉取模型失败：" + e, true);
  }
}

function debounceFetchModels() {
  clearTimeout(mdTimer);
  mdTimer = setTimeout(() => fetchModels(), 900);
}

async function warmModels() {
  const svcs = cfg.services || [];
  const first = svcs.find(
    (s: any) => String(s.openai_base_url || "").trim() && String(s.openai_api_key || "").trim()
  );
  if (!first) return;
  const baseUrl = String(first.openai_base_url).trim();
  if (modelCache.has(baseUrl)) return;
  const seq = ++modelsSeq;
  try {
    const res = await api.fetchModels(baseUrl, String(first.openai_api_key).trim());
    if (seq === modelsSeq && res.ok) modelCache.set(baseUrl, res.models);
  } catch (_) {}
}

let toastTimer: any = null;
function showToast(msg: string) {
  toast.value = msg;
  if (toastTimer) clearTimeout(toastTimer);
  toastTimer = setTimeout(() => (toast.value = ""), 2200);
}

// 服务标签以配置为准（添加/删除立即生效），运行态从 status 映射
const serviceList = computed(() =>
  (cfg.services || []).filter((s: any) => ["claude", "codex"].includes(s.mode || "claude"))
);
const runningMap = computed<Record<string, boolean>>(() => {
  const m: Record<string, boolean> = {};
  (status.services || []).forEach((s: any) => {
    m[s.name] = !!s.running;
  });
  return m;
});
const anyRunning = computed(() => Object.values(runningMap.value).some(Boolean));
const activeSvc = computed(
  () => serviceList.value.find((s: any) => s.comment === selected.value) || serviceList.value[0]
);
const scopeLabel = computed(() => (selected.value === ALL ? "全部" : selected.value));
const statsService = computed(() => (selected.value === ALL ? "" : selected.value));

const runningList = computed(() =>
  serviceList.value.filter((s: any) => runningMap.value[s.comment])
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
async function loadConfig() {
  try {
    const c = await api.getConfig();
    Object.keys(cfg).forEach((k) => delete (cfg as any)[k]);
    Object.assign(cfg, c || {});
  } catch (e: any) {
    showToast("读取配置失败: " + e);
  }
}

async function loadStatus() {
  try {
    const s = await api.getStatus();
    Object.keys(status).forEach((k) => delete (status as any)[k]);
    Object.assign(status, s);
  } catch (e: any) {
    showToast("读取状态失败: " + e);
  }
}

async function loadStats() {
  try {
    const s = await api.getStats(statsService.value);
    Object.keys(stats).forEach((k) => delete (stats as any)[k]);
    Object.assign(stats, s || {});
  } catch (e: any) {
    // 统计目录缺失时静默
  }
}

async function loadLive() {
  try {
    const d = await api.getLive(statsService.value);
    liveRecords.value = d?.records || [];
  } catch (e) {
    liveRecords.value = [];
  }
}

async function toggleSvc(name: string) {
  if (!(name in runningMap.value)) {
    showToast("该服务尚未保存，请先保存配置");
    return;
  }
  try {
    await api.toggleService(name);
    await loadStatus();
    offError.value = "";
    showToast(name + (runningMap.value[name] ? " 已启动" : " 已停止"));
  } catch (e: any) {
    showToast("操作失败: " + e);
    offError.value = String(e);
  }
}

async function stopAll() {
  try {
    await api.stopAll();
    await loadStatus();
  } catch (e: any) {
    showToast("停止失败: " + e);
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
  const svc = {
    comment: "service-" + (cfg.services.length + 1),
    mode: "claude",
    model: "qwen-plus",
    sub_model: "qwen-plus",
    listen_address: port,
    openai_base_url: "",
    openai_api_key: "",
    context_1m: false,
    max_tokens: 4096,
  };
  cfg.services.push(svc);
  selected.value = svc.comment;
  page.value = "config";
  showToast("已添加服务，填写后点击保存");
}

function removeSvc() {
  const s = activeSvc.value;
  if (!s) return;
  const name = s.comment;
  if (runningMap.value[name]) {
    api.stopService(name).catch(() => {});
  }
  cfg.services = cfg.services.filter((x: any) => x.comment !== name);
  selected.value = ALL;
  showToast("已删除服务，点击保存生效");
}

function validateConfig(): string | null {
  const svcs = cfg.services || [];
  const seen = new Set<string>();
  for (const s of svcs) {
    if (!["claude", "codex"].includes(s.mode || "claude")) continue;
    const comment = String(s.comment || "").trim();
    if (!comment) return "服务备注 comment 不能为空";
    if (seen.has(comment)) return "服务备注重复：" + comment;
    seen.add(comment);
    const port = Number(s.listen_address);
    if (!Number.isInteger(port) || port < 1 || port > 65535) {
      return `服务 ${comment} 的监听端口无效（需为 1-65535 整数）`;
    }
    if (s.max_tokens !== undefined && s.max_tokens !== null && s.max_tokens !== "") {
      const mt = Number(s.max_tokens);
      if (!Number.isInteger(mt) || mt < 1) return `服务 ${comment} 的 max_tokens 无效`;
    }
  }
  return null;
}

function normalizeConfig() {
  (cfg.services || []).forEach((s: any) => {
    s.comment = String(s.comment || "").trim();
    s.listen_address = Number(s.listen_address);
    if (s.max_tokens === "" || s.max_tokens === undefined || s.max_tokens === null) {
      delete s.max_tokens;
    } else {
      s.max_tokens = Number(s.max_tokens);
    }
    s.context_1m = !!s.context_1m;
  });
  if (cfg.cache_stats_enabled !== undefined) cfg.cache_stats_enabled = !!cfg.cache_stats_enabled;
  if (cfg.cache_stats_retention_days !== undefined) {
    const d = Number(cfg.cache_stats_retention_days);
    cfg.cache_stats_retention_days = Number.isInteger(d) && d >= 1 ? d : 30;
  }
}

async function saveConfig() {
  const err = validateConfig();
  if (err) {
    showToast(err);
    return;
  }
  normalizeConfig();
  try {
    const oldNames = (status.services || []).map((x: any) => x.name);
    const newNames = (cfg.services || []).map((x: any) => x.comment);
    const removed = oldNames.filter((n: any) => !newNames.includes(n));
    await api.saveConfig(cfg);
    await loadConfig();
    await loadStatus();
    for (const n of removed) {
      await api.stopService(n).catch(() => {});
    }
    showToast("配置已保存");
  } catch (e: any) {
    showToast("保存失败: " + e);
  }
}

function totalOf(o: any): number {
  return Number(o?.input || 0) + Number(o?.read || 0) + Number(o?.output || 0);
}

function hitClass(r: number): string {
  const v = Number(r || 0);
  if (v >= 0.3) return "good";
  if (v >= 0.1) return "mid";
  return "";
}

const modelOptions = computed(() => {
  const arr = range.value === "today" ? stats.byModel || [] : stats.monthByModel || [];
  return (arr as any[]).map((m: any) => m.model);
});

const modelStats = computed(() => {
  const arr = range.value === "today" ? stats.byModel || [] : stats.monthByModel || [];
  return (arr as any[]).filter((m: any) => !modelFilter.value || m.model === modelFilter.value);
});

const chartData = computed(() => {
  if (modelFilter.value) {
    if (range.value === "today") {
      const arr = stats.todayMinuteByModel?.[modelFilter.value] || [];
      return {
        labels: arr.map((x: any) => (x.minute || "").slice(11)),
        tokens: arr.map((x: any) => Number(x.input || 0) + Number(x.read || 0) + Number(x.output || 0)),
        hit: arr.map((x: any) => x.hitRate || 0),
      };
    }
    const arr = stats.monthDailyByModel?.[modelFilter.value] || [];
    return {
      labels: arr.map((x: any) => (x.date || "").slice(8)),
      tokens: arr.map((x: any) => Number(x.input || 0) + Number(x.read || 0) + Number(x.output || 0)),
      hit: arr.map((x: any) => x.hitRate || 0),
    };
  }
  if (range.value === "today") {
    const arr = stats.todayHourly || [];
    return {
      labels: arr.map((x: any) => x.hour),
      tokens: arr.map((x: any) => Number(x.input || 0) + Number(x.read || 0) + Number(x.output || 0)),
      hit: arr.map((x: any) => x.hitRate || 0),
    };
  }
  const arr = stats.monthDaily || [];
  return {
    labels: arr.map((x: any) => (x.date || "").slice(8)),
    tokens: arr.map((x: any) => Number(x.input || 0) + Number(x.read || 0) + Number(x.output || 0)),
    hit: arr.map((x: any) => x.hitRate || 0),
  };
});

const chartLabels = computed(() => chartData.value.labels);
const chartTokens = computed(() => chartData.value.tokens);
const chartHit = computed(() => chartData.value.hit);

const liveSpark = computed(() =>
  liveRecords.value.slice(-40).map((r: any) => Number(r.output_tokens || 0))
);
const liveFeed = computed(() =>
  liveRecords.value
    .slice(-6)
    .reverse()
    .map((r: any) => ({
      time: (r.timestamp || "").slice(11),
      model: r.model || "?",
      hit: r.cache_hit_rate || 0,
      tokens: Number(r.output_tokens || 0),
    }))
);

function setRange(r: "today" | "month") {
  range.value = r;
  modelFilter.value = "";
}

watch(selected, () => {
  modelFilter.value = "";
  loadStats();
  loadLive();
  if (page.value === "config") fetchModels();
});

watch(page, (p) => {
  if (p === "config") fetchModels();
});

let timers: any[] = [];
onMounted(async () => {
  await Promise.all([loadConfig(), loadStatus()]);
  loadStats();
  loadLive();
  warmModels();
  timers.push(setInterval(loadStatus, 3000));
  timers.push(setInterval(loadStats, 5000));
  timers.push(setInterval(loadLive, 3000));
});
onUnmounted(() => timers.forEach(clearInterval));
</script>
