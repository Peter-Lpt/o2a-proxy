#!/bin/bash
# 快速查看缓存统计

PROXY_URL="${PROXY_URL:-http://127.0.0.1:11011}"
PERIOD="${1:-day}"

case "$PERIOD" in
    hour|day|all)
        ;;
    *)
        echo "用法: $0 [hour|day|all]"
        echo "  hour - 当前小时"
        echo "  day  - 今天（默认）"
        echo "  all  - 全部"
        exit 1
        ;;
esac

echo "============================================"
echo "  缓存统计 - $PERIOD"
echo "============================================"
echo ""

response=$(curl -s "${PROXY_URL}/stats?period=${PERIOD}")

if [ -z "$response" ]; then
    echo "错误: 无法连接到代理 ${PROXY_URL}"
    exit 1
fi

# 解析并格式化输出
echo "$response" | python3 -c "
import json
import sys

data = json.load(sys.stdin)

if data.get('requests', 0) == 0 and data.get('daily_total', {}).get('requests', 0) == 0:
    print('暂无统计数据')
    sys.exit(0)

period = data.get('period', 'unknown')

if period == 'hour':
    print(f\"时间段: {data.get('hour', 'N/A')}\")
    print(f\"请求数: {data.get('requests', 0)}\")
    print(f\"平均命中率: {data.get('avg_cache_hit_rate', 0) * 100:.1f}%\")
    print(f\"平均覆盖率: {data.get('avg_cache_coverage', 0) * 100:.1f}%\")
    print(f\"缓存读取: {data.get('total_cache_read_tokens', 0):,} tokens\")
    print(f\"缓存写入: {data.get('total_cache_write_tokens', 0):,} tokens\")
    print(f\"实际输入: {data.get('total_input_tokens', 0):,} tokens\")
    print(f\"输出: {data.get('total_output_tokens', 0):,} tokens\")

elif period == 'day':
    daily = data.get('daily_total', {})
    print(f\"日期: {data.get('date', 'N/A')}\")
    print(f\"请求数: {daily.get('requests', 0)}\")
    print(f\"平均命中率: {daily.get('avg_cache_hit_rate', 0) * 100:.1f}%\")
    print(f\"平均覆盖率: {daily.get('avg_cache_coverage', 0) * 100:.1f}%\")
    print(f\"缓存读取: {daily.get('total_cache_read_tokens', 0):,} tokens\")
    print(f\"缓存写入: {daily.get('total_cache_write_tokens', 0):,} tokens\")
    print(f\"实际输入: {daily.get('total_input_tokens', 0):,} tokens\")
    print(f\"输出: {daily.get('total_output_tokens', 0):,} tokens\")
    
    hours = data.get('hours', [])
    if hours:
        print('')
        print('按小时明细:')
        for h in hours:
            hour_str = h.get('hour', '')[11:13]
            req = h.get('requests', 0)
            hit = h.get('avg_cache_hit_rate', 0) * 100
            read = h.get('total_cache_read_tokens', 0)
            print(f\"  {hour_str}:00 - {req:3d} 请求, 命中率 {hit:5.1f}%, 缓存读取 {read:>10,} tokens\")

elif period == 'all':
    total = data.get('total', {})
    days = data.get('days', [])
    print(f\"总请求数: {total.get('requests', 0)}\")
    print(f\"平均命中率: {total.get('avg_cache_hit_rate', 0) * 100:.1f}%\")
    print(f\"平均覆盖率: {total.get('avg_cache_coverage', 0) * 100:.1f}%\")
    print(f\"缓存读取: {total.get('total_cache_read_tokens', 0):,} tokens\")
    print(f\"缓存写入: {total.get('total_cache_write_tokens', 0):,} tokens\")
    print(f\"实际输入: {total.get('total_input_tokens', 0):,} tokens\")
    print(f\"输出: {total.get('total_output_tokens', 0):,} tokens\")
    
    if days:
        print('')
        print('按天明细:')
        for day in days[-7:]:  # 最近 7 天
            date = day.get('date', 'N/A')
            dt = day.get('daily_total', {})
            req = dt.get('requests', 0)
            hit = dt.get('avg_cache_hit_rate', 0) * 100
            read = dt.get('total_cache_read_tokens', 0)
            print(f\"  {date} - {req:3d} 请求, 命中率 {hit:5.1f}%, 缓存读取 {read:>10,} tokens\")
"
