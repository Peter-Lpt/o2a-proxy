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
      <div class="svc-tabs" ref="svcTabs" @mousedown="onTabsDown" @wheel.prevent="onTabsWheel" @scroll="onTabsScroll">
        <button class="svc-tab" :class="{ active: selected === '__all__' }" @click="selected = '__all__'">
          <span class="dot" :class="{ on: anyRunning, busy: anyBusy }"></span>全部
        </button>
        <button
          v-for="s in serviceList"
          :key="s.comment"
          class="svc-tab"
          :class="{ active: selected === s.comment }"
          @click="selected = s.comment"
          :title="s.comment + ' · ' + (s.client || 'auto') + ' · :' + (s.listen_address ?? '?')"
        >
          <span class="dot" :class="{ on: runningMap[s.comment], busy: busyMap[s.comment] }"></span>{{ s.comment }}
          <span class="power" @click.stop="toggleSvc(s.comment)" :title="runningMap[s.comment] ? '停止' : '启动'">
            <Icon :name="runningMap[s.comment] ? 'stop' : 'play'" :size="10" />
          </span>
        </button>
      </div>
      <span v-if="tabEdgeL" class="tab-edge left" aria-hidden="true"></span>
      <span v-if="tabEdgeR" class="tab-edge right" aria-hidden="true"></span>
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
      <button class="tab" :class="{ active: page === 'accounts' }" @click="page = 'accounts'">账号</button>
    </nav>

    <main>
      <!-- 统计 -->
      <section v-show="page === 'stats'" class="panel stats-panel" :class="{ active: page === 'stats' }">
        <!-- 首启引导：无服务 / 有服务无账号 -->
        <div v-if="guide === 'services'" class="card guide-card">
          <div class="guide-title">开始使用 o2a-proxy</div>
          <div class="guide-steps">
            <div class="guide-step"><b>1</b><span>添加服务（选择协议与监听端口）</span><button class="btn btn-sm" @click="addService">添加服务</button></div>
            <div class="guide-step"><b>2</b><span>在「账号」页配置 API Key 与端点</span><button class="btn btn-sm" @click="goAccounts">去配置账号</button></div>
            <div class="guide-step"><b>3</b><span>在服务标签上点击 ▶ 启动代理，这里开始出现实时统计</span></div>
          </div>
        </div>
        <div v-else-if="guide === 'accounts'" class="card guide-card">
          <div class="guide-title">还差一个账号</div>
          <div class="guide-steps">
            <div class="guide-step"><b>1</b><span>账号 = API Key + OpenAI / Anthropic 端点</span><button class="btn btn-sm" @click="goAccounts">去配置账号</button></div>
            <div class="guide-step"><b>2</b><span>回到服务标签绑定该账号后即可启动</span></div>
          </div>
        </div>

        <!-- KPI 汇总（当前小时 / 今日 / 本月） -->
        <div class="kpi-grid">
          <div class="kpi">
            <span class="kpi-l">请求数</span>
            <b class="kpi-v">{{ fmtNum(stats.today?.requests) }}</b>
            <span class="kpi-s">时 {{ fmtNum(stats.current?.requests) }} · 月 {{ fmtNum(stats.month?.requests) }}</span>
          </div>
          <div class="kpi">
            <span class="kpi-l">Token</span>
            <b class="kpi-v">{{ fmtNum(totalOf(stats.today)) }}</b>
            <span class="kpi-s">时 {{ fmtNum(totalOf(stats.current)) }} · 月 {{ fmtNum(totalOf(stats.month)) }}</span>
          </div>
          <div class="kpi">
            <span class="kpi-l">命中率</span>
            <b class="kpi-v" :class="hitClass(stats.today?.hitRate)">{{ fmtPct(stats.today?.hitRate) }}</b>
            <span class="kpi-s">时 {{ fmtPct(stats.current?.hitRate) }} · 月 {{ fmtPct(stats.month?.hitRate) }}</span>
          </div>
          <div class="kpi">
            <span class="kpi-l">费用</span>
            <b class="kpi-v cost">¥{{ fmtCost(stats.today?.cost) }}</b>
            <span class="kpi-s">时 ¥{{ fmtCost(stats.current?.cost) }} · 月 ¥{{ fmtCost(stats.month?.cost) }}</span>
          </div>
        </div>

        <div class="chart-head">
          <div class="chart-left">
            <div class="seg">
              <button class="seg-btn" :class="{ active: range === 'today' }" @click="setRange('today')">今日</button>
              <button class="seg-btn" :class="{ active: range === 'month' }" @click="setRange('month')">本月</button>
            </div>
            <button class="cal-btn" :class="{ active: range === 'custom' }" :title="calOpen ? '收起日历' : '自定义日期区间'" @click="calOpen = !calOpen">
              {{ range === 'custom' ? rangeLabel : '日历' }}<span class="cal-caret">{{ calOpen ? '▲' : '▼' }}</span>
            </button>
          </div>
          <div class="chart-head-right">
            <SelectBox v-model="modelFilter" :options="filterOptions" size="sm" :title="'按模型过滤（' + rangeLabel + '）'" />
            <button class="icon-btn" :class="{ spinning: statsLoading }" title="刷新" @click="loadStats()"><Icon name="refresh" :size="12" /></button>
          </div>
        </div>
        <CalendarHeat v-if="calOpen" :service="statsService" @select="onCalSelect" @quick="onCalQuick" />

        <div v-if="statsError" class="stats-err"><Icon name="alert" :size="12" />{{ statsError }}</div>

        <div class="card chart-box">
          <div class="chart-title">
            <span>缓存命中率 & Token 消耗（{{ chartTitle }}）</span>
            <span v-if="deltaText" class="chart-delta" :class="deltaCls">{{ deltaText }}</span>
            <button class="chart-reset" title="复位缩放与平移" @click="chartResetKey++"><Icon name="refresh" :size="10" /> 复位</button>
          </div>
          <LineChart :labels="chartLabels" :input="chartInput" :read="chartRead" :output="chartOutput" :hit-rate="chartHit" :theme="theme" :reset-key="chartResetKey" />
          <div class="chart-note">* 命中率 = 缓存读 / (缓存读 + 输入)，对齐 Anthropic 官方口径；滚轮缩放 · 拖拽平移 · 双击复位</div>
          <div class="updated">{{ stats.updatedAt ? "更新于 " + stats.updatedAt.replace('T', ' ').slice(0, 19) : "—" }}</div>
        </div>

        <div v-if="modelStats.length" class="card model-stats-card">
          <div class="fc-h">
            <span>{{ rangeLabel }}按模型</span>
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

        <div v-if="anyRunning" class="card live-card">
          <div class="live-head">
            <span class="live-title"><span class="live-dot"></span>实时调用</span>
            <span>{{ liveSum }}</span>
          </div>
          <Spark :points="liveSpark" :height="40" class="live-spark" />
          <div class="live-feed">
            <div v-for="r in liveFeed" :key="r.key" class="lf-row">
              <span class="t">{{ r.time }}</span>
              <span v-if="r.service" class="svc">{{ r.service }}</span>
              <span class="k">↑{{ fmtNum(r.total) }} · 读{{ fmtNum(r.cacheRead) }} · ↓{{ fmtNum(r.output) }}</span>
              <span class="h" :class="r.hitCls">{{ r.hitPct }}%</span>
            </div>
          </div>
        </div>
      </section>

      <!-- 配置 -->
      <section v-show="page === 'config'" class="panel" :class="{ active: page === 'config' }">
        <div v-if="activeSvcRunning" class="cfg-lock">
          <span class="cfg-lock-msg"><Icon name="lock" :size="13" /> 服务「{{ activeSvc.comment }}」运行中，该服务配置已锁定</span>
          <button class="btn btn-sm" @click="api.stopService(activeSvc.comment)">停止该代理以编辑</button>
        </div>
        <form @submit.prevent="saveConfig">
          <!-- 全部视图：全局配置 + 概览 -->
          <template v-if="selected === ALL">
            <div class="card form-card">
              <div class="fc-h">全局配置 <span class="fc-sub">作用于所有服务</span></div>
              <label>认证令牌 auth_token <input v-model="cfg.auth_token" type="text" :disabled="anyRunning" /></label>
              <label class="inline"><input v-model="cfg.cache_stats_enabled" type="checkbox" :disabled="anyRunning" /><span>启用缓存统计</span></label>
              <div class="grid2">
                <label>保留天数 <input v-model.number="cfg.cache_stats_retention_days" type="number" min="1" max="365" :disabled="anyRunning" /></label>
                <label>统计目录 <input v-model="cfg.cache_stats_dir" type="text" placeholder="留空使用默认" :disabled="anyRunning" /></label>
              </div>
              <div class="model-hint">留空使用默认目录：{{ status.statsDir || "…" }}</div>
            </div>
            <div class="card form-card">
              <div class="fc-h">概览</div>
              <div class="ov-rows">
                <div class="ov-row"><span class="ov-k">服务</span><b>{{ serviceList.length }}</b><span class="ov-sub">{{ runningCount }} 运行中 · {{ stoppedCount }} 已停止</span></div>
                <div class="ov-row"><span class="ov-k">账号</span><b>{{ accountList.length }}</b><span class="ov-sub">{{ dualCount }} 双协议 · {{ oaCount }} OpenAI · {{ anCount }} Anthropic</span></div>
                <div class="ov-row"><span class="ov-k">端口</span><b>{{ portSummary }}</b><span class="ov-sub">每服务独立监听</span></div>
              </div>
              <p class="hint">服务与账号分开管理：先到「账号」页添加账号（API Key + 端点），再在具体服务标签下绑定。引擎按 client × 账号端点自动选择透传或转换。</p>
            </div>
          </template>
          <!-- 单独服务视图：该服务配置 -->
          <template v-else>
            <div class="card form-card">
              <div class="fc-h">服务配置 <span class="fc-sub">{{ activeSvc?.comment || "未配置" }}</span></div>
              <div v-if="activeSvc">
                <div v-if="activeSvcAccount" class="acc-mini">
                  <span class="acc-mini-title">所属账号</span>
                  <span class="acc-kind" :class="accKindClass(activeSvcAccount)">{{ accKindLabel(activeSvcAccount) }}</span>
                  <span class="acc-mini-ep oa">{{ activeSvcAccount.openai_url || "无 OpenAI 端点" }}</span>
                  <span class="acc-mini-ep an">{{ activeSvcAccount.anthropic_url || "无 Anthropic 端点" }}</span>
                  <button type="button" class="link-btn" @click="goAccounts">管理账号 →</button>
                </div>
                <label>备注 comment <input v-model="activeSvc.comment" type="text" :disabled="activeSvcRunning" /></label>
                <label>账号 account
                  <SelectBox v-model="activeSvc.account" :options="accountOptions" :disabled="activeSvcRunning" />
                </label>
                <label>客户端类型 client
                  <SelectBox v-model="activeSvc.client" :options="clientOptions" :disabled="activeSvcRunning" />
                </label>
                <label>入口协议 api <span class="fc-sub" style="font-weight:400">（推荐显式声明）</span>
                  <SelectBox v-model="activeSvc.api" :options="apiOptions" placeholder="默认（回退 client/自动识别）" allow-custom :disabled="activeSvcRunning" />
                </label>
                <label v-if="activeSvc.api === 'openai-responses'" class="upstream-api-label">上游协议 upstream_api <span class="fc-sub" style="font-weight:400">（上游原生支持 Responses 时选透传）</span>
                  <SelectBox v-model="activeSvc.upstream_api" :options="upstreamApiOptions" :disabled="activeSvcRunning" />
                </label>
                <div class="proto-row"><span class="proto-lbl">入口协议</span><span class="proto-val">{{ entryProto }}</span></div>
                <label>主模型 model <SelectBox v-model="activeSvc.model" :options="fetchedModels" allow-custom placeholder="选择或输入模型" :disabled="activeSvcRunning" /></label>
                <label class="inline"><input v-model="activeSvc.override_model" type="checkbox" :disabled="activeSvcRunning" /><span>覆盖客户端模型 override_model <span class="fc-sub" style="font-weight:400">（关：透传客户端请求的模型名）</span></span></label>
                <label>监听端口 listen_address <input v-model="activeSvc.listen_address" type="number" min="1" max="65535" :disabled="activeSvcRunning" /></label>
                <div class="grid2">
                  <label>max_tokens <input v-model="activeSvc.max_tokens" type="number" min="1" :disabled="activeSvcRunning" /></label>
                  <label class="inline"><input v-model="activeSvc.context_1m" type="checkbox" :disabled="activeSvcRunning" /><span>1M 上下文</span></label>
                </div>
                <div class="model-hint">{{ outHint }}</div>
              </div>
              <div v-else class="hint">尚未配置服务，点击右上角「添加服务」。</div>
              <div class="model-hint" :class="{ err: modelHint.err }">{{ modelHint.text }}</div>
            </div>
          </template>

          <div class="form-actions">
            <button type="submit" class="btn btn-primary" :disabled="activeSvcRunning">保存配置</button>
            <button v-if="selected !== ALL" type="button" class="btn btn-danger" @click="removeSvc" :disabled="activeSvcRunning || !activeSvc">删除此服务</button>
            <button type="button" class="btn" @click="api.openConfigFile()">打开 config.json</button>
          </div>
        </form>
      </section>

      <!-- 账号 -->
      <section v-show="page === 'accounts'" class="panel" :class="{ active: page === 'accounts' }">
        <div v-for="acc in accountList" :key="acc.id" class="card form-card acc-card">
          <div class="fc-h">
            <span class="acc-name">{{ acc.name || acc.id }}</span>
            <span class="acc-kind" :class="accKindClass(acc)">{{ accKindLabel(acc) }}</span>
            <span class="fc-sub">{{ accStatsText(acc) }}</span>
          </div>
          <template v-if="editingAcc === acc.id">
            <div class="acc-edit">
              <label>账号名称
                <input v-model="acc.name" type="text" spellcheck="false" placeholder="如：阿里云 DashScope" />
              </label>
              <label>API Key
                <div class="field-row">
                  <input v-model="acc.api_key" :type="showKey ? 'text' : 'password'" autocomplete="off" spellcheck="false" placeholder="sk-…" @input="debounceTestAccount" />
                  <button type="button" class="icon-btn" @click="showKey = !showKey" :title="showKey ? '隐藏 API Key' : '显示 API Key'">
                    <Icon :name="showKey ? 'eye-off' : 'eye'" :size="13" />
                  </button>
                </div>
              </label>
              <div class="ep-grid">
                <label class="ep-field">
                  <span class="ep-tag oa">OpenAI 端点</span>
                  <input v-model="acc.openai_url" type="text" spellcheck="false" autocomplete="off" placeholder="https://…/v1 或 chat/completions 完整地址" @input="debounceTestAccount" />
                </label>
                <label class="ep-field">
                  <span class="ep-tag an">Anthropic 端点</span>
                  <input v-model="acc.anthropic_url" type="text" spellcheck="false" autocomplete="off" placeholder="https://api.anthropic.com/v1/messages" />
                </label>
              </div>
              <div class="model-hint" :class="{ err: accHint.err }">{{ accHint.text }}</div>
              <div class="form-actions">
                <button type="button" class="btn btn-primary" @click="saveConfig()">保存配置</button>
                <button type="button" class="btn" @click="editingAcc = null">收起</button>
                <span class="acc-edit-tip">同 Key 两端点复用；保存后该账号下所有服务生效</span>
              </div>
            </div>
          </template>
          <template v-else>
            <div class="acc-endpoints">
              <div><span class="ep-lbl"><span class="ep-dot oa"></span>OpenAI</span><span class="ep-val">{{ acc.openai_url || "未配置" }}</span></div>
              <div><span class="ep-lbl"><span class="ep-dot an"></span>Anthropic</span><span class="ep-val">{{ acc.anthropic_url || "未配置" }}</span></div>
            </div>
            <div class="form-actions">
              <button type="button" class="btn btn-sm" @click="editingAcc = acc.id">编辑账号</button>
              <button type="button" class="btn btn-sm btn-danger" @click="removeAccount(acc)">删除</button>
            </div>
          </template>
        </div>
        <div class="form-actions">
          <button type="button" class="btn btn-primary" @click="addAccount"><Icon name="plus" :size="12" /> 新建账号</button>
          <button type="button" class="btn" @click="saveConfig()">保存配置</button>
        </div>
        <p class="hint">账号 = 一个 API Key + 最多两个端点（OpenAI / Anthropic，同 Key）。类型自动推导：双端点即双协议，两端点都能服务对应客户端；只有一个端点则另一类客户端走转换（Claude→OpenAI）。</p>
      </section>
    </main>

    <footer class="foot">
      <span>o2a-proxy · v0.1.0</span>
      <button class="link-btn" @click="quitApp">退出应用</button>
    </footer>
    <div id="toast" class="toast" :class="{ show: !!toast, [toastType]: !!toast }">{{ toast }}</div>
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
import { api, fmtCost, fmtNum, fmtPct } from "./api";
import { hitTier } from "./format";
import CalendarHeat from "./components/CalendarHeat.vue";
import ConfirmDialog from "./components/ConfirmDialog.vue";
import Icon from "./components/Icon.vue";
import LineChart from "./components/LineChart.vue";
import SelectBox from "./components/SelectBox.vue";
import Spark from "./components/Spark.vue";
import { applyTheme, getTheme, toggleTheme, watchSystemTheme, type Theme } from "./theme";

