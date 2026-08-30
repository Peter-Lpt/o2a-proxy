"""declarative 适配器：用 accounts[].quota 声明式窗口，接入套餐目录。

quota_source="declarative" 时使用该适配器：
- accounts[].quota = {"plan": "glm-coding-plan"} 或
  {"windows": [{"kind":"month","unit":"requests","limit":200}], "plan": "my-plan"}
- 需要本地用量时通过 stats_dir 聚合（与 manual 一致）
- 不依赖外网，适合 Sub2API/New API 等“声明额度”场景
"""

from ..base import QuotaAdapter, QuotaContext, empty_window, make_snapshot, window_start
from ._stats_util import count_requests, count_tokens


class DeclarativeQuotaAdapter(QuotaAdapter):
    name = "declarative"
    source = "plan_config"

    def fetch(self, ctx: QuotaContext):
        acc = ctx.account
        if acc is None:
            return None
        cfg = acc.quota or {}
        plan_name = cfg.get("plan")
        windows_cfg = cfg.get("windows")
        if not plan_name and not windows_cfg:
            return None
        windows = []
        for w in windows_cfg or []:
            if not isinstance(w, dict):
                continue
            kind = w.get("kind", w.get("period", "month"))
            unit = w.get("unit", "requests")
            limit = w.get("limit")
            period = w.get("period", "month")
            if period not in ("day", "week", "month"):
                period = "month"
            start = window_start(ctx.now(), period)
            used = count_requests(ctx.stats_dir, acc.id, start) if unit == "requests" else \
                count_tokens(ctx.stats_dir, acc.id, start)
            windows.append(empty_window(kind, unit, used, limit,
                                        reset_at=ctx.iso(window_start(ctx.now(), "day"))))
        plan = None
        if plan_name:
            plan = {"name": plan_name, "included": cfg.get("included") or {},
                    "overage": cfg.get("overage") or {}, "free_tier": cfg.get("free_tier") or {}}
        return make_snapshot(self.name, windows or [], source=self.source,
                             plan=plan, now_fn=ctx.now_fn)