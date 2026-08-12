// 把 workflows/*.js 同步注册到 pi 的项目 saved workflow registry。
// 用法: node workflows/sync.js [--project-key <key>]
// 默认 project key 由本仓库路径推导（与 pi-dynamic-workflows 的 workflowProjectKey 一致：
// <slug>-<sha256(cwd)[0:12]>）。注册位置: ~/.pi/workflows/projects/<key>/saved/<name>.json
const fs = require('fs');
const path = require('path');
const os = require('os');
const crypto = require('crypto');

const ROOT = path.resolve(__dirname, '..');

const DESCRIPTIONS = {
  'code-review': 'o2a-proxy 多角度并行代码评审：9 个专项找问题（正确性/协议/异步引擎/配置统计/复用/简化/高度）+ 验证 → 分级报告。args: { diff, diffSource? }',
  'codebase-audit': 'o2a-proxy 全仓审计：并行专项检查 + 交叉验证 → 分级整改报告（含项目默认检查集）。args: { scope?, checks? }',
  'adversarial-review': 'o2a-proxy 对抗式评审：调研 → 独立怀疑者交叉质证 → 只保留存活结论。args: { task, reviewers?, threshold? }',
  'multi-perspective': 'o2a-proxy 多视角并行分析（默认项目视角：协议/引擎/配置/桌面/维护）+ 综合。args: { topic, perspectives? }',
  'bug-diagnosis': 'o2a-proxy 疑难 bug 诊断：侦察 → 四路假说取证（引擎/协议/配置/桌面）→ 挑战主因 → 根因报告+修复方案。args: { bug, focus? }',
  'feature-build': 'o2a-proxy 端到端功能开发：计划 → 挑战 → 实现 → 测试验证 → 评审修复循环（≤2 轮）→ 交付报告。args: { feature }',
  'release-prep': 'o2a-proxy 发布就绪检查：全量测试 + 并行合规检查（密钥/配置模板/版本/运行时）→ 发布清单。args: { version?, focus? }',
  'test-suite': 'o2a-proxy 全量测试：pytest 端到端 + 桌面构建 + Rust 检查 + 失败归因。args: { target? }',
  'protocol-verify': 'o2a-proxy 协议矩阵专项验证：Anthropic/Chat/Responses 转换静态审计 + 可选端到端。args: { focus?, runTests? }',
};

function projectKey(cwd) {
  const slug = path.basename(cwd).toLowerCase().replace(/[^a-z0-9._-]+/g, '-').replace(/^-+|-+$/g, '').slice(0, 48) || 'project';
  const hash = crypto.createHash('sha256').update(path.resolve(cwd)).digest('hex').slice(0, 12);
  return `${slug}-${hash}`;
}

const keyArg = process.argv.indexOf('--project-key');
const key = keyArg >= 0 ? process.argv[keyArg + 1] : projectKey(ROOT);
const savedDir = path.join(os.homedir(), '.pi', 'workflows', 'projects', key, 'saved');
fs.mkdirSync(savedDir, { recursive: true });

let count = 0;
for (const file of fs.readdirSync(path.join(ROOT, 'workflows')).filter((f) => f.endsWith('.js'))) {
  const name = file.replace(/\.js$/, '');
  const script = fs.readFileSync(path.join(ROOT, 'workflows', file), 'utf8');
  const record = {
    name,
    description: DESCRIPTIONS[name] || `o2a-proxy workflow: ${name}`,
    script,
    savedAt: new Date().toISOString(),
  };
  fs.writeFileSync(path.join(savedDir, `${name}.json`), JSON.stringify(record, null, 2));
  count++;
  console.log(`registered: ${name}`);
}
console.log(`\n${count} workflows registered → ${savedDir}`);
