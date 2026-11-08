//! One active data source. Both the local service and remote HTTP/SSE client use
//! the same DTOs; source generations reject late events and query completions.
use crate::{
    config::{self, AppConfig},
    connection,
    remote::{self, RemoteClient},
    WindowSave,
};
use hud_core::{
    service::{Service, ServiceConfig},
    usage::{RouteSnapshot, UsageLedger},
};
use serde_json::{json, Value};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tauri::{AppHandle, Emitter, Manager};

type Shared = Arc<Mutex<Runtime>>;
pub(crate) struct Runtime {
    pub config: AppConfig,
    pub config_error: Option<String>,
    pub snapshot: RouteSnapshot,
    pub timezone: String,
    pub server_clock_offset_ms: i64,
    pub window_save: Arc<WindowSave>,
    pub details_save: Arc<WindowSave>,
    local: Option<Arc<Service>>,
    remote: Option<Arc<RemoteClient>>,
    stop: Arc<AtomicBool>,
    generation: u64,
    epoch: u64,
    remote_stamp: Option<(String, String, u64)>,
}
impl Runtime {
    pub fn new() -> Self {
        let (config, config_error) = match config::load_config(&config::config_path()) {
            Ok(value) => (value, None),
            Err(error) => (AppConfig::default(), Some(error)),
        };
        let snapshot = empty_snapshot(&config, config_error.clone());
        Self {
            window_save: Arc::new(WindowSave::new(config.window.filter(crate::stable_window))),
            details_save: Arc::new(WindowSave::new(
                config.details_window.filter(crate::stable_window),
            )),
            config,
            config_error,
            snapshot,
            timezone: hud_core::paths::local_timezone(),
            server_clock_offset_ms: 0,
            local: None,
            remote: None,
            stop: Arc::new(AtomicBool::new(false)),
            generation: 0,
            epoch: 0,
            remote_stamp: None,
        }
    }
}
fn empty_snapshot(config: &AppConfig, error: Option<String>) -> RouteSnapshot {
    let host = if config.connection.mode == "remote" {
        &config.connection.remote_url
    } else {
        &config.magpie_host
    };
    let mut snapshot =
        UsageLedger::new(chrono::Local::now().date_naive()).snapshot(host, "waiting", error);
    if config.connection.mode == "remote" {
        snapshot.watch_label = "正在连接远端 HUD".into();
    }
    snapshot
}
fn lock(shared: &Shared) -> std::sync::MutexGuard<'_, Runtime> {
    shared.lock().unwrap_or_else(|e| e.into_inner())
}
fn shared(app: &AppHandle) -> Shared {
    app.state::<Shared>().inner().clone()
}

