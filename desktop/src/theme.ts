export type Theme = "dark" | "light";

const KEY = "o2a-theme";

export function getTheme(): Theme {
  const v = localStorage.getItem(KEY);
  return v === "light" ? "light" : "dark";
}

export function applyTheme(t?: Theme): Theme {
  const theme = t || getTheme();
  document.documentElement.dataset.theme = theme;
  return theme;
}

export function toggleTheme(): Theme {
  const next: Theme = getTheme() === "dark" ? "light" : "dark";
  localStorage.setItem(KEY, next);
  document.documentElement.dataset.theme = next;
  return next;
}