const ALL = "__all__";
const cfg = reactive<any>({});
const status = reactive<any>({ services: [] });
const stats = reactive<any>({});
const liveRecords = ref<any[]>([]);
const selected = ref<string>(ALL);
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
const page = ref<"stats" | "config" | "accounts">("stats");
type RangeKey = "today" | "yesterday" | "week" | "lastweek" | "month" | "lastmonth" | "custom";
// 区间档位（历史档由日历点选自定义区间替代，仅今日/本月暴露为主档）
const rangeOptions: { value: RangeKey; label: string }[] = [
  { value: "today", label: "今日" },
  { value: "yesterday", label: "昨日" },
  { value: "week", label: "本周" },
  { value: "lastweek", label: "上周" },
  { value: "month", label: "本月" },
  { value: "lastmonth", label: "上月" },
];
function readRange(): RangeKey {
  // 仅接受主界面暴露的档位（今日 / 本月）；自定义区间不记忆，刷新回今日
  try {
    const v = localStorage.getItem("o2a-stats-range");
    if (v === "today" || v === "month") return v as RangeKey;
  } catch (_) {}
  return "today";
}
const range = ref<RangeKey>(readRange());
const calOpen = ref(false);
const customRange = ref<{ start: string; end: string } | null>(null);
const modelFilter = ref<string>("");
const toast = ref("");
const toastType = ref<"info" | "success" | "error">("info");
const offError = ref("");
const showKey = ref(false);
const theme = ref<Theme>(getTheme());
const statsLoading = ref(false);
const statsError = ref("");
const chartResetKey = ref(0);
const tabEdgeL = ref(false);
const tabEdgeR = ref(false);
const fetchedModels = ref<string[]>([]);
const modelHint = ref<{ text: string; err: boolean }>({ text: "", err: false });
const modelCache = new Map<string, string[]>();
const clientOptions = [
  { value: "auto", label: "auto（自动识别协议）" },
  { value: "anthropic", label: "anthropic（Claude Code）" },
  { value: "openai", label: "openai（Codex / OpenAI 兼容）" },
];
const apiOptions = [
  { value: "openai-completions", label: "openai-completions（pi / 常规 Chat，整包透传）" },
  { value: "openai-responses", label: "openai-responses（Codex 专属 Responses）" },
  { value: "anthropic-messages", label: "anthropic-messages（Claude Code）" },
];
const upstreamApiOptions = [
  { value: "openai-completions", label: "openai-completions（上游只支持 Chat → 自动转换）" },
  { value: "openai-responses", label: "openai-responses（上游原生支持 Responses，如 DeepSeek → 整包透传）" },
];
const editingAcc = ref<string | null>(null);
const accHint = ref<{ text: string; err: boolean }>({ text: "", err: false });
const accStats = reactive<Record<string, any>>({});
const accStatsState = reactive<Record<string, "loading" | "ok" | "err">>({});
let modelsSeq = 0;
let accTimer: any = null;

