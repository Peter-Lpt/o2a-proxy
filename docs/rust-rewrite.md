# o2a-proxy 全 Rust 重写技术实现文档

> 状态：v1 草案（待评审）
> 原则：**以 Python 实现的功能行为为唯一基准**，开源项目（anthropic-proxy-rs / llm-bridge-core）仅作 Rust 流式实现惯例参考，不照搬其协议语义。

## 1. 目标与范围

将 `o2a/` Python 包（6,568 行 + 2,224 行测试）整体替换为 Rust 引擎二进制 `o2a-engine`，消除 Python 运行时依赖。

**范围内**：
- 引擎全部 HTTP 端点与三种分派模式（claude / codex / direct）
- 协议转换与 SSE 流式翻译（双向）
- 配置加载（config.json / auth.json / service_ids.json 迁移与惰性写回）
- 统计（JSONL 记录 + 小时聚合 + 费用重放）
- 额度适配器（8 个）
- 定价（复用桌面端已通过 golden 的 Rust 实现）
- 桌面端 spawn 改造（`proxy_async.py` → `o2a-engine`）

**范围外**（本里程碑不动）：
- 桌面端 Vue 前端（HTTP 契约不变则零改动）
- `desktop/src-tauri/src/stats.rs` 的读取端逻辑（读同一份 JSONL，保持现状；pricing.rs 的构建引用改为 path 依赖提取出的 crate，行为不动）
- `scripts/start-proxy.sh`（Python 脚本，后续单独处理或废弃）
- 根目录 `proxy.py` / `proxy_async.py` 兼容 shim（过渡期保留，最终删除）

**验收标准**：
1. 现有 `tests/` 中与引擎行为相关的用例，全部有等价的 Rust 契约测试覆盖（以 HTTP 行为对齐为准，不要求逐行移植）
2. `pricing/golden/cases.json` 在新引擎计价下全部通过（已有 Rust 实现复用）
3. 真实客户端冒烟：Claude Code（claude 模式）、Codex CLI（codex 模式）、Claude Code 直连（direct 模式）各跑一轮对话 + 工具调用
4. 历史统计数据（旧 JSONL/summary）在新引擎下可读、可聚合、费用重放一致

## 2. 现状模块 → Rust 映射

| Python 现状 | 行数 | Rust 目标 | 说明 |
|---|---|---|---|
| `o2a/base.py` | 130 | `o2a-config` crate | 路径解析（O2A_CONFIG/O2A_AUTH）、URL 归一化 |
| `o2a/config.py` | 503 | `o2a-config` crate | Account/Service、三种旧结构迁移、service_ids 惰性写回 |
| `o2a/convert.py` | 996 | `o2a-convert` crate | 请求转换 + Responses 流翻译（纯函数，重点测试对象） |
| `o2a/stats.py` | 755 | `o2a-stats` crate | JSONL/summary/费用重放/账号归并 |
| `o2a/engine.py` | 1889 | `o2a-engine` bin | HTTP 服务、四类 handler、热重载、鉴权、任务状态 |
| `o2a/quota/` | ~800 | `o2a-quota` crate | 注册表 + 8 适配器 |
| `o2a/pricing/` | 1052 | **复用** `desktop/src-tauri/src/pricing.rs` | golden 已双端对齐，提取为 crate |
| `desktop/src-tauri/src/proxy.rs` | 480 | 原地修改 | spawn python → spawn 二进制 |

## 3. 架构决策

### 3.1 独立引擎二进制（不嵌入 Tauri 进程）

保持"桌面端 spawn 子进程"模型不变，仅把子进程从 python 换成 `o2a-engine`：

- 桌面端改动最小：`proxy.rs` 的 spawn/children/watchdog 逻辑原样保留，只换命令与参数
- 引擎可独立于桌面端运行（`scripts/start-proxy.sh` 的替代品天然是 `o2a-engine --config ...`）
- 崩溃隔离：引擎崩溃不拖垮桌面端
- 后续若要嵌入 Tauri（消除进程管理），引擎 crate 可作为库被引用，路径是开放的

CLI 契约（对齐现有）：

```
o2a-engine [--service <id|comment|port>] [--config <路径|目录>] [--auth <路径|目录>]
```

环境变量等价：`O2A_CONFIG` / `O2A_AUTH` / `CACHE_STATS_DIR` / `CACHE_STATS_ENABLED` / `CACHE_STATS_RETENTION_DAYS` / `HTTP_PROXY`。桌面端 `proxy.rs` 现有的 env 传递逻辑（`O2A_CONFIG`、`O2A_AUTH`、`CACHE_STATS_DIR`）全部沿用。

路径解析优先级：`--config` 参数 > 环境变量 > 当前工作目录下的 config.json。Python 的 `_project_root()`（向上找 proxy.py 标记）不再需要——桌面端总是显式传路径。

**pricing.json / plans.json 的路径解析**（Python 依赖 PROJECT_ROOT，Rust 必须显式定义）：

