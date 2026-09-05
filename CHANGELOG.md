# Changelog

本项目版本格式遵循 [Semantic Versioning](https://semver.org/lang/zh-CN/)。桌面客户端版本同时记录于
`desktop/package.json`、`desktop/src-tauri/Cargo.toml`、`desktop/src-tauri/tauri.conf.json`（三处保持一致）。

## [Unreleased]

### 变更

- **桌面端启动即返回（事件驱动）**：`start_service` 不再同步等待验证窗口（旧固定 1.2s），spawn 后立即返回；监视线程识别引擎 stdout 的「代理启动」就绪标记（失败：进程未就绪即退出 → `proxy-start-failed` 事件附日志尾部；运行后退出 → `proxy-stopped` 事件），前端 toast + 刷新状态。`start_all`/autostart 随之从 N×1.2s 串行降为即时返回；启动验证兜底保留端口探测（python 回退引擎不打印标记时可用）。跨平台：BufReader 逐行读、TcpStream 探测、线程模型在 Windows/macOS/Linux 语义一致
- **引擎全量重写为 Rust**：原 Python 引擎（`o2a/` 包，asyncio + aiohttp）整体替换为 `o2a-engine` 二进制（tokio + axum），无 Python 运行时依赖。HTTP 契约、协议转换语义、JSONL 统计格式、config/auth 双向兼容性逐项对齐；定价与桌面端共用同一实现（golden 对齐）。详见 `docs/rust-rewrite.md`。
- **启动性能**：单服务冷启动从 ~1.2s 降至 ~0.1s（二进制直启，无解释器/导入开销）
- **移除 Python 实现**：`o2a/`、`proxy.py`、`proxy_async.py`、`cache-stats.py`、`tests/`（pytest）、`requirements.txt` 及相关脚本；CI 的 python-tests job 改为 `cargo test --workspace` + clippy
- **桌面端**：优先 spawn `o2a-engine` 二进制（`O2A_ENGINE` > 项目根 > 贴身目录 > `target/release/`），不再依赖系统 Python；找不到二进制时保留旧 Python 引擎回退（过渡期）

### 修复

- **桌面端退出后代理端口残留**：引擎 watchdog 原先在启动瞬间快照 `getppid()`，若桌面端在引擎
  初始化期间就退出，快照会记成 1（被 launchd 收养的孤儿），之后 `getppid()` 恒等于 1，
  watchdog 永不触发，服务进程与端口残留。现桌面端 spawn 引擎时显式传 `--parent <自身 PID>`
  （PID 在 spawn 前确定，无竞态），父进程退出后 `getppid()` 变为 1 与该 PID 不等即自动关闭；
  `--parent` 仅接受大于 1 的 PID，非法值按未传处理。命令行独立运行（未传 `--parent`）时仍取
  快照，但快照为 1 视为 systemd/launchd/nohup 正常收养（无法与孤儿区分）并跳过 watchdog，
  不再误杀独立部署。
- **实时列表 key 冲突修复**：悬浮窗列表与应用内「实时调用」在同秒内出现多条相同记录（如同一秒内
  连续报错）时，旧 key 公式（`时间戳_服务_输出_tokens_错误`）会让多条记录的 key 完全相同，
  Vue keyed diff 丢映射后这些节点被钉在旧位置不再移动，表现为部分行（多为旧错误行）永远停在
  列表顶端、看似未按时间排序。key 改为拼接 `input/cache_read/cache_write/output/duration_ms`
  等字段保证唯一，两条列表已实测恢复严格时间倒序（最新在前）。
- **实时列表只显示当天记录**：悬浮窗列表与应用内「实时调用」不再出现昨天的错误条目。
  新增 `desktop/src/format.ts` 统一口径 `todayLiveRecords()`（按 `timestamp` 日期段过滤当天
  + 完整 ISO 时间戳字符串倒序，最新在前），悬浮窗列表/迷你走势与面板 `livePool`（列表、
  走势、近5min 汇总）全部同源；后端 `get_live` 额外按 `timestamp` 日期过滤，防当天 jsonl
  残留跨天写入的旧记录（轮询暂停/缓存残留时也不会串日）。时间列恒为 `HH:mm:ss`，
  移除原先「非当天加 MM-DD 前缀」的兼容分支。
- **/v1/models 主模型恒在列**：白名单（`models`/`models_map`）未含主模型时，`/v1/models` 仍返回主模型并置首
  （`default=true`，`required` 随 `override_model`），已作为别名目标时不重复暴露上游名；
  `model_policy` 任何策略（含 `reject`）均恒放行对主模型的请求。

### 价格架构完善

- **GLM-5.3-Flash 定价**：`pricing.json` dashscope 段新增 `ZHIPU/GLM-5.3-Flash`
  （输入 0.8 / 输出 2.8 / 缓存命中 0.23 元/M，限时 5 折），来源 `qianwenai.com` 模型页。

- **费用结果增强**：`CostResult` 增加 `complete/currency/rule_id/source/updated_at/approximate`
  （Python 内部 API 返回；UI 默认不切换 `—`，避免存量回归）；`CacheStats._calc_cost` 与 Rust
  `recalc_cost` 均传入 `service_id`，服务级定价真正生效；`pricing_extra.batch` 注入
  `meta.batch=true`，batch modifier 可应用；free_quota/cumulative 累计口径继续以共享 golden 固化。
- **历史价格规则**：`pricing.json` v3 `rules`（事件时间区间 + 最具体 scope 优先 + 重叠校验）；
  同一模型不同日期命中不同规则；有 rules 未命中时 fail-closed 返回 `complete=false`；
  新增 `pricing_fingerprint` 与 `GET /pricing-meta`；`POST /pricing-reload` 显式清缓存热加载；
  Python `/stats` 改为从 JSONL 按事件时间派生费用（summary 仅作旧数据回退）。
- **套餐目录**：新增 `plans.json` 套餐目录（included/overage/free_tier/windows/version）；
  `services[].pricing.plan` 在 `/quota` 快照中补全套餐名、额度与超额定义。
- **第三方配额适配**：新增 `declarative`、`opencode-go`、`zai`（含 `glm-coding-plan` 别名）
  与 OpenRouter credits 模式；失败降级 local 并标 stale；mock 单测覆盖。
- **订阅消耗展示**：OpenCode Go 适配器升级为 Cookie + 工作区页（rolling/weekly/monthly），
   新增 ChatGPT / Codex 订阅适配器（chatgpt.com wham/usage + OAuth token/refresh）；引擎 /quota
   真正接入 aiohttp session，provider 适配器不再因缺 session 静默降级；桌面「全部」视图新增
   「订阅额度」网格，所有订阅账号用量集中一处显示，单服务保持原额度卡；额度卡支持展开/收缩与手动刷新（带加载动画），上下边距调整；Rust `/quota` 转发补接入凭证，移除 `[stats-diag]` 调试日志。
- **双端一致性**：`pricing/golden/cases.json` 新增 v3 历史规则、scope 精确优先、batch、
  完整性用例；pytest 与 cargo test 全绿。

## [0.3.0] - 2026-08-28

安全鉴权、服务身份 id 化、
模型白名单、定价引擎 v2、订阅额度、配置热加载与统计/面板体验重构。

### 安全

- **引擎接入层鉴权**：`services[].auth_token`（服务级，顶层 `auth_token` 全局兜底）非空时，
  所有路径校验 `Authorization: Bearer` / `x-api-key`，`/health` 恒放行探活；未配置凭证的
  服务启动时打安全警告。401 错误体同时兼容 Anthropic / OpenAI 客户端解析。

### 架构（服务身份 id 化）

- **services[].id**（`svc-<8hex>` 随机，终生不变）：缺失时惰性生成写回（自动备份）。
  `--service`、桌面端启停、children 表、统计记录（新增 `service_id` 字段，`service`
  显示名保持原样）、summary 目录（id 优先 + 旧名双查）、前端选中态/标签栏/运行态/
  删除判定全链路换 id——**改名不再误停服务、历史统计不丢、改名瞬间不回绑**。
- 新字段：`order` / `enabled`（停用不装载、不参与 start_all）/ `autostart`（App 启动自动拉起）。
- 一次性迁移脚本 `scripts/migrate_service_ids.py`（`--dry-run` + 备份 + summary 改名 +
  可选 JSONL `service_id` 回填）。

### 新能力

- **服务级模型白名单**：`models`（对外可见白名单，留空不限制）、`models_map`（对外名→上游名
  别名，统计记对外名）、`model_policy`（clamp 强转主模型 / reject 400 列出可用 / passthrough
  透传）；`/v1/models` 返回白名单全集（default/required 标记）。
- **pricing 字段升级（§2.3）**：除 `""` / `"none"` 外支持对象 `{"mode": "token"|"subscription"|"free"}`。
- **配置热加载**：`POST /_reload`（带接入凭证）或 SIGHUP 触发；按 id diff——新增启动、删除停机、
  host/port 变化换绑重启，其余字段（模型/白名单/凭证等）**原地生效**；重载期间请求 503 + Retry-After；
  先起新后停旧，失败回滚不留半加载；桌面端保存配置自动触发热加载。
- **订阅额度**：`o2a/quota/` 适配器注册表（local / local-rolling-5h / manual / openrouter，
  失败降级 local 并标 stale，1.5s 超时永不阻塞主流程）+ 引擎 `GET /quota?account=<id>`；
  桌面端 `QuotaCard`（多窗口进度条，>80% 琥珀 / >90% 红）。

### 定价引擎 v2

- `o2a/pricing/`（Python）与 `desktop/src-tauri/src/pricing.rs`（Rust）同构模块：v1 兼容映射、
  覆盖链（服务级 > 账号级 > 模型级）、modifier 管道；**共享 golden fixtures**（pytest 与
  cargo test 跑同一份 `pricing/golden/cases.json`），双实现零漂移。
- 新生效的定价维度（pricing.json 存量字段此前未读取）：`discount`（限时折扣，qwen3.7-max 费用减半）、
  `output_thinking`（思考输出单价）、多档 `tiers[].range`（上下文阶梯，300K+ 请求按高档计价）、
  `free_quota`（月度免费额度冲抵）。新增 modifier：schedule（峰谷/周末）、cumulative_tier（累计阶梯）。

### 桌面端

- 服务列表视图（>6 服务自动启用）：搜索 / 状态筛选 / 排序 / 批量启停删除 / 行内操作 / 今日用量。
- 服务管理增强：克隆（自动空闲端口）、`listen_host` UI、端口冲突保存前报错、外部端口占用预检、
  脏状态（切页确认 + 保存按钮高亮）、删除撤销（Toast 动作，5s 内存回滚）。
- 改名体验：comment 草稿提交 + 行内校验（空名/重名/非法字符红字不写回）、「保存并重启」单服务按钮。
- 统计/面板：单一心跳轮询（自适应降频 5s→15s）、区间下拉修复（预设与自定义分离、取值集统一可记忆）、
  Toast 动作/手动关闭、SelectBox 搜索（非 custom 模式、键盘流）、Ctrl+K 服务跳转、Esc 关面板。
- Rust 统计缓存单槽 → 多槽（cap 32），悬浮窗与"全部"视图不再互相顶掉。
- 托盘菜单新增逐服务启停（随运行态自动重建：○ 启动 / ● 停止）。
- addService 默认值修复（模型留空必填 +「新服务-N」）、migrateAccounts id 查重。

### 兼容性

- 存量 config.json / pricing.json **零迁移可用**（id 惰性补齐、pricing 双格式、定价 v1 映射零行为变更）。
- 桌面端启停与统计接口同时接受 id 与 comment（老脚本一个版本周期内兼容）。

## [未发布]

### 修复（桌面端 · 悬浮窗）

- **实时列表不再有跨天/旧记录排前**：列表此前依赖 `Date.parse` 与 jsonl 行序，解析异常或写进程时钟抖动时，旧记录（如昨天的错误）可能插到更新的正常请求前面，且时间列只显示时分秒难以分辨跨天。现在前后端都按完整时间戳（零填充 ISO 字符串，字典序即时间序）严格倒序，前端不再用 `Date.parse`；非当天记录的时间列自动加 `MM-DD` 前缀（如 `08-25 16:10`），昨天的数据一眼可辨且始终排在今天之后。`get_live` 短时缓存 key 纳入日期，跨天不会命中旧缓存。
- **服务下拉在小窗口里选不到后面的服务**：下拉菜单原为固定 `max-height: 220px` 且被
  悬浮窗窗口边界裁剪，服务多时超出窗口底部的选项点不到，必须拉高窗口才能选择。
  SelectBox 现按窗口剩余空间收敛菜单高度（内部滚动选择），下方空间不足且上方更宽裕时
  自动向上翻转；窗口缩放时实时重算定位。主面板所有下拉同步受益。

### 变更（桌面端 · 统计面板）

- **按模型过滤扩大到整页统计**：统计页「按模型过滤」此前只影响"按模型"分组卡片；现在过滤在记录层统一施加，
  KPI 卡（请求 / 错误 / Token / 命中率 / 费用）、性能条（耗时 / 首字 / 速度）、图表序列（逐分钟 / 逐日）、
  按服务拆分、区间汇总与同比基线（prevAgg）均随所选模型收窄口径；实时调用列表 / 迷你走势 / 近 5 分钟汇总在前端同步过滤。
  - `get_stats` 命令与 Rust `stats::get_stats` 新增 `model` 参数，TTL 缓存键纳入该参数；
    返回体新增 `models`（当前区间模型全集，不受过滤影响，供下拉框切换）与 `model` 回显字段。
  - 切换服务 / 时间区间仍会重置模型过滤；日历热力图保持全量口径（定位用导航组件，不参与过滤）。

## [0.2.0] - 2026-08-19

目录结构整理 + 代码收拢为 Python 包（行为等价重构，默认统计目录路径有变化）。

### 结构变化

- **Python 逻辑收拢为 `o2a/` 包**：`engine.py`（原 `proxy_async.py`）、`convert.py`（协议转换）、`config.py`（账号/服务/配置）、`stats.py`（缓存统计与计费）、`base.py`（日志/常量/项目根定位）
- **根目录兼容 shim**：`proxy.py` / `proxy_async.py` 改为 re-export 兼容层——桌面端路径探测（`find_root`）、绿色版组装、旧导入方式（`import proxy` / `import proxy_async`）全部不受影响；新增 `python -m o2a` 入口
- **脚本收拢**：`start-proxy.sh` / `cache-summary.sh` 移入 `scripts/`（`cache-stats.py` 保留在根目录）
- **测试收拢**：三个 `test_*.py` 移入 `tests/`（新增 `conftest.py` 注入项目根；CI 与 pytest 双路径均可运行）
- **运行时数据分离**：统计默认目录由 `cache_stats/` 改为 `<项目根>/data/cache_stats`；运行日志收拢到 `logs/`（均为运行时生成，已入 .gitignore）
- **.gitignore 修正**：`o2a-proxy.exe` → `o2a-desktop.exe`，新增 `data/`、`logs/`、`.pi-subagents/`、`.pytest_cache/`、`vite-dev.*.log`
- **桌面端适配**：`default_stats_dir` 同步为 `root/data/cache_stats`；绿色版打包（`build-portable.sh`）与 Tauri 资源（`tauri.conf.json`）增加 `o2a/` 包目录

### 行为说明

- 配置加载优先级、统计语义、协议转换逻辑均未变化；`cache_stats_dir` 显式配置时仍以项目根解析相对路径
- 已有历史统计数据可手动迁移：`cache_stats/*` → `data/cache_stats/`

## [0.1.1] - 2026-08-12

协议审计专项修复（30 项问题，含 3 个流式终止缺陷，均已在真实引擎上实证复现并修复）。

### 修复（客户端挂起 / usage 错乱）

- **流式终止兜底**：claude 流上游 EOF 无 `[DONE]` 时补发 `message_delta` / `message_stop`（含 thinking / 工具块闭合），并补 `record_stats`；`STREAM_TIMEOUT` 收尾同步补统计与工具块闭合
- **Responses 转换流终止**：`[DONE]` / EOF 时 flush `_ResponsesStreamTranslator._finish()` 补发 done 事件与 `response.completed`（此前无 finish_reason 的流永不结束）
- **completed usage 修复**：`response.completed` 延迟到流结束发射（usage 尾块先到达），usage 不再为 null；`_finish` / `complete` 幂等化

### 修复（映射与语义丢失）

- `tool_choice="any"`（Anthropic 必须调用）→ 单工具绑定 / 多工具 `required`，dict 形式不再静默丢弃
- DeepSeek 顶层 `prompt_cache_hit_tokens` / `prompt_cache_miss_tokens` 计入缓存读，命中率统计不再失真
- Responses 格式 `output_tokens_details.reasoning_tokens` 读取（推理 token 不再恒 0）
- 非 200 上游错误体原样透传（去掉 `[:300]` 截断，429 `Retry-After` 等错误码信息不再丢失）
- `max_tokens` 统一 131072 封顶（`convert_request` 与 `_responses_to_chat` 两条转换路径，避免 1M 上下文默认值触发上游 400）

### 修复（中低危与稳定性）

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
