"""额度适配器注册表 + auto 域名嗅探（§8.3）。

选择逻辑：accounts[].quota_source
- auto：按 openai_url 域名嗅探（openrouter.ai → openrouter；嗅探不到 → local）
- 显式名：openrouter / local / local-rolling-5h / manual / none
- 未注册的显式名（anthropic / codex / zen 等）→ 预留名，回退 local

失败隔离（§8.4-3）：fetch 抛错/超时 → 上层 get_snapshot 降级 local 并标 stale。
"""

import asyncio
import threading
from urllib.parse import urlparse

from .adapters.local import LocalQuotaAdapter
from .adapters.local_rolling_5h import LocalRolling5hAdapter
from .adapters.manual import ManualQuotaAdapter
from .adapters.openrouter import OpenRouterAdapter
from .base import QuotaAdapter, QuotaContext, QuotaError, make_snapshot

_ADAPTERS = {}


def register(adapter: QuotaAdapter):
    """注册一行：新增一个供应商适配 = 新增一个文件 + 这里一行（§8.4-1）。"""
    _ADAPTERS[adapter.name] = adapter


register(LocalQuotaAdapter())
register(LocalRolling5hAdapter())
register(ManualQuotaAdapter())
register(OpenRouterAdapter())

# 域名嗅探表：子串 → 适配器名（auto 用）
_SNIFF = [
    ("openrouter.ai", "openrouter"),
]

# 预留显式名（尚未实现 → 回退 local）
_RESERVED = {"anthropic", "codex", "openai_codex", "zen", "opencode_zen", "generic"}


def resolve_adapter_name(account) -> str:
    source = (getattr(account, "quota_source", "") or "auto").strip()
    if source in _RESERVED:
        return "local"
    if source != "auto" and source in _ADAPTERS:
        return source
    url = (getattr(account, "openai_url", "") or "")
    host = urlparse(url).netloc.lower()
    for frag, name in _SNIFF:
        if frag in host:
            return name
    # auto 且手填了额度上限（quota.limit）→ manual 套餐（冷启动兜底）
    quota = getattr(account, "quota", None)
    if source == "auto" and isinstance(quota, dict) and quota.get("limit"):
        return "manual"
    return "local"


def registered_adapters():
    return sorted(_ADAPTERS.keys())


_snapshot_cache = {}
_cache_lock = threading.Lock()


def _fetch_timeout(adapter, ctx: QuotaContext):
    result = adapter.fetch(ctx)
    return result


async def _finalize(snapshot, account, ctx: QuotaContext, name: str, ttl_cache=None, degrade=True):
    """降级 + 缓存写回（sync/async 共用）。"""
    if snapshot is None and degrade and name != "local":
        try:
            local = _ADAPTERS.get("local")
            if local is not None:
                snapshot = _fetch_timeout(local, ctx)
                if asyncio.iscoroutine(snapshot):
                    snapshot = await asyncio.wait_for(snapshot, timeout=UPSTREAM_TIMEOUT_S)
                if isinstance(snapshot, dict):
                    snapshot["stale"] = True
        except Exception:
            snapshot = None
    if snapshot is None and degrade:
        # local 也失败（如统计目录缺失）→ 最小快照，标 stale
        snapshot = make_snapshot("local", [], stale=True, now_fn=ctx.now_fn)
    if snapshot is not None and ttl_cache:
        ttl_cache.set(getattr(account, "id", "") or "", snapshot)
    return snapshot


async def get_snapshot_async(account, ctx: QuotaContext, ttl_cache=None, degrade=True):
    """取额度快照：优先缓存 → 注册表适配器 → 失败降级 local（标 stale）。

    适配器异常/超时绝不外泄（§8.4-3 失败隔离）。"""
    if account is None:
        return None
    key = getattr(account, "id", "") or ""
    if ttl_cache:
        cached = ttl_cache.get(key)
        if cached is not None:
            return cached
    name = resolve_adapter_name(account)
    snapshot = None
    adapter = _ADAPTERS.get(name)
    if adapter is not None:
        try:
            result = _fetch_timeout(adapter, ctx)
            if asyncio.iscoroutine(result):
                result = await asyncio.wait_for(result, timeout=UPSTREAM_TIMEOUT_S)
            snapshot = result
        except Exception:
            snapshot = None
    return await _finalize(snapshot, account, ctx, name, ttl_cache=ttl_cache, degrade=degrade)


def get_snapshot(account, ctx: QuotaContext, ttl_cache=None):
    """同步入口：local 系同步适配器零开销直调；async 适配器需事件循环。"""
    import asyncio

    if account is None:
        return None
    key = getattr(account, "id", "") or ""
    if ttl_cache:
        cached = ttl_cache.get(key)
        if cached is not None:
            return cached
    name = resolve_adapter_name(account)
    adapter = _ADAPTERS.get(name)
    if adapter is None or not asyncio.iscoroutinefunction(adapter.fetch):
        snapshot = None
        try:
            if adapter is not None:
                snapshot = _fetch_timeout(adapter, ctx)
                assert not asyncio.iscoroutine(snapshot)
        except Exception:
            snapshot = None
        return asyncio.run(_finalize(snapshot, account, ctx, name, ttl_cache=ttl_cache))
    try:
        loop = asyncio.get_running_loop()
    except RuntimeError:
        return asyncio.run(get_snapshot_async(account, ctx, ttl_cache=ttl_cache))
    else:
        return loop.run_until_complete(get_snapshot_async(account, ctx, ttl_cache=ttl_cache))