- pricing.json：`O2A_PRICING` env（文件）> config.json 同目录 > 当前工作目录
- plans.json：`O2A_PLANS` env > config.json 同目录 > 当前工作目录
- 桌面端 `proxy.rs` spawn 时同步传入 `O2A_PRICING` / `O2A_PLANS`（与 `CACHE_STATS_DIR` 同样的传法）

### 3.2 Crate 布局（workspace）

```
worktree 根/
├── Cargo.toml                  # workspace
├── engine/                     # bin: o2a-engine
│   └── src/main.rs, server/    # axum 路由、四类 handler、热重载、任务状态
├── crates/
│   ├── o2a-config/             # Account/Service/加载迁移/路径
│   ├── o2a-convert/            # 协议转换（纯函数 + 流式翻译器）
│   ├── o2a-stats/              # JSONL/summary/费用重放
│   ├── o2a-quota/              # 注册表 + 适配器
│   └── o2a-pricing/            # 从 desktop pricing.rs 提取（golden 对齐）
├── pricing/golden/cases.json   # 共享 golden（现有资产）
└── desktop/                    # 既有，proxy.rs 小改
```

依赖选型：`tokio`（runtime）、`axum`（HTTP 服务，SSE 支持 `axum::response::sse` 或手写 StreamResponse）、`reqwest`（上游客户端，连接池 `reqwest::Client` 共享，对应 aiohttp ClientSession）、`serde`/`serde_json`、`tracing`（日志）、`chrono`（时间）、`rand`（服务 id）。不引入额外重框架。

reqwest 注意点（对应 aiohttp 行为）：
- 连接池：`Client::builder().pool_max_idle_per_host(200)`，对应 `UPSTREAM_POOL_LIMIT=200`
- 超时：连接 120s（`CONNECT_TIMEOUT`），流式读无 total 超时但受 `STREAM_TIMEOUT=600s` 的"无进展"约束（Python 是 sock_read；Rust 用 `tokio::time::timeout` 包裹每次 chunk read 实现"读间隔超时"）
- 代理：`reqwest::Proxy::all(...)`，对应 `service.proxy`（来自 `HTTP_PROXY` env）
- 客户端断连：axum 中检测 body 写入失败/`Upgrade` 中断；用 `tokio::select!` 监听客户端连接关闭来取消上游流（对应 Python ClientGone/CancelledError 语义：取消即停止拉上游）

### 3.3 单进程多端口

与 Python 一致：一个进程加载全部 enabled 且 account.valid 的服务，每服务一个 TCP 监听（axum 多 listener：为每服务建 Router 后 `axum::serve(TcpListener)` 多任务并行）。`--service` 过滤逻辑照搬（id / comment / 端口三匹配）。

### 3.4 JSON 兼容性约定

- 请求/响应 JSON 字段顺序不作要求（客户端不依赖）；数值精度对齐（cost 保留 6 位小数、hit_rate 4 位，**注意 Python 内建 `round()` 是 banker's rounding（half-even，如 0.00065 → 0.0006），Rust `f64::round` 是 half-away-from-zero（同例 → 0.0007）**。实现采用 `format!("{:.n$}", x)` 后 parse 回 f64（Rust 的 `{:.n$}` 格式化同为 half-even，与 Python round 语义一致），并对 tie 场景（如 rate=13/20000）做逐字段快照测试钉死）
- JSONL 记录字段名/取值逐字段对齐（第 8 节），保证桌面端 `stats.rs` 读取端与 Python 引擎写入的历史数据无缝混读
- `ensure_ascii=False` 等价：serde_json 直接输出 UTF-8，中文不转义，一致

## 4. HTTP 契约清单（逐端点）

所有端点挂在通配路由上（Python 是 `add_route("*", "/{tail:.*}")`），鉴权先行。

### 4.1 鉴权（`_check_auth`）

- `service.auth_token` 非空时：除 `/health`（**path 精确匹配，不分 HTTP 方法**）外所有路径需 `Authorization: Bearer <token>` 或 `x-api-key` 头匹配，失败返回 401（错误体见 Python `auth_error_response`，双协议兼容格式）
- 空则放行（引擎启动日志打安全警告）
- token 提取顺序：Bearer 前缀优先，其次 x-api-key

### 4.2 GET 端点

| 端点 | 行为 |
|---|---|
| `GET /health` | `{"status":"ok"}`，恒无鉴权 |
| `GET /models` `/v1/models` | `{"object":"list","data":[...]}`；条目构造见 `_model_entries`（白名单/别名/主模型 default 标记逻辑逐条复刻） |
| `GET /status` | `{"active","active_streams","last_finish","last_activity","service","port","mode"}`（任务状态快照） |
| `GET /stats` | `?period=hour|day|all`，`?account=<id>` 走账号归并；统计禁用时 `{"error":"cache stats is disabled"}` |
| `GET /quota` | `?account=<id>`（缺省当前服务账号）；**统计禁用时同样返回 `{"error":"cache stats is disabled"}`**；TTL 60s 缓存；注册表降级语义见第 9 节 |
| `GET /pricing-meta` | `fingerprint` / `version`（缺省回退 v2）/ `currency`（缺省 CNY）/ `rules`（规则条数）/ `plans`（plan 名排序列表）/ `plans_fingerprint` |
| `GET /`（其他） | 状态摘要 JSON（`{"status","mode","client","account","target","endpoints"}`） |

