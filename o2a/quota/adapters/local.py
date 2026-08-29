"""local 适配器：从本地 JSONL 聚合日/周/月窗口用量（兜底，任何 provider 都能用）。"""

from ..base import QuotaAdapter, QuotaContext, empty_window, make_snapshot, window_start
from ._stats_util import count_requests


class LocalQuotaAdapter(QuotaAdapter):
    """按日/周/月窗口聚合本地统计。limit 可由 accounts[].quota 配置。"""

    name = "local"
    source = "local_stats"

    def fetch(self, ctx: QuotaContext):
        acc = ctx.account
        if acc is None:
            return None
        now = ctx.now()
        windows = []
        cfg_quota = acc.quota or {}
        period = cfg_quota.get("period", "month")
        limit = cfg_quota.get("limit")
        unit = cfg_quota.get("unit", "requests")
        kinds = [period] if period in ("day", "week", "month") else ["month"]
        for kind in kinds:
            start = window_start(now, kind)
            used = count_requests(ctx.stats_dir, acc.id, start)
            reset_at = ctx.iso(window_start(now, "day")) if kind == "day" else None
            windows.append(empty_window(kind, "requests", used, limit if unit == "requests" else None,
                                        reset_at=reset_at))
        return make_snapshot(self.name, windows, source=self.source, now_fn=ctx.now_fn)
