# o2a-proxy

Anthropic → OpenAI 协议转换代理：把 Claude Code / Claude Desktop 发出的 Anthropic Messages API 请求转换为 OpenAI 兼容格式，转发给 DashScope、DeepSeek、Kimi 等国内模型服务；同时支持 OpenAI Responses → Chat Completions 的透传（codex 模式，可接 Codex 类客户端）与 Anthropic 原生透传（direct 模式，直连 Anthropic 协议端点）。

## 特性

- **协议转换**：Anthropic Messages API → OpenAI Chat Completions；OpenAI Responses → Chat（codex 模式）；Anthropic 原生透传（direct 模式）
- **流式响应**：完整支持 SSE 流式输出，thinking / tool_calls / usage 逐段透传
- **异步引擎**：asyncio + aiohttp，单进程多服务、连接池复用、客户端断连即取消上游
- **多服务配置**：`config.json` 支持任意数量服务，每服务独立端口、独立启停
- **多账号管理**：账号（API Key + OpenAI/Anthropic 端点）与服务分离，多服务可复用同一账号，账号级统计聚合
- **费用与缓存统计**：JSONL 原始记录 + 小时聚合，命中率 / 覆盖率 / 费用估算
- **桌面客户端**（`desktop/`）：托盘图标 + 右键菜单、悬浮看板、统计图表、配置管理、模型列表联想、深/浅/跟随系统主题、全局快捷键（Ctrl+Alt+O 面板 / Ctrl+Alt+F 悬浮窗），跨平台（Windows / macOS / Linux）
- **显式协议声明**：服务配置 `api` 字段声明入口协议（chat / responses / anthropic），消除自动识别误判

## 架构

```mermaid
flowchart LR
    subgraph Client["客户端"]
        CC["Claude Code / Claude Desktop"]
        CX["Codex 类客户端"]
    end
    subgraph Proxy["o2a-proxy（本机）"]
        DESK["desktop/ 桌面客户端<br/>Tauri 2 + Vue 3"]
        ASYNC["proxy_async.py<br/>asyncio + aiohttp 引擎"]
        CORE["proxy.py<br/>核心库（转换 / 配置 / 统计）"]
        STATS[("cache_stats/<br/>JSONL + 小时聚合")]
    end
    subgraph Upstream["上游模型服务"]
        DS["DashScope / DeepSeek / Kimi 等<br/>OpenAI 兼容 API"]
    end
    CC -- "Anthropic Messages" --> ASYNC
    CX -- "OpenAI Responses" --> ASYNC
    ASYNC -- "协议转换 / 配置 / 统计" --> CORE
    ASYNC -- "Chat Completions" --> DS
    DESK -- "启停 / 统计 / 配置" --> ASYNC
    DESK -- "读取统计文件" --> STATS
    ASYNC -- "写入统计" --> STATS
```

### 组件说明

| 组件 | 说明 |
|---|---|
| `proxy_async.py` | **唯一引擎**：单进程 asyncio 事件循环承载所有服务端口，aiohttp 连接池复用上游连接，流式请求不占线程，客户端断开立即取消 |
| `proxy.py` | **核心库**：协议转换、配置加载、缓存统计与定价等纯函数（线程版引擎已合并删除，保留文件名供桌面端探测与导入） |
| `desktop/` | Tauri 2 + Vue 3 桌面客户端：托盘启停、悬浮看板、统计面板、配置编辑器、模型列表联想 |
| `cache_stats/` | 统计数据：`YYYY-MM-DD.jsonl`（原始记录）+ `summary/<服务>/YYYY-MM-DD.json`（小时聚合） |
| `pricing.json` | 模型定价数据，用于费用估算（支持按账号覆盖，见下文） |
| `auth.json` | API Key 独立存放（对齐 pi 的 auth.json 模式，可选） |
| `cache-stats.py` | 命令行统计查看工具 |

## 安装

### 1. 代理引擎（Python 3.10+）

```bash
pip install -r requirements.txt
cp config.example.json config.json
cp auth.example.json auth.json   # API Key 放这里，config.json 不存 Key
# 编辑 auth.json 填入各账号 API Key；编辑 config.json 填账号端点与服务
python proxy_async.py
```

也可以使用环境变量兜底（无 `config.json` 时，单服务）：