// ---------- 确认弹层 ----------
const confirmBox = ref<{ title: string; message: string; okText?: string; action: () => void } | null>(null);
function askConfirm(title: string, message: string, action: () => void, okText = "确认") {
  confirmBox.value = { title, message, action, okText };
}
function onConfirmOk() {
  const cb = confirmBox.value;
  confirmBox.value = null;
  cb?.action();
}

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

function accKindLabel(acc: any): string {
  const o = !!String(acc?.openai_url || "").trim();
  const a = !!String(acc?.anthropic_url || "").trim();
  if (o && a) return "双协议";
  if (o) return "OpenAI";
  if (a) return "Anthropic";
  return "未配置端点";
}
function accKindClass(acc: any): string {
  const o = !!String(acc?.openai_url || "").trim();
  const a = !!String(acc?.anthropic_url || "").trim();
  return o && a ? "both" : o ? "openai" : a ? "anthropic" : "none";
}
const accountList = computed(() => cfg.accounts || []);
function accountById(id: string | undefined | null): any {
  return (cfg.accounts || []).find((a: any) => a.id === id);
}
const accountOptions = computed(() =>
  (cfg.accounts || []).map((a: any) => ({
    value: a.id,
    label: (a.name || a.id) + " · " + accKindLabel(a),
  }))
);

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
    Promise.all(svcs.map((s: any) => api.getStats(s.comment)))
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
const accStatsText = (acc: any) => {
  const st = accStats[acc.id];
  const stt = accStatsState[acc.id];
  if (stt === "loading") return "统计加载中…";
  if (stt === "err") return "统计读取失败";
  if (!st) return "今日 —";
  const svcN = (cfg.services || []).filter((s: any) => s.account === acc.id).length;
  return `服务×${svcN} · 今日 ¥${fmtCost(st.today.cost)} / ${fmtNum(st.today.requests)} 次 · 本月 ¥${fmtCost(st.month.cost)}`;
};

