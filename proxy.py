#!/usr/bin/env python3
"""o2a-proxy 核心库（协议转换 / 配置加载 / 缓存统计 / 定价）。

线程版 HTTP 引擎已合并删除，代理引擎统一为 proxy_async.py（asyncio + aiohttp）。
本文件保留文件名供桌面端路径探测与测试引用，内部只提供纯函数与数据模型。
"""

import json
import logging
import os
import time
import threading
import uuid
from datetime import datetime, timedelta

# fcntl is Unix-only, provide fallback for Windows
try:
    import fcntl
    HAS_FCNTL = True
except ImportError:
    HAS_FCNTL = False

# 配置
LISTEN_HOST = os.environ.get("PROXY_HOST", "127.0.0.1")
LISTEN_PORT = int(os.environ.get("PROXY_PORT", "8317"))
TARGET_URL = os.environ.get(
    "DASHSCOPE_URL",
    "https://dashscope.aliyuncs.com/compatible-mode/v1/chat/completions"
)
API_KEY = os.environ.get("DASHSCOPE_API_KEY", "")
PROXY = os.environ.get("HTTP_PROXY", "")
PROXY_MODEL = os.environ.get("PROXY_MODEL", "qwen-plus")
# 最大输出 token 数（mac 客户端「1M 上下文」开关会将其设为 1,000,000）
PROXY_MAX_TOKENS = int(os.environ.get("PROXY_MAX_TOKENS", "4096"))
# 子 agent 模型配置（Claude Code 的 Task 工具会启动子 agent，使用 haiku 等模型）
# 默认与主 agent 相同，可单独配置
# 流式响应总超时（秒），防止模型长时间卡在推理阶段
STREAM_TIMEOUT = int(os.environ.get("STREAM_TIMEOUT", "600"))

logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s [%(levelname)s] %(message)s",
)
logger = logging.getLogger("proxy")


def _normalize_openai_url(url):
    """OpenAI 端点归一化为完整 chat/completions 地址（o2a 出口统一走 Chat）。

    - "https://api.deepseek.com"        -> "https://api.deepseek.com/chat/completions"
    - "https://.../compatible-mode/v1"  -> "https://.../compatible-mode/v1/chat/completions"
    - "https://.../v1/chat/completions" -> 原样（已是完整地址）
    """
    url = (url or "").strip().rstrip("/")
    if not url or url.endswith("/chat/completions"):
        return url
    return url + "/chat/completions"


class Account:
    """账号：凭证 + 端点。一个 key 最多两个端点（openai / anthropic），均可选填。

    kind 自动推导：
    - openai：只有 openai 端点（Codex 直连 / Claude 转 OpenAI）
    - anthropic：只有 anthropic 端点（Claude Code 透传）
    - both：双协议（中转站同 key 两端点）
    - invalid：两端点皆空

    api：该账号默认入口协议（openai-completions / openai-responses / anthropic-messages），
    可被 services[].api 覆盖；空表示未声明（回退 client/auto 识别）。
    """

    def __init__(self, id, name, api_key, openai_url="", anthropic_url="", api=""):
        self.id = id
        self.name = name
        self.api_key = api_key or ""
        self.openai_url = _normalize_openai_url(openai_url)
        self.anthropic_url = (anthropic_url or "").strip()
        self.api = (api or "").strip()

    @property
    def kind(self):
        has_o = bool(self.openai_url)
        has_a = bool(self.anthropic_url)
        if has_o and has_a:
            return "both"
        if has_o:
            return "openai"
        if has_a:
            return "anthropic"
        return "invalid"

    @property
    def valid(self):
        """账号是否可服务：有 key 且至少一个端点。"""
        return self.kind != "invalid" and bool(self.api_key)

    def to_dict(self):
        return {
            "id": self.id,
            "name": self.name,
            "api_key": self.api_key,
            "openai_url": self.openai_url,
            "anthropic_url": self.anthropic_url,
        }


class Service:
    """单个服务（接入点）：独立端口 + 引用账号 + 客户端类型 + 入口协议。

    api（入口协议，显式声明，对齐 pi 的 provider.api）：
    - "anthropic-messages"：Anthropic Messages（Claude Code）
    - "openai-completions"：OpenAI Chat Completions（pi 常规 / OpenAI 兼容客户端）
    - "openai-responses"：OpenAI Responses（Codex 新 CLI）
    - ""：未声明 → 回退旧 client / auto 识别（旧配置兼容）

    upstream_api（上游原生协议，配合 api=openai-responses 使用）：
    - "openai-completions"（默认）：上游只支持 Chat → Responses 入转 Chat 发，响应转回 Responses
    - "openai-responses"：上游原生支持 Responses（如 DeepSeek 官方）→ Responses 整包透传（零转换）

    client: 旧字段，api 未声明时的兼容入口（anthropic/openai/auto）。
    mode 由 api × 账号端点推导：
    - claude：Anthropic 入口 → 转换发送 OpenAI 端点
    - codex：OpenAI 入口（chat 透传 / responses 透传或转换）→ 发送 OpenAI 端点
    - direct：Anthropic 入口 → 透传发送 Anthropic 端点
    """

    def __init__(self, name, account, client, host, port, model, override_model=True,
                 max_tokens=4096, proxy="", api="", upstream_api="", thinking_mode="auto",
                 pricing=""):
        self.name = name
        self.account = account
        self.client = client
        self.host = host
        self.port = port
        self.model = model
        self.override_model = override_model
        self.max_tokens = max_tokens
        self.proxy = proxy or ""
        self.api = (api or "").strip()
        self.upstream_api = (upstream_api or "openai-completions").strip()
        self.thinking_mode = (thinking_mode or "auto").strip() or "auto"
        # 计价模式："" = 按 pricing.json 计价；"none" = 订阅制（token plan / code plan 等），
        # 按 token 计价无意义，统计记录与面板不显示价格
        self.pricing = (pricing or "").strip()
        self._mode_override = None  # auto 服务每次请求识别后临时指定

    @property
    def api_key(self):
        return self.account.api_key

    @property
    def kind(self):
        return self.account.kind

    @property
    def mode(self):
        """推导出的分派模式（claude / codex / direct / auto）。

        api 显式声明时按声明推导（不再做请求体猜测）；
        api 未声明时回退旧 client 推导，auto 则每次请求识别。
        """
        if self._mode_override:
            return self._mode_override
        if self.api:
            if self.api == "anthropic-messages":
                return "direct" if self.kind in ("anthropic", "both") else "claude"
            if self.api in ("openai-completions", "openai-responses"):
                return "codex"
            logger.warning(f"[config] 未知 api 协议 '{self.api}'（服务 {self.name}），回退 auto 识别")
            return "auto"
        c = self.client
        if c == "openai":
            return "codex"
        if c == "anthropic":
            # 账号有 anthropic 端点 → 透传；只有 openai 端点 → 转换
            return "direct" if self.kind in ("anthropic", "both") else "claude"
        return "auto"

    @property
    def target_url(self):
        """出口端点（完整 URL）。direct 用 anthropic 端点，其余用 openai 端点。"""
        if self.mode == "direct":
            return self.account.anthropic_url
        return self.account.openai_url

    def with_mode(self, mode):
        """返回模式确定的 Service 拷贝（auto 服务每个请求用），不共享状态。"""
        s = Service(self.name, self.account, self.client, self.host, self.port,
                    self.model, self.override_model, self.max_tokens, self.proxy, self.api,
                    self.upstream_api, self.thinking_mode, self.pricing)
        s._mode_override = mode
        return s


_OPENAI_API_VALUES = ("", "anthropic-messages", "openai-completions", "openai-responses")
_UPSTREAM_API_VALUES = ("openai-completions", "openai-responses")
# 思考深度透传模式（服务级 thinking_mode）：
# - auto：按上游 URL/模型名推断（dashscope/qwen → enable_thinking；deepseek/kimi → thinking；其他 → effort）
# - passthrough：Anthropic 风格 thinking 对象原样透传（DeepSeek V3.2 / Kimi K2 / 兼容网关）
# - effort：映射为 OpenAI reasoning_effort 档位（budget_tokens → low/medium/high）
# - enable_thinking：映射为布尔开关（DashScope/Qwen 兼容模式）
# - none：不透传（保持默认模型行为）
_THINKING_MODES = ("auto", "passthrough", "effort", "enable_thinking", "none")


def _responses_url(chat_url):
    """从归一化的 chat/completions 地址推导同基座的 responses 端点。

    - https://api.deepseek.com/chat/completions      -> https://api.deepseek.com/v1/responses
    - https://x.com/v1/chat/completions              -> https://x.com/v1/responses
    """
    base = chat_url[: -len("/chat/completions")] if chat_url.endswith("/chat/completions") else chat_url.rstrip("/")
    if base.endswith("/v1"):
        return base + "/responses"
    return base + "/v1/responses"


def _default_script_path(filename):
    """默认配置目录 = 本文件（proxy.py）所在目录，即项目根，Windows/macOS 一致。"""
    return os.path.join(os.path.dirname(os.path.abspath(__file__)), filename)


