"""plans：套餐/计划目录。

plans.json 独立于 pricing.json，支持版本化：
{
  "version": 1,
  "plans": {
    "glm-coding-plan": {
      "mode": "subscription",
      "currency": "CNY",
      "version": 1,
      "windows": [
        {"kind": "session", "period": "rolling-5h", "unit": "requests"},
        {"kind": "weekly", "period": "week", "unit": "tokens"}
      ],
      "included": {"requests": 500, "tokens": 10000000},
      "overage": {"default": {"unit": "tokens", "input": 8, "output": 24}},
      "free_tier": {"tokens": 100000}
    }
  }
}

Plan 是账号能力（不是模型单价），通过 services[].pricing.plan 与 account.quota_source
关联；QuotaAdapter 只产出统一快照，PlanRegistry 负责补全套餐余量与额度定义。
"""

import json
import os

from ..base import PROJECT_ROOT, logger

DEFAULT_PLANS_PATH = os.path.join(PROJECT_ROOT, "plans.json")


def load_plans(path: str = None, raw: dict = None) -> dict:
    """加载 plans.json（或直接传入 raw dict）。

    文件不存在/解析失败时返回空目录并 warning，调用方按空套餐继续。"""
    if raw is None:
        p = path or DEFAULT_PLANS_PATH
        try:
            with open(p, encoding="utf-8") as f:
                raw = json.load(f)
        except (OSError, ValueError) as e:
            logger.warning(f"[plans] 加载失败（按空目录继续）: {e}")
            return {"_meta": {"schema": "o2a-plans/v1"}, "plans": {}}
    if not isinstance(raw, dict):
        return {"_meta": {"schema": "o2a-plans/v1"}, "plans": {}}
    plans = raw.get("plans") if isinstance(raw.get("plans"), dict) else {}
    meta = {
        "schema": "o2a-plans/v1",
        "version": raw.get("version", 1),
    }
    return {"_meta": meta, "plans": plans}


def get_plan(plan_name: str, plans: dict = None, raw_plans: dict = None):
    """取单个计划（支持 load_plans 返回值或裸 plans dict）。"""
    if plans is None:
        plans = load_plans(raw=raw_plans).get("plans", {}) if raw_plans is not None else load_plans().get("plans", {})
    if not plan_name:
        return None
    plan = plans.get(plan_name)
    if not isinstance(plan, dict):
        return None
    out = dict(plan)
    out.setdefault("name", plan_name)
    return out


def plan_windows_to_snapshot(plan: dict) -> list:
    """把计划声明的 windows 转成额度窗口模板（limit 来自 included）。

    供 /quota 补全套餐余量展示；实际 used/reset_at 仍由适配器提供。
    """
    if not isinstance(plan, dict):
        return []
    out = []
    included = plan.get("included") or {}
    for w in plan.get("windows") or []:
        if not isinstance(w, dict):
            continue
        unit = w.get("unit", "requests")
        limit = included.get(unit)
        out.append({
            "kind": w.get("kind", "session"),
            "period": w.get("period", "month"),
            "unit": unit,
            "limit": limit,
            "used": None,
            "pct": None,
            "reset_at": None,
        })
    return out


def plans_fingerprint(raw: dict = None) -> str:
    """套餐目录指纹（与 pricing fingerprint 类似，用于缓存失效）。"""
    import hashlib

    data = load_plans(raw=raw)
    canonical = json.dumps(data, ensure_ascii=False, sort_keys=True,
                           separators=(",", ":"), default=str)
    return hashlib.sha256(canonical.encode("utf-8")).hexdigest()