function setAccHint(text: string, err = false) {
  accHint.value = { text, err };
}

// 账号连通性测试：探测 OpenAI 端点的 /models
async function testAccount() {
  const acc = accountList.value.find((a: any) => a.id === editingAcc.value);
  if (!acc) {
    setAccHint("");
    return;
  }
  const baseUrl = String(acc.openai_url || "").trim();
  const apiKey = String(acc.api_key || "").trim();
  if (!baseUrl) {
    setAccHint("填写 OpenAI 端点后可连通性测试", false);
    return;
  }
  const seq = ++modelsSeq;
  setAccHint("正在测试连接…", false);
  try {
    const res = await api.fetchModels(baseUrl, apiKey);
    if (seq !== modelsSeq) return;
    if (res.ok) {
      modelCache.set(baseUrl, res.models);
      fetchedModels.value = res.models;
      setAccHint(`连接成功：${res.models.length} 个模型可用`, false);
    } else {
      setAccHint("连接失败：" + (res.error || ""), true);
    }
  } catch (e: any) {
    if (seq !== modelsSeq) return;
    setAccHint("连接失败：" + e, true);
  }
}
function debounceTestAccount() {
  clearTimeout(accTimer);
  accTimer = setTimeout(() => testAccount(), 900);
}

function addAccount() {
  cfg.accounts = cfg.accounts || [];
  const used = new Set((cfg.accounts || []).map((a: any) => a.id));
  let n = 1;
  while (used.has("acc-" + n)) n++;
  const acc = { id: "acc-" + n, name: "账号" + n, api_key: "", openai_url: "", anthropic_url: "" };
  cfg.accounts.push(acc);
  editingAcc.value = acc.id;
  accHint.value = { text: "已新建账号，填写端点与 Key 后保存", err: false };
}

