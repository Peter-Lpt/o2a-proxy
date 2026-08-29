"""o2a-proxy 缓存统计：CacheStats 记录/聚合/查询、全局实例、账号级归并。

从原 proxy.py 拆出，逻辑逐字保留。
"""

import json
import os
import threading
from datetime import datetime, timedelta

try:
    import fcntl  # POSIX 文件锁（与 base.HAS_FCNTL 同步；Windows 无此模块）
except ImportError:
    fcntl = None

from .base import HAS_FCNTL, PROJECT_ROOT, logger
from .config import load_config
from .pricing import resolve_cost


class CacheStats:
    """缓存命中统计：记录、聚合、查询。service 非空时按服务分目录写 summary。

    服务身份 id 化（优化方案 §2.2）：service_id 非空时 summary 目录用 <id>，
    读取时若 id 目录缺失回退 <旧名> 目录（历史数据双查）；JSONL 记录新增
    service_id 字段，service（显示名）字段保持原样不回改。
    """

    def __init__(self, stats_dir="data/cache_stats", retention_days=30, service=None, account=None,
                 no_cost=False, service_id=None):
        self.stats_dir = stats_dir
        self.retention_days = retention_days
        self.service = service or ""
        self.service_id = service_id or ""
        self.account = account or ""
        self.no_cost = no_cost
        self._lock = threading.Lock()
        self._last_hour = None
        self._pricing = None
        os.makedirs(self._summary_root(), exist_ok=True)
        self._cleanup_old_files()

    def _summary_root(self):
        """summary 写入目录：优先按服务 id，无 id 时按服务名（历史行为）。"""
        root = os.path.join(self.stats_dir, "summary")
        if self.service_id:
            return os.path.join(root, self.service_id)
        if self.service:
            return os.path.join(root, self.service)
        return root

    def _summary_read_dirs(self):
        """summary 读取目录列表：id 目录优先，名字目录兜底（历史数据双查）。"""
        root = os.path.join(self.stats_dir, "summary")
        dirs = []
        if self.service_id:
            dirs.append(os.path.join(root, self.service_id))
        if self.service:
            dirs.append(os.path.join(root, self.service))
        if not dirs:
            dirs.append(root)
        # 去重保序
        seen = set()
        out = []
        for d in dirs:
            if d not in seen:
                seen.add(d)
                out.append(d)
        return out

    def _cleanup_old_files(self):
        """启动时清理超过保留天数的文件（含按服务分目录的 summary）。"""
        cutoff = datetime.now() - timedelta(days=self.retention_days)
        cutoff_ts = cutoff.timestamp()
        # jsonl 与 summary 根目录（含服务子目录）
        dirs = [self.stats_dir, os.path.join(self.stats_dir, "summary")]
        summary_children = os.path.join(self.stats_dir, "summary")
        if os.path.isdir(summary_children):
            for entry in os.listdir(summary_children):
                p = os.path.join(summary_children, entry)
                if os.path.isdir(p):
                    dirs.append(p)
        for dirpath in dirs:
            if not os.path.isdir(dirpath):
                continue
            for filename in os.listdir(dirpath):
                if not (filename.endswith(".jsonl") or filename.endswith(".json")):
                    continue
                filepath = os.path.join(dirpath, filename)
                try:
                    if os.path.getmtime(filepath) < cutoff_ts:
                        os.remove(filepath)
                        logger.info(f"[CACHE] Cleaned up old file: {filename}")
                except OSError:
                    pass

    def _load_pricing(self):
        """加载定价数据（缓存，取自项目根目录 pricing.json）。"""
        if self._pricing is not None:
            return self._pricing
        pricing_path = os.path.join(PROJECT_ROOT, "pricing.json")
        try:
            with open(pricing_path, "r", encoding="utf-8") as f:
                self._pricing = json.load(f)
        except (OSError, ValueError):
            self._pricing = {}
        return self._pricing

    def _calc_cost(self, model, input_tokens, cache_read, cache_write, output_tokens,
                   account=None, timestamp=None):
        """计算单次请求的费用（CNY）。

        account 为账号 id（也可识别 name）；有账号级定价
        （pricing.json["accounts"][账号 id/name]）时优先，否则回退全局按模型名查找。
        timestamp 为记录本地时间（schedule 判定用）；
        context_tokens（输入侧 prompt 总量）供 context_tier 阶梯判定（§7-④）。
        §7：解析与求值委托 o2a/pricing 包（v1 兼容映射在包内 loader，
        行为与旧实现逐字节一致，由 golden fixtures 固化）。
        """
        pricing = self._load_pricing()
        if not pricing:
            return 0.0
        # 账号键序：id 优先，name 兜底（v1 accounts 段语义）
        keys = [account] if account else []
        if account:
            for svc in load_config():
                if svc.account.id == account and svc.account.name:
                    keys.append(svc.account.name)
                    break
        result = resolve_cost(
            pricing, model, input_tokens, cache_read, cache_write, output_tokens,
            account_keys=keys, timestamp=timestamp,
            context_tokens=input_tokens + cache_read + cache_write,
        )
        return result["total"]

    def _get_today_file(self):
        """返回当天的 JSONL 文件路径（本地时间）。"""
        date_str = datetime.now().strftime("%Y-%m-%d")
        return os.path.join(self.stats_dir, f"{date_str}.jsonl")

    def _compute_rates(self, input_tokens, cache_read, cache_write):
        """计算缓存命中率和覆盖率。"""
        # cache_hit_rate: Anthropic 官方定义，不含 cache_write
        denom_hit = cache_read + input_tokens
        cache_hit_rate = cache_read / denom_hit if denom_hit > 0 else 0.0
        # cache_coverage: 整体缓存占比
        denom_cov = cache_read + input_tokens + cache_write
        cache_coverage = cache_read / denom_cov if denom_cov > 0 else 0.0
        return cache_hit_rate, cache_coverage

    def _build_record(self, model, usage, error=None, meta=None, upstream_model=None):
        """构建一条统计记录。

        usage 为空时仍可记录一次错误调用（error 非空）。meta 可携带：
        - duration_ms: 总耗时（毫秒）
        - first_token_ms: 首 token 耗时（毫秒，流式请求）
        - output_tokens_per_sec: 输出 token 速度（tok/s）

        §6 别名：model 为对外名（展示用），upstream_model 为实际上游名（计价用）。
        """
        input_tokens = usage.get("input_tokens", 0)
        cache_read = usage.get("cache_read_input_tokens", 0)
        cache_write = usage.get("cache_creation_input_tokens", 0)
        output_tokens = usage.get("output_tokens", 0)
        cache_hit_rate, cache_coverage = self._compute_rates(
            input_tokens, cache_read, cache_write
        )
        ts = datetime.now().strftime("%Y-%m-%dT%H:%M:%S")
        cost = 0.0 if self.no_cost else self._calc_cost(
            upstream_model or model, input_tokens, cache_read, cache_write,
            output_tokens, account=self.account, timestamp=ts
        )
        record = {
            "timestamp": ts,
            "service": self.service,
            "account": self.account,
            "model": model,
            "status": "error" if error else "ok",
            "input_tokens": input_tokens,
            "cache_read_tokens": cache_read,
            "cache_write_tokens": cache_write,
            "output_tokens": output_tokens,
            "cache_hit_rate": round(cache_hit_rate, 4),
            "cache_coverage": round(cache_coverage, 4),
            "cost": round(cost, 6),
        }
        if self.service_id:
            record["service_id"] = self.service_id  # 稳定身份；service 显示名保持原样
        if upstream_model and upstream_model != model:
            record["upstream_model"] = upstream_model  # 计价用上游名（双端一致）
        if error:
            record["error"] = error
        if meta:
            for key in ("duration_ms", "first_token_ms", "output_tokens_per_sec"):
                if key in meta and meta[key] is not None:
                    record[key] = round(float(meta[key]), 2)
        return record

    def _format_log(self, record):
        """格式化单次请求的缓存日志。"""
        hit_pct = record["cache_hit_rate"] * 100
        return (
            f"[CACHE] {record['model']} "
            f"hit={hit_pct:.1f}% "
            f"read={record['cache_read_tokens']:,} "
            f"write={record['cache_write_tokens']:,} "
            f"input={record['input_tokens']:,} "
            f"out={record['output_tokens']:,}"
        )

    def record(self, model, usage, error=None, meta=None, upstream_model=None):
        """记录一次请求的缓存统计（成功或失败）。

        error 非空时记录为一次失败调用；usage 可为空字典。meta 见 _build_record。
        upstream_model 为实际上游模型名（§6 别名映射时用于计价），记录的 model
        保持对外名。
        """
        if not usage and not error:
            return
        record = self._build_record(model, usage or {}, error=error, meta=meta,
                                    upstream_model=upstream_model)

        with self._lock:
            # 写入 JSONL（文件锁防多进程，仅 Unix 支持）
            filepath = self._get_today_file()
            try:
                with open(filepath, "a") as f:
                    if HAS_FCNTL:
                        fcntl.flock(f.fileno(), fcntl.LOCK_EX)
                    f.write(json.dumps(record, ensure_ascii=False) + "\n")
                    if HAS_FCNTL:
                        fcntl.flock(f.fileno(), fcntl.LOCK_UN)
            except OSError as e:
                logger.warning(f"[CACHE] Failed to write record: {e}")

            # 懒检查：跨小时则打印上一小时汇总
            current_hour = record["timestamp"][:13]
            if self._last_hour and current_hour != self._last_hour:
                self._print_hourly_summary(self._last_hour)
            self._last_hour = current_hour

            # 更新小时聚合
            self._update_hourly_summary(record)

        # 打印单次请求日志
        logger.info(self._format_log(record))

    def _update_hourly_summary(self, record):
        """更新当天的小时聚合 JSON（按服务分目录，跨进程加锁）。"""
        date_str = record["timestamp"][:10]
        hour_str = record["timestamp"][11:13]
        summary_path = os.path.join(self._summary_root(), f"{date_str}.json")

        summary = {}
        try:
            with open(summary_path, "r") as f:
                if HAS_FCNTL:
                    fcntl.flock(f.fileno(), fcntl.LOCK_EX)
                raw = f.read()
                if HAS_FCNTL:
                    fcntl.flock(f.fileno(), fcntl.LOCK_UN)
            if raw.strip():
                summary = json.loads(raw)
        except (json.JSONDecodeError, OSError):
            summary = {}

        if "hours" not in summary:
            summary["date"] = date_str
            summary["hours"] = {}

        h = summary["hours"].setdefault(hour_str, {
            "requests": 0,
            "total_input_tokens": 0,
            "total_cache_read_tokens": 0,
            "total_cache_write_tokens": 0,
            "total_output_tokens": 0,
            "total_cost": 0.0,
            "_hit_rate_sum": 0.0,
            "_coverage_sum": 0.0,
        })
        h["requests"] += 1
        h["total_input_tokens"] += record["input_tokens"]
        h["total_cache_read_tokens"] += record["cache_read_tokens"]
        h["total_cache_write_tokens"] += record["cache_write_tokens"]
        h["total_output_tokens"] += record["output_tokens"]
        h["total_cost"] = h.get("total_cost", 0.0) + record.get("cost", 0.0)
        h["_hit_rate_sum"] += record["cache_hit_rate"]
        h["_coverage_sum"] += record["cache_coverage"]

        try:
            with open(summary_path, "w") as f:
                if HAS_FCNTL:
                    fcntl.flock(f.fileno(), fcntl.LOCK_EX)
                json.dump(summary, f, ensure_ascii=False)
                if HAS_FCNTL:
                    fcntl.flock(f.fileno(), fcntl.LOCK_UN)
        except OSError as e:
            logger.warning(f"[CACHE] Failed to write summary: {e}")

    def _print_hourly_summary(self, hour_str):
        """打印上一小时的汇总日志。"""
        date_str = hour_str[:10]
        hour = hour_str[11:13] if len(hour_str) >= 13 else hour_str[-2:]
        summary = None
        for d in self._summary_read_dirs():
            p = os.path.join(d, f"{date_str}.json")
            if os.path.exists(p):
                try:
                    with open(p, encoding="utf-8") as f:
                        summary = json.load(f)
                    break
                except (json.JSONDecodeError, OSError):
                    pass
        if not summary:
            return
        try:
            h = summary.get("hours", {}).get(hour)
            if h and h["requests"] > 0:
                avg_hit = h["_hit_rate_sum"] / h["requests"] * 100
                logger.info(
                    f"[CACHE HOURLY {date_str}T{hour}] "
                    f"requests={h['requests']} "
                    f"avg_hit={avg_hit:.1f}% "
                    f"total_read={h['total_cache_read_tokens']:,} "
                    f"total_write={h['total_cache_write_tokens']:,} "
                    f"total_input={h['total_input_tokens']:,}"
                )
        except (json.JSONDecodeError, OSError):
            pass

    def get_summary(self, period="day"):
        """返回聚合统计。"""
        with self._lock:
            if period == "hour":
                return self._get_last_hour_summary()
            elif period == "day":
                return self._get_day_summary()
            elif period == "all":
                return self._get_all_summary()
            else:
                return {"error": f"unknown period: {period}"}

    def _load_day_summary(self, date_str):
        """加载某天的 summary JSON（id 目录优先，名字目录兜底），清理内部字段。"""
        summary = None
        for d in self._summary_read_dirs():
            summary_path = os.path.join(d, f"{date_str}.json")
            if not os.path.exists(summary_path):
                continue
            try:
                with open(summary_path, encoding="utf-8") as f:
                    summary = json.load(f)
                break
            except (json.JSONDecodeError, OSError):
                continue
        if summary is None:
            return None

        # 清理内部字段，计算 avg
        hours_list = []
        daily = {
            "requests": 0,
            "total_input_tokens": 0,
            "total_cache_read_tokens": 0,
            "total_cache_write_tokens": 0,
            "total_output_tokens": 0,
            "total_cost": 0.0,
        }
        for hour, h in sorted(summary.get("hours", {}).items()):
            req = h["requests"]
            hour_cost = h.get("total_cost", 0.0)
            hours_list.append({
                "hour": f"{date_str}T{hour}:00:00",
                "requests": req,
                "avg_cache_hit_rate": round(h["_hit_rate_sum"] / req, 4) if req else 0.0,
                "avg_cache_coverage": round(h["_coverage_sum"] / req, 4) if req else 0.0,
                "total_cache_read_tokens": h["total_cache_read_tokens"],
                "total_cache_write_tokens": h["total_cache_write_tokens"],
                "total_input_tokens": h["total_input_tokens"],
                "total_output_tokens": h["total_output_tokens"],
                "total_cost": round(hour_cost, 6),
            })
            daily["requests"] += req
            daily["total_input_tokens"] += h["total_input_tokens"]
            daily["total_cache_read_tokens"] += h["total_cache_read_tokens"]
            daily["total_cache_write_tokens"] += h["total_cache_write_tokens"]
            daily["total_output_tokens"] += h["total_output_tokens"]
            daily["total_cost"] += hour_cost

        denom_hit = daily["total_cache_read_tokens"] + daily["total_input_tokens"]
        denom_cov = denom_hit + daily["total_cache_write_tokens"]
        daily["avg_cache_hit_rate"] = round(
            daily["total_cache_read_tokens"] / denom_hit, 4
        ) if denom_hit > 0 else 0.0
        daily["avg_cache_coverage"] = round(
            daily["total_cache_read_tokens"] / denom_cov, 4
        ) if denom_cov > 0 else 0.0

        return {"date": date_str, "hours": hours_list, "daily_total": daily}

    def _get_last_hour_summary(self):
        """返回最近一小时的统计。"""
        date_str = datetime.now().strftime("%Y-%m-%d")
        hour_str = datetime.now().strftime("%H")
        day_data = self._load_day_summary(date_str)
        if not day_data:
            return {"period": "hour", "hour": f"{date_str}T{hour_str}", "requests": 0}
        for h in day_data["hours"]:
            if h["hour"][11:13] == hour_str:
                return {"period": "hour", **h}
        return {"period": "hour", "hour": f"{date_str}T{hour_str}", "requests": 0}

    def _get_day_summary(self):
        """返回今天的统计。"""
        date_str = datetime.now().strftime("%Y-%m-%d")
        day_data = self._load_day_summary(date_str)
        if not day_data:
            return {"period": "day", "date": date_str, "requests": 0}
        return {"period": "day", **day_data}

    def _get_all_summary(self):
        """返回所有天的汇总（id 目录与名字目录的日期并集，id 目录优先）。"""
        dates = []  # 保序去重
        for d in self._summary_read_dirs():
            if not os.path.isdir(d):
                continue
            for filename in sorted(os.listdir(d)):
                if filename.endswith(".json") and filename[:-5] not in dates:
                    dates.append(filename[:-5])
        dates.sort()
        days = []
        total = {
            "requests": 0,
            "total_input_tokens": 0,
            "total_cache_read_tokens": 0,
            "total_cache_write_tokens": 0,
            "total_output_tokens": 0,
            "total_cost": 0.0,
        }
        for date_str in dates:
            day_data = self._load_day_summary(date_str)
            if day_data:
                days.append(day_data)
                dt = day_data["daily_total"]
                total["requests"] += dt["requests"]
                total["total_input_tokens"] += dt["total_input_tokens"]
                total["total_cache_read_tokens"] += dt["total_cache_read_tokens"]
                total["total_cache_write_tokens"] += dt["total_cache_write_tokens"]
                total["total_output_tokens"] += dt["total_output_tokens"]
                total["total_cost"] += dt.get("total_cost", 0.0)

        denom_hit = total["total_cache_read_tokens"] + total["total_input_tokens"]
        denom_cov = denom_hit + total["total_cache_write_tokens"]
        total["avg_cache_hit_rate"] = round(
            total["total_cache_read_tokens"] / denom_hit, 4
        ) if denom_hit > 0 else 0.0
        total["avg_cache_coverage"] = round(
            total["total_cache_read_tokens"] / denom_cov, 4
        ) if denom_cov > 0 else 0.0

        return {"period": "all", "days": days, "total": total}


