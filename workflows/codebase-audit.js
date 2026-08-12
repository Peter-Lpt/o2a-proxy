// o2a-proxy 项目定制 codebase-audit（覆盖内置版）
// 用法: workflow({ name: 'codebase-audit', args: { scope?, checks? } })
// scope 省略时默认审计整个仓库；checks 省略时用本项目默认检查集。
export const meta = {
  name: 'codebase_audit',
  description: 'o2a-proxy 全仓审计：并行专项检查 + 交叉验证 → 分级整改报告（覆盖内置版，带项目默认检查集）',
  phases: [
    { title: 'Individual Checks' },
    { title: 'Cross-Validation' },
    { title: 'Report' },
  ],
}

const CTX = [
  'o2a-proxy：Anthropic→OpenAI 协议转换代理（Python + aiohttp），带 Tauri2+Vue3 桌面端。',
  'proxy.py 纯标准库核心库（Account/Service 配置、CacheStats 统计、协议转换纯函数 convert_request/_responses_to_chat/_chat_to_responses_json/_ResponsesStreamTranslator）；',
  'proxy_async.py aiohttp asyncio 引擎（handle_claude_stream/non_stream、handle_openai_stream/non_stream、handle_direct_stream/non_stream、handle_passthrough、record_stats、ClientGone、STREAM_TIMEOUT、watchdog）；',
  '入口协议 api 字段：anthropic-messages / openai-completions / openai-responses；upstream_api 声明上游协议；Service.mode 推导 claude/codex/direct。',
  '统计：cache_stats/YYYY-MM-DD.jsonl + summary/<服务>/YYYY-MM-DD.json；命中率口径 test_cache_stats.py。',
  'desktop/：Tauri 2 + Vue 3（PanelApp.vue 主面板 ~400px、FloatApp.vue 悬浮窗、components/ Canvas 图表、api.ts、src-tauri/src/{lib.rs,proxy.rs,stats.rs}）。',
  '测试：test_cache_stats.py、test_codex_direct.py（mock 上游 18901 + 真实引擎 18902，三种协议形态）。',
  '构建：python -m pytest test_cache_stats.py test_codex_direct.py -q；cd desktop && pnpm build；cd desktop/src-tauri && cargo check。',
].join('\n')

const DEFAULT_CHECKS = [
  '协议转换正确性：Anthropic Messages ↔ Chat Completions ↔ OpenAI Responses 请求/SSE 流式/usage 字段映射是否完整一致，是否覆盖三种形态（chat 透传 / responses 透传 / responses→chat 转换）',
  '异步引擎资源管理：ClientGone 断连是否取消上游、_task_begin/_task_end 是否配对、事件循环是否被阻塞、SSE 事件是否成对闭合、STREAM_TIMEOUT 收尾是否兜底',
  '配置加载与迁移：Service.api/upstream_api/client/mode 推导是否与 README 一致、旧配置自动迁移是否破坏新配置、auth.json/config.json 分离是否正确',
  '统计与定价口径：JSONL 记录字段、小时聚合、命中率公式、费用估算系数与 test_cache_stats.py 是否一致，Python 侧与 Rust 侧 stats.rs 是否口径一致',
  '桌面端前后端契约：api.ts ↔ Rust 命令签名 ↔ PanelApp.vue 调用三处一致、窄面板 ~400px 设计约束、pnpm build 类型安全',
  '安全与隐私：API Key 是否可能泄露（日志/错误响应/配置模板）、auth_token 认证是否生效、上游 URL 注入、缓存数据是否含敏感请求体',
  '测试覆盖与可维护性：关键转换路径是否有测试、死代码/重复代码、文档与实现漂移（README 表格 vs 实际行为）',
].join('\n---\n')

const scope = (args && args.scope) || 'the whole repository (proxy.py core, proxy_async.py engine, cache-stats scripts, desktop/ Tauri+Vue app)'
const rawChecks = (args && args.checks) || []
const checks = Array.isArray(rawChecks) && rawChecks.length > 0 ? rawChecks : DEFAULT_CHECKS.split('\n---\n').filter((c) => c.trim())

phase('Individual Checks')
const findings = await parallel(checks.map((check, i) => () =>
  agent(
    'You are an auditor for o2a-proxy. Audit the following concern and produce concrete, evidence-backed findings ' +
    '(file:line + issue + impact + suggested fix). Use read/grep/find tools.\n\n项目背景：\n' + CTX +
    '\n\n审计范围：' + scope + '\n\n检查项：' + check,
    {
      label: 'check-' + (i + 1),
      agentType: i % 2 === 0 ? 'reviewer' : 'protocol-auditor',
      schema: {
        type: 'object',
        properties: {
          findings: {
            type: 'array',
            items: {
              type: 'object',
              properties: {
                severity: { type: 'string', enum: ['critical', 'high', 'medium', 'low'] },
                file: { type: 'string' },
                line: { type: 'number' },
                issue: { type: 'string' },
                impact: { type: 'string' },
                fix: { type: 'string' },
              },
              required: ['severity', 'file', 'issue', 'impact'],
            },
          },
        },
        required: ['findings'],
      },
    }
  )
))

const allFindings = findings
  .filter(Boolean)
  .flatMap((f, i) => ((f && f.findings) || []).map((item) => ({ ...item, check: checks[i] })))

phase('Cross-Validation')
const validated = await agent(
  'You are a validator. Cross-validate these audit findings for o2a-proxy: remove false positives and duplicates, ' +
  'confirm real issues, re-rank by severity (critical/high/medium/low). Output the deduplicated, validated findings.\n\n' +
  'FINDINGS JSON:\n' + JSON.stringify(allFindings),
  { label: 'validator', agentType: 'reviewer' }
)

phase('Report')
const report = await agent(
  'Generate a prioritized audit report for o2a-proxy with actionable recommendations. Group by severity, ' +
  'cite file:line for every issue, mark which findings need immediate action before the next release. ' +
  'If a finding list is empty, say the area is clean.\n\n' +
  'VALIDATED FINDINGS:\n' + validated,
  { label: 'report-writer', agentType: 'reviewer' }
)

return { total: allFindings.length, validated, report }
