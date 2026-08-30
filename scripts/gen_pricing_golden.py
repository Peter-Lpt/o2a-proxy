"""生成 golden 定价 fixtures：用"旧 _calc_cost 算法"计算期望值并固化。

：双端（pytest / cargo test）跑同一份 fixtures，是 Python 与 Rust 双实现
不漂移的唯一可靠办法。本脚本只在算法变更或新增用例时手动运行：
    python scripts/gen_pricing_golden.py

期望值计算复刻自重构前的 stats.py::_calc_cost（tiers[0] + cache 回退 0.2/1.0），
用于固化"抽模块前后零行为变更"这一验收标准（）。
"""
import json
import os

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def old_calc_cost(price_entry, input_tokens, cache_read, cache_write, output_tokens):
    """重构前 stats.py::_calc_cost 的定价求值部分（逐字保留）。"""
    if not price_entry:
        return 0.0
    tier = price_entry["tiers"][0] if price_entry.get("tiers") else None
    if not tier:
        return 0.0
    input_cost = input_tokens * tier.get("input", 0) / 1_000_000
    output_cost = output_tokens * tier.get("output", 0) / 1_000_000
    if "cache_hit" in tier:
        cache_read_cost = cache_read * tier["cache_hit"] / 1_000_000
    else:
        cache_read_cost = cache_read * tier.get("input", 0) * 0.2 / 1_000_000
    if "cache_miss" in tier:
        cache_write_cost = cache_write * tier["cache_miss"] / 1_000_000
    else:
        cache_write_cost = cache_write * tier.get("input", 0) / 1_000_000
    return input_cost + output_cost + cache_read_cost + cache_write_cost


def _resolve_old(pricing, model, account_keys):
    """重构前的查找顺序：账号级（键序）优先，全局兜底；未命中 None。"""
    accounts_pricing = pricing.get("accounts")
    for key in account_keys or []:
        if not isinstance(accounts_pricing, dict) or not key:
            continue
        direct = accounts_pricing.get(key)
        if isinstance(direct, dict):
            m = (direct.get("models") or {}).get(model)
            if m is not None:
                return m
    for provider in pricing:
        if provider.startswith("_") or provider == "accounts":
            continue
        models = pricing[provider].get("models", {})
        if model in models:
            return models[model]
    return None


def case(name, pricing, model, usage, account_keys=None):
    entry = _resolve_old(pricing, model, account_keys)
    return {
        "name": name,
        "pricing": pricing,
        "model": model,
        "account_keys": account_keys or [],
        "usage": usage,
        "expected_total": old_calc_cost(entry, **{
            "input_tokens": usage["input"],
            "cache_read": usage["cache_read"],
            "cache_write": usage["cache_write"],
            "output_tokens": usage["output"],
        }),
    }


