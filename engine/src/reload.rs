//! 热重载（对齐 Python engine.py `diff_services` / `_do_reload` / `_reload_services`）。
//!
//! - 并发去重：RELOADING 标记已置位时直接返回
//! - 失败回滚：先起新（任一失败 → 清掉本次启动的、抛错回滚，旧 runner 未动）
//! - 提交：停旧（删除/换端口）→ 换新映射 → 原地替换 Service 对象
//! - 503 语义：RELOADING 期间（见 state::reloading）非 /health 请求一律 503 + Retry-After: 2

use std::collections::HashMap;
use std::sync::Arc;

use o2a_config::load_config_at;

use crate::state::{diff_services, matches_filter, EngineState, ReloadGuard, RunnerHandle};

/// 重载入口：并发去重 + 失败回滚（对齐 `_do_reload`，绝不留半加载状态）。
/// 同一个标志位同时承担 503 语义（Python 的 `_O2A_RELOADING` 即双职）。
/// 守卫 Drop（含 panic 展开）时自动复位，对齐 Python `finally` 语义。
pub async fn do_reload(state: Arc<EngineState>) {
    // 未在重载时置位成功；已在重载中（None）→ 并发去重直接返回
    let _guard: ReloadGuard<'_> = match state.reloading.try_begin() {
        Some(g) => g,
        None => return,
    };
    let result = reload_services(&state).await;
    if let Err(e) = result {
        tracing::error!("[reload] 失败，保持旧配置继续运行: {e:#}");
    }
    // _guard 在此 Drop：标记复位（对齐 finally）
}

async fn reload_services(state: &Arc<EngineState>) -> anyhow::Result<()> {
    let services = load_config_at(&state.config_path);
    let services: Vec<_> = services
        .into_iter()
        .filter(|s| s.enabled && s.account.valid())
        .filter(|s| match &state.filter {
            Some(f) => matches_filter(s, f),
            None => true,
        })
        .collect();
    let new: HashMap<String, o2a_config::Service> =
        services.into_iter().map(|s| (s.id.clone(), s)).collect();

    let old: HashMap<String, o2a_config::Service> = state
        .runners
        .read()
        .unwrap()
        .iter()
        .map(|(k, r)| (k.clone(), r.state.service.read().unwrap().clone()))
        .collect();

    let (start_ids, stop_ids, swap_ids) = diff_services(&old, &new);
    tracing::info!(
        "[reload] diff: start={:?} stop={:?} swap={:?}",
        start_ids,
        stop_ids,
        swap_ids
    );

    // 先起新（新端口绑定），任一失败 → 清掉本次启动的、抛错回滚（旧 runner 未动）
    let mut started: Vec<(String, RunnerHandle)> = Vec::new();
    for sid in &start_ids {
        let svc = new.get(sid).expect("diff 保证存在");
        match state.start_service(svc.clone()).await {
            Ok(h) => started.push((sid.clone(), h)),
            Err(e) => {
                for (_, h) in started {
                    h.shutdown().await;
                }
                anyhow::bail!("启动新配置失败: {e:#}");
            }
        }
    }

    // 提交：停旧（删除/换端口）→ 换新映射 → 原地替换 service 对象
    {
        let mut runners = state.runners.write().unwrap();
        for sid in &stop_ids {
            if let Some(h) = runners.remove(sid) {
                // 异步清理：不持锁跨 await（与 Python 的同步 cleanup 等价，仅时序差异）
                tokio::spawn(async move { h.shutdown().await });
            }
        }
        for (sid, h) in started {
            runners.insert(sid, h);
        }
        for sid in &swap_ids {
            if let Some(h) = runners.get(sid) {
                *h.state.service.write().unwrap() = new[sid].clone();
            }
        }
    }
    tracing::info!("[reload] 完成：运行 {} 个服务", state.runners.read().unwrap().len());
    Ok(())
}
