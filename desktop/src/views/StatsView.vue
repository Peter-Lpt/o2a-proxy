<template>
        <!-- 首启引导：无服务 / 有服务无账号 -->
        <div v-if="guide === 'services'" class="card guide-card">
          <div class="guide-title">开始使用 o2a-proxy</div>
          <div class="guide-steps">
            <div class="guide-step"><b>1</b><span>添加服务（选择协议与监听端口）</span><button class="btn btn-sm" @click="$emit('add-service')">添加服务</button></div>
            <div class="guide-step"><b>2</b><span>在「账号」页配置 API Key 与端点</span><button class="btn btn-sm" @click="$emit('go-accounts')">去配置账号</button></div>
            <div class="guide-step"><b>3</b><span>在服务标签上点击 ▶ 启动代理，这里开始出现实时统计</span></div>
          </div>
        </div>
        <div v-else-if="guide === 'accounts'" class="card guide-card">
          <div class="guide-title">还差一个账号</div>
          <div class="guide-steps">
            <div class="guide-step"><b>1</b><span>账号 = API Key + OpenAI / Anthropic 端点</span><button class="btn btn-sm" @click="$emit('go-accounts')">去配置账号</button></div>
            <div class="guide-step"><b>2</b><span>回到服务标签绑定该账号后即可启动</span></div>
          </div>
        </div>

        <!--  骨架屏：首次统计加载未就绪时占位，避免 5s 轮询闪白 -->
        <div v-if="statsLoading && !stats.updatedAt" class="kpi-grid" aria-hidden="true">
          <div v-for="i in 5" :key="i" class="kpi skel">
            <span class="skel-line"></span><b class="skel-bar"></b><span class="skel-line short"></span>
          </div>
        </div>
        <div v-if="statsLoading && !stats.updatedAt" class="card chart-box skel" aria-hidden="true">
          <span class="skel-bar wide"></span>
        </div>

        <!-- KPI 汇总：默认当前小时/今日/本月锚点；历史/自定义区间时切换到所选区间 -->
        <div v-if="!statsLoading || stats.updatedAt" class="kpi-grid" :class="{ 'no-fee': !showCost, historical: isHistoricalRange }">
          <div class="kpi">
            <span class="kpi-l">请求数</span>
            <b class="kpi-v" :title="`请求 ${fmtNum(kpiMain.requests)} 次（今日/区间）`">{{ fmtNum(kpiMain.requests) }}</b>
            <span class="kpi-s" :title="kpiSub.requests">{{ kpiSub.requests }}</span>
          </div>
          <div class="kpi">
            <span class="kpi-l">错误</span>
            <b class="kpi-v" :class="errCls(kpiMain.errors)" :title="`错误 ${fmtNum(kpiMain.errors)} 次（今日/区间）`">{{ fmtNum(kpiMain.errors) }}</b>
            <span class="kpi-s" :title="kpiSub.errors">{{ kpiSub.errors }}</span>
          </div>
          <div class="kpi">
            <span class="kpi-l">Token</span>
            <b class="kpi-v" :title="`Token ${fmtNum(kpiMain.tokens)}（输入+缓存读+输出）`">{{ fmtNum(kpiMain.tokens) }}</b>
            <span class="kpi-s" :title="kpiSub.tokens">{{ kpiSub.tokens }}</span>
          </div>
          <div class="kpi">
            <span class="kpi-l">命中率</span>
            <b class="kpi-v" :class="hitClass(kpiMain.hitRate)" :title="`命中率 ${fmtPct(kpiMain.hitRate)}（缓存读 / 输入+读）`">{{ fmtPct(kpiMain.hitRate) }}</b>
            <span class="kpi-s" :title="kpiSub.hitRate">{{ kpiSub.hitRate }}</span>
          </div>
          <div v-if="showCost" class="kpi">
            <span class="kpi-l">费用</span>
            <b class="kpi-v cost" :title="`费用 ¥${fmtCost(kpiMain.cost)}`">¥{{ fmtCost(kpiMain.cost) }}</b>
            <span class="kpi-s" :title="kpiSub.cost">{{ kpiSub.cost }}</span>
          </div>
        </div>

        <!--  订阅制服务的额度卡（pricing=none 时费用卡隐藏，这里展示额度） -->
        <QuotaCard v-if="quotaVisible && quotaSnapshot" :snapshot="quotaSnapshot" />

        <!-- 性能条：耗时 / 首字 / 速度（近一段时间） -->
        <div class="perf-row">
          <div class="perf-chip" :title="'平均单次耗时（含流式总时长）'">
            <span class="perf-l">耗时</span>
            <b class="perf-v">{{ fmtMs(perf.duration) || "—" }}</b>
          </div>
          <div class="perf-chip" :title="'平均首 token 延迟（流式请求）'">
            <span class="perf-l">首字</span>
            <b class="perf-v">{{ fmtMs(perf.firstToken) || "—" }}</b>
          </div>
          <div class="perf-chip" :title="'平均输出 token 速度'">
            <span class="perf-l">速度</span>
            <b class="perf-v">{{ perf.speed ? fmtSpeed(perf.speed) : "—" }}</b>
          </div>
          <span class="perf-note">{{ isHistoricalRange ? "区间" : "当前" }} · {{ fmtNum(perf.samples) }} 次</span>
        </div>

        <!-- 当前区间为空但存在历史数据：提示用户查看旧数据 -->
        <div v-if="kpiMain.requests === 0 && stats.hasOlderData && !isHistoricalRange" class="hist-hint">
          <Icon name="sparkles" :size="12" />
          <span>今日暂无数据，但历史共有 <b>{{ fmtNum(stats.availableDays?.length) }}</b> 天记录<template v-if="stats.lastDataDate">（最近 {{ stats.lastDataDate }}）</template>。</span>
          <button class="btn btn-sm" @click="calOpen = true">查看历史</button>
        </div>

        <div class="chart-head">
          <div class="chart-left">
            <span class="range-lbl">时间</span>
            <SelectBox
              :model-value="rangeSelectValue"
              :options="rangeSelectOptions"
              size="sm"
              :title="'统计时间区间（' + rangeLabel + '）'"
              @update:model-value="onRangeSelect"
            />
            <button class="cal-btn" :class="{ active: calOpen }" :title="calOpen ? '收起日历' : '自定义日期区间'" @click="calOpen = !calOpen">
              {{ calOpen ? '收起' : '日历' }}<span class="cal-caret">{{ calOpen ? '▲' : '▼' }}</span>
            </button>
            <button v-if="range !== 'today'" class="cal-btn" title="重置为今日" @click="setRange('today')">
              <Icon name="refresh" :size="10" />重置
            </button>
          </div>
          <div class="chart-head-right">
            <SelectBox v-model="modelFilter" :options="filterOptions" size="sm" :title="'按模型过滤整页统计（' + rangeLabel + '）'" />
            <button class="icon-btn" :class="{ spinning: statsLoading }" title="刷新" @click="loadStats()"><Icon name="refresh" :size="12" /></button>
          </div>
        </div>
        <CalendarHeat v-if="calOpen" :service="statsService" :show-cost="showCost" @select="onCalSelect" @quick="onCalQuick" />

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
            <span v-if="showCost" class="model-stats-total">总费用 ¥{{ fmtCost(modelStats.reduce((a, m) => a + (m.cost || 0), 0)) }}</span>
          </div>
          <div class="model-stats-list">
            <div v-for="m in modelStats" :key="m.model" class="ms-row">
              <span class="m" :title="m.model">{{ m.model }}</span>
              <span class="n" :title="`${fmtNum(m.requests)} 次请求`">{{ fmtNum(m.requests) }} 次</span>
              <span class="n" :title="`${fmtNum(m.input + m.read + m.output)} tok（输入+缓存读+输出）`">{{ fmtNum(m.input + m.read + m.output) }} tok</span>
              <span v-if="showCost" class="c" :title="`费用 ¥${fmtCost(m.cost)}`">¥{{ fmtCost(m.cost) }}</span>
            </div>
          </div>
        </div>
        <div v-else class="card">
          <div class="empty-tip">还没有按模型的统计数据，代理跑起来后这里会展示各模型的用量。</div>
        </div>

        <!-- 多服务视图：全部 下按服务汇总（含错误） -->
        <div v-if="selected === ALL && serviceStats.length" class="card svc-stats-card">
          <div class="fc-h"><span>{{ rangeLabel }}按服务</span><span class="model-stats-total">共 {{ fmtNum(serviceTotal) }} 次</span></div>
          <div class="model-stats-list">
            <div v-for="s in serviceStats" :key="s.service" class="ms-row">
              <span class="m svc" :title="s.service">{{ s.service }}</span>
              <span class="n" :title="`${fmtNum(s.requests)} 次请求`">{{ fmtNum(s.requests) }} 次</span>
              <span class="n" :title="`错误 ${fmtNum(s.errors)} 次`">{{ fmtNum(s.errors) }} 错</span>
              <span class="n" :title="`${fmtNum(s.input + s.read + s.output)} tok（输入+缓存读+输出）`">{{ fmtNum(s.input + s.read + s.output) }} tok</span>
              <span v-if="s.avgDurationMs" class="n meta-sm" :title="`平均耗时 ${fmtMs(s.avgDurationMs)}`">{{ fmtMs(s.avgDurationMs) }}</span>
              <span v-if="showCost" class="c" :title="`费用 ¥${fmtCost(s.cost)}`">¥{{ fmtCost(s.cost) }}</span>
            </div>
          </div>
          <p class="hint">「全部」视图下按服务拆分用量与错误，便于多服务排查。</p>
        </div>

        <div v-if="anyRunning" class="card live-card">
          <div class="live-head">
            <span class="live-title"><span class="live-dot"></span>实时调用</span>
            <span>{{ liveSum }}</span>
          </div>
          <Spark :points="liveSpark" :height="40" class="live-spark" />
          <div class="live-feed">
            <div v-for="r in liveFeed" :key="r.key" class="lf-row" :class="{ err: r.isErr }">
              <span class="t">{{ r.time }}</span>
              <span v-if="r.service" class="svc">{{ r.service }}</span>
              <span v-if="r.isErr" class="err-lbl">
                <Icon name="alert" :size="10" />{{ r.err }}
              </span>
              <span v-else class="k" :title="`输入+缓存读+缓存写 ${fmtNum(r.total)} tok · 缓存读 ${fmtNum(r.cacheRead)} · 输出 ${fmtNum(r.output)}`">↑{{ fmtNum(r.total) }} · 读{{ fmtNum(r.cacheRead) }} · ↓{{ fmtNum(r.output) }}</span>
              <span v-if="!r.isErr && r.duration > 0" class="meta" :title="'耗时 ' + r.duration + 'ms'">
                {{ fmtMs(r.duration) }}<span v-if="r.firstToken > 0" :title="'首 token ' + r.firstToken + 'ms'">·首{{ fmtMs(r.firstToken) }}</span>
                <span v-if="r.speed > 0" :title="'输出速度 ' + r.speed + ' tok/s'">·{{ fmtSpeed(r.speed) }}</span>
              </span>
              <span class="h" :class="r.hitCls" :title="`命中率 ${r.hitPctFull}`">{{ r.hitPct }}%</span>
            </div>
          </div>
        </div>

