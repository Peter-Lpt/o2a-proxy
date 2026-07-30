#!/usr/bin/env node
/**
 * Node 版缓存统计聚合 —— 复刻 proxy.py 的 CacheStats 逻辑。
 * 读取 cache_stats/summary/YYYY-MM-DD.json 与 cache_stats/YYYY-MM-DD.jsonl，
 * 输出：
 *   - current : 本小时聚合
 *   - today   : 今日聚合
 *   - month   : 本月聚合
 *   - todayHourly : 今日逐小时（0..23，缺省补 0）含命中率曲线点
 *   - monthDaily  : 本月逐日（1..今日，缺省补 0）含命中率曲线点
 * 与 Python 端口径一致：
 *   cache_hit_rate   = read / (read + input)            —— 对齐 Anthropic 官方定义
 *   cache_coverage   = read / (read + input + write)
 * 其中 input 为「实际输入」（已扣除缓存读/写）。
 */
const fs = require("fs");
const path = require("path");

// 加载定价数据
function loadPricing() {
  try {
    const p = path.join(__dirname, "..", "pricing.json");
    return JSON.parse(fs.readFileSync(p, "utf-8"));
  } catch (_) {
    return null;
  }
}

// 加载配置获取当前模型
function loadModel() {
  try {
    const p = path.join(__dirname, "..", "config.json");
    const cfg = JSON.parse(fs.readFileSync(p, "utf-8"));
    return cfg.services?.[0]?.model || "";
  } catch (_) {
    return "";
  }
}

// 计算费用
function calcCost(model, input, read, write, output, requests) {
  const pricing = loadPricing();
  if (!pricing) return 0;

  // 查找模型定价（支持多平台）
  let price = null;
  for (const provider of Object.keys(pricing)) {
    if (provider.startsWith("_")) continue;
    const models = pricing[provider]?.models;
    if (models && models[model]) {
      price = models[model];
      break;
    }
  }
  if (!price) return 0;

  // 选择价格档：按单次请求平均输入 tokens 判断
  // 因为 summary 是聚合数据，无法知道每个请求的实际 tier
  const avgInput = requests > 0 ? (input + read + write) / requests : (input + read + write);
  const tier = price.tiers.find(t => {
    if (t.range === "unlimited") return true;
    const match = t.range.match(/(\d+)-(\d+)K/);
    if (!match) return true;
    const min = parseInt(match[1]) * 1024;
    const max = parseInt(match[2]) * 1024;
    return avgInput >= min && avgInput < max;
  }) || price.tiers[0];

  if (!tier) return 0;

  // 计算费用（CNY/百万token）
  const inputCost = input * (tier.input || 0) / 1_000_000;
  const outputCost = output * (tier.output || 0) / 1_000_000;
  // 缓存读按 input × 0.2 计费（隐式缓存）
  const cacheReadCost = read * (tier.input || 0) * 0.2 / 1_000_000;
  // 缓存写 DashScope 不返回，计为 0
  const cacheWriteCost = 0;

  return inputCost + outputCost + cacheReadCost + cacheWriteCost;
}

function readJsonSafe(p) {
  try {
    return JSON.parse(fs.readFileSync(p, "utf-8"));
  } catch (_) {
    return null;
  }
}

function aggregateHours(hours, model) {
  const t = {
    requests: 0,
    input: 0,
    read: 0,
    write: 0,
    output: 0,
    cost: 0,
  };
  for (const h of Object.values(hours || {})) {
    t.requests += h.requests || 0;
    t.input += h.total_input_tokens || 0;
    t.read += h.total_cache_read_tokens || 0;
    t.write += h.total_cache_write_tokens || 0;
    t.output += h.total_output_tokens || 0;
    t.cost += calcCost(model, h.total_input_tokens || 0, h.total_cache_read_tokens || 0, h.total_cache_write_tokens || 0, h.total_output_tokens || 0, h.requests || 1);
  }
  const denomHit = t.read + t.input;
  const denomCov = denomHit + t.write;
  return {
    ...t,
    hitRate: denomHit > 0 ? t.read / denomHit : 0,
    coverage: denomCov > 0 ? t.read / denomCov : 0,
  };
}

function hourRate(h) {
  const read = h.total_cache_read_tokens || 0;
  const input = h.total_input_tokens || 0;
  const denom = read + input;
  return denom > 0 ? read / denom : 0;
}

class Stats {
  constructor(cacheStatsDir) {
    this.dir = cacheStatsDir;
    this.summaryDir = path.join(cacheStatsDir, "summary");
  }

