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
      <span v-if="tabEdgeL" class="tab-edge left" aria-hidden="true"></span>
      <span v-if="tabEdgeR" class="tab-edge right" aria-hidden="true"></span>
      <button class="icon-btn" :class="{ active: useListView }" style="flex:none"
              :title="useListView ? '切换为标签栏' : '切换为服务列表（搜索/排序/批量）'"
              @click="toggleListView">
        <Icon :name="useListView ? 'panel' : 'chevron-down'" :size="12" />
      </button>
    </div>

    <!-- §5.2A 服务列表视图：服务 >6 自动启用，或手动切换 -->
    <div v-if="useListView" class="card slv-card">
      <ServiceListView :services="listRows" @open="openServiceFromList" @toggle="toggleSvc"
                       @clone="cloneById" @remove="removeById" @batch-start="batchStart"
                       @batch-stop="batchStop" @batch-remove="batchRemove" @usage="loadListUsage" />
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
      <button class="tab" :class="{ active: page === 'stats' }" @click="goPage('stats')">统计</button>
      <button class="tab" :class="{ active: page === 'config' }" @click="goPage('config')">配置</button>
      <button class="tab" :class="{ active: page === 'accounts' }" @click="goPage('accounts')">账号</button>
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

        <!-- KPI 汇总：默认当前小时/今日/本月锚点；历史/自定义区间时切换到所选区间 -->
        <div class="kpi-grid" :class="{ 'no-fee': !showCost, historical: isHistoricalRange }">
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

        <!-- §8.5 订阅制服务的额度卡（pricing=none 时费用卡隐藏，这里展示额度） -->
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
      </section>

      <!-- 配置 -->
      <section v-show="page === 'config'" class="panel" :class="{ active: page === 'config' }">
        <div v-if="activeSvcRunning" class="cfg-lock">
          <span class="cfg-lock-msg"><Icon name="lock" :size="13" /> 服务「{{ activeSvc.comment }}」运行中，该服务配置已锁定</span>
          <button class="btn btn-sm" @click="api.stopService(activeSvc.id || activeSvc.comment)">停止该代理以编辑</button>
        </div>
        <form @submit.prevent="saveConfig">
          <!-- 全部视图：全局配置 + 概览 -->
          <template v-if="selected === ALL">
            <div class="card form-card">
              <div class="fc-h">全局配置 <span class="fc-sub">作用于所有服务</span></div>
              <label>认证令牌 auth_token <span class="fc-sub" style="font-weight:400">（全局兜底，服务级可覆盖；需重启生效）</span>
                <input v-model="cfg.auth_token" type="text" placeholder="留空 = 不校验（本机任意进程可用）" :disabled="anyRunning" />
              </label>
              <label class="inline"><input v-model="cfg.cache_stats_enabled" type="checkbox" :disabled="anyRunning" /><span>启用缓存统计</span></label>
              <div class="grid2">
                <label>保留天数 <input v-model.number="cfg.cache_stats_retention_days" type="number" min="1" max="365" :disabled="anyRunning" /></label>
                <label>统计目录 <input v-model="cfg.cache_stats_dir" type="text" placeholder="留空使用默认" :disabled="anyRunning" /></label>
              </div>
              <div class="model-hint">留空使用默认目录：{{ status.statsDir || "…" }}</div>
            </div>
            <div class="card form-card">
              <div class="fc-h">配置文件位置 <span class="fc-sub">config.json / auth.json 的读写位置</span></div>
              <label>路径
                <div class="field-row">
                  <input v-model="cfgLocInput" type="text" spellcheck="false" placeholder="绝对路径或目录（目录时取目录下 config.json）" />
                  <button type="button" class="btn btn-sm" @click="browseConfigFile">浏览文件</button>
                  <button type="button" class="btn btn-sm" @click="browseConfigDir">浏览目录</button>
                </div>
              </label>
              <div class="model-hint">
                当前生效：<code class="loc-path">{{ cfgLoc?.config || "…" }}</code>
                <span class="src-tag" :class="cfgLoc?.source">{{ srcLabel }}</span>
              </div>
              <div class="form-actions" style="margin-top:8px">
                <button type="button" class="btn btn-sm btn-primary" @click="saveConfigLocation">应用位置</button>
                <button type="button" class="btn btn-sm" @click="resetConfigLocation">恢复默认</button>
              </div>
              <p class="hint">切换位置会立即加载该位置的配置进行编辑；运行中的服务需重启后按新位置读取。auth.json 默认跟随 config.json 所在目录，可用环境变量 O2A_AUTH 单独指定。</p>
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
                <label>备注 comment
                  <input v-model="draftComment" type="text" :disabled="activeSvcRunning"
                         @change="commitComment" @blur="commitComment" />
                </label>
                <div v-if="commentErr" class="field-err">{{ commentErr }}</div>
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
                <label>思考透传 thinking_mode <span class="fc-sub" style="font-weight:400">（Anthropic thinking / Responses reasoning → 上游）</span>
                  <SelectBox v-model="activeSvc.thinking_mode" :options="thinkingOptions" placeholder="auto（默认）" :disabled="activeSvcRunning" />
                </label>
                <div class="proto-row"><span class="proto-lbl">入口协议</span><span class="proto-val">{{ entryProto }}</span></div>
                <label>主模型 model
                  <div class="field-row">
                    <SelectBox v-model="activeSvc.model" :options="fetchedModels" allow-custom placeholder="选择或输入模型" :disabled="activeSvcRunning" style="flex:1" />
                    <button type="button" class="icon-btn" :class="{ spinning: modelRefreshing }" title="刷新模型列表" @click="refreshModels(true)"><Icon name="refresh" :size="12" /></button>
                  </div>
                </label>
                <label class="inline"><input v-model="activeSvc.override_model" type="checkbox" :disabled="activeSvcRunning" /><span>覆盖客户端模型 override_model <span class="fc-sub" style="font-weight:400">（关：透传客户端请求的模型名）</span></span></label>
                <label>可见模型 models <span class="fc-sub" style="font-weight:400">（对外白名单，留空不限制；需重启生效）</span>
                  <MultiSelect v-model="activeSvcModels" :options="fetchedModels" :locked="activeSvc?.model || ''" :disabled="activeSvcRunning" />
                </label>
                <label>白名单外请求 model_policy
                  <SelectBox v-model="activeSvc.model_policy" :options="policyOptions" :disabled="activeSvcRunning" />
                </label>
                <label>别名映射 models_map <span class="fc-sub" style="font-weight:400">（每行一条：对外名=上游名，统计记对外名）</span>
                  <textarea v-model="modelsMapDraft" rows="2" spellcheck="false" :disabled="activeSvcRunning"
                            placeholder="claude-sonnet-4=deepseek-v4-flash" @change="commitModelsMap"></textarea>
                </label>
                <div class="grid2">
                  <label>监听端口 listen_address <input v-model="activeSvc.listen_address" type="number" min="1" max="65535" :disabled="activeSvcRunning" /></label>
                  <label>监听地址 listen_host <input v-model="activeSvc.listen_host" type="text" placeholder="127.0.0.1" :disabled="activeSvcRunning" /></label>
                </div>
                <label>接入凭证 auth_token <span class="fc-sub" style="font-weight:400">（非空时客户端需带 Authorization: Bearer / x-api-key，需重启生效）</span>
                  <input v-model="activeSvc.auth_token" type="text" placeholder="留空 = 不校验（本机任意进程可用）" :disabled="activeSvcRunning" />
                </label>
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
            <button type="submit" class="btn btn-primary" :class="{ attn: dirty && !activeSvcRunning }" :disabled="activeSvcRunning">保存配置</button>
            <button v-if="selected !== ALL && activeSvc" type="button" class="btn btn-primary"
                    :disabled="!activeSvcRunning" title="停止 → 用新配置重新启动该服务"
                    @click="saveAndRestart">保存并重启</button>
            <button v-if="selected !== ALL && activeSvc" type="button" class="btn" @click="cloneService()"
                    title="复制当前服务配置，自动分配下一个空闲端口">克隆</button>
            <button v-if="selected !== ALL" type="button" class="btn btn-danger" @click="removeSvc()" :disabled="activeSvcRunning || !activeSvc">删除此服务</button>
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
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { api, fmtCost, fmtNum, fmtPct } from "./api";
import { hitTier } from "./format";
import CalendarHeat from "./components/CalendarHeat.vue";
import ConfirmDialog from "./components/ConfirmDialog.vue";
import Icon from "./components/Icon.vue";
import LineChart from "./components/LineChart.vue";
import MultiSelect from "./components/MultiSelect.vue";
import QuotaCard from "./components/QuotaCard.vue";
import SelectBox from "./components/SelectBox.vue";
import ServiceListView, { type ServiceRow } from "./components/ServiceListView.vue";
import Spark from "./components/Spark.vue";
import { applyTheme, getTheme, toggleTheme, watchSystemTheme, type Theme } from "./theme";

