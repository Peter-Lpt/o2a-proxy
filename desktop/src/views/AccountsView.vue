<template>
  <div>
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
            <button type="button" class="btn btn-primary" @click="$emit('save')">保存配置</button>
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
      <button type="button" class="btn btn-primary" @click="$emit('save')">保存配置</button>
    </div>
    <p class="hint">账号 = 一个 API Key + 最多两个端点（OpenAI / Anthropic，同 Key）。类型自动推导：双端点即双协议，两端点都能服务对应客户端；只有一个端点则另一类客户端走转换（Claude→OpenAI）。</p>
  </div>
</template>

<script setup lang="ts">
//  账号页视图：从 PanelApp 零行为变更迁出。
// 共享状态（cfg / toast / confirm）来自 stores；账号统计（accStats）由父组件
// 心跳轮询后经 props 下发，本组件只读展示。
import { computed, ref } from "vue";
import { api, fmtCost, fmtNum } from "../api";
import { cfg } from "../stores/config";
import { askConfirm, showToast } from "../stores/ui";
import { cacheKey, modelCache } from "../composables/useModels";
import Icon from "../components/Icon.vue";

const props = defineProps<{
  accStats: Record<string, any>;
  accStatsState: Record<string, "loading" | "ok" | "err">;
}>();

defineEmits<{ (e: "save"): void }>();

const editingAcc = ref<string | null>(null);
const showKey = ref(false);
const accHint = ref<{ text: string; err: boolean }>({ text: "", err: false });
let testSeq = 0;
let accTimer: ReturnType<typeof setTimeout> | null = null;

const accountList = computed(() => cfg.accounts || []);

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

const accStatsText = (acc: any) => {
  const st = props.accStats[acc.id];
  const stt = props.accStatsState[acc.id];
  if (stt === "loading") return "统计加载中…";
  if (stt === "err") return "统计读取失败";
  if (!st) return "今日 —";
  const svcN = (cfg.services || []).filter((s: any) => s.account === acc.id).length;
  // 账号下存在按量计费服务才显示费用（订阅制账号只显示请求数）
  const noCost = (p: any) => p === "none" || (typeof p === "object" && (p?.mode === "subscription" || p?.mode === "free"));
  const showFee = (cfg.services || []).some((s: any) => s.account === acc.id && !noCost(s.pricing));
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
  if (accTimer) clearTimeout(accTimer);
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
      //  撤销：仅内存态回滚（未保存不落盘）
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
</script>
