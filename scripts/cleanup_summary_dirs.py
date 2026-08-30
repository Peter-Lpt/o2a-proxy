#!/usr/bin/env python3
"""清理 data/cache_stats/summary 下已不属于当前配置的目录。

保留：
- 当前 config.json 服务的 id 目录（svc-xxx）
- 当前 config.json 服务的 comment 目录（旧格式，仍可能被读取端兜底）

处理：
- 判断为旧服务/测试残留/死目录的，整体移动到 `_cleanup_backup/`（默认目录），
  不直接物理删除，便于误判后找回。
- 对需要迁移到当前 id 的历史数据，先用 scripts/merge_summary_ids.py 合并，
  本脚本只负责把确认无用的目录清出 summary。

用法：
    python scripts/cleanup_summary_dirs.py --dry-run          # 预览
    python scripts/cleanup_summary_dirs.py                    # 执行（移到备份）
"""
import argparse
import json
import os
import shutil
import sys
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def load_current_ids(cfg_path):
    with open(cfg_path, encoding="utf-8") as f:
        cfg = json.load(f)
    ids = set()
    comments = set()
    for s in cfg.get("services") or []:
        if s.get("id"):
            ids.add(str(s["id"]))
        if s.get("comment"):
            comments.add(str(s["comment"]))
    return ids, comments


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--config", default=os.path.join(ROOT, "config.json"))
    ap.add_argument("--stats-dir", default=None,
                    help="统计目录（默认 data/cache_stats）")
    ap.add_argument("--backup-dir", default=None,
                    help="备份目录（默认 <stats-dir>/_cleanup_backup）")
    ap.add_argument("--dry-run", action="store_true", help="只预览要移走的目录")
    args = ap.parse_args()

    stats_dir = args.stats_dir or os.path.join(ROOT, "data", "cache_stats")
    summary = os.path.join(stats_dir, "summary")
    if not os.path.isdir(summary):
        print(f"summary 目录不存在: {summary}")
        return 0
    if not os.path.exists(args.config):
        print(f"config.json 不存在: {args.config}")
        return 1

    current_ids, current_comments = load_current_ids(args.config)
    backup = args.backup_dir or os.path.join(stats_dir, "_cleanup_backup")
    os.makedirs(backup, exist_ok=True)

    moved = skipped = 0
    for name in sorted(os.listdir(summary)):
        src = os.path.join(summary, name)
        if not os.path.isdir(src):
            continue
        if name in current_ids or name in current_comments:
            skipped += 1
            continue
        dst = os.path.join(backup, name)
        if os.path.exists(dst):
            dst = os.path.join(backup, f"{name}.{int(time.time())}")
        print(f"  {name} -> {dst}")
        if not args.dry_run:
            shutil.move(src, dst)
        moved += 1

    print(f"summary 清理: 移走 {moved} 个，保留 {skipped} 个"
          + ("（dry-run）" if args.dry_run else f" 备份于 {backup}"))
    return 0


if __name__ == "__main__":
    sys.exit(main())