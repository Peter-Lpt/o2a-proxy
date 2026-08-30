"""cumulative_tier modifier：按周期内累计用量分档（ 阶梯之二）。

用法：
{"type": "cumulative_tier", "period": "month", "by": "tokens",
 "tiers": [{"upto": 1_000_000, "override": {"input": 0.5, "output": 2.0}},
           {"upto": null, "override": {"input": 1.0, "output": 4.0}}]}

- by：取 ctx.cumulative[by]（默认 tokens，由调用方按  同口径计算：
  本周期内早于本记录的 input+cache_read+cache_write+output）
- 命中第一档 value <= upto（upto=null 无上限），override 覆盖同名单价键或乘 factor
- cumulative 未知时不生效
"""

def apply(components: dict, ctx: dict, mod: dict):
    cumulative = ctx.get("cumulative") or {}
    value = cumulative.get(mod.get("by", "tokens"))
    if value is None:
        return components, []
    for t in mod.get("tiers") or []:
        if not isinstance(t, dict):
            continue
        upto = t.get("upto")
        if upto is not None and value > upto:
            continue
        if "override" in t:
            comps = dict(components)
            for k, v in (t["override"] or {}).items():
                if k in comps or k == "output_thinking":
                    comps[k] = float(v)
        else:
            factor = float(t.get("factor", 1.0))
            comps = {k: v * factor for k, v in components.items()}
        return comps, [t.get("note") or "cumulative_tier"]
    return components, []
