# o2a-proxy Workflows 配置

本项目配置了一套**项目级 workflow（pi-dynamic-workflows）**，覆盖评审、审计、排障、开发、发布、测试、协议验证等场景。
脚本源码在 `workflows/*.js`（随仓库版本管理），并通过 `~/.pi/workflows/projects/o2a-proxy-*/saved/` 注册为可复用命令。

## 快速开始

所有 workflow 都可以直接对 pi 说，或在 `/workflows` 界面运行；也可以显式调用：

```text
workflow({ name: 'code-review', args: { diff: '...' } })
workflow({ name: 'bug-diagnosis', args: { bug: '客户端挂起，SSE 流中途中断' } })
```

其中 4 个与内置同名（`code-review` / `codebase-audit` / `adversarial-review` / `multi-perspective`）——
**项目版优先于内置版**，所以在这个仓库里自然语言触发评审/审计时自动走项目版。想临时用回内置版，删掉对应
`saved/<name>.json` 即可（`deep-research` 未覆盖，仍是内置版，因为它依赖内置的联网工具集）。

## 项目 agent（`.pi/agents/*.md`）

workflow 的子代理角色定义，随仓库分发：

| agent | 职责 | 工具 |
|---|---|---|
| `proxy-engineer` | Python 代理核心（proxy.py 核心库 + proxy_async.py 引擎）的实现/重构/修复 | read/grep/find/ls/bash/write/edit |
| `desktop-engineer` | Tauri 2 + Vue 3 桌面端（面板/悬浮窗/图表/Rust 命令层） | read/grep/find/ls/bash/write/edit |
| `protocol-auditor` | 协议转换审计（Anthropic↔Chat↔Responses 请求/SSE/usage 映射） | read/grep/find/ls |
| `stats-auditor` | 统计/缓存/定价口径审计 | read/grep/find/ls/bash |
| `tester` | 跑 pytest 端到端、pnpm build、cargo check 并归因失败 | read/grep/find/ls/bash |
| `reviewer` | 对照项目不变式（协议/异步/统计/配置/桌面契约）评审 | read/grep/find/ls |
| `scout` | 代码侦察，快速定位相关文件/数据流/风险 | read/grep/find/ls |
| `worker` | 按批准方案实现并自验证，不越权决策 | read/grep/find/ls/bash/write/edit |
| `oracle` | 动手前挑战方案假设、指出盲点与更优路径 | read/grep/find/ls |
| `researcher` | 代码内/联网调研（联网工具可用时用，不可用则基于代码证据） | read/grep/find/ls/bash |

## Workflow 清单

### 1. `code-review` —— 多角度并行代码评审（覆盖内置版）

- **args**：`{ diff: string, diffSource?: string }`（diff 需调用方提供，如 `git diff HEAD`）
- **流程**：9 个专项找问题 → 逐条验证（CONFIRMED/PLAUSIBLE/REFUTED）→ 分级报告
- **9 个视角**：A 逐行正确性 / B 删除行为审计 / C 跨文件调用点追踪 / D **协议转换**（本项目 #1 风险区）/ E **异步资源**（断连取消、任务计数、SSE 闭合）/ F **配置统计**（迁移、命中率口径、pricing）/ G 复用 / H 简化 / I 抽象高度
- **输出**：ranked findings（file:line + 失败场景）+ 一句话结论 SAFE / NEEDS FIXES

### 2. `codebase-audit` —— 全仓审计（覆盖内置版）

- **args**：`{ scope?: string, checks?: string[] }`（省略时用项目默认检查集）
- **默认检查集**：协议转换正确性 / 异步引擎资源管理 / 配置加载与迁移 / 统计与定价口径 / 桌面端前后端契约 / 安全与隐私（Key 泄露、认证）/ 测试覆盖与维护性
- **流程**：并行专项检查（severity 标注）→ 交叉验证去伪 → 分级整改报告

