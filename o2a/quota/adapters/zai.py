"""Z.ai / GLM Coding Plan 适配器：quota + subscription 接口。

- 默认 base 从 accounts[].quota.url，缺省 https://open.bigmodel.cn/api/paas/v4
- 先尝试 /balance（OpenAI 风格余额），再尝试 /plan（套餐余量）
- 响应支持 {"data": {"usage": ..., "limit": ...}} 或 {"usage": ..., "limit": ...}
- 网络失败抛 QuotaError → 注册表降级 local 并标 stale
"""

from ..base import UPSTREAM_TIMEOUT_S, QuotaAdapter, QuotaContext, QuotaError, empty_window, make_snapshot


class ZaiAdapter(QuotaAdapter):
    name = "zai"
    source = "provider_api"

    async def fetch(self, ctx: QuotaContext):
        acc = ctx.account
        if acc is None or not acc.api_key or ctx.session is None:
            return None
        cfg = acc.quota or {}
        base = (cfg.get("url") or "https://open.bigmodel.cn/api/paas/v4").rstrip("/")
        last_err = None
        for path in ("/balance", "/plan"):
            url = base + path
            try:
                async with ctx.session.get(
                    url,
                    headers={"Authorization": f"Bearer {acc.api_key}"},
                    timeout=UPSTREAM_TIMEOUT_S,
                ) as resp:
                    if resp.status != 200:
                        last_err = QuotaError(f"zai {path} status {resp.status}")
                        continue
                    data = await resp.json(content_type=None)
                usage, limit = _extract(data)
                if usage is None and limit is None:
                    last_err = QuotaError(f"zai {path} no usage payload")
                    continue
                windows = [empty_window("month", "usd", usage or 0, limit)]
                return make_snapshot(self.name, windows, source=self.source, now_fn=ctx.now_fn)
            except Exception as e:
                last_err = QuotaError(f"zai request failed: {e}")
        raise last_err or QuotaError("zai unavailable")


def _extract(data: dict):
    if not isinstance(data, dict):
        return None, None
    obj = data.get("data") if isinstance(data.get("data"), dict) else data
    usage = obj.get("usage") or obj.get("used_quota") or obj.get("used")
    limit = obj.get("limit") or obj.get("total_quota") or obj.get("total")
    if isinstance(usage, dict):
        usage = usage.get("total") or usage.get("used") or usage.get("credits")
    if isinstance(limit, dict):
        limit = limit.get("total") or limit.get("limit")
    return usage, limit