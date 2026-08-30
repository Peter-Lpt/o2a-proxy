"""定价 golden fixtures 回归。

pytest 与 cargo test 跑同一份 pricing/golden/cases.json，
期望值由重构前旧 _calc_cost 算法计算，固化抽模块前后零行为变更。

运行方式：
    python -m pytest test_pricing_golden.py -v
"""
import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import pytest

from o2a.pricing import resolve_cost

GOLDEN = os.path.join(
    os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
    "pricing", "golden", "cases.json",
)

with open(GOLDEN, encoding="utf-8") as _f:
    _data = json.load(_f)
CASES = _data["cases"]


def _run(case):
    usage = case["usage"]
    result = resolve_cost(
        case["pricing"], case["model"],
        usage["input"], usage["cache_read"], usage["cache_write"], usage["output"],
        account_keys=case.get("account_keys") or [],
        service_id=case.get("service_id", ""),
        timestamp=case.get("timestamp"),
        context_tokens=case.get("context_tokens"),
        cumulative_tokens=case.get("cumulative"),
        meta=case.get("meta"),
    )
    return result["total"]


def _run_full(case):
    """返回完整 CostResult（用于校验 complete/rule_id/source）。"""
    usage = case["usage"]
    return resolve_cost(
        case["pricing"], case["model"],
        usage["input"], usage["cache_read"], usage["cache_write"], usage["output"],
        account_keys=case.get("account_keys") or [],
        service_id=case.get("service_id", ""),
        timestamp=case.get("timestamp"),
        context_tokens=case.get("context_tokens"),
        cumulative_tokens=case.get("cumulative"),
        meta=case.get("meta"),
    )


@pytest.mark.parametrize("case", CASES, ids=[c["name"] for c in CASES])
def test_golden_case(case):
    # 1e-12 容差：双端同序浮点求值应逐位一致，容差仅防 JSON 往返噪声
    assert _run(case) == pytest.approx(case["expected_total"], abs=1e-12)
    # 若夹具显式声明完整性/规则元数据，也一并校验
    if "expected_complete" in case:
        assert _run_full(case)["complete"] is case["expected_complete"]
    if "expected_rule_id" in case:
        assert _run_full(case).get("rule_id") == case["expected_rule_id"]
    if "expected_source" in case:
        assert _run_full(case).get("source") == case["expected_source"]


def test_v1_behaviour_unchanged_cache_fallback():
    """v1 兼容映射：无 cache_hit/miss 时按 input×0.2 / input×1.0 烘焙（旧规则）。"""
    pricing = {"p": {"models": {"m": {"tiers": [{"input": 3.0, "output": 12.0}]}}}}
    result = resolve_cost(pricing, "m", 1_000_000, 100_000, 10_000, 0)
    # input 1M×3.0=3.0；cache_read 100k×(3.0×0.2)=0.06；cache_write 10k×3.0=0.03
    assert result["total"] == pytest.approx(3.09, abs=1e-12)


def test_v2_components_pass_through():
    """v2 形态（components/modifiers）原样通过 loader。"""
    pricing = {"p": {"models": {"m": {
        "components": {"input": 2.0, "output": 8.0, "cache_read": 0.4, "cache_write": 2.0},
        "modifiers": [{"type": "discount", "factor": 0.5}],
    }}}}
    result = resolve_cost(pricing, "m", 1_000_000, 0, 0, 1_000_000)
    assert result["total"] == pytest.approx((2.0 + 8.0) * 0.5, abs=1e-12)
    assert result["applied"] == ["discount:0.5"]


def test_service_level_override_wins():
    """覆盖优先级：服务级 > 账号级 > 模型级。"""
    pricing = {
        "p": {"models": {"m": {"tiers": [{"input": 1.0, "output": 1.0}]}}},
        "accounts": {"acc-1": {"models": {"m": {"tiers": [{"input": 2.0, "output": 2.0}]}}}},
        "services": {"svc-x": {"models": {"m": {"tiers": [{"input": 3.0, "output": 3.0}]}}}},
    }
    r = resolve_cost(pricing, "m", 1_000_000, 0, 0, 0, account_keys=["acc-1"], service_id="svc-x")
    assert r["total"] == pytest.approx(3.0, abs=1e-12)
    r2 = resolve_cost(pricing, "m", 1_000_000, 0, 0, 0, account_keys=["acc-1"])
    assert r2["total"] == pytest.approx(2.0, abs=1e-12)
    r3 = resolve_cost(pricing, "m", 1_000_000, 0, 0, 0)
    assert r3["total"] == pytest.approx(1.0, abs=1e-12)


def test_duplicate_modifier_type_overrides():
    """同 type modifier：后者覆盖前者（merge=append 保留），防止折扣重复相乘。"""
    from o2a.pricing.evaluate import evaluate_entry
    entry = {
        "components": {"input": 4.0, "output": 0, "cache_read": 0, "cache_write": 0, "request": 0},
        "modifiers": [
            {"type": "discount", "factor": 0.5},
            {"type": "discount", "factor": 0.8},
            {"type": "batch", "factor": 0.9, "merge": "append"},
        ],
    }
    result = evaluate_entry(entry, 1_000_000, ctx={"meta": {"batch": True}})
    # discount 去重取 0.8（非 0.5×0.8），batch 追加 → 4.0×0.8×0.9 = 2.88
    assert result["total"] == pytest.approx(2.88, abs=1e-12)