def _resolve_config_path(filename, env_name):
    """解析配置文件路径：优先环境变量（O2A_CONFIG / O2A_AUTH），否则项目根。

    - 环境变量指向已存在目录或以分隔符结尾 → 视为目录，取目录下 filename
    - 环境变量指向具体文件 → 直接用
    - 未设置 → 项目根（proxy.py 所在目录）
    """
    env = os.environ.get(env_name, "").strip()
    if env:
        if os.path.isdir(env) or env.endswith(("/", "\\")):
            return os.path.join(env, filename)
        return env
    return _default_script_path(filename)


def _config_file_path():
    """config.json 路径：O2A_CONFIG 可指定文件或目录（目录时取目录下 config.json）。"""
    return _resolve_config_path("config.json", "O2A_CONFIG")


def _auth_file_path():
    """auth.json 路径：默认跟随 config.json 所在目录（整套配置一起迁移）；
    也可用 O2A_AUTH 单独指定文件或目录。"""
    if os.environ.get("O2A_AUTH", "").strip():
        return _resolve_config_path("auth.json", "O2A_AUTH")
    return os.path.join(os.path.dirname(_config_file_path()), "auth.json")


def load_auth():
    """从 auth.json 读取账号密钥（对齐 pi 的 auth.json 模式，敏感凭证独立存放）。

    格式（键可为账号 id 或 name，也兼容简单字符串值）：
    {
      "acc-1": {"type": "api_key", "key": "sk-xxx"},
      "我的账号": "sk-yyy"
    }

    文件不存在或解析失败时返回空 dict（回退 config.json 内嵌 api_key）。
    路径：默认项目根（proxy.py 同目录），可用 O2A_CONFIG / O2A_AUTH 环境变量指定。
    """
    path = _auth_file_path()
    try:
        with open(path, encoding="utf-8") as f:
            data = json.load(f)
    except (OSError, ValueError):
        return {}
    out = {}
    for k, v in (data or {}).items():
        if k.startswith("_"):
            continue  # 跳过 _readme 等元键
        if isinstance(v, dict):
            out[k] = v.get("key", "")
        elif isinstance(v, str):
            out[k] = v
    return out


def _resolve_api_key(auth, acc_id, acc_name, embedded):
    """解析账号 api_key：auth.json 优先（按 id，再按 name），回退配置内嵌（旧配置兼容）。"""
    if acc_id and acc_id in auth and auth[acc_id]:
        return auth[acc_id]
    if acc_name and acc_name in auth and auth[acc_name]:
        return auth[acc_name]
    return embedded


def load_config():
    """从 config.json 读取账号与服务列表；文件不存在时回退到环境变量（单服务）。

    支持三种结构（向后兼容）：
    - 新结构：accounts[]（账号）+ services[].account（引用 id）+ client
    - 旧结构：services[] 内嵌 openai_base_url/openai_api_key —— 自动迁移为账号
    - 密钥分离：auth.json 按账号 id/name 提供 api_key，优先于 config.json 内嵌
    """
    config_path = _config_file_path()
    auth = load_auth()
    services = []
    if os.path.exists(config_path):
        try:
            with open(config_path, encoding="utf-8") as f:
                config = json.load(f)
        except (OSError, ValueError):
            config = {}
        if config:
            # 全局缓存统计设置（供 get_stats / is_cache_stats_enabled 读取）
            os.environ.setdefault("CACHE_STATS_ENABLED",
                                  str(config.get("cache_stats_enabled", True)).lower())
            os.environ.setdefault("CACHE_STATS_DIR",
                                  config.get("cache_stats_dir", "cache_stats"))
            os.environ.setdefault("CACHE_STATS_RETENTION_DAYS",
                                  str(config.get("cache_stats_retention_days", 30)))

            services_raw = config.get("services", [])
            accounts_raw = config.get("accounts", [])
            legacy = not accounts_raw and any(
                ("openai_base_url" in s or "openai_api_key" in s) for s in services_raw
            )

            accounts = {}
            for i, a in enumerate(accounts_raw):
                acc = Account(
                    id=a.get("id") or f"acc-{i + 1}",
                    name=a.get("name") or a.get("id") or f"账号{i + 1}",
                    api_key=_resolve_api_key(auth, a.get("id"), a.get("name"), a.get("api_key", "")),
                    openai_url=a.get("openai_url", ""),
                    anthropic_url=a.get("anthropic_url", ""),
                    api=a.get("api", ""),
                )
                accounts[acc.id] = acc

            mode_to_client = {"claude": "anthropic", "codex": "openai", "direct": "anthropic"}
            for i, svc in enumerate(services_raw):
                mode = svc.get("mode", "claude")
                if mode not in ("claude", "codex", "direct", "auto"):
                    continue  # 未知模式跳过
                acc_id = svc.get("account")
                acc = accounts.get(acc_id) if acc_id else None
                if acc is None:
                    # 自动迁移：旧格式（services 内嵌 url/key）或引用缺失时按服务生成账号
                    acc = Account(
                        id=f"acc-{i + 1}",
                        name=svc.get("comment") or f"账号{i + 1}",
                        api_key=_resolve_api_key(auth, f"acc-{i + 1}", svc.get("comment"),
                                                 svc.get("openai_api_key", "")),
                        openai_url=svc.get("openai_base_url", ""),
                        anthropic_url=svc.get("anthropic_base_url", ""),
                        api=svc.get("api", ""),
                    )
                    accounts[acc.id] = acc
                # 入口协议：服务级 api 优先，回退账号级 api
                api = svc.get("api") or acc.api or ""
                if api not in _OPENAI_API_VALUES:
                    logger.warning(f"[config] 服务 {svc.get('comment')} 的 api '{api}' 不是已知协议，回退 auto")
                    api = ""
                upstream_api = svc.get("upstream_api", "openai-completions")
                if upstream_api not in _UPSTREAM_API_VALUES:
                    logger.warning(f"[config] 服务 {svc.get('comment')} 的 upstream_api '{upstream_api}' 非法，回退 openai-completions")
                    upstream_api = "openai-completions"
                thinking_mode = svc.get("thinking_mode", "auto") or "auto"
                if thinking_mode not in _THINKING_MODES:
                    logger.warning(f"[config] 服务 {svc.get('comment')} 的 thinking_mode '{thinking_mode}' 非法，回退 auto")
                    thinking_mode = "auto"
                client = svc.get("client") or mode_to_client.get(mode, "auto")
                if client not in ("anthropic", "openai", "auto"):
                    client = "auto"
                pricing = svc.get("pricing", "")
                if pricing not in ("", "none"):
                    logger.warning(f"[config] 服务 {svc.get('comment')} 的 pricing '{pricing}' 非法，忽略（仅支持 none）")
                    pricing = ""
                services.append(Service(
                    name=svc.get("comment") or svc.get("model") or mode,
                    account=acc,
                    client=client,
                    host=svc.get("listen_host", "127.0.0.1"),
                    port=int(svc.get("listen_address", "8317")),
                    model=svc.get("model", "qwen-plus"),
                    override_model=svc.get("override_model", True),
                    max_tokens=int(svc.get("max_tokens", 1000000 if svc.get("context_1m") else 4096)),
                    proxy=os.environ.get("HTTP_PROXY", ""),
                    api=api,
                    upstream_api=upstream_api,
                    thinking_mode=thinking_mode,
                    pricing=pricing,
                ))
    if not services and API_KEY:
        # 回退：环境变量配置（单服务）
        services.append(Service(
            name="default",
            account=Account(id="acc-env", name="环境变量账号", api_key=API_KEY,
                            openai_url=TARGET_URL, anthropic_url=""),
            client="auto",
            host=LISTEN_HOST, port=LISTEN_PORT,
            model=PROXY_MODEL, override_model=True, max_tokens=PROXY_MAX_TOKENS,
            proxy=PROXY,
        ))
    return services