```bash
export DASHSCOPE_API_KEY=sk-xxx
export DASHSCOPE_URL=https://dashscope.aliyuncs.com/compatible-mode/v1/chat/completions
export PROXY_PORT=11011
python proxy_async.py
```

### 2. 桌面客户端（推荐）

前置：Node.js 18+、pnpm、Rust 工具链；Windows 另需 VS Build Tools（C++）与 WebView2（Win10 20H2+ 自带）。

```bash
cd desktop
pnpm install
pnpm tauri dev      # 开发运行（托盘在系统托盘区）
pnpm tauri build    # 打包（Windows NSIS/MSI、macOS dmg）
```

客户端自动定位项目根目录（含 `proxy.py` 的目录），Python 路径可用 `O2A_PYTHON` 环境变量指定。

### 3. 配置 Claude Code

```bash
export ANTHROPIC_BASE_URL=http://127.0.0.1:11011
export ANTHROPIC_AUTH_TOKEN=your-auth-token
```

或写入 `~/.claude/settings.json`：

```json
{
  "env": {
    "ANTHROPIC_BASE_URL": "http://127.0.0.1:11011",
    "ANTHROPIC_AUTH_TOKEN": "your-auth-token"
  }
}
```

## 配置说明

```json
{
  "auth_token": "your-auth-token",
  "cache_stats_enabled": true,
  "cache_stats_dir": "cache_stats",
  "cache_stats_retention_days": 30,
  "accounts": [
    {
      "id": "acc-1",
      "name": "DashScope",
      "openai_url": "https://dashscope.aliyuncs.com/compatible-mode/v1/chat/completions",
      "anthropic_url": ""
    }
  ],
  "services": [
    {
      "comment": "dashscope",
      "account": "acc-1",
      "client": "anthropic",
      "model": "qwen-plus",
      "override_model": true,
      "listen_address": 11011
    }
  ]
}
```

> **Key 默认不写入 `config.json`**：账号 API Key 放在 `auth.json`（见下文）。`accounts[].api_key` 仅作为旧配置兼容保留，新配置不写。

| 字段 | 说明 |
|---|---|
| `auth_token` | 认证令牌（可选，建议生产启用） |
| `cache_stats_enabled` | 是否记录缓存统计 |
| `cache_stats_dir` | 统计目录；**留空默认 `<项目根>/cache_stats`**（应用所在位置的相对目录），显式填写时相对路径基于项目根解析 |
| `cache_stats_retention_days` | 统计保留天数 |
| `accounts[].id` | 账号唯一 id（服务引用它，自动生成不可改） |
| `accounts[].name` | 账号显示名 |
| `accounts[].api_key` | 账号 API Key（**旧配置兼容字段，新配置默认不写**，改放 `auth.json`） |
| `accounts[].openai_url` | OpenAI 兼容端点（**base 或完整 chat/completions 地址均可**：`https://api.deepseek.com`、`…/v1`、`…/chat/completions` 都会归一化为完整 chat/completions 地址） |
| `accounts[].anthropic_url` | Anthropic 兼容端点（如 `https://api.anthropic.com/v1/messages`，可留空） |
| `services[].comment` | 服务备注（客户端里作为服务名） |
| `services[].account` | 引用的账号 id |
| `services[].api` | **入口协议（推荐显式声明）**：`openai-completions`（pi/常规 Chat）/ `openai-responses`（Codex）/ `anthropic-messages`（Claude Code）；未声明时回退 `client`/自动识别（旧配置兼容）。账号级 `accounts[].api` 可作为该账号所有服务的默认值 |
| `services[].upstream_api` | **上游原生协议**（配合 `api=openai-responses`）：`openai-completions`（默认，上游只支持 Chat → 转换）/ `openai-responses`（上游原生支持 Responses，如 DeepSeek 官方 → 整包透传零转换） |
| `services[].client` | 客户端类型（旧字段，`api` 未声明时生效）：`anthropic`（Claude Code）/ `openai`（Codex）/ `auto`（按请求自动识别，默认） |
| `services[].model` | 模型（默认覆盖客户端请求的模型） |
| `services[].override_model` | **模型覆盖开关**（默认 `true`）：`true` 时所有请求一律使用服务配置的 `model`（忽略客户端请求里的模型名）；`false` 时忠实透传客户端请求的模型名（缺省才回退服务配置）。`false` 适合客户端自己选模型（如 opencode / pi / hermes 各自指定主、子 agent 模型）的场景 |
| `services[].listen_address` | 监听端口，每服务独立 |
| `services[].context_1m` | 1M 上下文模式（影响默认 `max_tokens`） |
| `services[].max_tokens` | 最大输出 token（缺省 4096） |
| `services[].thinking_mode` | **思考深度透传模式**（默认 `auto`）：`auto`（按上游 URL/模型自动推断）/ `passthrough`（Anthropic 风格 `thinking` 对象原样透传，DeepSeek V3.2 / Kimi K2 / 兼容网关）/ `effort`（`budget_tokens` → `reasoning_effort` 档位，OpenAI 标准）/ `enable_thinking`（布尔开关，DashScope/Qwen 兼容模式）/ `none`（不透传）。见下文「思考深度透传」 |

