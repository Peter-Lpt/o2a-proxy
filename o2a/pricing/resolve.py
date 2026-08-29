"""pricing 覆盖链解析：服务级 > 账号级 > 模型级（全局）（优化方案 §7.3）。

账号键兼容 v1 语义：accounts 段的键可为账号 id 或 name（与 auth.json 同规），
调用方传入候选键序列（id 优先，name 兜底）。
"""

from .evaluate import evaluate_entry
from .schema import normalize_pricing


def resolve_entry(raw_pricing: dict, model: str, account_keys=None, service_id: str = None):
    """解析某模型在（服务、账号）上下文下的定价条目（ResolvedEntry）。

    返回 v2 entry dict；未命中返回 None（调用方按 0 处理，与旧行为一致）。
    """
    if not raw_pricing:
        return None
    data = normalize_pricing(raw_pricing)
    # 1) 服务级覆盖（§7.2 services.<svc-id>.models，"*" 通配）
    if service_id:
        svc = data.get("services", {}).get(service_id) or {}
        sm = svc.get("models") or {}
        entry = sm.get(model) or sm.get("*")
        if isinstance(entry, dict):
            return _ensure_v2(entry)
    # 2) 账号级（键序：id 优先，name 兜底 —— v1 accounts 段语义）
    for key in account_keys or []:
        acc = data.get("accounts", {}).get(key)
        if isinstance(acc, dict):
            entry = (acc.get("models") or {}).get(model)
            if isinstance(entry, dict):
                return _ensure_v2(entry)
    # 3) 全局模型级
    entry = data.get("models", {}).get(model)
    if isinstance(entry, dict):
        return _ensure_v2(entry)
    return None


def _ensure_v2(entry: dict) -> dict:
    from .schema import _entry_to_v2
    return _entry_to_v2(entry)


def resolve_cost(raw_pricing: dict, model: str, input_tokens=0, cache_read=0,
                 cache_write=0, output_tokens=0, reasoning_tokens=0, requests=0,
                 account_keys=None, service_id: str = None, timestamp: str = None,
                 context_tokens: int = None) -> dict:
    """resolve + evaluate 一步到位；未命中定价时 total=0（旧行为）。"""
    entry = resolve_entry(raw_pricing, model, account_keys=account_keys, service_id=service_id)
    if entry is None:
        return {"total": 0.0, "breakdown": {}, "applied": []}
    ctx = {}
    if timestamp:
        ctx["timestamp"] = timestamp
    if context_tokens is not None:
        ctx["meta"] = {"context_tokens": context_tokens}
    return evaluate_entry(entry, input_tokens, cache_read, cache_write,
                          output_tokens, reasoning_tokens, requests,
                          ctx=ctx or None)
