// o2a-proxy bug 诊断 workflow
// 用法: workflow({ name: 'bug-diagnosis', args: { bug, focus? } })
// bug: 症状描述（客户端表现/报错/复现步骤）；focus: 可选重点怀疑方向（引擎/协议/配置/桌面）。
export const meta = {
  name: 'bug_diagnosis',
  description: 'o2a-proxy 疑难 bug 诊断：侦察定位 → 多假说并行取证 → 挑战主因 → 根因报告 + 修复方案',
  phases: [
    { title: 'Recon' },
    { title: 'Hypotheses' },
    { title: 'Challenge' },
    { title: 'Diagnosis' },
  ],
}

const CTX = [
  'o2a-proxy：Anthropic→OpenAI 协议转换代理（Python + aiohttp），带 Tauri2+Vue3 桌面端。',
  'proxy.py 纯标准库核心库（协议转换纯函数、Account/Service 配置、CacheStats 统计、load_config/load_auth）；',
  'proxy_async.py aiohttp 引擎（handle_claude_stream/non_stream、handle_openai_stream/non_stream、handle_direct_stream/non_stream、handle_passthrough、record_stats、ClientGone、_task_begin/_task_end、STREAM_TIMEOUT、watchdog）；',
  'Service.mode 推导 claude/codex/direct；api 字段声明入口协议；upstream_api 声明上游协议。',
  '统计：cache_stats/YYYY-MM-DD.jsonl + summary/ 小时聚合；proxy_*.log 是引擎运行日志（含 [FWD] 转发/错误记录）。',
  'desktop/：Tauri 2 + Vue 3（PanelApp.vue 主面板、FloatApp.vue 悬浮窗、api.ts、src-tauri/src/{lib.rs,proxy.rs,stats.rs}）。',
  '测试：test_cache_stats.py、test_codex_direct.py（mock 上游 18901 + 真实引擎 18902）。',
].join('\n')

const bug = (args && args.bug) || ''
const focus = (args && args.focus) || ''

phase('Recon')
const recon = await agent(
  'You are a scout for o2a-proxy. Locate the code paths that could plausibly relate to this bug report. ' +
  'List: relevant files + key functions, the data flow, and where symptoms would originate. Do NOT diagnose yet.\n\n' +
  '项目背景：\n' + CTX + '\n\nBUG REPORT:\n' + bug + (focus ? '\n\nFOCUS AREA: ' + focus : ''),
  { label: 'recon', agentType: 'scout' }
)

phase('Hypotheses')
const probes = await parallel([
  () => agent(
    'You are a probe agent. Investigate the ENGINE hypothesis (proxy_async.py): could the symptom come from ' +
    'request routing, SSE translation, stream lifecycle, ClientGone handling, task counting, timeouts, or error propagation? ' +
    'Read the actual code, trace the path, and produce concrete evidence for or against this hypothesis, ' +
    'with file:line and a reproduction scenario.\n\n' +
    '项目背景：\n' + CTX + '\n\nBUG REPORT:\n' + bug + '\n\nRECON:\n' + recon,
    { label: 'probe-engine', agentType: 'proxy-engineer' }
  ),
  () => agent(
    'You are a probe agent. Investigate the PROTOCOL hypothesis (proxy.py conversions + SSE event mapping): ' +
    'could the symptom come from Anthropic↔Chat↔Responses request/response/usage field mapping, content block indexing, ' +
    'tool_use/tool_result, stop_reason, or model/override_model handling? Read the code and tests, ' +
    'produce evidence with file:line and a reproduction scenario.\n\n' +
    '项目背景：\n' + CTX + '\n\nBUG REPORT:\n' + bug + '\n\nRECON:\n' + recon,
    { label: 'probe-protocol', agentType: 'protocol-auditor' }
  ),
  () => agent(
    'You are a probe agent. Investigate the CONFIG/STATS hypothesis (Service.mode derivation, config.json/auth.json ' +
    'loading and legacy migration, cache_stats recording/aggregation, pricing): could the symptom come from a misconfigured ' +
    'service, wrong protocol declaration, missing key, or stats divergence? Read the code, produce evidence with file:line.\n\n' +
    '项目背景：\n' + CTX + '\n\nBUG REPORT:\n' + bug + '\n\nRECON:\n' + recon,
    { label: 'probe-config', agentType: 'stats-auditor' }
  ),
  () => agent(
    'You are a probe agent. Investigate the DESKTOP hypothesis (desktop/ Tauri+Vue): could the symptom come from ' +
    'the panel/floating UI, api.ts invoke wrappers, Rust commands (proxy.rs/stats.rs), stats reading, or theme? ' +
    'Read the code, produce evidence with file:line.\n\n' +
    '项目背景：\n' + CTX + '\n\nBUG REPORT:\n' + bug + '\n\nRECON:\n' + recon,
    { label: 'probe-desktop', agentType: 'desktop-engineer' }
  ),
])

phase('Challenge')
const challenge = await agent(
  'You are an oracle for o2a-proxy. Below is a bug report, recon notes, and four hypothesis probes. ' +
  'Challenge the leading hypothesis: is the evidence actually conclusive? What competing explanation fits the ' +
  'same evidence? What critical test or log line would discriminate? Give a pointed verdict on which hypothesis ' +
  'is most credible and what would falsify it.\n\n' +
  '项目背景：\n' + CTX + '\n\nBUG REPORT:\n' + bug + '\n\nRECON:\n' + recon +
  '\n\nPROBES:\n' + JSON.stringify(probes),
  { label: 'challenge', agentType: 'oracle' }
)

phase('Diagnosis')
const diagnosis = await agent(
  'Write the final bug diagnosis for o2a-proxy: ROOT CAUSE (with file:line evidence), WHY it produces the observed ' +
  'symptom, CONFIRMATION steps (tests/log commands to run, e.g. pytest cases or proxy_*.log patterns), ' +
  'FIX PLAN (minimal changes, ordered), and a one-line severity verdict. If the cause is not yet certain, ' +
  'say so and list the discriminating experiment.\n\n' +
  '项目背景：\n' + CTX + '\n\nBUG REPORT:\n' + bug + '\n\nRECON:\n' + recon +
  '\n\nPROBES:\n' + JSON.stringify(probes) + '\n\nCHALLENGE:\n' + challenge,
  { label: 'diagnosis', agentType: 'reviewer' }
)

return { recon, probes, challenge, diagnosis }
