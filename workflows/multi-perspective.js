// o2a-proxy 项目定制 multi-perspective（覆盖内置版）
// 用法: workflow({ name: 'multi-perspective', args: { topic, perspectives? } })
// perspectives 省略时用项目默认视角（协议正确性/引擎可靠性/配置兼容/桌面可用性/维护性）。
export const meta = {
  name: 'multi_perspective_analysis',
  description: 'o2a-proxy 多视角并行分析：默认项目视角（协议/引擎/配置/桌面/维护）+ 综合',
  phases: [
    { title: 'Perspective Analysis' },
    { title: 'Synthesis' },
  ],
}

const CTX = [
  'o2a-proxy：Anthropic→OpenAI 协议转换代理（Python + aiohttp），带 Tauri2+Vue3 桌面端（~400px 窄面板）。',
  'proxy.py 纯标准库核心（协议转换纯函数 + 配置 + 统计）；proxy_async.py aiohttp 引擎；',
  'desktop/ Tauri 2 + Vue 3（PanelApp.vue / FloatApp.vue / Canvas 图表 / api.ts / Rust 命令层 stats.rs+proxy.rs）。',
  '入口协议：anthropic-messages / openai-completions / openai-responses；upstream_api 上游协议；Service.mode 推导 claude/codex/direct。',
  '统计：cache_stats/ JSONL + summary 小时聚合；命中率口径 test_cache_stats.py。',
  '测试：test_codex_direct.py 端到端覆盖三种协议形态；pnpm build 类型检查。',
].join('\n')

const DEFAULT_PERSPECTIVES = [
  '协议正确性（protocol correctness）：Anthropic Messages ↔ Chat ↔ Responses 的请求/SSE 流式/usage 字段映射是否完整，客户端兼容性',
  '异步引擎可靠性（async engine reliability）：断连取消、任务计数、事件循环阻塞、SSE 时序与超时兜底',
  '配置与统计一致性（config & stats consistency）：Service.api/upstream_api/mode 推导、旧配置迁移、命中率与费用口径、Python↔Rust 口径一致',
  '桌面端可用性（desktop UX）：窄面板 ~400px 信息架构、统计可读性、启停/配置交互、主题一致性',
  '维护性与扩展性（maintainability）：模块边界、测试覆盖、文档一致性、新协议/新账号接入的改动成本',
]

const topic = (args && args.topic) || ''
const rawPerspectives = (args && args.perspectives) || []
const perspectives = Array.isArray(rawPerspectives) && rawPerspectives.length >= 2 ? rawPerspectives : DEFAULT_PERSPECTIVES

phase('Perspective Analysis')
const analyses = await parallel(perspectives.map((p, i) => () =>
  agent(
    'Analyze this topic from the following perspective, grounded in the o2a-proxy codebase (use read/grep; ' +
    'cite file:line where relevant):\n\n项目背景：\n' + CTX + '\n\nTOPIC: ' + topic + '\n\nPERSPECTIVE: ' + p,
    { label: 'perspective-' + (i + 1), agentType: i % 2 === 0 ? 'reviewer' : 'oracle' }
  )
))

phase('Synthesis')
const synthesis = await agent(
  'Synthesize these independent perspectives on the o2a-proxy topic into a balanced analysis: ' +
  'agreements, conflicts, and the most important combined conclusion. Give concrete recommendations.\n\n' +
  'Analyses: ' + JSON.stringify(analyses) + '\n\nTopic: ' + topic,
  { label: 'synthesizer', agentType: 'reviewer' }
)

return { analyses, synthesis }
