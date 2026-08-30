"""o2a-quota：订阅额度适配器。

隔离原则（，用户点名要求）：
1. 目录隔离：本包 = 基类 + 注册表 + 每适配器一文件；新增供应商 = 新文件 + 注册一行，
   不碰引擎主链路。
2. 依赖隔离：适配器只能通过 QuotaContext（统计目录 / 账号信息 / http session /
   now()）取数，禁止直接读 config.json / 全局状态 → 可单测。
3. 失败隔离：单适配器抛错/超时 → 返回 None（上层降级 local 并标 stale），
   绝不影响统计页其余渲染与代理主流程。
4. 端隔离：引擎提供 GET /quota?account=<id>；Rust 只做转发与缓存，
   额度只存在这一份实现（与计价的双端一致不同：额度只要"此刻"快照）。
5. 前端隔离：QuotaCard.vue 只认 QuotaSnapshot，不知道任何供应商细节。
"""

from .base import QuotaContext, QuotaError, TTLCache
from .registry import get_snapshot, get_snapshot_async, resolve_adapter_name

__all__ = [
    "QuotaContext",
    "QuotaError",
    "TTLCache",
    "get_snapshot",
    "get_snapshot_async",
    "resolve_adapter_name",
]
