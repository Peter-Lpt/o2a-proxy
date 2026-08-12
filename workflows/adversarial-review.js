// o2a-proxy 项目定制 adversarial-review（覆盖内置版）
// 用法: workflow({ name: 'adversarial-review', args: { task, reviewers?, threshold? } })
export const meta = {
  name: 'adversarial_review',
  description: 'o2a-proxy 对抗式评审：调研产出可核验发现 → 独立怀疑者交叉质证 → 只保留存活结论',
  phases: [
    { title: 'Investigate' },
    { title: 'Refute' },
    { title: 'Consensus' },
  ],
}

const CTX = [
  'o2a-proxy：Anthropic→OpenAI 协议转换代理（Python + aiohttp），带 Tauri2+Vue3 桌面端。',
  '核心风险区：协议转换（Anthropic Messages ↔ Chat ↔ Responses 的事件/字段映射，错位会让 Claude Code/Codex 挂起）、',
  'asyncio 引擎资源管理（ClientGone 取消、任务计数、SSE 事件成对闭合、STREAM_TIMEOUT）、统计口径（test_cache_stats.py）、',
  '配置迁移（Service.api/upstream_api/mode 推导）、桌面端前后端契约（api.ts ↔ Rust ↔ Vue 三处一致）。',
].join('\n')

const task = (args && args.task) || ''
const reviewers = (args && args.reviewers) || 2
const threshold = (args && args.threshold) || 0.5

phase('Investigate')
const investigation = await agent(
  'Investigate the following against the o2a-proxy codebase and list concrete, individually-checkable findings. ' +
  'Each finding must be a single checkable claim (not an opinion). Use read/grep tools.\n\n项目背景：\n' + CTX +
  '\n\nTASK:\n' + task,
  {
    label: 'investigate',
    schema: {
      type: 'object',
      properties: { findings: { type: 'array', items: { type: 'string' } } },
      required: ['findings'],
    },
  }
)
const findings = (investigation && investigation.findings) || []

phase('Refute')
const judged = await parallel(findings.map((f, i) => () =>
  parallel(Array.from({ length: reviewers }, (_, r) => () =>
    agent(
      'You are a skeptical reviewer for o2a-proxy. Try to REFUTE this finding for the task below. ' +
      'Default to real=false when uncertain. Investigate with the available tools if needed. ' +
      'Challenge protocol-conversion claims against the actual conversion functions and tests.\n\n' +
      '项目背景：\n' + CTX + '\n\nTASK: ' + task + '\nFINDING: ' + f,
      {
        label: 'refute ' + (i + 1) + '.' + (r + 1),
        schema: {
          type: 'object',
          properties: { real: { type: 'boolean' }, reason: { type: 'string' } },
          required: ['real'],
        },
      }
    )
  )).then((votes) => {
    const valid = votes.filter(Boolean)
    const realCount = valid.filter((v) => v && v.real).length
    const ratio = valid.length ? realCount / valid.length : 0
    return { finding: f, realVotes: realCount, totalVotes: valid.length, survives: ratio >= threshold }
  })
))

const survivors = judged.filter((j) => j && j.survives)

phase('Consensus')
const report = await agent(
  'Write a final adversarial review report for o2a-proxy. Include ONLY the findings that survived cross-checking, ' +
  'each with a short justification and, where applicable, file:line. Note how many were discarded. ' +
  'End with the top 1-3 recommended actions.\n\n' +
  'SURVIVING FINDINGS JSON:\n' + JSON.stringify(survivors),
  { label: 'consensus', agentType: 'reviewer' }
)

return { total: findings.length, survivors, report }
