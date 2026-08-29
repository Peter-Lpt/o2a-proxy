"""batch modifier：批量折扣（pricing.json 中已存在但 v1 未读取的字段，§7.6-②）。

用法：{"type": "batch", "factor": 0.5, "when": {"batch": true}}
仅当 ctx.meta.batch 为真时生效（可选字段，缺省即生效）。
"""


def apply(components: dict, ctx: dict, mod: dict):
    when = mod.get("when") or {}
    meta = ctx.get("meta") or {}
    for k, v in when.items():
        if meta.get(k) != v:
            return components, []
    factor = float(mod.get("factor", 1.0))
    comps = {k: v * factor for k, v in components.items()}
    note = mod.get("note") or f"batch:{factor:g}"
    return comps, [note]
