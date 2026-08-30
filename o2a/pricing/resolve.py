"""pricing 覆盖链解析：服务级 > 账号级 > 模型级，并支持 v3 规则事件时间选择。

账号键兼容 v1 语义：accounts 段的键可为账号 id 或 name（与 auth.json 同规），
调用方传入候选键序列（id 优先，name 兜底）。
v3 规则采用“最具体 scope 优先”：service+account+model 精确 > 单维 > 通配；
同一层内按事件时间命中。
"""

from .evaluate import evaluate_entry
from .schema import normalize_pricing


def _parse_ts(value):
    """解析时间戳。支持 "YYYY-MM-DDTHH:MM:SS"、ISO8601 带时区；解析失败返回 None。"""
    if not value:
        return None
    import datetime as _dt
    s = str(value).strip()
    try:
        return _dt.datetime.fromisoformat(s.replace("Z", "+00:00"))
    except ValueError:
        try:
            return _dt.datetime.strptime(s[:19], "%Y-%m-%dT%H:%M:%S")
        except (ValueError, TypeError):
            return None


def _in_interval(rule_time, effective_from, effective_to):
    """时间点是否在区间内：from 含、to 不含。None 表示无界。"""
    rule_time = _parse_ts(rule_time)
    if rule_time is None:
        return False
    if effective_from:
        f = _parse_ts(effective_from)
        if f is None or _normalize_compare(rule_time, f) < 0:
            return False
    if effective_to:
        t = _parse_ts(effective_to)
        if t is None or _normalize_compare(rule_time, t) >= 0:
            return False
    return True


def _normalize_compare(a, b):
    """naive/aware 混合比较时统一为 UTC/naive 的比较。"""
    import datetime as _dt
    if a.tzinfo is None and b.tzinfo is not None:
        a = a.replace(tzinfo=_dt.timezone.utc)
    elif b.tzinfo is None and a.tzinfo is not None:
        b = b.replace(tzinfo=_dt.timezone.utc)
    return (a if a.tzinfo is None else a.astimezone(_dt.timezone.utc)) \
        .timestamp() - (b if b.tzinfo is None else b.astimezone(_dt.timezone.utc)).timestamp()


def _rule_matches(rule, model, service_id, account_keys, timestamp):
    """返回 (是否命中, 具体度)。具体度 (service_exact, account_exact, model_exact)。"""
    scope = rule.get("scope") or {}
    r_model = rule.get("model") or "*"
    if r_model != "*" and r_model != model:
        return False, None
    svc = scope.get("service", "*")
    if svc != "*" and svc != service_id:
        return False, None
    acc = scope.get("account", "*")
    if acc != "*" and acc not in (account_keys or []):
        return False, None
    if not _in_interval(timestamp, rule.get("effective_from"), rule.get("effective_to")):
        return False, None
    score = (svc != "*", acc != "*", r_model != "*")
    return True, score


def _select_rule(rules, model, service_id, account_keys, timestamp):
    best = None
    best_score = None
    for rule in rules:
        hit, score = _rule_matches(rule, model, service_id, account_keys, timestamp)
        if not hit:
            continue
        # 具体度高者优先；同具体度取先出现者（规则表已校验区间不重叠）
        if best_score is None or score > best_score:
            best = rule
            best_score = score
    return best


def resolve_entry(raw_pricing: dict, model: str, account_keys=None, service_id: str = None,
                  timestamp: str = None):
    """解析某模型在（服务、账号、事件时间）上下文下的定价条目。

    优先使用 v3 rules 的事件时间规则；无 rules 时回退 v1/v2 覆盖链。
    返回 v2 entry dict；未命中返回 None。
    """
    if not raw_pricing:
        return None
    data = normalize_pricing(raw_pricing)
    rules = data.get("rules") or []
    if rules:
        entry = _select_rule(rules, model, service_id, account_keys, timestamp)
        if entry is not None:
            return dict(entry)
        # 有 rules 但未命中：返回 None，避免用当前价冒充历史
        return None
    return _resolve_v1_v2(data, model, account_keys, service_id)


def _resolve_v1_v2(data, model, account_keys=None, service_id=None):
    if service_id:
        svc = data.get("services", {}).get(service_id) or {}
        sm = svc.get("models") or {}
        entry = sm.get(model) or sm.get("*")
        if isinstance(entry, dict):
            return _ensure_v2(entry)
    for key in account_keys or []:
        acc = data.get("accounts", {}).get(key)
        if isinstance(acc, dict):
            entry = (acc.get("models") or {}).get(model)
            if isinstance(entry, dict):
                return _ensure_v2(entry)
    entry = data.get("models", {}).get(model)
    if isinstance(entry, dict):
        return _ensure_v2(entry)
    return None


def _ensure_v2(entry: dict) -> dict:
    from .schema import _entry_to_v2
    return _entry_to_v2(entry)


def resolve_cost(raw_pricing: dict, model: str, input_tokens=0, cache_read=0,
                 cache_write=0, output_tokens=0, reasoning_tokens=0, requests=0,
                 account_keys=None, service_id: str = None, timestamp: str = None,
                 context_tokens: int = None, cumulative_tokens: int = None,
                 meta: dict = None) -> dict:
    """resolve + evaluate 一步到位。

    返回值包含 complete/currency/rule_id/source/provenance；
    未命中定价时 total=0、complete=False（旧“0 元”行为仍保留，UI 可按 complete 展示 —）。
    """
    entry, approximate = None, False
    if raw_pricing:
        data = normalize_pricing(raw_pricing)
        if data.get("rules"):
            rule = _select_rule(data["rules"], model, service_id, account_keys, timestamp)
            if rule is not None:
                entry = dict(rule)
                approximate = False
            else:
                entry = _resolve_v1_v2(data, model, account_keys, service_id)
                approximate = True
        else:
            entry = _resolve_v1_v2(data, model, account_keys, service_id)
    if entry is None:
        return {"total": 0.0, "currency": _currency_of(raw_pricing),
                "complete": False, "breakdown": {},
                "applied": [], "approximate": approximate}
    ctx = {}
    if timestamp:
        ctx["timestamp"] = timestamp
    meta_full = {}
    if context_tokens is not None:
        meta_full["context_tokens"] = context_tokens
    if meta:
        meta_full.update(meta)
    if meta_full:
        ctx["meta"] = meta_full
    if cumulative_tokens is not None:
        ctx["cumulative"] = {"tokens": cumulative_tokens}
    result = evaluate_entry(entry, input_tokens, cache_read, cache_write,
                            output_tokens, reasoning_tokens, requests,
                            ctx=ctx or None)
    result["currency"] = entry.get("currency") or _currency_of(raw_pricing)
    if entry.get("rule_id"):
        result["rule_id"] = entry["rule_id"]
    if entry.get("source"):
        result["source"] = entry["source"]
    if entry.get("updated_at"):
        result["updated_at"] = entry["updated_at"]
    result["approximate"] = approximate
    return result


def _currency_of(raw_pricing: dict) -> str:
    if not isinstance(raw_pricing, dict):
        return "CNY"
    if raw_pricing.get("currency"):
        return raw_pricing["currency"]
    meta = raw_pricing.get("_meta")
    if isinstance(meta, dict) and meta.get("currency"):
        return meta["currency"]
    return "CNY"