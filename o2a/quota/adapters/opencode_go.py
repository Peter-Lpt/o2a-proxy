"""OpenCode Go 适配器：Cookie 工作区用量页优先，Bearer usage endpoint 兜底。

OpenCode Go 订阅额度目前没有公开的 API usage endpoint：
- 可用的官方路径是登录后的工作区页面 `/workspace/<wrk>/go`，SSR HTML 里
  `data-slot="usage-item"` 直接内联了滚动 / 每周 / 每月的用量百分比与重置时间。
- 实测 OpenCode Go 网关自身的 OpenAI 兼容端点上，`GET {base}/usage` 也会返回
  `{"usage": {"rolling": {...}, "weekly": {...}, "monthly": {...}}}`，
  因此优先从账号已有的 openai_url 推导 base 并请求 `/usage`；失败再走 SSR/旧形态。

配置：
    accounts[].quota_source = "opencode-go"  # 或 auto 按域名 opencode.ai 嗅探
    accounts[].quota = {
      "cookie": "auth=fe26...; oc_locale=zh",   # 可选：需要工作区 SSR 时
      "workspace_id": "wrk_0123...",             # 可选
      "url": "https://opencode.ai/zen/go/v1"     # 可选；缺省用账号 openai_url 推导
    }

网络失败抛 QuotaError → 注册表降级 local 并标 stale（断网可用）。
"""
import re
from datetime import datetime, timedelta

from ..base import UPSTREAM_TIMEOUT_S, QuotaAdapter, QuotaContext, QuotaError, empty_window, make_snapshot


class OpenCodeGoAdapter(QuotaAdapter):
    name = "opencode-go"
    source = "provider_api"

    async def fetch(self, ctx: QuotaContext):
        acc = ctx.account
        if acc is None or ctx.session is None:
            return None
        cfg = acc.quota or {}
        base = _resolve_base(acc, cfg)
        last_err = None

        # 1) 从账号 OpenAI 兼容端点直接读 usage（实测 {base}/usage 返回订阅窗口）
        if acc.api_key:
            paths = ["/usage"]
            if not base.endswith("/v1"):
                paths.append("/v1/usage")
            for path in paths:
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
                    windows = _extract_v2(data, ctx.now())
                    if windows:
                        return make_snapshot(self.name, windows, source=self.source, now_fn=ctx.now_fn)
                    usage, limit = _extract(data)
                    if usage is None and limit is None:
                        last_err = QuotaError(f"opencode-go {path} no usage payload")
                        continue
                    windows = [empty_window("month", "usd", usage or 0, limit)]
                    return make_snapshot(self.name, windows, source=self.source, now_fn=ctx.now_fn)
                except Exception as e:
                    last_err = QuotaError(f"opencode-go request failed: {e}")

        # 2) Cookie + workspace 路径：读取 OpenCode Go 控制台 SSR 页面
        cookie = cfg.get("cookie")
        workspace_id = cfg.get("workspace_id") or cfg.get("workspaceID")
        if cookie and workspace_id:
            try:
                html = await _get_text(ctx.session, f"{base}/workspace/{_quote(workspace_id)}/go",
                                       headers={"Cookie": cookie, "Accept": "text/html"})
                windows = _parse_ssr_windows(html, ctx.now())
                if windows:
                    return make_snapshot(self.name, windows, source=self.source, now_fn=ctx.now_fn)
                last_err = QuotaError("opencode-go workspace page parsed empty (cookie expired or invalid?)")
            except Exception as e:
                last_err = QuotaError(f"opencode-go workspace request failed: {e}")

        # 3) 兼容兜底：Bearer usage endpoint（旧形态 OpenAI-style usage / limit）
        if acc.api_key:
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


def _resolve_base(acc, cfg: dict) -> str:
    """优先 quota.url；否则由账号 openai_url 去掉 /chat/completions 推导。"""
    if cfg.get("url"):
        return str(cfg["url"]).rstrip("/")
    oa = (getattr(acc, "openai_url", "") or "").strip()
    if oa.endswith("/chat/completions"):
        return oa[: -len("/chat/completions")].rstrip("/")
    return oa.rstrip("/") or "https://api.opencode.ai"


async def _get_text(session, url: str, headers: dict) -> str:
    async with session.get(url, headers=headers, timeout=UPSTREAM_TIMEOUT_S) as resp:
        if resp.status != 200:
            raise QuotaError(f"HTTP {resp.status} for {url}")
        return await resp.text()