> **兼容**：旧格式（`services[].openai_base_url` / `openai_api_key` 内嵌，`mode` 字段）读取时自动迁移为账号结构，无需手动改配置。

### 思考深度透传（thinking_mode）

客户端请求里的"思考深度"参数按入口协议有两种形态：Anthropic 的 `thinking: {type, budget_tokens}`（token 预算）与 OpenAI 的 `reasoning: {effort}` / `reasoning_effort`（low/medium/high 档位）。两者不同构（预算 vs 档位），代理按服务级 `thinking_mode` 映射到上游：

| thinking_mode | 上游收到的参数 | 适用上游 |
|---|---|---|
| `auto`（默认） | 按上游 URL/模型名推断：dashscope/qwen → `enable_thinking`；deepseek/kimi/moonshot → `thinking` 原样（含 `budget_tokens`）；其他 OpenAI 兼容网关 → `reasoning_effort` | 无需配置，覆盖主流 |
| `passthrough` | `thinking: {type, budget_tokens}` 原样透传 | DeepSeek V3.2 / Kimi K2 Thinking / 接受 Anthropic 风格 thinking 的中转网关 |
| `effort` | `reasoning_effort`（`budget_tokens` ≥8192 → high，≥2048 → medium，其余 → low；enabled 无预算时 medium 兜底） | OpenAI 标准档位的端点 |
| `enable_thinking` | `enable_thinking: true/false` | DashScope / Qwen 兼容模式 |
| `none` | 不透传（模型默认行为） | — |

各入口 × 上游组合的处理方式：

| 入口 | 上游 | 思考深度处理 |
|---|---|---|
| `anthropic-messages` | OpenAI Chat | 按 thinking_mode 转换（上表） |
| `anthropic-messages` | Anthropic（direct 透传） | **整包透传**，`thinking` 原样保留 |
| `openai-responses` | OpenAI Chat | `reasoning: {effort}` / `reasoning_effort` → 按 thinking_mode 转换 |
| `openai-responses` | Responses（整包透传） | **原样保留** |
| `openai-completions` | OpenAI Chat | **整包透传**，`reasoning_effort` 等字段原样保留 |

> 思考**内容**（上游 `reasoning_content` → Anthropic `thinking` 块 / Responses `reasoning` item）与推理 token 统计（`reasoning_tokens`）已在响应方向完整透传，不受 `thinking_mode` 影响。

### 入口协议（api 字段，推荐显式声明）

不同客户端使用不同协议：pi 常规用 `openai-completions`（Chat）、Codex 新 CLI 用 `openai-responses`、Claude Code 用 `anthropic-messages`。在服务上显式声明 `api` 后不再做请求体猜测（自动识别在部分客户端下不可靠）：

```json
{
  "services": [
    { "comment": "pi 直连", "account": "acc-3", "api": "openai-completions", "model": "deepseek-v4-flash", "listen_address": 11012 },
    { "comment": "Claude Code", "account": "acc-2", "api": "anthropic-messages", "model": "claude-4", "listen_address": 11013 }
  ]
}
```