class CacheStats:
    """缓存命中统计：记录、聚合、查询。service 非空时按服务分目录写 summary。"""

    def __init__(self, stats_dir="cache_stats", retention_days=30, service=None, account=None,
                 no_cost=False):
        self.stats_dir = stats_dir
        self.retention_days = retention_days
        self.service = service or ""
        self.account = account or ""
        self.no_cost = no_cost
        self._lock = threading.Lock()
        self._last_hour = None
        self._pricing = None
        os.makedirs(self._summary_root(), exist_ok=True)
        self._cleanup_old_files()

    def _summary_root(self):
        """summary 根目录；按服务分目录时返回其子目录。"""
        root = os.path.join(self.stats_dir, "summary")
        if self.service:
            return os.path.join(root, self.service)
        return root

    def _cleanup_old_files(self):
        """启动时清理超过保留天数的文件（含按服务分目录的 summary）。"""
        cutoff = datetime.now() - timedelta(days=self.retention_days)
        cutoff_ts = cutoff.timestamp()
        # jsonl 与 summary 根目录（含服务子目录）
        dirs = [self.stats_dir, os.path.join(self.stats_dir, "summary")]
        summary_children = os.path.join(self.stats_dir, "summary")
        if os.path.isdir(summary_children):
            for entry in os.listdir(summary_children):
                p = os.path.join(summary_children, entry)
                if os.path.isdir(p):
                    dirs.append(p)
        for dirpath in dirs:
            if not os.path.isdir(dirpath):
                continue
            for filename in os.listdir(dirpath):
                if not (filename.endswith(".jsonl") or filename.endswith(".json")):
                    continue
                filepath = os.path.join(dirpath, filename)
                try:
                    if os.path.getmtime(filepath) < cutoff_ts:
                        os.remove(filepath)
                        logger.info(f"[CACHE] Cleaned up old file: {filename}")
                except OSError:
                    pass

    def _load_pricing(self):
        """加载定价数据（缓存）。"""
        if self._pricing is not None:
            return self._pricing
        pricing_path = os.path.join(os.path.dirname(self.stats_dir), "pricing.json")
        try:
            with open(pricing_path, "r", encoding="utf-8") as f:
                self._pricing = json.load(f)
        except (OSError, ValueError):
            self._pricing = {}
        return self._pricing

    def _account_pricing(self, account, model):
        """在 pricing.json["accounts"] 中按账号 id/name 匹配模型价格，未命中返回 None。

        键可为账号 id 或 name（auth.json 同样支持两种键）。"""
        accounts_pricing = self._load_pricing().get("accounts")
        if not isinstance(accounts_pricing, dict) or not account:
            return None
        direct = accounts_pricing.get(account)
        if isinstance(direct, dict):
            m = (direct.get("models") or {}).get(model)
            if m is not None:
                return m
        # 通过 config.json 的账号列表把 id 映射到 name 再匹配
        for svc in load_config():
            acc = svc.account
            if acc.id == account and acc.name in accounts_pricing:
                m = (accounts_pricing[acc.name].get("models") or {}).get(model)
                if m is not None:
                    return m
        return None

    def _calc_cost(self, model, input_tokens, cache_read, cache_write, output_tokens, account=None):
        """计算单次请求的费用（CNY）。

        account 为账号 id（也可识别 name）；有账号级定价
        （pricing.json["accounts"][账号 id/name]）时优先，否则回退全局按模型名查找。
        """
        pricing = self._load_pricing()
        if not pricing:
            return 0.0
        # 查找模型定价：账号级优先，全局兜底
        price = self._account_pricing(account, model)
        if price is None:
            for provider in pricing:
                if provider.startswith("_") or provider == "accounts":
                    continue
                models = pricing[provider].get("models", {})
                if model in models:
                    price = models[model]
                    break
        if not price:
            return 0.0
        # 使用第一档价格（单次请求无法判断 tier）
        tier = price["tiers"][0] if price.get("tiers") else None
        if not tier:
            return 0.0
        input_cost = input_tokens * tier.get("input", 0) / 1_000_000
        output_cost = output_tokens * tier.get("output", 0) / 1_000_000
        # 缓存读：优先用 cache_hit 价格，否则按 input * 0.2
        if "cache_hit" in tier:
            cache_read_cost = cache_read * tier["cache_hit"] / 1_000_000
        else:
            cache_read_cost = cache_read * tier.get("input", 0) * 0.2 / 1_000_000
        # 缓存写：优先用 cache_miss 价格，否则按 input * 1.0
        if "cache_miss" in tier:
            cache_write_cost = cache_write * tier["cache_miss"] / 1_000_000
        else:
            cache_write_cost = cache_write * tier.get("input", 0) / 1_000_000
        return input_cost + output_cost + cache_read_cost + cache_write_cost

    def _get_today_file(self):
        """返回当天的 JSONL 文件路径（本地时间）。"""
        date_str = datetime.now().strftime("%Y-%m-%d")
        return os.path.join(self.stats_dir, f"{date_str}.jsonl")

    def _compute_rates(self, input_tokens, cache_read, cache_write):
        """计算缓存命中率和覆盖率。"""
        # cache_hit_rate: Anthropic 官方定义，不含 cache_write
        denom_hit = cache_read + input_tokens
        cache_hit_rate = cache_read / denom_hit if denom_hit > 0 else 0.0
        # cache_coverage: 整体缓存占比
        denom_cov = cache_read + input_tokens + cache_write
        cache_coverage = cache_read / denom_cov if denom_cov > 0 else 0.0
        return cache_hit_rate, cache_coverage

    def _build_record(self, model, usage):
        """构建一条统计记录。"""
        input_tokens = usage.get("input_tokens", 0)
        cache_read = usage.get("cache_read_input_tokens", 0)
        cache_write = usage.get("cache_creation_input_tokens", 0)
        output_tokens = usage.get("output_tokens", 0)
        cache_hit_rate, cache_coverage = self._compute_rates(
            input_tokens, cache_read, cache_write
        )
        cost = 0.0 if self.no_cost else self._calc_cost(
            model, input_tokens, cache_read, cache_write, output_tokens, account=self.account
        )
        return {
            "timestamp": datetime.now().strftime("%Y-%m-%dT%H:%M:%S"),
            "service": self.service,
            "account": self.account,
            "model": model,
            "input_tokens": input_tokens,
            "cache_read_tokens": cache_read,
            "cache_write_tokens": cache_write,
            "output_tokens": output_tokens,
            "cache_hit_rate": round(cache_hit_rate, 4),
            "cache_coverage": round(cache_coverage, 4),
            "cost": round(cost, 6),
        }

    def _format_log(self, record):
        """格式化单次请求的缓存日志。"""
        hit_pct = record["cache_hit_rate"] * 100
        return (
            f"[CACHE] {record['model']} "
            f"hit={hit_pct:.1f}% "
            f"read={record['cache_read_tokens']:,} "
            f"write={record['cache_write_tokens']:,} "
            f"input={record['input_tokens']:,} "
            f"out={record['output_tokens']:,}"
        )

    def record(self, model, usage):
        """记录一次请求的缓存统计。"""
        if not usage:
            return
        record = self._build_record(model, usage)

        with self._lock:
            # 写入 JSONL（文件锁防多进程，仅 Unix 支持）
            filepath = self._get_today_file()
            try:
                with open(filepath, "a") as f:
                    if HAS_FCNTL:
                        fcntl.flock(f.fileno(), fcntl.LOCK_EX)
                    f.write(json.dumps(record, ensure_ascii=False) + "\n")
                    if HAS_FCNTL:
                        fcntl.flock(f.fileno(), fcntl.LOCK_UN)
            except OSError as e:
                logger.warning(f"[CACHE] Failed to write record: {e}")

            # 懒检查：跨小时则打印上一小时汇总
            current_hour = record["timestamp"][:13]
            if self._last_hour and current_hour != self._last_hour:
                self._print_hourly_summary(self._last_hour)
            self._last_hour = current_hour

            # 更新小时聚合
            self._update_hourly_summary(record)

        # 打印单次请求日志
        logger.info(self._format_log(record))

    def _update_hourly_summary(self, record):
        """更新当天的小时聚合 JSON（按服务分目录，跨进程加锁）。"""
        date_str = record["timestamp"][:10]
        hour_str = record["timestamp"][11:13]
        summary_path = os.path.join(self._summary_root(), f"{date_str}.json")

        summary = {}
        try:
            with open(summary_path, "r") as f:
                if HAS_FCNTL:
                    fcntl.flock(f.fileno(), fcntl.LOCK_EX)
                raw = f.read()
                if HAS_FCNTL:
                    fcntl.flock(f.fileno(), fcntl.LOCK_UN)
            if raw.strip():
                summary = json.loads(raw)
        except (json.JSONDecodeError, OSError):
            summary = {}

        if "hours" not in summary:
            summary["date"] = date_str
            summary["hours"] = {}

        h = summary["hours"].setdefault(hour_str, {
            "requests": 0,
            "total_input_tokens": 0,
            "total_cache_read_tokens": 0,
            "total_cache_write_tokens": 0,
            "total_output_tokens": 0,
            "total_cost": 0.0,
            "_hit_rate_sum": 0.0,
            "_coverage_sum": 0.0,
        })
        h["requests"] += 1
        h["total_input_tokens"] += record["input_tokens"]
        h["total_cache_read_tokens"] += record["cache_read_tokens"]
        h["total_cache_write_tokens"] += record["cache_write_tokens"]
        h["total_output_tokens"] += record["output_tokens"]
        h["total_cost"] = h.get("total_cost", 0.0) + record.get("cost", 0.0)
        h["_hit_rate_sum"] += record["cache_hit_rate"]
        h["_coverage_sum"] += record["cache_coverage"]

        try:
            with open(summary_path, "w") as f:
                if HAS_FCNTL:
                    fcntl.flock(f.fileno(), fcntl.LOCK_EX)
                json.dump(summary, f, ensure_ascii=False)
                if HAS_FCNTL:
                    fcntl.flock(f.fileno(), fcntl.LOCK_UN)
        except OSError as e:
            logger.warning(f"[CACHE] Failed to write summary: {e}")

    def _print_hourly_summary(self, hour_str):
        """打印上一小时的汇总日志。"""
        date_str = hour_str[:10]
        hour = hour_str[11:13] if len(hour_str) >= 13 else hour_str[-2:]
        summary_path = os.path.join(self._summary_root(), f"{date_str}.json")
        if not os.path.exists(summary_path):
            return
        try:
            with open(summary_path, "r") as f:
                summary = json.load(f)
            h = summary.get("hours", {}).get(hour)
            if h and h["requests"] > 0:
                avg_hit = h["_hit_rate_sum"] / h["requests"] * 100
                logger.info(
                    f"[CACHE HOURLY {date_str}T{hour}] "
                    f"requests={h['requests']} "
                    f"avg_hit={avg_hit:.1f}% "
                    f"total_read={h['total_cache_read_tokens']:,} "
                    f"total_write={h['total_cache_write_tokens']:,} "
                    f"total_input={h['total_input_tokens']:,}"
                )
        except (json.JSONDecodeError, OSError):
            pass

    def get_summary(self, period="day"):
        """返回聚合统计。"""
        with self._lock:
            if period == "hour":
                return self._get_last_hour_summary()
            elif period == "day":
                return self._get_day_summary()
            elif period == "all":
                return self._get_all_summary()
            else:
                return {"error": f"unknown period: {period}"}

    def _load_day_summary(self, date_str):
        """加载某天的 summary JSON，清理内部字段。"""
        summary_path = os.path.join(self._summary_root(), f"{date_str}.json")
        if not os.path.exists(summary_path):
            return None
        try:
            with open(summary_path, "r") as f:
                summary = json.load(f)
        except (json.JSONDecodeError, OSError):
            return None

        # 清理内部字段，计算 avg
        hours_list = []
        daily = {
            "requests": 0,
            "total_input_tokens": 0,
            "total_cache_read_tokens": 0,
            "total_cache_write_tokens": 0,
            "total_output_tokens": 0,
            "total_cost": 0.0,
        }
        for hour, h in sorted(summary.get("hours", {}).items()):
            req = h["requests"]
            hour_cost = h.get("total_cost", 0.0)
            hours_list.append({
                "hour": f"{date_str}T{hour}:00:00",
                "requests": req,
                "avg_cache_hit_rate": round(h["_hit_rate_sum"] / req, 4) if req else 0.0,
                "avg_cache_coverage": round(h["_coverage_sum"] / req, 4) if req else 0.0,
                "total_cache_read_tokens": h["total_cache_read_tokens"],
                "total_cache_write_tokens": h["total_cache_write_tokens"],
                "total_input_tokens": h["total_input_tokens"],
                "total_output_tokens": h["total_output_tokens"],
                "total_cost": round(hour_cost, 6),
            })
            daily["requests"] += req
            daily["total_input_tokens"] += h["total_input_tokens"]
            daily["total_cache_read_tokens"] += h["total_cache_read_tokens"]
            daily["total_cache_write_tokens"] += h["total_cache_write_tokens"]
            daily["total_output_tokens"] += h["total_output_tokens"]
            daily["total_cost"] += hour_cost

        denom_hit = daily["total_cache_read_tokens"] + daily["total_input_tokens"]
        denom_cov = denom_hit + daily["total_cache_write_tokens"]
        daily["avg_cache_hit_rate"] = round(
            daily["total_cache_read_tokens"] / denom_hit, 4
        ) if denom_hit > 0 else 0.0
        daily["avg_cache_coverage"] = round(
            daily["total_cache_read_tokens"] / denom_cov, 4
        ) if denom_cov > 0 else 0.0

        return {"date": date_str, "hours": hours_list, "daily_total": daily}

    def _get_last_hour_summary(self):
        """返回最近一小时的统计。"""
        date_str = datetime.now().strftime("%Y-%m-%d")
        hour_str = datetime.now().strftime("%H")
        day_data = self._load_day_summary(date_str)
        if not day_data:
            return {"period": "hour", "hour": f"{date_str}T{hour_str}", "requests": 0}
        for h in day_data["hours"]:
            if h["hour"][11:13] == hour_str:
                return {"period": "hour", **h}
        return {"period": "hour", "hour": f"{date_str}T{hour_str}", "requests": 0}

    def _get_day_summary(self):
        """返回今天的统计。"""
        date_str = datetime.now().strftime("%Y-%m-%d")
        day_data = self._load_day_summary(date_str)
        if not day_data:
            return {"period": "day", "date": date_str, "requests": 0}
        return {"period": "day", **day_data}

    def _get_all_summary(self):
        """返回所有天的汇总。"""
        summary_dir = self._summary_root()
        days = []
        total = {
            "requests": 0,
            "total_input_tokens": 0,
            "total_cache_read_tokens": 0,
            "total_cache_write_tokens": 0,
            "total_output_tokens": 0,
            "total_cost": 0.0,
        }
        for filename in sorted(os.listdir(summary_dir)):
            if not filename.endswith(".json"):
                continue
            date_str = filename[:-5]
            day_data = self._load_day_summary(date_str)
            if day_data:
                days.append(day_data)
                dt = day_data["daily_total"]
                total["requests"] += dt["requests"]
                total["total_input_tokens"] += dt["total_input_tokens"]
                total["total_cache_read_tokens"] += dt["total_cache_read_tokens"]
                total["total_cache_write_tokens"] += dt["total_cache_write_tokens"]
                total["total_output_tokens"] += dt["total_output_tokens"]
                total["total_cost"] += dt.get("total_cost", 0.0)

        denom_hit = total["total_cache_read_tokens"] + total["total_input_tokens"]
        denom_cov = denom_hit + total["total_cache_write_tokens"]
        total["avg_cache_hit_rate"] = round(
            total["total_cache_read_tokens"] / denom_hit, 4
        ) if denom_hit > 0 else 0.0
        total["avg_cache_coverage"] = round(
            total["total_cache_read_tokens"] / denom_cov, 4
        ) if denom_cov > 0 else 0.0

        return {"period": "all", "days": days, "total": total}


