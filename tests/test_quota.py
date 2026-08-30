"""额度适配器单元测试。

覆盖：
- QuotaSnapshot 归一化结构（adapterId/scope/source/windows/plan/stale）
- local-rolling-5h 窗口边界（5h 内计入 / 5h 外不计）
- local 日/周/月窗口
- manual 手填额度（plan_config 来源）
- declarative / opencode-go / zai / OpenRouter credits
- 注册表隔离：auto 域名嗅探、显式名、未注册名回退 local
- 失败降级：适配器抛错 → local 兜底标 stale，绝不外泄异常
- TTL 缓存命中与 stale 降级

运行方式：
    python -m pytest test_quota.py -v
"""
import json
import os
import sys
from datetime import datetime, timedelta

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import pytest

from o2a.config import Account
from o2a.quota import QuotaContext, QuotaError, resolve_adapter_name
from o2a.quota.base import TTLCache, empty_window, make_snapshot, window_start
from o2a.quota.registry import get_snapshot, registered_adapters
from o2a.quota.adapters._stats_util import iter_records
from o2a.quota.adapters.local import LocalQuotaAdapter
from o2a.quota.adapters.local_rolling_5h import LocalRolling5hAdapter
from o2a.quota.adapters.declarative import DeclarativeQuotaAdapter
from o2a.quota.adapters.opencode_go import OpenCodeGoAdapter
from o2a.quota.adapters.zai import ZaiAdapter
from o2a.quota.adapters.openrouter import OpenRouterAdapter


def make_account(**kw):
    kw.setdefault("openai_url", "https://api.example.com/v1")
    return Account(id="acc-1", name="测试账号", api_key="sk-x", **kw)


def write_jsonl(stats_dir, records):
    os.makedirs(stats_dir, exist_ok=True)
    by_date = {}
    for ts, rec in records:
        by_date.setdefault(ts[:10], []).append({**rec, "timestamp": ts, "account": "acc-1"})
    for ds, recs in by_date.items():
        with open(os.path.join(stats_dir, f"{ds}.jsonl"), "a", encoding="utf-8") as f:
            for r in recs:
                f.write(json.dumps(r, ensure_ascii=False) + "\n")


def rec(ts, model="m1"):
    return (ts, {"model": model, "input_tokens": 10, "output_tokens": 5,
                 "cache_read_tokens": 0, "cache_write_tokens": 0})


# ---------- 快照结构 ----------

def test_snapshot_shape():
    snap = make_snapshot("local-rolling-5h", [empty_window("rolling", "requests", 3, 200)],
                         source="local_stats", plan={"name": "pro"})
    assert snap["adapterId"] == "local-rolling-5h"
    assert snap["scope"] == "account"
    assert snap["stale"] is False
    w = snap["windows"][0]
    assert w["used"] == 3 and w["limit"] == 200 and w["pct"] == 1.5


# ---------- local-rolling-5h 窗口边界 ----------

def test_rolling_5h_boundary(tmp_path):
    now = datetime(2026, 1, 10, 12, 0, 0)
    stats_dir = str(tmp_path / "stats")
    # 窗口内 3 条（5h 内），窗口外 2 条（>5h）
    write_jsonl(stats_dir, [
        rec("2026-01-10T07:00:00"),  # 恰在 5h 边界上（>=）
        rec("2026-01-10T09:30:00"),
        rec("2026-01-10T11:59:00"),
        rec("2026-01-10T06:59:59"),  # 窗口外
        rec("2026-01-09T23:00:00"),  # 窗口外
    ])
    ctx = QuotaContext(stats_dir=stats_dir, account=make_account(quota={"limit": 100}),
                       now_fn=lambda: now)
    snap = LocalRolling5hAdapter().fetch(ctx)
    w = snap["windows"][0]
    assert w["used"] == 3
    assert w["reset_at"] == "2026-01-10T12:00:00"  # 最早记录 07:00 + 5h
    assert snap["adapterId"] == "local-rolling-5h"


def test_rolling_5h_empty_window(tmp_path):
    ctx = QuotaContext(stats_dir=str(tmp_path / "none"), account=make_account(),
                       now_fn=lambda: datetime(2026, 1, 10, 12, 0, 0))
    snap = LocalRolling5hAdapter().fetch(ctx)
    assert snap["windows"][0]["used"] == 0
    assert snap["windows"][0]["reset_at"] is None