function removeAccount(acc: any) {
  const refs = (cfg.services || []).filter((s: any) => s.account === acc.id);
  if (refs.length) {
    showToast(`无法删除：仍有 ${refs.length} 个服务引用该账号（${refs.map((s: any) => s.comment).join("、")}）`, "error");
    return;
  }
  askConfirm(
    "删除账号",
    `确定删除账号「${acc.name || acc.id}」？\n其 API Key 与端点将从配置中移除，保存后生效。`,
    () => {
      cfg.accounts = cfg.accounts.filter((a: any) => a.id !== acc.id);
      if (editingAcc.value === acc.id) editingAcc.value = null;
      showToast("已删除账号，点击保存配置生效", "success");
    },
    "删除"
  );
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
  // 同一地址已缓存：直接展示（即使该服务未填 Key 也能复用同提供商的模型列表）
  if (modelCache.has(baseUrl)) {
    fetchedModels.value = modelCache.get(baseUrl)!;
    setModelHint(`已加载 ${fetchedModels.value.length} 个模型（${baseUrl}）`, false);
    return;
  }
  if (!apiKey) {
    setModelHint("该账号未填写 API Key，拉取模型列表需要 Key", false);
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
      setModelHint("该账号拉取模型失败：" + (res.error || ""), true);
    }
  } catch (e: any) {
    if (seq !== modelsSeq) return;
    setModelHint("该账号拉取模型失败：" + e, true);
  }
}

async function warmModels() {
  const first = (cfg.accounts || []).find(
    (a: any) => String(a.openai_url || "").trim() && String(a.api_key || "").trim()
  );
  if (!first) return;
  const baseUrl = String(first.openai_url).trim();
  if (modelCache.has(baseUrl)) return;
  const seq = ++modelsSeq;
  try {
    const res = await api.fetchModels(baseUrl, String(first.api_key).trim());
    if (seq === modelsSeq && res.ok) modelCache.set(baseUrl, res.models);
  } catch (_) {}
}

