"""OpenAI Codex / ChatGPT 订阅额度适配器：chatgpt.com/backend-api/wham/usage。

OpenAI 的 ChatGPT Plus / Pro / Codex 订阅走 OAuth 会话，不用普通平台 API key。
真正可用的 usage 端点是未公开的 `chatgpt.com/backend-api/wham/usage`，它返回：
- `rate_limit.primary_window` / `rate_limit.secondary_window`：5 小时滚动、每周配额
  （OpenAI 会按 plan 切换这两个字段的顺序，因此按 `limit_window_seconds` 归类更稳）
- `credits`：按量余额 / unlimited

实现参考：
- pi-codex-usage（inouemoby/pi-codex-usage）
- opencode-usage-dashboard（duysqubix/opencode-usage-dashboard）的 codex.js
- OpenChamber 的 provider usage 模块

配置：
    accounts[].quota_source = "codex" | "gpt" | "openai-codex"  # 或 chatgpt.com 自动嗅探
    accounts[].quota = {
      "access_token": "…",                       # 方式一：直接给 access token
      "refresh_token": "…",                      # 可选，过期自动刷新（尽力写回原文件）
      "token_file": "~/.codex/auth.json",         # 方式二：从 Codex / pi / OpenCode auth 文件读取
      "usage_url": "https://chatgpt.com/backend-api/wham/usage"  # 可选覆盖
    }

网络失败抛 QuotaError → 注册表降级 local 并标 stale（断网可用）。
"""
import json
import os
from datetime import datetime
from pathlib import Path

from ..base import UPSTREAM_TIMEOUT_S, QuotaAdapter, QuotaContext, QuotaError, empty_window, make_snapshot

# OAuth 客户端 id（pi-codex-usage 中公开使用的 Codex CLI 客户端；如需可配置）
DEFAULT_CLIENT_ID = "app_EMoamEEZ73f0CkXaXp7hrann"
TOKEN_URL = "https://auth.openai.com/oauth/token"
DEFAULT_USAGE_URL = "https://chatgpt.com/backend-api/wham/usage"


class OpenAICodexAdapter(QuotaAdapter):
    name = "codex"
    source = "provider_api"

    async def fetch(self, ctx: QuotaContext):
        acc = ctx.account
        if acc is None or ctx.session is None:
            return None
        cfg = acc.quota or {}
        token, refresh_token, token_file = _resolve_token(acc, cfg)
        if not token:
            raise QuotaError("codex: missing access token (set quota.access_token / quota.token_file, or login with Codex CLI)")

        usage_url = (cfg.get("usage_url") or "").strip() or DEFAULT_USAGE_URL
        try:
            async with ctx.session.get(
                usage_url,
                headers={"Authorization": f"Bearer {token}", "Accept": "application/json"},
                timeout=UPSTREAM_TIMEOUT_S,
            ) as resp:
                if resp.status == 401 and refresh_token:
                    refreshed = await _refresh_access_token(ctx.session, refresh_token)
                    token = refreshed["access_token"]
                    if refreshed.get("refresh_token") and token_file:
                        _write_back_token(token_file, refreshed["access_token"], refreshed["refresh_token"])
                    async with ctx.session.get(
                        usage_url,
                        headers={"Authorization": f"Bearer {token}", "Accept": "application/json"},
                        timeout=UPSTREAM_TIMEOUT_S,
                    ) as resp2:
                        if resp2.status != 200:
                            raise QuotaError(f"codex usage status {resp2.status}")
                        data = await resp2.json(content_type=None)
                elif resp.status != 200:
                    raise QuotaError(f"codex usage status {resp.status}")
                else:
                    data = await resp.json(content_type=None)
        except QuotaError:
            raise
        except Exception as e:
            raise QuotaError(f"codex request failed: {e}") from e

        windows, plan = _parse_usage(data, now_fn=ctx.now_fn)
        return make_snapshot(self.name, windows, source=self.source,
                             plan=plan, now_fn=ctx.now_fn)


def _resolve_token(acc, cfg: dict):
    """按优先级取 access/refresh/token_file：quota 显式 > Codex/pi/OpenCode auth 文件 > 账号 key。"""
    direct = cfg.get("access_token") or cfg.get("access")
    refresh = cfg.get("refresh_token") or cfg.get("refresh")
    token_file = cfg.get("token_file") or cfg.get("auth_file") or ""
    if direct:
        return str(direct), (str(refresh) if refresh else None), token_file
    if token_file:
        info = _token_from_file(token_file)
        if info and info.get("access"):
            return info["access"], info.get("refresh"), token_file
    # 显式让账号 key 充当订阅 access token（适合手动粘贴短期 token 的场景）
    if cfg.get("use_api_key") and acc.api_key:
        return acc.api_key, None, token_file
    return acc.api_key, None, token_file


def _token_from_file(token_file: str):
    path = _resolve_token_path(token_file)
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return None

    def _norm(tok):
        if not isinstance(tok, dict):
            return None
        access = tok.get("access_token") or tok.get("access") or tok.get("accessToken")
        if not access:
            return None
        refresh = tok.get("refresh_token") or tok.get("refresh") or tok.get("refreshToken")
        return {"access": access, "refresh": refresh}

    # 常见形态：~/.codex/auth.json → {tokens: {access_token, refresh_token}}
    if isinstance(data, dict):
        info = _norm(data.get("tokens"))
        if info:
            return info
        # ~/.pi/agent/auth.json → {openai-codex: {access, refresh, ...}}
        info = _norm(data.get("openai-codex"))
        if info:
            return info
        # OpenCode auth.json → {providers: {codex: {tokens: {access_token, ...}}}}
        if isinstance(data.get("providers"), dict):
            for provider in ("codex", "openai-codex", "openai"):
                pv = data["providers"].get(provider)
                if not isinstance(pv, dict):
                    continue
                info = _norm(pv.get("tokens") if isinstance(pv.get("tokens"), dict) else pv)
                if info:
                    return info
    return None