# 全局缓存统计实例（按服务区分）
_stats = {}
_stats_lock = threading.Lock()


def get_stats(service=None, account=None, no_cost=False):
    """获取 CacheStats 实例（线程安全的懒初始化，按服务区分）。"""
    key = service or "default"
    if key not in _stats:
        with _stats_lock:
            if key not in _stats:  # 双重检查
                stats_dir = os.environ.get("CACHE_STATS_DIR", "cache_stats")
                retention = int(os.environ.get("CACHE_STATS_RETENTION_DAYS", "30"))
                _stats[key] = CacheStats(stats_dir=stats_dir, retention_days=retention,
                                         service=service, account=account, no_cost=no_cost)
    return _stats[key]


def detect_client(request, payload):
    """自动识别入口协议：anthropic（Claude Code）还是 openai（Codex）。

    先看路径（/v1/messages、/v1/responses、/chat/completions），
    再看请求体特征（Anthropic 必有 max_tokens/system，OpenAI Responses 有 input）。
    """
    path = getattr(request, "path", "") or ""
    p = path.lower()
    if "/v1/messages" in p:
        return "anthropic"
    if "/responses" in p or "/chat/completions" in p or "/completions" in p:
        return "openai"
    if isinstance(payload, dict):
        if "input" in payload and "messages" not in payload:
            return "openai"  # OpenAI Responses
        if "max_tokens" in payload and "system" in payload:
            return "anthropic"  # Anthropic Messages
        if "messages" in payload:
            msgs = payload.get("messages") or []
            # Anthropic 的 content 是 block 列表（text/tool_use/tool_result）
            if msgs and isinstance(msgs[0], dict) and isinstance(msgs[0].get("content"), list):
                return "anthropic"
            return "openai"
        if "max_tokens" in payload:
            return "anthropic"
    return "openai"  # 默认


def resolve_mode(service, request=None, payload=None):
    """确定一次请求的分派模式（claude / codex / direct）。

    api 显式声明时直接采用推导结果，不再做请求体猜测（避免误判）；
    client 显式时按旧逻辑推导；auto 时先识别入口协议，再按账号端点选转换或透传。
    返回 None 表示该组合不支持（OpenAI 客户端 + 无 OpenAI 端点的账号）。
    """
    if service.api:
        # api 已显式声明（openai-completions / openai-responses / anthropic-messages）
        return service.mode
    if service.client == "auto":
        client = detect_client(request, payload)
        if client == "anthropic":
            return "direct" if service.kind in ("anthropic", "both") else "claude"
        return "codex" if service.kind != "anthropic" else None
    # 显式 client
    if service.client == "openai":
        return "codex" if service.kind != "anthropic" else None
    # anthropic 客户端
    return "direct" if service.kind in ("anthropic", "both") else "claude"


