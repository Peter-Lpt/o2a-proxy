"""pricing schema：v1/v2/v3 pricing 配置 → 内部归一化。

v1 结构（现状，逐字节兼容）：
{
  "<provider>": {"models": {"<model>": {"tiers": [{"input": .., "output": ..,
                                        "cache_hit"? .., "cache_miss"? ..}]}}},
  "accounts": {"<id|name>": {"models": {"<model>": {...同上}}}}
}

v2 内部结构：
{
  "_meta": {"schema": "o2a-pricing/v2", "defaults": {...}},
  "models":   {"<model>": ResolvedEntry},
  "accounts": {"<id|name>": {"models": {"<model>": ResolvedEntry}}},
  "services": {"<svc-id>": {"models": {...}}}
}
ResolvedEntry = {"billing": "token", "components": {input, output, cache_read,
                cache_write, request}, "modifiers": [...],
                "_explicit_components": [...], "currency"?, "source"?,
                "updated_at"?, "rule_id"?}

v3 规则形态：
{
  "version": 3,
  "currency": "CNY",
  "rules": [
    {
      "id": "...", "model": "...",
      "scope": {"service": "*", "account": "*"},
      "effective_from": "...", "effective_to": null,
      "components": {...},
      "modifiers": [],
      "source": "...", "updated_at": "..."
    }
  ]
}
v3 rules 在归一化后放入 data["rules"]，由 resolve.py 按事件时间选择。

v1 → v2 映射规则（与旧 _calc_cost 行为逐字节一致）：
- 恒取 tiers[0]（v1 语义：单次请求无法判断档位；v2 起由 context_tier modifier 表达）
- cache_read：tier["cache_hit"] 优先，否则 input × 0.2
- cache_write：tier["cache_miss"] 优先，否则 input × 1.0
- 无 tiers / 空 entry：components 全 0（旧代码返回 0.0）
"""

# v1 缺省回退比例（与旧 _calc_cost 中的 0.2 / 1.0 一致）
CACHE_READ_RATIO = 0.2
CACHE_WRITE_RATIO = 1.0

