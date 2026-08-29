"""quota 基础设施：上下文、快照结构、适配器协议（§8.2/§8.3）。"""

import threading
from datetime import datetime, timedelta

# 上游请求超时：永不阻塞主流程（§8.3）
UPSTREAM_TIMEOUT_S = 1.5


class QuotaError(Exception):
    """适配器内部错误（由注册表捕获并降级，不外泄）。"""


class QuotaContext:
    """适配器取数上下文（§8.4-2 依赖隔离：适配器只允许从这里拿数据）。

    - stats_dir：JSONL 统计目录（local 系适配器聚合用）
    - account：o2a.config.Account（只读使用）
    - session：aiohttp.ClientSession（upstream 适配器发请求用；可为 None）
    - now_fn：可注入时钟（单测用），默认本地时间
    """

    def __init__(self, stats_dir=None, account=None, session=None, now_fn=None):
        self.stats_dir = stats_dir
        self.account = account
        self.session = session
        self.now_fn = now_fn or datetime.now

    def now(self) -> datetime:
        return self.now_fn()

    def iso(self, dt) -> str:
        return dt.strftime("%Y-%m-%dT%H:%M:%S")


def empty_window(kind, unit="requests", used=0, limit=None, reset_at=None):
    """构造一个窗口条目。"""
    used = float(used or 0)
    limit = float(limit) if limit else None
    pct = round(used / limit * 100, 1) if limit else None
    return {"kind": kind, "unit": unit, "used": used, "limit": limit,
            "reset_at": reset_at, "pct": pct}


def make_snapshot(adapter_id, windows, scope="account", source="local_stats",
                  plan=None, stale=False, now_fn=None):
    """构造 QuotaSnapshot（§8.2 归一化结构，前端 QuotaCard 只认这个形状）。"""
    now = (now_fn or datetime.now)()
    return {
        "adapterId": adapter_id,
        "scope": scope,
        "source": source,
        "fetched_at": now.strftime("%Y-%m-%dT%H:%M:%S"),
        "stale": stale,
        "windows": windows or [],
        "plan": plan,
    }


class QuotaAdapter:
    """适配器协议：fetch(account_ctx) -> snapshot dict | None。

    fetch 内允许抛 QuotaError / 任意异常 —— 注册表统一捕获降级。
    """

    name = ""
    source = "local_stats"

    def fetch(self, ctx: QuotaContext):
        raise NotImplementedError


class TTLCache:
    """额度查询 TTL 缓存（60s，§8.3）：面板隐藏时不刷新由调用方保证。"""

    def __init__(self, ttl_s=60):
        self.ttl_s = ttl_s
        self._data = {}
        self._lock = threading.Lock()

    def get(self, key):
        with self._lock:
            item = self._data.get(key)
            if not item:
                return None
            ts, value = item
            if (datetime.now() - ts).total_seconds() < self.ttl_s:
                return value
            return None

    def set(self, key, value):
        with self._lock:
            self._data[key] = (datetime.now(), value)

    def stale(self, key):
        """过期但尚存的旧值（上游失败时降级展示用）。"""
        with self._lock:
            item = self._data.get(key)
            if not item:
                return None
            value = dict(item[1])
            value["stale"] = True
            return value


# 供 local 系适配器共用的窗口起点
def window_start(now: datetime, kind: str) -> datetime:
    if kind == "day":
        return now.replace(hour=0, minute=0, second=0, microsecond=0)
    if kind == "week":
        start = now.replace(hour=0, minute=0, second=0, microsecond=0)
        return start - timedelta(days=start.weekday())
    if kind == "month":
        return now.replace(day=1, hour=0, minute=0, second=0, microsecond=0)
    raise ValueError(f"unknown window kind: {kind}")
