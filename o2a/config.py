"""o2a-proxy 配置模型与加载：账号（Account）、服务（Service）、config.json / auth.json 解析。

从原 proxy.py 拆出，逻辑逐字保留。
"""

import json
import os
import secrets

from .base import (
    API_KEY,
    LISTEN_HOST,
    LISTEN_PORT,
    PROXY,
    PROXY_MAX_TOKENS,
    PROXY_MODEL,
    TARGET_URL,
    _auth_file_path,
    _config_file_path,
    _normalize_openai_url,
    logger,
)


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

    def __init__(self, id, name, api_key, openai_url="", anthropic_url="", api="",
                 quota_source="auto", quota=None):
        self.id = id
        self.name = name
        self.api_key = api_key or ""
        self.openai_url = _normalize_openai_url(openai_url)
        self.anthropic_url = (anthropic_url or "").strip()
        self.api = (api or "").strip()
        # §8 额度来源：auto（按端点域名嗅探，嗅探不到 → local）| openrouter |
        # anthropic | codex | zen | local | manual | none
        self.quota_source = (quota_source or "auto").strip()
        # manual 适配器的手填额度（冷启动兜底）：{"limit": 200, "unit": "requests",
        # "period": "month"}；unit: requests | tokens | usd；period: day | week | month
        self.quota = quota if isinstance(quota, dict) else None

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


def new_service_id() -> str:
    """生成稳定服务 id：svc-<8 位十六进制随机>。

    随机而非递增：多份配置合并 / 导入时无需全局协调（优化方案 §2.2）。"""
    return "svc-" + secrets.token_hex(4)


