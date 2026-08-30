"""配置迁移与 id 化回归测试（优化方案  /  test_config_migration）。

覆盖：
- load_config 缺失 id 惰性生成并写回（含 .bak 备份与去重）
- v0/v1 样本配置（旧结构 / 无 id / 无新字段）加载兼容
- summary 目录 id 优先 + 旧名双查兜底
- JSONL 记录新增 service_id 字段且 service 显示名保持原样
- 迁移脚本 --dry-run 不写任何文件

运行方式：
    python -m pytest test_config_migration.py -v
"""
import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import pytest

from o2a.config import load_config
from o2a.stats import CacheStats

BASE_CFG = {
    "cache_stats_enabled": True,
    "accounts": [{"id": "acc-1", "name": "a", "api_key": "k",
                  "openai_url": "https://api.example.com/v1"}],
}


def write_cfg(tmp_path, cfg):
    p = tmp_path / "config.json"
    p.write_text(json.dumps(cfg, ensure_ascii=False), encoding="utf-8")
    return str(p)


def svc_min(i=1, **kw):
    d = {"comment": f"t{i}", "account": "acc-1", "client": "openai",
         "listen_address": 18000 + i, "model": "m"}
    d.update(kw)
    return d


@pytest.fixture(autouse=True)
def _env(tmp_path, monkeypatch):
    monkeypatch.setenv("CACHE_STATS_DIR", str(tmp_path / "stats"))
    monkeypatch.setenv("CACHE_STATS_ENABLED", "true")
    monkeypatch.setenv("O2A_CONFIG", str(tmp_path / "config.json"))
    monkeypatch.delenv("O2A_AUTH", raising=False)
    yield


# ---------- load_config：id 惰性生成 + 写回 ----------

def test_missing_ids_generated_and_written_back(tmp_path):
    p = write_cfg(tmp_path, {**BASE_CFG, "services": [svc_min(1), svc_min(2)]})
    services = load_config()
    assert all(s.id.startswith("svc-") and len(s.id) == 12 for s in services)
    assert len({s.id for s in services}) == 2  # 去重
    cfg = json.loads(open(p, encoding="utf-8").read())
    assert all(str(s.get("id", "")).startswith("svc-") for s in cfg["services"])
    assert os.path.exists(p + ".bak")  # 写回前备份


def test_existing_ids_preserved(tmp_path):
    p = write_cfg(tmp_path, {**BASE_CFG, "services": [svc_min(1, id="svc-abcdef01")]})
    services = load_config()
    assert services[0].id == "svc-abcdef01"
    cfg = json.loads(open(p, encoding="utf-8").read())
    assert cfg["services"][0]["id"] == "svc-abcdef01"


def test_duplicate_ids_regenerated(tmp_path):
    p = write_cfg(tmp_path, {**BASE_CFG, "services": [
        svc_min(1, id="svc-dup11111"), svc_min(2, id="svc-dup11111")]})
    services = load_config()
    assert services[0].id == "svc-dup11111"
    assert services[1].id != "svc-dup11111"


# ---------- v0/v1 样本兼容 + 新字段 ----------

def test_v0_legacy_config_loads(tmp_path):
    """v0：services 内嵌 url/key 的最老结构 + mode 字段。"""
    p = write_cfg(tmp_path, {"services": [{
        "comment": "legacy", "mode": "claude", "model": "m",
        "listen_address": 18001, "openai_base_url": "https://x.example.com",
        "openai_api_key": "sk-x"}]})
    services = load_config()
    assert len(services) == 1
    assert services[0].name == "legacy"
    assert services[0].enabled is True      # 缺省启用
    assert services[0].autostart is False   # 缺省不自启
    assert services[0].order == 0


def test_v1_config_new_fields(tmp_path):
    p = write_cfg(tmp_path, {**BASE_CFG, "services": [
        svc_min(1, order=5, enabled=False, autostart=True)]})
    services = load_config()
    assert services[0].order == 5
    assert services[0].enabled is False
    assert services[0].autostart is True


def test_enabled_string_false_rejected(tmp_path):
    p = write_cfg(tmp_path, {**BASE_CFG, "services": [svc_min(1, enabled="false")]})
    assert load_config()[0].enabled is False


# ----------  pricing 字段升级 ----------

def test_pricing_string_none_maps_to_subscription(tmp_path):
    p = write_cfg(tmp_path, {**BASE_CFG, "services": [svc_min(1, pricing="none")]})
    s = load_config()[0]
    assert s.pricing_mode == "subscription"
    assert s.pricing == "none"  # 兼容别名原样保留