def get_account_summary(account_id, period="day"):
    """按账号聚合其下所有服务的统计（服务级 summary 动态归并，避免双写一致性问题）。"""
    services = load_config()
    matched = [s for s in services if s.account.id == account_id]
    if not matched:
        return {"period": period, "account": account_id, "requests": 0}
    if period == "all":
        total = {
            "requests": 0, "total_input_tokens": 0, "total_cache_read_tokens": 0,
            "total_cache_write_tokens": 0, "total_output_tokens": 0, "total_cost": 0.0,
        }
        days = []
        for svc in matched:
            s = get_stats(svc.name, svc.account.id).get_summary("all")
            for d in s.get("days", []):
                for k, v in d.get("daily_total", {}).items():
                    if k in total:
                        total[k] += v
                days.append(d)
        return {"period": "all", "account": account_id, "days": days, "total": total}
    # day / hour：合并 daily_total，hours 按时间排序叠加
    agg_daily = {
        "requests": 0, "total_input_tokens": 0, "total_cache_read_tokens": 0,
        "total_cache_write_tokens": 0, "total_output_tokens": 0, "total_cost": 0.0,
    }
    hours = {}
    for svc in matched:
        s = get_stats(svc.name, svc.account.id).get_summary(period)
        daily = s.get("daily_total") if period == "day" else s
        if not daily:
            continue
        for k, v in daily.items():
            if k in agg_daily:
                agg_daily[k] += v
        for h in s.get("hours", []):
            hid = h.get("hour", "")
            if hid in hours:
                cur = hours[hid]
                cur["requests"] += h.get("requests", 0)
                cur["total_input_tokens"] += h.get("total_input_tokens", 0)
                cur["total_cache_read_tokens"] += h.get("total_cache_read_tokens", 0)
                cur["total_cache_write_tokens"] += h.get("total_cache_write_tokens", 0)
                cur["total_output_tokens"] += h.get("total_output_tokens", 0)
                cur["total_cost"] += h.get("total_cost", 0.0)
                cur["avg_cache_hit_rate"] = (
                    (cur["avg_cache_hit_rate"] + h.get("avg_cache_hit_rate", 0.0)) / 2
                )
                cur["avg_cache_coverage"] = (
                    (cur["avg_cache_coverage"] + h.get("avg_cache_coverage", 0.0)) / 2
                )
            else:
                hours[hid] = dict(h)
    agg_daily["avg_cache_hit_rate"] = (
        agg_daily["total_cache_read_tokens"]
        / (agg_daily["total_cache_read_tokens"] + agg_daily["total_input_tokens"])
        if (agg_daily["total_cache_read_tokens"] + agg_daily["total_input_tokens"]) > 0 else 0.0
    )
    agg_daily["avg_cache_coverage"] = (
        agg_daily["total_cache_read_tokens"]
        / (agg_daily["total_cache_read_tokens"] + agg_daily["total_cache_write_tokens"])
        if (agg_daily["total_cache_read_tokens"] + agg_daily["total_cache_write_tokens"]) > 0 else 0.0
    )
    return {
        "period": period, "account": account_id,
        "hours": [hours[h] for h in sorted(hours)],
        "daily_total": agg_daily,
    }


def is_cache_stats_enabled():
    """检查缓存统计是否启用（默认开启）。"""
    return os.environ.get("CACHE_STATS_ENABLED", "true").lower() in ("true", "1", "yes")


def sse_event(data, event_type=None):
    """格式化 SSE 事件。"""
    lines = []
    if event_type is None and isinstance(data, dict):
        event_type = data.get("type")
    if event_type:
        lines.append(f"event: {event_type}")
    lines.append(f"data: {json.dumps(data)}")
    lines.append("")
    return "\n".join(lines) + "\n"


def _to_int(value, default=0):
    """Best-effort conversion for provider usage fields."""
    if value is None:
        return default
    try:
        return int(value)
    except (TypeError, ValueError):
        return default


def _convert_usage(usage):
    """Convert OpenAI-compatible usage into Anthropic usage semantics."""
    usage = usage or {}
    prompt_details = usage.get("prompt_tokens_details") or {}
    input_details = usage.get("input_tokens_details") or {}

    prompt_total = _to_int(
        usage.get("prompt_tokens", usage.get("input_tokens", 0))
    )
    output_tokens = _to_int(
        usage.get("completion_tokens", usage.get("output_tokens", 0))
    )

    # DeepSeek 顶层字段：prompt_cache_hit_tokens（命中）/ prompt_cache_miss_tokens（未命中）
    # 命中部分计入缓存读，prompt_total 是全量（含命中），相减后才是真实输入。
    ds_cache_hit = _to_int(usage.get("prompt_cache_hit_tokens", 0))

    cached_tokens = _to_int(
        ds_cache_hit
        or prompt_details.get(
            "cached_tokens",
            prompt_details.get(
                "cache_read_input_tokens",
                input_details.get(
                    "cached_tokens",
                    input_details.get(
                        "cache_read_input_tokens",
                        usage.get("cache_read_input_tokens", usage.get("cached_tokens", 0)),
                    ),
                ),
            ),
        )
    )
    cache_write_tokens = _to_int(
        prompt_details.get(
            "cache_creation_input_tokens",
            prompt_details.get(
                "cache_write_tokens",
                input_details.get(
                    "cache_write_tokens",
                    input_details.get(
                        "cache_creation_input_tokens",
                        usage.get("cache_creation_input_tokens", usage.get("cache_write_tokens", 0)),
                    ),
                ),
            ),
        )
    )

    # Anthropic reports cache writes separately from ordinary input tokens.
    input_tokens = max(0, prompt_total - cached_tokens - cache_write_tokens)

    completion_details = usage.get("completion_tokens_details") or {}
    # Responses 格式的推理 token 在 output_tokens_details.reasoning_tokens
    output_details = usage.get("output_tokens_details") or {}
    reasoning_tokens = _to_int(
        completion_details.get("reasoning_tokens")
        or output_details.get("reasoning_tokens", 0)
    )

    return {
        "input_tokens": input_tokens,
        "output_tokens": output_tokens,
        "cache_creation_input_tokens": cache_write_tokens,
        "cache_read_input_tokens": cached_tokens,
        "reasoning_tokens": reasoning_tokens,
        "prompt_total": prompt_total,
    }


def _anthropic_stop_reason(finish_reason, has_tool_calls=False):
    """Map OpenAI finish_reason values to Anthropic stop_reason values."""
    if has_tool_calls or finish_reason == "tool_calls":
        return "tool_use"
    if finish_reason == "length":
        return "max_tokens"
    if finish_reason in ("stop", None, ""):
        return "end_turn"
    if finish_reason == "content_filter":
        return "stop_sequence"
    return finish_reason


def _extract_text(content):
    """将 Anthropic content blocks 转为纯文本字符串。"""
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        parts = []
        for block in content:
            if isinstance(block, str):
                parts.append(block)
            elif isinstance(block, dict):
                if block.get("type") == "text":
                    parts.append(block.get("text", ""))
                elif block.get("type") == "tool_result":
                    content_val = block.get("content", "")
                    if isinstance(content_val, str):
                        parts.append(content_val)
                    elif isinstance(content_val, list):
                        for cb in content_val:
                            if isinstance(cb, dict) and cb.get("type") == "text":
                                parts.append(cb.get("text", ""))
            else:
                parts.append(str(block))
        return "\n".join(parts)
    return str(content)


def convert_tool_input(input_schema):
    """将 Anthropic input_schema 转为 OpenAI function parameters 格式。"""
    if not isinstance(input_schema, dict):
        return input_schema
    params = dict(input_schema)
    if "type" not in params:
        params["type"] = "object"
    return params


def _strip_cache_control(obj):
    """递归移除 cache_control 字段（DashScope 不支持）。"""
    if isinstance(obj, dict):
        return {k: _strip_cache_control(v) for k, v in obj.items() if k != "cache_control"}
    elif isinstance(obj, list):
        return [_strip_cache_control(item) for item in obj]
    return obj


def normalize_roles(payload):
    """将 OpenAI 特有的 developer 角色规范化为 system（chat messages 与 responses input 均处理）。

    多数非 OpenAI 上游（DeepSeek / Kimi / Qwen 等）的角色枚举不含 developer（DeepSeek
    只认 system / user / assistant / tool 等），透传前统一降级为 system——与
    _responses_to_chat 已有的规范化一致；system 是所有上游都接受的通用角色，且不影响
    reasoning_effort / thinking 等其它字段。返回是否发生修改（未修改时透传保持字节一致）。
    """
    changed = False
    for key in ("messages", "input"):
        items = payload.get(key)
        if not isinstance(items, list):
            continue
        for item in items:
            if isinstance(item, dict) and item.get("role") == "developer":
                item["role"] = "system"
                changed = True
    return changed


def _responses_content_to_text(content):
    """将 Responses API 消息 content parts 提取为纯文本。"""
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        parts = []
        for p in content:
            if isinstance(p, str):
                parts.append(p)
            elif isinstance(p, dict):
                t = p.get("type")
                if "text" in p and isinstance(p.get("text"), str):
                    parts.append(p.get("text"))
                elif t in ("input_text", "output_text"):
                    parts.append(p.get("text", ""))
        return "\n".join(parts)
    return ""


