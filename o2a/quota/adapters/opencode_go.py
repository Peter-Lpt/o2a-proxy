"""OpenCode Go 适配器：Bearer usage endpoint / dashboard fallback。

- 默认从 accounts[].quota.url 读取（或 https://api.opencode.ai）
- 请求 {base}/v1/usage（兼容 OpenAI-style usage）或 {base}/usage
- 响应支持 {"usage": ..., "limit": ...} 或 {"data": {"usage": ..., "limit": ...}}
- 网络失败抛 QuotaError → 注册表降级 local 并标 stale（断网可用）
"""

from ..base import UPSTREAM_TIMEOUT_S, QuotaAdapter, QuotaContext, QuotaError, empty_window, make_snapshot


class OpenCodeGoAdapter(QuotaAdapter):
    name = "opencode-go"
    source = "provider_api"

    async def fetch(self, ctx: QuotaContext):
        acc = ctx.account
        if acc is None or not acc.api_key or ctx.session is None:
            return None
        cfg = acc.quota or {}
        base = (cfg.get("url") or "https://api.opencode.ai").rstrip("/")
        last_err = None
        for path in ("/v1/usage", "/usage"):
            url = base + path
            try:
                async with ctx.session.get(
                    url,
                    headers={"Authorization": f"Bearer {acc.api_key}"},
                    timeout=UPSTREAM_TIMEOUT_S,
                ) as resp:
                    if resp.status != 200:
                        last_err = QuotaError(f"opencode-go {path} status {resp.status}")
                        continue
                    data = await resp.json(content_type=None)
                usage, limit = _extract(data)
                if usage is None and limit is None:
                    last_err = QuotaError(f"opencode-go {path} no usage payload")
                    continue
                windows = [empty_window("month", "usd", usage or 0, limit)]
                return make_snapshot(self.name, windows, source=self.source, now_fn=ctx.now_fn)
            except Exception as e:
                last_err = QuotaError(f"opencode-go request failed: {e}")
        raise last_err or QuotaError("opencode-go unavailable")


def _extract(data: dict):
    if not isinstance(data, dict):
        return None, None
    obj = data.get("data") if isinstance(data.get("data"), dict) else data
    usage = obj.get("usage")
    limit = obj.get("limit")
    if usage is None and isinstance(obj.get("data"), dict):
        usage = obj["data"].get("usage")
        limit = obj["data"].get("limit")
    if isinstance(usage, dict):
        usage = usage.get("total") or usage.get("used") or usage.get("credits")
    if isinstance(limit, dict):
        limit = limit.get("total") or limit.get("limit")
    return usage, limit