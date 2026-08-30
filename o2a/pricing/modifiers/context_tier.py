"""context_tier modifier：按上下文长度分档计价（，阶梯）。

用法：
{"type": "context_tier", "by": "context_tokens", "tiers": [
    {"upto": 262144, "override": {"input": 1.0, "output": 4.0}},
    {"upto": 1048576, "override": {"input": 2.0, "output": 8.0}},
    {"upto": null, "override": {"input": 3.0, "output": 12.0}}
]}

- by：取 ctx.meta[by]（默认 context_tokens = 输入侧 prompt 总 token，
  由调用方按 input+cache_read+cache_write 计算，双端口径一致）
- 命中第一个 value <= upto 的档位（upto=null = 无上限）；override 覆盖同名单价键，
  或用 factor 乘全部分量
- ctx 无该值时不生效（如 v1 单档旧记录回读）
"""

def apply(components: dict, ctx: dict, mod: dict):
    meta = ctx.get("meta") or {}
    value = meta.get(mod.get("by", "context_tokens"))
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
        return comps, [t.get("note") or "context_tier"]
    return components, []