def _quote(value: str) -> str:
    # 工作区 id 形如 wrk_xxxx，仅做安全转义；避免引入 urllib 额外依赖。
    return value.replace("/", "%2F").replace("?", "%3F").replace("#", "%23")


def _extract_v2(data: dict, now):
    """解析 OpenCode Go 网关返回的订阅窗口：
    {"usage": {"rolling": {"percent":10,"resetsAt":"..."}, "weekly": ..., "monthly": ...}}"""
    if not isinstance(data, dict):
        return []
    obj = data.get("data") if isinstance(data.get("data"), dict) else data
    usage = obj.get("usage")
    if not isinstance(usage, dict):
        return []
    windows = []
    for kind in ("rolling", "weekly", "monthly"):
        w = usage.get(kind)
        if not isinstance(w, dict) or w.get("percent") is None:
            continue
        percent = max(0, min(100, float(w.get("percent"))))
        reset_at = _parse_resets_at(w.get("resetsAt") or w.get("reset_at"))
        windows.append(empty_window(kind, "percent", used=percent, limit=100, reset_at=reset_at))
    return windows


def _parse_resets_at(value):
    if not value:
        return None
    try:
        dt = datetime.fromisoformat(str(value).replace("Z", "+00:00"))
        return dt.astimezone().strftime("%Y-%m-%dT%H:%M:%S")
    except (ValueError, TypeError, OSError):
        return None


def _parse_ssr_windows(html: str, now):
    """解析 OpenCode Go 工作区 SSR HTML 中的用量窗口。

    返回统一 windows 列表，unit="percent"（used 是 0-100 的整数百分比）。
    SSR 里的 reset 文案是英文 "Resets in ..." 或中文 "重置于 ..."。
    """
    windows = []
    item_start_re = re.compile(r'<div[^>]*data-slot="usage-item"')
    starts = [m.start() for m in item_start_re.finditer(html)]
    for i, start in enumerate(starts):
        block = html[start:starts[i + 1] if i + 1 < len(starts) else len(html)]
        label_match = re.search(r'data-slot="usage-label"[^>]*>([^<]+)<', block)
        value_match = re.search(r'data-slot="usage-value"[\s\S]*?<!--\$-->\s*(\d+)\s*<!--\/-->', block)
        reset_match = re.search(
            r'data-slot="reset-time"[\s\S]*?(?:Resets in|重置于)(?:<!--\/-->\s*)?([\s\S]*?)(?:<!--\/-->|</span>|</div>)',
            block,
        )
        if not label_match or not value_match:
            continue
        kind = _label_to_kind(label_match.group(1).strip())
        if not kind:
            continue
        percent = max(0, min(100, int(value_match.group(1))))
        reset_sec = _parse_duration_to_sec(reset_match.group(1) if reset_match else "")
        reset_at = ctx_iso(now + timedelta(seconds=reset_sec)) if reset_sec else None
        windows.append(empty_window(kind, "percent", used=percent, limit=100, reset_at=reset_at))
    return windows


def _label_to_kind(label: str):
    lower = label.lower()
    if lower.startswith("rolling") or lower.startswith("滚动"):
        return "rolling"
    if lower.startswith("weekly") or lower.startswith("每周"):
        return "weekly"
    if lower.startswith("monthly") or lower.startswith("每月"):
        return "monthly"
    return None


def _parse_duration_to_sec(phrase: str) -> int:
    """把 "2 hours 29 minutes" / "2 小时 29 分钟" 解析为秒（粗略展示用）。"""
    if not phrase:
        return 0
    cleaned = re.sub(r"<!--[\s\S]*?-->", " ", phrase).strip().lower()
    total = 0
    for m in re.finditer(r"(\d+)\s*(?:个\s*)?(second|minute|hour|day|week|month|year|秒|分钟|小时|天|周|月|年)s?", cleaned):
        n = int(m.group(1))
        unit = m.group(2)
        total += {
            "second": 1, "秒": 1,
            "minute": 60, "分钟": 60,
            "hour": 3600, "小时": 3600,
            "day": 86400, "天": 86400,
            "week": 604800, "周": 604800,
            "month": 2592000, "月": 2592000,
            "year": 31536000, "年": 31536000,
        }.get(unit, 0) * n
    return total


def ctx_iso(dt):
    return dt.strftime("%Y-%m-%dT%H:%M:%S")


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