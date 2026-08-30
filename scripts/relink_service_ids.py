"""一次性修复脚本：把"失联"的 service_id 重新挂回当前配置对应的服务 id。

背景：config.json 的服务 id 被重新生成（例如旧面板会话用内存快照覆盖保存配置）
后，历史 JSONL 记录的 service_id 与 summary/ 目录名指向的旧 id 已不在当前配置
里，导致统计按 id 匹配时丢历史数据（桌面端 stats.rs 对 id 失联的记录虽有显示名
兜底，但 id 原样透传查询时仍会丢；Python 侧账号汇总读 summary 目录同样失联）。

用法（默认 dry-run 只预览，--apply 才写盘）：
    python scripts/relink_service_ids.py            # 预览将要做的变更
    python scripts/relink_service_ids.py --apply    # 执行（自动备份原文件）

动作：
1. 扫描 data/cache_stats/*.jsonl，统计每个"孤儿 service_id"（不在当前配置）的
   记录按 service 显示名的分布；当孤儿 id 的记录名占优（>=98%）且该名在当前
   配置中有 id 时，建立 孤儿id → 当前id 的重写映射。
2. --apply：改写 JSONL 中这些记录的 service_id 字段（service 显示名保持不动，
   与 id 化迁移的约定一致），原文件先复制备份到 _relink_backup-<时间戳>/。
3. summary/<孤儿id>/ 下的按日文件移动到 summary/<当前id>/；同名文件按小时合并
   进当前目录版本（避免同一天新旧 id 的聚合互相覆盖/丢失），原文件备份到
   _relink_backup/summary/<孤儿id>/，孤儿目录清空后删除。
"""
import argparse
import json
import os
import shutil
import sys
from collections import Counter, defaultdict

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DOMINANT_RATIO = 0.98


def load_config(cfg_path):
    with open(cfg_path, encoding="utf-8") as f:
        cfg = json.load(f)
    services = cfg.get("services") or []
    current_ids = {str(s.get("id") or "") for s in services if s.get("id")}
    name_to_id = {str(s.get("comment") or ""): str(s.get("id") or "")
                  for s in services if s.get("id") and s.get("comment")}
    return current_ids, name_to_id


def jsonl_files(stats_dir):
    files = [f for f in os.listdir(stats_dir) if f.endswith(".jsonl") and f[0].isdigit()]
    return sorted(files)


def build_orphan_stats(stats_dir, files, current_ids):
    """孤儿 id → 其记录的 service 显示名分布。"""
    hist = defaultdict(Counter)
    for fn in files:
        with open(os.path.join(stats_dir, fn), encoding="utf-8") as f:
            for line in f:
                line = line.strip()
                if not line:
                    continue
                try:
                    rec = json.loads(line)
                except ValueError:
                    continue
                sid = str(rec.get("service_id") or "")
                if not sid or sid in current_ids:
                    continue
                hist[sid][str(rec.get("service") or "")] += 1
    return hist


def build_mapping(hist, name_to_id):
    mapping = {}
    for orphan, names in sorted(hist.items()):
        total = sum(names.values())
        name, cnt = names.most_common(1)[0]
        if total > 1 and cnt / total < DOMINANT_RATIO:
            print(f"  ! {orphan}: 记录名分布不唯一 {dict(names)}，跳过（需人工确认）")
            continue
        target = name_to_id.get(name)
        if not target:
            print(f"  ! {orphan}: 显示名「{name}」不在当前配置，跳过（服务可能已删除）")
            continue
        if target == orphan:
            continue
        mapping[orphan] = target
        print(f"  + {orphan} → {target}（显示名「{name}」，{total} 条）")
    return mapping


def rewrite_jsonl(stats_dir, files, mapping, backup_dir):
    total = patched = 0
    os.makedirs(backup_dir, exist_ok=True)
    for fn in files:
        path = os.path.join(stats_dir, fn)
        out_lines = []
        n = 0
        with open(path, encoding="utf-8") as f:
            for line in f:
                line = line.strip()
                if not line:
                    continue
                total += 1
                try:
                    rec = json.loads(line)
                except ValueError:
                    out_lines.append(line)
                    continue
                sid = str(rec.get("service_id") or "")
                if sid in mapping:
                    rec["service_id"] = mapping[sid]
                    n += 1
                out_lines.append(json.dumps(rec, ensure_ascii=False))
        if n:
            shutil.copy2(path, os.path.join(backup_dir, fn))
            with open(path, "w", encoding="utf-8") as f:
                f.write("\n".join(out_lines) + "\n")
            print(f"  + {fn}: 重写 {n} 条 service_id")
        patched += n
    print(f"JSONL 重写: {patched}/{total} 条")