</template>

<script setup lang="ts">
//  统计页视图：从 PanelApp 零行为变更迁出。
// 统计数据与轮询来自 stores/stats；服务运行态来自 stores/services。
import { computed } from "vue";
import { fmtCost, fmtNum, fmtPct } from "../api";
import { hitTier } from "../format";
import { cfg, selected, ALL } from "../stores/config";
import { anyRunning, serviceList } from "../stores/services";
import {
  calOpen,
  chartResetKey,
  liveRecords,
  loadStats,
  modelFilter,
  quotaSnapshot,
  quotaVisible,
  range,
  rangeLabel,
  rangeSelectOptions,
  rangeSelectValue,
  setRange,
  showCost,
  stats,
  statsError,
  statsLoading,
  statsService,
  onCalSelect,
  onCalQuick,
  onRangeSelect,
} from "../stores/stats";
import CalendarHeat from "../components/CalendarHeat.vue";
import Icon from "../components/Icon.vue";
import LineChart from "../components/LineChart.vue";
import QuotaCard from "../components/QuotaCard.vue";
import SelectBox from "../components/SelectBox.vue";
import Spark from "../components/Spark.vue";

defineProps<{ theme: string }>();
defineEmits<{
  (e: "add-service"): void;
  (e: "go-accounts"): void;
}>();

