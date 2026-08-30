/**
 * 服务域状态与逻辑（ stores 拆分）。
 *
 * 从 PanelApp 零行为变更迁出：服务列表/选中态/运行态映射/模型缓存读取/
 * comment 草稿与别名映射草稿/下拉选项常量/入口出口提示。
 * PanelApp 与 ServicesView 共享同一份实例。
 */
import { computed, ref, watch } from "vue";
import { cfg, status, selected, selectedSvc, ALL } from "./config";
import { showToast } from "./ui";
import { modelCache } from "../composables/useModels";

// ---------- 列表与运行态 ----------

export const serviceList = computed(() =>
  (cfg.services || []).filter((s: any) => s.comment || s === selectedSvc.value)
);

export const runningMap = computed<Record<string, boolean>>(() => {
  const m: Record<string, boolean> = {};
  (status.services || []).forEach((s: any) => {
    if (s.id) m[s.id] = !!s.running;
    else m[s.name] = !!s.running;
  });
  return m;
});

// 忙碌态：服务有活跃任务（引擎 /status 的 task.active）
export const busyMap = computed<Record<string, boolean>>(() => {
  const m: Record<string, boolean> = {};
  (status.services || []).forEach((s: any) => {
    if (s.id) m[s.id] = !!s.task?.active;
    else m[s.name] = !!s.task?.active;
  });
  return m;
});

export const anyRunning = computed(() => Object.values(runningMap.value).some(Boolean));
export const anyBusy = computed(() => Object.values(busyMap.value).some(Boolean));

// ---------- 选中服务 ----------

export const activeSvcRunning = computed(
  () => !!activeSvc.value && !!runningMap.value[activeSvc.value.id || activeSvc.value.comment]
);

export const activeSvc = computed(() => {
  if (selected.value === ALL) return null;
  const cur = selectedSvc.value;
  if (cur && (cfg.services || []).includes(cur)) return cur;
  // 身份查找：id 优先（稳定身份，改名瞬间列表身份不变），comment 兼容兜底
  const byId = serviceList.value.find((s: any) => s.id === selected.value) || null;
  const byName = byId || serviceList.value.find((s: any) => s.comment === selected.value) || null;
  if (byName) selectedSvc.value = byName;
  return byName;
});

export function accountById(id: string | undefined | null): any {
  return (cfg.accounts || []).find((a: any) => a.id === id);
}

export const activeSvcAccount = computed(() => accountById(activeSvc.value?.account));

// ---------- 账号展示辅助 ----------

export function accKindLabel(acc: any): string {
  const o = !!String(acc?.openai_url || "").trim();
  const a = !!String(acc?.anthropic_url || "").trim();
  if (o && a) return "双协议";
  if (o) return "OpenAI";
  if (a) return "Anthropic";
  return "未配置端点";
}

export function accKindClass(acc: any): string {
  const o = !!String(acc?.openai_url || "").trim();
  const a = !!String(acc?.anthropic_url || "").trim();
  return o && a ? "both" : o ? "openai" : a ? "anthropic" : "none";
}

export const accountOptions = computed(() =>
  (cfg.accounts || []).map((a: any) => ({
    value: a.id,
    label: (a.name || a.id) + " · " + accKindLabel(a),
  }))
);

// ---------- 模型缓存读取 ----------

export const fetchedModels = computed<string[]>(() => {
  const s = activeSvc.value;
  if (!s) return [];
  const acc = accountById(s.account);
  if (!acc) return [];
  const baseUrl = String(acc.openai_url || "").trim();
  const apiKey = String(acc.api_key || "").trim();
  if (!baseUrl) return [];
  return modelCache.get(`${baseUrl}\n${apiKey}`)?.models || [];
});

// ----------  白名单 / 别名 / 策略 ----------

export const policyOptions = [
  { value: "clamp", label: "clamp（白名单外强转主模型，默认）" },
  { value: "reject", label: "reject（返回 400 并列出可用模型）" },
  { value: "passthrough", label: "passthrough（白名单仅展示，请求照旧）" },
];