### 4.3 POST 端点（管理）

| 端点 | 行为 |
|---|---|
| `POST /_reload` | 触发热重载（异步执行），立即返回 `{"status":"reloading"}` |
| `POST /pricing-reload` | 清空定价缓存，`{"status":"pricing reloaded"}` |
| 其他 POST | 代理请求（见 4.4） |

重载进行中（`_O2A_RELOADING` 标记）：非 /health 请求一律 503 + `Retry-After: 2`。

### 4.4 代理请求分发（`handle_request` 主链路）

1. 读 body → JSON 解析失败：codex 模式回 OpenAI 风格 400，其余 Anthropic 风格 400
2. `resolve_mode(service, path, payload)`：api 显式 → 直接推导；client=auto → `detect_client`（路径特征 `/v1/messages`→anthropic，`/responses`/`/chat/completions`/`/completions`→openai；body 特征 `input`无`messages`→openai，`max_tokens`+`system`→anthropic，messages[0].content 是 list→anthropic）；返回 None（OpenAI 客户端 + 无 OpenAI 端点账号）→ 400 中文错误
3. auto 服务 `with_mode(mode)` 生成模式确定的副本
4. `_apply_model_policy`（白名单/别名/策略）—— 所有模式统一施加，规则逐条复刻（别名命中优先于白名单判断；主模型恒放行；reject 时 400 列前 10 个可用模型）
5. 按 mode 分派：

**claude 模式**（Anthropic 入 → Chat 上游）：
- `convert_request` → `_strip_cache_control`（递归删 cache_control）
- stream=true → `handle_claude_stream`（第 6 节状态机）
- stream=false → `handle_claude_non_stream`（响应构造见第 6.3 节）

**codex 模式**（OpenAI 入）：
- `api=openai-completions`：Chat 整包透传（`handle_passthrough`），先 `normalize_roles`（developer→system，修改才重序列化 body）
- `api=openai-responses` + `upstream_api=openai-responses`：Responses 整包透传到 `_responses_url()` 推导端点，`responses_usage=true`（usage 从 `response.completed` 提取）
- 其余（responses→chat 转换）：`_responses_to_chat`（input 字符串/item 数组/Chat messages 直通三分支），stream → `handle_openai_stream`（Chat 入原样透传 / Responses 入走翻译器），非流式 → `handle_openai_non_stream`（Chat 入原样返回 / Responses 入 `_chat_to_responses_json`）

**direct 模式**（Anthropic 入 → Anthropic 上游透传）：
- 请求体仅两处修改：override_model 时注入 model；max_tokens 缺失时补默认。有修改才重序列化
- 上游头：`Authorization: Bearer` + `x-api-key` + `anthropic-version: 2023-06-01`，转发客户端 `anthropic-beta` 头
- 流式：SSE 原样逐行透传，旁路抓 usage（`message_start`/`message_delta` 的 Anthropic usage；其余事件的 OpenAI usage；prompt_tokens 存在则 `_convert_usage`），stop_reason 从 message_delta 抓取

**codex 分支补充**：`api=openai-responses + upstream=responses` 整包透传前同样执行 `normalize_roles`（input 项 developer→system，修改才重序列化）。另注意 **legacy 路径**：api 未声明且 client 推导为 codex 时，Chat 入请求也走 `_responses_to_chat`（其 Chat 直通分支会补 max_tokens 默认值、重建 messages），流式响应方向为 Chat SSE 原样透传（chat_body 是重建后的请求体，非原始字节）。

**passthrough 通用行为**（chat 与 responses 透传共用）：
- 上游非 200：错误体**原样透传**（包成 `openai_error_response(status, "upstream error: <err>")` 外壳），统计记 error
- 非流式：响应原样返回，旁路记 usage
- 流式：逐行透传（含非 data 行），ClientGone 静默，异常时流内发 error 事件
- model 注入规则：override_model=true 强改；false 仅缺失时补

### 4.5 上游请求通用行为

- 目标 URL：`service.target_url`（direct=anthropic_url，其余=openai_url 已归一化为完整 chat/completions）+ 客户端 query string 追加（`?beta=true` 场景）
- 头：`Content-Type: application/json` + `Authorization: Bearer <api_key>`；direct 模式附加第 4.4 节三头
- 上游非 200 时：读 text 记 error 日志（含 payload summary：model/messages 数/tools 数/bytes，**不含对话内容**），统计记 error，claude 模式回 Anthropic 风格错误、codex 回 OpenAI 风格
- `aiohttp.ClientError` 等价（连接失败/超时）：统计记 error；响应头未发 → 502 错误响应；已开始流 → 仅 `write_eof()` 结束当前响应

