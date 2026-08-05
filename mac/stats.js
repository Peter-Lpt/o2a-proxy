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

// 第一个可代理(claude/codex)服务：历史遗留(无 service 标记)数据归它
function primaryService() {
  try {
    const cfg = JSON.parse(fs.readFileSync(path.join(__dirname, "..", "config.json"), "utf-8"));
    const s = (cfg.services || []).find((x) => x.mode === "claude" || x.mode === "codex");
    return s ? s.comment : "";
  } catch (_) { return ""; }
}

// 加载配置获取模型（仅用于旧 summary 无 total_cost 字段时的回退）
function loadModel(service) {
  try {
    const p = path.join(__dirname, "..", "config.json");
    const cfg = JSON.parse(fs.readFileSync(p, "utf-8"));
    if (service) {
      const s = (cfg.services || []).find((x) => x.comment === service);
      if (s) return s.model || "";
    }
    return cfg.services?.[0]?.model || "";
  } catch (_) {
    return "";
  }
}

// 计算费用（仅用于旧 summary 无 total_cost 字段时的回退）
function calcCostFallback(model, input, read, write, output, requests, forceFirstTier) {
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

  // 选择价格档
  let tier;
  if (forceFirstTier) {
    // 旧数据强制使用第一档（默认 200k 上下文内）
    tier = price.tiers[0];
  } else {
    // 按单次请求平均输入 tokens 判断
    const avgInput = requests > 0 ? (input + read + write) / requests : (input + read + write);
    tier = price.tiers.find(t => {
      if (t.range === "unlimited") return true;
      const match = t.range.match(/(\d+)-(\d+)K/);
      if (!match) return true;
      const min = parseInt(match[1]) * 1024;
      const max = parseInt(match[2]) * 1024;
      return avgInput >= min && avgInput < max;
    }) || price.tiers[0];
  }

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
    // 优先使用预计算的费用，否则回退到旧逻辑
    if (h.total_cost !== undefined) {
      t.cost += h.total_cost;
    } else {
      t.cost += calcCostFallback(model, h.total_input_tokens || 0, h.total_cache_read_tokens || 0, h.total_cache_write_tokens || 0, h.total_output_tokens || 0, h.requests || 1);
    }
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

  /**
   * 一次性迁移：把升级前的历史数据（根目录 summary/*.json + 无 service 字段的 jsonl）
   * 归入第一个可代理服务，使其与按服务归档的结构一致。幂等，可重复调用。
   */
  migrateLegacy() {
    if (this._migrated) return;
    this._migrated = true;
    const primary = primaryService();
    if (!primary) return;
    // 1) 根目录的全局 summary 移到 summary/<primary>/
    const target = path.join(this.summaryDir, primary);
    let hadLegacy = false;
    try {
      fs.mkdirSync(target, { recursive: true });
      for (const f of fs.readdirSync(this.summaryDir)) {
        const full = path.join(this.summaryDir, f);
        if (!f.endsWith(".json")) continue;
        let st; try { st = fs.statSync(full); } catch (_) { continue; }
        if (!st.isFile()) continue;
        hadLegacy = true;
        const dest = path.join(target, f);
        try { if (!fs.existsSync(dest)) fs.renameSync(full, dest); else fs.unlinkSync(full); } catch (_) {}
      }
    } catch (_) {}
    // 2) 为根目录 jsonl 中无 service 字段的记录补上 service=<primary>
    try {
      for (const f of fs.readdirSync(this.dir)) {
        if (!f.endsWith(".jsonl")) continue;
        const p = path.join(this.dir, f);
        let st; try { st = fs.statSync(p); } catch (_) { continue; }
        if (!st.isFile()) continue;
        const lines = fs.readFileSync(p, "utf-8").split("\n").filter(Boolean);
        let changed = false;
        const out = lines.map((ln) => {
          try {
            const rec = JSON.parse(ln);
            if (!rec.service) { rec.service = primary; changed = true; }
            return JSON.stringify(rec);
          } catch (_) { return ln; }
        });
        if (changed) fs.writeFileSync(p, out.join("\n") + "\n", "utf-8");
      }
    } catch (_) {}
    if (hadLegacy) console.log(`[stats] 历史数据已迁移到服务 "${primary}"`);
  }

  /** 列出所有 summary 来源：{service, dateStr}（兼容根目录全局 + 按服务子目录） */
  listSummarySources() {
    const out = [];
    const readDir = (dir) => { try { return fs.readdirSync(dir); } catch (_) { return []; } };
    for (const f of readDir(this.summaryDir)) {
      if (f.endsWith(".json")) out.push({ service: "", dateStr: f.replace(/\.json$/, "") });
    }
    for (const d of readDir(this.summaryDir)) {
      const sub = path.join(this.summaryDir, d);
      let st;
      try { st = fs.statSync(sub); } catch (_) { continue; }
      if (!st.isDirectory()) continue;
      for (const f of readDir(sub)) {
        if (f.endsWith(".json")) out.push({ service: d, dateStr: f.replace(/\.json$/, "") });
      }
    }
    return out;
  }

  listDayFiles() {
    const seen = new Set();
    for (const s of this.listSummarySources()) seen.add(s.dateStr);
    return [...seen];
  }

  /** 读取某天 summary（可按服务过滤，跨来源求和合并），返回 {date, day, hours} 或 null */
  loadDay(dateStr, model, service) {
    const includeLegacy = !!(service && service === primaryService());
    const sources = this.listSummarySources().filter(
      (s) => s.dateStr === dateStr && (
        !service || s.service === service || (includeLegacy && s.service === "")
      )
    );
    if (!sources.length) return null;
    const acc = {}; // HH -> {requests,input,read,write,output,cost}
    for (const src of sources) {
      const p = path.join(this.summaryDir, src.service, `${dateStr}.json`);
      const raw = readJsonSafe(p);
      if (!raw || !raw.hours) continue;
      for (const [hh, h] of Object.entries(raw.hours)) {
        if (!acc[hh]) acc[hh] = { requests: 0, input: 0, read: 0, write: 0, output: 0, cost: 0 };
        const g = acc[hh];
        g.requests += h.requests || 0;
        g.input += h.total_input_tokens || 0;
        g.read += h.total_cache_read_tokens || 0;
        g.write += h.total_cache_write_tokens || 0;
        g.output += h.total_output_tokens || 0;
        g.cost += h.total_cost !== undefined
          ? h.total_cost
          : calcCostFallback(model, h.total_input_tokens || 0, h.total_cache_read_tokens || 0, h.total_cache_write_tokens || 0, h.total_output_tokens || 0, h.requests || 1);
      }
    }
    const hours = {};
    for (const [hh, g] of Object.entries(acc)) {
      const denom = g.read + g.input;
      hours[hh] = { ...g, hitRate: denom > 0 ? g.read / denom : 0 };
    }
    const day = { requests: 0, input: 0, read: 0, write: 0, output: 0, cost: 0 };
    for (const g of Object.values(acc)) {
      day.requests += g.requests; day.input += g.input; day.read += g.read;
      day.write += g.write; day.output += g.output; day.cost += g.cost;
    }
    const denomHit = day.read + day.input;
    const denomCov = denomHit + day.write;
    day.hitRate = denomHit > 0 ? day.read / denomHit : 0;
    day.coverage = denomCov > 0 ? day.read / denomCov : 0;
    return { date: dateStr, day, hours };
  }

  getStats(service) {
    const now = new Date();
    const pad = (n) => String(n).padStart(2, "0");
    const todayStr = `${now.getFullYear()}-${pad(now.getMonth() + 1)}-${pad(now.getDate())}`;
    const monthPrefix = `${now.getFullYear()}-${pad(now.getMonth() + 1)}`;
    const curHour = pad(now.getHours());
    const model = loadModel(service);

    const today = this.loadDay(todayStr, model, service);
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

    // 今日逐分钟（从 jsonl 原始请求聚合，粒度分钟）
    const todayMinute = this._aggregateTodayMinute(todayStr, service);
    // 今日逐分钟按模型分组（用于图表按模型切换）
    const todayMinuteByModel = this._aggregateTodayMinuteByModel(todayStr, service);

    // 本月逐日
    const dayFiles = this.listDayFiles()
      .map((f) => f.replace(/\.json$/, ""))
      .filter((d) => d.startsWith(monthPrefix))
      .sort();
    const daysMap = {};
    let monthTotal = { requests: 0, input: 0, read: 0, write: 0, output: 0, cost: 0 };
    for (const ds of dayFiles) {
      const dd = this.loadDay(ds, model, service);
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

    // 按模型分组（今天 + 本月）
    const byModel = this._aggregateByModel(todayStr, service);
    const monthByModel = this._aggregateMonthByModel(monthPrefix, service);
    const monthDailyByModel = this._aggregateMonthDailyByModel(monthPrefix, service);

    return {
      updatedAt: now.toISOString(),
      current,
      today: today ? { date: todayStr, ...today.day } : { date: todayStr, requests: 0, input: 0, read: 0, write: 0, output: 0, cost: 0, hitRate: 0, coverage: 0 },
      month,
      todayHourly,
      todayMinute,
      todayMinuteByModel,
      monthDaily,
      monthDailyByModel,
      byModel,
      monthByModel,
    };
  }

  /** 从今天 jsonl 按分钟聚合统计（比 summary 小时粒度更细） */
  _aggregateTodayMinute(dateStr, service) {
    const byModel = this._aggregateTodayMinuteByModel(dateStr, service);
    const map = {};
    for (const model of Object.keys(byModel)) {
      for (const rec of byModel[model]) {
        const key = rec.minute;
        if (!map[key]) map[key] = { requests: 0, input: 0, read: 0, write: 0, output: 0, cost: 0 };
        const g = map[key];
        g.requests += rec.requests;
        g.input += rec.input;
        g.read += rec.read;
        g.write += rec.write;
        g.output += rec.output;
        g.cost += rec.cost;
      }
    }
    return Object.keys(map).sort().map((key) => {
      const g = map[key];
      const denom = g.read + g.input;
      g.hitRate = denom > 0 ? g.read / denom : 0;
      g.minute = key; // YYYY-MM-DDTHH:MM
      return g;
    });
  }

  /** 从今天 jsonl 按分钟聚合统计，并按模型分组 */
  _aggregateTodayMinuteByModel(dateStr, service) {
    const jsonlPath = path.join(this.dir, `${dateStr}.jsonl`);
    let raw;
    try { raw = fs.readFileSync(jsonlPath, "utf-8"); } catch (_) { return {}; }
    const byModel = {}; // model -> { minuteKey: agg }
    for (const line of raw.split("\n")) {
      const t = line.trim();
      if (!t) continue;
      let rec;
      try { rec = JSON.parse(t); } catch (_) { continue; }
      if (service) {
        const legacy = !rec.service && service === primaryService();
        if (rec.service !== service && !legacy) continue;
      }
      const ts = String(rec.timestamp || "");
      if (ts.length < 16) continue;
      const key = ts.slice(0, 16); // YYYY-MM-DDTHH:MM
      const model = rec.model || "unknown";
      if (!byModel[model]) byModel[model] = {};
      const m = byModel[model];
      if (!m[key]) m[key] = { requests: 0, input: 0, read: 0, write: 0, output: 0, cost: 0 };
      const g = m[key];
      g.requests++;
      g.input += rec.input_tokens || 0;
      g.read += rec.cache_read_tokens || 0;
      g.write += rec.cache_write_tokens || 0;
      g.output += rec.output_tokens || 0;
      g.cost += rec.cost || 0;
    }
    const result = {};
    for (const model of Object.keys(byModel)) {
      result[model] = Object.keys(byModel[model]).sort().map((key) => {
        const g = byModel[model][key];
        const denom = g.read + g.input;
        g.hitRate = denom > 0 ? g.read / denom : 0;
        g.minute = key;
        return g;
      });
    }
    return result;
  }

  /** 聚合单个 jsonl 文件为按模型统计 */
  _sumByModel(jsonlPath, service) {
    let raw;
    try { raw = fs.readFileSync(jsonlPath, "utf-8"); } catch (_) { return []; }
    const map = {};
    for (const line of raw.split("\n")) {
      const t = line.trim();
      if (!t) continue;
      let rec;
      try { rec = JSON.parse(t); } catch (_) { continue; }
      if (service) {
        const legacy = !rec.service && service === primaryService();
        if (rec.service !== service && !legacy) continue;
      }
      const m = rec.model || "unknown";
      if (!map[m]) map[m] = { model: m, requests: 0, input: 0, read: 0, write: 0, output: 0, cost: 0 };
      const g = map[m];
      g.requests++;
      g.input += rec.input_tokens || 0;
      g.read += rec.cache_read_tokens || 0;
      g.write += rec.cache_write_tokens || 0;
      g.output += rec.output_tokens || 0;
      // 如果记录有 cost 字段则使用，否则用 fallback 计算（强制使用第一档价格）
      if (rec.cost !== undefined) {
        g.cost += rec.cost;
      } else {
        // 旧数据默认在 200k 上下文内，使用第一档价格
        g.cost += calcCostFallback(m, rec.input_tokens || 0, rec.cache_read_tokens || 0, rec.cache_write_tokens || 0, rec.output_tokens || 0, 1, true);
      }
    }
    return this._finalizeByModel(map);
  }

  /** 从今天 jsonl 按模型聚合统计 */
  _aggregateByModel(dateStr, service) {
    return this._sumByModel(path.join(this.dir, `${dateStr}.jsonl`), service);
  }

  /** 从本月所有 jsonl 按模型聚合逐日曲线（用于图表按模型切换） */
  _aggregateMonthDailyByModel(monthPrefix, service) {
    const now = new Date();
    const daysInMonth = new Date(now.getFullYear(), now.getMonth() + 1, 0).getDate();
    const upTo = Math.min(now.getDate(), daysInMonth);
    const pad = (n) => String(n).padStart(2, "0");
    const byModel = {}; // model -> { date: agg }
    const files = this.listDayFiles()
      .map((f) => f.replace(/\.json$/, ""))
      .filter((d) => d.startsWith(monthPrefix));
    for (const ds of files) {
      for (const g of this._sumByModel(path.join(this.dir, `${ds}.jsonl`), service)) {
        if (!byModel[g.model]) byModel[g.model] = {};
        byModel[g.model][ds] = { date: ds, requests: g.requests, input: g.input, read: g.read, write: g.write, output: g.output, cost: g.cost, hitRate: g.hitRate };
      }
    }
    const result = {};
    const z = () => ({ requests: 0, input: 0, read: 0, write: 0, output: 0, cost: 0, hitRate: 0 });
    for (const model of Object.keys(byModel)) {
      const arr = [];
      for (let d = 1; d <= upTo; d++) {
        const ds = `${monthPrefix}-${pad(d)}`;
        const g = byModel[model][ds];
        arr.push(g ? { ...g } : { date: ds, ...z() });
      }
      result[model] = arr;
    }
    return result;
  }

  /** 从本月所有 jsonl 按模型聚合统计 */
  _aggregateMonthByModel(monthPrefix, service) {
    const files = this.listDayFiles()
      .map((f) => f.replace(/\.json$/, ""))
      .filter((d) => d.startsWith(monthPrefix));
    const map = {};
    for (const ds of files) {
      for (const g of this._sumByModel(path.join(this.dir, `${ds}.jsonl`), service)) {
        if (!map[g.model]) map[g.model] = { model: g.model, requests: 0, input: 0, read: 0, write: 0, output: 0, cost: 0 };
        const t = map[g.model];
        t.requests += g.requests;
        t.input += g.input;
        t.read += g.read;
        t.write += g.write;
        t.output += g.output;
        t.cost += g.cost;
      }
    }
    return this._finalizeByModel(map);
  }

  _finalizeByModel(map) {
    const result = Object.values(map).map((g) => {
      const denom = g.read + g.input;
      g.hitRate = denom > 0 ? g.read / denom : 0;
      return g;
    });
    result.sort((a, b) => b.cost - a.cost);
    return result;
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
