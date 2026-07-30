const { contextBridge, ipcRenderer } = require("electron");

contextBridge.exposeInMainWorld("api", {
  getConfig: () => ipcRenderer.invoke("get-config"),
  saveConfig: (cfg) => ipcRenderer.invoke("save-config", cfg),
  getStats: () => ipcRenderer.invoke("get-stats"),
  getStatus: () => ipcRenderer.invoke("get-status"),
  getLive: () => ipcRenderer.invoke("get-live"),
  onLiveRecords: (cb) => {
    const listener = (_e, recs) => cb(recs);
    ipcRenderer.on("live-records", listener);
    return () => ipcRenderer.removeListener("live-records", listener);
  },
  fetchModels: (payload) => ipcRenderer.invoke("fetch-models", payload),
  toggleProxy: () => ipcRenderer.invoke("toggle-proxy"),
  startProxy: () => ipcRenderer.invoke("start-proxy"),
  stopProxy: () => ipcRenderer.invoke("stop-proxy"),
  openConfigFile: () => ipcRenderer.invoke("open-config-file"),
  hidePanel: () => ipcRenderer.invoke("hide-panel"),
  toggleFloat: (forceOpen) => ipcRenderer.invoke("toggle-float", forceOpen),
  getFloatState: () => ipcRenderer.invoke("get-float-state"),
  onFloatState: (cb) => {
    const listener = (_e, s) => cb(s);
    ipcRenderer.on("float-state", listener);
    return () => ipcRenderer.removeListener("float-state", listener);
  },
  quitApp: () => ipcRenderer.invoke("quit-app"),
  onStatus: (cb) => {
    const listener = (_e, payload) => cb(payload);
    ipcRenderer.on("status", listener);
    return () => ipcRenderer.removeListener("status", listener);
  },
  onPanelShown: (cb) => {
    const listener = () => cb();
    ipcRenderer.on("panel-shown", listener);
    return () => ipcRenderer.removeListener("panel-shown", listener);
  },
});
