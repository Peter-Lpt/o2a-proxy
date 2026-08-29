"""一次性迁移脚本：服务身份 id 化（优化方案 §2.2）。

用法（手动执行，禁止启动时自动迁移）：
    python scripts/migrate_service_ids.py --dry-run   # 只预览将要做的变更
    python scripts/migrate_service_ids.py             # 执行（自动备份原文件）

动作：
1. config.json：为缺失 id 的服务生成 svc-<8hex> 稳定 id（备份 config.json.bak）。
2. summary 目录：summary/<旧名>/ → summary/<id>/（仅当目录名能按当前 comment
   唯一映射时改名；改名后 Python 端 summary 双查无需回退）。
3. --backfill-jsonl：把历史 JSONL 记录按 comment→id 映射补写 service_id 字段
   （record 的 service 显示名字段保持原样不回改）。可选，记录量大时耗时较长。

注意：统计读取端对无 service_id 的记录按当前 comment→id 映射兜底，改名前的
历史记录即使不跑第 3 步也不会丢（前提是服务未改名）。
"""
import argparse
import json
import os
import secrets
import shutil
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def new_service_id():
    return "svc-" + secrets.token_hex(4)


def migrate_config(cfg_path, dry_run):
    with open(cfg_path, encoding="utf-8") as f:
        cfg = json.load(f)
    services = cfg.get("services") or []
    assigned = set()
    added = 0
    for s in services:
        sid = str(s.get("id") or "").strip()
        if not sid or sid in assigned:
            sid = new_service_id()
            while sid in assigned:
                sid = new_service_id()
            s["id"] = sid
            added += 1
            print(f"  + 服务「{s.get('comment')}」→ id={sid}")
        assigned.add(sid)
    if added and not dry_run:
        bak = cfg_path + ".bak"
        if os.path.exists(cfg_path):
            with open(cfg_path, encoding="utf-8") as f:
                raw = f.read()
            with open(bak, "w", encoding="utf-8") as f:
                f.write(raw)
            print(f"  已备份原文件 → {bak}")
        with open(cfg_path, "w", encoding="utf-8") as f:
            json.dump(cfg, f, ensure_ascii=False, indent=2)
    print(f"config.json: 新增 {added} 个 id" + ("（dry-run，未写入）" if dry_run else ""))
    return {s.get("comment"): s["id"] for s in services if s.get("id") and s.get("comment")}


def migrate_summary_dirs(stats_dir, name_to_id, dry_run):
    summary = os.path.join(stats_dir, "summary")
    if not os.path.isdir(summary):
        print("summary 目录不存在，跳过")
        return
    moved = 0
    for name in sorted(os.listdir(summary)):
        src = os.path.join(summary, name)
        sid = name_to_id.get(name)
        if not sid or not os.path.isdir(src):
            continue
        dst = os.path.join(summary, sid)
        if os.path.exists(dst):
            print(f"  ! {name} → {sid} 已存在同名/同 id 目录，跳过")
            continue
        print(f"  + summary/{name}/ → summary/{sid}/")
        if not dry_run:
            shutil.move(src, dst)
        moved += 1
    print(f"summary 目录改名: {moved} 个" + ("（dry-run，未执行）" if dry_run else ""))


def backfill_jsonl(stats_dir, name_to_id, dry_run):
    if not os.path.isdir(stats_dir):
        print("统计目录不存在，跳过 JSONL 回填")
        return
    files = [
        f for f in os.listdir(stats_dir)
        if f.endswith(".jsonl") and f[0].isdigit()
    ]
    total = patched = 0
    for fn in sorted(files):
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
                if not rec.get("service_id"):
                    sid = name_to_id.get(rec.get("service") or "")
                    if sid:
                        rec["service_id"] = sid
                        n += 1
                out_lines.append(json.dumps(rec, ensure_ascii=False))
        if n and not dry_run:
            with open(path, "w", encoding="utf-8") as f:
                f.write("\n".join(out_lines) + "\n")
        patched += n
        if n:
            print(f"  + {fn}: 补写 {n} 条 service_id")
    print(f"JSONL 回填: {patched}/{total} 条" + ("（dry-run，未写入）" if dry_run else ""))


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--config", default=os.path.join(ROOT, "config.json"))
    ap.add_argument("--stats-dir", default=None,
                    help="统计目录（默认读 config.json 的 cache_stats_dir）")
    ap.add_argument("--dry-run", action="store_true", help="只预览，不写任何文件")
    ap.add_argument("--backfill-jsonl", action="store_true",
                    help="把历史 JSONL 记录按 comment→id 映射补写 service_id")
    args = ap.parse_args()

    if not os.path.exists(args.config):
        print(f"config.json 不存在: {args.config}")
        return 1
    with open(args.config, encoding="utf-8") as f:
        cfg = json.load(f)
    stats_dir = args.stats_dir or cfg.get("cache_stats_dir") or os.path.join(ROOT, "data", "cache_stats")
    if not os.path.isabs(stats_dir):
        stats_dir = os.path.join(ROOT, stats_dir)

    print(f"== 服务 id 迁移{'（dry-run）' if args.dry_run else ''} ==")
    name_to_id = migrate_config(args.config, args.dry_run)
    print("== summary 目录改名 ==")
    migrate_summary_dirs(stats_dir, name_to_id, args.dry_run)
    if args.backfill_jsonl:
        print("== JSONL service_id 回填 ==")
        backfill_jsonl(stats_dir, name_to_id, args.dry_run)
    return 0


if __name__ == "__main__":
    sys.exit(main())