// 首启引导：无服务 / 有服务但无账号
const guide = computed(() => {
  if (!serviceList.value.length) return "services";
  if (!(cfg.accounts || []).length) return "accounts";
  return "";
});

function totalOf(o: any): number {
  return Number(o?.input || 0) + Number(o?.read || 0) + Number(o?.output || 0);
}

// 当前所选区间是否属于历史/自定义（今日之外）。历史/自定义时顶部 KPI 行
// 切到所选区间汇总，让日期筛选的作用范围覆盖整页统计，而非只影响图表。
const isHistoricalRange = computed(
  () => range.value === "custom" || ["yesterday", "week", "lastweek", "month", "lastmonth"].includes(range.value)
);
// 顶部 KPI 主值：默认今日锚点；历史/自定义区间改用所选区间汇总（rangeAgg）
const kpiMain = computed(() => {
  if (!isHistoricalRange.value) {
    return {
      requests: Number(stats.today?.requests || 0),
      errors: Number(stats.today?.errors || 0),
      tokens: totalOf(stats.today),
      hitRate: stats.today?.hitRate || 0,
      cost: Number(stats.today?.cost || 0),
    };
  }
  const ra = stats.rangeAgg || {};
  return {
    requests: Number(ra.requests || 0),
    errors: Number(ra.errors || 0),
    tokens: totalOf(ra),
    hitRate: ra.hitRate || 0,
    cost: Number(ra.cost || 0),
  };
});
// 顶部 KPI 副标签
const kpiSub = computed(() => {
  const fmtN = (v: any) => fmtNum(v || 0);
  if (!isHistoricalRange.value) {
    return {
      requests: `时 ${fmtN(stats.current?.requests)} · 月 ${fmtN(stats.month?.requests)}`,
      errors: `时 ${fmtN(stats.current?.errors)} · 月 ${fmtN(stats.month?.errors)}`,
      tokens: `时 ${fmtN(totalOf(stats.current))} · 月 ${fmtN(totalOf(stats.month))}`,
      hitRate: `时 ${fmtPct(stats.current?.hitRate)} · 月 ${fmtPct(stats.month?.hitRate)}`,
      cost: `时 ¥${fmtCost(stats.current?.cost)} · 月 ¥${fmtCost(stats.month?.cost)}`,
    };
  }
  const pa = stats.prevAgg || {};
  return {
    requests: `${rangeLabel.value} · 较${prevLabel.value} ${fmtN(pa.requests)}`,
    errors: `${rangeLabel.value} 错误 ${fmtN(kpiMain.value.errors)}`,
    tokens: `${rangeLabel.value} · 较${prevLabel.value} ${fmtN(totalOf(pa))}`,
    hitRate: `${rangeLabel.value} · 较${prevLabel.value} ${fmtPct(pa.hitRate)}`,
    cost: `${rangeLabel.value} · 较${prevLabel.value} ¥${fmtCost(pa.cost)}`,
  };
});

