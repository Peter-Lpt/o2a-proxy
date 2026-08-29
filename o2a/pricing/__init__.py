"""o2a-pricing：定价解析与求值（优化方案 §7）。

双层设计：plan/entry 做"可复用的价格方案"，modifier 管道做"可扩展求值"。
v1 pricing.json（providers[].models[].tiers[]）在 loader 中映射为内部 v2 结构，
存量 pricing.json 零迁移且结果逐字节一致（兼容回归由 golden fixtures 固化）。

模块结构：
- schema.py    解析 + v1→v2 归一化
- resolve.py   覆盖链：服务级 > 账号级 > 模型级（全局）
- evaluate.py  求值：components × 用量 → CostResult（breakdown + applied）
- modifiers/   modifier 注册表（每种价格方案一个文件，新增方案零改主流程）
"""

from .evaluate import evaluate, evaluate_entry
from .resolve import resolve_entry, resolve_cost

__all__ = ["evaluate", "evaluate_entry", "resolve_entry", "resolve_cost"]