### 3. `adversarial-review` —— 对抗式评审（覆盖内置版）

- **args**：`{ task: string, reviewers?: number, threshold?: number }`
- **流程**：调研产出可核验发现 → 每个发现由 N 个怀疑者尝试证伪 → 只保留存活结论 → 共识报告

### 4. `multi-perspective` —— 多视角分析（覆盖内置版）

- **args**：`{ topic: string, perspectives?: string[] }`
- **默认视角**：协议正确性 / 异步引擎可靠性 / 配置与统计一致性 / 桌面端可用性 / 维护性
- **流程**：并行独立分析 → 综合（共识、冲突、行动建议）

### 5. `bug-diagnosis` —— 疑难 bug 诊断（新增）

- **args**：`{ bug: string, focus?: string }`（focus 可给引擎/协议/配置/桌面，跳过无关路径的排查负担）
- **流程**：侦察定位 → **四路假说并行取证**（引擎 / 协议 / 配置统计 / 桌面）→ oracle 挑战主因 → 根因报告（file:line 证据 + 确认步骤 + 修复方案）

### 6. `feature-build` —— 端到端功能开发（新增）

- **args**：`{ feature: string }`
- **流程**：scout 计划 → oracle 挑战方案 → worker 实现（单 writer）→ tester 验证 → reviewer 评审（PASS/FAIL + blockers）→ 修复循环（≤2 轮）→ 交付报告
- **特点**：先挑战再动手；验证命令内置（pytest / pnpm build / cargo check）；不越权决策，需要产品决策时 worker 停下上报

### 7. `release-prep` —— 发布就绪检查（新增）

- **args**：`{ version?: string, focus?: string }`
- **流程**：全量测试（pytest + desktop build + cargo check）→ 并行合规检查（**密钥与 .gitignore** / 配置模板 vs README / 版本一致性与变更记录 / 运行时健壮性）→ 发布清单（PASS/FAIL/AT-RISK + go/no-go）

### 8. `test-suite` —— 全量测试与失败归因（新增）

- **args**：`{ target?: string }`（pytest / desktop / rust / stats / all，默认 all，作为提示传入）
- **流程**：跑全部测试并记录证据 → 并行深挖 Python 失败（转换路径归因）与桌面构建失败（vue-tsc/vite/Rust 错误定位）→ 健康度结论（区分真 bug vs 环境问题）

### 9. `protocol-verify` —— 协议矩阵专项验证（新增）

- **args**：`{ focus?: string, runTests?: boolean }`（focus：anthropic / responses / passthrough；runTests: true 时跑 `test_codex_direct.py` 端到端）
- **流程**：静态审计 Anthropic→Chat / Responses↔Chat / 透传与 direct 三路（对照 README 协议矩阵）→ 可选端到端 → 矩阵报告（每行 VERIFIED/AT-RISK/BROKEN）

## 维护说明

- **改脚本**：编辑 `workflows/*.js`，然后重新注册：
  ```bash
  node -e "..."   # 或用以下命令把 workflows/*.js 同步到 saved registry
  ```
  注册文件位于 `~/.pi/workflows/projects/o2a-proxy-<hash>/saved/<name>.json`，内容为 `{name, description, script}`。
- **脚本约束**（pi-dynamic-workflows 运行时）：首条语句必须是 `export const meta = { name, description, phases }`（字面量）；
  禁止 `Date.now()` / `Math.random()` / `new Date()`（无参）；不能用 import/require/fs；返回普通 JSON。
- **新增 agent**：在 `.pi/agents/*.md` 加 frontmatter（name/description/tools/disallowedTools/model/isolation）+ 正文即角色提示词，随后即可在脚本里 `agent(prompt, { agentType: 'xxx' })`。
- **恢复内置版**：删除 `~/.pi/workflows/projects/o2a-proxy-<hash>/saved/<name>.json` 即回退到内置同名 workflow。
- 查看运行与历史：`workflow_control list` / `/workflows`。
