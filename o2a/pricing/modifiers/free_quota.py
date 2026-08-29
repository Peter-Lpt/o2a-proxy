"""free_quota modifier：周期免费额度冲抵（§7.6-②/⑤，pricing.json 已有字段）。

用法：{"type": "free_quota", "period": "month", "unit": "tokens", "amount": 1000000}

冲抵语义（最后一步，§7.3）：
- remaining = max(0, amount - cumulative)（cumulative = 本周期内、当前记录之前
  已消耗的 tokens，由调用方从统计确定性计算：当月 ts 严格早于本记录的记录总量）
- 本次请求 tokens = input + cache_read + cache_write + output（reasoning ⊂ output 不重复计）
- ratio = remaining / request_tokens（clamp 到 [0,1]）→ total = raw_total × ratio
- cumulative 未知（ctx 无 cumulative）时不生效
"""

def apply(components: dict, ctx: dict, mod: dict):
    cumulative = (ctx.get("cumulative") or {})
    used = cumulative.get("tokens")
    if used is None:
        return components, []
    amount = float(mod.get("amount", 0) or 0)
    if amount <= 0:
        return components, []
    tokens = ctx.get("tokens") or {}
    req_tokens = (tokens.get("input", 0) + tokens.get("cache_read", 0)
                  + tokens.get("cache_write", 0) + tokens.get("output", 0))
    if req_tokens <= 0:
        return components, []
    remaining = max(0.0, amount - float(used))
    ratio = min(1.0, remaining / req_tokens)
    if ratio >= 1.0:
        return components, []
    comps = {k: v * ratio for k, v in components.items()}
    return comps, [mod.get("note") or f"free_quota:{mod.get('period', 'month')}"]
