"""local 系适配器共用：从 JSONL 记录聚合窗口用量（任何 provider 都能用）。"""

import os
from datetime import datetime, timedelta


def iter_records(stats_dir, account_id: str, since: datetime):
    """遍历统计目录中某账号自 since 起的记录（timestamp 升序无保证）。"""
    if not stats_dir or not account_id:
        return
    try:
        days = (datetime.now() - since).days + 1
    except Exception:
        return
    for offset in range(days + 1):
        ds = (datetime.now() - timedelta(days=offset)).strftime("%Y-%m-%d")
        p = os.path.join(str(stats_dir), f"{ds}.jsonl")
        if not os.path.isfile(p):
            continue
        try:
            with open(p, encoding="utf-8") as f:
                for line in f:
                    line = line.strip()
                    if not line:
                        continue
                    try:
                        import json
                        rec = json.loads(line)
                    except ValueError:
                        continue
                    if rec.get("account") != account_id:
                        continue
                    try:
                        ts = datetime.strptime(str(rec.get("timestamp", ""))[:19],
                                               "%Y-%m-%dT%H:%M:%S")
                    except ValueError:
                        continue
                    if ts >= since:
                        yield ts, rec
        except OSError:
            continue


def count_requests(stats_dir, account_id: str, since: datetime):
    """窗口内请求数（成功 + 失败）。"""
    n = 0
    for _ts, _rec in iter_records(stats_dir, account_id, since):
        n += 1
    return n


def count_tokens(stats_dir, account_id: str, since: datetime):
    """窗口内 token 总量（输入侧 + 输出）。"""
    total = 0
    for _ts, rec in iter_records(stats_dir, account_id, since):
        total += (rec.get("input_tokens") or 0) + (rec.get("cache_read_tokens") or 0) \
            + (rec.get("cache_write_tokens") or 0) + (rec.get("output_tokens") or 0)
    return total
