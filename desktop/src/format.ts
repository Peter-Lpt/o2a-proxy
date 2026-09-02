// 统一格式化与命中率色档：面板、悬浮窗、图表共用同一口径。
// 历史问题：api.ts 与 LineChart 各自实现 fmtNum（阈值不同），
// 同一数字在不同位置显示不一致；命中率色档在三处各写一遍。

/** 数字缩写：≥1K 缩 K/M/B，1 位小数；小数字取整。 */
export function fmtNum(n: number | undefined | null): string {
  const v = Number(n || 0);
  if (v >= 1e9) return (v / 1e9).toFixed(2) + "B";
  if (v >= 1e6) return (v / 1e6).toFixed(2) + "M";
  if (v >= 1e3) return (v / 1e3).toFixed(1) + "K";
  return String(Math.round(v));
}

export function fmtPct(r: number | undefined | null): string {
  const v = Number(r || 0);
  return (v * 100).toFixed(1) + "%";
}

export function fmtCost(c: number | undefined | null): string {
  const v = Number(c || 0);
  if (v >= 100) return v.toFixed(0);
  if (v >= 1) return v.toFixed(2);
  return v.toFixed(4);
}

// ---------- 实时列表（悬浮窗 / 面板实时调用）共用口径 ----------
// 记录时间戳是引擎按本地时间写的 0 填充 ISO 串（YYYY-MM-DDTHH:mm:ss，无时区偏移），
// 字典序即时间序；不用 Date/Date.parse 解析，避免 WebView 差异与 NaN 退化。

/** 当天日期（本地时区）YYYY-MM-DD。 */
export function todayStr(d = new Date()): string {
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`;
}

/** 记录是否属于当天（跨天残留的旧记录一律判 false）。 */
export function isTodayRecord(rec: any, today: string = todayStr()): boolean {
  return String(rec?.timestamp || "").slice(0, 10) === today;
}

/** 完整时间戳倒序比较（最新在前；空时间戳排最后）。 */
export function cmpTsDesc(a: any, b: any): number {
  const sa = String(a?.timestamp || "");
  const sb = String(b?.timestamp || "");
  if (sa === sb) return 0;
  if (!sa) return 1;
  if (!sb) return -1;
  return sa > sb ? -1 : 1;
}

/**
 * 实时列表统一口径：只保留当天记录，按时间倒序（最新在前）。
 * limit > 0 时截断。后端 get_live 已按当天文件读取，这里再按 timestamp
 * 兜底过滤，防止轮询暂停/缓存残留把昨天的记录显示成今天的。
 */
export function todayLiveRecords(records: any[], limit = 0): any[] {
  const today = todayStr();
  const out = (records || [])
    .filter((r: any) => isTodayRecord(r, today))
    .sort(cmpTsDesc);
  return limit > 0 ? out.slice(0, limit) : out;
}

export type HitTier = "good" | "mid" | "bad" | "";

/**
 * 命中率色档，统一为两个口径：
 * - strict（单条记录）：≥0.6 good / >0.15 mid / 其余 bad
 * - wide（汇总口径，近 N 分钟聚合）：≥0.3 good / ≥0.1 mid / 其余无
 */
export function hitTier(rate: number | undefined | null, strict = true): HitTier {
  const v = Number(rate || 0);
  if (strict) {
    if (v >= 0.6) return "good";
    if (v > 0.15) return "mid";
    return "bad";
  }
  if (v >= 0.3) return "good";
  if (v >= 0.1) return "mid";
  return "";
}
