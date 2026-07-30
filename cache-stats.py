#!/usr/bin/env python3
"""
跨平台缓存统计查看工具
用法: python3 cache-stats.py [hour|day|all]
"""

import json
import os
import sys
from urllib.request import urlopen
from urllib.error import URLError


def main():
    # 解析参数
    period = sys.argv[1] if len(sys.argv) > 1 else "day"
    
    if period not in ("hour", "day", "all"):
        print("用法: python3 cache-stats.py [hour|day|all]")
        print("  hour - 当前小时")
        print("  day  - 今天（默认）")
        print("  all  - 全部")
        sys.exit(1)
    
    # 获取代理地址
    proxy_host = os.environ.get("PROXY_HOST", "127.0.0.1")
    proxy_port = os.environ.get("PROXY_PORT", "11011")
    proxy_url = f"http://{proxy_host}:{proxy_port}"
    
    print("=" * 44)
    print("  缓存统计 -", period)
    print("=" * 44)
    print()
    
    # 请求统计接口
    try:
        url = f"{proxy_url}/stats?period={period}"
        with urlopen(url, timeout=5) as response:
            data = json.loads(response.read().decode("utf-8"))
    except URLError as e:
        print(f"错误: 无法连接到代理 {proxy_url}")
        print(f"  {e}")
        sys.exit(1)
    except Exception as e:
        print(f"错误: {e}")
        sys.exit(1)
    
    # 检查是否有数据
    if data.get("requests", 0) == 0 and data.get("daily_total", {}).get("requests", 0) == 0:
        print("暂无统计数据")
        sys.exit(0)
    
    # 格式化输出
    period_type = data.get("period", "unknown")
    
    if period_type == "hour":
        print(f"时间段: {data.get('hour', 'N/A')}")
        print(f"请求数: {data.get('requests', 0)}")
        print(f"平均命中率: {data.get('avg_cache_hit_rate', 0) * 100:.1f}%")
        print(f"平均覆盖率: {data.get('avg_cache_coverage', 0) * 100:.1f}%")
        print(f"缓存读取: {data.get('total_cache_read_tokens', 0):,} tokens")
        print(f"缓存写入: {data.get('total_cache_write_tokens', 0):,} tokens")
        print(f"实际输入: {data.get('total_input_tokens', 0):,} tokens")
        print(f"输出: {data.get('total_output_tokens', 0):,} tokens")
    
    elif period_type == "day":
        daily = data.get("daily_total", {})
        print(f"日期: {data.get('date', 'N/A')}")
        print(f"请求数: {daily.get('requests', 0)}")
        print(f"平均命中率: {daily.get('avg_cache_hit_rate', 0) * 100:.1f}%")
        print(f"平均覆盖率: {daily.get('avg_cache_coverage', 0) * 100:.1f}%")
        print(f"缓存读取: {daily.get('total_cache_read_tokens', 0):,} tokens")
        print(f"缓存写入: {daily.get('total_cache_write_tokens', 0):,} tokens")
        print(f"实际输入: {daily.get('total_input_tokens', 0):,} tokens")
        print(f"输出: {daily.get('total_output_tokens', 0):,} tokens")
        
        hours = data.get("hours", [])
        if hours:
            print()
            print("按小时明细:")
            for h in hours:
                hour_str = h.get("hour", "")[11:13]
                req = h.get("requests", 0)
                hit = h.get("avg_cache_hit_rate", 0) * 100
                read = h.get("total_cache_read_tokens", 0)
                print(f"  {hour_str}:00 - {req:3d} 请求, 命中率 {hit:5.1f}%, 缓存读取 {read:>10,} tokens")
    
    elif period_type == "all":
        total = data.get("total", {})
        days = data.get("days", [])
        print(f"总请求数: {total.get('requests', 0)}")
        print(f"平均命中率: {total.get('avg_cache_hit_rate', 0) * 100:.1f}%")
        print(f"平均覆盖率: {total.get('avg_cache_coverage', 0) * 100:.1f}%")
        print(f"缓存读取: {total.get('total_cache_read_tokens', 0):,} tokens")
        print(f"缓存写入: {total.get('total_cache_write_tokens', 0):,} tokens")
        print(f"实际输入: {total.get('total_input_tokens', 0):,} tokens")
        print(f"输出: {total.get('total_output_tokens', 0):,} tokens")
        
        if days:
            print()
            print("按天明细（最近 7 天）:")
            for day in days[-7:]:
                date = day.get("date", "N/A")
                dt = day.get("daily_total", {})
                req = dt.get("requests", 0)
                hit = dt.get("avg_cache_hit_rate", 0) * 100
                read = dt.get("total_cache_read_tokens", 0)
                print(f"  {date} - {req:3d} 请求, 命中率 {hit:5.1f}%, 缓存读取 {read:>10,} tokens")


if __name__ == "__main__":
    main()
