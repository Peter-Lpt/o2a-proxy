"""o2a-pricing：定价解析与求值。

双层设计：plan/entry 做"可复用的价格方案"，modifier 管道做"可扩展求值"。
v1 pricing.json（providers[].models[].tiers[]）在 loader 中映射为内部 v2 结构，
存量 pricing.json 零迁移且结果逐字节一致（兼容回归由 golden fixtures 固化）。

模块结构：
- schema.py    解析 + v1→v2 归一化 + v3 rules
- resolve.py   覆盖链：服务级 > 账号级 > 模型级 + 事件时间规则
- evaluate.py  求值：components × 用量 → CostResult
- modifiers/   modifier 注册表（每种价格方案一个文件，新增方案零改主流程）
- fingerprint.py 价格目录指纹（缓存失效用）
- plans.py     套餐目录
"""

from .evaluate import evaluate, evaluate_entry
from .fingerprint import pricing_fingerprint
from .plans import get_plan, load_plans, plans_fingerprint, plan_windows_to_snapshot
from .resolve import resolve_entry, resolve_cost

__all__ = ["evaluate", "evaluate_entry", "resolve_entry", "resolve_cost",
           "pricing_fingerprint", "load_plans", "get_plan", "plan_windows_to_snapshot",
           "plans_fingerprint"]