| api 值 | 入口 | 行为 |
|---|---|---|
| `openai-completions` | Chat Completions | **整包透传**（请求/响应零转换，直连上游，仅缺 model 时注入服务配置） |
| `openai-responses` + `upstream_api: openai-responses` | Responses | **整包透传**（上游原生支持 Responses，如 DeepSeek 官方 `/v1/responses`，零转换） |
| `openai-responses`（默认 upstream=chat） | Responses | 转 Chat 发送上游，响应转回 Responses 事件 |
| `anthropic-messages` | Anthropic Messages | 账号有 anthropic 端点 → 透传（direct）；只有 openai 端点 → 转换发送（claude） |
| 未声明 | — | 回退旧 `client` 字段；无 client 时按请求自动识别（兼容旧配置） |

> **developer 角色规范化**：Chat/Responses 整包透传前，会把 OpenAI 特有的 `developer` 角色统一降级为 `system`（chat 的 `messages` 与 responses 的 `input` 消息项都会处理）。多数非 OpenAI 上游（DeepSeek / Kimi / Qwen 等）的角色枚举不含 `developer`（DeepSeek 只认 `system / user / assistant / tool` 等），不降级会直接 400（典型报错：`unknown variant developer`）。`system` 是所有上游都接受的通用角色，语义与 OpenAI 换名前的发送方式一致，且不影响 `reasoning_effort` / `thinking` 等字段；未含 developer 角色时请求体保持字节级原样透传。

> **模型列表拿不到协议**：`/models` 接口只返回模型 id，不含协议类型（同一中转账号下三种协议的模型可能并存），所以协议必须在配置中显式声明——这正是 pi 中 `provider.api` 的做法。桌面客户端「服务配置」里也已提供 `api` 下拉。

### Codex 接入（Responses 协议）

Codex CLI 通过 **Responses API** 与模型交互（`wire_api = "responses"`），且 **DeepSeek 官方原生支持 Responses**（`base_url = https://api.deepseek.com`）。两种接入方式：

**A. Codex → DeepSeek 官方（零转换透传，推荐）**：上游原生支持 Responses，配 `upstream_api: "openai-responses"` 直接透传：

```json
{
  "services": [
    { "comment": "codex-ds", "account": "acc-3", "api": "openai-responses", "upstream_api": "openai-responses", "model": "deepseek-v4-flash", "listen_address": 11013 }
  ]
}
```

Codex 侧 `~/.codex/config.toml`（参考 DeepSeek 官方接入文档，仅把 base_url 指向 o2a）：

```toml
model = "deepseek-v4-flash"
model_provider = "o2a-ds"
[model_providers.o2a-ds]
name = "o2a-ds"
base_url = "http://127.0.0.1:11013"
wire_api = "responses"
experimental_bearer_token = "qs-cc"
```

**B. Codex → 中转（仅支持 Chat，如 opencode.ai）**：默认 `upstream_api` 即可，o2a 自动做 Responses→Chat→Responses 转换：

```json
{ "comment": "codex-oc", "account": "acc-2", "api": "openai-responses", "model": "deepseek-v4-flash-free", "listen_address": 11014 }
```

> 同一账号可开多个服务（不同端口）分别服务不同客户端协议：pi 用 `openai-completions`（透传）、Codex 用 `openai-responses`（透传或转换）、Claude Code 用 `anthropic-messages`。

> **兼容**：旧格式（`services[].openai_base_url` / `openai_api_key` 内嵌，`mode` 字段）读取时自动迁移为账号结构，无需手动改配置。

### 密钥独立存放（auth.json，推荐）

对齐 pi 的 `auth.json` 模式：敏感 API Key 默认不写入 `config.json`，统一放 `auth.json`，键可为账号 `id` 或 `name`（值支持 `{type, key}` 或纯字符串），已加入 `.gitignore`：

```json
{
  "acc-1": { "type": "api_key", "key": "sk-your-api-key" },
  "oc-zen": { "type": "api_key", "key": "public" }
}
```

**加载优先级（向后兼容）：** `auth.json`（按 id → name） > `config.json` 的 `accounts[].api_key` > 旧 `services[].openai_api_key` 内嵌。`auth.json` 不存在或账号缺失时无缝回退旧配置，零破坏迁移。桌面客户端保存配置时也会自动把账号 Key 分流写入 `auth.json`，`config.json` 保持不含 Key。

### 模型价格按账号配置（pricing.json 的 accounts 段）

不同账号同一模型价格可能不同（如免费中转 vs 官方付费）。在 `pricing.json` 顶层加 `accounts` 段即可覆盖，键可为账号 `id` 或 `name`，结构与全局模型一致（`tiers[]`，CNY/百万 token）：

