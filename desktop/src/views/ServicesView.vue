<template>
  <div>
    <div v-if="activeSvcRunning" class="cfg-lock">
      <span class="cfg-lock-msg"><Icon name="lock" :size="13" /> 服务「{{ activeSvc.comment }}」运行中，该服务配置已锁定</span>
      <button class="btn btn-sm" @click="api.stopService(activeSvc.id || activeSvc.comment)">停止该代理以编辑</button>
    </div>
    <form @submit.prevent="$emit('save')">
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
            <button type="button" class="btn btn-sm btn-primary" @click="applyLocation">应用位置</button>
            <button type="button" class="btn btn-sm" @click="resetLocation">恢复默认</button>
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
              <button type="button" class="link-btn" @click="page = 'accounts'">管理账号 →</button>
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
                <button type="button" class="icon-btn" :class="{ spinning: modelRefreshing }" title="刷新模型列表" @click="$emit('refresh-models')"><Icon name="refresh" :size="12" /></button>
              </div>
            </label>
            <label class="inline"><input v-model="activeSvc.override_model" type="checkbox" :disabled="activeSvcRunning" /><span>覆盖客户端模型 override_model <span class="fc-sub" style="font-weight:400">（关：透传客户端请求的模型名）</span></span></label>
            <label>可见模型 models <span class="fc-sub" style="font-weight:400">（对外白名单，留空不限制；需重启生效）</span>
              <MultiSelect v-model="activeSvcModels" :options="fetchedModels" :locked="activeSvc?.model || ''" :disabled="activeSvcRunning" />
            </label>
            <label>白名单外请求 model_policy
              <SelectBox v-model="activeSvc.model_policy" :options="policyOptions" placeholder="clamp（默认）" :disabled="activeSvcRunning" />
            </label>
            <label>别名映射 models_map <span class="fc-sub" style="font-weight:400">（统计记对外名，实际转发用上游名）</span>
              <div class="map-editor">
                <textarea v-model="modelsMapDraft" rows="3" spellcheck="false" :disabled="activeSvcRunning"
                          placeholder="claude-sonnet-4=deepseek-v4-flash" @change="commitModelsMap"></textarea>
                <div class="map-editor-tip">每行一条：对外名=上游名，例如 claude-sonnet-4=deepseek-v4-flash</div>
              </div>
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
                @click="$emit('save-and-restart')">保存并重启</button>
        <button v-if="selected !== ALL && activeSvc" type="button" class="btn" @click="$emit('clone')"
                title="复制当前服务配置，自动分配下一个空闲端口">克隆</button>
        <button v-if="selected !== ALL" type="button" class="btn btn-danger" @click="$emit('remove')" :disabled="activeSvcRunning || !activeSvc">删除此服务</button>
        <button type="button" class="btn" @click="api.openConfigFile()">打开 config.json</button>
      </div>
    </form>
  </div>
</template>

<script setup lang="ts">
// §10.1 配置页视图：从 PanelApp 零行为变更迁出。
// 共享状态（cfg / 选中服务 / 运行态 / 模型缓存 / 草稿）来自 stores；
// 保存 / 热重启 / 克隆 / 删除等编排动作经 emit 由 PanelApp 执行。
import { computed, onMounted, ref } from "vue";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { api } from "../api";
import { cfg, page, status, selected, ALL, dirty } from "../stores/config";
import {
  accountOptions,
  accKindClass,
  accKindLabel,
  activeSvc,
  activeSvcAccount,
  activeSvcModels,
  activeSvcRunning,
  anyRunning,
  commitComment,
  commitModelsMap,
  commentErr,
  draftComment,
  entryProto,
  fetchedModels,
  modelsMapDraft,
  outHint,
  policyOptions,
  runningMap,
  serviceList,
  clientOptions,
  apiOptions,
  upstreamApiOptions,
  thinkingOptions,
} from "../stores/services";
import { modelHint, modelRefreshing } from "../composables/useModels";
import Icon from "../components/Icon.vue";
import MultiSelect from "../components/MultiSelect.vue";
import SelectBox from "../components/SelectBox.vue";

const emit = defineEmits<{
  (e: "save"): void;
  (e: "save-and-restart"): void;
  (e: "clone"): void;
  (e: "remove"): void;
  (e: "refresh-models"): void;
  (e: "reload"): void;
}>();

// ---------- 配置文件位置（仅本页 UI 状态） ----------
const cfgLoc = ref<any>(null);
const cfgLocInput = ref("");
const srcLabel = computed(() => {
  const s = cfgLoc.value?.source;
  return s === "env" ? "环境变量 O2A_CONFIG" : s === "settings" ? "UI 设置" : "默认位置";
});

onMounted(async () => {
  try {
    cfgLoc.value = await api.getConfigLocation();
    cfgLocInput.value = cfgLoc.value?.config || "";
  } catch (e) {
    cfgLoc.value = null;
  }
});

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

async function applyLocation() {
  const p = cfgLocInput.value.trim();
  if (!p) return;
  cfgLoc.value = await api.setConfigLocation(p);
  cfgLocInput.value = cfgLoc.value?.config || p;
  emit("reload");
}

async function resetLocation() {
  cfgLoc.value = await api.setConfigLocation("");
  cfgLocInput.value = cfgLoc.value?.config || "";
  emit("reload");
}

// ---------- 概览计数 ----------
const accountList = computed(() => cfg.accounts || []);
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
  const ports = (cfg.services || []).map((s: any) => s.listen_address).filter((p: any) => p);
  if (!ports.length) return "—";
  const n = ports.map(Number);
  return Math.min(...n) === Math.max(...n) ? String(n[0]) : `${Math.min(...n)}–${Math.max(...n)}`;
});
</script>
<style scoped>
.map-editor {
  width: 100%;
}
.map-editor textarea {
  display: block;
  width: 100%;
  box-sizing: border-box;
  background: var(--bg);
  border: 1px solid var(--border-soft);
  border-radius: 8px;
  padding: 8px 10px;
  font-family: var(--font-mono);
  font-size: 11.5px;
  line-height: 1.6;
  color: var(--text);
  outline: none;
  resize: vertical;
  min-height: 64px;
  transition: border-color 0.15s, box-shadow 0.15s;
}
.map-editor textarea:focus {
  border-color: var(--blue);
  box-shadow: 0 0 0 3px rgba(79, 140, 255, 0.15);
}
.map-editor-tip {
  margin-top: 3px;
  font-size: 10.5px;
  color: var(--muted-2);
  line-height: 1.5;
}
</style>
