#!/usr/bin/env python3
"""
Anthropic -> OpenAI 协议转换代理（完整流式支持）
将 Claude Code 发出的 Anthropic Messages API 请求转换为 OpenAI 格式，
转发给 OpenAI 兼容 API，再将响应转回 Anthropic 格式。
"""

import json
import logging
import os
import select
import socket
import sys
import time
import ssl
import threading
from datetime import datetime, timedelta
from http.server import HTTPServer, BaseHTTPRequestHandler
from socketserver import ThreadingMixIn
from urllib.request import Request, urlopen
from urllib.error import URLError

# fcntl is Unix-only, provide fallback for Windows
try:
    import fcntl
    HAS_FCNTL = True
except ImportError:
    HAS_FCNTL = False

# 配置
LISTEN_HOST = os.environ.get("PROXY_HOST", "127.0.0.1")
LISTEN_PORT = int(os.environ.get("PROXY_PORT", "8317"))
TARGET_URL = os.environ.get(
    "DASHSCOPE_URL",
    "https://dashscope.aliyuncs.com/compatible-mode/v1/chat/completions"
)
API_KEY = os.environ.get("DASHSCOPE_API_KEY", "")
PROXY = os.environ.get("HTTP_PROXY", "")
PROXY_MODEL = os.environ.get("PROXY_MODEL", "qwen-plus")
# 最大输出 token 数（mac 客户端「1M 上下文」开关会将其设为 1,000,000）
PROXY_MAX_TOKENS = int(os.environ.get("PROXY_MAX_TOKENS", "4096"))
# 子 agent 模型配置（Claude Code 的 Task 工具会启动子 agent，使用 haiku 等模型）
# 默认与主 agent 相同，可单独配置
SUB_PROXY_MODEL = os.environ.get("SUB_PROXY_MODEL", PROXY_MODEL)
# 流式响应总超时（秒），防止模型长时间卡在推理阶段
STREAM_TIMEOUT = int(os.environ.get("STREAM_TIMEOUT", "600"))

logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s [%(levelname)s] %(message)s",
)
logger = logging.getLogger("proxy")


class ThreadedHTTPServer(ThreadingMixIn, HTTPServer):
    daemon_threads = True