```json
{
  "accounts": {
    "acc-2": {
      "models": {
        "deepseek-v4-flash-free": { "tiers": [{ "input": 0, "output": 0 }] }
      }
    },
    "acc-3": {
      "models": {
        "deepseek-v4-flash": { "tiers": [{ "input": 1, "output": 2, "cache_hit": 0.02, "cache_miss": 1 }] }
      }
    }
  },
  "deepseek": { "models": { "deepseek-v4-flash": { "tiers": [{ "input": 1, "output": 2 }] } } }
}
```

**查找优先级：** `pricing.json["accounts"][账号 id/name][模型]` > 全局按模型名（provider 段）。未配置时价格为 0，与旧行为一致。计费同时生效于代理引擎与桌面统计面板（历史记录读取时按当前定价重算）。

### 转换矩阵（client × 账号端点）

| client | 账号只有 OpenAI 端点 | 账号只有 Anthropic 端点 | 双协议端点 |
|---|---|---|---|
| `anthropic`（Claude Code） | Anthropic→OpenAI 转换发送 | Anthropic 原生透传 | 透传 |
| `openai`（Codex） | Responses→Chat 透传 | **报错**（无 OpenAI 端点） | 透传 |
| `auto` | 按请求路径/格式自动识别 | 同左 | 同左 |

## 统计与费用

- **口径**：`缓存命中率 = 缓存读 / (缓存读 + 输入)`，对齐 Anthropic 官方定义
- **记录文件**：`cache_stats/YYYY-MM-DD.jsonl`（每次请求一条）
- **聚合文件**：`cache_stats/summary/<服务>/YYYY-MM-DD.json`（逐小时汇总，含费用）
- **命令行查看**：

```bash
python cache-stats.py day
```

- **桌面客户端**：统计页提供当前小时 / 今日 / 本月对比、逐小时 / 逐日图表、按模型分组统计、实时调用流，代理停止时历史数据仍可查看（直接读文件）。

> 费用为估算值，实际以平台账单为准。

## 支持平台

理论上支持所有 OpenAI 兼容的模型服务（DashScope、DeepSeek、Kimi、OpenAI 等），只需配置 `openai_base_url` 与 `openai_api_key`。

## 开发

```text
o2a-proxy/
├── proxy_async.py          # asyncio 引擎（唯一引擎）
├── proxy.py                # 核心库（转换/配置/统计纯函数）
├── config.example.json     # 配置模板
├── pricing.json            # 模型定价
├── requirements.txt        # Python 依赖
├── cache-stats.py          # 命令行统计
├── desktop/                # Tauri 2 + Vue 3 桌面客户端
│   ├── src-tauri/          # Rust 后端（托盘 / 进程管理 / 统计聚合）
│   ├── src/                # Vue 前端（面板 / 悬浮窗 / 图表）
│   └── scripts/            # 图标生成等脚本
└── cache_stats/            # 统计数据（不提交）
```

测试：

```bash
cd desktop/src-tauri && cargo test   # Rust 单测（统计聚合 / 进程管理 / 模型列表 / key 分流）
python test_cache_stats.py           # 统计逻辑测试
python test_codex_direct.py          # 端到端：Chat 整包透传 / Responses 透传 / Responses→Chat 转换（mock 上游，两引擎）
```

> `test_codex_direct.py` 复用真实 `config.json` 中 ds 服务的账号配置，把上游指向本地 mock 服务器，不产生真实调用。

## 常见问题

### Q: 为什么缓存命中率是 0？

部分模型不支持缓存，或请求内容没有重复。缓存命中率只在有缓存读时才有意义。

### Q: 如何切换模型？

修改 `config.json` 的 `model` / `override_model` 后重启代理；桌面客户端里也可直接编辑并保存。

### Q: 模型列表怎么来的？

客户端按服务 API 地址请求 `…/models` 端点获取，同一地址缓存复用；仅用于输入联想，不影响实际转发。

## 安全说明

- **不要提交 `config.json`**：包含 API Key，已加入 `.gitignore`
- **建议启用 `auth_token`**：生产环境保护代理接口
- **限制访问**：默认只监听 `127.0.0.1`，不要暴露到公网

## 许可证

MIT License
