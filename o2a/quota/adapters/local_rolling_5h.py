"""local-rolling-5h 适配器：5 小时滚动窗（Claude Pro/Max 式，§8.2）。

窗口起点 = 当前 5h 窗内最早一条记录（近似 provider 的滚动窗重置时刻）；
limit 来自 accounts[].quota.limit（requests），未配置时只展示用量不显示百分比。
"""

from datetime import timedelta

from ..base import QuotaAdapter, QuotaContext, empty_window, make_snapshot
from ._stats_util import iter_records


class LocalRolling5hAdapter(QuotaAdapter):
    name = "local-rolling-5h"
    source = "local_stats"

    def fetch(self, ctx: QuotaContext):
        acc = ctx.account
        if acc is None:
            return None
        now = ctx.now()
        start = now - timedelta(hours=5)
        used = 0
        oldest = None
        for ts, _rec in iter_records(ctx.stats_dir, acc.id, start):
            used += 1
            if oldest is None or ts < oldest:
                oldest = ts
        reset_at = ctx.iso(oldest + timedelta(hours=5)) if oldest else None
        limit = (acc.quota or {}).get("limit")
        windows = [empty_window("rolling", "requests", used, limit, reset_at=reset_at)]
        return make_snapshot(self.name, windows, source=self.source, now_fn=ctx.now_fn)
