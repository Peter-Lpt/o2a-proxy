#!/bin/bash
# 缓存统计汇总 - 读取本地 jsonl 文件

cd "$(dirname "$0")"

DATE="${1:-$(date +%Y-%m-%d)}"
FILE="../data/cache_stats/${DATE}.jsonl"

if [ ! -f "$FILE" ]; then
    echo "文件不存在: $FILE"
    exit 1
fi

python3 -c "
import json, sys

file = sys.argv[1]
date = sys.argv[2]

ti = tc = to = c = 0
for line in open(file):
    r = json.loads(line)
    ti += r['input_tokens']
    tc += r['cache_read_tokens']
    to += r['output_tokens']
    c += 1

t = ti + tc
rate = tc / t * 100 if t else 0

print(f'日期: {date}')
print(f'请求数: {c}')
print(f'Input tokens: {ti:,}')
print(f'Cache read tokens: {tc:,}')
print(f'Output tokens: {to:,}')
print(f'整体缓存命中率: {rate:.2f}%')
" "$FILE" "$DATE"
