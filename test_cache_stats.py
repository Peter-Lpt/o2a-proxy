#!/usr/bin/env python3
"""测试 CacheStats 类。"""

import json
import os
import shutil
import sys
from datetime import datetime

# 导入 proxy 模块
sys.path.insert(0, os.path.dirname(__file__))
from proxy import CacheStats


def test_basic_record():
    """测试基本记录功能。"""
    print("Test 1: Basic record...")
    stats_dir = "/tmp/test_cache_stats"
    if os.path.exists(stats_dir):
        shutil.rmtree(stats_dir)
    
    stats = CacheStats(stats_dir=stats_dir, retention_days=30)
    
    usage = {
        "input_tokens": 1000,
        "cache_read_input_tokens": 5000,
        "cache_creation_input_tokens": 200,
        "output_tokens": 500,
    }
    
    stats.record("qwen-plus", usage)
    
    # 检查 JSONL 文件
    date_str = datetime.now().strftime("%Y-%m-%d")
    jsonl_file = os.path.join(stats_dir, f"{date_str}.jsonl")
    assert os.path.exists(jsonl_file), f"JSONL file not found: {jsonl_file}"
    
    with open(jsonl_file, "r") as f:
        record = json.loads(f.readline())
    
    assert record["model"] == "qwen-plus"
    assert record["input_tokens"] == 1000
    assert record["cache_read_tokens"] == 5000
    assert record["cache_write_tokens"] == 200
    assert record["output_tokens"] == 500
    assert abs(record["cache_hit_rate"] - 5000 / (5000 + 1000)) < 0.0001  # 0.8333
    assert abs(record["cache_coverage"] - 5000 / (5000 + 1000 + 200)) < 0.0001  # 0.8065
    
    print("✓ Basic record test passed")


def test_cache_hit_rate_formula():
    """测试缓存命中率公式（Anthropic 官方定义）。"""
    print("\nTest 2: Cache hit rate formula...")
    stats_dir = "/tmp/test_cache_stats2"
    if os.path.exists(stats_dir):
        shutil.rmtree(stats_dir)
    
    stats = CacheStats(stats_dir=stats_dir, retention_days=30)
    
    # 测试用例：cache_hit_rate 不含 cache_write
    usage = {
        "input_tokens": 1000,
        "cache_read_input_tokens": 9000,
        "cache_creation_input_tokens": 0,
        "output_tokens": 100,
    }
    
    hit_rate, coverage = stats._compute_rates(1000, 9000, 0)
    assert abs(hit_rate - 0.9) < 0.001, f"Expected 0.9, got {hit_rate}"
    assert abs(coverage - 0.9) < 0.001, f"Expected 0.9, got {coverage}"
    
    # 测试用例：有 cache_write
    usage = {
        "input_tokens": 1000,
        "cache_read_input_tokens": 8000,
        "cache_creation_input_tokens": 1000,
        "output_tokens": 100,
    }
    
    hit_rate, coverage = stats._compute_rates(1000, 8000, 1000)
    # cache_hit_rate = 8000 / (8000 + 1000) = 0.8889
    assert abs(hit_rate - 8000/9000) < 0.001, f"Expected 0.8889, got {hit_rate}"
    # cache_coverage = 8000 / (8000 + 1000 + 1000) = 0.8
    assert abs(coverage - 0.8) < 0.001, f"Expected 0.8, got {coverage}"
    
    print("✓ Cache hit rate formula test passed")


def test_zero_division():
    """测试除零保护。"""
    print("\nTest 3: Zero division protection...")
    stats_dir = "/tmp/test_cache_stats3"
    if os.path.exists(stats_dir):
        shutil.rmtree(stats_dir)
    
    stats = CacheStats(stats_dir=stats_dir, retention_days=30)
    
    # 所有 tokens 为 0
    hit_rate, coverage = stats._compute_rates(0, 0, 0)
    assert hit_rate == 0.0
    assert coverage == 0.0
    
    print("✓ Zero division test passed")


def test_summary_query():
    """测试统计查询。"""
    print("\nTest 4: Summary query...")
    stats_dir = "/tmp/test_cache_stats4"
    if os.path.exists(stats_dir):
        shutil.rmtree(stats_dir)
    
    stats = CacheStats(stats_dir=stats_dir, retention_days=30)
    
    # 记录多条数据
    for i in range(3):
        usage = {
            "input_tokens": 1000 + i * 100,
            "cache_read_input_tokens": 5000 + i * 500,
            "cache_creation_input_tokens": 200,
            "output_tokens": 500,
        }
        stats.record("qwen-plus", usage)
    
    # 查询当天统计
    summary = stats.get_summary("day")
    assert summary["period"] == "day"
    assert summary["daily_total"]["requests"] == 3
    assert summary["daily_total"]["total_input_tokens"] == 1000 + 1100 + 1200
    assert summary["daily_total"]["total_cache_read_tokens"] == 5000 + 5500 + 6000
    
    # 查询小时统计
    hour_summary = stats.get_summary("hour")
    assert hour_summary["period"] == "hour"
    assert hour_summary["requests"] == 3
    
    print("✓ Summary query test passed")


def test_cleanup():
    """测试文件清理。"""
    print("\nTest 5: File cleanup...")
    stats_dir = "/tmp/test_cache_stats5"
    if os.path.exists(stats_dir):
        shutil.rmtree(stats_dir)
    
    os.makedirs(stats_dir)
    
    # 创建一个旧文件（修改时间为 40 天前）
    old_file = os.path.join(stats_dir, "2026-06-01.jsonl")
    with open(old_file, "w") as f:
        f.write("{}\n")
    
    # 设置修改时间为 40 天前
    old_time = datetime.now().timestamp() - 40 * 24 * 3600
    os.utime(old_file, (old_time, old_time))
    
    # 创建 CacheStats（会自动清理）
    stats = CacheStats(stats_dir=stats_dir, retention_days=30)
    
    # 检查旧文件是否被删除
    assert not os.path.exists(old_file), "Old file should be cleaned up"
    
    print("✓ File cleanup test passed")


if __name__ == "__main__":
    print("Running CacheStats tests...\n")
    test_basic_record()
    test_cache_hit_rate_formula()
    test_zero_division()
    test_summary_query()
    test_cleanup()
    print("\n✓ All tests passed!")