def test_pricing_object_subscription(tmp_path):
    obj = {"mode": "subscription", "plan": "max-5h"}
    p = write_cfg(tmp_path, {**BASE_CFG, "services": [svc_min(1, pricing=obj)]})
    s = load_config()[0]
    assert s.pricing_mode == "subscription"
    assert s.pricing_extra == {"plan": "max-5h"}
    # 对象形式原样保留写回（保存不丢字段）
    cfg = json.loads(open(p, encoding="utf-8").read())
    assert cfg["services"][0]["pricing"] == obj


def test_pricing_object_free(tmp_path):
    p = write_cfg(tmp_path, {**BASE_CFG, "services": [
        svc_min(1, pricing={"mode": "free"})]})
    assert load_config()[0].pricing_mode == "free"


def test_pricing_object_invalid_mode_falls_back(tmp_path):
    p = write_cfg(tmp_path, {**BASE_CFG, "services": [
        svc_min(1, pricing={"mode": "bogus"})]})
    s = load_config()[0]
    assert s.pricing_mode == "token"


def test_pricing_default_is_token(tmp_path):
    p = write_cfg(tmp_path, {**BASE_CFG, "services": [svc_min(1)]})
    s = load_config()[0]
    assert s.pricing_mode == "token"
    assert s.pricing == ""


# ---------- summary 双查 + service_id 记录 ----------

def test_summary_read_falls_back_to_name_dir(tmp_path):
    from datetime import datetime
    stats_dir = tmp_path / "stats"
    name_dir = stats_dir / "summary" / "t1"
    name_dir.mkdir(parents=True)
    today = datetime.now().strftime("%Y-%m-%d")
    (name_dir / f"{today}.json").write_text(json.dumps({
        "date": today,
        "hours": {"10": {"requests": 3, "total_input_tokens": 100,
                          "total_cache_read_tokens": 0, "total_cache_write_tokens": 0,
                          "total_output_tokens": 50, "total_cost": 0.1,
                          "_hit_rate_sum": 0.0, "_coverage_sum": 0.0}},
    }), encoding="utf-8")
    cs = CacheStats(stats_dir=str(stats_dir), retention_days=30,
                    service="t1", service_id="svc-abcdef01")
    day = cs.get_summary("day")
    assert day["daily_total"]["requests"] == 3  # id 目录缺失 → 旧名目录兜底


def test_record_includes_service_id(tmp_path):
    stats_dir = tmp_path / "stats"
    cs = CacheStats(stats_dir=str(stats_dir), retention_days=30,
                    service="t1", service_id="svc-abcdef01")
    rec = cs._build_record("m1", {"input_tokens": 1, "output_tokens": 1})
    assert rec["service_id"] == "svc-abcdef01"
    assert rec["service"] == "t1"  # 显示名字段保持原样
    # 无 id 的实例（历史行为）不写 service_id
    cs2 = CacheStats(stats_dir=str(tmp_path / "stats2"), retention_days=30, service="t2")
    rec2 = cs2._build_record("m1", {"input_tokens": 1, "output_tokens": 1})
    assert "service_id" not in rec2


# ---------- 迁移脚本：--dry-run 不写文件 ----------

def test_migrate_script_dry_run_touches_nothing(tmp_path, monkeypatch):
    import subprocess
    cfg_path = write_cfg(tmp_path, {**BASE_CFG, "services": [svc_min(1)]})
    monkeypatch.setenv("O2A_CONFIG", cfg_path)
    script = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
                          "scripts", "migrate_service_ids.py")
    r = subprocess.run([sys.executable, script, "--config", cfg_path, "--dry-run"],
                       capture_output=True, text=True)
    assert r.returncode == 0, r.stderr
    cfg = json.loads(open(cfg_path, encoding="utf-8").read())
    assert "id" not in cfg["services"][0]          # dry-run 未写入
    assert not os.path.exists(cfg_path + ".bak")   # 未备份


def test_migrate_script_applies_ids(tmp_path):
    import subprocess
    cfg_path = write_cfg(tmp_path, {**BASE_CFG, "services": [svc_min(1)]})
    script = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
                          "scripts", "migrate_service_ids.py")
    r = subprocess.run([sys.executable, script, "--config", cfg_path],
                       capture_output=True, text=True)
    assert r.returncode == 0, r.stderr
    cfg = json.loads(open(cfg_path, encoding="utf-8").read())
    assert cfg["services"][0]["id"].startswith("svc-")
    assert os.path.exists(cfg_path + ".bak")
