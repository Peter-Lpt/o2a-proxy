"""接入层鉴权单元测试（优化方案 §11.1/§11.3 + §13 test_auth）。

覆盖：
- auth_token 未配置 → 全部放行（历史行为，逐字节兼容）
- auth_token 已配置 → Authorization: Bearer / x-api-key 凭证矩阵
- /health 恒放行（供探活）
- load_config 对 services[].auth_token 的解析与裁剪

运行方式：
    python -m pytest test_auth.py -v
"""
import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import pytest
from aiohttp.test_utils import make_mocked_request

from o2a.config import Account, Service, load_config
from o2a.engine import _check_auth, auth_error_response


def make_service(auth_token=""):
    acc = Account(id="a1", name="a1", api_key="k", openai_url="https://api.example.com/v1")
    return Service(
        name="s1", account=acc, client="openai", host="127.0.0.1",
        port=18000, model="m1", auth_token=auth_token,
    )


def make_request(method="POST", path="/v1/messages", headers=None):
    return make_mocked_request(method, path, headers=headers or {})


def hdr(**kw):
    return {k.lower(): v for k, v in kw.items()}


# ---------- 未配置 auth_token：历史行为，全部放行 ----------

def test_no_token_allows_everything():
    svc = make_service("")
    assert _check_auth(make_request("POST", "/v1/messages"), svc)
    assert _check_auth(make_request("GET", "/stats"), svc)
    assert _check_auth(make_request("GET", "/status"), svc)
    assert _check_auth(make_request("GET", "/health"), svc)
    assert _check_auth(make_request("GET", "/"), svc)


# ---------- 已配置 auth_token：凭证矩阵 ----------

def test_token_bearer_ok():
    svc = make_service("sk-local-1")
    assert _check_auth(make_request(headers=hdr(Authorization="Bearer sk-local-1")), svc)


def test_token_x_api_key_ok():
    svc = make_service("sk-local-1")
    assert _check_auth(make_request(headers=hdr(**{"x-api-key": "sk-local-1"})), svc)


def test_token_missing_or_wrong_rejected():
    svc = make_service("sk-local-1")
    assert not _check_auth(make_request(), svc)  # 无凭证
    assert not _check_auth(make_request(headers=hdr(Authorization="Bearer wrong")), svc)
    assert not _check_auth(make_request(headers=hdr(**{"x-api-key": "wrong"})), svc)
    assert not _check_auth(make_request(headers=hdr(Authorization="sk-local-1")), svc)  # 缺 Bearer 前缀


def test_token_empty_header_rejected():
    svc = make_service("sk-local-1")
    assert not _check_auth(make_request(headers=hdr(Authorization="Bearer ")), svc)
    assert not _check_auth(make_request(headers=hdr(**{"x-api-key": ""})), svc)


# ---------- /health 恒放行 ----------

def test_health_always_allowed():
    svc = make_service("sk-local-1")
    assert _check_auth(make_request("GET", "/health"), svc)  # 无凭证也放行
    assert _check_auth(make_request("GET", "/health", headers=hdr(Authorization="Bearer wrong")), svc)


# ---------- 401 错误体：双协议兼容 ----------

def test_auth_error_body_shape():
    resp = auth_error_response()
    body = json.loads(resp.body)
    assert resp.status == 401
    assert body["error"]["message"]
    assert body["error"]["type"] == "authentication_error"


# ---------- load_config 解析 services[].auth_token ----------

@pytest.fixture
def config_env(tmp_path, monkeypatch):
    cfg = {
        "auth_token": "sk-global",
        "accounts": [{"id": "acc-1", "name": "a", "api_key": "k",
                      "openai_url": "https://api.example.com/v1"}],
        "services": [
            {"comment": "t1", "account": "acc-1", "client": "openai",
             "listen_address": 18001, "model": "m1", "auth_token": "  sk-t1  "},
            {"comment": "t2", "account": "acc-1", "client": "openai",
             "listen_address": 18002, "model": "m2"},
        ],
    }
    p = tmp_path / "config.json"
    p.write_text(json.dumps(cfg), encoding="utf-8")
    monkeypatch.setenv("O2A_CONFIG", str(p))
    monkeypatch.delenv("O2A_AUTH", raising=False)
    return p


def test_load_config_reads_auth_token(config_env):
    services = load_config()
    by_name = {s.name: s for s in services}
    assert by_name["t1"].auth_token == "sk-t1"      # 服务级覆盖全局 + strip
    assert by_name["t2"].auth_token == "sk-global"  # 服务级缺省 → 顶层全局兜底


def test_global_token_enforces_auth(config_env):
    services = load_config()
    svc = next(s for s in services if s.name == "t2")
    assert _check_auth(make_request(headers=hdr(Authorization="Bearer sk-global")), svc)
    assert not _check_auth(make_request(), svc)


def test_with_mode_preserves_auth_token():
    svc = make_service("sk-t")
    svc2 = svc.with_mode("codex")
    assert svc2.auth_token == "sk-t"
    assert svc2._mode_override == "codex"
