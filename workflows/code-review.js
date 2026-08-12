// o2a-proxy 项目定制 code-review（覆盖内置版）
// 用法: workflow({ name: 'code-review', args: { diff, diffSource? } })
// diff 需调用方自己提供（如 git diff HEAD），与内置版一致。
export const meta = {
  name: 'code_review',
  description: 'o2a-proxy 多角度并行代码评审：9 个专项找问题 + 验证 → 分级报告（覆盖内置版，带协议/异步引擎视角）',
  phases: [
    { title: 'Find' },
    { title: 'Verify' },
    { title: 'Report' },
  ],
}

const MAX_DIFF_CHARS = 200000
const rawDiff = (args && args.diff) || ''
const diffSource = (args && args.diffSource) || 'git diff HEAD'
const diffTruncated = rawDiff.length > MAX_DIFF_CHARS
const diff = diffTruncated ? rawDiff.slice(0, MAX_DIFF_CHARS) : rawDiff
if (diffTruncated) {
  log('Diff truncated for review: showing the first ' + MAX_DIFF_CHARS + ' of ' + rawDiff.length + ' characters (' + (rawDiff.length - MAX_DIFF_CHARS) + ' omitted). Findings past the cut are not covered.')
}

const CTX = [
  'o2a-proxy：Anthropic→OpenAI 协议转换代理（Python + aiohttp），带 Tauri2+Vue3 桌面端。',
  '架构：proxy.py 纯标准库核心库（配置模型 Account/Service、CacheStats 统计、协议转换纯函数）；',
  'proxy_async.py aiohttp asyncio 引擎（唯一引擎）；desktop/ 桌面端（Tauri 2 + Vue 3 + Canvas 自绘图表）。',
  '入口协议（Service.api）：anthropic-messages（Claude Code）/ openai-completions（pi 常规）/ openai-responses（Codex）；',
  'Service.mode 推导 claude/codex/direct；upstream_api 声明上游原生协议（responses 则整包透传零转换）。',
  '协议转换是核心风险区：Anthropic SSE ↔ Chat SSE ↔ Responses 事件映射、usage 缓存字段',
  '（cache_creation_input_tokens / cache_read_input_tokens）、tool_use/tool_result 块索引；任何字段错位都会让客户端挂起。',
  '引擎风险区：ClientGone 断连取消、_task_begin/_task_end 计数配对、SSE 事件成对闭合、STREAM_TIMEOUT 收尾、不阻塞事件循环。',
  '统计：cache_stats/YYYY-MM-DD.jsonl + summary/<服务>/YYYY-MM-DD.json 小时聚合；命中率口径见 test_cache_stats.py。',
  '测试：test_cache_stats.py（单测）、test_codex_direct.py（端到端：mock 上游 18901 + 真实引擎 18902，覆盖 chat 透传 / responses 透传 / responses→chat 转换）。',
  '桌面端：desktop/src/PanelApp.vue（主面板 ~400px popover）、FloatApp.vue、components/（Canvas 图表）、api.ts（invoke 封装）、',
  'src-tauri/src/{lib.rs,proxy.rs,stats.rs}（Rust 命令层）；契约改动需 api.ts ↔ Rust ↔ Vue 三处一致。',
].join('\n')

const candidateSchema = {
  type: 'object',
  properties: {
    candidates: {
      type: 'array',
      items: {
        type: 'object',
        properties: {
          file: { type: 'string' },
          line: { type: 'number' },
          summary: { type: 'string' },
          failure_scenario: { type: 'string' },
        },
        required: ['file', 'line', 'summary', 'failure_scenario'],
      },
    },
  },
  required: ['candidates'],
}

const diffBlock = '\n\n<diff source="' + diffSource + '"' + (diffTruncated ? ' truncated="true"' : '') + '>\n' +
  diff + (diffTruncated ? '\n\n[... diff truncated: ' + (rawDiff.length - MAX_DIFF_CHARS) + ' more characters omitted ...]' : '') +
  '\n</diff>\n'
const base = 'Use the read/grep tools to pull in any additional file context you need.\n\n项目背景：\n' + CTX + diffBlock