# 全局缓存统计实例（按服务区分）
_stats = {}
_stats_lock = threading.Lock()


def get_stats(service=None, account=None, no_cost=False, service_id=None):
    """获取 CacheStats 实例（线程安全的懒初始化，按服务 id / 名字区分）。"""
    key = service_id or service or "default"
    if key not in _stats:
        with _stats_lock:
            if key not in _stats:  # 双重检查
                stats_dir = os.environ.get("CACHE_STATS_DIR", "data/cache_stats")
                retention = int(os.environ.get("CACHE_STATS_RETENTION_DAYS", "30"))
                _stats[key] = CacheStats(stats_dir=stats_dir, retention_days=retention,
                                         service=service, account=account, no_cost=no_cost,
                                         service_id=service_id)
    return _stats[key]


def is_cache_stats_enabled():
    """检查缓存统计是否启用（默认开启）。"""
    return os.environ.get("CACHE_STATS_ENABLED", "true").lower() in ("true", "1", "yes")


def get_account_summary(account_id, period="day"):
    """按账号聚合其下所有服务的统计（服务级 summary 动态归并，避免双写一致性问题）。"""
    services = load_config()
    matched = [s for s in services if s.account.id == account_id]
    if not matched:
        return {"period": period, "account": account_id, "requests": 0}
    if period == "all":
        total = {
            "requests": 0, "total_input_tokens": 0, "total_cache_read_tokens": 0,
            "total_cache_write_tokens": 0, "total_output_tokens": 0, "total_cost": 0.0,
        }
        days = []
        for svc in matched:
            s = get_stats(svc.name, svc.account.id, service_id=svc.id).get_summary("all")
            for d in s.get("days", []):
                for k, v in d.get("daily_total", {}).items():
                    if k in total:
                        total[k] += v
                days.append(d)
        return {"period": "all", "account": account_id, "days": days, "total": total}
    # day / hour：合并 daily_total，hours 按时间排序叠加
    agg_daily = {
        "requests": 0, "total_input_tokens": 0, "total_cache_read_tokens": 0,
        "total_cache_write_tokens": 0, "total_output_tokens": 0, "total_cost": 0.0,
    }
    hours = {}
    for svc in matched:
        s = get_stats(svc.name, svc.account.id, service_id=svc.id).get_summary(period)
        daily = s.get("daily_total") if period == "day" else s
        if not daily:
            continue
        for k, v in daily.items():
            if k in agg_daily:
                agg_daily[k] += v
        for h in s.get("hours", []):
            hid = h.get("hour", "")
            if hid in hours:
                cur = hours[hid]
                cur["requests"] += h.get("requests", 0)
                cur["total_input_tokens"] += h.get("total_input_tokens", 0)
                cur["total_cache_read_tokens"] += h.get("total_cache_read_tokens", 0)
                cur["total_cache_write_tokens"] += h.get("total_cache_write_tokens", 0)
                cur["total_output_tokens"] += h.get("total_output_tokens", 0)
                cur["total_cost"] += h.get("total_cost", 0.0)
                cur["avg_cache_hit_rate"] = (
                    (cur["avg_cache_hit_rate"] + h.get("avg_cache_hit_rate", 0.0)) / 2
                )
                cur["avg_cache_coverage"] = (
                    (cur["avg_cache_coverage"] + h.get("avg_cache_coverage", 0.0)) / 2
                )
            else:
                hours[hid] = dict(h)
    agg_daily["avg_cache_hit_rate"] = (
        agg_daily["total_cache_read_tokens"]
        / (agg_daily["total_cache_read_tokens"] + agg_daily["total_input_tokens"])
        if (agg_daily["total_cache_read_tokens"] + agg_daily["total_input_tokens"]) > 0 else 0.0
    )
    agg_daily["avg_cache_coverage"] = (
        agg_daily["total_cache_read_tokens"]
        / (agg_daily["total_cache_read_tokens"] + agg_daily["total_cache_write_tokens"])
        if (agg_daily["total_cache_read_tokens"] + agg_daily["total_cache_write_tokens"]) > 0 else 0.0
    )
    return {
        "period": period, "account": account_id,
        "hours": [hours[h] for h in sorted(hours)],
        "daily_total": agg_daily,
    }