DEFAULT_CURRENCY = "CNY"
_COMPONENT_KEYS = ("input", "output", "cache_read", "cache_write", "request")

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
    """v1 多档 tiers（含 range）→ context_tier modifier。

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


def _default_components():
    return {"input": 0, "output": 0, "cache_read": 0, "cache_write": 0, "request": 0}


def _provenance(entry: dict) -> dict:
    out = {}
    for key in ("currency", "source", "updated_at", "rule_id"):
        if key in entry and entry[key] is not None and entry[key] != "":
            out[key] = entry[key]
    return out


def _entry_to_v2(entry: dict) -> dict:
    """单模型条目归一化：v1（tiers）与 v2（components/modifiers）均接受。

    v2 形态（含 components 或 plan/modifiers 键）原样保留 modifiers；
    v1 形态烘焙 tiers[0] + 缓存比例。无有效 tier → components 全 0（旧行为：cost 0）。

    - tiers[0].output_thinking → components.output_thinking（reasoning token 计价）
    - 模型级 discount / discount_note → discount modifier（限时折扣立刻生效）
    - free_quota → modifier；range 阶梯 → context_tier
    """
    explicit = set()
    if entry.get("_explicit_components") is not None:
        # 幂等：已归一化条目直接保留显式分量集合，避免默认值被误判为显式声明
        explicit = set(entry["_explicit_components"])
        comps = _default_components()
        comps.update(entry.get("components") or {})
        out = {"billing": entry.get("billing", "token"),
               "components": comps,
               "modifiers": list(entry.get("modifiers") or []),
               "_explicit_components": sorted(explicit)}
        out.update(_provenance(entry))
        return out
    if "components" in entry or "modifiers" in entry:
        # v2 形态（或 v1+modifiers 混合）：components 缺省补齐；
        # 无 components 但有 tiers 时从 tiers[0] 派生（混合形态，discount/schedule 场景）
        comps = _default_components()
        if "components" in entry:
            comps.update(entry.get("components") or {})
            explicit = set((entry.get("components") or {}).keys())
        elif entry.get("tiers"):
            comps.update(_v1_tier_to_components(entry["tiers"][0]))
            explicit = set(comps.keys())
            ot = entry["tiers"][0].get("output_thinking")
            if isinstance(ot, (int, float)):
                comps["output_thinking"] = float(ot)
                explicit.add("output_thinking")
        mods = list(entry.get("modifiers") or [])
        # 混合形态下同样把多档 range 映射为 context_tier（置于声明 modifiers 之前）
        if entry.get("tiers"):
            mods = _v1_context_tier(entry, entry.get("tiers") or []) + mods
        out = {"billing": entry.get("billing", "token"),
               "components": comps, "modifiers": mods,
               "_explicit_components": sorted(explicit)}
        out.update(_provenance(entry))
        return out
    tiers = entry.get("tiers") or []
    if not tiers:
        out = {"billing": "token", "components": _default_components(),
               "modifiers": [], "_explicit_components": []}
        out.update(_provenance(entry))
        return out
    tier = tiers[0]
    comps = _v1_tier_to_components(tier)
    explicit = set(comps.keys())
    ot = tier.get("output_thinking")
    if isinstance(ot, (int, float)):
        comps["output_thinking"] = float(ot)
        explicit.add("output_thinking")
    modifiers = []
    modifiers.extend(_v1_context_tier(entry, tiers))
    discount = entry.get("discount")
    if isinstance(discount, (int, float)) and discount != 1:
        modifiers.append({"type": "discount", "factor": float(discount),
                          "note": entry.get("discount_note") or f"discount:{discount:g}"})
    # v1 free_quota（模型级数字，按月 tokens 额度）接入，冲抵在最后一步
    fq = entry.get("free_quota")
    if isinstance(fq, (int, float)) and fq > 0:
        modifiers.append({"type": "free_quota", "period": "month", "unit": "tokens",
                          "amount": float(fq)})
    out = {"billing": "token", "components": comps, "modifiers": modifiers,
           "_explicit_components": sorted(explicit)}
    out.update(_provenance(entry))
    return out


def _normalize_scope(scope):
    if not isinstance(scope, dict):
        return {"service": "*", "account": "*"}
    return {
        "service": str(scope.get("service", "*") or "*"),
        "account": str(scope.get("account", "*") or "*"),
    }


def normalize_rule(rule: dict) -> dict:
    """v3 单条规则 → 内部 ResolvedEntry（含规则元数据）。"""
    rule = dict(rule)
    components = rule.get("components") or {}
    comps = _default_components()
    comps.update({k: v for k, v in components.items() if k in _COMPONENT_KEYS})
    if isinstance(components.get("output_thinking"), (int, float)):
        comps["output_thinking"] = float(components["output_thinking"])
    explicit = set(components.keys()) & set(_COMPONENT_KEYS)
    if "output_thinking" in components:
        explicit.add("output_thinking")
    mods = list(rule.get("modifiers") or [])
    # v3 规则保留计划/批量等元信息；如规则用 plan 模式，通过 modifiers/overage 表达
    if rule.get("plan"):
        mods.append({"type": "plan", "plan": rule["plan"]})
    out = {
        "billing": rule.get("billing", "token"),
        "components": comps,
        "modifiers": mods,
        "_explicit_components": sorted(explicit),
        "scope": _normalize_scope(rule.get("scope")),
        "effective_from": rule.get("effective_from"),
        "effective_to": rule.get("effective_to"),
        "currency": rule.get("currency"),
        "source": rule.get("source"),
        "updated_at": rule.get("updated_at"),
        "rule_id": rule.get("id") or rule.get("rule_id"),
    }
    # 去掉 None 元数据字段，保持输出精简
    for k in ("currency", "source", "updated_at", "rule_id"):
        if not out.get(k):
            out.pop(k, None)
    return out


def validate_pricing_rules(raw: dict) -> list:
    """校验 v3 rules：同 (service, account, model, currency) 生效区间不可重叠。

    返回规范后的规则列表；有重叠抛 ValueError（启动/加载时给出明确错误，不静默 0）。"""
    rules = raw.get("rules") if isinstance(raw, dict) else None
    if not rules:
        return []
    norm = [normalize_rule(r) for r in rules if isinstance(r, dict)]
    buckets = {}
    for r in norm:
        key = (
            r["scope"]["service"],
            r["scope"]["account"],
            r.get("model", "*"),
            r.get("currency") or raw.get("currency") or DEFAULT_CURRENCY,
        )
        buckets.setdefault(key, []).append(r)
    for key, items in buckets.items():
        for i in range(len(items)):
            for j in range(i + 1, len(items)):
                if _intervals_overlap(items[i], items[j]):
                    a, b = items[i], items[j]
                    raise ValueError(
                        "pricing rules overlap: "
                        f"({key[0]}, {key[1]}, {key[2]}, {key[3]}) "
                        f"rules {a.get('rule_id')!r} and {b.get('rule_id')!r} "
                        f"({_ts_text(a['effective_from'])}~{_ts_text(a['effective_to'])} "
                        f"vs {_ts_text(b['effective_from'])}~{_ts_text(b['effective_to'])})"
                    )
    return norm


def _ts_text(v):
    return v if v else "∞"


def _intervals_overlap(a, b):
    """两条规则生效区间是否重叠（from 含、to 不含；None 表示无界）。"""
    a0 = a.get("effective_from")
    a1 = a.get("effective_to")
    b0 = b.get("effective_from")
    b1 = b.get("effective_to")
    # not (a entirely before b) and not (b entirely before a)
    if a0 is not None and b1 is not None and a0 >= b1:
        return False
    if b0 is not None and a1 is not None and b0 >= a1:
        return False
    return True


def normalize_pricing(raw: dict) -> dict:
    """整份 pricing 归一化（幂等：v2 输入原样通过；v3 rules 保留为规则表）。"""
    if not isinstance(raw, dict):
        return {"_meta": {"schema": "o2a-pricing/v2", "currency": DEFAULT_CURRENCY},
                "models": {}, "accounts": {}, "services": {}, "rules": []}
    # v3 校验：有 rules 时先查重叠，失败抛错（调用方可在加载时捕获给出明确错误）
    rules = validate_pricing_rules(raw)
    models: dict = {}
    for provider, pv in raw.items():
        if provider.startswith("_") or provider in ("accounts", "services", "rules"):
            continue
        if not isinstance(pv, dict):
            continue
        provider_source = pv.get("source")
        provider_updated = pv.get("last_updated") or pv.get("updated_at")
        for model, mv in (pv.get("models") or {}).items():
            if isinstance(mv, dict):
                entry = dict(mv)
                if provider_source and not entry.get("source"):
                    entry["source"] = provider_source
                if provider_updated and not entry.get("updated_at"):
                    entry["updated_at"] = provider_updated
                models[model] = _entry_to_v2(entry)
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
    meta = {"schema": "o2a-pricing/v2", "currency": DEFAULT_CURRENCY,
            **(raw.get("_meta") or {})}
    if raw.get("currency"):
        meta["currency"] = raw["currency"]
    if raw.get("version") is not None:
        meta["version"] = raw["version"]
    return {
        "_meta": meta,
        "models": models,
        "accounts": accounts,
        "services": raw.get("services") or {},
        "rules": rules,
    }