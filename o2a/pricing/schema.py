"""pricing schema：v1 pricing.json → 内部 v2 结构归一化（零行为变更）。

v1 结构（现状，逐字节兼容）：
{
  "<provider>": {"models": {"<model>": {"tiers": [{"input": .., "output": ..,
                                        "cache_hit"?: .., "cache_miss"?: ..}]}}},
  "accounts": {"<id|name>": {"models": {"<model>": {...同上}}}}
}

内部 v2 结构：
{
  "_meta": {"schema": "o2a-pricing/v2", "defaults": {...}},
  "models":   {"<model>": ResolvedEntry},
  "accounts": {"<id|name>": {"models": {"<model>": ResolvedEntry}}},
  "services": {"<svc-id>": {"models": {...}}}
}
ResolvedEntry = {"billing": "token", "components": {input, output, cache_read,
                cache_write, request}, "modifiers": [...]}

v1 → v2 映射规则（§7.5-4，与旧 _calc_cost 行为逐字节一致）：
- 恒取 tiers[0]（v1 语义：单次请求无法判断档位；v2 起由 context_tier modifier 表达）
- cache_read：tier["cache_hit"] 优先，否则 input × 0.2
- cache_write：tier["cache_miss"] 优先，否则 input × 1.0
- 无 tiers / 空 entry：components 全 0（旧代码返回 0.0）
"""

# v1 缺省回退比例（与旧 _calc_cost 中的 0.2 / 1.0 一致）
CACHE_READ_RATIO = 0.2
CACHE_WRITE_RATIO = 1.0

_RANGE_UNITS = {"K": 1024, "M": 1024 * 1024, "G": 1024 * 1024 * 1024}


def parse_range(text):
    """解析 v1 tiers[].range（"0-256K" / "256K-1M" / "unlimited"）→ (low, upto)。

    upto=None 表示无上限；解析失败返回 None。"""
    if not isinstance(text, str):
        return None
    t = text.strip().lower()
    if t in ("unlimited", "∞", "*"):
        return (0, None)
    if "-" not in t:
        return None

    def _num(part: str):
        part = part.strip()
        if not part:
            return None
        unit = _RANGE_UNITS.get(part[-1].upper())
        if unit and len(part) > 1 and part[-1].upper() in _RANGE_UNITS:
            try:
                return int(float(part[:-1]) * unit)
            except ValueError:
                return None
        try:
            return int(float(part))
        except ValueError:
            return None

    low_s, _, high_s = t.partition("-")
    low = _num(low_s) or 0
    high = _num(high_s)
    if high is None:
        return None
    return (low, high)


def _v1_context_tier(entry: dict, tiers: list) -> list:
    """v1 多档 tiers（含 range）→ context_tier modifier（§7.6-④ 阶梯）。

    无 range 的档位跳过；cache_hit/miss/output_thinking 映射进 override。
    无可解析 range 时返回 []（维持旧单档行为）。"""
    specs = []
    for t in tiers:
        if not isinstance(t, dict):
            continue
        parsed = parse_range(t.get("range"))
        if parsed is None:
            continue
        _, upto = parsed
        override = {}
        for src, dst in (("input", "input"), ("output", "output"),
                         ("cache_hit", "cache_read"), ("cache_miss", "cache_write"),
                         ("output_thinking", "output_thinking")):
            v = t.get(src)
            if isinstance(v, (int, float)):
                override[dst] = float(v)
        spec = {"upto": upto}
        if override:
            spec["override"] = override
        specs.append(spec)
    if not specs:
        return []
    return [{"type": "context_tier", "by": "context_tokens", "tiers": specs}]


def _v1_tier_to_components(tier: dict) -> dict:
    """v1 tier → v2 components（烘焙缺省缓存比例，保持结果一致）。"""
    inp = float(tier.get("input", 0) or 0)
    out = float(tier.get("output", 0) or 0)
    cache_read = tier["cache_hit"] if "cache_hit" in tier else inp * CACHE_READ_RATIO
    cache_write = tier["cache_miss"] if "cache_miss" in tier else inp * CACHE_WRITE_RATIO
    return {
        "input": inp,
        "output": out,
        "cache_read": float(cache_read or 0),
        "cache_write": float(cache_write or 0),
        "request": float(tier.get("request", 0) or 0),
    }


