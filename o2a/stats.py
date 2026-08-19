"""o2a-proxy 缓存统计：CacheStats 记录/聚合/查询、全局实例、账号级归并。

从原 proxy.py 拆出，逻辑逐字保留。
"""

import json
import os
import threading
from datetime import datetime, timedelta

from .base import HAS_FCNTL, PROJECT_ROOT, logger
from .config import load_config


class CacheStats:
    """缓存命中统计：记录、聚合、查询。service 非空时按服务分目录写 summary。"""

    def __init__(self, stats_dir="data/cache_stats", retention_days=30, service=None, account=None,
                 no_cost=False):
        self.stats_dir = stats_dir
        self.retention_days = retention_days
        self.service = service or ""
        self.account = account or ""
        self.no_cost = no_cost
        self._lock = threading.Lock()
        self._last_hour = None
        self._pricing = None
        os.makedirs(self._summary_root(), exist_ok=True)
        self._cleanup_old_files()

    def _summary_root(self):
        """summary 根目录；按服务分目录时返回其子目录。"""
        root = os.path.join(self.stats_dir, "summary")
        if self.service:
            return os.path.join(root, self.service)
        return root

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

    def _account_pricing(self, account, model):
        """在 pricing.json["accounts"] 中按账号 id/name 匹配模型价格，未命中返回 None。

        键可为账号 id 或 name（auth.json 同样支持两种键）。"""
        accounts_pricing = self._load_pricing().get("accounts")
        if not isinstance(accounts_pricing, dict) or not account:
            return None
        direct = accounts_pricing.get(account)
        if isinstance(direct, dict):
            m = (direct.get("models") or {}).get(model)
            if m is not None:
                return m
        # 通过 config.json 的账号列表把 id 映射到 name 再匹配
        for svc in load_config():
            acc = svc.account
            if acc.id == account and acc.name in accounts_pricing:
                m = (accounts_pricing[acc.name].get("models") or {}).get(model)
                if m is not None:
                    return m
        return None

    def _calc_cost(self, model, input_tokens, cache_read, cache_write, output_tokens, account=None):
        """计算单次请求的费用（CNY）。

        account 为账号 id（也可识别 name）；有账号级定价
        （pricing.json["accounts"][账号 id/name]）时优先，否则回退全局按模型名查找。
        """
        pricing = self._load_pricing()
        if not pricing:
            return 0.0
        # 查找模型定价：账号级优先，全局兜底
        price = self._account_pricing(account, model)
        if price is None:
            for provider in pricing:
                if provider.startswith("_") or provider == "accounts":
                    continue
                models = pricing[provider].get("models", {})
                if model in models:
                    price = models[model]
                    break
        if not price:
            return 0.0
        # 使用第一档价格（单次请求无法判断 tier）
        tier = price["tiers"][0] if price.get("tiers") else None
        if not tier:
            return 0.0
        input_cost = input_tokens * tier.get("input", 0) / 1_000_000
        output_cost = output_tokens * tier.get("output", 0) / 1_000_000
        # 缓存读：优先用 cache_hit 价格，否则按 input * 0.2
        if "cache_hit" in tier:
            cache_read_cost = cache_read * tier["cache_hit"] / 1_000_000
        else:
            cache_read_cost = cache_read * tier.get("input", 0) * 0.2 / 1_000_000
        # 缓存写：优先用 cache_miss 价格，否则按 input * 1.0
        if "cache_miss" in tier:
            cache_write_cost = cache_write * tier["cache_miss"] / 1_000_000
        else:
            cache_write_cost = cache_write * tier.get("input", 0) / 1_000_000
        return input_cost + output_cost + cache_read_cost + cache_write_cost

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

    def _build_record(self, model, usage, error=None, meta=None):
        """构建一条统计记录。

        usage 为空时仍可记录一次错误调用（error 非空）。meta 可携带：
        - duration_ms: 总耗时（毫秒）
        - first_token_ms: 首 token 耗时（毫秒，流式请求）
        - output_tokens_per_sec: 输出 token 速度（tok/s）
        """
        input_tokens = usage.get("input_tokens", 0)
        cache_read = usage.get("cache_read_input_tokens", 0)
        cache_write = usage.get("cache_creation_input_tokens", 0)
        output_tokens = usage.get("output_tokens", 0)
        cache_hit_rate, cache_coverage = self._compute_rates(
            input_tokens, cache_read, cache_write
        )
        cost = 0.0 if self.no_cost else self._calc_cost(
            model, input_tokens, cache_read, cache_write, output_tokens, account=self.account
        )
        record = {
            "timestamp": datetime.now().strftime("%Y-%m-%dT%H:%M:%S"),
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

    def record(self, model, usage, error=None, meta=None):
        """记录一次请求的缓存统计（成功或失败）。

        error 非空时记录为一次失败调用；usage 可为空字典。meta 见 _build_record。
        """
        if not usage and not error:
            return
        record = self._build_record(model, usage or {}, error=error, meta=meta)

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
        summary_path = os.path.join(self._summary_root(), f"{date_str}.json")
        if not os.path.exists(summary_path):
            return
        try:
            with open(summary_path, "r") as f:
                summary = json.load(f)
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
        """加载某天的 summary JSON，清理内部字段。"""
        summary_path = os.path.join(self._summary_root(), f"{date_str}.json")
        if not os.path.exists(summary_path):
            return None
        try:
            with open(summary_path, "r") as f:
                summary = json.load(f)
        except (json.JSONDecodeError, OSError):
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
        """返回所有天的汇总。"""
        summary_dir = self._summary_root()
        days = []
        total = {
            "requests": 0,
            "total_input_tokens": 0,
            "total_cache_read_tokens": 0,
            "total_cache_write_tokens": 0,
            "total_output_tokens": 0,
            "total_cost": 0.0,
        }
        for filename in sorted(os.listdir(summary_dir)):
            if not filename.endswith(".json"):
                continue
            date_str = filename[:-5]
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


def get_stats(service=None, account=None, no_cost=False):
    """获取 CacheStats 实例（线程安全的懒初始化，按服务区分）。"""
    key = service or "default"
    if key not in _stats:
        with _stats_lock:
            if key not in _stats:  # 双重检查
                stats_dir = os.environ.get("CACHE_STATS_DIR", "data/cache_stats")
                retention = int(os.environ.get("CACHE_STATS_RETENTION_DAYS", "30"))
                _stats[key] = CacheStats(stats_dir=stats_dir, retention_days=retention,
                                         service=service, account=account, no_cost=no_cost)
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
            s = get_stats(svc.name, svc.account.id).get_summary("all")
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
        s = get_stats(svc.name, svc.account.id).get_summary(period)
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