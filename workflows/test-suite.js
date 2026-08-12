// o2a-proxy 测试套件 workflow
// 用法: workflow({ name: 'test-suite', args: { target? } })
// target: 可选范围（pytest / desktop / rust / stats / all，默认 all）。
export const meta = {
  name: 'test_suite',
  description: 'o2a-proxy 全量测试：pytest 端到端 + 桌面构建 + Rust 检查 + 统计工具，失败定位归因',
  phases: [
    { title: 'Run Tests' },
    { title: 'Deep Dive' },
    { title: 'Report' },
  ],
}

const CTX = [
  'o2a-proxy：Anthropic→OpenAI 协议转换代理（Python + aiohttp），带 Tauri2+Vue3 桌面端。',
  '测试：test_cache_stats.py（单测：记录/命中率公式/零除/汇总/清理）；test_codex_direct.py（端到端：mock 上游 18901 + 真实 aiohttp 引擎 18902，三种协议形态 chat 透传 / responses 透传 / responses→chat 转换）。',
  '构建：cd desktop && pnpm build（vue-tsc --noEmit + vite build）；cd desktop/src-tauri && cargo check。',
  '统计工具：cache-stats.py（读取 cache_stats/ 聚合）。',
  '运行前注意：端到端测试要求 18901/18902 端口空闲；有代理实例在跑需先停（start-proxy.sh 起的实例）。',
].join('\n')

const target = (args && args.target) || 'all'

phase('Run Tests')
const results = await agent(
  'You are the tester for o2a-proxy. Run the test suite for target="' + target + '":\n' +
  '- pytest: python -m pytest test_cache_stats.py test_codex_direct.py -v\n' +
  '- desktop: cd desktop && pnpm build\n' +
  '- rust: cd desktop/src-tauri && cargo check (if toolchain available)\n' +
  '- stats: python cache-stats.py --help (sanity)\n' +
  'For each command report: exact command, exit code, pass/fail, key output. If a test fails, quote the failing ' +
  'assertion/output verbatim. If ports are occupied or dependencies missing, classify as ENVIRONMENT issue, not a test failure.\n\n' +
  '项目背景：\n' + CTX,
  { label: 'run-tests', agentType: 'tester' }
)

phase('Deep Dive')
const deepDive = await parallel([
  () => agent(
    'You are a debugger for o2a-proxy. Based on the test results, investigate any Python test failures: trace the ' +
    'failing assertion to the responsible code path (protocol conversion function, stats formula, engine handler), ' +
    'explain root cause with file:line, and propose a minimal fix. If all Python tests passed, say so and skip.\n\n' +
    '项目背景：\n' + CTX + '\n\nTEST RESULTS:\n' + results,
    { label: 'dive-python', agentType: 'protocol-auditor' }
  ),
  () => agent(
    'You are a debugger for o2a-proxy. Based on the test results, investigate any desktop/build failures: ' +
    'vue-tsc type errors, vite build errors, Rust compile errors — map each to the offending file:line and propose ' +
    'a fix. If all desktop checks passed, say so and skip.\n\n' +
    '项目背景：\n' + CTX + '\n\nTEST RESULTS:\n' + results,
    { label: 'dive-desktop', agentType: 'desktop-engineer' }
  ),
])

phase('Report')
const report = await agent(
  'Write the final test report for o2a-proxy: summary table (item / result / evidence), root causes for failures ' +
  '(from the deep dive), classification (real bug vs environment), and one-line health verdict: ' +
  'ALL-GREEN / FAILURES-DIAGNOSED / FAILURES-UNRESOLVED.\n\n' +
  'TEST RESULTS:\n' + results + '\n\nDEEP DIVE:\n' + JSON.stringify(deepDive),
  { label: 'test-report', agentType: 'reviewer' }
)

return { results, deepDive, report }
