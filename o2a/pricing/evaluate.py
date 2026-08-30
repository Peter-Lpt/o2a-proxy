"""pricing 求值：components × 用量 → CostResult。

求值步骤（固定顺序）：
1. components = 解析后的基础单价
2. for m in entry.modifiers（按数组顺序）: components, applied = apply_modifier(...)
3. cost = input×input + output×output + cache_read×cache_read
        + cache_write×cache_write + requests×request
   （reasoning token 存在且配置 output_thinking 时按该单价计）
4. CostResult = {total, breakdown, applied, complete, currency, ...}

完整性：未知模型/无规则/正 token 但显式组件未配置价格 → complete=false。
"""

from .modifiers import apply_all

_BASE_COMPONENT_USAGE_MAP = (
    ("input", "input"),
    ("output", "output"),
    ("cache_read", "cache_read"),
    ("cache_write", "cache_write"),
    ("requests", "request"),
)


def evaluate(components: dict, input_tokens=0, cache_read=0, cache_write=0,
             output_tokens=0, reasoning_tokens=0, requests=0) -> dict:
    """无 modifier 的基础求值（与旧 _calc_cost 结果逐字节一致）。"""
    output_price = float(components.get("output", 0) or 0)
    if reasoning_tokens and components.get("output_thinking") is not None:
        output_price = float(components["output_thinking"])
    breakdown = {
        "input": input_tokens * float(components.get("input", 0) or 0) / 1_000_000,
        "output": output_tokens * output_price / 1_000_000,
        "cache_read": cache_read * float(components.get("cache_read", 0) or 0) / 1_000_000,
        "cache_write": cache_write * float(components.get("cache_write", 0) or 0) / 1_000_000,
        "request": requests * float(components.get("request", 0) or 0),
    }
    return {"total": sum(breakdown.values()), "breakdown": breakdown, "applied": []}


def _complete(entry: dict, tokens: dict) -> bool:
    """完整性检查：正用量对应的单价必须由原始配置显式声明。

    v1 tiers 会烘焙五项完整分量，因此显式集合总是完整；v2/v3 可保留缺省 0，
    但该“缺省 0”必须视为未声明，防止 unknow 被误展示为免费。"""
    explicit = set(entry.get("_explicit_components") or ())
    if explicit == {"input", "output", "cache_read", "cache_write", "request"}:
        # v1 烘焙完整：视为全部声明过（旧行为，费用可信）
        return True
    for usage_key, comp_key in _BASE_COMPONENT_USAGE_MAP:
        if (tokens.get(usage_key) or 0) > 0 and comp_key not in explicit:
            return False
    # reasoning_tokens > 0 时 output_thinking 未声明不视为不完整：仍按 output 价计
    return True


def evaluate_entry(entry: dict, input_tokens=0, cache_read=0, cache_write=0,
                   output_tokens=0, reasoning_tokens=0, requests=0,
                   ctx: dict = None) -> dict:
    """带 modifier 管道的求值。ctx = {timestamp, tokens, meta, cumulative, ...}。"""
    comps = dict(entry.get("components") or {})
    ctx_full = {
        "timestamp": None,
        "tokens": {"input": input_tokens, "output": output_tokens,
                   "cache_read": cache_read, "cache_write": cache_write,
                   "reasoning": reasoning_tokens, "requests": requests},
        "meta": {"requests": requests, **((ctx or {}).get("meta") or {})},
    }
    if ctx:
        for k, v in ctx.items():
            if k != "meta":
                ctx_full[k] = v
    comps, applied = apply_all(entry.get("modifiers") or [], comps, ctx_full)
    result = evaluate(comps, input_tokens, cache_read, cache_write,
                      output_tokens, reasoning_tokens, requests)
    result["applied"] = applied
    result["complete"] = _complete(entry, ctx_full["tokens"])
    return result