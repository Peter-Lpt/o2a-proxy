"""modifier 注册表：每种价格方案一个文件、注册一行，主流程零改动（§7.4）。

modifier 签名（Py/Rs 同构）：
    apply(components: dict, ctx: UsageContext) -> (new_components, applied_notes)

合并规则（写进 schema 文档）：entry 声明的 modifiers 按数组顺序执行；
若同 type 重复声明则后者覆盖前者（除非显式 "merge": "append"）。
"""

from .context_tier import apply as apply_context_tier
from .discount import apply as apply_discount
from .batch import apply as apply_batch
from .schedule import apply as apply_schedule

# type → apply(components, ctx) -> (components, applied: list[str])
_REGISTRY = {
    "discount": apply_discount,
    "batch": apply_batch,
    "schedule": apply_schedule,
    "context_tier": apply_context_tier,
    # cumulative_tier / free_quota / overage：阶段3 后续接入
}


def consolidate(modifiers: list) -> list:
    """同 type 去重（后者覆盖前者，显式 merge=append 保留）。"""
    out: list = []
    for m in modifiers or []:
        if not isinstance(m, dict) or "type" not in m:
            continue
        if m.get("merge") == "append":
            out.append(m)
            continue
        out = [x for x in out if x.get("type") != m["type"]]
        out.append(m)
    return out


def apply_all(modifiers: list, components: dict, ctx: dict):
    """顺序执行 modifier 管道，返回 (components, applied 说明列表)。"""
    applied = []
    for m in consolidate(modifiers):
        fn = _REGISTRY.get(m["type"])
        if fn is None:
            continue
        components, notes = fn(components, ctx or {}, m)
        applied.extend(notes)
    return components, applied