let toastTimer: any = null;
function showToast(msg: string, type: "info" | "success" | "error" = "info") {
  toast.value = msg;
  toastType.value = type;
  if (toastTimer) clearTimeout(toastTimer);
  toastTimer = setTimeout(() => (toast.value = ""), type === "error" ? 4200 : 2200);
}

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
const serviceList = computed(() => (cfg.services || []).filter((s: any) => s.comment));
const runningMap = computed<Record<string, boolean>>(() => {
  const m: Record<string, boolean> = {};
  (status.services || []).forEach((s: any) => {
    m[s.name] = !!s.running;
  });
  return m;
});
// 忙碌态：服务有活跃任务（引擎 /status 的 task.active）
const busyMap = computed<Record<string, boolean>>(() => {
  const m: Record<string, boolean> = {};
  (status.services || []).forEach((s: any) => {
    m[s.name] = !!s.task?.active;
  });
  return m;
});
const anyRunning = computed(() => Object.values(runningMap.value).some(Boolean));
const anyBusy = computed(() => Object.values(busyMap.value).some(Boolean));
const headOrbCls = computed(() => (anyBusy.value ? "busy" : anyRunning.value ? "running" : "stopped"));
const headOrbTitle = computed(() =>
  anyBusy.value ? "正在处理请求" : anyRunning.value ? "运行中" : "已停止"
);
const headSubText = computed(() =>
  anyBusy.value ? "代理处理中" : anyRunning.value ? "代理运行中" : "代理已停止"
);
// 首启引导：无服务 / 有服务但无账号
const guide = computed(() => {
  if (!serviceList.value.length) return "services";
  if (!(cfg.accounts || []).length) return "accounts";
  return "";
});
const activeSvcRunning = computed(
  () => !!activeSvc.value && !!runningMap.value[activeSvc.value.comment]
);
const activeSvc = computed(
  () => serviceList.value.find((s: any) => s.comment === selected.value) || serviceList.value[0]
);
const activeSvcAccount = computed(() => accountById(activeSvc.value?.account));
const runningCount = computed(() => Object.values(runningMap.value).filter(Boolean).length);
const stoppedCount = computed(() => Math.max(0, serviceList.value.length - runningCount.value));
const dualCount = computed(
  () => (cfg.accounts || []).filter((a: any) => String(a.openai_url || "").trim() && String(a.anthropic_url || "").trim()).length
);
const oaCount = computed(
  () => (cfg.accounts || []).filter((a: any) => String(a.openai_url || "").trim() && !String(a.anthropic_url || "").trim()).length
);
const anCount = computed(
  () => (cfg.accounts || []).filter((a: any) => !String(a.openai_url || "").trim() && String(a.anthropic_url || "").trim()).length
);
const portSummary = computed(() => {
  const ports = serviceList.value.map((s: any) => s.listen_address).filter((p: any) => p);
  if (!ports.length) return "—";
  const n = ports.map(Number);
  return Math.min(...n) === Math.max(...n) ? String(n[0]) : `${Math.min(...n)}–${Math.max(...n)}`;
});
function goAccounts() {
  page.value = "accounts";
}

// 服务入口提示：api 显式声明优先，回退 client / 自动识别
const entryProto = computed(() => {
  const api = activeSvc.value?.api || "";
  if (api === "anthropic-messages") return "入口 /v1/messages（Anthropic Messages）";
  if (api === "openai-responses") return "入口 /v1/responses（OpenAI Responses）";
  if (api === "openai-completions") return "入口 /chat/completions（Chat Completions）";
  const c = activeSvc.value?.client || "auto";
  if (c === "anthropic") return "入口 /v1/messages（Anthropic Messages）";
  if (c === "openai") return "入口 /v1/responses · /chat/completions（OpenAI 兼容）";
  return "自动识别：/v1/messages · /v1/responses · /chat/completions";
});
const outHint = computed(() => {
  const s = activeSvc.value;
  const acc = accountById(s?.account);
  if (!s || !acc) return "请先绑定账号";
  const o = String(acc.openai_url || "").trim();
  const a = String(acc.anthropic_url || "").trim();
  const api = s.api || "";
  if (api === "openai-completions") {
    return o
      ? `入口 Chat → 出口 ${o}（整包透传，零转换）`
      : "⚠ 该账号未配置 OpenAI 端点";
  }
  if (api === "openai-responses") {
    if (!o) return "⚠ 该账号未配置 OpenAI 端点";
    const up = s.upstream_api || "openai-completions";
    return up === "openai-responses"
      ? `入口 Responses → 出口 ${o}（上游原生支持，整包透传）`
      : `入口 Responses → 出口 ${o}（转 Chat 发送，响应转回 Responses）`;
  }
  if (api === "anthropic-messages") {
    return a
      ? `入口 /v1/messages → 出口 ${a}（直连透传）`
      : o
        ? `入口 /v1/messages → 出口 ${o}（转换发送）`
        : "⚠ 该账号未配置任何端点";
  }
  // 未声明 api：回退 client 推导
  const c = s.client || "auto";
  if (c === "openai") {
    return o
      ? `入口 /v1/responses · /chat/completions → 出口 ${o}（透传）`
      : "⚠ 该账号未配置 OpenAI 端点，Codex 请求会被拒绝";
  }
  if (c === "anthropic") {
    return a
      ? `入口 /v1/messages → 出口 ${a}（直连透传）`
      : o
        ? `入口 /v1/messages → 出口 ${o}（转换发送）`
        : "⚠ 该账号未配置任何端点";
  }
  // auto
  if (a) return `自动识别 → Claude 透传 ${a}；Codex 请求需 OpenAI 端点`;
  return o ? `自动识别 → 出口 ${o}（Claude 转换 / Codex 透传）` : "⚠ 该账号未配置任何端点";
});
const floatService = computed(() =>
  selected.value === ALL ? "" : serviceList.value.find((s: any) => s.comment === selected.value)?.comment || ""
);
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
    migrateAccounts(cfg);
  } catch (e: any) {
    showToast("读取配置失败: " + e, "error");
  }
}

