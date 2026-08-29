/**
 * 全局 UI 状态（§10.1 stores/config.ts 单一数据源方向的 UI 部分）。
 *
 * toast（含动作按钮/手动关闭，§10.2-5）与确认弹层（confirmBox）在此集中管理，
 * 供 PanelApp 与拆分后的各视图组件共享同一实例。
 */
import { ref } from "vue";

// ---------- Toast ----------
export const toast = ref("");
export const toastType = ref<"info" | "success" | "error">("info");
export const toastAction = ref<{ label: string; fn: () => void } | null>(null);

let toastTimer: ReturnType<typeof setTimeout> | null = null;

export function showToast(
  msg: string,
  type: "info" | "success" | "error" = "info",
  action?: { label: string; fn: () => void }
) {
  toast.value = msg;
  toastType.value = type;
  toastAction.value = action || null;
  if (toastTimer) clearTimeout(toastTimer);
  // 带动作（如「撤销」）的 toast 时长更长，给用户反应时间
  const ttl = action ? 5000 : type === "error" ? 4200 : 2200;
  toastTimer = setTimeout(() => {
    toast.value = "";
    toastAction.value = null;
  }, ttl);
}

export function onToastAction() {
  const act = toastAction.value;
  dismissToast();
  act?.fn();
}

export function dismissToast() {
  if (toastTimer) clearTimeout(toastTimer);
  toast.value = "";
  toastAction.value = null;
}

// ---------- 确认弹层 ----------
export const confirmBox = ref<{
  title: string;
  message: string;
  okText?: string;
  action: () => void;
} | null>(null);

export function askConfirm(
  title: string,
  message: string,
  action: () => void,
  okText = "确认"
) {
  confirmBox.value = { title, message, action, okText };
}

export function onConfirmOk() {
  const cb = confirmBox.value;
  confirmBox.value = null;
  cb?.action();
}
