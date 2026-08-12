// o2a-proxy 发布准备 workflow
// 用法: workflow({ name: 'release-prep', args: { version?, focus? } })
// version: 目标版本号（可选，用于核对版本字段）；focus: 可选额外关注点。
export const meta = {
  name: 'release_prep',
  description: 'o2a-proxy 发布就绪检查：全量测试 + 并行合规检查（配置/密钥/版本/文档）→ 发布清单',
  phases: [
    { title: 'Tests' },
    { title: 'Checks' },
    { title: 'Report' },
  ],
}

const CTX = [
  'o2a-proxy：Anthropic→OpenAI 协议转换代理（Python + aiohttp），带 Tauri2+Vue3 桌面端。',
  '发布产物：Python 引擎（proxy_async.py + proxy.py）+ desktop/（Tauri 打包 NSIS/MSI/dmg）。',
  '验证命令：python -m pytest test_cache_stats.py test_codex_direct.py -q；cd desktop && pnpm build；cd desktop/src-tauri && cargo check。',
  '配置模板：config.example.json / auth.example.json（真实 config.json/auth.json 在 .gitignore 中，不入库）；pricing.json 定价。',
  '统计：cache_stats/ 目录由 .gitignore 忽略；proxy_*.log 由 .gitignore 忽略。',
  'desktop/package.json 含版本字段；Tauri 版本在 desktop/src-tauri/tauri.conf.json。',
].join('\n')

const version = (args && args.version) || ''
const focus = (args && args.focus) || ''

phase('Tests')
const tests = await agent(
  'You are the tester for o2a-proxy. Run the full verification suite and report results with evidence ' +
  '(commands, exit codes, key output):\n' +
  '1. python -m pytest test_cache_stats.py test_codex_direct.py -v\n' +
  '2. cd desktop && pnpm build\n' +
  '3. cd desktop/src-tauri && cargo check (if the Rust toolchain is available)\n' +
  '4. python cache-stats.py --help (sanity)\n' +
  'Report pass/fail per item and quote failing output.\n\n项目背景：\n' + CTX,
  { label: 'tests', agentType: 'tester' }
)

phase('Checks')
const checks = await parallel([
  () => agent(
    'You are a release compliance auditor for o2a-proxy. Check SECRETS & IGNORES: (1) is any real API key ' +
    'committed (search config.json/auth.json/token patterns in the git tree and tracked files)? (2) does .gitignore ' +
    'cover config.json, auth.json, cache_stats/, proxy_*.log, node_modules, target? (3) do config.example.json and ' +
    'auth.example.json contain only placeholders? Report findings with file:line evidence.\n\n项目背景：\n' + CTX,
    { label: 'check-secrets', agentType: 'reviewer' }
  ),
  () => agent(
    'You are a release compliance auditor for o2a-proxy. Check CONFIG TEMPLATE vs DOC: (1) does config.example.json ' +
    'cover every field README documents (accounts/services/auth_token/cache_stats_*; services: comment/account/api/' +
    'upstream_api/client/model/override_model/listen_address/context_1m/max_tokens/proxy)? (2) any field in the example ' +
    'not documented or vice versa? (3) is the api/upstream_api enum set documented (anthropic-messages/openai-completions/' +
    'openai-responses × openai-completions/openai-responses) consistent with proxy.py validation? Report with file:line.\n\n项目背景：\n' + CTX,
    { label: 'check-config', agentType: 'stats-auditor' }
  ),
  () => agent(
    'You are a release compliance auditor for o2a-proxy. Check VERSION & CHANGELOG:' + (version ? ' target version is ' + version + '.' : '') +
    ' (1) find all version fields (desktop/package.json, desktop/src-tauri/tauri.conf.json, any __version__/VERSION in Python) ' +
    'and check they are consistent and match the target version if given; (2) is there a CHANGELOG or release notes file? ' +
    'If missing, note it. (3) check README quick-start still matches current config shape (accounts/services structure). ' +
    'Report with file:line.\n\n项目背景：\n' + CTX,
    { label: 'check-version', agentType: 'reviewer' }
  ),
  () => agent(
    'You are a release compliance auditor for o2a-proxy. Check RUNTIME ROBUSTNESS for release: (1) does proxy_async.py ' +
    'handle missing/empty config gracefully (env-var fallback single service)? (2) is the parent-process watchdog and ' +
    'service startup/stop behavior sane for the desktop app spawning it (desktop/src-tauri/src/proxy.rs)? ' +
    '(3) any hardcoded absolute paths, leftover debug prints, or .log files that would ship? Report with file:line.\n\n项目背景：\n' + CTX,
    { label: 'check-runtime', agentType: 'proxy-engineer' }
  ),
])

phase('Report')
const report = await agent(
  'You are the release manager for o2a-proxy. Produce the final release readiness checklist: for each item give ' +
  'PASS/FAIL/AT-RISK with evidence summary; list any release-blocking issues; give the go/no-go recommendation.\n\n' +
  'TEST RESULTS:\n' + tests + '\n\nCOMPLIANCE CHECKS:\n' + JSON.stringify(checks) + (focus ? '\n\nEXTRA FOCUS: ' + focus : ''),
  { label: 'release-report', agentType: 'reviewer' }
)

return { tests, checks, report }