## 5. 协议转换语义规约（`o2a-convert`）

### 5.1 `_convert_usage`（最易错，逐条复刻）

OpenAI usage → Anthropic 语义。**Python 语义是混合的，必须区分两种 fallback**：
- **键存在即命中**（dict.get 链，仅键缺失才回退；显式 null/0 不穿透）：`prompt_total`/`output_tokens` 的取值、cached/write 的嵌套链
- **falsy(0) 穿透**（`or` 链，值为 0/None 时继续回退）：DeepSeek 首跳 `prompt_cache_hit_tokens or ...`、`reasoning_tokens` 的整条 or 链

Rust 实现用 serde 区分 missing / null / 0 三态，逐条规约：
- `prompt_total` = 键存在取值（缺失回退）`prompt_tokens`，再缺失回退 `input_tokens`，再缺失 0
- `output_tokens` = 同上语义：`completion_tokens` 存在即取，缺失回退 `output_tokens`，再缺失 0
- `cached_tokens`：先取顶层 `prompt_cache_hit_tokens`（**falsy 穿透**：0 时继续），否则嵌套链（**键存在即命中**）：`prompt_tokens_details.cached_tokens` > `prompt_tokens_details.cache_read_input_tokens` > `input_tokens_details.cached_tokens` > `input_tokens_details.cache_read_input_tokens` > 顶层 `cache_read_input_tokens`（存在即命中）> 顶层 `cached_tokens`
- `cache_write_tokens`（嵌套链全部键存在即命中）：`prompt_tokens_details.cache_creation_input_tokens` > `prompt_tokens_details.cache_write_tokens` > `input_tokens_details.cache_creation_input_tokens` > `input_tokens_details.cache_write_tokens` > 顶层 `cache_creation_input_tokens` > 顶层 `cache_write_tokens`
- `input_tokens` = max(0, prompt_total − cached − cache_write)（Anthropic 把缓存写单独报，普通输入要扣除）
- `reasoning_tokens` = `completion_tokens_details.reasoning_tokens`（**falsy 穿透**）`output_tokens_details.reasoning_tokens`，全 0/缺失 → 0
- 所有字段 best-effort int（类型不符/解析失败按 0）

### 5.2 请求方向矩阵

| 入口 | 上游 | 处理 |
|---|---|---|
| anthropic-messages | chat | `convert_request` + thinking 映射 + tools 映射 |
| openai-responses | responses | 整包透传 |
| openai-responses | chat | `_responses_to_chat` + `_apply_reasoning_to_chat` |
| openai-completions | chat | 整包透传 + normalize_roles |
| anthropic-messages | anthropic | direct 透传 |

### 5.3 `convert_request` 要点（Anthropic → Chat）

- system（str 或 block 列表）→ `_extract_text` 后插首条 system
- messages：content 为 list 时
  - 含 `tool_result` 块：逐块展开为 role=tool 消息；tool_use_id 取值顺序 `block.tool_use_id` → `block.id`，**两者皆无才跳过并警告**；交错的 text 块按出现顺序冲刷为 user 消息（`"\n".join`）
  - assistant 含 `tool_use`：合并为 `{role:"assistant", content: text 或 null, tool_calls:[...]}`（arguments = json.dumps(input)）
  - 普通：`_extract_text` 为纯文本；空文本消息跳过（部分上游拒绝空 content）
- thinking：`_apply_thinking_to_chat`，按 thinking_mode 五态（auto 推断规则：url/model 含 dashscope/qwen→enable_thinking；deepseek/moonshot/kimi→passthrough；其他→effort）；passthrough 保留 type+budget_tokens；enable_thinking 布尔；effort 映射 budget≥8192→high、≥2048→medium、其余 low、enabled 无预算→medium
- tools：input_schema 补 `"type":"object"`；tool_choice：字符串 `any` → 单工具绑定/多工具 `required`；对象形式 `type:tool` → 命名 function 绑定，`type:any` → 同字符串 any，`auto/none` 原样
- stream=true 时加 `stream_options:{include_usage:true}`；temperature/top_p 存在才转发；max_tokens 缺省用 service.max_tokens
- 模型：override_model=true 强转 service.model；false 透传客户端名（缺失回退）

### 5.4 `_responses_to_chat` 要点

