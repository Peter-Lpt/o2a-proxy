// 主题三态：深色 / 浅色 / 跟随系统。
// 切换按钮循环 dark → light → system；system 模式下监听系统深浅变化实时跟随。
// 跨窗口同步由各窗口通过 Tauri event（"o2a-theme"）转发，localStorage 仅作持久化。

export type Theme = "dark" | "light" | "system";

const KEY = "o2a-theme";

export function systemPref(): "dark" | "light" {
  return typeof window !== "undefined" && window.matchMedia?.("(prefers-color-scheme: light)").matches
    ? "light"
    : "dark";
}

export function getTheme(): Theme {
  try {
    const v = localStorage.getItem(KEY);
    if (v === "light" || v === "system") return v;
  } catch (_) {}
  return "dark";
}

/** 将三态解析为实际渲染的深/浅。 */
export function resolveTheme(t?: Theme): "dark" | "light" {
  const theme = t || getTheme();
  return theme === "system" ? systemPref() : theme;
}

export function applyTheme(t?: Theme): Theme {
  const theme = t || getTheme();
  document.documentElement.dataset.theme = resolveTheme(theme);
  return theme;
}

const ORDER: Theme[] = ["dark", "light", "system"];

export function toggleTheme(): Theme {
  const cur = getTheme();
  const next = ORDER[(ORDER.indexOf(cur) + 1) % ORDER.length];
  try {
    localStorage.setItem(KEY, next);
  } catch (_) {}
  applyTheme(next);
  return next;
}

/**
 * system 模式下跟随系统深浅变化；返回取消订阅函数。
 * onChange 在系统主题实际变化（且当前为 system 模式）时回调，用于通知其他窗口。
 */
export function watchSystemTheme(onChange?: (resolved: "dark" | "light") => void): () => void {
  const mq = window.matchMedia?.("(prefers-color-scheme: light)");
  if (!mq || typeof mq.addEventListener !== "function") return () => {};
  const handler = () => {
    if (getTheme() === "system") {
      document.documentElement.dataset.theme = resolveTheme("system");
      onChange?.(systemPref());
    }
  };
  mq.addEventListener("change", handler);
  return () => mq.removeEventListener("change", handler);
}