def _responses_to_chat(req, service):
    """将 OpenAI Responses API 请求转成 Chat Completions 请求。

    兼容两种入参格式（Codex / pi 等客户端可能发任一种）：
    - Responses 格式：req 含 input（字符串或 item 数组）
    - Chat Completions 格式：req 含 messages —— 直通，仅做 role 规范化
    """
    messages = []
    pending_calls = []  # 连续 function_call 项合并为一条 assistant 消息

    def flush_calls():
        if pending_calls:
            messages.append({
                "role": "assistant",
                "content": None,
                "tool_calls": list(pending_calls),
            })
            del pending_calls[:]

    if not req.get("input"):
        # Chat Completions 直通：整包透传（保留 stream/tools/stop 等全部字段），仅替换 model、规范化 role
        chat = {k: v for k, v in req.items() if k != "model"}
        msgs = []
        for msg in chat.get("messages", []):
            if not isinstance(msg, dict):
                continue
            m = dict(msg)
            if m.get("role") == "developer":
                m["role"] = "system"
            msgs.append(m)
        chat["messages"] = msgs
        # 模型覆盖开关：默认用服务配置的 model；override_model=false 时透传客户端模型名（缺省回退服务配置）
        if service.override_model:
            chat["model"] = service.model
        else:
            chat["model"] = req.get("model") or service.model
        if not chat.get("max_tokens") and not chat.get("max_output_tokens"):
            # 没带 max_tokens 时用服务默认（不做封顶，透传）
            chat["max_tokens"] = service.max_tokens
        return chat
    else:
        raw_input = req.get("input", [])
        if isinstance(raw_input, str):
            # Responses 规范允许 input 为纯字符串
            raw_input = [{"role": "user", "content": raw_input}]
        for item in raw_input:
            if not isinstance(item, dict):
                continue
            itype = item.get("type")
            if itype == "function_call":
                pending_calls.append({
                    "id": item.get("call_id") or item.get("id") or "",
                    "type": "function",
                    "function": {
                        "name": item.get("name", ""),
                        "arguments": item.get("arguments", ""),
                    },
                })
            elif itype == "function_call_output":
                flush_calls()
                messages.append({
                    "role": "tool",
                    "tool_call_id": item.get("call_id") or item.get("id") or "",
                    "content": item.get("output", ""),
                })
            elif "role" in item:
                flush_calls()
                role = item.get("role")
                if role == "developer":
                    role = "system"
                messages.append({"role": role, "content": _responses_content_to_text(item.get("content", ""))})
    flush_calls()

    instructions = req.get("instructions", "")
    if instructions:
        if messages and messages[0].get("role") == "system":
            # input 已含 system 角色消息时合并，避免产生两条 system
            prev = messages[0].get("content", "") or ""
            messages[0]["content"] = (instructions + "\n\n" + prev) if prev else instructions
        else:
            messages.insert(0, {"role": "system", "content": instructions})

    if service.override_model:
        chat_model = service.model
    else:
        chat_model = req.get("model") or service.model
    chat = {
        "model": chat_model,
        "messages": messages,
        "stream": req.get("stream", False),
    }
    if "max_output_tokens" in req:
        chat["max_tokens"] = req["max_output_tokens"]
    elif "max_tokens" in req:
        chat["max_tokens"] = req["max_tokens"]
    else:
        chat["max_tokens"] = service.max_tokens
    for k in ("temperature", "top_p", "stream_options", "seed", "parallel_tool_calls"):
        if k in req:
            chat[k] = req[k]
    if req.get("stream") and "stream_options" not in chat:
        chat["stream_options"] = {"include_usage": True}

    tools = req.get("tools", [])
    if tools:
        chat_tools = []
        for t in tools:
            if isinstance(t, dict) and t.get("type") == "function":
                chat_tools.append({
                    "type": "function",
                    "function": {
                        "name": t.get("name", ""),
                        "description": t.get("description", ""),
                        "parameters": t.get("parameters", {"type": "object"}) or {"type": "object"},
                        "strict": t.get("strict", False),
                    },
                })
        if chat_tools:
            chat["tools"] = chat_tools

    tool_choice = req.get("tool_choice")
    if tool_choice:
        if isinstance(tool_choice, str):
            chat["tool_choice"] = tool_choice
        elif isinstance(tool_choice, dict):
            chat["tool_choice"] = {
                "type": "function",
                "function": {"name": tool_choice.get("name", "")},
            }
    # 思考深度：Responses reasoning（effort 档位）→ 上游 Chat 参数
    _apply_reasoning_to_chat(chat, req, service)
    return chat


def _chat_usage_to_responses(usage):
    """将 Chat Completions usage 转成 Responses API usage 格式。"""
    usage = usage or {}
    prompt = _to_int(usage.get("prompt_tokens", usage.get("input_tokens", 0)))
    completion = _to_int(usage.get("completion_tokens", usage.get("output_tokens", 0)))
    # 兼容 DeepSeek 顶层缓存字段与 Responses 格式的 details 嵌套
    cached = _to_int(
        usage.get("prompt_cache_hit_tokens", 0)
        or (usage.get("prompt_tokens_details") or {}).get("cached_tokens", 0)
        or (usage.get("input_tokens_details") or {}).get("cached_tokens", 0)
    )
    reasoning = _to_int(
        (usage.get("completion_tokens_details") or {}).get("reasoning_tokens", 0)
        or (usage.get("output_tokens_details") or {}).get("reasoning_tokens", 0)
    )
    return {
        "input_tokens": prompt,
        "input_tokens_details": {"cached_tokens": cached},
        "output_tokens": completion,
        "output_tokens_details": {"reasoning_tokens": reasoning},
        "total_tokens": prompt + completion,
    }


def _chat_to_responses_json(data, model):
    """将 Chat Completions 非流式响应转成 Responses API 响应。"""
    resp_id = "resp_" + uuid.uuid4().hex[:24]
    created = int(time.time())
    output = []
    choice = (data.get("choices") or [{}])[0]
    message = choice.get("message") or {}
    # 文本输出
    text = message.get("content") or ""
    if isinstance(text, list):
        # 上游 content 为 block 列表时转为纯文本，避免 Responses 结构非法
        text = _responses_content_to_text(text)
    if text:
        output.append({
            "id": f"msg_{len(output)}",
            "type": "message",
            "role": "assistant",
            "status": "completed",
            "content": [{"type": "output_text", "text": text, "annotations": []}],
        })
    # 推理内容（reasoning_content -> Responses reasoning item）
    reasoning = message.get("reasoning_content") or ""
    if reasoning:
        output.append({
            "id": f"reasoning_{len(output)}",
            "type": "reasoning",
            "status": "completed",
            "summary": [{"type": "summary_text", "text": reasoning}],
            "content": [{"type": "reasoning_text", "text": reasoning}],
        })
    # 函数调用
    for tc in message.get("tool_calls") or []:
        fn = tc.get("function") or {}
        output.append({
            "id": f"fc_{len(output)}",
            "type": "function_call",
            "status": "completed",
            "name": fn.get("name", ""),
            "call_id": tc.get("id", ""),
            "arguments": fn.get("arguments", ""),
        })
    return {
        "id": resp_id,
        "object": "response",
        "created_at": created,
        "status": "completed",
        "model": model or data.get("model", ""),
        "output": output,
        "parallel_tool_calls": True,
        "tools": [],
        "usage": _chat_usage_to_responses(data.get("usage")),
    }