- 无 `input`（Chat messages 入）：**逐条重建 messages**（非 dict 项丢弃；developer→system），整包字段保留（stream/tools/stop 等），仅换 model + 规范化 role；无 max_tokens/max_output_tokens 时补 service.max_tokens。注意此分支与 `api=openai-completions` 的整包透传不同（后者不重建、不补 max_tokens）
- input 为字符串 → 单条 user
- item 遍历：`function_call` 聚合 pending（连续项合并为一条 assistant+tool_calls）；`function_call_output` 冲刷 pending 后转 role=tool；role 项（developer→system）content 提取为纯文本
- `instructions` 合并：首条已是 system 时拼 `"\n\n"`，否则插入
- stream=true 补 `stream_options`；**白名单透传**：temperature/top_p/stream_options/seed/parallel_tool_calls 存在才复制；`max_output_tokens` 存在时**重命名**为 `max_tokens`
- tools 仅取 `type:function` 转 OpenAI 结构（name/description/parameters/strict）；tool_choice 对象转 `{"type":"function","function":{name}}`，字符串原样
- `_apply_reasoning_to_chat`：reasoning.effort 或顶层 reasoning_effort → 按 thinking_mode 映射（effort 原样 / passthrough→`thinking:{type:"enabled"}` / enable_thinking→true）

### 5.5 Responses 流翻译器（`_ResponsesStreamTranslator` 逐事件复刻）

状态：response_id（`resp_`+24hex）、created_at、output_index、_emitted_created、_finished、_completed、message/reasoning/tool 三类 item 的 delivered 标志与文本缓冲、_tool_states（index→{id,name,arguments,output_index,item_id}）、_output_sequence（交付顺序）。

事件序列（对每个 chat chunk）：
- `reasoning_content` 增量 → ensure_created → 首次交付 reasoning item（`response.output_item.added`）→ `response.reasoning_summary_text.delta`
- `content` 增量 → ensure_created → 首次交付 message item（added + content_part.added）→ `response.output_text.delta`
- `tool_calls` 增量 → 按 index 聚合 state（name/id 后到覆盖）→ arguments 非空时 ensure_created + deliver_tool（added）→ `response.function_call_arguments.delta`
- finish_reason 出现 → `_close_items`（幂等 done 事件：reasoning done→item.done、message text.done→content_part.done→item.done、tools arguments.done→item.done；**不发射 completed**）
- 流结束（[DONE]/EOF）→ `complete()`：close_items + `response.completed`（assemble 按 _output_sequence 顺序输出全部 item，status=completed，usage=最后收到的 chunk usage）
- usage 抓取：`_chat_usage_to_responses`（含 DeepSeek 顶层缓存字段与 details 嵌套兼容）

### 5.6 响应方向（claude 模式）

非流式：`thinking`（reasoning_content）→ text → tool_use（arguments 解析失败时保留原始字符串）的 content_list；stop_reason 映射（tool_calls/tool_use→tool_use、length→max_tokens、stop/None→end_turn、content_filter→stop_sequence）；usage 从 `_convert_usage`。

流式：完整状态机见第 6 节。

## 6. claude 模式流式状态机（`handle_claude_stream`）

这是全项目最精细的部分，事件序列与边界行为必须逐条对齐：

**输出事件骨架**：`message_start`（首 chunk 时发，id/model 取 chunk，usage 含 input/cached/cache_write/reasoning）→ `content_block_start/delta/stop`（index 递增；块类型 thinking/text/tool_use）→ `message_delta{stop_reason,usage}` → `message_stop`。

**块切换规则**：
- reasoning_content 到来：若 text 块开着先关（stop + idx++），再开 thinking 块
- content 到来：若 thinking 块开着先关（idx++），再开 text 块
- **thinking 块不因空/null reasoning_content 提前关闭**（分段思考间空块保持同一块）
- tool_use 首块（带 id）到来：先关 thinking/text 开块（idx++）
- tool_calls 分片：按 `index` 聚合；**无 id 的首块参数缓冲**（pending_orphan_args），id 块到达时合并开块；续块（无 id 或网关重复带 id）追加参数不重开块
- 每次 tool_use 参数增量发 `input_json_delta{partial_json}`

**收尾规则（多路兜底，全部幂等防重复发 message_stop）**：
1. 收到 `[DONE]` 且已 started：关全部开块 → message_delta（stop_reason 由 pending_finish_reason + had_tool_calls 推导）→ message_stop → 记统计 → break
2. finish_reason 出现：关全部开块（**不发** message_delta，等 usage 尾块）
3. 上游 EOF 无 [DONE]：同 1 补发，防客户端挂起
4. choices 为空的 chunk（带 usage 尾块）：若 pending_finish_reason 存在且未 finished → 补 message_delta+message_stop

**超时行为**：请求开始起 `STREAM_TIMEOUT=600s` 无完成 → 关全部开块、已 started 则发 max_tokens 停止 + message_stop、记统计、break。

**任务状态**：`_task_begin/end` 计数 active_streams；`_task_finish(is_final)`：有 tool_call 或 finish∈{tool_calls,tool_use,length,max_tokens} → continue（长链中间），否则 final。`/status` 的 active = active_streams>0 或 last_finish==continue。

