"""服务级模型白名单 / 别名映射 / model_policy 单元测试（优化方案 §6 + §13）。

覆盖：
- _model_entries：白名单空（现状不变）/ 非空（全集 + default/required 标记 + 别名列入）
- _apply_model_policy：clamp / reject / passthrough × 白名单内外 × 别名命中
- 不配置白名单的存量服务行为不变（兼容）
- 统计对外名反查（reverse_models_map）

运行方式：
    python -m pytest test_models_endpoint.py -v
"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from o2a.config import Account, Service
from o2a.engine import _apply_model_policy, _model_entries


def make_service(**kw):
    acc = Account(id="a1", name="a1", api_key="k", openai_url="https://api.example.com/v1")
    return Service(name="s1", account=acc, client="openai", host="127.0.0.1",
                   port=18000, model="deepseek-v4-flash", **kw)


# ---------- /v1/models 输出矩阵 ----------

def test_models_empty_whitelist_keeps_legacy():
    """白名单空：现状不变，单条 required 随 override_model。"""
    for override in (True, False):
        svc = make_service(override_model=override)
        entries = _model_entries(svc)
        assert len(entries) == 1
        assert entries[0]["id"] == "deepseek-v4-flash"
        assert entries[0]["required"] is override
        assert "default" not in entries[0]


def test_models_whitelist_full_set():
    svc = make_service(override_model=False,
                       models=["deepseek-v4-flash", "deepseek-v4-pro"])
    entries = _model_entries(svc)
    assert [e["id"] for e in entries] == ["deepseek-v4-flash", "deepseek-v4-pro"]
    assert all(e["required"] is False for e in entries)
    main = next(e for e in entries if e["id"] == "deepseek-v4-flash")
    assert main["default"] is True
    assert all(e["context"] == svc.max_tokens for e in entries)


def test_models_whitelist_override_marks_main_required():
    svc = make_service(override_model=True,
                       models=["deepseek-v4-flash", "deepseek-v4-pro"])
    entries = _model_entries(svc)
    main = next(e for e in entries if e["id"] == "deepseek-v4-flash")
    other = next(e for e in entries if e["id"] == "deepseek-v4-pro")
    assert main["required"] is True and main["default"] is True
    assert other["required"] is False


def test_models_alias_keys_listed_upstream_hidden():
    svc = make_service(models=["deepseek-v4-flash"],
                       models_map={"claude-sonnet-4": "deepseek-v4-flash"})
    entries = _model_entries(svc)
    ids = {e["id"] for e in entries}
    assert "claude-sonnet-4" in ids          # 别名对外名列入
    assert all(e["id"] != "hidden-x" for e in entries)
    assert not any("claude-sonnet-4" in str(e.get("x")) for e in entries)


# ---------- 请求策略矩阵 ----------

def test_no_whitelist_passthrough_unchanged():
    """不配置白名单的存量服务：行为与升级前逐字节一致。"""
    svc = make_service()
    payload = {"model": "anything-else"}
    assert _apply_model_policy(svc, payload) is None
    assert payload["model"] == "anything-else"  # 原样透传


def test_clamp_forces_main_model():
    svc = make_service(models=["deepseek-v4-flash"])
    payload = {"model": "gpt-99"}
    assert _apply_model_policy(svc, payload) is None
    assert payload["model"] == "deepseek-v4-flash"  # 客户端无感


def test_reject_returns_400_with_available_models():
    svc = make_service(models=["deepseek-v4-flash", "deepseek-v4-pro"], model_policy="reject")
    payload = {"model": "gpt-99"}
    resp = _apply_model_policy(svc, payload)
    assert resp is not None and resp.status == 400
    body = str(resp.body)
    assert "deepseek-v4-flash" in body
    assert payload["model"] == "gpt-99"  # 不改写


def test_passthrough_policy_keeps_request():
    svc = make_service(models=["deepseek-v4-flash"], model_policy="passthrough")
    payload = {"model": "gpt-99"}
    assert _apply_model_policy(svc, payload) is None
    assert payload["model"] == "gpt-99"  # 白名单仅展示，请求照旧


def test_whitelist_member_untouched():
    svc = make_service(models=["deepseek-v4-flash", "deepseek-v4-pro"])
    payload = {"model": "deepseek-v4-pro"}
    assert _apply_model_policy(svc, payload) is None
    assert payload["model"] == "deepseek-v4-pro"


def test_alias_maps_to_upstream():
    svc = make_service(models=["claude-sonnet-4"],
                       models_map={"claude-sonnet-4": "deepseek-v4-flash"})
    payload = {"model": "claude-sonnet-4"}
    assert _apply_model_policy(svc, payload) is None
    assert payload["model"] == "deepseek-v4-flash"  # 转发用上游名


def test_alias_works_without_whitelist():
    svc = make_service(models_map={"claude-sonnet-4": "deepseek-v4-flash"})
    payload = {"model": "claude-sonnet-4"}
    assert _apply_model_policy(svc, payload) is None
    assert payload["model"] == "deepseek-v4-flash"


def test_reverse_map_for_stats():
    svc = make_service(models_map={"claude-sonnet-4": "deepseek-v4-flash"})
    assert svc.reverse_models_map == {"deepseek-v4-flash": "claude-sonnet-4"}


def test_invalid_policy_falls_back_to_clamp():
    from o2a.config import load_config  # noqa: F401 (import sanity)
    svc = make_service(model_policy="bogus")
    assert svc.model_policy == "clamp"
