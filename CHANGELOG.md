# Changelog

本项目版本格式遵循 [Semantic Versioning](https://semver.org/lang/zh-CN/)。桌面客户端版本同时记录于
`desktop/package.json`、`desktop/src-tauri/Cargo.toml`、`desktop/src-tauri/tauri.conf.json`（三处保持一致）。

## [0.2.0] - 2026-08-19

目录结构整理 + 代码收拢为 Python 包（行为等价重构，默认统计目录路径有变化）。

### 结构变化

- **Python 逻辑收拢为 `o2a/` 包**：`engine.py`（原 `proxy_async.py`）、`convert.py`（协议转换）、`config.py`（账号/服务/配置）、`stats.py`（缓存统计与计费）、`base.py`（日志/常量/项目根定位）
- **根目录兼容 shim**：`proxy.py` / `proxy_async.py` 改为 re-export 兼容层——桌面端路径探测（`find_root`）、绿色版组装、旧导入方式（`import proxy` / `import proxy_async`）全部不受影响；新增 `python -m o2a` 入口
- **脚本收拢**：`start-proxy.sh` / `cache-stats.sh` / `cache-summary.sh` 移入 `scripts/`
- **测试收拢**：三个 `test_*.py` 移入 `tests/`（新增 `conftest.py` 注入项目根；CI 与 pytest 双路径均可运行）
- **运行时数据分离**：统计默认目录由 `cache_stats/` 改为 `<项目根>/data/cache_stats`；运行日志收拢到 `logs/`（均为运行时生成，已入 .gitignore）
- **.gitignore 修正**：`o2a-proxy.exe` → `o2a-desktop.exe`，新增 `data/`、`logs/`、`.pi-subagents/`、`.pytest_cache/`、`vite-dev.*.log`
- **桌面端适配**：`default_stats_dir` 同步为 `root/data/cache_stats`；绿色版打包（`build-portable.sh`）与 Tauri 资源（`tauri.conf.json`）增加 `o2a/` 包目录

### 行为说明

- 配置加载优先级、统计语义、协议转换逻辑均未变化；`cache_stats_dir` 显式配置时仍以项目根解析相对路径
- 已有历史统计数据可手动迁移：`cache_stats/*` → `data/cache_stats/`

## [0.1.1] - 2026-08-12

协议审计专项修复（30 项问题，含 3 个 P1 级流式终止缺陷，均已在真实引擎上实证复现并修复）。

### 修复（P1 —— 客户端挂起 / usage 错乱）

- **流式终止兜底**：claude 流上游 EOF 无 `[DONE]` 时补发 `message_delta` / `message_stop`（含 thinking / 工具块闭合），并补 `record_stats`；`STREAM_TIMEOUT` 收尾同步补统计与工具块闭合
- **Responses 转换流终止**：`[DONE]` / EOF 时 flush `_ResponsesStreamTranslator._finish()` 补发 done 事件与 `response.completed`（此前无 finish_reason 的流永不结束）
- **completed usage 修复**：`response.completed` 延迟到流结束发射（usage 尾块先到达），usage 不再为 null；`_finish` / `complete` 幂等化

### 修复（P2 —— 映射与语义丢失）

- `tool_choice="any"`（Anthropic 必须调用）→ 单工具绑定 / 多工具 `required`，dict 形式不再静默丢弃
- DeepSeek 顶层 `prompt_cache_hit_tokens` / `prompt_cache_miss_tokens` 计入缓存读，命中率统计不再失真
- Responses 格式 `output_tokens_details.reasoning_tokens` 读取（推理 token 不再恒 0）
- 非 200 上游错误体原样透传（去掉 `[:300]` 截断，429 `Retry-After` 等错误码信息不再丢失）
- `max_tokens` 统一 131072 封顶（`convert_request` 与 `_responses_to_chat` 两条转换路径，避免 1M 上下文默认值触发上游 400）

### 修复（P3/P4 —— 中低危与稳定性）

- 三个流式 handler（passthrough / codex / direct）异常时发流内 `error` event，客户端不再挂起到自身超时
- 流式 `stop_reason` 映射带 `has_tool_calls`；对外 usage 补 `reasoning_tokens` 字段
- thinking 块不再被空/`null` 的 `reasoning_content` 提前关闭（分段思考拆块问题）
- 工具调用块去重开块（网关重复带 id 不再产生错位新块）；无 id 首块参数缓冲，杜绝孤儿空 id 块
- `assemble()` 按交付顺序输出（工具先于文本时 output 数组顺序与 output_index 一致）
- direct 透传遵循 `override_model` / `max_tokens` 默认值契约，并转发客户端 `anthropic-beta` 头
- `instructions` 与 input 中已有 system 消息合并，不再产生双 system
- 空 content 消息跳过（纯 thinking 块不再让部分上游 400）；缺 `tool_use_id` 的 tool_result 跳过
- 与 tool_result 交错的文本块按出现顺序冲刷为 user 消息（不再乱序合并）
- `reasoning_content` → Responses `reasoning` item（流式 / 非流式）
- passthrough 流已开始后异常不再返回新的错误响应头；无 OpenAI 端点的 codex 请求返回友好 400
- Responses 转换失败返回明确错误，不再静默回退为 Chat 结构错位响应；非流式 content 为 list 时转纯文本
- 统计归因使用实际生效模型（`override_model=false` 时不再错记为服务默认模型）

### 测试

- `test_codex_direct.py` 重构为 pytest（anyio 插件）可运行，新增 6 个流式终止形态回归用例（三种上游终止行为 × claude / responses 路径）
- 全量 14 个用例通过（pytest 与 `python test_codex_direct.py` 双路径）

## [0.1.0] - 2026-08-02

初始版本：Anthropic → OpenAI 协议转换代理（Python aiohttp 引擎 + Tauri 2 / Vue 3 桌面客户端）。

- 协议转换：Anthropic Messages → Chat Completions；OpenAI Responses → Chat（codex 模式）；Anthropic 原生透传（direct）
- 流式响应：SSE 逐段透传，thinking / tool_calls / usage 支持
- 多服务多账号配置、费用与缓存统计（JSONL + 小时聚合）、桌面客户端（托盘 / 悬浮窗 / 统计面板 / 配置管理）
