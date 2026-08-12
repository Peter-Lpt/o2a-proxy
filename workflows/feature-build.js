// o2a-proxy 功能开发 workflow：计划 → 挑战 → 实现 → 验证 → 评审 → 修复（最多 2 轮）→ 交付报告
// 用法: workflow({ name: 'feature-build', args: { feature } })
// feature: 需求描述。实现阶段只有一个 writer（worker），评审与修复循环在同一工作区进行。
export const meta = {
  name: 'feature_build',
  description: 'o2a-proxy 端到端功能开发：侦察计划 → 挑战 → 实现 → 测试验证 → 评审修复循环 → 交付报告',
  phases: [
    { title: 'Plan' },
    { title: 'Challenge' },
    { title: 'Implement' },
    { title: 'Verify' },
    { title: 'Review & Fix' },
    { title: 'Report' },
  ],
}

const CTX = [
  'o2a-proxy：Anthropic→OpenAI 协议转换代理（Python + aiohttp），带 Tauri2+Vue3 桌面端（~400px 窄面板）。',
  'proxy.py 纯标准库核心库（协议转换纯函数 convert_request/_responses_to_chat/_chat_to_responses_json/_ResponsesStreamTranslator、Account/Service 配置、CacheStats 统计）；',
  'proxy_async.py aiohttp asyncio 引擎（handle_claude_stream/non_stream、handle_openai_stream/non_stream、handle_direct_stream/non_stream、handle_passthrough、record_stats、ClientGone、STREAM_TIMEOUT）；',
  'Service.mode 推导 claude/codex/direct；api 声明入口协议；upstream_api 声明上游协议；override_model 模型覆盖开关。',
  '统计：cache_stats/YYYY-MM-DD.jsonl + summary/<服务>/YYYY-MM-DD.json；命中率口径 test_cache_stats.py。',
  'desktop/：Tauri 2 + Vue 3（PanelApp.vue / FloatApp.vue / components/ Canvas 图表 / api.ts / src-tauri/src/{lib.rs,proxy.rs,stats.rs}）；契约改动需 api.ts ↔ Rust ↔ Vue 三处一致。',
  '验证命令：python -m pytest test_cache_stats.py test_codex_direct.py -q（端到端 mock 端口 18901/18902 不可改）；cd desktop && pnpm build；cd desktop/src-tauri && cargo check。',
].join('\n')

const feature = (args && args.feature) || ''

phase('Plan')
const plan = await agent(
  'You are a scout for o2a-proxy. For the feature below: (1) locate the files and functions to touch ' +
  '(engine / core converter / config / stats / desktop UI / Rust commands / tests); (2) describe the data flow; ' +
  '(3) list the risks specific to this project (protocol matrix, SSE timing, stats format compat, config migration); ' +
  '(4) propose an implementation plan with ordered steps.\n\n' +
  '项目背景：\n' + CTX + '\n\nFEATURE:\n' + feature,
  { label: 'plan', agentType: 'scout' }
)

phase('Challenge')
const challenge = await agent(
  'You are an oracle for o2a-proxy. Challenge the proposed plan for the feature: is the approach right for this ' +
  'codebase? Are there protocol/async/config/desktop contract pitfalls it misses? Is there a smaller or more ' +
  'idiomatic alternative? Does it risk breaking existing behaviors or tests? Give a verdict: proceed / adjust / rethink.\n\n' +
  '项目背景：\n' + CTX + '\n\nFEATURE:\n' + feature + '\n\nPLAN:\n' + plan,
  { label: 'challenge', agentType: 'oracle' }
)

phase('Implement')
const implementation = await agent(
  'You are the implementation engineer for o2a-proxy. Implement the feature following the plan, incorporating the ' +
  'challenge feedback. Edit files (write/edit/bash allowed). Keep changes minimal, in existing style (Chinese comments), ' +
  'and add or update tests where the project test files (test_cache_stats.py / test_codex_direct.py) cover the area. ' +
  'Do NOT change the e2e mock ports 18901/18902. Do NOT guess beyond the plan — if something needs a product decision, ' +
  'stop and report it instead.\n\n' +
  '项目背景：\n' + CTX + '\n\nFEATURE:\n' + feature + '\n\nPLAN:\n' + plan + '\n\nCHALLENGE:\n' + challenge,
  { label: 'implement', agentType: 'worker', timeoutMs: 1800000 }
)

