"""pricing 求值：components × 用量 → CostResult（Py/Rs 同构，§7.3）。

求值步骤（固定顺序）：
1. components = 解析后的基础单价
2. for m in entry.modifiers（按数组顺序）: components, applied = apply_modifier(...)
3. cost = input×input + output×output + cache_read×cache_read
        + cache_write×cache_write + requests×request
   （reasoning token 存在且配置 output_thinking 时按该单价计）
4. CostResult = {total, breakdown, applied}
"""

from .modifiers import apply_all


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


def evaluate_entry(entry: dict, input_tokens=0, cache_read=0, cache_write=0,
                   output_tokens=0, reasoning_tokens=0, requests=0,
                   ctx: dict = None) -> dict:
    """带 modifier 管道的求值。ctx = {timestamp, tokens, meta, cumulative, ...}。"""
    comps = dict(entry.get("components") or {})
    ctx_full = {
        "timestamp": None,
        "tokens": {"input": input_tokens, "output": output_tokens,
                   "cache_read": cache_read, "cache_write": cache_write,
                   "reasoning": reasoning_tokens},
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
    return result