// 性能条（平均耗时 / 首字 / 输出速度）：来自当前时段（today）或所选区间（rangeAgg）
const perf = computed(() => {
  const src = isHistoricalRange.value ? stats.rangeAgg || {} : stats.today || {};
  return {
    duration: Number(src.avgDurationMs || 0),
    firstToken: Number(src.avgFirstTokenMs || 0),
    speed: Number(src.avgTokensPerSec || 0),
    samples: Number(src.requests || 0),
  };
});

// 耗时格式化：<1000ms 显示 ms，否则显示 s 并保留一位小数
function fmtMs(ms: number): string {
  if (!ms || isNaN(ms) || ms <= 0) return "";
  if (ms < 1000) return ms.toFixed(0) + "ms";
  return (ms / 1000).toFixed(1) + "s";
}
// 输出速度格式化：tok/s，>=1000 显示 k
function fmtSpeed(s: number): string {
  if (!s || isNaN(s) || s <= 0) return "";
  return s >= 1000 ? (s / 1000).toFixed(1) + "k/s" : s.toFixed(0) + "/s";
}

function hitClass(r: number): string {
  return hitTier(r, false);
}

function errCls(n: number | undefined): string {
  const v = Number(n || 0);
  return v > 0 ? "mid" : "";
}

// 模型下拉选项：用后端返回的区间内模型全集（models 不受过滤影响，
// 保证选中某模型后仍能切换到其他模型）；旧版后端回退到 byModel 推导。
const modelOptions = computed(() => {
  if (Array.isArray(stats.models) && stats.models.length) return stats.models as string[];
  return (stats.byModel || []).map((m: any) => m.model);
});