const ALL = "__all__";
const cfg = reactive<any>({});
const status = reactive<any>({ services: [] });
const stats = reactive<any>({});
// 当前所选服务集合是否展示费用（订阅制如 opencode token/code plan 不计价）
const showCost = computed(() => stats.showCost !== false);
const liveRecords = ref<any[]>([]);
const selected = ref<string>(ALL);
const selectedSvc = ref<any | null>(null);
// 配置文件位置（UI 设置项）
const cfgLoc = ref<any>(null);
const cfgLocInput = ref("");
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
// 区间档位：下拉可选档 = 主档 + 快捷预设（近7天/近30天）；lastweek/lastmonth
// 仅保留 label 兼容（原 rangeOptions 死代码，§10.2-2 统一取值集合）
const rangeOptions: { value: RangeKey; label: string }[] = [
  { value: "today", label: "今日" },
  { value: "yesterday", label: "昨日" },
  { value: "week", label: "本周" },
  { value: "month", label: "本月" },
];
// 持久化偏好（§10.2-2）：下拉全集 today/yesterday/week/month/7d/30d 都可记忆，
// 重启后按同一语义恢复（7d/30d 重算相对日期）；自定义区间不记忆（原行为）
function readRangePref(): { range: RangeKey; preset: string | null } {
  try {
    const v = localStorage.getItem("o2a-stats-range");
    if (v === "today" || v === "yesterday" || v === "week" || v === "month") {
      return { range: v as RangeKey, preset: null };
    }
    if (v === "7d" || v === "30d") return { range: "custom", preset: v };
  } catch (_) {}
  return { range: "today", preset: null };
}
const _rangePref = readRangePref();
const range = ref<RangeKey>(_rangePref.range);
const presetKey = ref<string | null>(_rangePref.preset);
function persistRangePref() {
  try {
    if (range.value === "custom") {
      if (presetKey.value) localStorage.setItem("o2a-stats-range", presetKey.value);
      else localStorage.removeItem("o2a-stats-range");
    } else {
      localStorage.setItem("o2a-stats-range", range.value);
    }
  } catch (_) {}
}
const calOpen = ref(false);
const customRange = ref<{ start: string; end: string } | null>(null);
// 启动时恢复「近7天/近30天」预设：按当天重算区间（不触发 loadStats，由 onMounted 统一拉取）
if (presetKey.value) {
  const now = new Date();
  const p = (n: number) => String(n).padStart(2, "0");
  const iso = (d: Date) => `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`;
  const days = presetKey.value === "7d" ? 6 : 29;
  customRange.value = { start: iso(new Date(now.getTime() - days * 86400000)), end: iso(now) };
}
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

