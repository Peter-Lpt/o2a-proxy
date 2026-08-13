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
