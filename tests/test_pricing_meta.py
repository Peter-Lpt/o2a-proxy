"""价格热加载 / 审计元数据端点测试。

覆盖：
- GET /pricing-meta：返回 fingerprint / version / rules / plans
- POST /pricing-reload：显式清缓存，返回 pricing reloaded
- /quota 在 service.pricing.plan 引用套餐时返回 plan 详情
"""
import asyncio
import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from aiohttp import web
from aiohttp.test_utils import TestClient, TestServer

from o2a import engine


def _make_config(tmp_path, pricing_plan=None):
    svc = {
        "id": "svc-pricemeta", "comment": "pm", "account": "acc-1", "client": "openai",
        "listen_address": 18901, "model": "m", "openai_base_url": "https://api.example.com/v1",
        "openai_api_key": "sk-x",
    }
    if pricing_plan:
        svc["pricing"] = {"mode": "subscription", "plan": pricing_plan}
    cfg = {
        "cache_stats_enabled": True,
        "accounts": [{"id": "acc-1", "name": "账号1", "api_key": "sk-x",
                      "openai_url": "https://api.example.com/v1"}],
        "services": [svc],
    }
    p = tmp_path / "config.json"
    p.write_text(json.dumps(cfg), encoding="utf-8")
    return p


def _app():
    services = engine.load_config()
    svc = services[0]
    app = web.Application()
    app["service"] = svc
    app["session"] = None
    app.router.add_route("*", "/{tail:.*}", engine.handle_request)
    return app


def test_pricing_meta_and_reload_endpoints(tmp_path, monkeypatch):
    monkeypatch.setenv("O2A_CONFIG", str(_make_config(tmp_path)))
    monkeypatch.delenv("O2A_AUTH", raising=False)
    monkeypatch.delenv("CACHE_STATS_ENABLED", raising=False)

    async def main():
        client = TestClient(TestServer(_app()))
        await client.start_server()
        try:
            r = await client.get("/pricing-meta")
            assert r.status == 200
            data = await r.json()
            assert data["fingerprint"]
            assert "rules" in data
            assert "plans" in data

            r2 = await client.post("/pricing-reload")
            assert r2.status == 200
            data2 = await r2.json()
            assert data2["status"] == "pricing reloaded"
        finally:
            await client.close()

    asyncio.run(main())


def test_quota_enrich_plan_from_pricing(tmp_path, monkeypatch):
    monkeypatch.setenv("O2A_CONFIG", str(_make_config(tmp_path, pricing_plan="glm-coding-plan")))
    monkeypatch.delenv("O2A_AUTH", raising=False)
    monkeypatch.delenv("CACHE_STATS_ENABLED", raising=False)
    monkeypatch.delenv("CACHE_STATS_DIR", str(tmp_path / "stats"))

    async def main():
        client = TestClient(TestServer(_app()))
        await client.start_server()
        try:
            r = await client.get("/quota?account=acc-1")
            assert r.status == 200
            data = await r.json()
            # subscription 服务 no_cost=true，但套餐目录仍应补进快照
            assert data.get("planName") == "glm-coding-plan"
            assert data.get("plan", {}).get("name") == "glm-coding-plan"
            assert "included" in data["plan"]
        finally:
            await client.close()

    asyncio.run(main())