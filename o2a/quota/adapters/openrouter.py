"""OpenRouter 适配器：GET /api/v1/key → usage / limit（provider_api，§8.2）。"""

from ..base import UPSTREAM_TIMEOUT_S, QuotaAdapter, QuotaContext, QuotaError, empty_window, make_snapshot


class OpenRouterAdapter(QuotaAdapter):
    name = "openrouter"
    source = "provider_api"

    async def fetch(self, ctx: QuotaContext):
        acc = ctx.account
        if acc is None or not acc.api_key or ctx.session is None:
            return None
        url = "https://openrouter.ai/api/v1/key"
        try:
            async with ctx.session.get(
                url,
                headers={"Authorization": f"Bearer {acc.api_key}"},
                timeout=UPSTREAM_TIMEOUT_S,
            ) as resp:
                if resp.status != 200:
                    raise QuotaError(f"openrouter key api status {resp.status}")
                data = (await resp.json(content_type=None)).get("data") or {}
        except QuotaError:
            raise
        except Exception as e:  # 网络错误一律转 QuotaError，由注册表降级
            raise QuotaError(f"openrouter request failed: {e}") from e
        usage = data.get("usage")
        limit = data.get("limit")
        if usage is None and limit is None:
            return None
        windows = [empty_window("month", "usd", usage or 0, limit)]
        return make_snapshot(self.name, windows, source=self.source, now_fn=ctx.now_fn)