const modelHint = ref<{ text: string; err: boolean }>({ text: "", err: false });
const modelRefreshing = ref(false);
const MODEL_CACHE_TTL = 10 * 60 * 1000;
interface ModelCacheEntry {
  models: string[];
  fetchedAt: number;
  inflight?: Promise<string[] | null> | null;
}
const modelCache = new Map<string, ModelCacheEntry>();
function cacheKey(baseUrl: string, apiKey: string): string {
  return `${baseUrl}\n${apiKey}`;
}
function fmtModelTime(ts: number): string {
  const d = new Date(ts);
  const p = (n: number) => String(n).padStart(2, "0");
  return `${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`;
}

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
const thinkingOptions = [
  { value: "auto", label: "auto（按上游自动推断）" },
  { value: "passthrough", label: "passthrough（thinking 对象原样透传，DeepSeek / Kimi 类）" },
  { value: "effort", label: "effort（reasoning_effort 档位，OpenAI 标准）" },
  { value: "enable_thinking", label: "enable_thinking（布尔开关，DashScope / Qwen 类）" },
  { value: "none", label: "none（不透传）" },
];
const editingAcc = ref<string | null>(null);
const accHint = ref<{ text: string; err: boolean }>({ text: "", err: false });
const accStats = reactive<Record<string, any>>({});
const accStatsState = reactive<Record<string, "loading" | "ok" | "err">>({});
let modelFetchCount = 0;
let warmSeq = 0;
let testSeq = 0;
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
const accStatsText = (acc: any) => {
  const st = accStats[acc.id];
  const stt = accStatsState[acc.id];
  if (stt === "loading") return "统计加载中…";
  if (stt === "err") return "统计读取失败";
  if (!st) return "今日 —";
  const svcN = (cfg.services || []).filter((s: any) => s.account === acc.id).length;
  // 账号下存在按量计费服务才显示费用（订阅制账号只显示请求数）
  const showFee = (cfg.services || []).some((s: any) => s.account === acc.id && s.pricing !== "none");
  if (!showFee) return `服务×${svcN} · 今日 ${fmtNum(st.today.requests)} 次`;
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
  const seq = ++testSeq;
  setAccHint("正在测试连接…", false);
  try {
    const res = await api.fetchModels(baseUrl, apiKey);
    if (seq !== testSeq) return;
    if (res.ok) {
      modelCache.set(cacheKey(baseUrl, apiKey), { models: res.models, fetchedAt: Date.now() });
      setAccHint(`连接成功：${res.models.length} 个模型可用`, false);
    } else {
      setAccHint("连接失败：" + (res.error || ""), true);
    }
  } catch (e: any) {
    if (seq !== testSeq) return;
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
      const idx = (cfg.accounts || []).indexOf(acc);
      cfg.accounts = cfg.accounts.filter((a: any) => a.id !== acc.id);
      if (editingAcc.value === acc.id) editingAcc.value = null;
      // §10.2-5 撤销：仅内存态回滚（未保存不落盘）
      showToast(`已删除账号「${acc.name || acc.id}」，点击保存配置生效`, "success", {
        label: "撤销",
        fn: () => {
          if (idx >= 0) (cfg.accounts || []).splice(idx, 0, acc);
          else (cfg.accounts || []).push(acc);
          showToast(`已恢复账号「${acc.name || acc.id}」`, "success");
        },
      });
    },
    "删除"
  );
}