def main():
    pricing_flat = {
        "deepseek": {"models": {"deepseek-v4-flash": {"tiers": [
            {"input": 1.0, "output": 4.0, "cache_hit": 0.2, "cache_miss": 1.0}]}}},
    }
    pricing_fallback = {
        "qwen": {"models": {"qwen3-max": {"tiers": [{"input": 3.0, "output": 12.0}]}}},
    }
    pricing_empty_tiers = {
        "x": {"models": {"no-tiers": {"tiers": []}}},
    }
    pricing_accounts = {
        "aliyun": {"models": {"qwen-plus": {"tiers": [{"input": 0.8, "output": 2.0}]}}},
        "accounts": {
            "acc-3": {"models": {"qwen-plus": {"tiers": [
                {"input": 1.0, "output": 4.0, "cache_hit": 0.2}]}},
            },
            "某中转站": {"models": {"qwen-plus": {"tiers": [{"input": 0.5, "output": 1.5}]}}},
        },
    }
    pricing_discount = {
        "dashscope": {"models": {"qwen3.7-max": {
            "tiers": [{"range": "0-1M", "input": 12, "output": 36, "output_thinking": 36}],
            "discount": 0.5, "discount_note": "限时 5 折", "batch": True, "free_quota": 1000000}}},
    }
    usage = {"input": 100_000, "output": 50_000, "cache_read": 200_000, "cache_write": 20_000}
    zero_usage = {"input": 0, "output": 0, "cache_read": 0, "cache_write": 0}

    cases = [
        case("flat-explicit-cache", pricing_flat, "deepseek-v4-flash", usage),
        case("cache-fallback-ratios", pricing_fallback, "qwen3-max", usage),
        case("empty-tiers-cost-zero", pricing_empty_tiers, "no-tiers", usage),
        case("missing-model-cost-zero", pricing_flat, "unknown-model", usage),
        case("empty-usage-cost-zero", pricing_flat, "deepseek-v4-flash", zero_usage),
        case("account-by-id", pricing_accounts, "qwen-plus", usage, ["acc-3"]),
        case("account-by-name", pricing_accounts, "qwen-plus", usage, ["acc-9", "某中转站"]),
        case("account-fallback-global", pricing_accounts, "qwen-plus", usage, ["acc-404"]),
        case("no-account-global", pricing_accounts, "qwen-plus", usage, []),
        #  行为变更：v1 discount 字段自此生效（原为未读取 → 费用虚高一倍）。
        # 期望 = 旧算法结果 × 0.5（0.5 为 2 的幂，浮点精确；双端一致验证 discount 生效）。
        case("v1-discount-halves-cost", pricing_discount, "qwen3.7-max", usage),
    ]
    cases[-1]["expected_total"] = cases[-1]["expected_total"] * 0.5
    cases[-1]["note"] = " 行为变更：discount 0.5 生效，费用减半（旧算法不读取 discount）"

    #  schedule（峰谷/周末）：期望值按语义手算 ——
    # 基础 comps: input 2.0 / output 4.0 / cache_read 0.4(=2.0×0.2) / cache_write 2.0(=2.0×1.0)。
    # schedule 只覆盖 input/output；cache 分量保持烘焙值。
    def sched_expected(input_p, output_p):
        return (usage["input"] * input_p + usage["output"] * output_p
                + usage["cache_read"] * 0.4 + usage["cache_write"] * 2.0) / 1_000_000

    pricing_schedule = {
        "pv": {"models": {"peak-valley-model": {
            "tiers": [{"input": 2.0, "output": 4.0}],
            "modifiers": [{"type": "schedule", "windows": [
                {"days": ["sat", "sun"], "override": {"input": 1.0, "output": 2.0},
                 "note": "周末价"},
                {"days": ["mon", "tue", "wed", "thu", "fri"], "from": "22:00", "to": "08:00",
                 "override": {"input": 1.0, "output": 2.0}, "note": "错峰"},
                {"days": ["mon", "tue", "wed", "thu", "fri"], "from": "08:00", "to": "22:00",
                 "override": {"input": 4.0, "output": 16.0}, "note": "高峰"},
            ]}]}}},
    }
    # 2026-01-07 是周三；2026-01-03 是周六
    # （ 验收：同一模型 21:59 与 22:01 分别按峰/谷价 —— 21:59 仍在 08:00-22:00 峰窗内）
    for name, ts, ip, op, note in [
        ("sched-wed-2159-peak", "2026-01-07T21:59:00", 4.0, 16.0, "22:00 前一分钟仍在高峰窗口"),
        ("sched-wed-2201-valley", "2026-01-07T22:01:00", 1.0, 2.0, "22:00 后一分钟走谷价（跨天区间）"),
        ("sched-wed-1200-peak", "2026-01-07T12:00:00", 4.0, 16.0, "工作日日间走高峰"),
        ("sched-sat-1200-weekend", "2026-01-03T12:00:00", 1.0, 2.0, "周六全天走周末价"),
    ]:
        c = case(name, pricing_schedule, "peak-valley-model", usage)
        c["timestamp"] = ts
        c["expected_total"] = sched_expected(ip, op)
        c["note"] = note
        cases.append(c)

    # fallback：无周末窗口的 schedule，周日不命中任何 window → fallback 单价
    pricing_sched_fallback = {
        "pv2": {"models": {"fv-model": {
            "tiers": [{"input": 2.0, "output": 4.0}],
            "modifiers": [
                {"type": "schedule",
                 "windows": [
                     {"days": ["mon", "tue", "wed", "thu", "fri"],
                      "from": "08:00", "to": "22:00",
                      "override": {"input": 4.0, "output": 16.0}}],
                 "fallback": {"input": 1.5, "output": 4.5}}
            ]}}}
    }
    c = case("sched-sun-fallback", pricing_sched_fallback, "fv-model", usage)
    c["timestamp"] = "2026-01-04T12:00:00"
    c["expected_total"] = sched_expected(1.5, 4.5)
    c["note"] = "周日不命中窗口 → fallback 单价"
    cases.append(c)

    #  context_tier（v1 range 多档映射）：v1 tiers 双档 0-256K / 256K-1M。
    # usage: input 100K / cache_read 200K / cache_write 20K → context_tokens = 320K
    #   → 落在第二档（256K-1M），即方案验收"300K 上下文请求按第二档计价"。
    # 第二档单价 input 2.0 / output 8.0（cache 不在 override 内 → 保持第一档烘焙值 0.4 / 2.0）
    pricing_tiered = {
        "t": {"models": {"long-context-model": {"tiers": [
            {"range": "0-256K", "input": 1.0, "output": 4.0},
            {"range": "256K-1M", "input": 2.0, "output": 8.0}]}}},
    }
    ctx_tokens = usage["input"] + usage["cache_read"] + usage["cache_write"]  # 320K
    # 第二档 override 仅含 input/output；cache 分量保持 tiers[0]（input 1.0）烘焙值：
    # cache_read = 1.0×0.2 = 0.2，cache_write = 1.0×1.0 = 1.0
    c = case("ctx-tier-second-tier", pricing_tiered, "long-context-model", usage)
    c["context_tokens"] = ctx_tokens
    c["expected_total"] = (usage["input"] * 2.0 + usage["output"] * 8.0
                           + usage["cache_read"] * 0.2 + usage["cache_write"] * 1.0) / 1_000_000
    c["note"] = f"context_tokens={ctx_tokens} → 第二档单价（ 300K 验收）"
    cases.append(c)

    # 第一档：context 100K（input 100K / cache 0）→ 第一档单价（与旧 tiers[0] 行为一致）
    small_usage = {"input": 100_000, "output": 50_000, "cache_read": 0, "cache_write": 0}
    c = case("ctx-tier-first-tier", pricing_tiered, "long-context-model", small_usage)
    c["context_tokens"] = 100_000
    c["expected_total"] = (small_usage["input"] * 1.0 + small_usage["output"] * 4.0) / 1_000_000
    c["note"] = "第一档内请求价格与旧单档行为一致（兼容）"
    cases.append(c)

    # v2 upto 写法（第三档无上限）+ unlimited 单档（映射后无行为变化）
    pricing_upto = {
        "t2": {"models": {"upto-model": {
            "components": {"input": 1.0, "output": 2.0, "cache_read": 0.2, "cache_write": 1.0},
            "modifiers": [{"type": "context_tier", "by": "context_tokens", "tiers": [
                {"upto": 262144, "override": {"input": 0.5, "output": 1.0}},
                {"upto": None, "override": {"input": 1.2, "output": 2.4}}]}]}}},
    }
    c = case("ctx-tier-upto-null", pricing_upto, "upto-model", usage)
    c["context_tokens"] = 500_000
    c["expected_total"] = (usage["input"] * 1.2 + usage["output"] * 2.4
                           + usage["cache_read"] * 0.2 + usage["cache_write"] * 1.0) / 1_000_000
    c["note"] = "v2 upto:null 无上限档命中"
    cases.append(c)

    #  free_quota（v1 模型级字段，月度 tokens 额度冲抵）。
    # 基础 comps: input 2.0 / output 4.0 / cache_read 0.4 / cache_write 2.0；
    # req_tokens = input+cache_read+cache_write+output = 370K；ratio = remaining/req
    # 期望按 evaluate 的分量顺序逐项 ×ratio 求和（与双端实现位一致）。
    pricing_fq = {
        "fq": {"models": {"fq-model": {
            "tiers": [{"input": 2.0, "output": 4.0}],
            "free_quota": 1_000_000}}},
    }
    req_tokens = usage["input"] + usage["cache_read"] + usage["cache_write"] + usage["output"]

    def fq_expected(cumulative):
        remaining = max(0.0, 1_000_000 - cumulative)
        r = min(1.0, remaining / req_tokens)
        # 精确复刻双端求值顺序：单价先乘 ratio，再逐项 t×p/1e6 后按序求和
        return (usage["input"] * (2.0 * r) / 1_000_000
                + usage["output"] * (4.0 * r) / 1_000_000
                + usage["cache_read"] * (0.4 * r) / 1_000_000
                + usage["cache_write"] * (2.0 * r) / 1_000_000
                + 0 * 0.0)

    for name, cum in [
        ("fq-partial-remaining", 800_000),
        ("fq-exhausted", 1_000_000),
        ("fq-unused-full", 0),
    ]:
        c = case(name, pricing_fq, "fq-model", usage)
        c["cumulative"] = cum
        c["expected_total"] = fq_expected(cum)
        c["note"] = f"月免费额度 1M，已用 {cum} → 冲抵"
        cases.append(c)

    #  cumulative_tier：月累计 tokens 阶梯（800K 已用 → 命中第一档 <=1M 的降档价）。
    pricing_cum = {
        "ct": {"models": {"cum-model": {
            "components": {"input": 3.0, "output": 6.0, "cache_read": 0.6, "cache_write": 3.0},
            "modifiers": [{"type": "cumulative_tier", "period": "month", "by": "tokens",
                           "tiers": [{"upto": 1_000_000,
                                      "override": {"input": 1.5, "output": 3.0}},
                                     {"upto": None}]}]}}},
    }
    c = case("cum-tier-first", pricing_cum, "cum-model", usage)
    c["cumulative"] = 800_000
    c["expected_total"] = (usage["input"] * 1.5 + usage["output"] * 3.0
                           + usage["cache_read"] * 0.6 + usage["cache_write"] * 3.0) / 1_000_000
    c["note"] = "月累计 800K <= 1M → 第一档折扣价"
    cases.append(c)
    c = case("cum-tier-second", pricing_cum, "cum-model", usage)
    c["cumulative"] = 1_200_000
    # 第二档 upto:null 无 override → 保持基础单价
    c["expected_total"] = (usage["input"] * 3.0 + usage["output"] * 6.0
                           + usage["cache_read"] * 0.6 + usage["cache_write"] * 3.0) / 1_000_000
    c["note"] = "月累计 1.2M 超出第一档 → 第二档（无覆盖，基础价）"
    cases.append(c)
    out = {
        "_readme": "共享 golden fixtures：pytest 与 cargo test 双端跑同一份（）。"
                   "期望值由重构前旧算法计算，固化零行为变更。由 scripts/gen_pricing_golden.py 生成。",
        "cases": cases,
    }
    dst = os.path.join(ROOT, "pricing", "golden", "cases.json")
    os.makedirs(os.path.dirname(dst), exist_ok=True)
    with open(dst, "w", encoding="utf-8") as f:
        json.dump(out, f, ensure_ascii=False, indent=2)
    print(f"written: {dst} ({len(cases)} cases)")
    for c in cases:
        print(f"  {c['name']}: {c['expected_total']!r}")


if __name__ == "__main__":
    main()