# ---------- local 日/周/月 ----------

def test_local_month_window(tmp_path):
    now = datetime(2026, 1, 10, 12, 0, 0)
    stats_dir = str(tmp_path / "stats")
    write_jsonl(stats_dir, [
        rec("2026-01-02T10:00:00"),  # 本月
        rec("2025-12-31T23:59:00"),  # 上月
    ])
    ctx = QuotaContext(stats_dir=stats_dir, account=make_account(), now_fn=lambda: now)
    snap = LocalQuotaAdapter().fetch(ctx)
    assert snap["windows"][0]["kind"] == "month"
    assert snap["windows"][0]["used"] == 1


def test_week_window_start(tmp_path):
    now = datetime(2026, 1, 7, 9, 0, 0)  # 周三
    start = window_start(now, "week")
    assert start.weekday() == 0  # 周一
    assert start.strftime("%Y-%m-%d") == "2026-01-05"


# ---------- manual ----------

def test_manual_plan_config(tmp_path):
    ctx = QuotaContext(stats_dir=str(tmp_path / "none"),
                       account=make_account(quota={"limit": 200, "unit": "requests",
                                                   "period": "month"}),
                       now_fn=lambda: datetime(2026, 1, 10, 12, 0, 0))
    snap = get_snapshot(ctx.account, ctx)
    assert snap["adapterId"] == "manual"
    assert snap["source"] == "plan_config"
    assert snap["plan"]["included"]["amount"] == 200


# ---------- 注册表与嗅探 ----------

def test_registry_auto_sniff():
    or_acc = make_account(openai_url="https://openrouter.ai/api/v1")
    assert resolve_adapter_name(or_acc) == "openrouter"
    plain = make_account()
    assert resolve_adapter_name(plain) == "local"


def test_registry_reserved_names_fallback():
    for src in ("anthropic", "codex", "zen", "unknown-thing"):
        assert resolve_adapter_name(make_account(quota_source=src)) == "local"
    assert "manual" in registered_adapters()
    assert "openrouter" in registered_adapters()


def test_no_source_means_no_quota():
    acc = make_account(quota_source="none")
    # none 不在嗅探/注册表内 → 回退 local（含标记 none 的处理由调用方决定）
    assert resolve_adapter_name(acc) == "local"


# ---------- 验证性适配器（declarative / opencode-go / zai / credits） ----------

def test_declarative_adapter(tmp_path):
    ctx = QuotaContext(
        stats_dir=str(tmp_path / "stats"),
        account=make_account(quota_source="declarative", quota={
            "plan": "glm-coding-plan",
            "windows": [{"kind": "month", "period": "month", "unit": "requests", "limit": 200}],
        }),
        now_fn=lambda: datetime(2026, 1, 10, 12, 0, 0),
    )
    snap = DeclarativeQuotaAdapter().fetch(ctx)
    assert snap["adapterId"] == "declarative"
    assert snap["source"] == "plan_config"
    assert snap["plan"]["name"] == "glm-coding-plan"
    assert snap["windows"][0]["limit"] == 200


class _FakeResp:
    def __init__(self, data, status=200):
        self.data = data
        self.status = status

    async def __aenter__(self):
        return self

    async def __aexit__(self, *exc):
        return False

    async def json(self, **kw):
        return self.data


class _FakeSession:
    def __init__(self, data):
        self.data = data

    def get(self, url, **kw):
        return _FakeResp(self.data)


def test_opencode_go_adapter_mock():
    import asyncio
    ctx = QuotaContext(
        stats_dir="/tmp/none",
        account=make_account(quota_source="opencode-go", quota={"url": "https://api.opencode.ai"}),
        session=_FakeSession({"data": {"usage": 1.25, "limit": 100}}),
        now_fn=lambda: datetime(2026, 1, 10, 12, 0, 0),
    )
    snap = asyncio.run(OpenCodeGoAdapter().fetch(ctx))
    assert snap["adapterId"] == "opencode-go"
    assert snap["windows"][0]["used"] == 1.25
    assert snap["windows"][0]["limit"] == 100