**错误传播**：ClientGone（写失败）静默退出；其他异常：已 started → 流内 `error` 事件；未 started → JSON error 体；均记统计 error。

## 7. 配置模型（`o2a-config`）

### 7.1 数据结构

`Account { id, name, api_key, openai_url(归一化), anthropic_url, api, quota_source, quota }`，kind 推导（both/openai/anthropic/invalid），valid = kind!=invalid && api_key 非空。

`Service` 字段全集（26 个，全部复刻）：id/name/account/client/host/port/model/override_model/max_tokens/proxy/api/upstream_api/thinking_mode/pricing(mode+extra)/auth_token/order/enabled/autostart/models/models_map/model_policy + mode 推导 + target_url + reverse_models_map + with_mode。

mode 推导（逐条）：api=anthropic-messages → kind∈{anthropic,both} ? direct : claude；api∈{openai-completions,responses} → codex；api 非法值 → 警告回退 auto；无 api 时 client 推导（openai→codex；anthropic→kind 判定；auto→请求时识别）。

### 7.2 加载与迁移

- 加载顺序：config.json 存在 → 解析；`_ensure_service_ids` 惰性写回（缺 id/重复 id 时按 service_ids.json 找回或生成 `svc-<8hex>`，写回前备份 config.json.bak）
- 三种结构兼容：accounts[]+services[].account 引用；**services[].mode 非法值（不在 claude/codex/direct/auto）→ 该服务整体跳过**；旧 `mode` 字段映射到 client（claude/direct→anthropic、codex→openai，作为 api 未声明时的回退）；旧 services 内嵌 openai_base_url/key 自动迁移为 `acc-<i+1>`；auth.json 按 id→name 顺序解析 key（`{"_readme"}` 元键跳过，dict 取 `.key`，str 直取）
- 字段校验失败一律警告 + 回退默认（api 回空、upstream_api 回 completions、thinking_mode 回 auto、model_policy 回 clamp、pricing 回 token）
- env 回退：无 services 且有 DASHSCOPE_API_KEY → 单服务（LISTEN_HOST/PORT/PROXY_MODEL/PROXY_MAX_TOKENS/TARGET_URL/DASHSCOPE_URL）
- auth_token：服务级缺失回退顶层 `config.auth_token`
- CACHE_STATS_* 环境变量 setdefault 语义（env 不存在才设）

### 7.3 URL 归一化

- `_normalize_openai_url`：空/以 /chat/completions 结尾 → 原样；否则补 `/chat/completions`
- `_responses_url`：去 /chat/completions 后，以 /v1 结尾补 /responses，否则补 /v1/responses

## 8. 统计（`o2a-stats`）

### 8.1 写入（record）

JSONL 追加 `{stats_dir}/YYYY-MM-DD.jsonl`，单行字段顺序与语义：

```
timestamp(本地 "%Y-%m-%dT%H:%M:%S") service account model
status(ok|error) input_tokens cache_read_tokens cache_write_tokens output_tokens
cache_hit_rate(4位) cache_coverage(4位) cost(6位)
[service_id] [batch=true] [upstream_model≠model时] [error] [duration_ms|first_token_ms|output_tokens_per_sec(2位)]
```

- hit_rate = read/(read+input)；coverage = read/(read+input+write)
- 费用：no_cost（pricing_mode≠token）时 0；否则走 o2a-pricing（account_keys=[id,name]），cumulative_tokens = 本月早于本记录的该账号 tokens 总和（input+read+write+output，按 (month, 文件 stat 签名) 缓存）
- 写文件加进程内锁；文件锁（fcntl）在 Rust 单进程模型下由进程内 Mutex 等价，**但为兼容桌面端/旧 Python 引擎并存写同一目录，Windows/Linux/macOS 建议追加写单行 ≤ PIPE_BUF 的原子性即可**（记录一行 < 4KB，O_APPEND 追加在主流平台对小写入是原子的；文档级决策：不引入跨进程文件锁，注释说明与 Python fcntl 的差异及风险——桌面端只读，实际无并发写方）
- 小时聚合 summary：`summary/<service_id>/<date>.json`（无 id 回退名目录），`hours.<HH>` 累加 requests/total_*/_hit_rate_sum/_coverage_sum；跨小时打印上一小时汇总日志
- 启动清理：retention_days（默认 30）外的 .jsonl/.json 删除（含 summary 子目录）
- 别名：model 记对外名（reverse_models_map 反查），upstream_model 记上游名（计价用）
- meta：duration_ms（请求开始→完成）、first_token_ms（首 chunk）、output_tokens_per_sec（output_tokens/生成秒数，加权口径）

### 8.2 读取（get_summary）

- period=day：按天重放 JSONL（status=error 跳过；服务匹配规则：service_id 命中 || service 名命中 || 两者皆空），费用按当前 pricing 目录**重算**（upstream_model 优先）；JSONL 缺失回退旧 summary JSON（清内部字段）
- period=hour：day 结果中取当前小时
- period=all：日期并集聚合 + total
- 账号归并（get_account_summary）：该账号全部服务 get_summary 后合并 daily_total，hours 按 id 叠加（hit_rate/coverage 取平均——沿用 Python 的相加除二近似，注释标注口径）

