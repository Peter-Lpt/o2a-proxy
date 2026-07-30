#!/usr/bin/env python3
"""从 config.json 读取配置，输出 shell export 语句。"""
import json
import sys

if len(sys.argv) < 2:
    print("用法: load_config.py <config.json>", file=sys.stderr)
    sys.exit(1)

with open(sys.argv[1]) as f:
    config = json.load(f)

services = config.get("services", [])
if not services:
    print("echo '错误: config.json 中 services 为空'; exit 1", file=sys.stderr)
    sys.exit(1)

svc = services[0]
print(f'export PROXY_HOST="127.0.0.1"')
print(f'export PROXY_PORT="{svc.get("listen_address", "8317")}"')
print(f'export DASHSCOPE_URL="{svc.get("openai_base_url", "")}"')
print(f'export DASHSCOPE_API_KEY="{svc.get("openai_api_key", "")}"')
print(f'export PROXY_MODEL="{svc.get("model", "qwen-plus")}"')
print(f'export SUB_PROXY_MODEL="{svc.get("sub_model", svc.get("model", "qwen-plus"))}"')
print(f'export CACHE_STATS_ENABLED="{str(config.get("cache_stats_enabled", True)).lower()}"')
print(f'export CACHE_STATS_DIR="{config.get("cache_stats_dir", "cache_stats")}"')
print(f'export CACHE_STATS_RETENTION_DAYS="{config.get("cache_stats_retention_days", 30)}"')