pub(crate) fn start_source(app: AppHandle) {
    let state = shared(&app);
    let (config, generation, stop, previous) = {
        let mut runtime = lock(&state);
        runtime.stop.store(true, Ordering::Release);
        let previous = runtime.local.take();
        runtime.remote = None;
        runtime.generation += 1;
        runtime.epoch += 1;
        runtime.remote_stamp = None;
        runtime.stop = Arc::new(AtomicBool::new(false));
        runtime.snapshot = empty_snapshot(&runtime.config, runtime.config_error.clone());
        runtime.timezone = if runtime.config.connection.mode == "remote" { String::new() } else { hud_core::paths::local_timezone() };
        runtime.server_clock_offset_ms = 0;
        let _ = app.emit("source-change", json!({"mode":runtime.config.connection.mode,"remoteUrl":runtime.config.connection.remote_url,"generation":runtime.generation,"timezone":runtime.timezone}));
        let _ = app.emit("route-update", &runtime.snapshot);
        (
            runtime.config.clone(),
            runtime.generation,
            runtime.stop.clone(),
            previous,
        )
    };
    if let Some(service) = previous {
        std::thread::spawn(move || service.stop());
    }
    *app.state::<Mutex<hud_core::details::DetailContext>>()
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = Default::default();
    std::thread::spawn(move || {
        if config.connection.mode == "remote" {
            let client = match connection::client(&config.connection, None) {
                Ok(client) => client,
                Err(error) => {
                    source_error(&app, generation, &error);
                    return;
                }
            };
            // A saved server may be offline at startup. Keep retrying, without
            // reading the local gateway or replacing it with demo data.
            while !stop.load(Ordering::Acquire) {
                match client.capabilities() {
                    Ok(capabilities) => {
                        let mut runtime = lock(&state);
                        if runtime.generation != generation {
                            return;
                        }
                        runtime.remote = Some(client.clone());
                        runtime.timezone = capabilities["timezone"].as_str().unwrap_or("UTC").to_owned();
                        note_server_clock(&mut runtime, &capabilities);
                        let _ = app.emit("connection-status", json!({"connected":true,"timezone":capabilities["timezone"],"serverNow":capabilities["serverNow"],"collectors":capabilities["collectors"]}));
                        break;
                    }
                    Err(error) => {
                        source_error(&app, generation, &error);
                        if !pause(&stop, 20) {
                            return;
                        }
                    }
                }
            }
            if stop.load(Ordering::Acquire) {
                return;
            }
            let incoming = app.clone();
            let disconnected = app.clone();
            client.subscribe(
                stop,
                move |value| accept_remote(&incoming, generation, value),
                move || source_error(&disconnected, generation, "远端 HUD 已断开，显示最后数据"),
            );
        } else {
            let cache = config::config_path()
                .parent()
                .unwrap_or(std::path::Path::new("."))
                .to_path_buf();
            let service = Service::start(ServiceConfig {
                usage_path: config::usage_path(&config),
                magpie_host: config.magpie_host,
                cache_dir: cache,
                include_unassociated: false,
            });
            {
                let mut runtime = lock(&state);
                if runtime.generation != generation {
                    drop(runtime);
                    service.stop();
                    return;
                }
                runtime.local = Some(service.clone());
            }
            let mut previous = None;
            while !stop.load(Ordering::Acquire) {
                if let Some(current) = service.snapshot_if_changed(previous) {
                    previous = Some(current.revision);
                    let mut snapshot = (*current).clone();
                    let mut runtime = lock(&state);
                    if runtime.generation != generation {
                        break;
                    }
                    snapshot.revision = runtime.snapshot.revision + 1;
                    if runtime.config_error.is_some() {
                        snapshot.error = runtime.config_error.clone();
                    }
                    runtime.snapshot = snapshot;
                    let _ = app.emit("route-update", &runtime.snapshot);
                }
                if !pause(&stop, 2) {
                    break;
                }
            }
            service.stop();
        }
    });
}
fn pause(stop: &AtomicBool, ticks: usize) -> bool {
    for _ in 0..ticks {
        if stop.load(Ordering::Acquire) {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    !stop.load(Ordering::Acquire)
}
fn source_error(app: &AppHandle, generation: u64, error: &str) {
    let state = shared(app);
    let mut runtime = lock(&state);
    if runtime.generation != generation {
        return;
    }
    runtime.snapshot.revision += 1;
    runtime.snapshot.state = "disconnected".into();
    runtime.snapshot.watch_label = "远端已断开 · 最后数据".into();
    runtime.snapshot.error = Some(error.into());
    for session in &mut runtime.snapshot.sessions {
        for card in session
            .routes
            .iter_mut()
            .chain(session.subagents.iter_mut())
        {
            card.current = false;
            if card.state == "running" || card.state == "active" || card.state == "pending" {
                card.state = "unknown".into();
                card.detail = "连接已断开，状态待确认".into();
            }
        }
    }
    let _ = app.emit(
        "connection-status",
        json!({"connected":false,"error":error}),
    );
    let _ = app.emit("route-update", &runtime.snapshot);
}
fn note_server_clock(runtime: &mut Runtime, value: &Value) {
    if let Some(stamp) = value["serverNow"].as_str()
        .or_else(|| value["collectedAt"].as_str())
        .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
    {
        runtime.server_clock_offset_ms = stamp.timestamp_millis() - chrono::Utc::now().timestamp_millis();
    }
}

fn accept_remote(app: &AppHandle, generation: u64, value: Value) {
    let Some(instance) = value["instanceId"].as_str() else {
        return;
    };
    let Some(boot) = value["bootId"].as_str() else {
        return;
    };
    let Some(revision) = value["revision"].as_u64() else {
        return;
    };
    let Ok(mut snapshot) = serde_json::from_value::<RouteSnapshot>(value["snapshot"].clone())
    else {
        source_error(app, generation, "远端 HUD 快照格式不兼容");
        return;
    };
    let state = shared(app);
    let mut runtime = lock(&state);
    if runtime.generation != generation {
        return;
    }
    if let Some((old_instance, old_boot, old_revision)) = &runtime.remote_stamp {
        if old_instance == instance && old_boot == boot && revision < *old_revision {
            return;
        }
    }
    let changed = runtime
        .remote_stamp
        .as_ref()
        .is_none_or(|(old_instance, old_boot, _)| old_instance != instance || old_boot != boot);
    if changed {
        runtime.epoch += 1;
        let _ = app.emit("source-change", json!({"mode":"remote","remoteUrl":runtime.config.connection.remote_url,"instanceId":instance,"bootId":boot,"generation":runtime.generation}));
    }
    runtime.remote_stamp = Some((instance.into(), boot.into(), revision));
    note_server_clock(&mut runtime, &value);
    if let Some(timezone) = value["timezone"].as_str().filter(|zone| !zone.is_empty()) {
        runtime.timezone = timezone.to_owned();
    }
    // UI revisions are local monotonic counters; the upstream revision is kept
    // separately so a restart or disconnect marker cannot suppress fresh data.
    snapshot.revision = runtime.snapshot.revision + 1;
    runtime.snapshot = snapshot;
    let _ = app.emit(
        "connection-status",
        json!({"connected":true,"collectedAt":value["collectedAt"],"timezone":runtime.timezone,"serverNow":value["serverNow"]}),
    );
    let _ = app.emit("route-update", &runtime.snapshot);
}

pub(crate) fn clear_local_detail_cache(app: &AppHandle) {
    let service = lock(&shared(app)).local.clone();
    if let Some(service) = service {
        std::thread::spawn(move || { let _ = service.clear_detail_cache(); });
    }
}

pub(crate) async fn query(
    app: AppHandle,
    command: &'static str,
    args: Value,
) -> Result<Value, String> {
    let state = shared(&app);
    let (generation, epoch, local, remote) = {
        let runtime = lock(&state);
        (
            runtime.generation,
            runtime.epoch,
            runtime.local.clone(),
            runtime.remote.clone(),
        )
    };
    let result = tauri::async_runtime::spawn_blocking(move || {
        if let Some(client) = remote {
            client.query(command, args)
        } else if let Some(service) = local {
            service.query(command, args)
        } else {
            Err("数据源尚未连接，请稍后重试".into())
        }
    })
    .await
    .map_err(|_| "查询任务失败".to_string())?;
    let runtime = lock(&state);
    if runtime.generation != generation || runtime.epoch != epoch {
        return Err("数据源已切换，请重新查询".into());
    }
    result
}

#[tauri::command]
pub(crate) async fn connection_settings(
    app: AppHandle,
) -> Result<connection::ConnectionView, String> {
    let config = lock(&shared(&app)).config.connection.clone();
    tauri::async_runtime::spawn_blocking(move || connection::view(&config))
        .await
        .map_err(|_| "无法读取连接设置".into())
}
#[tauri::command]
pub(crate) async fn test_connection(
    app: AppHandle,
    remote_url: String,
    key: String,
) -> Result<Value, String> {
    let reference = lock(&shared(&app))
        .config
        .connection
        .credential_ref
        .clone()
        .unwrap_or_else(connection::credential_reference);
    tauri::async_runtime::spawn_blocking(move || {
        connection::test(&remote_url, Some(&key), reference)
    })
    .await
    .map_err(|_| "连接测试任务失败".to_string())?
}
#[tauri::command]
pub(crate) async fn save_connection(
    app: AppHandle,
    mode: String,
    remote_url: String,
    key: String,
) -> Result<connection::ConnectionView, String> {
    if mode != "local" && mode != "remote" {
        return Err("未知数据源模式".into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        let state = shared(&app);
        // Serialize settings with window persistence. Network work happens first,
        // and the config is copied again afterwards to retain any moved window.
        let reference = lock(&state)
            .config
            .connection
            .credential_ref
            .clone()
            .unwrap_or_else(connection::credential_reference);
        let prepared = if mode == "remote" {
            let candidate = remote::ConnectionConfig {
                mode: mode.clone(),
                remote_url: remote_url.trim().into(),
                credential_ref: Some(reference.clone()),
            };
            connection::client(&candidate, Some(&key))?.capabilities()?;
            Some(candidate)
        } else {
            None
        };
        let mut runtime = lock(&state);
        if let Some(error) = &runtime.config_error {
            return Err(error.clone());
        }
        let mut config = runtime.config.clone();
        let previous_key = remote::load_key(&reference).ok();
        let key_changed = mode == "remote" && !key.trim().is_empty();
        if let Some(candidate) = prepared {
            if key_changed {
                remote::save_key(&reference, key.trim())?;
            }
            config.connection = candidate;
        } else {
            config.connection.mode = "local".into();
        }
        if let Err(error) = config::save_config(&config::config_path(), &config) {
            if key_changed {
                if let Some(old) = previous_key {
                    let _ = remote::save_key(&reference, &old);
                } else {
                    let _ = remote::forget_key(&reference);
                }
            }
            return Err(error);
        }
        runtime.config = config;
        let connection = runtime.config.connection.clone();
        drop(runtime);
        start_source(app);
        Ok(connection::view(&connection))
    })
    .await
    .map_err(|_| "保存连接任务失败".to_string())?
}
#[tauri::command]
pub(crate) async fn forget_connection(
    app: AppHandle,
) -> Result<connection::ConnectionView, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = shared(&app);
        let mut runtime = lock(&state);
        if let Some(error) = &runtime.config_error {
            return Err(error.clone());
        }
        let mut config = runtime.config.clone();
        let reference = config
            .connection
            .credential_ref
            .clone()
            .unwrap_or_else(|| connection::credential_reference());
        let previous_key = remote::load_key(&reference).ok();
        remote::forget_key(&reference)?;
        config.connection.mode = "local".into();
        config.connection.credential_ref = None;
        if let Err(error) = config::save_config(&config::config_path(), &config) {
            if let Some(old) = previous_key {
                let _ = remote::save_key(&reference, &old);
            }
            return Err(error);
        }
        runtime.config = config;
        let connection = runtime.config.connection.clone();
        drop(runtime);
        start_source(app);
        Ok(connection::view(&connection))
    })
    .await
    .map_err(|_| "删除连接任务失败".to_string())?
}

pub(crate) fn stop_source(app: &AppHandle) {
    let state = shared(app);
    let previous = {
        let mut runtime = lock(&state);
        runtime.stop.store(true, Ordering::Release);
        runtime.generation += 1;
        runtime.remote = None;
        runtime.local.take()
    };
    if let Some(service) = previous {
        std::thread::spawn(move || service.stop());
    }
}