## 9. 额度（`o2a-quota`）

注册表 + 8 适配器，语义全部照搬：

- `resolve_adapter_name`：显式且已注册 → 直用；**显式但未注册** → 继续走域名嗅探（openrouter.ai/opencode.ai/chatgpt.com/openai.com/bigmodel.cn/z.ai 子串），嗅探不中 → local；预留名 anthropic/zen/generic → local；auto → 嗅探 > auto+quota.limit→manual > local。别名表：glm-coding-plan→zai、opencode_go→opencode-go、openai-codex/openai/gpt/chatgpt→codex 等
- `get_snapshot_async`：TTL 60s 缓存（键=account id）→ 适配器 fetch（超时 1.5s）→ 异常/None 降级 local（标 stale）→ local 也失败 → 空窗口最小快照 stale
- 适配器清单与端点：
  - local：JSONL 窗口计数（day/week/month，requests|tokens|usd）
  - local-rolling-5h：5h 滚动窗，reset_at = 窗内最早记录 +5h
  - manual：quota.limit 手填，本地用量
  - declarative：quota.windows 声明式
  - openrouter：GET /api/v1/key（mode=credits → /api/v1/credits）
  - opencode-go：{base}/usage（rolling/weekly/monthly 百分比）→ Cookie SSR 页面解析 → 旧 /v1/usage 兜底
  - zai：{base}/balance → /plan
  - codex：chatgpt.com/backend-api/wham/usage；401 时用 refresh_token 走 auth.openai.com/oauth/token 刷新并尽力写回 token 文件；limit_window_seconds 归类窗口（字段顺序切换免疫）
- `/quota` 响应组装：快照 + pricing plan 注入（planName/plan/windows 补全），错误 `{"error":"account not found: <id>"}`

## 10. 定价（`o2a-pricing`）

复用 `desktop/src-tauri/src/pricing.rs` 的 resolve/evaluate 实现（已通过 `pricing/golden/cases.json` 双端 golden），**但提取范围不止于此**：

1. 将 `pricing.rs` 的 `entry_to_v2/resolve_entry/evaluate/resolve_cost` 提取为 workspace crate（实现不动，仅调整引用路径）
2. **移植 Python 独有的两个模块**（desktop pricing.rs 不含）：`o2a/pricing/plans.py`（load_plans/get_plan/plan_windows_to_snapshot，/quota 的 plan 注入依赖）与 `o2a/pricing/fingerprint.py`（pricing_fingerprint/plans_fingerprint，/pricing-meta 依赖）；两者补单测（golden 只覆盖 resolve/evaluate，不覆盖指纹与 plan 目录）
3. 桌面端 `desktop/src-tauri` 改为 path 依赖该 crate——**行为不动，仅构建引用可动**
4. 引擎侧用于：record 时 cost 快照、/stats 读取重放、/pricing-meta 指纹
5. 热加载：文件 mtime+size 签名缓存（对应 Python `_load_pricing`），`POST /pricing-reload` 清缓存

## 11. 引擎运行时（`engine` bin）

- 日志：tracing，格式对齐 `%(asctime)s [%(levelname)s] %(message)s`（桌面端日志面板按行展示，无解析依赖，格式近似即可）
- 父进程 watchdog：线程每 2s 检查父 PID 变化 → 退出（对应 `_parent_watchdog`，桌面端防孤儿依赖）
- SIGHUP 热重载（仅 POSIX；Windows 走 POST /_reload，桌面端已用后者）
- 热重载 diff：按 id diff 出 (start/stop/swap)；先起新（失败回滚已启的），停旧，swap 原地换 Service；`_O2A_RELOADING` 标记期间 503+Retry-After:2；并发去重
- 未配置 auth_token 的服务启动时打安全警告（文案对齐）
- 请求体上限 128MB（对应 MAX_BODY_SIZE）

## 12. 桌面端改造（`desktop/src-tauri`）

1. `proxy.rs`：`Command::new(state.python).arg("proxy_async.py")` → `Command::new(engine_binary_path())`，参数 `--service <name> --config <path> --auth <path>`；engine 路径解析顺序：`O2A_ENGINE` env > 项目根 `o2a-engine`（或 `.exe`）> 与桌面端同目录 > 报错提示；同步传入 `O2A_PRICING` / `O2A_PLANS`（见 §3.1）
2. **`find_root` 标记改造**（lib.rs:49，三处依赖 `proxy.py` 存在性：O2A_ROOT 校验 / cwd 向上扫描 / resource_dir 兜底）：标记改为 `o2a-engine` 二进制（打包版）或 `config.json`（开发态）；`state.root` 被 config/auth 默认路径、`log_path`、`default_stats_dir`、`resolve_root` 依赖，标记漂移会导致打包版全部路径回退 cwd，必须一并处理
3. `find_python` / `resolve_python` 命令保留但不再被启动链路依赖（过渡期兼容外部 python 引擎探测），最终移除
4. `build-portable.sh`：不再拷 o2a/ + proxy*.py，改拷 `o2a-engine` 二进制；使用说明更新（无需 Python）
5. `tauri.conf.json` resources：o2a 包目录 → o2a-engine 二进制
6. 验活窗口：保持 1.2s（本轮不优化启动速度，另开任务）