class Service:
    """单个服务（接入点）：独立端口 + 引用账号 + 客户端类型 + 入口协议。

    id：稳定身份（svc-<8hex>），生成后终生不变；comment 仅为显示名，可随意改。
    order / enabled / autostart：显式排序 / 停用（保留配置不装载）/ 自启标记。

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
                 pricing="", auth_token="", id="", order=0, enabled=True, autostart=False,
                 models=None, models_map=None, model_policy="clamp"):
        self.id = id or new_service_id()
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
        # 计价模式（§2.3）："" = 按 pricing.json 计价；"none" = 订阅制兼容别名；
        # 对象形式 {"mode": "token"|"subscription"|"free", ...}
        if isinstance(pricing, dict):
            self.pricing = pricing
        else:
            self.pricing = str(pricing or "").strip()
        self.pricing_mode, self.pricing_extra = normalize_pricing_value(self.pricing)
        # 客户端凭证（接入层鉴权）：非空时校验请求头 Authorization: Bearer <token> / x-api-key；
        # 为空时不校验（保持历史行为），引擎启动时会打警告
        self.auth_token = (auth_token or "").strip()
        self.order = int(order) if order is not None else 0
        self.enabled = enabled is not False and enabled != "false"
        self.autostart = autostart is True or autostart == "true"
        # §6 服务级模型白名单与别名映射：
        # models：可见模型白名单（对外名）；空 = 不限制（历史行为，逐字节兼容）
        # models_map：{对外名: 上游名} 别名映射；命中后按上游名转发，统计仍记对外名
        # model_policy：白名单外请求处理 —— clamp 强转主模型（默认）/ reject 400 / passthrough 透传
        self.models = [str(m) for m in (models or []) if str(m).strip()]
        self.models_map = {str(k): str(v) for k, v in (models_map or {}).items() if str(v).strip()}
        self.model_policy = model_policy if model_policy in MODEL_POLICIES else "clamp"
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
                    self.upstream_api, self.thinking_mode, self.pricing, self.auth_token,
                    models=self.models, models_map=self.models_map,
                    model_policy=self.model_policy)
        s.id = self.id
        s.order = self.order
        s.enabled = self.enabled
        s.autostart = self.autostart
        s._mode_override = mode
        return s

    @property
    def reverse_models_map(self):
        """{上游名: 对外名}（统计按对外名记录用）。"""
        return {v: k for k, v in self.models_map.items() if v}


_OPENAI_API_VALUES = ("", "anthropic-messages", "openai-completions", "openai-responses")
_UPSTREAM_API_VALUES = ("openai-completions", "openai-responses")
# 思考深度透传模式（服务级 thinking_mode）：
# - auto：按上游 URL/模型名推断（dashscope/qwen → enable_thinking；deepseek/kimi → thinking；其他 → effort）
# - passthrough：Anthropic 风格 thinking 对象原样透传（DeepSeek V3.2 / Kimi K2 / 兼容网关）
# - effort：映射为 OpenAI reasoning_effort 档位（budget_tokens → low/medium/high）
# - enable_thinking：映射为布尔开关（DashScope/Qwen 兼容模式）
# - none：不透传（保持默认模型行为）
_THINKING_MODES = ("auto", "passthrough", "effort", "enable_thinking", "none")

# 服务级模型策略（§6 模型白名单）：白名单外的请求如何处理
MODEL_POLICIES = ("clamp", "reject", "passthrough")

# §2.3 pricing 字段升级："" | "none" | {"mode": "token"|"subscription"|"free", ...}
PRICING_MODES = ("token", "subscription", "free")


def normalize_pricing_value(raw):
    """归一化 services[].pricing → ("token"|"subscription"|"free", 附加 dict|None)。

    - ""（缺省）→ token：按 pricing.json 计价（历史行为）
    - "none" → subscription 的兼容别名（历史行为，语义与现状逐字节一致）
    - dict → mode 必填合法；可附 plan / quota_source 等（衔接 §7.2/§8）
    非法值回退 token 并警告。"""
    if isinstance(raw, dict):
        mode = raw.get("mode", "token")
        if mode not in PRICING_MODES:
            logger.warning(f"[config] pricing.mode '{mode}' 非法，回退 token")
            return ("token", None)
        extra = {k: v for k, v in raw.items() if k != "mode"}
        return (mode, extra or None)
    s = str(raw or "").strip()
    if s == "none":
        return ("subscription", None)
    if s == "":
        return ("token", None)
    logger.warning(f"[config] pricing '{s}' 非法，回退 token（仅支持 none / 对象形式）")
    return ("token", None)


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


def _ensure_service_ids(config_path, config):
    """惰性写回：为缺失 id 的服务生成稳定 id 并写回 config.json（优化方案 §2.2）。

    仅在解析成功且确有缺失时写一次；格式化采用 2 空格缩进 + ensure_ascii=False。
    写回前备份为 config.json.bak（同目录，覆盖旧备份）。
    """
    services = config.get("services")
    if not isinstance(services, list):
        return
    seen = set()
    missing = False
    for svc in services:
        if not isinstance(svc, dict):
            continue
        sid = str(svc.get("id") or "").strip()
        if not sid:
            missing = True
        elif sid in seen:
            missing = True  # 重复 id：保留首个，后续重新生成
        else:
            seen.add(sid)
    if not missing:
        return
    assigned = set()
    for svc in services:
        if not isinstance(svc, dict):
            continue
        sid = str(svc.get("id") or "").strip()
        if not sid or sid in assigned:
            sid = new_service_id()
            while sid in assigned:
                sid = new_service_id()
            svc["id"] = sid
        assigned.add(sid)
    backup = config_path + ".bak"
    try:
        if os.path.exists(config_path):
            with open(config_path, encoding="utf-8") as f:
                raw = f.read()
            with open(backup, "w", encoding="utf-8") as f:
                f.write(raw)  # 备份原始内容
        with open(config_path, "w", encoding="utf-8") as f:
            json.dump(config, f, ensure_ascii=False, indent=2)
        logger.info("[config] 已为缺失 id 的服务生成稳定 id 并写回 config.json（备份: %s）", backup)
    except OSError as e:
        logger.warning("[config] 服务 id 写回失败（不影响本次运行）: %s", e)


def load_config():
    """从 config.json 读取账号与服务列表；文件不存在时回退到环境变量（单服务）。

    支持三种结构（向后兼容）：
    - 新结构：accounts[]（账号）+ services[].account（引用 id）+ client
    - 旧结构：services[] 内嵌 openai_base_url/openai_api_key —— 自动迁移为账号
    - 密钥分离：auth.json 按账号 id/name 提供 api_key，优先于 config.json 内嵌
    - 接入鉴权：services[].auth_token 覆盖顶层 auth_token（全局兜底）
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
            # 惰性写回：为缺失 id 的服务生成稳定 id（§2 服务身份 id 化）
            _ensure_service_ids(config_path, config)
            # 全局缓存统计设置（供 get_stats / is_cache_stats_enabled 读取）
            os.environ.setdefault("CACHE_STATS_ENABLED",
                                  str(config.get("cache_stats_enabled", True)).lower())
            os.environ.setdefault("CACHE_STATS_DIR",
                                  config.get("cache_stats_dir", "data/cache_stats"))
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
                    quota_source=a.get("quota_source", "auto"),
                    quota=a.get("quota"),
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
                if isinstance(pricing, dict):
                    # §2.3 对象形式：归一化校验（非法 mode 回退 token），原样保留写回
                    normalize_pricing_value(pricing)
                elif pricing not in ("", "none"):
                    logger.warning(f"[config] 服务 {svc.get('comment')} 的 pricing '{pricing}' 非法，忽略（仅支持 none / 对象形式）")
                    pricing = ""
                auth_token = str(svc.get("auth_token", "") or "").strip()
                if not auth_token:
                    # 服务级未配置 → 回退 config.json 顶层 auth_token（全局兜底，
                    # 与桌面端「全局设置 → 认证令牌」UI 字段对应）
                    auth_token = str(config.get("auth_token", "") or "").strip()
                model_policy = svc.get("model_policy", "clamp")
                if model_policy not in MODEL_POLICIES:
                    logger.warning(f"[config] 服务 {svc.get('comment')} 的 model_policy '{model_policy}' 非法，回退 clamp")
                    model_policy = "clamp"
                services.append(Service(
                    id=str(svc.get("id") or "").strip(),
                    order=svc.get("order", i),
                    enabled=svc.get("enabled", True),
                    autostart=svc.get("autostart", False),
                    models=svc.get("models") or [],
                    models_map=svc.get("models_map") or {},
                    model_policy=model_policy,
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
                    auth_token=auth_token,
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