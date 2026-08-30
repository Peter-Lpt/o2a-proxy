"""discount modifier：无条件折扣（pricing.json 中已存在但 v1 未读取的字段，）。

用法：{"type": "discount", "factor": 0.5, "note": "限时 5 折"}
全部单价乘 factor（不改变计价结构，仅缩放 components）。
"""


def apply(components: dict, ctx: dict, mod: dict):
    factor = float(mod.get("factor", 1.0))
    comps = {k: v * factor for k, v in components.items()}
    note = mod.get("note") or f"discount:{factor:g}"
    return comps, [note]
