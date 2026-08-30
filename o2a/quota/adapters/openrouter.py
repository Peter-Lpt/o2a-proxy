"""OpenRouter 适配器：GET /api/v1/key → usage / limit（provider_api）。

支持两种语义：
- 默认：/api/v1/key（API Key usage/limit）
- accounts[].quota.mode = "credits"：/api/v1/credits（Management Key credits）
统一快照仍为 windows[{kind, unit, used, limit}]。
"""

from ..base import UPSTREAM_TIMEOUT_S, QuotaAdapter, QuotaContext, QuotaError, empty_window, make_snapshot


class OpenRouterAdapter(QuotaAdapter):
    name = "openrouter"
    source = "provider_api"

    async def fetch(self, ctx: QuotaContext):
        acc = ctx.account
        if acc is None or not acc.api_key or ctx.session is None:
            return None
        cfg = acc.quota or {}
        mode = (cfg.get("mode") or "key").lower()
        path = "/api/v1/credits" if mode == "credits" else "/api/v1/key"
        url = (cfg.get("url") or "https://openrouter.ai").rstrip("/") + path
        try:
            async with ctx.session.get(
                url,
                headers={"Authorization": f"Bearer {acc.api_key}"},
                timeout=UPSTREAM_TIMEOUT_S,
            ) as resp:
                if resp.status != 200:
                    raise QuotaError(f"openrouter {path} status {resp.status}")
                data = (await resp.json(content_type=None)).get("data") or {}
        except QuotaError:
            raise
        except Exception as e:  # 网络错误一律转 QuotaError，由注册表降级
            raise QuotaError(f"openrouter request failed: {e}") from e
        usage = data.get("usage")
        limit = data.get("limit")
        if usage is None and limit is None:
            # credits 响应形态：{credits: {...}} 或 {used_credits, total_credits}
            usage = data.get("used_credits") or (
                (data.get("credits") or {}).get("used") if isinstance(data.get("credits"), dict) else None
            )
            limit = data.get("total_credits") or (
                (data.get("credits") or {}).get("total") if isinstance(data.get("credits"), dict) else None
            )
        if usage is None and limit is None:
            return None
        windows = [empty_window("month", "usd", usage or 0, limit)]
        return make_snapshot(self.name, windows, source=self.source, now_fn=ctx.now_fn)