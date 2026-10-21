//! Tauri 窗口、托盘与只读日志监视。单一运行状态缓存当日统计和会话，前端启动主动拉取快照。

mod config;
mod live;
mod placement;
mod tail;
mod telemetry;
mod usage;
mod winmon;

use chrono::Local;
use config::{load_config, save_config, usage_path, AppConfig, WindowPlacement};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use placement::{constrain, monitor_for_window, PhysicalRect};
use serde::Serialize;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tail::LogCursor;
use tauri::{
    AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, PhysicalPosition, PhysicalSize,
    WebviewWindow,
};
use usage::{RouteSnapshot, UsageLedger};

/// 后台增量游标和已发布快照只在这个状态内持有，避免启动与更新分叉。
struct Runtime {
    config: AppConfig,
    config_error: Option<String>,
    ledger: UsageLedger,
    cursor: LogCursor,
    live: live::LiveState,
    telemetry: telemetry::Telemetry,
    quota_refresh: Option<std::sync::mpsc::SyncSender<()>>,
    snapshot: RouteSnapshot,
}

impl Runtime {
    /// 初次流式汇总只读日志；无效配置保留可见错误，不在启动时覆盖文件。
    fn new() -> Self {
        let (config, config_error) = match load_config(&config::config_path()) {
            Ok(config) => (config, None),
            Err(error) => (AppConfig::default(), Some(error)),
        };
        let ledger = UsageLedger::new(Local::now().date_naive());
        let snapshot = ledger.snapshot(&config.magpie_host, "waiting", config_error.clone());
        let mut runtime = Self {
            config,
            config_error,
            ledger,
            cursor: LogCursor::default(),
            live: live::LiveState::default(),
            telemetry: telemetry::Telemetry::default(),
            quota_refresh: None,
            snapshot,
        };
        runtime.refresh(None);
        runtime
    }

    /// 每次只消费新增日志；相同内容不发事件，跨日和文件状态也会产生新快照。
    fn refresh(&mut self, watcher_error: Option<String>) -> Option<RouteSnapshot> {
        let path = usage_path(&self.config);
        let read_error = self.cursor.update(&path, &mut self.ledger).err();
        let mut snapshot = self.ledger.snapshot(
            &self.config.magpie_host,
            if self.cursor.missing {
                "missing"
            } else {
                "ready"
            },
            self.config_error.clone().or(read_error).or(watcher_error),
        );
        // 只有新完成记录才触发额度补刷；启动历史、活跃态和普通重绘不触发。
        if snapshot.date == self.snapshot.date && snapshot.daily.requests > self.snapshot.daily.requests {
            if let Some(sender) = &self.quota_refresh {
                let _ = sender.try_send(());
            }
        }
        if snapshot.sessions.is_empty() {
            snapshot.notes[1].text = path.display().to_string();
        }
        self.live.apply(&mut snapshot);
        self.telemetry.apply(&mut snapshot);
        snapshot.revision = self.snapshot.revision;
        if snapshot == self.snapshot {
            return None;
        }
        snapshot.revision = self.snapshot.revision + 1;
        self.snapshot = snapshot.clone();
        Some(snapshot)
    }
}

/// 启动结果包含当前快照及原生置顶状态，订阅后拉取以消除首次事件丢失。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StartupState {
    snapshot: RouteSnapshot,
    pinned: bool,
}