class _ResponsesStreamTranslator:
    """将 Chat Completions 流式 SSE 翻译为 Responses API 流式 SSE。"""

    def __init__(self, model):
        self.model = model
        self.response_id = "resp_" + uuid.uuid4().hex[:24]
        self.created_at = int(time.time())
        self.output_index = 0
        self._emitted_created = False
        self._finished = False      # 内容 done 事件已发射（幂等）
        self._completed = False     # response.completed 已发射（幂等）
        self._msg_item_id = None
        self._msg_output_index = 0
        self._msg_delivered = False
        self._text = ""
        self._tool_states = {}  # index -> state
        self._tool_order = []
        self._delivered_tool = set()
        self._output_sequence = []  # 按交付(output_index)顺序记录 ('message'|'tool'|'reasoning', key)
        self._reasoning_item_id = None
        self._reasoning_output_index = 0
        self._reasoning_delivered = False
        self._reasoning_text = ""
        self.usage = None

    def _base_response(self):
        return {
            "id": self.response_id,
            "object": "response",
            "created_at": self.created_at,
            "status": "in_progress",
            "model": self.model,
            "output": [],
            "parallel_tool_calls": True,
            "tools": [],
            "usage": self.usage,
        }

    def _ensure_created(self, events):
        if not self._emitted_created:
            self._emitted_created = True
            events.append({"type": "response.created", "response": self._base_response()})

    def _deliver_message(self, events):
        if self._msg_delivered:
            return
        self._msg_delivered = True
        self._msg_item_id = f"msg_{self.output_index}"
        self._msg_output_index = self.output_index
        self.output_index += 1
        self._output_sequence.append(("message", None))
        item = {
            "id": self._msg_item_id,
            "type": "message",
            "role": "assistant",
            "status": "in_progress",
            "content": [],
        }
        events.append({"type": "response.output_item.added",
                       "output_index": self._msg_output_index, "item": item})
        events.append({"type": "response.content_part.added",
                       "item_id": self._msg_item_id,
                       "output_index": self._msg_output_index,
                       "content_index": 0,
                       "part": {"type": "output_text", "text": "", "annotations": []}})

    def _deliver_reasoning(self, events):
        """交付推理 item（reasoning_content -> reasoning）。"""
        if self._reasoning_delivered:
            return
        self._reasoning_delivered = True
        self._reasoning_item_id = f"rs_{self.output_index}"
        self._reasoning_output_index = self.output_index
        self.output_index += 1
        self._output_sequence.append(("reasoning", None))
        item = {
            "id": self._reasoning_item_id,
            "type": "reasoning",
            "status": "in_progress",
            "summary": [],
            "content": [],
        }
        events.append({"type": "response.output_item.added",
                       "output_index": self._reasoning_output_index, "item": item})

    def _deliver_tool(self, idx, events):
        if idx in self._delivered_tool:
            return
        self._delivered_tool.add(idx)
        state = self._tool_states[idx]
        state["output_index"] = self.output_index
        state["item_id"] = f"fc_{self.output_index}"
        self.output_index += 1
        self._output_sequence.append(("tool", idx))
        item = {
            "id": state["item_id"],
            "type": "function_call",
            "status": "in_progress",
            "name": state["name"],
            "call_id": state["id"],
            "arguments": "",
        }
        events.append({"type": "response.output_item.added",
                       "output_index": state["output_index"], "item": item})

    def translate(self, data):
        """处理一个 chat chunk，返回 Responses 事件 dict 列表。"""
        events = []
        choices = data.get("choices") or []
        if data.get("usage"):
            self.usage = _chat_usage_to_responses(data.get("usage"))
        if not choices:
            return events
        delta = choices[0].get("delta") or {}

        content = delta.get("content")
        reasoning = delta.get("reasoning_content")
        # 推理内容 -> reasoning item（保持与文本输出并行）
        if isinstance(reasoning, str) and reasoning:
            self._ensure_created(events)
            self._deliver_reasoning(events)
            self._reasoning_text += reasoning
            events.append({
                "type": "response.reasoning_summary_text.delta",
                "item_id": self._reasoning_item_id,
                "output_index": self._reasoning_output_index,
                "delta": reasoning,
            })

        if isinstance(content, str) and content:
            self._ensure_created(events)
            self._deliver_message(events)
            self._text += content
            events.append({
                "type": "response.output_text.delta",
                "item_id": self._msg_item_id,
                "output_index": self._msg_output_index,
                "content_index": 0,
                "delta": content,
            })

        for tc in delta.get("tool_calls") or []:
            idx = tc.get("index", 0)
            fn = tc.get("function") or {}
            state = self._tool_states.get(idx)
            if state is None:
                state = {"id": tc.get("id", ""), "name": fn.get("name", ""), "arguments": ""}
                self._tool_states[idx] = state
                self._tool_order.append(idx)
            else:
                if fn.get("name"):
                    state["name"] = fn["name"]
                if tc.get("id"):
                    state["id"] = tc["id"]
            if fn.get("arguments"):
                self._ensure_created(events)
                self._deliver_tool(idx, events)
                state["arguments"] += fn["arguments"]
                events.append({
                    "type": "response.function_call_arguments.delta",
                    "item_id": state["item_id"],
                    "output_index": state["output_index"],
                    "delta": fn["arguments"],
                })

        finish_reason = choices[0].get("finish_reason")
        if finish_reason:
            # 只收尾内容块（done 事件）；response.completed 延迟到流结束（[DONE]/EOF）
            # 由外层调用 complete() 发射，确保 usage 尾块（标准顺序在 finish_reason 之后）
            # 已到达——否则 completed 的 usage 会是 None（Codex 计费错乱）。
            self._close_items(events)
        return events

    def _close_items(self, events):
        """收尾内容块：done 事件（推理/消息/工具），幂等；不发射 response.completed。"""
        if self._finished:
            return
        self._finished = True
        if not self._emitted_created:
            self._ensure_created(events)
        # 关闭推理内容
        if self._reasoning_delivered:
            events.append({"type": "response.reasoning_summary_text.done",
                           "item_id": self._reasoning_item_id,
                           "output_index": self._reasoning_output_index,
                           "text": self._reasoning_text})
            events.append({"type": "response.output_item.done",
                           "output_index": self._reasoning_output_index, "item": {
                               "id": self._reasoning_item_id, "type": "reasoning",
                               "status": "completed",
                               "summary": [{"type": "summary_text", "text": self._reasoning_text}],
                               "content": [{"type": "reasoning_text", "text": self._reasoning_text}]}})
        # 关闭文本消息
        if self._msg_delivered:
            events.append({"type": "response.output_text.done",
                           "item_id": self._msg_item_id,
                           "output_index": self._msg_output_index,
                           "content_index": 0, "text": self._text})
            events.append({"type": "response.content_part.done",
                           "item_id": self._msg_item_id,
                           "output_index": self._msg_output_index,
                           "content_index": 0,
                           "part": {"type": "output_text", "text": self._text, "annotations": []}})
            events.append({"type": "response.output_item.done",
                           "output_index": self._msg_output_index, "item": {
                               "id": self._msg_item_id, "type": "message",
                               "role": "assistant", "status": "completed",
                               "content": [{"type": "output_text", "text": self._text, "annotations": []}]}})
        # 关闭工具调用
        for idx in self._tool_order:
            state = self._tool_states[idx]
            if idx not in self._delivered_tool:
                self._deliver_tool(idx, events)
            events.append({"type": "response.function_call_arguments.done",
                           "item_id": state["item_id"],
                           "output_index": state["output_index"],
                           "arguments": state["arguments"]})
            events.append({"type": "response.output_item.done",
                           "output_index": state["output_index"], "item": {
                               "id": state["item_id"], "type": "function_call",
                               "status": "completed", "name": state["name"],
                               "call_id": state["id"], "arguments": state["arguments"]}})

    def complete(self, events):
        """发射 response.completed（含最终 usage），幂等。流结束（[DONE]/EOF）时调用。"""
        if self._completed:
            return
        self._close_items(events)
        self._completed = True
        events.append({"type": "response.completed", "response": self.assemble()})

    def _finish(self, events):
        """完整收尾：done 事件 + response.completed（幂等，供流结束统一调用）。"""
        self._close_items(events)
        self.complete(events)

    def assemble(self):
        output = []
        for kind, key in self._output_sequence:
            if kind == "message":
                output.append({"id": self._msg_item_id, "type": "message", "role": "assistant",
                               "status": "completed",
                               "content": [{"type": "output_text", "text": self._text, "annotations": []}]})
            elif kind == "reasoning":
                output.append({"id": self._reasoning_item_id, "type": "reasoning",
                               "status": "completed",
                               "summary": [{"type": "summary_text", "text": self._reasoning_text}],
                               "content": [{"type": "reasoning_text", "text": self._reasoning_text}]})
            else:
                state = self._tool_states[key]
                output.append({"id": state["item_id"], "type": "function_call",
                               "status": "completed", "name": state["name"],
                               "call_id": state["id"], "arguments": state["arguments"]})
        resp = self._base_response()
        resp["status"] = "completed"
        resp["output"] = output
        resp["usage"] = self.usage
        return resp


def _tool_choice_any(openai_tools):
    """Anthropic tool_choice='any'（必须调用）→ OpenAI：单工具时绑定该工具，多工具时 required。"""
    names = [
        t.get("function", {}).get("name", "")
        for t in (openai_tools or [])
        if isinstance(t, dict) and isinstance(t.get("function"), dict)
    ]
    names = [n for n in names if n]
    if len(names) == 1:
        return {"type": "function", "function": {"name": names[0]}}
    return "required"


# ---------------------------------------------------------------------------
# 思考深度透传：Anthropic thinking / OpenAI Responses reasoning → 上游参数
#
# 入口 × 上游矩阵：
#   anthropic-messages 入口 → OpenAI Chat 上游    : _apply_thinking_to_chat
#   openai-responses 入口 → OpenAI Chat 上游      : _apply_reasoning_to_chat
#   openai-completions 入口 → Chat 上游           : 整包透传（reasoning_effort 等原样保留）
#   Responses 入口 → Responses 上游 / anthropic 入口 → Anthropic 上游（direct）：整包透传
# 响应方向（上游思考内容 → 客户端）已有完整转换（thinking 块 / reasoning item），此处只处理请求方向。
# ---------------------------------------------------------------------------

def _budget_to_effort(budget):
    """Anthropic budget_tokens（token 预算）→ OpenAI reasoning_effort 档位（近似映射）。

    Anthropic 的深度是 token 预算，OpenAI 系是 low/medium/high 档位，两者不同构，
    只能做阈值近似：≥8192 → high，≥2048 → medium，其余 → low。
    """
    try:
        b = int(budget or 0)
    except (TypeError, ValueError):
        return None
    if b <= 0:
        return None
    if b >= 8192:
        return "high"
    if b >= 2048:
        return "medium"
    return "low"