def test_zai_adapter_mock():
    import asyncio
    ctx = QuotaContext(
        stats_dir="/tmp/none",
        account=make_account(quota_source="zai", quota={"url": "https://open.bigmodel.cn/api/paas/v4"}),
        session=_FakeSession({"data": {"used_quota": 5, "total_quota": 20}}),
        now_fn=lambda: datetime(2026, 1, 10, 12, 0, 0),
    )
    snap = asyncio.run(ZaiAdapter().fetch(ctx))
    assert snap["adapterId"] == "zai"
    assert snap["windows"][0]["limit"] == 20


def test_openrouter_credits_adapter_mock():
    import asyncio
    ctx = QuotaContext(
        stats_dir="/tmp/none",
        account=make_account(quota_source="openrouter",
                             quota={"mode": "credits", "url": "https://openrouter.ai"}),
        session=_FakeSession({"data": {"used_credits": 7, "total_credits": 50}}),
        now_fn=lambda: datetime(2026, 1, 10, 12, 0, 0),
    )
    snap = asyncio.run(OpenRouterAdapter().fetch(ctx))
    assert snap["adapterId"] == "openrouter"
    assert snap["windows"][0]["used"] == 7
    assert snap["windows"][0]["limit"] == 50


def test_registry_new_adapter_names():
    assert "declarative" in registered_adapters()
    assert "opencode-go" in registered_adapters()
    assert "zai" in registered_adapters()
    assert resolve_adapter_name(make_account(quota_source="glm-coding-plan")) == "zai"


# ---------- 失败降级 ----------

def test_upstream_failure_degrades_to_local(tmp_path, monkeypatch):
    ctx = QuotaContext(stats_dir=str(tmp_path / "stats"), account=make_account())
    # 注册一个总是失败的假适配器（隔离性验收：新增适配器只加文件/注册）
    class Boom:
        name = "boom"
        source = "provider_api"
        def fetch(self, c):
            raise QuotaError("boom")

    from o2a.quota import registry
    registry.register(Boom())
    monkeypatch.setattr(registry, "resolve_adapter_name", lambda acc: "boom")
    snap = get_snapshot(ctx.account, ctx)
    assert snap is not None
    assert snap["adapterId"] == "local"   # 降级 local
    assert snap["stale"] is True          # 标记滞后


def test_ttl_cache_hit_and_stale(tmp_path):
    cache = TTLCache(ttl_s=60)
    cache.set("acc-1", {"adapterId": "x", "stale": False})
    assert cache.get("acc-1")["adapterId"] == "x"
    stale = cache.stale("acc-1")
    assert stale["stale"] is True


def test_iter_records_filters_account(tmp_path):
    stats_dir = str(tmp_path / "stats")
    write_jsonl(stats_dir, [rec("2026-01-10T10:00:00")])
    # 别的账号不计
    assert list(iter_records(stats_dir, "acc-other",
                             datetime(2026, 1, 1))) == []


# ---------- /quota 端点 ----------

def test_quota_endpoint(tmp_path, monkeypatch):
    import asyncio
    from aiohttp import web
    from aiohttp.test_utils import TestClient, TestServer
    from o2a import engine

    stats_dir = str(tmp_path / "stats")
    monkeypatch.setenv("CACHE_STATS_DIR", stats_dir)
    write_jsonl(stats_dir, [rec("2026-01-10T10:00:00")])

    cfg = {"services": [{
        "id": "svc-abcdef01", "comment": "t1", "account": "acc-1", "client": "openai",
        "listen_address": 18001, "model": "m1",
        "openai_base_url": "https://api.example.com/v1", "openai_api_key": "sk-x",
    }]}
    p = tmp_path / "config.json"
    p.write_text(json.dumps(cfg), encoding="utf-8")
    monkeypatch.setenv("O2A_CONFIG", str(p))
    monkeypatch.delenv("O2A_AUTH", raising=False)

    async def main():
        services = engine.load_config()
        svc = services[0]
        app = web.Application()
        app["service"] = svc
        app["session"] = None
        app.router.add_route("*", "/{tail:.*}", engine.handle_request)
        client = TestClient(TestServer(app))
        await client.start_server()
        try:
            resp = await client.get("/quota")
            assert resp.status == 200
            data = await resp.json()
            assert data["adapterId"] in ("local", "manual")
            assert "windows" in data
            # 指定其他账号 → 404 风格错误体
            resp2 = await client.get("/quota?account=acc-nope")
            data2 = await resp2.json()
            assert "error" in data2
        finally:
            await client.close()

    asyncio.run(main())