async function loadStatus() {
  try {
    const s = await api.getStatus();
    Object.keys(status).forEach((k) => delete (status as any)[k]);
    Object.assign(status, s);
  } catch (e: any) {
    showToast("读取状态失败: " + e, "error");
  }
}

async function loadStats() {
  statsLoading.value = true;
  try {
    const c = customRange.value;
    const isCustom = range.value === "custom";
    const s = await api.getStats(
      statsService.value,
      range.value,
      isCustom && c ? c.start : undefined,
      isCustom && c ? c.end : undefined
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
    showToast("该服务尚未保存，请先保存配置", "error");
    return;
  }
  try {
    await api.toggleService(name);
    await loadStatus();
    offError.value = "";
    showToast(name + (runningMap.value[name] ? " 已启动" : " 已停止"), "success");
  } catch (e: any) {
    showToast("操作失败: " + e, "error");
    offError.value = String(e);
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
  const svc = {
    comment: "service-" + (cfg.services.length + 1),
    account: (cfg.accounts || [])[0]?.id || "",
    client: "auto",
    model: "qwen-plus",
    override_model: true,
    listen_address: port,
    context_1m: false,
    max_tokens: 4096,
  };
  cfg.services.push(svc);
  selected.value = svc.comment;
  page.value = "config";
  showToast("已添加服务，绑定账号后保存", "success");
}

function removeSvc() {
  const s = activeSvc.value;
  if (!s) return;
  const name = s.comment;
  const running = !!runningMap.value[name];
  askConfirm(
    "删除服务",
    running
      ? `服务「${name}」正在运行，删除将先停止它并从配置中移除。\n保存配置后生效。`
      : `确定删除服务「${name}」？\n保存配置后生效。`,
    () => {
      if (running) api.stopService(name).catch(() => {});
      cfg.services = cfg.services.filter((x: any) => x.comment !== name);
      selected.value = ALL;
      showToast("已删除服务，点击保存生效", "success");
    },
    "删除"
  );
}

function validateConfig(): string | null {
  const svcs = cfg.services || [];
  const seen = new Set<string>();
  for (const s of svcs) {
    const comment = String(s.comment || "").trim();
    if (!comment) return "服务备注 comment 不能为空";
    if (seen.has(comment)) return "服务备注重复：" + comment;
    seen.add(comment);
    if (!accountById(s.account)) return `服务 ${comment} 未绑定有效账号`;
    const port = Number(s.listen_address);
    if (!Number.isInteger(port) || port < 1 || port > 65535) {
      return `服务 ${comment} 的监听端口无效（需为 1-65535 整数）`;
    }
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
    c.accounts = (c.services as any[]).map((s: any, i: number) => ({
      id: "acc-" + (i + 1),
      name: s.comment || "账号" + (i + 1),
      api_key: s.openai_api_key || "",
      openai_url: s.openai_base_url || "",
      anthropic_url: s.anthropic_base_url || "",
    }));
    (c.services as any[]).forEach((s: any, i: number) => {
      s.account = "acc-" + (i + 1);
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
    if (s.max_tokens === "" || s.max_tokens === undefined || s.max_tokens === null) {
      delete s.max_tokens;
    } else {
      s.max_tokens = Number(s.max_tokens);
    }
    s.context_1m = !!s.context_1m;
    if (!s.api) delete s.api;
    if (!s.upstream_api || s.upstream_api === "openai-completions") delete s.upstream_api;
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

async function saveConfig() {
  const err = validateConfig();
  if (err) {
    showToast(err, "error");
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
    loadAccountStats();
    for (const n of removed) {
      await api.stopService(n).catch(() => {});
    }
    // 引擎启动时一次性读取配置（不热加载）：运行中保存只对下次启动生效
    if (anyRunning.value) {
      showToast("配置已写入 config.json，运行中的服务重启后生效", "info");
    } else {
      showToast("配置已保存", "success");
    }
  } catch (e: any) {
    showToast("保存失败: " + e, "error");
  }
}

function totalOf(o: any): number {
  return Number(o?.input || 0) + Number(o?.read || 0) + Number(o?.output || 0);
}

function hitClass(r: number): string {
  return hitTier(r, false);
}

const modelOptions = computed(() => (stats.byModel || []).map((m: any) => m.model));

const filterOptions = computed(() => [
  { value: "", label: "全部模型" },
  ...modelOptions.value.map((m: any) => ({ value: m, label: m })),
]);

const modelStats = computed(() => {
  const arr = stats.byModel || [];
  return (arr as any[]).filter((m: any) => !modelFilter.value || m.model === modelFilter.value);
});

// 所选范围的相关文案与同比
const rangeLabel = computed(() => {
  if (range.value === "custom") {
    const s = String(stats.rangeStart || "").slice(5);
    const e = String(stats.rangeEnd || "").slice(5);
    return s && e ? `${s} ~ ${e}` : "自定义";
  }
  const r = String(stats.range || range.value);
  return rangeOptions.find((o) => o.value === r)?.label || "今日";
});
// 历史入口已由日历取代（点两个日期即可），仅保留今日/本月两档
const prevLabel = computed(() => String(stats.prevLabel || "上期"));
const seriesKind = computed(() => String(stats.seriesKind || "minute"));
const kindLabel = computed(() =>
  seriesKind.value === "minute" ? "逐分钟" : seriesKind.value === "hour" ? "逐小时" : "逐日"
);
const chartTitle = computed(() => `${rangeLabel.value}${kindLabel.value}`);
const deltaText = computed(() => {
  // 同比只在历史档显示（今日/本月为当前期，不比）
  if (range.value === "today" || range.value === "month") return "";
  const a = Number(stats.rangeAgg?.requests || 0);
  const b = Number(stats.prevAgg?.requests || 0);
  if (!b) return `较${prevLabel.value} —`;
  const pct = Math.round(((a - b) / b) * 100);
  return `较${prevLabel.value} ${pct >= 0 ? "+" : ""}${pct}% ${pct >= 0 ? "↑" : "↓"}`;
});
const deltaCls = computed(() => {
  if (range.value === "today" || range.value === "month") return "";
  const a = Number(stats.rangeAgg?.requests || 0);
  const b = Number(stats.prevAgg?.requests || 0);
  if (!b) return "";
  return a >= b ? "up" : "down";
});

const chartData = computed(() => {
  const s: any[] = stats.series || [];
  return {
    labels: s.map((x: any) => x.label),
    input: s.map((x: any) => Number(x.input || 0)),
    read: s.map((x: any) => Number(x.read || 0)),
    output: s.map((x: any) => Number(x.output || 0)),
    hit: s.map((x: any) => x.hitRate || 0),
  };
});

const chartLabels = computed(() => chartData.value.labels);
const chartInput = computed(() => chartData.value.input);
const chartRead = computed(() => chartData.value.read);
const chartOutput = computed(() => chartData.value.output);
const chartHit = computed(() => chartData.value.hit);

const liveSpark = computed(() =>
  liveRecords.value.slice(-40).map((r: any) => Number(r.cache_hit_rate || 0))
);
const liveFeed = computed(() =>
  liveRecords.value
    .slice(0, 30)
    .map((r: any) => {
      const rate = Number(r.cache_hit_rate || 0);
      return {
        key: `${r.timestamp}_${r.service}_${r.output_tokens}`,
        time: String(r.timestamp || "").slice(11, 19),
        service: r.service || "",
        total:
          Number(r.input_tokens || 0) +
          Number(r.cache_read_tokens || 0) +
          Number(r.cache_write_tokens || 0),
        cacheRead: Number(r.cache_read_tokens || 0),
        output: Number(r.output_tokens || 0),
        hitPct: (rate * 100).toFixed(0),
        hitCls: hitTier(rate, true),
      };
    })
);

// 近 5 分钟汇总（命中率用近 5 分钟 token 加权）
const liveSum = computed(() => {
  const pool = liveRecords.value;
  if (!pool.length) return "等待请求…";
  const now = Date.now();
  let n = 0, inp = 0, rd = 0, wr = 0, out = 0;
  for (const r of pool) {
    const ts = Date.parse(r.timestamp);
    if (isNaN(ts) || now - ts > 300000) continue;
    n++;
    inp += Number(r.input_tokens || 0);
    rd += Number(r.cache_read_tokens || 0);
    wr += Number(r.cache_write_tokens || 0);
    out += Number(r.output_tokens || 0);
  }
  if (n > 0) {
    const hr = rd + inp > 0 ? rd / (rd + inp) : 0;
    return `近5min：${n} 次 · ${fmtNum(inp + rd + wr + out)} tok · 命中 ${(hr * 100).toFixed(0)}%`;
  }
  const last = pool[pool.length - 1];
  return `最近一次 ${String(last.timestamp || "").slice(11, 19)}`;
});

function setRange(r: RangeKey) {
  range.value = r;
  try {
    if (r === "custom") localStorage.removeItem("o2a-stats-range");
    else localStorage.setItem("o2a-stats-range", r);
  } catch (_) {}
  modelFilter.value = "";
  calOpen.value = false;
  loadStats();
}

// 自定义区间：起止日期（YYYY-MM-DD）；保持日历展开，便于用户看到选中区间并可微调
function setCustomRange(start: string, end: string) {
  customRange.value = { start, end };
  range.value = "custom";
  try {
    localStorage.removeItem("o2a-stats-range");
  } catch (_) {}
  modelFilter.value = "";
  loadStats();
}

function onCalSelect(start: string, end: string) {
  setCustomRange(start, end);
}

// 日历底部快捷：昨日 / 近7天 / 近30天（今日/本月由外部主档位提供，避免重复）
function onCalQuick(key: string) {
  const now = new Date();
  const p = (n: number) => String(n).padStart(2, "0");
  const iso = (d: Date) => `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`;
  if (key === "yesterday") {
    const y = new Date(now.getTime() - 86400000);
    const s = iso(y);
    setCustomRange(s, s);
    return;
  }
  const days = key === "7d" ? 6 : 29;
  const start = iso(new Date(now.getTime() - days * 86400000));
  setCustomRange(start, iso(now));
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

let timers: any[] = [];
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
  timers.push(setInterval(() => { if (panelVisible.value) loadStatus(); }, 3000));
  timers.push(setInterval(() => { if (panelVisible.value) loadStats(); }, 5000));
  timers.push(setInterval(() => { if (panelVisible.value) loadLive(); }, 3000));
  timers.push(setInterval(() => { if (panelVisible.value) loadAccountStats(); }, 10000));
});
onUnmounted(() => {
  unlistenPanel?.();
  document.removeEventListener("visibilitychange", onVisibilityChange);
  timers.forEach(clearInterval);
});
</script>
