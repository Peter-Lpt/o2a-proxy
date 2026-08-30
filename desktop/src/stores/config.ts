/**
 * 配置/运行态单一数据源（ stores/config.ts）。
 *
 * PanelApp 与拆分出的各视图组件共享同一份响应式状态：
 * - cfg / status：config.json 与引擎状态镜像
 * - selected / selectedSvc：选中服务身份（id 优先， id 化）
 * - page：主页面签
 * - 脏状态快照（）：snapCfg / dirty
 * - 服务 id 生成（）
 */
import { computed, reactive, ref } from "vue";

export const ALL = "__all__";
export const cfg = reactive<any>({});
export const status = reactive<any>({ services: [] });
export const selected = ref<string>(ALL);
export const selectedSvc = ref<any | null>(null);
export const page = ref<"stats" | "config" | "accounts">("stats");

// ----------  服务身份 id ----------
// svc-<8 位十六进制随机>：稳定身份，生成后终生不变；comment 仅为显示名。
export function newSvcId(): string {
  const b = new Uint8Array(4);
  crypto.getRandomValues(b);
  return "svc-" + Array.from(b, (x) => x.toString(16).padStart(2, "0")).join("");
}

export function ensureSvcIds(c: any) {
  const used = new Set<string>();
  for (const s of c.services || []) {
    if (s.id && !used.has(s.id)) used.add(s.id);
  }
  for (const s of c.services || []) {
    if (!s.id || used.has(s.id)) {
      let id = newSvcId();
      while (used.has(id)) id = newSvcId();
      s.id = id;
      used.add(id);
    }
  }
}

// ----------  脏状态 ----------
// 快照用 ref 保存：snapCfg() 后即使 cfg 本身没有再次变化，dirty 也会因
// 快照依赖变化而重新计算，避免“加载/弃改后仍误报未保存”的过期脏状态。
// 初始快照直接取当前空 cfg，避免配置尚未加载时被误判为“有未保存改动”。
const cfgSnapshot = ref(JSON.stringify(cfg));
export function snapCfg() {
  cfgSnapshot.value = JSON.stringify(cfg);
}
export const dirty = computed(() => JSON.stringify(cfg) !== cfgSnapshot.value);
