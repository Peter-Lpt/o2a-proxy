import { invoke } from "@tauri-apps/api/core";
import { fmtCost, fmtNum, fmtPct } from "./format";

export { fmtCost, fmtNum, fmtPct };

export const api = {
  resolveRoot: () => invoke<string>("resolve_root"),
  resolvePython: () => invoke<string>("resolve_python"),
  getConfig: () => invoke<any>("read_config"),
  saveConfig: (cfg: any) => invoke<void>("save_config", { cfg }),
  getStatus: () => invoke<any>("get_status"),
  isPortOpen: (host: string, port: number) => invoke<boolean>("is_port_open", { host, port }),
  getQuota: (account: string) => invoke<any>("get_quota", { account }),
  startService: (name: string) => invoke<void>("start_service", { name }),
  stopService: (name: string) => invoke<void>("stop_service", { name }),
  toggleService: (name: string) => invoke<void>("toggle_service", { name }),
  startAll: () => invoke<void>("start_all"),
  stopAll: () => invoke<void>("stop_all"),
  getStats: (service: string, range = "today", start?: string, end?: string, model?: string) =>
    invoke<any>("get_stats", { service, range, start, end, model }),
  getDaily: (service: string, start: string, end: string) =>
    invoke<any>("get_daily", { service, start, end }),
  getLive: (service: string) => invoke<any>("get_live", { service }),
  fetchModels: (baseUrl: string, apiKey: string) =>
    invoke<any>("fetch_models", { baseUrl, apiKey }),
  openConfigFile: () => invoke<void>("open_config_file"),
  getConfigLocation: () => invoke<any>("get_config_location"),
  setConfigLocation: (path: string) => invoke<any>("set_config_location", { path }),
  toggleFloat: () => invoke<boolean>("toggle_float"),
  toggleFloatFor: (service: string) => invoke<boolean>("toggle_float_for", { service }),
  togglePanel: () => invoke<boolean>("toggle_panel"),
  hidePanel: () => invoke<void>("hide_panel"),
  setFloatSize: (width: number, height: number) =>
    invoke<void>("set_float_size", { width, height }),
  quitApp: () => invoke<void>("quit_app"),
};