# summary 聚合文件里按小时求和/累加的字段
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
INT_KEYS = {
    "requests",
    "total_input_tokens",
    "total_cache_read_tokens",
    "total_cache_write_tokens",
    "total_output_tokens",
}


def merge_summary_file(src_path, dst_path):
    """同一日期的两代服务 id 聚合文件，按小时合并（不再丢冲突文件）。"""
    try:
        with open(src_path, encoding="utf-8") as f:
            src = json.load(f)
        with open(dst_path, encoding="utf-8") as f:
            dst = json.load(f)
    except (OSError, ValueError):
        return False
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
                pass
    dst["date"] = src.get("date") or dst.get("date") or ""
    with open(dst_path, "w", encoding="utf-8") as f:
        json.dump(dst, f, ensure_ascii=False)
    return True


def merge_summary_dirs(stats_dir, mapping, backup_dir):
    summary = os.path.join(stats_dir, "summary")
    if not os.path.isdir(summary):
        return
    for orphan, target in sorted(mapping.items()):
        src = os.path.join(summary, orphan)
        if not os.path.isdir(src):
            continue
        dst = os.path.join(summary, target)
        os.makedirs(dst, exist_ok=True)
        moved = merged = kept = 0
        for fn in sorted(os.listdir(src)):
            s, d = os.path.join(src, fn), os.path.join(dst, fn)
            if not os.path.isfile(s):
                continue
            if os.path.exists(d):
                if merge_summary_file(s, d):
                    bdir = os.path.join(backup_dir, "summary", orphan)
                    os.makedirs(bdir, exist_ok=True)
                    shutil.copy2(s, os.path.join(bdir, fn))
                    os.remove(s)
                    merged += 1
                else:
                    kept += 1  # 无法合并则保留孤儿文件，待人工处理
                continue
            shutil.move(s, d)
            moved += 1
        leftover = len(os.listdir(src))
        if leftover == 0:
            os.rmdir(src)
        print(f"  + summary/{orphan}/ → summary/{target}/：移动 {moved} 个，合并 {merged} 个，保留 {kept} 个"
              + (f"（孤儿目录残留 {leftover} 个文件未删）" if leftover else ""))


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--config", default=os.path.join(ROOT, "config.json"))
    ap.add_argument("--stats-dir", default=None,
                    help="统计目录（默认读 config.json 的 cache_stats_dir）")
    ap.add_argument("--apply", action="store_true", help="执行写盘（默认 dry-run 预览）")
    args = ap.parse_args()

    if not os.path.exists(args.config):
        print(f"config.json 不存在: {args.config}")
        return 1
    with open(args.config, encoding="utf-8") as f:
        cfg = json.load(f)
    stats_dir = args.stats_dir or cfg.get("cache_stats_dir") or os.path.join(ROOT, "data", "cache_stats")
    if not os.path.isabs(stats_dir):
        stats_dir = os.path.join(ROOT, stats_dir)

    current_ids, name_to_id = load_config(args.config)
    files = jsonl_files(stats_dir)
    print(f"== 失联 service_id 重挂{'（dry-run）' if not args.apply else ''} ==")
    print(f"统计目录: {stats_dir}；JSONL 文件 {len(files)} 个；当前配置服务 {len(current_ids)} 个")
    hist = build_orphan_stats(stats_dir, files, current_ids)
    if not hist:
        print("未发现失联的 service_id，无需修复")
        return 0
    print(f"发现 {len(hist)} 个失联 id，建立重写映射：")
    mapping = build_mapping(hist, name_to_id)
    if not mapping:
        print("无可自动重挂的 id")
        return 0
    if not args.apply:
        print("（dry-run，未写盘；加 --apply 执行）")
        return 0
    backup_dir = os.path.join(stats_dir, "_relink_backup")
    rewrite_jsonl(stats_dir, files, mapping, backup_dir)
    merge_summary_dirs(stats_dir, mapping, backup_dir)
    print(f"完成。原文件备份于: {backup_dir}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
