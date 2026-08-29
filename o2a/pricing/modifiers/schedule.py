"""schedule modifier：时段/星期命中后覆盖单价或乘系数（§7.4，峰谷计价）。

用法：
{
  "type": "schedule",
  "windows": [
    {"days": ["sat", "sun"], "override": {"input": 1.0, "output": 2.0}, "note": "周末价"},
    {"days": ["mon", "tue", "wed", "thu", "fri"], "from": "22:00", "to": "08:00",
     "override": {"input": 1.0, "output": 2.0}, "note": "错峰 5 折"},
    {"days": ["mon", "tue", "wed", "thu", "fri"], "from": "08:00", "to": "22:00",
     "override": {"input": 4.0, "output": 16.0}, "note": "高峰"}
  ],
  "fallback": {"input": 1.5, "output": 4.5}
}

- 命中第一个匹配的 window：override 覆盖同名单价键 / factor 乘全部分量
- days 缺省 = 全部星期；from/to 支持跨天区间（22:00→08:00）
- 无命中窗口时用 fallback（若有）
- 时间取 ctx.timestamp（记录本地时间 "YYYY-MM-DDTHH:MM:SS"）——
  读取端重算历史账单用记录自身的时间戳，改价/换季不改写历史口径（§7.3）
- timestamp 缺失时 schedule 不生效（如个别只传 meta 的调用路径）
"""

_DAYS = ("mon", "tue", "wed", "thu", "fri", "sat", "sun")


def _weekday(ts: str):
    """本地时间戳 → (weekday 名, "HH:MM")。解析失败返回 None。"""
    try:
        from datetime import datetime
        dt = datetime.strptime(ts[:19], "%Y-%m-%dT%H:%M:%S")
        return _DAYS[dt.weekday()], dt.strftime("%H:%M")
    except (ValueError, TypeError, IndexError):
        return None


def _in_window(weekday: str, hhmm: str, window: dict) -> bool:
    days = window.get("days")
    if days and weekday not in days:
        return False
    w_from = window.get("from")
    w_to = window.get("to")
    if not w_from or not w_to:
        return True  # 无时间区间 = 全天
    if w_from <= w_to:
        return w_from <= hhmm < w_to
    # 跨天区间（22:00→08:00）
    return hhmm >= w_from or hhmm < w_to


def _replace(comps: dict, patch: dict) -> dict:
    out = dict(comps)
    for k, v in (patch or {}).items():
        if k in out or k in ("input", "output", "cache_read", "cache_write", "request",
                             "output_thinking"):
            out[k] = float(v)
    return out


def apply(components: dict, ctx: dict, mod: dict):
    ts = ctx.get("timestamp")
    if not ts:
        return components, []
    wd = _weekday(ts)
    if wd is None:
        return components, []
    weekday, hhmm = wd
    for w in mod.get("windows") or []:
        if not isinstance(w, dict):
            continue
        if _in_window(weekday, hhmm, w):
            if "override" in w:
                comps = _replace(components, w["override"])
            else:
                factor = float(w.get("factor", 1.0))
                comps = {k: v * factor for k, v in components.items()}
            return comps, [w.get("note") or "schedule"]
    fallback = mod.get("fallback")
    if isinstance(fallback, dict):
        return _replace(components, fallback), [mod.get("note") or "schedule:fallback"]
    return components, []