/// 运行独立桌面应用；不修改 Magpie，不开启 HTTP 接口。
pub fn run() {
    tauri::Builder::default()
        .manage(Arc::new(Mutex::new(Runtime::new())))
        .setup(|app| {
            setup_window(app.handle())?;
            setup_tray(app.handle())?;
            start_watcher(app.handle().clone());
            start_live(app.handle().clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![startup_state, set_pinned])
        .run(tauri::generate_context!())
        .expect("MagpieHUD 启动失败");
}

/// 恢复用户保存的横竖尺寸，按实际工作区限制整窗，不强制回宽模式。
fn setup_window(app: &AppHandle) -> tauri::Result<()> {
    let config = app
        .state::<Arc<Mutex<Runtime>>>()
        .lock()
        .expect("状态锁损坏")
        .config
        .clone();
    let window = app.get_webview_window("main").expect("缺少 main 窗口");
    window.set_always_on_top(config.always_on_top)?;
    if let Some(mut placement) = config.window {
        placement.width = placement.width.max(360.0);
        placement.height = placement.height.max(280.0);
        window.set_position(LogicalPosition::new(placement.x, placement.y))?;
        window.set_size(LogicalSize::new(placement.width, placement.height))?;
    }
    constrain_current(&window)?;
    window.show()?;
    let handle = app.clone();
    window.on_window_event(move |event| {
        if matches!(event, tauri::WindowEvent::CloseRequested { .. }) {
            let _ = persist_window(&handle);
        }
    });
    Ok(())
}

/// 托盘提供显示/隐藏/退出，显示时重新核对工作区。
fn setup_tray(app: &AppHandle) -> tauri::Result<()> {
    let show = tauri::menu::MenuItem::with_id(app, "show", "显示窗口", true, None::<&str>)?;
    let hide = tauri::menu::MenuItem::with_id(app, "hide", "隐藏窗口", true, None::<&str>)?;
    let separator = tauri::menu::PredefinedMenuItem::separator(app)?;
    let exit = tauri::menu::MenuItem::with_id(app, "exit", "退出", true, None::<&str>)?;
    let menu = tauri::menu::Menu::with_items(app, &[&show, &hide, &separator, &exit])?;
    tauri::tray::TrayIconBuilder::with_id("main")
        .tooltip("Magpie HUD")
        .menu(&menu)
        .icon(app.default_window_icon().expect("缺少应用图标").clone())
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = constrain_current(&window);
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            }
            "hide" => {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.hide();
                }
            }
            "exit" => {
                let _ = persist_window(app);
                app.exit(0);
            }
            _ => {}
        })
        .build(app)?;
    Ok(())
}

/// 文件事件在 100ms 内合并，最多等待 150ms；一秒兜底检查负责跨日与缺目录恢复。
fn start_watcher(app: AppHandle) {
    let shared = Arc::clone(app.state::<Arc<Mutex<Runtime>>>().inner());
    let path = usage_path(&shared.lock().expect("状态锁损坏").config);
    std::thread::spawn(move || {
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut watch_error = None;
        let _watcher = match RecommendedWatcher::new(sender, notify::Config::default()) {
            Ok(mut watcher) => {
                // 目录尚不存在时监视最近存在的上级；绝不为了监视创建 Magpie 目录。
                let parent = path.parent().expect("usage 路径缺少目录");
                let ancestor = parent.ancestors().find(|candidate| candidate.is_dir());
                if let Some(ancestor) = ancestor {
                    if let Err(error) = watcher.watch(
                        ancestor,
                        if ancestor == parent {
                            RecursiveMode::NonRecursive
                        } else {
                            RecursiveMode::Recursive
                        },
                    ) {
                        watch_error = Some(format!("文件监视不可用：{error}"));
                    }
                }
                Some(watcher)
            }
            Err(error) => {
                watch_error = Some(format!("文件监视不可用：{error}"));
                None
            }
        };
        loop {
            match receiver.recv_timeout(Duration::from_secs(1)) {
                Ok(Ok(event)) => {
                    if !event
                        .paths
                        .iter()
                        .any(|changed| changed == &path || path.starts_with(changed))
                    {
                        continue;
                    }
                    let deadline = Instant::now() + Duration::from_millis(150);
                    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
                        if receiver
                            .recv_timeout(left.min(Duration::from_millis(100)))
                            .is_err()
                        {
                            break;
                        }
                    }
                }
                Ok(Err(error)) => watch_error = Some(format!("文件监视异常：{error}")),
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    std::thread::sleep(Duration::from_secs(1))
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            }
            let next = shared
                .lock()
                .expect("状态锁损坏")
                .refresh(watch_error.clone());
            if let Some(snapshot) = next {
                let _ = app.emit("route-update", snapshot);
            }
        }
    });
}