def _infer_thinking_style(service):
    """auto 模式下按上游 URL / 模型名推断思考参数风格。

    - dashscope / qwen        → enable_thinking（布尔开关）
    - deepseek / kimi / moonshot → thinking（Anthropic 风格对象，可带 budget_tokens）
    - 其他 OpenAI 兼容网关    → reasoning_effort（OpenAI 标准档位）
    """
    url = (service.account.openai_url or "").lower()
    model = (service.model or "").lower()
    if "dashscope" in url or "qwen" in url or "qwen" in model:
        return "enable_thinking"
    if "deepseek" in url or "moonshot" in url or "kimi" in url or "kimi" in model:
        return "passthrough"
    return "effort"


def _apply_thinking_to_chat(chat, thinking, service):
    """Anthropic Messages thinking 配置 → OpenAI Chat 请求参数（按服务 thinking_mode）。"""
    if service.thinking_mode == "none" or not thinking or not isinstance(thinking, dict):
        return
    mode = service.thinking_mode
    if mode == "auto":
        mode = _infer_thinking_style(service)
    enabled = thinking.get("type") != "disabled"
    if mode == "passthrough":
        # 上游原生支持 Anthropic 风格 thinking（DeepSeek V3.2 / Kimi K2 / 兼容网关）：
        # 原样保留 type 与 budget_tokens（Kimi 支持 budget 控制深度）
        out = {"type": thinking.get("type", "enabled")}
        if enabled and thinking.get("budget_tokens"):
            out["budget_tokens"] = thinking["budget_tokens"]
        chat["thinking"] = out
    elif mode == "enable_thinking":
        # DashScope / Qwen 兼容模式：布尔开关
        chat["enable_thinking"] = enabled
    elif mode == "effort":
        # OpenAI 标准档位：budget → low/medium/high；enabled 无预算时用 medium 兜底
        effort = _budget_to_effort(thinking.get("budget_tokens")) if enabled else None
        if enabled and not effort:
            effort = "medium"
        if effort:
            chat["reasoning_effort"] = effort
        # disabled 时 OpenAI 系无关闭语义，忽略（由模型默认决定）


def _apply_reasoning_to_chat(chat, req, service):
    """OpenAI Responses reasoning（effort 档位）→ OpenAI Chat 请求参数（按服务 thinking_mode）。

    兼容两种入参：Responses 的 reasoning: {effort} 对象，或顶层 reasoning_effort 标量。
    """
    if service.thinking_mode == "none":
        return
    reasoning = req.get("reasoning") or {}
    effort = reasoning.get("effort") if isinstance(reasoning, dict) else None
    if not effort:
        effort = req.get("reasoning_effort")
    if not effort:
        return
    mode = service.thinking_mode
    if mode == "auto":
        mode = _infer_thinking_style(service)
    if mode == "effort":
        chat["reasoning_effort"] = effort
    elif mode == "passthrough":
        # Responses 无 token 预算概念，effort 存在即开启思考（深度由上游默认）
        chat["thinking"] = {"type": "enabled"}
    elif mode == "enable_thinking":
        chat["enable_thinking"] = True


def convert_request(req, service):
    """将 Anthropic Messages 格式转为 OpenAI chat completions 格式。"""
    raw_messages = list(req.get("messages", []))

    messages = []
    for msg in raw_messages:
        role = msg.get("role", "user")
        content = msg.get("content", "")

        if isinstance(content, list):
            # 检查是否包含 tool_result blocks
            tool_results = [b for b in content if isinstance(b, dict) and b.get("type") == "tool_result"]
            if tool_results:
                # 转换为 OpenAI tool 消息格式
                # 与 tool_result 交错的文本块按出现顺序冲刷为 user 消息，保持交错顺序
                orphan_text_parts = []
                for block in content:
                    if isinstance(block, dict) and block.get("type") == "tool_result":
                        if orphan_text_parts:
                            messages.append({
                                "role": "user",
                                "content": "\n".join(orphan_text_parts),
                            })
                            orphan_text_parts = []
                        tool_id = block.get("tool_use_id", block.get("id", ""))
                        if not tool_id:
                            # 缺 tool_use_id 无法形成合法 tool 消息（上游会 400）
                            logger.warning("tool_result 块缺少 tool_use_id，已跳过")
                            continue
                        content_val = block.get("content", "")
                        text = _extract_text(content_val)
                        messages.append({
                            "role": "tool",
                            "tool_call_id": tool_id,
                            "content": text,
                        })
                    elif isinstance(block, dict) and block.get("type") == "text":
                        # 与 tool_result 同行的文本块没有 tool_use_id，
                        # 收集后作为 user 消息追加，避免生成非法的空 tool_call_id
                        orphan_text_parts.append(block.get("text", ""))
                if orphan_text_parts:
                    messages.append({
                        "role": "user",
                        "content": "\n".join(orphan_text_parts),
                    })
                continue
            # 检查 assistant 消息是否包含 tool_use
            if role == "assistant":
                tool_uses = [b for b in content if isinstance(b, dict) and b.get("type") == "tool_use"]
                if tool_uses:
                    text_parts = []
                    tool_calls = []
                    for block in content:
                        if isinstance(block, dict) and block.get("type") == "text":
                            text_parts.append(block.get("text", ""))
                        elif isinstance(block, dict) and block.get("type") == "tool_use":
                            tc = {
                                "id": block.get("id", ""),
                                "type": "function",
                                "function": {
                                    "name": block.get("name", ""),
                                    "arguments": json.dumps(block.get("input", {})),
                                },
                            }
                            tool_calls.append(tc)
                    oai_msg = {"role": "assistant", "content": None}
                    if tool_calls:
                        oai_msg["tool_calls"] = tool_calls
                    if text_parts:
                        oai_msg["content"] = "\n".join(text_parts)
                    messages.append(oai_msg)
                    continue

        # 普通文本消息 - 转为纯文本（DashScope 不支持 content blocks 格式）
        text = _extract_text(content)
        if not text:
            # 纯 thinking 块等空 content 消息，跳过（部分上游拒绝空 content）
            continue
        messages.append({
            "role": role,
            "content": text,
        })

    system = req.get("system", "")
    if system:
        # 转为纯文本（DashScope 不支持 content blocks 格式）
        system_content = _extract_text(system)
        messages.insert(0, {"role": "system", "content": system_content})

    is_stream = req.get("stream", False)
    # 模型覆盖开关：默认用服务配置的 model 覆盖客户端请求的模型；
    # override_model=false 时忠实透传客户端模型名（缺失时回退服务配置）
    client_model = req.get("model", "")
    if service.override_model:
        model = service.model
    else:
        model = client_model or service.model
    if client_model and client_model != model:
        logger.debug(f"[MODEL] client requested {client_model} -> use {model} (override={service.override_model})")
    openai_req = {
        "model": model,
        "messages": messages,
        "max_tokens": req.get("max_tokens", service.max_tokens),
        "stream": is_stream,
    }
    if is_stream:
        openai_req["stream_options"] = {"include_usage": True}

    # 转发采样参数（子 agent 可能设置特定 temperature）
    if "temperature" in req:
        openai_req["temperature"] = req["temperature"]
    if "top_p" in req:
        openai_req["top_p"] = req["top_p"]

    # 处理 thinking 参数（Claude Code 的扩展思考功能）：
    # 按服务 thinking_mode 映射到上游（auto 推断 / passthrough 原样 / effort 档位 / enable_thinking 布尔）
    if "thinking" in req:
        _apply_thinking_to_chat(openai_req, req["thinking"], service)

    # 转换 tools: Anthropic -> OpenAI
    tools = req.get("tools", [])
    openai_tools = []
    if tools:
        for tool in tools:
            if isinstance(tool, dict):
                name = tool.get("name", "")
                description = tool.get("description", "")
                input_schema = tool.get("input_schema", {})
                openai_tools.append({
                    "type": "function",
                    "function": {
                        "name": name,
                        "description": description,
                        "parameters": convert_tool_input(input_schema),
                        "strict": False,
                    },
                })
        openai_req["tools"] = openai_tools

    # 转换 tool_choice: Anthropic -> OpenAI
    tool_choice = req.get("tool_choice")
    if tool_choice:
        if isinstance(tool_choice, str):
            if tool_choice == "any":
                openai_req["tool_choice"] = _tool_choice_any(openai_tools)
            elif tool_choice in ("auto", "none"):
                openai_req["tool_choice"] = tool_choice
        elif isinstance(tool_choice, dict):
            tool_type = tool_choice.get("type", "")
            if tool_type == "tool":
                openai_req["tool_choice"] = {
                    "type": "function",
                    "function": {"name": tool_choice.get("name", "")},
                }
            elif tool_type == "any":
                openai_req["tool_choice"] = _tool_choice_any(openai_tools)
            elif tool_type in ("auto", "none"):
                openai_req["tool_choice"] = tool_type

    return openai_req


# 不再使用 raw socket，统一用 urllib


if __name__ == "__main__":
    import sys as _sys
    print("proxy.py 现为核心库（协议转换 / 配置 / 统计），不包含代理引擎。")
    print("请运行：python proxy_async.py [--service <名称|端口>]")
    _sys.exit(1)
