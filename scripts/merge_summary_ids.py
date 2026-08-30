#!/usr/bin/env python3
"""一次性/按需修复：把旧服务 id 的 summary 聚合目录合并到当前服务 id。

背景：`scripts/relink_service_ids.py` 会把 JSONL 里的孤儿 service_id 重写为当前 id，
并移动 summary/<孤儿id>/ 下的文件到 summary/<当前id>/。但如果同一天的目标 summary
文件已存在（当前 id 的引擎已经写过当天聚合），旧实现会跳过冲突文件，导致旧 id 当天
的 summary 数据残留在孤儿目录，/stats 等按 summary 读取的统计就会少算。

本脚本接受显式映射（旧id=新id），把 lingering summary 按小时聚合合并进目标文件，
并自动备份原文件到 `_relink_backup/summary/<孤儿id>/`。

用法：
    python scripts/merge_summary_ids.py svc-d33c385e=svc-3b01a673 svc-7d5de886=svc-3b01a673
    python scripts/merge_summary_ids.py --stats-dir /path/to/cache_stats old=new ...
"""
import argparse
import json
import os
import shutil
import sys

# 聚合文件里需要按小时求和/累加的字段
SUM_KEYS = (
    "requests",
    "total_input_tokens",
    "total_cache_read_tokens",
    "total_cache_write_tokens",
    "total_output_tokens",
    "total_cost",
    "_hit_rate_sum",
    "_coverage_sum",
)
# 必须保持整数的字段（写入值与引擎侧一致：JSON 中不要出现 87.0）
INT_KEYS = {
    "requests",
    "total_input_tokens",
    "total_cache_read_tokens",
    "total_cache_write_tokens",
    "total_output_tokens",
}


def load_json(path):
    try:
        with open(path, encoding="utf-8") as f:
            return json.load(f)
    except (OSError, ValueError):
        return None


def save_json(path, data):
    with open(path, "w", encoding="utf-8") as f:
        json.dump(data, f, ensure_ascii=False)


def merge_summary_file(src_path, dst_path):
    """把 src summary 的小时聚合并入 dst summary；返回 True 表示合并成功。"""
    src, dst = load_json(src_path), load_json(dst_path)
    if not isinstance(src, dict) or not isinstance(dst, dict):
        return False
    src_hours = src.get("hours")
    dst_hours = dst.get("hours")
    if not isinstance(src_hours, dict) or not isinstance(dst_hours, dict):
        return False

    for hour, src_h in src_hours.items():
        if not isinstance(src_h, dict):
            continue
        if hour not in dst_hours or not isinstance(dst_hours[hour], dict):
            dst_hours[hour] = dict(src_h)
            continue
        dst_h = dst_hours[hour]
        for key in SUM_KEYS:
            a = src_h.get(key, 0)
            b = dst_h.get(key, 0)
            try:
                if key in INT_KEYS:
                    dst_h[key] = int(a) + int(b)
                else:
                    dst_h[key] = float(a) + float(b)
            except (TypeError, ValueError):
                # 非数值字段不要破坏原文件，保留目标值
                pass
    dst["date"] = src.get("date") or dst.get("date") or ""
    save_json(dst_path, dst)
    return True


def backup_file(path, backup_root):
    if not os.path.exists(path):
        return
    os.makedirs(os.path.dirname(backup_root), exist_ok=True)
    shutil.copy2(path, backup_root)


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("mappings", nargs="+", metavar="OLD_ID=NEW_ID",
                    help="旧 summary 目录名=当前服务 id，可传多个")
    ap.add_argument("--stats-dir", default=None,
                    help="统计目录（默认 data/cache_stats）")
    args = ap.parse_args()

    stats_dir = args.stats_dir or os.path.join(
        os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
        "data", "cache_stats",
    )
    summary = os.path.join(stats_dir, "summary")
    backup_root = os.path.join(stats_dir, "_relink_backup", "summary")

    mappings = []
    for item in args.mappings:
        if "=" not in item:
            print(f"映射格式应为 OLD_ID=NEW_ID: {item}")
            return 1
        old, new = item.split("=", 1)
        old, new = old.strip(), new.strip()
        if not old or not new:
            print(f"映射不能为空: {item}")
            return 1
        mappings.append((old, new))

    for old, new in mappings:
        src_dir = os.path.join(summary, old)
        dst_dir = os.path.join(summary, new)
        if not os.path.isdir(src_dir):
            print(f"跳过（不存在）: {src_dir}")
            continue
        if old == new:
            print(f"跳过（旧=新）: {old}")
            continue
        os.makedirs(dst_dir, exist_ok=True)
        merged = moved = kept = 0
        for fn in sorted(os.listdir(src_dir)):
            src_path = os.path.join(src_dir, fn)
            dst_path = os.path.join(dst_dir, fn)
            if not os.path.isfile(src_path) or not fn.endswith(".json"):
                continue
            if not os.path.exists(dst_path):
                backup_file(src_path, os.path.join(backup_root, old, fn))
                shutil.move(src_path, dst_path)
                moved += 1
                continue
            if merge_summary_file(src_path, dst_path):
                backup_file(src_path, os.path.join(backup_root, old, fn))
                os.remove(src_path)
                merged += 1
            else:
                kept += 1
                print(f"  ! 合并失败，保留: {src_path}")
        leftover = [f for f in os.listdir(src_dir) if os.path.isfile(os.path.join(src_dir, f))]
        if not leftover:
            os.rmdir(src_dir)
        print(f"{old} -> {new}: 移动 {moved} 个，合并 {merged} 个，保留/失败 {kept} 个"
              + (f"（孤儿目录残留 {leftover} 个文件未删）" if leftover else ""))

    return 0


if __name__ == "__main__":
    sys.exit(main())