## 13. 测试与验收

### 13.1 单元/契约测试（cargo test）

| crate | 测试 | 对照 Python |
|---|---|---|
| o2a-convert | 请求转换矩阵（claude/responses/chat×上游）、usage fallback 链全组合、thinking 五态、Responses 流翻译器事件序列快照 | test_thinking.py、test_codex_direct.py 行为 |
| o2a-config | 三种结构迁移、auth.json 解析、service_ids 写回+备份、字段非法回退 | test_config_migration.py、test_auth.py |
| o2a-pricing | 共享 golden `pricing/golden/cases.json` | test_pricing_golden.py |
| o2a-stats | JSONL 记录格式快照、summary 聚合、费用重放、账号归并 | test_cache_stats.py |
| o2a-quota | 注册表解析（嗅探/别名/预留）、TTL 缓存、降级标 stale | test_quota.py |
| engine | HTTP 契约测试：起测试服务器（随机端口）+ mock 上游（axum 或 wiremock），覆盖 /models 白名单矩阵、/status 任务状态、鉴权、503 重载、claude 流式端到端（mock 上游 SSE → 断言客户端 SSE 事件序列） | test_models_endpoint.py、test_reload.py、test_pricing_meta.py |

### 13.2 手动验收（真实流量）

- Claude Code → claude 模式：对话 + 工具调用 + thinking（dashscope/qwen 与 deepseek 各一）
- Codex CLI → codex 模式（responses→chat 转换）
- Claude Code → direct 模式（anthropic 端点直连）
- 桌面端启停/统计/额度卡（QuotaCard 数据形状）
- 旧统计数据读取：复制主目录 data/cache_stats 到 worktree 验证

### 13.3 桌面端单测同步

`desktop/src-tauri/src/proxy.rs` 的 `start_service_detects_immediate_exit` 等单测以 `proxy_async.py + o2a/` 为夹具，改造后需重写为以 `o2a-engine` 二进制为夹具（M6 一并处理）。

### 13.4 明确的兼容性红线

- JSONL 记录格式：桌面端 stats.rs 无改动即可读
- summary 目录结构：不变
- /status /stats /quota /models 响应形状：桌面端前端零改动
- config.json / auth.json / service_ids.json：双向兼容（旧 Python 引擎仍可读 Rust 写回的配置）

## 14. 里程碑与任务拆分（多 agent）

| 里程碑 | 内容 | 依赖 | 可并行 |
|---|---|---|---|
| M0 | workspace 骨架 + o2a-pricing 提取（golden 通过）+ o2a-config（迁移测试通过） | - | pricing ∥ config |
| M1 | o2a-convert：请求转换矩阵 + usage + thinking + Responses 流翻译（纯函数测试全绿） | M0 | 单 agent 串行（内聚性高） |
| M2 | o2a-engine 骨架：多端口 serve、鉴权、/health /status /models、任务状态、watchdog | M0 | ∥ M1 |
| M3 | claude 模式流式/非流式 handler + 契约测试（mock 上游） | M1+M2 | 单 agent |
| M4 | codex/direct/passthrough handlers + o2a-stats 写入 + 契约测试 | M3 | stats 可 ∥ |
| M5 | o2a-quota 全适配器 + /quota /stats /pricing-meta 端点 + o2a-stats **读取端**（get_summary/费用重放/账号归并，M4 只含写入端） | M4 | 单 agent |
| M6 | 桌面端改造 + build-portable + 端到端手动验收 | M5 | - |

每个里程碑完成标准：`cargo test` 全绿 + `cargo clippy` 无警告 + 对照对应 Python 测试逐条核对行为。

## 15. 风险清单

1. **流式状态机的长尾行为**（orphan args 缓冲、thinking 块合并、usage 尾块时序）——用事件序列快照测试钉死，mock 上游覆盖 5 种边界
2. **JSON 数值格式**（cost 小数位、中文不转义）——快照测试对齐
3. **Windows 差异**：fcntl 无、SIGHUP 无——Python 已有降级路径，Rust 直接条件编译
4. **双引擎并存期**：桌面端可配置回退 python（env 开关），保证可灰度回滚
5. **curl/py client 的 SSE 心跳**：上游长时间无 chunk 时 Python 靠 sock_read 超时；Rust 需等价的"读间隔超时"而非 total 超时，否则长推理会被误杀
