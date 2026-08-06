import { invoke } from "@tauri-apps/api/core";

export const api = {
  resolveRoot: () => invoke<string>("resolve_root"),
  resolvePython: () => invoke<string>("resolve_python"),
  getConfig: () => invoke<any>("read_config"),
  saveConfig: (cfg: any) => invoke<void>("save_config", { cfg }),
  getStatus: () => invoke<any>("get_status"),
  startService: (name: string) => invoke<void>("start_service", { name }),
  stopService: (name: string) => invoke<void>("stop_service", { name }),
  toggleService: (name: string) => invoke<void>("toggle_service", { name }),
  startAll: () => invoke<void>("start_all"),
  stopAll: () => invoke<void>("stop_all"),
  getStats: (service: string) => invoke<any>("get_stats", { service }),
  getLive: (service: string) => invoke<any>("get_live", { service }),
  fetchModels: (baseUrl: string, apiKey: string) =>
    invoke<any>("fetch_models", { baseUrl, apiKey }),
  openConfigFile: () => invoke<void>("open_config_file"),
  toggleFloat: () => invoke<boolean>("toggle_float"),
  toggleFloatFor: (service: string) => invoke<boolean>("toggle_float_for", { service }),
  getFloatState: () => invoke<boolean>("get_float_state"),
  togglePanel: () => invoke<boolean>("toggle_panel"),
  hidePanel: () => invoke<void>("hide_panel"),
  quitApp: () => invoke<void>("quit_app"),
};

export function fmtNum(n: number | undefined | null): string {
  const v = Number(n || 0);
  if (v >= 1_000_000) return (v / 1_000_000).toFixed(2) + "M";
  if (v >= 10_000) return (v / 1_000).toFixed(1) + "K";
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
