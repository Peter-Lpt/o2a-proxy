"""pricing fingerprint：价格目录/配置身份指纹。

指纹用于缓存失效：Python 统计读取以重算为主，指纹改变时丢弃聚合缓存，
避免旧缓存展示新价格。
"""

import hashlib
import json

from .schema import DEFAULT_CURRENCY, normalize_pricing


def pricing_fingerprint(raw: dict, provider_identity: str = "") -> str:
    """计算价格目录指纹。

    对归一化后的 v1/v2/v3 结构做 canonical JSON，并叠加 provider/route 身份。
    返回 64 位 hex（SHA-256）。
    """
    normalized = normalize_pricing(raw) if isinstance(raw, dict) else {}
    payload = {
        "schema": normalized.get("_meta", {}).get("schema"),
        "version": normalized.get("_meta", {}).get("version"),
        "currency": normalized.get("_meta", {}).get("currency", DEFAULT_CURRENCY),
        "models": normalized.get("models", {}),
        "accounts": normalized.get("accounts", {}),
        "services": normalized.get("services", {}),
        "rules": normalized.get("rules", []),
    }
    canonical = json.dumps(payload, ensure_ascii=False, sort_keys=True,
                           separators=(",", ":"), default=str)
    h = hashlib.sha256()
    h.update(canonical.encode("utf-8"))
    if provider_identity:
        h.update(b"|")
        h.update(provider_identity.encode("utf-8"))
    return h.hexdigest()