const filterOptions = computed(() => [
  { value: "", label: "全部模型" },
  ...modelOptions.value.map((m: any) => ({ value: m, label: m })),
]);

const modelStats = computed(() => {
  const arr = stats.byModel || [];
  return (arr as any[]).filter((m: any) => !modelFilter.value || m.model === modelFilter.value);
});

// 多服务“全部”视图：按服务汇总（后端 byService）
const serviceStats = computed(() => stats.byService || []);
const serviceTotal = computed(() =>
  (serviceStats.value as any[]).reduce((a: number, s: any) => a + Number(s.requests || 0), 0)
);

// 所选范围的相关文案与同比
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

// 实时调用同样跟随模型过滤：选中模型后，实时列表 / 迷你走势 / 近5min汇总
// 只统计该模型的请求（记录自带 model 字段，客户端过滤即可）。
const livePool = computed(() =>
  !modelFilter.value
    ? liveRecords.value
    : liveRecords.value.filter((r: any) => r.model === modelFilter.value)
);

const liveSpark = computed(() =>
  livePool.value.slice(-40).map((r: any) => Number(r.cache_hit_rate || 0))
);
const liveFeed = computed(() => {
  // 严格按完整时间戳倒序（最新在前）：时间戳是 0 填充的 ISO 字符串
  // （YYYY-MM-DDTHH:mm:ss），字典序即时间序，跨天/跨引擎都正确；
  // 不用 Date.parse，避免解析差异/NaN 时退化为原数组顺序导致旧记录排前。
  const sorted = [...livePool.value].sort((a, b) => {
    const sa = String(a.timestamp || "");
    const sb = String(b.timestamp || "");
    if (sa === sb) return 0;
    if (!sa) return 1;
    if (!sb) return -1;
    return sa > sb ? -1 : 1;
  });
  return sorted.slice(0, 30).map((r: any) => {
    const rate = Number(r.cache_hit_rate || 0);
    const isErr = !!r.error || r.status === "error";
    return {
      key: `${r.timestamp}_${r.service}_${r.output_tokens}_${r.error || ""}`,
      time: String(r.timestamp || "").slice(11, 19),
      service: r.service || "",
      total:
        Number(r.input_tokens || 0) +
        Number(r.cache_read_tokens || 0) +
        Number(r.cache_write_tokens || 0),
      cacheRead: Number(r.cache_read_tokens || 0),
      output: Number(r.output_tokens || 0),
      hitPct: (rate * 100).toFixed(0),
      hitPctFull: (rate * 100).toFixed(2) + "%",
      hitCls: hitTier(rate, true),
      isErr,
      err: isErr ? String(r.error || r.status || "error") : "",
      duration: Number(r.duration_ms || 0),
      firstToken: Number(r.first_token_ms || 0),
      speed: Number(r.output_tokens_per_sec || 0),
    };
  });
});

// 近 5 分钟汇总（命中率用近 5 分钟 token 加权）
const liveSum = computed(() => {
  const pool = livePool.value;
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


</script>