function setModelHint(text: string, err = false) {
  modelHint.value = { text, err };
}

async function performFetchModels(baseUrl: string, apiKey: string): Promise<string[] | null> {
  const key = cacheKey(baseUrl, apiKey);
  const existing = modelCache.get(key);
  if (existing?.inflight) return existing.inflight;
  if (!apiKey) return existing?.models ?? null;
  modelFetchCount++;
  modelRefreshing.value = true;
  const entry = existing || { models: [], fetchedAt: 0 };
  const promise = (async () => {
    try {
      const res = await api.fetchModels(baseUrl, apiKey);
      if (res.ok) {
        modelCache.set(key, { models: res.models, fetchedAt: Date.now() });
        return res.models;
      }
      return entry.models || null;
    } catch (e: any) {
      return entry.models || null;
    } finally {
      const cur = modelCache.get(key);
      if (cur) cur.inflight = null;
      modelFetchCount = Math.max(0, modelFetchCount - 1);
      if (modelFetchCount === 0) modelRefreshing.value = false;
    }
  })();
  entry.inflight = promise;
  modelCache.set(key, entry);
  return promise;
}

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

let toastTimer: any = null;
// §10.2-5 Toast 增强：支持动作按钮（如「撤销」）与手动关闭
const toastAction = ref<{ label: string; fn: () => void } | null>(null);
function showToast(msg: string, type: "info" | "success" | "error" = "info",
                   action?: { label: string; fn: () => void }) {
  toast.value = msg;
  toastType.value = type;
  toastAction.value = action || null;
  if (toastTimer) clearTimeout(toastTimer);
  const ttl = action ? 5000 : type === "error" ? 4200 : 2200;
  toastTimer = setTimeout(() => {
    toast.value = "";
    toastAction.value = null;
  }, ttl);
}
function onToastAction() {
  const act = toastAction.value;
  if (toastTimer) clearTimeout(toastTimer);
  toast.value = "";
  toastAction.value = null;
  act?.fn();
}
function dismissToast() {
  if (toastTimer) clearTimeout(toastTimer);
  toast.value = "";
  toastAction.value = null;
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
const serviceList = computed(() =>
  (cfg.services || []).filter((s: any) => s.comment || s === selectedSvc.value)
);
// 运行态：以服务 id 为 key（§2 id 化；status.services 现带 id，旧状态按 name 兜底）
const runningMap = computed<Record<string, boolean>>(() => {
  const m: Record<string, boolean> = {};
  (status.services || []).forEach((s: any) => {
    if (s.id) m[s.id] = !!s.running;
    else m[s.name] = !!s.running;
  });
  return m;
});
// 忙碌态：服务有活跃任务（引擎 /status 的 task.active）
const busyMap = computed<Record<string, boolean>>(() => {
  const m: Record<string, boolean> = {};
  (status.services || []).forEach((s: any) => {
    if (s.id) m[s.id] = !!s.task?.active;
    else m[s.name] = !!s.task?.active;
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
  () => !!activeSvc.value && !!runningMap.value[activeSvc.value.id || activeSvc.value.comment]
);
const activeSvc = computed(() => {
  if (selected.value === ALL) return null;
  const cur = selectedSvc.value;
  if (cur && (cfg.services || []).includes(cur)) return cur;
  // 身份查找：id 优先（稳定身份，改名瞬间列表身份不变），comment 兼容兜底
  const byId = serviceList.value.find((s: any) => s.id === selected.value) || null;
  const byName = byId || serviceList.value.find((s: any) => s.comment === selected.value) || null;
  if (byName) selectedSvc.value = byName;
  return byName;
});
const fetchedModels = computed<string[]>(() => {
  const s = activeSvc.value;
  if (!s) return [];
  const acc = accountById(s.account);
  if (!acc) return [];
  const baseUrl = String(acc.openai_url || "").trim();
  const apiKey = String(acc.api_key || "").trim();
  if (!baseUrl) return [];
  const e = modelCache.get(cacheKey(baseUrl, apiKey));
  return e?.models || [];
});
function selectService(s: any) {
  selectedSvc.value = s;
  selected.value = s.id || s.comment;
}
function selectAll() {
  selectedSvc.value = null;
  selected.value = ALL;
}
const activeSvcAccount = computed(() => accountById(activeSvc.value?.account));

// ---------- §6 模型白名单 / 别名映射（服务级） ----------
const policyOptions = [
  { value: "clamp", label: "clamp（白名单外强转主模型，默认）" },
  { value: "reject", label: "reject（返回 400 并列出可用模型）" },
  { value: "passthrough", label: "passthrough（白名单仅展示，请求照旧）" },
];
const activeSvcModels = computed<string[]>({
  get: () => (activeSvc.value?.models as string[]) || [],
  set: (v) => {
    const s = activeSvc.value;
    if (!s) return;
    s.models = Array.from(new Set(v)).filter(Boolean);
  },
});
// models_map 编辑：textarea 每行「对外名=上游名」，change/失焦/保存时解析写回
const modelsMapDraft = ref("");
watch(activeSvc, (s) => {
  modelsMapDraft.value = Object.entries(s?.models_map || {})
    .map(([k, v]) => `${k}=${v}`)
    .join("\n");
});
function parseModelsMap(text: string): Record<string, string> {
  const out: Record<string, string> = {};
  for (const line of String(text || "").split(/\r?\n/)) {
    const idx = line.indexOf("=");
    if (idx <= 0) continue;
    const k = line.slice(0, idx).trim();
    const v = line.slice(idx + 1).trim();
    if (k && v) out[k] = v;
  }
  return out;
}
function commitModelsMap(): boolean {
  const s = activeSvc.value;
  if (!s) return true;
  const map = parseModelsMap(modelsMapDraft.value);
  if (Object.keys(map).length !== modelsMapDraft.value.split(/\r?\n/).filter((l) => l.trim()).length) {
    showToast("别名映射存在无法解析的行（应为 对外名=上游名），已忽略无效行", "error");
  }
  s.models_map = map;
  modelsMapDraft.value = Object.entries(map).map(([k, v]) => `${k}=${v}`).join("\n");
  return true;
}

// ---------- §5.2D 脏状态 ----------
// 配置快照深比较：有未保存改动时保存按钮高亮、切页有确认弹层
let cfgSnapshot = "";
function snapCfg() {
  cfgSnapshot = JSON.stringify(cfg);
}
const dirty = computed(() => JSON.stringify(cfg) !== cfgSnapshot);
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

// ---------- §5.2A 服务列表视图（>6 服务自动启用，或手动切换） ----------
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
function batchStart(ids: string[]) {
  ids.forEach((id) => api.startService(id).catch((e) => showToast(`启动失败 ${id}: ${e}`, "error")));
  setTimeout(() => loadStatus(), 1500);
}
function batchStop(ids: string[]) {
  ids.forEach((id) => api.stopService(id).catch(() => {}));
  setTimeout(() => loadStatus(), 800);
}
function batchRemove(ids: string[]) {
  askConfirm(
    "批量删除服务",
    `确定删除 ${ids.length} 个服务？\n保存配置后生效。`,
    () => {
      const idSet = new Set(ids);
      const removed = (cfg.services || []).filter((x: any) => idSet.has(x.id || x.comment));
      for (const id of ids) api.stopService(id).catch(() => {});
      cfg.services = cfg.services.filter((x: any) => !idSet.has(x.id || x.comment));
      // §10.2-5 批量撤销：整体恢复（未保存不落盘）
      showToast(`已删除 ${ids.length} 个服务，点击保存生效`, "success", {
        label: "撤销",
        fn: () => {
          for (const s of removed) {
            if (!(cfg.services || []).includes(s)) (cfg.services || []).push(s);
          }
          showToast(`已恢复 ${removed.length} 个服务`, "success");
        },
      });
    },
    "删除"
  );
}
// 今日用量按需拉取（列表打开/手动刷新时一次，不进轮询）
function loadListUsage(ids: string[], done: (u: Record<string, { requests: number; cost: number }>) => void) {
  Promise.all(ids.map((id) => api.getStats(id).catch(() => null))).then((res) => {
    const out: Record<string, { requests: number; cost: number }> = {};
    ids.forEach((id, i) => {
      const r: any = res[i];
      if (r) out[id] = { requests: Number(r.today?.requests || 0), cost: Number(r.today?.cost || 0) };
    });
    done(out);
  });
}

// ---------- comment 改名 draft 提交（§3.3） ----------
// 改名输入绑本地 draft，@change / 失焦 / 保存时才写回；
// 校验失败 → 输入框下方红字，不写回、不 toast（回绑问题已由 id 身份物理消除）
const draftComment = ref("");
const commentErr = ref("");
watch(activeSvc, (s) => {
  draftComment.value = s?.comment || "";
  commentErr.value = "";
});
function validateCommentName(name: string, self: any): string {
  const v = String(name || "").trim();
  if (!v) return "名称不能为空";
  if (/[\/\\:*?"<>|]/.test(v)) return "名称含非法文件名字符（/ \\ : * ? \" < > |）";
  const dup = (cfg.services || []).find((s: any) => s !== self && s.comment === v);
  if (dup) return `与其他服务重名：「${v}」（:${dup.listen_address} · ${dup.model || "?"}）`;
  return "";
}
function commitComment(): boolean {
  const s = activeSvc.value;
  if (!s) return true;
  if (draftComment.value === s.comment) {
    commentErr.value = "";
    return true;
  }
  const err = validateCommentName(draftComment.value, s);
  if (err) {
    commentErr.value = err; // 不写回、不 toast
    return false;
  }
  commentErr.value = "";
  s.comment = draftComment.value.trim();
  return true;
}

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
  selected.value === ALL
    ? ""
    : serviceList.value.find((s: any) => (s.id || s.comment) === selected.value)?.comment || ""
);
const statsService = computed(() => (selected.value === ALL ? "" : selected.value));

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
// ---------- 服务身份 id（§2 id 化） ----------
// svc-<8 位十六进制随机>：稳定身份，生成后终生不变；comment 仅为显示名。
function newSvcId(): string {
  const b = new Uint8Array(4);
  crypto.getRandomValues(b);
  return "svc-" + Array.from(b, (x) => x.toString(16).padStart(2, "0")).join("");
}
function ensureSvcIds(c: any) {
  const used = new Set<string>();
  for (const s of c.services || []) {
    if (s.id && !used.has(s.id)) used.add(s.id);
  }
  for (const s of c.services || []) {
    if (!s.id || used.has(s.id)) {
      let id = newSvcId();
      while (used.has(id)) id = newSvcId();
      s.id = id;
      used.add(id);
    }
  }
}

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

async function loadConfigLocation() {
  try {
    cfgLoc.value = await api.getConfigLocation();
    cfgLocInput.value = cfgLoc.value?.config || "";
  } catch (e: any) {
    cfgLoc.value = null;
    showToast("读取配置位置失败: " + e, "error");
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
      isCustom && c ? c.end : undefined,
      modelFilter.value || undefined
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
  if (quotaVisible.value) loadQuota();
}

// ---------- §8.5 订阅额度展示（pricing=none 的服务：费用卡位置展示额度卡） ----------
const quotaVisible = computed(() => selected.value !== ALL && activeSvc.value?.pricing === "none");
const quotaSnapshot = ref<any>(null);
async function loadQuota() {
  const acc = activeSvc.value?.account;
  if (!acc) return;
  try {
    quotaSnapshot.value = await api.getQuota(acc);
  } catch {
    // 引擎未运行 / 端口不可达：隐藏额度卡，不影响统计页其余渲染（§8.4-3）
    quotaSnapshot.value = null;
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

async function toggleSvc(id: string) {
  if (!(id in runningMap.value)) {
    showToast("该服务尚未保存，请先保存配置", "error");
    return;
  }
  const label = serviceList.value.find((s: any) => (s.id || s.comment) === id)?.comment || id;
  try {
    await api.toggleService(id);
    await loadStatus();
    offError.value = "";
    showToast(label + (runningMap.value[id] ? " 已启动" : " 已停止"), "success");
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
const srcLabel = computed(() => {
  const s = cfgLoc.value?.source;
  return s === "env"
    ? "环境变量 O2A_CONFIG"
    : s === "settings"
      ? "UI 设置"
      : "默认位置";
});

// 浏览选择：具体 config.json 文件 / 配置目录（config.json + auth.json 一起放）
async function browseConfigFile() {
  const sel = await openDialog({
    title: "选择 config.json",
    multiple: false,
    directory: false,
    filters: [{ name: "JSON 配置", extensions: ["json"] }],
  });
  if (typeof sel === "string" && sel) cfgLocInput.value = sel;
}

async function browseConfigDir() {
  const sel = await openDialog({
    title: "选择配置目录（存放 config.json 与 auth.json）",
    multiple: false,
    directory: true,
  });
  if (typeof sel === "string" && sel) cfgLocInput.value = sel;
}

async function saveConfigLocation() {
  const p = cfgLocInput.value.trim();
  if (!p) {
    showToast("请输入配置文件路径", "error");
    return;
  }
  try {
    cfgLoc.value = await api.setConfigLocation(p);
    cfgLocInput.value = cfgLoc.value?.config || p;
    await loadConfig();
    await loadStatus();
    showToast(
      "配置位置已应用" + (anyRunning.value ? "，运行中的服务重启后生效" : ""),
      "success"
    );
  } catch (e: any) {
    showToast("设置失败: " + e, "error");
  }
}

async function resetConfigLocation() {
  try {
    cfgLoc.value = await api.setConfigLocation("");
    cfgLocInput.value = cfgLoc.value?.config || "";
    await loadConfig();
    await loadStatus();
    showToast("已恢复默认位置", "success");
  } catch (e: any) {
    showToast("恢复失败: " + e, "error");
  }
}

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

function setRange(r: RangeKey) {
  range.value = r;
  presetKey.value = null;
  persistRangePref();
  modelFilter.value = "";
  calOpen.value = false;
  loadStats();
}

// 时间区间下拉的当前值（§10.2-1 修复）：主档直接映射；预设（近7天/近30天）保持
// 预设键显示"近 7 天"而非跳成"自定义"；仅日历手选的区间才显示"自定义"
const rangeSelectValue = computed(() => {
  if (presetKey.value) return presetKey.value;
  if (["today", "yesterday", "week", "month"].includes(range.value)) return range.value;
  return "custom";
});

// 时间区间下拉选项（与模型过滤等下拉统一用 SelectBox 组件）；自定义区间时追加动态 label
const rangeSelectOptions = computed(() => {
  const opts = [
    { value: "today", label: "今日" },
    { value: "yesterday", label: "昨日" },
    { value: "week", label: "本周" },
    { value: "month", label: "本月" },
    { value: "7d", label: "近 7 天" },
    { value: "30d", label: "近 30 天" },
  ];
  if (!presetKey.value && range.value === "custom") {
    opts.push({ value: "custom", label: `自定义 ${rangeLabel.value}` });
  }
  return opts;
});

// 时间区间下拉切换（SelectBox 直接传所选 value）
function onRangeSelect(v: string) {
  if (["today", "yesterday", "week", "month"].includes(v)) {
    setRange(v as RangeKey);
  } else if (v === "7d" || v === "30d") {
    presetKey.value = v;
    onQuickRange(v);
  }
  // v === "custom"：当前已是自定义，无需处理
}

// 自定义区间：起止日期（YYYY-MM-DD）；保持日历展开，便于用户看到选中区间并可微调。
// fromPreset：来自"近7天/近30天"快捷（保持预设键以正确显示下拉文案并持久化）
function setCustomRange(start: string, end: string, fromPreset = false) {
  customRange.value = { start, end };
  range.value = "custom";
  if (!fromPreset) presetKey.value = null;
  persistRangePref();
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
    setCustomRange(s, s); // 日历手选性质：清除预设键
    return;
  }
  const days = key === "7d" ? 6 : 29;
  const start = iso(new Date(now.getTime() - days * 86400000));
  setCustomRange(start, iso(now), true);
}

// 图表头部快捷区间：近7天 / 近30天（复用日历快捷逻辑）
function onQuickRange(key: string) {
  onCalQuick(key);
}

watch(selected, (v) => {
  if (v === ALL) {
    selectedSvc.value = null;
  } else if (
    !selectedSvc.value ||
    !(cfg.services || []).includes(selectedSvc.value) ||
    (selectedSvc.value.id || selectedSvc.value.comment) !== v
  ) {
    // id 优先（稳定身份），comment 兼容兜底
    selectedSvc.value =
      (cfg.services || []).find((s: any) => (s.id || s.comment) === v) || selectedSvc.value;
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

onMounted(async () => {
  await Promise.all([loadConfig(), loadStatus(), loadConfigLocation()]);
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
  startHeartbeat();
});
onUnmounted(() => {
  unlistenPanel?.();
  document.removeEventListener("visibilitychange", onVisibilityChange);
  if (heartbeat) clearInterval(heartbeat);
});
</script>