/// 路由长轮询与活跃请求分开读取，等待路由变化不阻塞活跃结束检测。
fn start_live(app: AppHandle) {
    let shared = Arc::clone(app.state::<Arc<Mutex<Runtime>>>().inner());
    let host = shared.lock().expect("状态锁损坏").config.magpie_host.clone();
    let base = match live::gateway_url(&host) {
        Ok(base) => base,
        Err(error) => {
            shared.lock().expect("状态锁损坏").live.error = Some(error);
            return;
        }
    };
    let (quota_sender, quota_receiver) = std::sync::mpsc::sync_channel(1);
    shared.lock().expect("状态锁损坏").quota_refresh = Some(quota_sender);
    let trace_app = app.clone();
    let trace_shared = Arc::clone(&shared);
    let trace_base = base.clone();
    std::thread::spawn(move || {
        let agent = live::client(Duration::from_secs(30));
        loop {
            let seq = trace_shared.lock().expect("状态锁损坏").live.seq;
            let url = format!("{trace_base}/v1/magpie/trace?after={seq}{}", if seq > 0 { "&wait=1" } else { "" });
            match live::read_json::<live::TraceUpdate>(&agent, &url) {
                Ok(update) => {
                    let mut runtime = trace_shared.lock().expect("状态锁损坏");
                    runtime.live.update(update);
                    if let Some(snapshot) = runtime.refresh(None) {
                        let _ = trace_app.emit("route-update", snapshot);
                    }
                }
                Err(_) => std::thread::sleep(Duration::from_secs(2)),
            }
        }
    });
    let quota_app = app.clone();
    let quota_shared = Arc::clone(&shared);
    let quota_base = base.clone();
    std::thread::spawn(move || {
        let agent = live::client(Duration::from_secs(5));
        loop {
            let started = Instant::now();
            let quotas = live::read_json::<telemetry::Quotas>(&agent, &format!("{quota_base}/v1/magpie/quotas?kept=1"));
            {
                let mut runtime = quota_shared.lock().expect("状态锁损坏");
                match quotas {
                    Ok(quotas) => runtime.telemetry.update_quotas(quotas.data),
                    Err(error) => runtime.telemetry.quota_error = Some(error),
                }
                if let Some(snapshot) = runtime.refresh(None) {
                    let _ = quota_app.emit("route-update", snapshot);
                }
            }
            // 启动立即读取，之后每分钟刷新；完成事件合并两秒后补读缓存。
            match quota_receiver.recv_timeout(Duration::from_secs(60).saturating_sub(started.elapsed())) {
                Ok(()) => {
                    std::thread::sleep(Duration::from_secs(2));
                    while quota_receiver.try_recv().is_ok() {}
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
            }
        }
    });
    let usage_app = app.clone();
    let usage_shared = Arc::clone(&shared);
    let usage_base = base.clone();
    std::thread::spawn(move || {
        let agent = live::client(Duration::from_secs(5));
        loop {
            let started = Instant::now();
            let usage = live::read_json::<telemetry::Usage>(&agent, &format!("{usage_base}/v1/magpie/usage?period=today"));
            {
                let mut runtime = usage_shared.lock().expect("状态锁损坏");
                match usage {
                    Ok(usage) => { runtime.telemetry.usage = Some(usage); runtime.telemetry.cost_error = None; }
                    Err(error) => runtime.telemetry.cost_error = Some(error),
                }
                if let Some(snapshot) = runtime.refresh(None) {
                    let _ = usage_app.emit("route-update", snapshot);
                }
            }
            std::thread::sleep(Duration::from_secs(5).saturating_sub(started.elapsed()));
        }
    });
    std::thread::spawn(move || {
        let agent = live::client(Duration::from_secs(2));
        let mut titles = live::Titles::default();
        let mut names = std::collections::HashMap::new();
        let mut last_titles = Instant::now() - Duration::from_secs(3);
        loop {
            if last_titles.elapsed() >= Duration::from_secs(2) {
                names = titles.scan(&live::desktop_roots());
                last_titles = Instant::now();
            }
            let result = live::read_json::<live::Activity>(&agent, &format!("{base}/v1/magpie/concurrency"));
            {
                let mut runtime = shared.lock().expect("状态锁损坏");
                runtime.live.names = names.clone();
                match result {
                    Ok(activity) => { runtime.live.activity = activity; runtime.live.error = None; }
                    Err(error) => { runtime.live.activity = live::Activity::default(); runtime.live.error = Some(error); }
                }
                if let Some(snapshot) = runtime.refresh(None) {
                    let _ = app.emit("route-update", snapshot);
                }
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    });
}

/// 提供已缓存的快照，避免页面每次切会话都重读日志。
#[tauri::command]
fn startup_state(app: AppHandle) -> StartupState {
    let snapshot = app
        .state::<Arc<Mutex<Runtime>>>()
        .lock()
        .expect("状态锁损坏")
        .snapshot
        .clone();
    let pinned = app
        .get_webview_window("main")
        .expect("缺少窗口")
        .is_always_on_top()
        .unwrap_or(true);
    StartupState { snapshot, pinned }
}

/// 切换真实置顶并保存，失败返回错误，不让图标假装成功。
#[tauri::command]
fn set_pinned(app: AppHandle, pinned: bool) -> Result<bool, String> {
    let window = app.get_webview_window("main").ok_or("窗口不存在")?;
    window
        .set_always_on_top(pinned)
        .map_err(|error| error.to_string())?;
    let state = app.state::<Arc<Mutex<Runtime>>>();
    let mut runtime = state.lock().expect("状态锁损坏");
    runtime.config.always_on_top = pinned;
    save_config(&config::config_path(), &runtime.config)?;
    Ok(pinned)
}

/// 正常退出保存逻辑坐标，不保存日志、账号或会话内容。
fn persist_window(app: &AppHandle) -> tauri::Result<()> {
    let window = app.get_webview_window("main").expect("缺少窗口");
    let position = window.outer_position()?;
    let size = window.outer_size()?;
    let scale = window.scale_factor()?;
    let state = app.state::<Arc<Mutex<Runtime>>>();
    let mut runtime = state.lock().expect("状态锁损坏");
    runtime.config.window = Some(WindowPlacement {
        x: position.x as f64 / scale,
        y: position.y as f64 / scale,
        width: size.width as f64 / scale,
        height: size.height as f64 / scale,
    });
    if let Err(error) = save_config(&config::config_path(), &runtime.config) {
        let _ = app.emit("config-error", error);
    }
    Ok(())
}

/// 启动和托盘恢复时按实际工作区限制整窗。
fn constrain_current(window: &WebviewWindow) -> tauri::Result<()> {
    let position = window.outer_position()?;
    let size = window.outer_size()?;
    let current = PhysicalRect {
        x: position.x,
        y: position.y,
        width: size.width,
        height: size.height,
    };
    let Some(work) = monitor_for_window(current, &winmon::work_areas()) else {
        return Ok(());
    };
    let next = constrain(current, work);
    // 工作区不足时降低最小尺寸，正常桌面仍保留可操作的缩放下限。
    let scale = window.scale_factor()?;
    window.set_min_size(Some(PhysicalSize::new(
        ((360.0 * scale) as u32).min(work.width),
        ((280.0 * scale) as u32).min(work.height),
    )))?;
    window.set_size(PhysicalSize::new(next.width, next.height))?;
    window.set_position(PhysicalPosition::new(next.x, next.y))
}
