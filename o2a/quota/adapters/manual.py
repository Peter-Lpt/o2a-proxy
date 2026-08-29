"""manual 适配器：config 手填额度（accounts[].quota），冷启动兜底。

quota = {"limit": 200, "unit": "requests" | "tokens" | "usd", "period": "day" | "week" | "month"}
用量来自本地统计（usd 单位暂用本地费用汇总的替代口径：请求数 × quota.limit 比例）。
"""

from ..base import QuotaAdapter, QuotaContext, empty_window, make_snapshot, window_start
from ._stats_util import count_requests, count_tokens


class ManualQuotaAdapter(QuotaAdapter):
    name = "manual"
    source = "plan_config"

    def fetch(self, ctx: QuotaContext):
        acc = ctx.account
        if acc is None:
            return None
        cfg = acc.quota or {}
        limit = cfg.get("limit")
        if not limit:
            return None
        unit = cfg.get("unit", "requests")
        period = cfg.get("period", "month")
        if period not in ("day", "week", "month"):
            period = "month"
        start = window_start(ctx.now(), period)
        if unit == "tokens":
            used = count_tokens(ctx.stats_dir, acc.id, start)
        elif unit == "usd":
            # 本地暂无逐条费用聚合窗口：以请求数近似展示（limit 只做刻度）
            used = count_requests(ctx.stats_dir, acc.id, start)
        else:
            used = count_requests(ctx.stats_dir, acc.id, start)
        windows = [empty_window(period, unit, used, limit,
                                reset_at=ctx.iso(window_start(ctx.now(), "day")))]
        plan = {"name": f"{acc.name} 套餐", "period": period,
                "included": {"unit": unit, "amount": limit}}
        return make_snapshot(self.name, windows, source=self.source, plan=plan,
                             now_fn=ctx.now_fn)