  /** 读取某天 summary，返回 {date, day, hours:{HH:hourAgg}} 或 null */
  loadDay(dateStr, model) {
    const p = path.join(this.summaryDir, `${dateStr}.json`);
    const raw = readJsonSafe(p);
    if (!raw || !raw.hours) return null;
    const hours = {};
    for (const [hh, h] of Object.entries(raw.hours)) {
      const cost = calcCost(model, h.total_input_tokens || 0, h.total_cache_read_tokens || 0, h.total_cache_write_tokens || 0, h.total_output_tokens || 0, h.requests || 1);
      hours[hh] = {
        requests: h.requests || 0,
        input: h.total_input_tokens || 0,
        read: h.total_cache_read_tokens || 0,
        write: h.total_cache_write_tokens || 0,
        output: h.total_output_tokens || 0,
        cost,
        hitRate: hourRate(h),
      };
    }
    return { date: dateStr, day: aggregateHours(raw.hours, model), hours };
  }

  listDayFiles() {
    try {
      return fs.readdirSync(this.summaryDir).filter((f) => f.endsWith(".json"));
    } catch (_) {
      return [];
    }
  }

  getStats() {
    const now = new Date();
    const pad = (n) => String(n).padStart(2, "0");
    const todayStr = `${now.getFullYear()}-${pad(now.getMonth() + 1)}-${pad(now.getDate())}`;
    const monthPrefix = `${now.getFullYear()}-${pad(now.getMonth() + 1)}`;
    const curHour = pad(now.getHours());
    const model = loadModel();

    const today = this.loadDay(todayStr, model);
    const current = today && today.hours[curHour]
      ? { hour: curHour, ...today.hours[curHour] }
      : { hour: curHour, requests: 0, input: 0, read: 0, write: 0, output: 0, cost: 0, hitRate: 0 };

    // 今日逐小时（0..23 连续，补 0）
    const todayHourly = [];
    for (let h = 0; h < 24; h++) {
      const hh = pad(h);
      const d = today && today.hours[hh];
      todayHourly.push({
        hour: hh,
        requests: d ? d.requests : 0,
        input: d ? d.input : 0,
        read: d ? d.read : 0,
        write: d ? d.write : 0,
        output: d ? d.output : 0,
        cost: d ? d.cost : 0,
        hitRate: d ? d.hitRate : 0,
      });
    }

    // 本月逐日
    const dayFiles = this.listDayFiles()
      .map((f) => f.replace(/\.json$/, ""))
      .filter((d) => d.startsWith(monthPrefix))
      .sort();
    const daysMap = {};
    let monthTotal = { requests: 0, input: 0, read: 0, write: 0, output: 0, cost: 0 };
    for (const ds of dayFiles) {
      const dd = this.loadDay(ds, model);
      if (!dd) continue;
      daysMap[ds] = dd.day;
      monthTotal.requests += dd.day.requests;
      monthTotal.input += dd.day.input;
      monthTotal.read += dd.day.read;
      monthTotal.write += dd.day.write;
      monthTotal.output += dd.day.output;
      monthTotal.cost += dd.day.cost || 0;
    }
    const mDenomHit = monthTotal.read + monthTotal.input;
    const mDenomCov = mDenomHit + monthTotal.write;
    const month = {
      ...monthTotal,
      hitRate: mDenomHit > 0 ? monthTotal.read / mDenomHit : 0,
      coverage: mDenomCov > 0 ? monthTotal.read / mDenomCov : 0,
      days: dayFiles.length,
    };

    // 本月逐日曲线（1..今日，补 0）
    const monthDaily = [];
    const daysInMonth = new Date(now.getFullYear(), now.getMonth() + 1, 0).getDate();
    const upTo = Math.min(now.getDate(), daysInMonth);
    for (let d = 1; d <= upTo; d++) {
      const ds = `${monthPrefix}-${pad(d)}`;
      const dd = daysMap[ds];
      monthDaily.push({
        date: ds,
        requests: dd ? dd.requests : 0,
        input: dd ? dd.input : 0,
        read: dd ? dd.read : 0,
        write: dd ? dd.write : 0,
        output: dd ? dd.output : 0,
        cost: dd ? dd.cost || 0 : 0,
        hitRate: dd ? dd.hitRate : 0,
      });
    }

    return {
      updatedAt: now.toISOString(),
      current,
      today: today ? { date: todayStr, ...today.day } : { date: todayStr, requests: 0, input: 0, read: 0, write: 0, output: 0, cost: 0, hitRate: 0, coverage: 0 },
      month,
      todayHourly,
      monthDaily,
    };
  }
}

module.exports = Stats;

// 直接运行自测
if (require.main === module) {
  const dir = process.argv[2] || path.join(__dirname, "..", "cache_stats");
  const s = new Stats(dir);
  const r = s.getStats();
  console.log(JSON.stringify(r, null, 2));
}