phase('Verify')
const verification = await agent(
  'You are the tester for o2a-proxy. Run the verification commands for the changes (python -m pytest ' +
  'test_cache_stats.py test_codex_direct.py -q; and for desktop changes cd desktop && pnpm build; for Rust ' +
  'cd desktop/src-tauri && cargo check if the toolchain exists). Report: each command, exit code, key output, ' +
  'and pass/fail with evidence. If tests fail, identify which assertions failed and the likely cause (with file:line).\n\n' +
  '项目背景：\n' + CTX + '\n\nFEATURE:\n' + feature + '\n\nCHANGES MADE:\n' + implementation,
  { label: 'verify', agentType: 'tester' }
)

phase('Review & Fix')
const reviewSchema = {
  type: 'object',
  properties: {
    verdict: { type: 'string', enum: ['PASS', 'PASS-WITH-NOTES', 'FAIL'] },
    blockers: { type: 'array', items: { type: 'string' } },
    notes: { type: 'array', items: { type: 'string' } },
  },
  required: ['verdict', 'blockers'],
}

const reviewAndFix = async (round) => {
  const review = await agent(
    'You are a reviewer for o2a-proxy. Review the implemented changes against the feature request and the project ' +
    'invariants (protocol conversion correctness, SSE timing, resource cleanup, stats consistency, config compat, ' +
    'desktop contracts). Cite file:line for every issue. Verdict: PASS (no blockers), PASS-WITH-NOTES (notes only), ' +
    'or FAIL (has blockers). List concrete blockers and notes.\n\n' +
    '项目背景：\n' + CTX + '\n\nFEATURE:\n' + feature + '\n\nIMPLEMENTATION:\n' + implementation +
    '\n\nTEST VERIFICATION:\n' + verification,
    { label: 'review-round-' + (round + 1), agentType: 'reviewer', schema: reviewSchema }
  )
  const verdict = (review && review.verdict) || 'FAIL'
  const blockers = (review && review.blockers) || []
  if (verdict === 'PASS' || verdict === 'PASS-WITH-NOTES' || round >= 2 || blockers.length === 0) {
    return { review, fixes: null }
  }
  const fixes = await agent(
    'You are the implementation engineer. The reviewer listed blockers. Fix exactly these issues with minimal edits, ' +
    'then re-run the relevant tests to confirm. Do not introduce changes beyond the blockers.\n\n' +
    'BLOCKERS:\n' + JSON.stringify(blockers, null, 2) + '\n\nIMPLEMENTATION:\n' + implementation,
    { label: 'fix-round-' + (round + 1), agentType: 'worker', timeoutMs: 1200000 }
  )
  return { review, fixes, fixedBlockers: blockers }
}

const round1 = await reviewAndFix(0)
const round2 = round1.fixes ? await reviewAndFix(1) : null
const finalReview = (round2 && round2.review) || round1.review
const finalImpl = (round2 && round2.fixes) || round1.fixes || implementation

phase('Report')
const report = await agent(
  'Write the final delivery report for the o2a-proxy feature build: what was implemented (files changed), ' +
  'test results, review outcome (blockers fixed / remaining), risks, and next steps. One-line verdict: DONE / DONE-WITH-NOTES / BLOCKED.\n\n' +
  'FEATURE:\n' + feature + '\n\nFINAL IMPLEMENTATION:\n' + finalImpl + '\n\nFINAL REVIEW:\n' +
  JSON.stringify(finalReview),
  { label: 'report', agentType: 'reviewer' }
)

return { plan, challenge, implementation: finalImpl, review: finalReview, report }