phase('Find')
const finders = await parallel([
  () => agent(
    'You are a line-by-line correctness scanner. Hunt ONLY for: inverted conditions, off-by-one errors, ' +
    'null/nil dereferences, wrong variable used, swallowed errors. For each candidate name the exact file, ' +
    'line number, a one-line summary, and the concrete failure scenario. Return ONLY issues you can justify ' +
    'with a line in the diff.' + base,
    { label: 'A-line-scan', agentType: 'reviewer', schema: candidateSchema }
  ),
  () => agent(
    'You are a removed-behavior auditor. For every deleted line or block in the diff: name the invariant ' +
    'or contract it enforced, then find where (or prove) that contract is re-established elsewhere. ' +
    'Report only gaps where the invariant is NOT re-established. Pay special attention to removed SSE event ' +
    'emissions, removed usage/stat fields, and removed config fallbacks.' + base,
    { label: 'B-removed-behavior', agentType: 'reviewer', schema: candidateSchema }
  ),
  () => agent(
    'You are a cross-file call-site tracer. For each function/method whose signature or behavior changed ' +
    'in the diff (e.g. Service 构造、convert_request、record_stats、api.ts 的 invoke 封装、Rust 命令签名): ' +
    'grep the codebase for callers, then check whether each call site is still correct after the change. ' +
    'Report only call sites that are now broken or need updating.' + base,
    { label: 'C-cross-file-tracer', agentType: 'reviewer', schema: candidateSchema }
  ),
  () => agent(
    'You are a protocol-conversion auditor (this is the project\'s #1 risk area). Verify every touched conversion path: ' +
    'Anthropic Messages ↔ Chat Completions ↔ OpenAI Responses (request body, SSE stream events, usage cache fields, ' +
    'tool_use/tool_result/tool_call_id mapping, stop_reason mapping, content block index bookkeeping). ' +
    'For each candidate: exact file, line, one-line summary, and the concrete failure scenario (which client hangs / which event ' +
    'is malformed). Also check Service.mode / api / upstream_api derivation if the diff touches config.' + base,
    { label: 'D-protocol', agentType: 'protocol-auditor', schema: candidateSchema }
  ),
  () => agent(
    'You are an async/resource auditor for the aiohttp asyncio engine. Hunt for: client disconnect (ClientGone) not cancelling ' +
    'upstream, task counter (_task_begin/_task_end) leaks, blocking calls on the event loop, SSE streams that can end without ' +
    'closing content blocks or emitting message_stop, STREAM_TIMEOUT paths that leave the client hanging, missing ' +
    'stream_write flush, upstream errors not propagated with correct status codes.' + base,
    { label: 'E-async-resource', agentType: 'reviewer', schema: candidateSchema }
  ),
  () => agent(
    'You are a config/stats integrity auditor. Hunt for: changes that break config loading or legacy migration ' +
    '(load_config / load_auth / _resolve_api_key / accounts vs services structure), stats record or aggregation changes that ' +
    'diverge from test_cache_stats.py formulas (hit rate = cache_read/(input+cache_read) etc.), pricing.json coefficient misuse, ' +
    'stats format changes that desktop stats.rs or cache-stats.py will misread, retention/cleanup edge cases.' + base,
    { label: 'F-config-stats', agentType: 'stats-auditor', schema: candidateSchema }
  ),
  () => agent(
    'You are a reuse finder. Identify new code in the diff that duplicates existing helpers, utilities, ' +
    'or patterns already present in the codebase (e.g. _extract_text, _convert_usage, sse_event, _strip_cache_control, ' +
    'formatters in desktop). Propose the existing symbol that should be used instead.' + base,
    { label: 'G-reuse', schema: candidateSchema }
  ),
  () => agent(
    'You are a simplification finder. Look for: redundant state that could be derived, copy-paste ' +
    'variation that could be a shared function, and dead code introduced by the diff.' + base,
    { label: 'H-simplification', schema: candidateSchema }
  ),
  () => agent(
    'You are an altitude reviewer. Assess whether the change is made at the RIGHT abstraction level. ' +
    'Look for: bandaids on shared infrastructure (e.g. patching in the engine what belongs in the core converter, ' +
    'or compensating in the Vue UI for a stats.rs data problem), fixes in the wrong layer, ' +
    'or the change solving a symptom rather than the cause.' + base,
    { label: 'I-altitude', schema: candidateSchema }
  ),
])

const allRaw = finders.flatMap((r, fi) => {
  const label = ['A', 'B', 'C', 'D', 'E', 'F', 'G', 'H', 'I'][fi]
  return ((r && r.candidates) || []).map((c) => ({ ...c, angle: label }))
})

const seen = new Set()
const allCandidates = allRaw.filter((c) => {
  const key = (c.file || '') + ':' + (c.line || 0) + ':' + (c.summary || '').slice(0, 40)
  if (seen.has(key)) return false
  seen.add(key)
  return true
})

phase('Verify')
const verdicts = allCandidates.length > 0
  ? await parallel(allCandidates.map((c, i) => () =>
      agent(
        'You are a verifier. Determine whether this code review finding is CONFIRMED, PLAUSIBLE, or REFUTED. ' +
        'CONFIRMED = you can trace the exact failure in the diff. PLAUSIBLE = concern is valid but not certain. ' +
        'REFUTED = finding is wrong or already handled.\n\n' +
        'FINDING:\nFile: ' + c.file + '\nLine: ' + c.line + '\nSummary: ' + c.summary + '\n' +
        'Failure scenario: ' + c.failure_scenario + diffBlock,
        {
          label: 'verify-' + (i + 1),
          schema: {
            type: 'object',
            properties: { verdict: { type: 'string', enum: ['CONFIRMED', 'PLAUSIBLE', 'REFUTED'] }, reason: { type: 'string' } },
            required: ['verdict'],
          },
        }
      )
    ))
  : []

const surviving = allCandidates
  .map((c, i) => ({ ...c, verdict: (verdicts[i] && verdicts[i].verdict) || 'PLAUSIBLE', verifyReason: (verdicts[i] && verdicts[i].reason) || '' }))
  .filter((c) => c.verdict !== 'REFUTED')

const rankAngle = (a) => ['A', 'B', 'C', 'D', 'E', 'F'].includes(a) ? 0 : ['G', 'H'].includes(a) ? 1 : 2
surviving.sort((a, b) => rankAngle(a.angle) - rankAngle(b.angle))
const top = surviving.slice(0, 12)

phase('Report')
const report = await agent(
  'You are a senior reviewer for o2a-proxy writing the final report. Below are the verified findings from a ' +
  'multi-angle code review (already ranked). Write a concise markdown report: 1 sentence per finding with file, line, ' +
  'and the failure scenario. Order: correctness/protocol/async/config-stats (A-F) first, then reuse/simplification (G-H), ' +
  'then altitude (I). Note the total found vs shown. Add a one-line verdict: SAFE / NEEDS FIXES.\n\n' +
  'FINDINGS JSON:\n' + JSON.stringify(top, null, 2),
  { label: 'synthesis', agentType: 'reviewer' }
)

return { total: allCandidates.length, surviving: surviving.length, findings: top, report, diffTruncated }
