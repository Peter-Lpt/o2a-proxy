# o2a-proxy 菜单栏客户端 (macOS)

基于 Electron 的 **macOS 系统托盘应用**，把现有的 `proxy.py`（Anthropic → OpenAI 协议转换代理）完整封装起来：

- 🟢 **系统托盘一键开关**代理（启动 / 停止 `proxy.py` 子进程）
- ⚙️ **配置面板**：`config.json` 中所有字段均可编辑（API 地址、模型、子模型、监听端口、API Key、认证令牌、统计开关与保留天数），以及 **1M 上下文开关**
- 📊 **统计面板**：当前(本小时) / 今日 / 本月 的 token 消耗（输入、缓存读、缓存写、输出）、缓存命中率与覆盖率，以及「缓存命中率曲线」和「Token 消耗」图表（纯 Canvas 绘制，离线可用）

复用 `proxy.py` 已有的 `cache_stats` 统计文件，因此历史数据完全兼容 `cache-stats.py`。

## 目录结构

```
mac/
├── package.json
├── main.js          # 托盘 / 代理进程管理 / IPC
├── preload.js       # 安全的渲染进程桥接
├── stats.js         # Node 版缓存统计聚合（复刻 proxy.py 口径）
├── gen-icon.js      # 生成托盘图标（无需依赖）
├── assets/trayTemplate.png  # 生成的模板图标（macOS 菜单栏单色）
└── renderer/
    ├── index.html
    ├── styles.css
    ├── charts.js     # 纯 Canvas 图表库
    └── app.js        # 面板逻辑
```

## 运行

```bash
cd mac
npm install      # 仅安装 electron
npm start        # 启动菜单栏应用
```

这是一个**纯菜单栏应用**（不占 Dock、无主窗口）。启动后菜单栏右上角会出现一个 ⇄ 图标：
- **左键点击图标** → 在图标正下方弹出/收起小面板（点面板外部或按 Esc 自动收起）
- **右键点击图标** → 快捷菜单：「启动/停止代理」「打开配置文件」「退出」
- 面板头部为**电源开关**，控制当前选中的服务；顶部「全部 / 服务1 / 服务2 …」标签可切换服务，每个服务独立启停、互不影响（运行中的服务带绿色圆点）
- 统计页含「本小时 / 今日 / 本月」对比表 + 今日/本月切换的命中率与 Token 消耗图表，每 5 秒自动刷新；**统计跟随顶部选中的服务**（选「全部」看总体，选单个服务看该服务）
- 退出应用：面板底部「退出应用」或右键菜单「退出」

> 前提：本机已安装 `python3`（系统自带即可，`proxy.py` 仅用标准库），且 `../config.json` 中已填写有效的 `openai_api_key`。

## 配置说明

配置直接读写项目根的 `config.json`，字段与 `proxy.py` 完全一致：

| 字段 | 说明 | UI 位置 |
|------|------|---------|
| `auth_token` | 认证令牌 | 全局 |
| `cache_stats_enabled` | 是否记录统计 | 全局 |
| `cache_stats_retention_days` | 统计保留天数 | 全局 |
| `cache_stats_dir` | 统计目录名 | 全局 |
| `services[].comment` | 服务备注 | 服务卡片 |
| `services[].mode` | 模式：`claude`/`codex` | 服务卡片 |
| `services[].model` | 主模型 | 服务卡片 |
| `services[].sub_model` | 子 agent 模型（仅 claude） | 服务卡片 |
| `services[].listen_address` | 监听端口（claude/codex） | 服务卡片 |
| `services[].openai_base_url` | API 请求地址 | 服务卡片 |
| `services[].openai_api_key` | API Key | 服务卡片 |
| `services[].context_1m` | **1M 上下文开关**（仅 claude） | 服务卡片 |

> 代理为 `services` 列表中每个服务各监听一个端口：`claude` 走 Anthropic 转换、`codex` 走 OpenAI 透传。多服务同时监听各自端口，UI 支持添加/删除多个服务，并在卡片上切换模式。

## 1M 上下文开关

claude 模式服务勾选后，`config.json` 中该服务的 `context_1m` 为 `true`。`proxy.py` 会将其 `max_tokens` 默认值设为 1,000,000（客户端未显式传 `max_tokens` 时生效）。若上游服务需要额外参数（如特定模型名或 query 参数），可在 `proxy.py` 中扩展。

## 统计口径

与 `proxy.py` 保持一致：

- **缓存命中率** `hitRate = cache_read / (cache_read + input)` —— 对齐 Anthropic 官方定义（不含 cache_write）
- **缓存覆盖率** `coverage = cache_read / (cache_read + input + cache_write)`
- 其中 `input` 为「实际输入 tokens」（已扣除缓存读 / 写）

数据来源：`../cache_stats/summary/YYYY-MM-DD.json`（逐小时聚合），与 `cache-stats.py` 读取的是同一份文件。