def _entry_to_v2(entry: dict) -> dict:
    """单模型条目归一化：v1（tiers）与 v2（components/modifiers）均接受。

    v2 形态（含 components 或 plan/modifiers 键）原样保留 modifiers；
    v1 形态烘焙 tiers[0] + 缓存比例。无有效 tier → components 全 0（旧行为：cost 0）。

    §7.6-②：v1 已有但此前未读取的字段接入：
    - tiers[0].output_thinking → components.output_thinking（reasoning token 计价）
    - 模型级 discount / discount_note → discount modifier（限时折扣立刻生效）
    free_quota / batch(布尔标记) / range 阶梯留待后续步骤（需周期累计状态 /
    context 分档上下文）。
    """
    if "components" in entry or "modifiers" in entry:
        # v2 形态（或 v1+modifiers 混合）：components 缺省补齐；
        # 无 components 但有 tiers 时从 tiers[0] 派生（混合形态，discount/schedule 场景）
        comps = {"input": 0, "output": 0, "cache_read": 0, "cache_write": 0, "request": 0}
        if "components" in entry:
            comps.update(entry.get("components") or {})
        elif entry.get("tiers"):
            comps.update(_v1_tier_to_components(entry["tiers"][0]))
            ot = entry["tiers"][0].get("output_thinking")
            if isinstance(ot, (int, float)):
                comps["output_thinking"] = float(ot)
        mods = list(entry.get("modifiers") or [])
        # 混合形态下同样把多档 range 映射为 context_tier（置于声明 modifiers 之前）
        if entry.get("tiers"):
            mods = _v1_context_tier(entry, entry.get("tiers") or []) + mods
        return {"billing": entry.get("billing", "token"),
                "components": comps, "modifiers": mods}
    tiers = entry.get("tiers") or []
    if not tiers:
        return {"billing": "token",
                "components": {"input": 0, "output": 0, "cache_read": 0,
                               "cache_write": 0, "request": 0},
                "modifiers": []}
    tier = tiers[0]
    comps = _v1_tier_to_components(tier)
    ot = tier.get("output_thinking")
    if isinstance(ot, (int, float)):
        comps["output_thinking"] = float(ot)
    modifiers = []
    modifiers.extend(_v1_context_tier(entry, tiers))
    discount = entry.get("discount")
    if isinstance(discount, (int, float)) and discount != 1:
        modifiers.append({"type": "discount", "factor": float(discount),
                          "note": entry.get("discount_note") or f"discount:{discount:g}"})
    return {"billing": "token", "components": comps, "modifiers": modifiers}


def normalize_pricing(raw: dict) -> dict:
    """整份 pricing 归一化（幂等：v2 输入原样通过）。"""
    if not isinstance(raw, dict):
        return {"_meta": {"schema": "o2a-pricing/v2"}, "models": {},
                "accounts": {}, "services": {}}
    models: dict = {}
    for provider, pv in raw.items():
        if provider.startswith("_") or provider == "accounts":
            continue
        if not isinstance(pv, dict):
            continue
        for model, mv in (pv.get("models") or {}).items():
            if isinstance(mv, dict):
                models[model] = _entry_to_v2(mv)
    accounts: dict = {}
    acc_raw = raw.get("accounts")
    if isinstance(acc_raw, dict):
        for key, av in acc_raw.items():
            if not isinstance(av, dict):
                continue
            m = {}
            for name, mv in (av.get("models") or {}).items():
                if isinstance(mv, dict):
                    m[name] = _entry_to_v2(mv)
            accounts[key] = {"models": m}
    return {
        "_meta": {"schema": "o2a-pricing/v2", **(raw.get("_meta") or {})},
        "models": models,
        "accounts": accounts,
        "services": raw.get("services") or {},
    }
