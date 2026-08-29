/**
 * 模型列表缓存子系统（§4 / §10.1 useModels）。
 *
 * 服务页主模型下拉、白名单多选器与账号页连通性测试共用同一份缓存：
 * key = baseUrl + "\n" + apiKey（同端点不同 Key 不共用，避免跨账号污染），
 * TTL 10 分钟 + stale-while-revalidate + inflight 并发去重。
 * 模块级单例：多组件共享同一 Map 与请求序号。
 */
import { ref } from "vue";
import { api } from "../api";

export const MODEL_CACHE_TTL = 10 * 60 * 1000;

export const modelHint = ref<{ text: string; err: boolean }>({ text: "", err: false });
export const modelRefreshing = ref(false);

export interface ModelCacheEntry {
  models: string[];
  fetchedAt: number;
  inflight?: Promise<string[] | null> | null;
}
export const modelCache = new Map<string, ModelCacheEntry>();

export function cacheKey(baseUrl: string, apiKey: string): string {
  return `${baseUrl}\n${apiKey}`;
}

export function fmtModelTime(ts: number): string {
  const d = new Date(ts);
  const p = (n: number) => String(n).padStart(2, "0");
  return `${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`;
}

export function setModelHint(text: string, err = false) {
  modelHint.value = { text, err };
}

let modelFetchCount = 0;

export async function performFetchModels(baseUrl: string, apiKey: string): Promise<string[] | null> {
  const key = cacheKey(baseUrl, apiKey);
  const existing = modelCache.get(key);
  if (existing?.inflight) return existing.inflight;
  if (!apiKey) return existing?.models ?? null;
  modelFetchCount++;
  modelRefreshing.value = true;
  const entry = existing || { models: [], fetchedAt: 0 };
  const promise = (async () => {
    try {
      const res = await api.fetchModels(baseUrl, apiKey);
      if (res.ok) {
        modelCache.set(key, { models: res.models, fetchedAt: Date.now() });
        return res.models;
      }
      return entry.models || null;
    } catch (e: any) {
      return entry.models || null;
    } finally {
      const cur = modelCache.get(key);
      if (cur) cur.inflight = null;
      modelFetchCount = Math.max(0, modelFetchCount - 1);
      if (modelFetchCount === 0) modelRefreshing.value = false;
    }
  })();
  entry.inflight = promise;
  modelCache.set(key, entry);
  return promise;
}