class CacheStats:
    """缓存命中统计：记录、聚合、查询。"""

    def __init__(self, stats_dir="cache_stats", retention_days=30):
        self.stats_dir = stats_dir
        self.retention_days = retention_days
        self._lock = threading.Lock()
        self._last_hour = None
        os.makedirs(os.path.join(stats_dir, "summary"), exist_ok=True)
        self._cleanup_old_files()

    def _cleanup_old_files(self):
        """启动时清理超过保留天数的文件。"""
        cutoff = datetime.now() - timedelta(days=self.retention_days)
        cutoff_ts = cutoff.timestamp()
        for subdir in ["", "summary"]:
            dirpath = os.path.join(self.stats_dir, subdir) if subdir else self.stats_dir
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

    def _build_record(self, model, usage):
        """构建一条统计记录。"""
        input_tokens = usage.get("input_tokens", 0)
        cache_read = usage.get("cache_read_input_tokens", 0)
        cache_write = usage.get("cache_creation_input_tokens", 0)
        output_tokens = usage.get("output_tokens", 0)
        cache_hit_rate, cache_coverage = self._compute_rates(
            input_tokens, cache_read, cache_write
        )
        return {
            "timestamp": datetime.now().strftime("%Y-%m-%dT%H:%M:%S"),
            "model": model,
            "input_tokens": input_tokens,
            "cache_read_tokens": cache_read,
            "cache_write_tokens": cache_write,
            "output_tokens": output_tokens,
            "cache_hit_rate": round(cache_hit_rate, 4),
            "cache_coverage": round(cache_coverage, 4),
        }

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

    def record(self, model, usage):
        """记录一次请求的缓存统计。"""
        if not usage:
            return
        record = self._build_record(model, usage)

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
        """更新当天的小时聚合 JSON。"""
        date_str = record["timestamp"][:10]
        hour_str = record["timestamp"][11:13]
        summary_path = os.path.join(
            self.stats_dir, "summary", f"{date_str}.json"
        )

        summary = {}
        if os.path.exists(summary_path):
            try:
                with open(summary_path, "r") as f:
                    summary = json.load(f)
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
            "_hit_rate_sum": 0.0,
            "_coverage_sum": 0.0,
        })
        h["requests"] += 1
        h["total_input_tokens"] += record["input_tokens"]
        h["total_cache_read_tokens"] += record["cache_read_tokens"]
        h["total_cache_write_tokens"] += record["cache_write_tokens"]
        h["total_output_tokens"] += record["output_tokens"]
        h["_hit_rate_sum"] += record["cache_hit_rate"]
        h["_coverage_sum"] += record["cache_coverage"]

        try:
            with open(summary_path, "w") as f:
                json.dump(summary, f, ensure_ascii=False)
        except OSError as e:
            logger.warning(f"[CACHE] Failed to write summary: {e}")

    def _print_hourly_summary(self, hour_str):
        """打印上一小时的汇总日志。"""
        date_str = hour_str[:10]
        hour = hour_str[11:13] if len(hour_str) >= 13 else hour_str[-2:]
        summary_path = os.path.join(
            self.stats_dir, "summary", f"{date_str}.json"
        )
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
        summary_path = os.path.join(
            self.stats_dir, "summary", f"{date_str}.json"
        )
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
        }
        for hour, h in sorted(summary.get("hours", {}).items()):
            req = h["requests"]
            hours_list.append({
                "hour": f"{date_str}T{hour}:00:00",
                "requests": req,
                "avg_cache_hit_rate": round(h["_hit_rate_sum"] / req, 4) if req else 0.0,
                "avg_cache_coverage": round(h["_coverage_sum"] / req, 4) if req else 0.0,
                "total_cache_read_tokens": h["total_cache_read_tokens"],
                "total_cache_write_tokens": h["total_cache_write_tokens"],
                "total_input_tokens": h["total_input_tokens"],
                "total_output_tokens": h["total_output_tokens"],
            })
            daily["requests"] += req
            daily["total_input_tokens"] += h["total_input_tokens"]
            daily["total_cache_read_tokens"] += h["total_cache_read_tokens"]
            daily["total_cache_write_tokens"] += h["total_cache_write_tokens"]
            daily["total_output_tokens"] += h["total_output_tokens"]

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
        summary_dir = os.path.join(self.stats_dir, "summary")
        days = []
        total = {
            "requests": 0,
            "total_input_tokens": 0,
            "total_cache_read_tokens": 0,
            "total_cache_write_tokens": 0,
            "total_output_tokens": 0,
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

        denom_hit = total["total_cache_read_tokens"] + total["total_input_tokens"]
        denom_cov = denom_hit + total["total_cache_write_tokens"]
        total["avg_cache_hit_rate"] = round(
            total["total_cache_read_tokens"] / denom_hit, 4
        ) if denom_hit > 0 else 0.0
        total["avg_cache_coverage"] = round(
            total["total_cache_read_tokens"] / denom_cov, 4
        ) if denom_cov > 0 else 0.0

        return {"period": "all", "days": days, "total": total}


# 全局缓存统计实例
_stats = None
_stats_lock = threading.Lock()


def get_stats():
    """获取全局 CacheStats 实例（线程安全的懒初始化）。"""
    global _stats
    if _stats is None:
        with _stats_lock:
            if _stats is None:  # 双重检查
                stats_dir = os.environ.get("CACHE_STATS_DIR", "cache_stats")
                retention = int(os.environ.get("CACHE_STATS_RETENTION_DAYS", "30"))
                _stats = CacheStats(stats_dir=stats_dir, retention_days=retention)
    return _stats


def is_cache_stats_enabled():
    """检查缓存统计是否启用（默认开启）。"""
    return os.environ.get("CACHE_STATS_ENABLED", "true").lower() in ("true", "1", "yes")


def sse_event(data, event_type=None):
    """格式化 SSE 事件。"""
    lines = []
    if event_type is None and isinstance(data, dict):
        event_type = data.get("type")
    if event_type:
        lines.append(f"event: {event_type}")
    lines.append(f"data: {json.dumps(data)}")
    lines.append("")
    return "\n".join(lines) + "\n"


def _to_int(value, default=0):
    """Best-effort conversion for provider usage fields."""
    if value is None:
        return default
    try:
        return int(value)
    except (TypeError, ValueError):
        return default


def _convert_usage(usage):
    """Convert OpenAI-compatible usage into Anthropic usage semantics."""
    usage = usage or {}
    prompt_details = usage.get("prompt_tokens_details") or {}
    input_details = usage.get("input_tokens_details") or {}

    prompt_total = _to_int(
        usage.get("prompt_tokens", usage.get("input_tokens", 0))
    )
    output_tokens = _to_int(
        usage.get("completion_tokens", usage.get("output_tokens", 0))
    )

    cached_tokens = _to_int(
        prompt_details.get(
            "cached_tokens",
            prompt_details.get(
                "cache_read_input_tokens",
                input_details.get(
                    "cached_tokens",
                    input_details.get(
                        "cache_read_input_tokens",
                        usage.get("cache_read_input_tokens", usage.get("cached_tokens", 0)),
                    ),
                ),
            ),
        )
    )
    cache_write_tokens = _to_int(
        prompt_details.get(
            "cache_creation_input_tokens",
            prompt_details.get(
                "cache_write_tokens",
                input_details.get(
                    "cache_write_tokens",
                    input_details.get(
                        "cache_creation_input_tokens",
                        usage.get("cache_creation_input_tokens", usage.get("cache_write_tokens", 0)),
                    ),
                ),
            ),
        )
    )

    # Anthropic reports cache writes separately from ordinary input tokens.
    input_tokens = max(0, prompt_total - cached_tokens - cache_write_tokens)

    completion_details = usage.get("completion_tokens_details") or {}
    reasoning_tokens = _to_int(completion_details.get("reasoning_tokens", 0))

    return {
        "input_tokens": input_tokens,
        "output_tokens": output_tokens,
        "cache_creation_input_tokens": cache_write_tokens,
        "cache_read_input_tokens": cached_tokens,
        "reasoning_tokens": reasoning_tokens,
        "prompt_total": prompt_total,
    }


def _anthropic_stop_reason(finish_reason, has_tool_calls=False):
    """Map OpenAI finish_reason values to Anthropic stop_reason values."""
    if has_tool_calls or finish_reason == "tool_calls":
        return "tool_use"
    if finish_reason == "length":
        return "max_tokens"
    if finish_reason in ("stop", None, ""):
        return "end_turn"
    if finish_reason == "content_filter":
        return "stop_sequence"
    return finish_reason


def _extract_text(content):
    """将 Anthropic content blocks 转为纯文本字符串。"""
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        parts = []
        for block in content:
            if isinstance(block, str):
                parts.append(block)
            elif isinstance(block, dict):
                if block.get("type") == "text":
                    parts.append(block.get("text", ""))
                elif block.get("type") == "tool_result":
                    content_val = block.get("content", "")
                    if isinstance(content_val, str):
                        parts.append(content_val)
                    elif isinstance(content_val, list):
                        for cb in content_val:
                            if isinstance(cb, dict) and cb.get("type") == "text":
                                parts.append(cb.get("text", ""))
            else:
                parts.append(str(block))
        return "\n".join(parts)
    return str(content)


def convert_tool_input(input_schema):
    """将 Anthropic input_schema 转为 OpenAI function parameters 格式。"""
    if not isinstance(input_schema, dict):
        return input_schema
    params = dict(input_schema)
    if "type" not in params:
        params["type"] = "object"
    return params


def _strip_cache_control(obj):
    """递归移除 cache_control 字段（DashScope 不支持）。"""
    if isinstance(obj, dict):
        return {k: _strip_cache_control(v) for k, v in obj.items() if k != "cache_control"}
    elif isinstance(obj, list):
        return [_strip_cache_control(item) for item in obj]
    return obj


def convert_request(req):
    """将 Anthropic Messages 格式转为 OpenAI chat completions 格式。"""
    raw_messages = list(req.get("messages", []))

    messages = []
    for msg in raw_messages:
        role = msg.get("role", "user")
        content = msg.get("content", "")

        if isinstance(content, list):
            # 检查是否包含 tool_result blocks
            tool_results = [b for b in content if isinstance(b, dict) and b.get("type") == "tool_result"]
            if tool_results:
                # 转换为 OpenAI tool 消息格式
                # 先收集同一条 Anthropic 消息中的纯文本块（不属于 tool_result）
                orphan_text_parts = []
                for block in content:
                    if isinstance(block, dict) and block.get("type") == "tool_result":
                        tool_id = block.get("tool_use_id", block.get("id", ""))
                        content_val = block.get("content", "")
                        text = _extract_text(content_val)
                        messages.append({
                            "role": "tool",
                            "tool_call_id": tool_id,
                            "content": text,
                        })
                    elif isinstance(block, dict) and block.get("type") == "text":
                        # 与 tool_result 同行的文本块没有 tool_use_id，
                        # 收集后作为 user 消息追加，避免生成非法的空 tool_call_id
                        orphan_text_parts.append(block.get("text", ""))
                if orphan_text_parts:
                    messages.append({
                        "role": "user",
                        "content": "\n".join(orphan_text_parts),
                    })
                continue
            # 检查 assistant 消息是否包含 tool_use
            if role == "assistant":
                tool_uses = [b for b in content if isinstance(b, dict) and b.get("type") == "tool_use"]
                if tool_uses:
                    text_parts = []
                    tool_calls = []
                    for block in content:
                        if isinstance(block, dict) and block.get("type") == "text":
                            text_parts.append(block.get("text", ""))
                        elif isinstance(block, dict) and block.get("type") == "tool_use":
                            tc = {
                                "id": block.get("id", ""),
                                "type": "function",
                                "function": {
                                    "name": block.get("name", ""),
                                    "arguments": json.dumps(block.get("input", {})),
                                },
                            }
                            tool_calls.append(tc)
                    oai_msg = {"role": "assistant", "content": None}
                    if tool_calls:
                        oai_msg["tool_calls"] = tool_calls
                    if text_parts:
                        oai_msg["content"] = "\n".join(text_parts)
                    messages.append(oai_msg)
                    continue

        # 普通文本消息 - 转为纯文本（DashScope 不支持 content blocks 格式）
        if isinstance(content, list):
            messages.append({
                "role": role,
                "content": _extract_text(content),
            })
        else:
            messages.append({
                "role": role,
                "content": _extract_text(content),
            })

    system = req.get("system", "")
    if system:
        # 转为纯文本（DashScope 不支持 content blocks 格式）
        system_content = _extract_text(system)
        messages.insert(0, {"role": "system", "content": system_content})

    is_stream = req.get("stream", False)
    # 根据模型名判断是否子 agent 请求，使用对应的模型配置
    # Claude Code 子 agent (Task 工具) 使用 haiku 模型名
    client_model = req.get("model", "")
    is_subagent = "haiku" in client_model.lower()
    model = SUB_PROXY_MODEL if is_subagent else PROXY_MODEL
    if is_subagent:
        logger.debug(f"[SUBAGENT] Detected sub-agent request: {client_model} -> {model}")
    openai_req = {
        "model": model,
        "messages": messages,
        "max_tokens": req.get("max_tokens", PROXY_MAX_TOKENS),
        "stream": is_stream,
    }
    if is_stream:
        openai_req["stream_options"] = {"include_usage": True}

    # 转发采样参数（子 agent 可能设置特定 temperature）
    if "temperature" in req:
        openai_req["temperature"] = req["temperature"]
    if "top_p" in req:
        openai_req["top_p"] = req["top_p"]

    # 处理 thinking 参数（Claude Code 的扩展思考功能）
    # 上游 API 如果支持 reasoning 模式，会通过模型本身启用，这里仅记录日志
    if "thinking" in req:
        thinking_config = req["thinking"]
        logger.debug(f"[THINKING] Received thinking config: {thinking_config}")

    # 转换 tools: Anthropic -> OpenAI
    tools = req.get("tools", [])
    if tools:
        openai_tools = []
        for tool in tools:
            if isinstance(tool, dict):
                name = tool.get("name", "")
                description = tool.get("description", "")
                input_schema = tool.get("input_schema", {})
                openai_tools.append({
                    "type": "function",
                    "function": {
                        "name": name,
                        "description": description,
                        "parameters": convert_tool_input(input_schema),
                        "strict": False,
                    },
                })
        openai_req["tools"] = openai_tools

    # 转换 tool_choice: Anthropic -> OpenAI
    tool_choice = req.get("tool_choice")
    if tool_choice:
        if tool_choice == "any" or tool_choice == "auto":
            openai_req["tool_choice"] = "auto"
        elif tool_choice == "none":
            openai_req["tool_choice"] = "none"
        elif isinstance(tool_choice, dict):
            tool_type = tool_choice.get("type", "")
            if tool_type == "tool":
                openai_req["tool_choice"] = {
                    "type": "function",
                    "function": {"name": tool_choice.get("name", "")},
                }

    return openai_req


# 不再使用 raw socket，统一用 urllib


class AnthropicProxyHandler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, format, *args):
        logger.info(format % args)

    def _w(self, data: bytes):
        """直接写原始数据到 wfile，不触发 chunked 编码。"""
        self.wfile.write(data)
        self.wfile.flush()

    def do_POST(self):
        req_start = time.time()
        content_length = int(self.headers.get("Content-Length", 0))
        body = self.rfile.read(content_length)

        try:
            anthropic_request = json.loads(body)
        except json.JSONDecodeError:
            self._send_error(400, "invalid json")
            return

        logger.debug(f"Request: {json.dumps(anthropic_request)[:500]}")

        stream = anthropic_request.get("stream", False)
        logger.info(f"[REQ] received model={anthropic_request.get('model')} stream={stream} "
                    f"bytes={content_length} messages={len(anthropic_request.get('messages', []))} "
                    f"tools={len(anthropic_request.get('tools', []))} "
                    f"has_thinking={'thinking' in anthropic_request} "
                    f"elapsed={time.time()-req_start:.3f}s")
        openai_request = convert_request(anthropic_request)
        # 移除 cache_control（DashScope 不支持）
        openai_request = _strip_cache_control(openai_request)
        logger.debug(f"Converted: {json.dumps(openai_request)[:500]}")
        self._req_start = req_start

        if stream:
            self._handle_stream(openai_request)
        else:
            self._handle_non_stream(openai_request)

    def _handle_stream(self, openai_request):
        """处理流式请求，使用 urllib 的流式读取（自动处理 HTTP chunked encoding）。"""
        from urllib.request import Request, urlopen

        target = TARGET_URL
        # 转发查询参数（如 ?beta=true）
        if self.path and "?" in self.path:
            target += "?" + self.path.split("?", 1)[1]
        req = Request(target, method="POST")
        req.add_header("Content-Type", "application/json")
        req.add_header("Authorization", f"Bearer {API_KEY}")
        if PROXY:
            proxy_host = PROXY.replace("http://", "").replace("https://", "")
            req.set_proxy(proxy_host, "https")

        body = json.dumps(openai_request).encode("utf-8")
        conn_start = time.time()
        logger.info(f"[FWD] forwarding stream model={openai_request.get('model')} "
                    f"url={target} timeout=120s payload_bytes={len(body)} "
                    f"elapsed_since_req={time.time()-(getattr(self, '_req_start', None) or conn_start):.3f}s")

        try:
            response = urlopen(req, body, timeout=120)
            status_code = response.getcode()
            logger.info(f"[FWD] upstream connected status={status_code} "
                        f"connect_time={time.time()-conn_start:.2f}s "
                        f"elapsed_since_req={time.time()-(getattr(self, '_req_start', None) or conn_start):.2f}s")
        except Exception as e:
            if hasattr(e, 'read'):
                try:
                    err_body = e.read().decode("utf-8", errors="replace")
                except Exception:
                    err_body = str(e)
            else:
                err_body = str(e)
            logger.error(f"Upstream request failed: {err_body[:500]}")
            logger.error(f"Sent payload (first 1000 chars): {body[:1000]}")
            self._send_error(502, f"upstream error: {err_body[:300]}")
            return

        if status_code != 200:
            err_body = response.read().decode("utf-8", errors="replace")
            logger.error(f"Upstream error {status_code}: {err_body}")
            logger.error(f"Sent payload (first 1000 chars): {body[:1000]}")
            self._send_error(status_code, f"upstream error: {err_body}")
            return

        # 发送响应头给客户端，使用 chunked transfer encoding
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream; charset=utf-8")
        self.send_header("Cache-Control", "no-cache")
        self.send_header("X-Accel-Buffering", "no")
        self.send_header("Transfer-Encoding", "chunked")
        self.end_headers()
        logger.info(f"[STREAM] response headers sent to client model={PROXY_MODEL}")

        message_id = "proxy-msg-stream"
        model = PROXY_MODEL
        started = False
        finished = False
        input_tokens = 0
        output_tokens = 0
        cached_tokens = 0
        cache_write_tokens = 0
        reasoning_tokens = 0
        latest_usage = {
            "input_tokens": 0,
            "output_tokens": 0,
            "cache_creation_input_tokens": 0,
            "cache_read_input_tokens": 0,
        }
        content_block_idx = 0
        content_block_open = False
        thinking_block_open = False
        pending_finish_reason = None  # 保存待处理的 finish_reason
        tool_input_buf = {}  # 本地变量，避免多线程竞争
        tool_index_to_block_index = {}
        client_disconnected = False  # 跟踪客户端连接状态
        n_chunks = 0
        reasoning_bytes = 0
        text_bytes = 0
        first_chunk_ts = None
        last_prog_ts = time.time()

        def write_chunk(data: bytes) -> bool:
            """写入一个 chunk（HTTP chunked encoding 格式）
            返回 True 表示写入成功，False 表示客户端已断开
            """
            nonlocal client_disconnected
            if client_disconnected:
                return False
            if data:
                try:
                    chunk_size = format(len(data), 'x').encode()
                    self.wfile.write(chunk_size + b"\r\n" + data + b"\r\n")
                    self.wfile.flush()
                except (BrokenPipeError, ConnectionResetError):
                    client_disconnected = True
                    return False
            return True

        def end_chunked():
            """结束 chunked 响应"""
            nonlocal client_disconnected
            if client_disconnected:
                return
            try:
                self.wfile.write(b"0\r\n\r\n")
                self.wfile.flush()
            except (BrokenPipeError, ConnectionResetError):
                client_disconnected = True

        try:
            while True:
                line = response.readline()
                if not line:
                    break
                line = line.decode("utf-8").strip()
                if not line.startswith("data:"):
                    continue

                n_chunks += 1
                if first_chunk_ts is None:
                    first_chunk_ts = time.time()
                    logger.info(f"[STREAM] first upstream chunk received "
                                f"time_to_first_token={first_chunk_ts - conn_start:.2f}s "
                                f"elapsed_since_req={first_chunk_ts - (getattr(self, '_req_start', None) or conn_start):.2f}s")
                now = time.time()
                req_elapsed = now - (getattr(self, '_req_start', None) or conn_start)
                if req_elapsed > STREAM_TIMEOUT:
                    logger.warning(f"[STREAM] timeout after {req_elapsed:.1f}s "
                                   f"(limit={STREAM_TIMEOUT}s) chunks={n_chunks} "
                                   f"reasoning_bytes={reasoning_bytes} text_bytes={text_bytes}")
                    # 关闭未关闭的 block
                    if thinking_block_open:
                        write_chunk(sse_event({
                            "type": "content_block_stop",
                            "index": content_block_idx,
                        }).encode())
                        thinking_block_open = False
                    if content_block_open:
                        write_chunk(sse_event({
                            "type": "content_block_stop",
                            "index": content_block_idx,
                        }).encode())
                        content_block_open = False
                    # 发送超时错误给客户端
                    if started:
                        write_chunk(sse_event({
                            "type": "message_delta",
                            "delta": {"stop_reason": "max_tokens"},
                            "usage": latest_usage,
                        }).encode())
                    write_chunk(sse_event({"type": "message_stop"}).encode())
                    finished = True
                    break
                if now - last_prog_ts >= 5.0:
                    last_prog_ts = now
                    logger.info(f"[STREAM] progress chunks={n_chunks} "
                                f"elapsed={req_elapsed:.1f}s "
                                f"reasoning_bytes={reasoning_bytes} text_bytes={text_bytes}")

                data_str = line[5:].strip()
                if data_str == "[DONE]":
                    if not finished and started:
                        # 关闭未关闭的 thinking block
                        if thinking_block_open:
                            write_chunk(sse_event({
                                "type": "content_block_stop",
                                "index": content_block_idx,
                            }).encode())
                            thinking_block_open = False
                        if content_block_open:
                            write_chunk(sse_event({
                                "type": "content_block_stop",
                                "index": content_block_idx,
                            }).encode())
                            content_block_open = False
                        # 使用保存的 finish_reason，如果没有则默认为 stop
                        stop_reason = _anthropic_stop_reason(pending_finish_reason)
                        write_chunk(sse_event({
                            "type": "message_delta",
                            "delta": {"stop_reason": stop_reason, "stop_sequence": None},
                            "usage": latest_usage,
                        }).encode())
                        write_chunk(sse_event({"type": "message_stop"}).encode())
                        finished = True
                        # 记录缓存统计
                        if is_cache_stats_enabled() and latest_usage and latest_usage.get("input_tokens"):
                            get_stats().record(model, latest_usage)
                    logger.info(f"[STREAM] completed finished={finished} chunks={n_chunks} "
                                f"total_elapsed={time.time() - (getattr(self, '_req_start', None) or conn_start):.2f}s "
                                f"reasoning_bytes={reasoning_bytes} text_bytes={text_bytes} "
                                f"input={latest_usage.get('input_tokens')} output={latest_usage.get('output_tokens')}")
                    break

                try:
                    chunk = json.loads(data_str)
                except json.JSONDecodeError:
                    continue

                usage = chunk.get("usage", {})
                if usage:
                    # 调试日志：原始数据
                    logger.debug(f"[DEBUG] Raw usage: {json.dumps(usage)}")

                    converted_usage = _convert_usage(usage)
                    input_tokens = converted_usage["input_tokens"]
                    output_tokens = converted_usage["output_tokens"] or output_tokens
                    cached_tokens = converted_usage["cache_read_input_tokens"]
                    cache_write_tokens = converted_usage["cache_creation_input_tokens"]
                    reasoning_tokens = converted_usage["reasoning_tokens"]
                    latest_usage = {
                        "input_tokens": input_tokens,
                        "output_tokens": output_tokens,
                        "cache_creation_input_tokens": cache_write_tokens,
                        "cache_read_input_tokens": cached_tokens,
                    }
                    
                    # 调试日志
                    logger.debug(f"[DEBUG] cached_tokens={cached_tokens}, cache_write_tokens={cache_write_tokens}, input_tokens={input_tokens}, prompt_total={converted_usage['prompt_total']}, reasoning_tokens={reasoning_tokens}")

                choices = chunk.get("choices", [])
                if not choices:
                    # 最后一个 chunk（choices 为空）包含 usage，发送结束事件
                    if pending_finish_reason and not finished:
                        stop_reason = _anthropic_stop_reason(pending_finish_reason)
                        write_chunk(sse_event({
                            "type": "message_delta",
                            "delta": {"stop_reason": stop_reason, "stop_sequence": None},
                            "usage": latest_usage,
                        }).encode())
                        write_chunk(sse_event({"type": "message_stop"}).encode())
                        finished = True
                        # 记录缓存统计
                        if is_cache_stats_enabled() and latest_usage and latest_usage.get("input_tokens"):
                            get_stats().record(model, latest_usage)
                    continue

                choice = choices[0]
                delta = choice.get("delta", {})
                finish_reason = choice.get("finish_reason")

                if not started:
                    started = True
                    message_id = chunk.get("id", message_id)
                    model = chunk.get("model", model)

                    write_chunk(sse_event({
                        "type": "message_start",
                        "message": {
                            "id": message_id,
                            "type": "message",
                            "role": "assistant",
                            "content": [],
                            "model": model,
                            "stop_reason": None,
                            "stop_sequence": None,
                            "usage": {
                                "input_tokens": input_tokens,
                                "output_tokens": 0,
                                "cache_creation_input_tokens": cache_write_tokens,
                                "cache_read_input_tokens": cached_tokens,
                            },
                        },
                    }).encode())

                # 处理思考内容 (reasoning_content -> thinking block)
                reasoning_content = delta.get("reasoning_content", "")
                if reasoning_content:
                    if not thinking_block_open:
                        # 关闭已打开的文本块
                        if content_block_open:
                            write_chunk(sse_event({
                                "type": "content_block_stop",
                                "index": content_block_idx,
                            }).encode())
                            content_block_open = False
                            content_block_idx += 1
                        # 开启 thinking block
                        write_chunk(sse_event({
                            "type": "content_block_start",
                            "index": content_block_idx,
                            "content_block": {"type": "thinking", "thinking": ""},
                        }).encode())
                        thinking_block_open = True
                    write_chunk(sse_event({
                        "type": "content_block_delta",
                        "index": content_block_idx,
                        "delta": {"type": "thinking_delta", "thinking": reasoning_content},
                    }).encode())
                    reasoning_bytes += len(reasoning_content)
                elif thinking_block_open and not reasoning_content:
                    # reasoning_content 结束，关闭 thinking block
                    write_chunk(sse_event({
                        "type": "content_block_stop",
                        "index": content_block_idx,
                    }).encode())
                    thinking_block_open = False
                    content_block_idx += 1

                # 处理文本内容
                content = delta.get("content", "")
                if content:
                    # 如果 thinking block 还开着，先关闭
                    if thinking_block_open:
                        write_chunk(sse_event({
                            "type": "content_block_stop",
                            "index": content_block_idx,
                        }).encode())
                        thinking_block_open = False
                        content_block_idx += 1
                    if not content_block_open:
                        write_chunk(sse_event({
                            "type": "content_block_start",
                            "index": content_block_idx,
                            "content_block": {"type": "text", "text": ""},
                        }).encode())
                        content_block_open = True
                    write_chunk(sse_event({
                        "type": "content_block_delta",
                        "index": content_block_idx,
                        "delta": {"type": "text_delta", "text": content},
                    }).encode())
                    text_bytes += len(content)

                # 处理 tool_calls
                tool_calls = delta.get("tool_calls", [])
                for tc in tool_calls:
                    tool_call_index = int(tc.get("index", len(tool_index_to_block_index)))
                    tc_id = tc.get("id", "")
                    tc_func = tc.get("function", {})
                    tc_name = tc_func.get("name", "")
                    tc_args = tc_func.get("arguments", "")

                    # 如果有 id，说明是新的 tool_use 开始
                    if tc_id:
                        # 关闭之前的 thinking block
                        if thinking_block_open:
                            write_chunk(sse_event({
                                "type": "content_block_stop",
                                "index": content_block_idx,
                            }).encode())
                            thinking_block_open = False
                            content_block_idx += 1
                        # 关闭之前的文本内容块
                        if content_block_open:
                            write_chunk(sse_event({
                                "type": "content_block_stop",
                                "index": content_block_idx,
                            }).encode())
                            content_block_open = False
                            content_block_idx += 1

                        block_idx = content_block_idx
                        tool_index_to_block_index[tool_call_index] = block_idx
                        write_chunk(sse_event({
                            "type": "content_block_start",
                            "index": block_idx,
                            "content_block": {
                                "type": "tool_use",
                                "id": tc_id,
                                "name": tc_name,
                                "input": {},
                            },
                        }).encode())
                        # 初始化 tool_use 输入（后续 chunks 会累积）
                        tool_input_buf[block_idx] = {
                            "id": tc_id,
                            "name": tc_name,
                            "input_str": tc_args,
                        }
                        content_block_idx += 1
                    elif tc_args:
                        # 同一 tool_use 的参数片段累积
                        idx = tool_index_to_block_index.get(tool_call_index, content_block_idx)
                        if idx in tool_input_buf:
                            tool_input_buf[idx]["input_str"] += tc_args
                        else:
                            tool_input_buf[idx] = {
                                "id": "",
                                "name": tc_name,
                                "input_str": tc_args,
                            }
                    # 发送 content_block_delta（OpenAI 的 tool_calls 对应 Anthropic 的 input_json_delta）
                    if tc_args:
                        write_chunk(sse_event({
                            "type": "content_block_delta",
                            "index": tool_index_to_block_index.get(tool_call_index, content_block_idx),
                            "delta": {"type": "input_json_delta", "partial_json": tc_args},
                        }).encode())
                        text_bytes += len(tc_args)

                if finish_reason and not finished:
                    # 保存 finish_reason，等待 usage 或 [DONE] 时再发送结束事件
                    pending_finish_reason = finish_reason
                    # 关闭未关闭的 thinking block
                    if thinking_block_open:
                        write_chunk(sse_event({
                            "type": "content_block_stop",
                            "index": content_block_idx,
                        }).encode())
                        thinking_block_open = False
                    # 关闭未关闭的 tool_use content_block
                    for idx, tool_info in tool_input_buf.items():
                        write_chunk(sse_event({
                            "type": "content_block_stop",
                            "index": idx,
                        }).encode())
                    tool_input_buf.clear()

                    if content_block_open:
                        write_chunk(sse_event({
                            "type": "content_block_stop",
                            "index": content_block_idx,
                        }).encode())
                        content_block_open = False

            if not finished:
                logger.info(f"[STREAM] upstream closed without [DONE] chunks={n_chunks} "
                            f"elapsed={time.time()-(getattr(self, '_req_start', None) or conn_start):.2f}s "
                            f"finished={finished} client_disconnected={client_disconnected}")
        except (BrokenPipeError, ConnectionResetError) as e:
            # 客户端断开连接，不是错误
            client_disconnected = True
            logger.info(f"Client disconnected: {e}")
        except Exception as e:
            logger.error(f"Stream error: {e}", exc_info=True)
            if not finished and started and not client_disconnected:
                write_chunk(sse_event({
                    "type": "error",
                    "error": {"type": "api_error", "message": str(e)},
                }).encode())
            elif not started and not client_disconnected:
                self._send_error(502, str(e))
        finally:
            end_chunked()
            response.close()

    def _handle_non_stream(self, openai_request):
        """处理非流式请求。"""
        target = TARGET_URL
        # 转发查询参数（如 ?beta=true）
        if self.path and "?" in self.path:
            target += "?" + self.path.split("?", 1)[1]
        req = Request(target, method="POST")
        req.add_header("Content-Type", "application/json")
        req.add_header("Authorization", f"Bearer {API_KEY}")
        if PROXY:
            proxy_host = PROXY.replace("http://", "").replace("https://", "")
            req.set_proxy(proxy_host, "https")

        body = json.dumps(openai_request).encode("utf-8")
        conn_start = time.time()
        logger.info(f"[FWD] forwarding(non-stream) model={openai_request.get('model')} "
                    f"url={target} timeout=120s payload_bytes={len(body)} "
                    f"elapsed_since_req={time.time()-(getattr(self, '_req_start', None) or conn_start):.3f}s")

        try:
            response = urlopen(req, body, timeout=120)
            status_code = response.getcode()
            logger.info(f"[FWD] upstream connected status={status_code} "
                        f"connect_time={time.time()-conn_start:.2f}s "
                        f"elapsed_since_req={time.time()-(getattr(self, '_req_start', None) or conn_start):.2f}s")
        except Exception as e:
            if hasattr(e, 'read'):
                try:
                    err_body = e.read().decode("utf-8", errors="replace")
                except Exception:
                    err_body = str(e)
            else:
                err_body = str(e)
            logger.error(f"Upstream request failed: {err_body[:500]}")
            logger.error(f"Sent payload (first 1000 chars): {body[:1000]}")
            self._send_error(502, f"upstream error: {err_body[:300]}")
            return

        if status_code != 200:
            err_body = response.read().decode("utf-8", errors="replace")
            logger.error(f"Upstream error {status_code}: {err_body}")
            logger.error(f"Sent payload (first 1000 chars): {body[:1000]}")
            self._send_error(status_code, f"upstream error: {err_body}")
            return

        raw = json.loads(response.read().decode("utf-8"))
        logger.info(f"[NONSTREAM] completed elapsed={time.time()-(getattr(self, '_req_start', None) or conn_start):.2f}s "
                    f"model={raw.get('model')} usage={raw.get('usage')}")

        content = ""
        finish_reason = "stop"
        tool_calls = []
        if raw.get("choices"):
            choice = raw["choices"][0]
            message = choice.get("message", {})
            content = message.get("content", "")
            finish_reason = choice.get("finish_reason", "stop")

            # 处理非流式 tool_calls
            raw_tool_calls = message.get("tool_calls", [])
            for tc in raw_tool_calls:
                try:
                    tc_input = json.loads(tc.get("function", {}).get("arguments", "{}"))
                except (json.JSONDecodeError, ValueError):
                    tc_input = tc.get("function", {}).get("arguments", "{}")
                tool_calls.append({
                    "type": "tool_use",
                    "id": tc.get("id", ""),
                    "name": tc.get("function", {}).get("name", ""),
                    "input": tc_input,
                })

        usage = raw.get("usage", {})
        converted_usage = _convert_usage(usage)
        input_tokens = converted_usage["input_tokens"]
        output_tokens = converted_usage["output_tokens"]
        cached_tokens = converted_usage["cache_read_input_tokens"]
        cache_write_tokens = converted_usage["cache_creation_input_tokens"]
        reasoning_tokens = converted_usage["reasoning_tokens"]

        content_list = []
        # 添加 thinking block（如果有思考内容）
        reasoning_content = ""
        if raw.get("choices"):
            choice = raw["choices"][0]
            message = choice.get("message", {})
            reasoning_content = message.get("reasoning_content", "")
        if reasoning_content:
            content_list.append({"type": "thinking", "thinking": reasoning_content})
        if content:
            content_list.append({"type": "text", "text": content})
        content_list.extend(tool_calls)

        stop_reason = _anthropic_stop_reason(finish_reason, bool(tool_calls))

        # 记录缓存统计
        if is_cache_stats_enabled() and converted_usage and converted_usage.get("input_tokens"):
            response_model = raw.get("model", PROXY_MODEL)
            get_stats().record(response_model, converted_usage)

        self._send_json(200, {
            "id": raw.get("id", "proxy-msg"),
            "type": "message",
            "role": "assistant",
            "content": content_list if content_list else [],
            "model": raw.get("model", PROXY_MODEL),
            "stop_reason": stop_reason,
            "stop_sequence": None,
            "usage": {
                "input_tokens": input_tokens,
                "output_tokens": output_tokens,
                "cache_creation_input_tokens": cache_write_tokens,
                "cache_read_input_tokens": cached_tokens,
            },
        })

    def _send_json(self, status_code, data):
        response = json.dumps(data).encode("utf-8")
        self.send_response(status_code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(response)))
        self.send_header("Connection", "close")
        self.end_headers()
        self.wfile.write(response)

    def _send_error(self, status_code, message):
        try:
            self._send_json(status_code, {
                "type": "error",
                "error": {"type": "api_error", "message": message},
            })
        except Exception:
            pass

    def _get_query_param(self, name, default=""):
        """解析 URL 查询参数。"""
        if "?" not in self.path:
            return default
        query = self.path.split("?", 1)[1]
        for part in query.split("&"):
            if "=" in part:
                k, v = part.split("=", 1)
                if k == name:
                    return v
            elif part == name:
                return "true"
        return default

    def do_GET(self):
        path = self.path.split("?")[0]
        if path == "/stats":
            if not is_cache_stats_enabled():
                self._send_json(200, {"error": "cache stats is disabled"})
                return
            period = self._get_query_param("period", "day")
            summary = get_stats().get_summary(period)
            self._send_json(200, summary)
        elif path == "/health":
            self._send_json(200, {"status": "ok"})
        else:
            self._send_json(200, {
                "status": "ok",
                "target": TARGET_URL,
                "endpoints": ["/stats?period=hour|day|all", "/health"],
            })


def main():
    if not API_KEY:
        logger.error("DASHSCOPE_API_KEY 环境变量未设置")
        sys.exit(1)

    server = ThreadedHTTPServer((LISTEN_HOST, LISTEN_PORT), AnthropicProxyHandler)
    logger.info(f"代理启动: http://{LISTEN_HOST}:{LISTEN_PORT}")
    logger.info(f"目标: {TARGET_URL}")
    logger.info(f"主 agent 模型: {PROXY_MODEL}")
    logger.info(f"子 agent 模型: {SUB_PROXY_MODEL}")

    try:
        server.serve_forever()
    except KeyboardInterrupt:
        logger.info("收到中断信号，关闭代理")
        server.shutdown()


if __name__ == "__main__":
    main()