def _resolve_token_path(token_file: str) -> Path:
    path = Path(os.path.expanduser(token_file))
    if not path.is_absolute():
        # 相对路径按项目根解析，方便桌面绿色版
        from ...base import PROJECT_ROOT
        path = Path(PROJECT_ROOT) / path
    return path


def _write_back_token(token_file: str, access_token: str, refresh_token: str):
    """尽力把刷新后的 token 写回原 auth 文件（refresh token 一次性，必须回写）。"""
    try:
        path = _resolve_token_path(token_file)
        data = json.loads(path.read_text(encoding="utf-8"))
        if not isinstance(data, dict):
            return
        # Codex CLI ~/.codex/auth.json
        if isinstance(data.get("tokens"), dict):
            data["tokens"]["access_token"] = access_token
            data["tokens"]["refresh_token"] = refresh_token
        # pi ~/.pi/agent/auth.json
        elif isinstance(data.get("openai-codex"), dict):
            data["openai-codex"]["access"] = access_token
            data["openai-codex"]["refresh"] = refresh_token
        # OpenCode auth.json providers
        elif isinstance(data.get("providers"), dict):
            for provider in ("codex", "openai-codex", "openai"):
                pv = data["providers"].get(provider)
                if not isinstance(pv, dict):
                    continue
                if isinstance(pv.get("tokens"), dict):
                    pv["tokens"]["access_token"] = access_token
                    pv["tokens"]["refresh_token"] = refresh_token
                else:
                    pv["access_token"] = access_token
                    pv["refresh_token"] = refresh_token
                break
        else:
            return
        tmp = path.with_suffix(path.suffix + ".tmp")
        tmp.write_text(json.dumps(data, ensure_ascii=False, indent=2), encoding="utf-8")
        tmp.replace(path)
    except Exception:
        # 写回失败不影响额度查询；下次可能需重新登录。
        pass


def _refresh_access_token(session, refresh_token: str):
    """用 refresh token 换新 access token，并带回可能轮换的新 refresh token。"""
    async def _do():
        body = {
            "grant_type": "refresh_token",
            "refresh_token": refresh_token,
            "client_id": DEFAULT_CLIENT_ID,
        }
        async with session.post(
            TOKEN_URL,
            data=body,
            headers={"Content-Type": "application/x-www-form-urlencoded"},
            timeout=UPSTREAM_TIMEOUT_S,
        ) as resp:
            if resp.status != 200:
                raise QuotaError(f"codex token refresh status {resp.status}")
            data = await resp.json(content_type=None)
        access = data.get("access_token")
        if not access:
            raise QuotaError("codex token refresh missing access_token")
        return {
            "access_token": access,
            "refresh_token": data.get("refresh_token"),
        }

    return _do()


def _parse_usage(data: dict, now_fn=None):
    """把 wham/usage JSON 归一化为 windows + plan。

    5h 与 weekly 按 `limit_window_seconds` 归类：≤6h 视为 5h 滚动窗，
    其余按每周配额。辅助 rate limit 与 credits 尽力透出。
    """
    windows = []
    rate_limit = data.get("rate_limit") or {}
    for w in (rate_limit.get("primary_window"), rate_limit.get("secondary_window")):
        if not isinstance(w, dict):
            continue
        seconds = int(w.get("limit_window_seconds") or 0)
        used_pct = w.get("used_percent")
        if used_pct is None:
            continue
        kind = "rolling" if 0 < seconds <= 21600 else "weekly"
        reset_at = None
        reset_ts = w.get("reset_at")
        if reset_ts:
            try:
                reset_at = datetime.fromtimestamp(int(reset_ts)).strftime("%Y-%m-%dT%H:%M:%S")
            except (OSError, ValueError, TypeError):
                reset_at = None
        windows.append(empty_window(kind, "percent", float(used_pct), 100, reset_at=reset_at))

    # 附加模型级限制（如 Codex Spark），命名 role 前缀避免与主窗混淆。
    extra = data.get("additional_rate_limits") or []
    for idx, item in enumerate(extra):
        if not isinstance(item, dict):
            continue
        lim = item.get("rate_limit") if isinstance(item.get("rate_limit"), dict) else item
        win = lim.get("primary_window") if isinstance(lim, dict) and isinstance(lim.get("primary_window"), dict) else None
        if win and win.get("used_percent") is not None:
            label = str(item.get("name") or item.get("label") or item.get("id") or f"extra-{idx}")
            windows.append(empty_window(f"{label}", "percent", float(win["used_percent"]), 100, reset_at=None))

    # credits / unlimited
    credits = data.get("credits") or {}
    if credits.get("unlimited"):
        windows.append({
            "kind": "credits", "unit": "usd", "used": 0, "limit": None,
            "pct": None, "reset_at": None, "value_label": "Unlimited",
        })
    elif credits.get("has_credits") or credits.get("balance") is not None:
        windows.append({
            "kind": "credits", "unit": "usd", "used": float(credits.get("balance") or 0),
            "limit": None, "pct": None, "reset_at": None,
            "value_label": f"${credits.get('balance') or 0}",
        })

    plan = {"name": str(data.get("plan_type") or "ChatGPT")}
    if rate_limit.get("limit_reached"):
        plan["limit_reached"] = True
    return windows, plan