export const activeSvcModels = computed<string[]>({
  get: () => (activeSvc.value?.models as string[]) || [],
  set: (v) => {
    const s = activeSvc.value;
    if (!s) return;
    s.models = Array.from(new Set(v)).filter(Boolean);
  },
});

export const modelsMapDraft = ref("");
watch(activeSvc, (s) => {
  modelsMapDraft.value = Object.entries(s?.models_map || {})
    .map(([k, v]) => `${k}=${v}`)
    .join("\n");
});

export function parseModelsMap(text: string): Record<string, string> {
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

export function commitModelsMap(): boolean {
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

// ---------- comment 改名草稿（） ----------

export const draftComment = ref("");
export const commentErr = ref("");
watch(activeSvc, (s) => {
  draftComment.value = s?.comment || "";
  commentErr.value = "";
});

export function validateCommentName(name: string, self: any): string {
  const v = String(name || "").trim();
  if (!v) return "名称不能为空";
  if (/[\/\\:*?"<>|]/.test(v)) return "名称含非法文件名字符（/ \\ : * ? \" < > |）";
  const dup = (cfg.services || []).find((s: any) => s !== self && s.comment === v);
  if (dup) return `与其他服务重名：「${v}」（:${dup.listen_address} · ${dup.model || "?"}）`;
  return "";
}

export function commitComment(): boolean {
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

// ---------- 下拉选项常量 ----------

export const clientOptions = [
  { value: "auto", label: "auto（自动识别协议）" },
  { value: "anthropic", label: "anthropic（Claude Code）" },
  { value: "openai", label: "openai（Codex / OpenAI 兼容）" },
];
export const apiOptions = [
  { value: "openai-completions", label: "openai-completions（pi / 常规 Chat，整包透传）" },
  { value: "openai-responses", label: "openai-responses（Codex 专属 Responses）" },
  { value: "anthropic-messages", label: "anthropic-messages（Claude Code）" },
];
export const upstreamApiOptions = [
  { value: "openai-completions", label: "openai-completions（上游只支持 Chat → 自动转换）" },
  { value: "openai-responses", label: "openai-responses（上游原生支持 Responses，如 DeepSeek → 整包透传）" },
];
export const thinkingOptions = [
  { value: "auto", label: "auto（按上游自动推断）" },
  { value: "passthrough", label: "passthrough（thinking 对象原样透传，DeepSeek / Kimi 类）" },
  { value: "effort", label: "effort（reasoning_effort 档位，OpenAI 标准）" },
  { value: "enable_thinking", label: "enable_thinking（布尔开关，DashScope / Qwen 类）" },
  { value: "none", label: "none（不透传）" },
];

// ---------- 入口 / 出口提示 ----------

export const entryProto = computed(() => {
  const api = activeSvc.value?.api || "";
  if (api === "anthropic-messages") return "入口 /v1/messages（Anthropic Messages）";
  if (api === "openai-responses") return "入口 /v1/responses（OpenAI Responses）";
  if (api === "openai-completions") return "入口 /chat/completions（Chat Completions）";
  const c = activeSvc.value?.client || "auto";
  if (c === "anthropic") return "入口 /v1/messages（Anthropic Messages）";
  if (c === "openai") return "入口 /v1/responses · /chat/completions（OpenAI 兼容）";
  return "自动识别：/v1/messages · /v1/responses · /chat/completions";
});

export const outHint = computed(() => {
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
  // auto
  if (a) return `自动识别 → Claude 透传 ${a}；Codex 请求需 OpenAI 端点`;
  return o ? `自动识别 → 出口 ${o}（Claude 转换 / Codex 透传）` : "⚠ 该账号未配置任何端点";
});

// ---------- 悬浮窗目标服务 ----------

export const floatService = computed(() =>
  selected.value === ALL
    ? ""
    : serviceList.value.find((s: any) => (s.id || s.comment) === selected.value)?.comment || ""
);
