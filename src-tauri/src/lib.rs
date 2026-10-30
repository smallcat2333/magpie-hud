//! Tauri 窗口、托盘与只读日志监视。单一运行状态缓存当日统计和会话，前端启动主动拉取快照。

mod backdrop;
mod config;
mod details;
mod history;
mod hourly;
mod live;
mod placement;
mod tail;
mod telemetry;
mod usage;
mod winmon;

use chrono::{Datelike, Local};
use config::{load_config, save_config, usage_path, AppConfig, WindowPlacement};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use placement::{constrain, monitor_for_window, PhysicalRect};
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tail::LogCursor;
use tauri::{
    AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, PhysicalPosition, PhysicalSize,
    WebviewWindow,
};
use usage::{RouteSnapshot, UsageLedger};

/// 历史日期统计缓存项，与主账本和实时游标完全隔离。
#[derive(Clone)]
struct CachedDayStats {
    host: String,
    file_created: Option<std::time::SystemTime>,
    file_modified: Option<std::time::SystemTime>,
    file_len: u64,
    read_at: Instant,
    view: DateStatsView,
}

/// 按日统计查询结果；历史日期与实时主账本分离，不覆盖当日会话或实时游标。
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct DateStatsView {
    date: String,
    earliest_date: String,
    latest_date: String,
    daily: usage::DailyStats,
    /// 该日全部完成记录的来源分组，字段与实时快照 agentStats 相同。
    agent_stats: Vec<usage::AgentStats>,
    cost_error: Option<String>,
}

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
    history_cache: std::collections::HashMap<chrono::NaiveDate, CachedDayStats>,
    /// 合并窗口保存请求。事件回调不在持锁时等待写盘线程。
    window_save: Arc<WindowSave>,
    details_save: Arc<WindowSave>,
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
        let saved_window = config.window.filter(|placement| stable_window(placement));
        let saved_details = config.details_window.filter(|placement| stable_window(placement));
        let mut runtime = Self {
            config,
            config_error,
            ledger,
            cursor: LogCursor::default(),
            live: live::LiveState::default(),
            telemetry: telemetry::Telemetry::default(),
            quota_refresh: None,
            snapshot,
            history_cache: std::collections::HashMap::new(),
            window_save: Arc::new(WindowSave::new(saved_window)),
            details_save: Arc::new(WindowSave::new(saved_details)),
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

/// 拖动期间只保留一个写盘线程，安静 250ms 后保存最后的正常矩形。
struct WindowSave {
    normal: Mutex<Option<WindowPlacement>>,
    dirty: AtomicBool,
    worker: AtomicBool,
}

impl WindowSave {
    /// 用已验证的启动配置初始化；损坏或哨兵矩形视为没有历史位置。
    fn new(initial: Option<WindowPlacement>) -> Self {
        Self { normal: Mutex::new(initial), dirty: AtomicBool::new(false), worker: AtomicBool::new(false) }
    }

    /// 记录稳定矩形。返回 true 表示配置内容实际变化，需要写盘。
    fn remember(&self, placement: WindowPlacement) -> bool {
        let mut normal = self.normal.lock().expect("窗口位置锁损坏");
        if *normal == Some(placement) { return false; }
        *normal = Some(placement);
        true
    }

    /// 最小化或隐藏时返回上次正常矩形，不把当前哨兵值带入配置。
    fn cached(&self) -> Option<WindowPlacement> {
        *self.normal.lock().expect("窗口位置锁损坏")
    }
}

/// 启动结果包含当前快照及原生置顶状态，订阅后拉取以消除首次事件丢失。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StartupState {
    snapshot: RouteSnapshot,
    pinned: bool,
}

/// 窗口创建尚未完成时，后续双击等待同一轮创建，避免并发注册相同标签。
struct DetailsWindowLock(tauri::async_runtime::Mutex<()>);

/// 运行独立桌面应用；不修改 Magpie，不开启 HTTP 接口。
pub fn run() {
    tauri::Builder::default()
        .manage(Arc::new(Mutex::new(Runtime::new())))
        .manage(Mutex::new(details::DetailContext::default()))
        .manage(DetailsWindowLock(tauri::async_runtime::Mutex::new(())))
        .setup(|app| {
            setup_window(app.handle())?;
            setup_tray(app.handle())?;
            start_watcher(app.handle().clone());
            start_live(app.handle().clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            startup_state,
            query_date_stats,
            query_month_usage,
            open_usage_details,
            usage_details_context,
            query_usage_details,
            query_model_costs,
            query_route_history,
            set_pinned,
            reset_window_position,
            hide_window,
            close_window
        ])
        .run(tauri::generate_context!())
        .expect("MagpieHUD 启动失败");
}

/// 恢复保存的正常尺寸，限制到当前工作区，再显示并开始合并位置保存。
fn setup_window(app: &AppHandle) -> tauri::Result<()> {
    let config = app.state::<Arc<Mutex<Runtime>>>().lock().expect("状态锁损坏").config.clone();
    let window = app.get_webview_window("main").expect("缺少 main 窗口");
    window.set_always_on_top(config.always_on_top)?;
    if let Some(placement) = config.window.filter(|placement| stable_window(placement)) {
        let width = placement.width.max(360.0);
        let height = placement.height.max(280.0);
        window.set_position(LogicalPosition::new(placement.x, placement.y))?;
        window.set_size(LogicalSize::new(width, height))?;
    }
    constrain_current(&window)?;
    backdrop::apply(&window)?;
    window.show()?;
    window.unminimize()?;
    // 约束可能改变位置；显示后的稳定矩形才是下次启动应恢复的值。
    let _ = remember_window(app);
    let handle = app.clone();
    window.on_window_event(move |event| match event {
        tauri::WindowEvent::Moved(_) | tauri::WindowEvent::Resized(_) => {
            if let Some(window) = handle.get_webview_window("main") {
                if window.is_minimized().unwrap_or(false) {
                    let _ = window.hide();
                    let _ = window.unminimize();
                } else if let Some(placement) = stable_outer_placement(&window) {
                    let state = handle.state::<Arc<Mutex<Runtime>>>();
                    state.lock().expect("状态锁损坏").window_save.remember(placement);
                }
            }
            schedule_window_save(&handle);
        }
        tauri::WindowEvent::CloseRequested { .. } | tauri::WindowEvent::Focused(false) => {
            let _ = remember_window(&handle);
        }
        _ => {}
    });
    Ok(())
}

/// 托盘显示、隐藏、初始化位置和退出共用窗口辅助函数。
fn setup_tray(app: &AppHandle) -> tauri::Result<()> {
    let show = tauri::menu::MenuItem::with_id(app, "show", "显示窗口", true, None::<&str>)?;
    let hide = tauri::menu::MenuItem::with_id(app, "hide", "隐藏窗口", true, None::<&str>)?;
    let reset = tauri::menu::MenuItem::with_id(app, "reset", "重置 HUD 浮窗位置", true, None::<&str>)?;
    let reset_details = tauri::menu::MenuItem::with_id(app, "reset-details", "重置用量详情位置", true, None::<&str>)?;
    let separator = tauri::menu::PredefinedMenuItem::separator(app)?;
    let exit = tauri::menu::MenuItem::with_id(app, "exit", "退出", true, None::<&str>)?;
    let menu = tauri::menu::Menu::with_items(app, &[&show, &hide, &reset, &reset_details, &separator, &exit])?;
    let tray = tauri::tray::TrayIconBuilder::with_id("main")
        .tooltip("Magpie HUD")
        .menu(&menu)
        .icon(app.default_window_icon().expect("缺少应用图标").clone())
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => show_main_window(app),
            "hide" => hide_main_window(app),
            "reset" => {
                if let Err(error) = reset_window_position(app.clone()) {
                    let _ = app.emit("config-error", error);
                }
            }
            "reset-details" => {
                if let Err(error) = reset_details_position(&app) {
                    let _ = app.emit("config-error", error);
                }
            }
            "exit" => {
                let _ = remember_details(&app);
                let _ = remember_window(app);
                app.exit(0);
            }
            _ => {}
        })
        .build(app)?;
    // 菜单点击只在托盘图标存在时注册；左键同样恢复并聚焦。
    tray.on_tray_icon_event(|tray, event| {
        if let tauri::tray::TrayIconEvent::Click { button: tauri::tray::MouseButton::Left, button_state: tauri::tray::MouseButtonState::Up, .. } = event {
            show_main_window(tray.app_handle());
        }
    });
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
                    Ok(usage) => runtime.telemetry.update_usage(usage, &host),
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
        let mut custom_titles = live::CustomTitles::default();
        let mut child_titles = live::ChildTitles::default();
        let mut names = std::collections::HashMap::new();
        let mut aliases = std::collections::HashMap::new();
        let mut child_labels = std::collections::HashMap::new();
        let mut last_titles = Instant::now() - Duration::from_secs(3);
        let mut last_children = Instant::now() - Duration::from_secs(8);
        loop {
            if last_titles.elapsed() >= Duration::from_secs(2) {
                names = custom_titles.scan();
                names.extend(titles.scan(&live::desktop_roots()));
                aliases = titles.aliases();
                last_titles = Instant::now();
            }
            if last_children.elapsed() >= Duration::from_secs(8) {
                let wanted = {
                    let runtime = shared.lock().expect("状态锁损坏");
                    live::wanted_children(&runtime.snapshot)
                };
                child_labels = child_titles.scan(&wanted);
                last_children = Instant::now();
            }
            let result = live::read_json::<live::Activity>(&agent, &format!("{base}/v1/magpie/concurrency"));
            {
                let mut runtime = shared.lock().expect("状态锁损坏");
                runtime.live.names = names.clone();
                runtime.live.aliases = aliases.clone();
                runtime.live.child_labels = child_labels.clone();
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

/// 单实例详情窗口；启动先订阅再读取上下文，避免首次打开丢失事件。
#[tauri::command]
async fn open_usage_details(app: AppHandle, context: details::DetailContext) -> Result<(), String> {
    if !["tokens", "speed", "cost", "cache", "calls", "average"].contains(&context.metric.as_str()) {
        return Err("未知统计指标".into());
    }
    let gate = app.state::<DetailsWindowLock>();
    let _opening = gate.0.lock().await;
    *app.state::<Mutex<details::DetailContext>>().lock().map_err(|_| "详情状态锁损坏")? = context.clone();
    if let Some(window) = app.get_webview_window("usage-details") {
        window.emit("usage-details-context", &context).map_err(|e| e.to_string())?;
        window.unminimize().map_err(|e| e.to_string())?;
        if !window.is_maximized().unwrap_or(false) { constrain_current(&window).map_err(|e|e.to_string())?; }
        window.show().map_err(|e| e.to_string())?;
        window.set_focus().map_err(|e| e.to_string())?;
    } else {
        let window = tauri::WebviewWindowBuilder::new(&app, "usage-details", tauri::WebviewUrl::App("details.html".into()))
            .title("Magpie HUD · 用量详情").inner_size(960.0, 720.0)
            .min_inner_size(480.0, 480.0).resizable(true).decorations(false).shadow(false).transparent(true).visible(false).center()
            .build().map_err(|e| e.to_string())?;
        let saved = app.state::<Arc<Mutex<Runtime>>>().lock().map_err(|_|"状态锁损坏")?.config.details_window;
        if let Some(saved) = saved.filter(stable_window) {
            window.set_size(LogicalSize::new(saved.width.max(480.0), saved.height.max(480.0))).map_err(|e|e.to_string())?;
            window.set_position(LogicalPosition::new(saved.x,saved.y)).map_err(|e|e.to_string())?;
        }
        constrain_current(&window).map_err(|e|e.to_string())?;
        #[cfg(target_os = "macos")]
        window.set_effects(tauri::utils::config::WindowEffectsConfig {
            effects: vec![tauri::utils::WindowEffect::HudWindow],
            ..Default::default()
        }).map_err(|e| e.to_string())?;
        backdrop::apply(&window).map_err(|e| e.to_string())?;
        window.show().map_err(|e|e.to_string())?;
        let _ = remember_details(&app);
        let handle=app.clone();
        window.on_window_event(move |event| match event {
            tauri::WindowEvent::Moved(_) | tauri::WindowEvent::Resized(_) => {
                if let Some(window)=handle.get_webview_window("usage-details") {
                    if let Some(rect)=stable_outer_placement(&window) {
                        handle.state::<Arc<Mutex<Runtime>>>().lock().expect("状态锁损坏").details_save.remember(rect);
                    }
                }
                schedule_details_save(&handle);
            }
            tauri::WindowEvent::CloseRequested{..} | tauri::WindowEvent::Focused(false) => {let _=remember_details(&handle);}
            _=>{}
        });
    }
    Ok(())
}

#[tauri::command]
fn usage_details_context(app: AppHandle) -> details::DetailContext {
    app.state::<Mutex<details::DetailContext>>().lock().expect("详情状态锁损坏").clone()
}

#[tauri::command]
async fn query_usage_details(app: AppHandle, start: String, end: String, filters: Option<details::DetailFilters>) -> Result<details::DetailData, String> {
    let (path, host) = {
        let state = app.state::<Arc<Mutex<Runtime>>>();
        let runtime = state.lock().map_err(|_| "状态锁损坏")?;
        (usage_path(&runtime.config), runtime.config.magpie_host.clone())
    };
    tauri::async_runtime::spawn_blocking(move || details::query(&path, &host, &start, &end, &filters.unwrap_or_default()))
        .await.map_err(|e| format!("详情查询任务失败：{e}"))?
}

/// 模型图费用按需查询：锁内仅复制当前账本的窄字段，网络分页在后台线程执行。
#[tauri::command]
async fn query_model_costs(app: AppHandle, session: Option<String>, source_key: Option<String>) -> Result<details::ModelCostData, String> {
    let (host, date, ledger_revision, rows) = {
        let state = app.state::<Arc<Mutex<Runtime>>>();
        let runtime = state.lock().map_err(|_| "状态锁损坏")?;
        let selected = session.as_deref().filter(|s| !s.is_empty());
        let canonical = selected.map(|s| runtime.live.aliases.get(s).map(String::as_str).unwrap_or(s));
        (runtime.config.magpie_host.clone(), runtime.snapshot.date.clone(), runtime.ledger.revision,
         runtime.ledger.model_cost_rows(canonical, source_key.as_deref().filter(|s| !s.is_empty()), &runtime.live.aliases))
    };
    tauri::async_runtime::spawn_blocking(move || details::model_costs(rows, &host, &date, ledger_revision))
        .await.map_err(|_| "模型费用查询任务失败".to_string())
}

/// 按需查询一个会话（或其某个子 Agent）的近期请求履历；只接收身份与条数，
/// 路径由配置解析，不触发费用、额度或快照克隆。
#[tauri::command]
async fn query_route_history(
    app: AppHandle,
    session: String,
    subagent: Option<String>,
    limit: Option<usize>,
) -> Result<history::RouteHistoryData, String> {
    let subagent = subagent.filter(|id| !id.trim().is_empty());
    if session.trim().is_empty() {
        return Err("缺少会话身份".into());
    }
    let limit = limit.unwrap_or(history::DEFAULT_LIMIT).clamp(1, history::MAX_LIMIT);
    let (path, scope, today) = {
        let state = app.state::<Arc<Mutex<Runtime>>>();
        let runtime = state.lock().map_err(|_| "状态锁损坏")?;
        (
            usage_path(&runtime.config),
            runtime.live.history_scope(&session, subagent.as_deref()),
            Local::now().date_naive(),
        )
    };
    tauri::async_runtime::spawn_blocking(move || history::query(&path, &session, subagent.as_deref(), limit, scope, today))
        .await
        .map_err(|e| format!("履历查询任务失败：{e}"))?
}

/// 查询指定本地日期的六项统计与 24 小时趋势；历史日期走只读缓存轨，不干扰当日实时账本。
#[tauri::command]
async fn query_date_stats(app: AppHandle, date: String) -> Result<DateStatsView, String> {
    tauri::async_runtime::spawn_blocking(move || read_date_stats(&app, &date))
        .await
        .map_err(|error| format!("历史查询任务失败：{error}"))?
}

fn read_date_stats(app: &AppHandle, date: &str) -> Result<DateStatsView, String> {
    let day = chrono::NaiveDate::parse_from_str(date.trim(), "%Y-%m-%d")
        .map_err(|_| "日期格式无效，应为 YYYY-MM-DD".to_string())?;
    // `%Y-%m-%d` 也接受非零填充，这里要求与接口完全一致才放行。
    if day.format("%Y-%m-%d").to_string() != date.trim() {
        return Err("日期格式无效，应为 YYYY-MM-DD".into());
    }
    let today = Local::now().date_naive();
    if day > today {
        return Err("不能查看未来日期的统计".into());
    }
    let state = app.state::<Arc<Mutex<Runtime>>>();
    let (path, host) = {
        let runtime = state.lock().expect("状态锁损坏");
        if day == today && runtime.snapshot.date == today.to_string() {
            return Ok(DateStatsView {
                date: runtime.snapshot.date.clone(),
                earliest_date: runtime.snapshot.earliest_date.clone(),
                latest_date: today.to_string(),
                daily: runtime.snapshot.daily.clone(),
                agent_stats: runtime.snapshot.agent_stats.clone(),
                cost_error: runtime.snapshot.cost_error.clone(),
            });
        }
        (
            usage_path(&runtime.config),
            runtime.config.magpie_host.clone(),
        )
    };
    let metadata = std::fs::metadata(&path).ok();
    let file_created = metadata.as_ref().and_then(|meta| meta.created().ok());
    let file_modified = metadata.as_ref().and_then(|meta| meta.modified().ok());
    let file_len = metadata.as_ref().map_or(0, std::fs::Metadata::len);
    if day < today {
        let runtime = state.lock().expect("状态锁损坏");
        if let Some(cached) = runtime.history_cache.get(&day) {
            if cached.host == host
                && cached.file_created == file_created
                && cached.file_modified == file_modified
                && cached.file_len == file_len
                && cached.read_at.elapsed() < Duration::from_secs(60)
                && cached.view.cost_error.is_none()
            {
                let mut view = cached.view.clone();
                view.latest_date = today.to_string();
                return Ok(view);
            }
        }
    }
    let (mut daily, mut agent_stats, scanned_earliest) = usage::scan_day_stats(&path, day)?;
    let period = format!("{day}..{day}");
    let cost_error = match live::gateway_url(&host) {
        Ok(base) => {
            let agent = live::client(Duration::from_secs(5));
            let url = format!("{base}/v1/magpie/usage?period={period}");
            match live::read_json::<telemetry::Usage>(&agent, &url) {
                Ok(usage) => telemetry::apply_history_usage(&mut daily, &mut agent_stats, day, &period, &usage).err(),
                Err(error) => Some(error),
            }
        }
        Err(error) => Some(error),
    };
    let mut runtime = state.lock().expect("状态锁损坏");
    let mut current_earliest = chrono::NaiveDate::parse_from_str(&runtime.snapshot.earliest_date, "%Y-%m-%d")
        .unwrap_or(today);
    if let Some(earliest) = scanned_earliest {
        current_earliest = current_earliest.min(earliest);
    }
    let view = DateStatsView {
        date: day.to_string(),
        earliest_date: current_earliest.to_string(),
        latest_date: today.to_string(),
        daily,
        agent_stats,
        cost_error,
    };
    if day < today && view.cost_error.is_none() {
        if runtime.history_cache.len() >= 32 {
            runtime.history_cache.clear();
        }
        runtime.history_cache.insert(
            day,
            CachedDayStats {
                host,
                file_created,
                file_modified,
                file_len,
                read_at: Instant::now(),
                view: view.clone(),
            },
        );
    }
    Ok(view)
}

/// 热力日历的一次单月结果；未来日期以 0 返回，由前端禁用。
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct MonthUsageView {
    month: String,
    earliest_date: String,
    latest_date: String,
    days: Vec<MonthDayUsage>,
    agent_days: Vec<MonthAgentUsage>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct MonthAgentUsage {
    agent: String,
    caller_key_id: String,
    caller_key_name: String,
    days: Vec<MonthDayUsage>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct MonthDayUsage {
    date: String,
    tokens: u64,
    requests: u64,
}

/// 查询所选本地月每天的热力数据；历史缓存轨独立扫描，不占用实时账本或主游标。
#[tauri::command]
async fn query_month_usage(app: AppHandle, month: String) -> Result<MonthUsageView, String> {
    tauri::async_runtime::spawn_blocking(move || read_month_usage(&app, &month))
        .await
        .map_err(|error| format!("月份查询任务失败：{error}"))?
}

fn read_month_usage(app: &AppHandle, month: &str) -> Result<MonthUsageView, String> {
    let (month, start, end) = parse_month(month.trim(), Local::now().date_naive())?;
    let state = app.state::<Arc<Mutex<Runtime>>>();
    let path = {
        let runtime = state.lock().expect("状态锁损坏");
        usage_path(&runtime.config)
    };
    let (days, agent_days, scanned_earliest) = usage::scan_month_scoped_usage(&path, start, end)?;
    let today = Local::now().date_naive();
    let earliest = scanned_earliest
        .map(|date| date.min(today))
        .unwrap_or(today);
    Ok(MonthUsageView {
        month,
        earliest_date: earliest.to_string(),
        latest_date: today.to_string(),
        agent_days: agent_days.into_iter().map(|(agent, caller_key_id, caller_key_name, days)| MonthAgentUsage {
            agent, caller_key_id, caller_key_name,
            days: days.into_iter().map(|day| MonthDayUsage { date: day.date.to_string(), tokens: day.tokens, requests: day.requests }).collect(),
        }).collect(),
        days: days
            .into_iter()
            .map(|day| MonthDayUsage {
                date: day.date.to_string(),
                tokens: day.tokens,
                requests: day.requests,
            })
            .collect(),
    })
}

/// 严格校验 `YYYY-MM` 并给出当月闭区间；格式多余零填充和未来月份都在扫描前拒绝。
fn parse_month(
    input: &str,
    today: chrono::NaiveDate,
) -> Result<(String, chrono::NaiveDate, chrono::NaiveDate), String> {
    if input.len() != 7 || input.as_bytes().get(4) != Some(&b'-') {
        return Err("月份格式无效，应为 YYYY-MM".into());
    }
    let start = chrono::NaiveDate::parse_from_str(&format!("{input}-01"), "%Y-%m-%d")
        .map_err(|_| "月份格式无效，应为 YYYY-MM".to_string())?;
    if start.format("%Y-%m").to_string() != input {
        return Err("月份格式无效，应为 YYYY-MM".into());
    }
    let next = if start.month() == 12 {
        chrono::NaiveDate::from_ymd_opt(start.year() + 1, 1, 1)
    } else {
        chrono::NaiveDate::from_ymd_opt(start.year(), start.month() + 1, 1)
    }
    .ok_or("月份超出支持范围")?;
    let end = next.pred_opt().ok_or("月份超出支持范围")?;
    let this_month = chrono::NaiveDate::from_ymd_opt(today.year(), today.month(), 1)
        .ok_or("无法确定当前月份")?;
    if start > this_month {
        return Err("不能查看未来月份的热力日历".into());
    }
    Ok((input.to_string(), start, end))
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
    if let Some(error) = &runtime.config_error { return Err(error.clone()); }
    let mut config = runtime.config.clone();
    config.always_on_top = pinned;
    if let Some(placement) = runtime.window_save.cached() { config.window = Some(placement); }
    save_config(&config::config_path(), &config)?;
    runtime.config = config;
    Ok(pinned)
}

/// 右键菜单和托盘菜单恢复默认尺寸，并在当前显示器工作区居中。
#[tauri::command]
fn reset_window_position(app: AppHandle) -> Result<(), String> {
    let window = app.get_webview_window("main").ok_or("窗口不存在")?;
    window.unminimize().map_err(|error| error.to_string())?;
    window.unmaximize().map_err(|error| error.to_string())?;
    window.show().map_err(|error| error.to_string())?;
    let scale = window.scale_factor().map_err(|error| error.to_string())?;
    let width = (1120.0 * scale).round().max(1.0) as u32;
    let height = (400.0 * scale).round().max(1.0) as u32;
    let position = window.outer_position().map_err(|error| error.to_string())?;
    let current = PhysicalRect { x: position.x, y: position.y, width, height };
    let work = current_work_area(&window, current).ok_or("找不到当前显示器工作区")?;
    let target = constrain(current, work);
    let x = work.x.saturating_add(work.width.saturating_sub(target.width) as i32 / 2);
    let y = work.y.saturating_add(work.height.saturating_sub(target.height) as i32 / 2);
    window.set_size(PhysicalSize::new(target.width, target.height)).map_err(|error| error.to_string())?;
    window.set_position(PhysicalPosition::new(x, y)).map_err(|error| error.to_string())?;
    constrain_current(&window).map_err(|error| error.to_string())?;
    remember_window(&app).map_err(|error| error.to_string())?;
    window.set_focus().map_err(|error| error.to_string())
}

/// 详情位置独立保存；关闭后的重置清除保存值，下次打开居中。
fn reset_details_position(app: &AppHandle) -> Result<(),String> {
    if let Some(window)=app.get_webview_window("usage-details") {
        window.unminimize().map_err(|e|e.to_string())?;
        window.unmaximize().map_err(|e|e.to_string())?;
        window.set_size(LogicalSize::new(960.0,720.0)).map_err(|e|e.to_string())?;
        window.center().map_err(|e|e.to_string())?;
        constrain_current(&window).map_err(|e|e.to_string())?;
        window.show().map_err(|e|e.to_string())?;
        remember_details(app)?;
        window.set_focus().map_err(|e|e.to_string())
    } else {
        let state=app.state::<Arc<Mutex<Runtime>>>();
        let mut runtime=state.lock().map_err(|_|"状态锁损坏")?;
        if let Some(error)=&runtime.config_error {return Err(error.clone());}
        let mut config=runtime.config.clone();config.details_window=None;
        save_config(&config::config_path(),&config)?;
        runtime.config=config;
        *runtime.details_save.normal.lock().map_err(|_|"详情位置锁损坏")?=None;
        Ok(())
    }
}

fn remember_details(app: &AppHandle) -> Result<(),String> {
    if let Some(window)=app.get_webview_window("usage-details") {
        if let Some(rect)=stable_outer_placement(&window) {
            app.state::<Arc<Mutex<Runtime>>>().lock().map_err(|_|"状态锁损坏")?.details_save.remember(rect);
        }
    }
    persist_details(app)
}

fn persist_details(app: &AppHandle) -> Result<(),String> {
    let state=app.state::<Arc<Mutex<Runtime>>>();
    let mut runtime=state.lock().map_err(|_|"状态锁损坏")?;
    if let Some(error)=&runtime.config_error {return Err(error.clone());}
    let placement=runtime.details_save.cached();
    if runtime.config.details_window==placement {return Ok(());}
    let mut config=runtime.config.clone();config.details_window=placement;
    save_config(&config::config_path(),&config)?;runtime.config=config;Ok(())
}

fn schedule_details_save(app: &AppHandle) {
    let save=app.state::<Arc<Mutex<Runtime>>>().lock().expect("状态锁损坏").details_save.clone();
    save.dirty.store(true,Ordering::Release);
    if save.worker.swap(true,Ordering::AcqRel) {return;}
    let app=app.clone();
    std::thread::spawn(move || {
        loop {
            save.dirty.store(false,Ordering::Release);
            std::thread::sleep(Duration::from_millis(250));
            if save.dirty.load(Ordering::Acquire) {continue;}
            if let Err(error)=persist_details(&app) {let _=app.emit("config-error",error);}
            save.worker.store(false,Ordering::Release);
            if save.dirty.load(Ordering::Acquire) {schedule_details_save(&app);}
            break;
        }
    });
}

/// 供界面隐藏按钮使用：先记住正常矩形，再隐藏到托盘。
#[tauri::command]
fn hide_window(app: AppHandle) -> Result<(), String> {
    hide_main_window(&app);
    Ok(())
}

/// 标题栏关闭按钮保存位置后直接退出，不弹确认。
#[tauri::command]
fn close_window(app: AppHandle) {
    let _ = remember_details(&app);
    let _ = remember_window(&app);
    app.exit(0);
}

/// 显示、取消最小化、限制到工作区并聚焦；托盘左键和“显示窗口”共用。
fn show_main_window(app: &AppHandle) {
    let Some(window) = app.get_webview_window("main") else { return };
    let _ = window.unminimize();
    let _ = constrain_current(&window);
    let _ = window.show();
    let _ = window.set_focus();
}

/// 隐藏前保存最后的正常矩形，避免托盘恢复时回到哨兵位置。
fn hide_main_window(app: &AppHandle) {
    let _ = remember_window(app);
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.hide();
    }
}

/// 250ms 合并写盘；正常矩形已由窗口事件缓存，写盘线程不再查询窗口。
fn schedule_window_save(app: &AppHandle) {
    let save = app.state::<Arc<Mutex<Runtime>>>().lock().expect("状态锁损坏").window_save.clone();
    save.dirty.store(true, Ordering::Release);
    if save.worker.swap(true, Ordering::AcqRel) { return; }
    let app = app.clone();
    std::thread::spawn(move || {
        loop {
            save.dirty.store(false, Ordering::Release);
            std::thread::sleep(Duration::from_millis(250));
            if save.dirty.load(Ordering::Acquire) { continue; }
            let result = persist_window(&app);
            if let Err(error) = result { let _ = app.emit("config-error", error); }
            save.worker.store(false, Ordering::Release);
            if save.dirty.load(Ordering::Acquire) { schedule_window_save(&app); }
            break;
        }
    });
}

/// 所有配置写入持有同一状态锁；成功后才标记保存，失败允许后续重试。
fn persist_window(app: &AppHandle) -> Result<(), String> {
    let state = app.state::<Arc<Mutex<Runtime>>>();
    let mut runtime = state.lock().expect("状态锁损坏");
    if let Some(error) = &runtime.config_error { return Err(error.clone()); }
    let Some(placement) = runtime.window_save.cached() else { return Ok(()) };
    if runtime.config.window == Some(placement) { return Ok(()) }
    let mut config = runtime.config.clone();
    config.window = Some(placement);
    save_config(&config::config_path(), &config)?;
    runtime.config = config;
    Ok(())
}

/// 同步捕获并保存最后正常矩形，用于关闭、隐藏、退出和初始化位置。
fn remember_window(app: &AppHandle) -> Result<(), String> {
    let window = app.get_webview_window("main").ok_or("缺少窗口")?;
    let placement = stable_outer_placement(&window);
    {
        let state = app.state::<Arc<Mutex<Runtime>>>();
        let runtime = state.lock().expect("状态锁损坏");
        if let Some(placement) = placement { runtime.window_save.remember(placement); }
        runtime.window_save.dirty.store(false, Ordering::Release);
    }
    let result = persist_window(app);
    if let Err(error) = &result { let _ = app.emit("config-error", error); }
    result
}

/// 拒绝最小化哨兵、零尺寸和非有限坐标，只接受可再次恢复的正常矩形。
fn stable_outer_placement(window: &WebviewWindow) -> Option<WindowPlacement> {
    #[cfg(windows)]
    {
        use windows::Win32::Foundation::HWND;
        use windows::Win32::UI::WindowsAndMessaging::{IsIconic, IsWindowVisible, IsZoomed};
        let hwnd = HWND(window.hwnd().ok()?.0);
        // 原生移动/缩放事件可能早于 Tauri 的最大化状态更新，不能缓存过渡矩形。
        if unsafe { IsIconic(hwnd).as_bool() || IsZoomed(hwnd).as_bool() || !IsWindowVisible(hwnd).as_bool() } { return None; }
    }
    if window.is_minimized().ok()? || window.is_maximized().ok()? || !window.is_visible().unwrap_or(false) { return None; }
    let position = window.outer_position().ok()?;
    let size = window.outer_size().ok()?;
    let scale = window.scale_factor().ok()?;
    let placement = WindowPlacement {
        x: position.x as f64 / scale,
        y: position.y as f64 / scale,
        width: size.width as f64 / scale,
        height: size.height as f64 / scale,
    };
    stable_window(&placement).then_some(placement)
}

/// 配置和运行中矩形共用的稳定性判断。
fn stable_window(placement: &WindowPlacement) -> bool {
    [placement.x, placement.y, placement.width, placement.height].iter().all(|value| value.is_finite())
        && placement.width >= 1.0
        && placement.height >= 1.0
        && placement.x > -30_000.0
        && placement.y > -30_000.0
}

/// 优先使用 Tauri 当前显示器工作区，失败时退回 Win32 重叠面积选择。
fn current_work_area(window: &WebviewWindow, current: PhysicalRect) -> Option<PhysicalRect> {
    if let Some(monitor) = window.current_monitor().ok().flatten() {
        let area = monitor.work_area();
        if area.size.width > 0 && area.size.height > 0 {
            return Some(PhysicalRect { x: area.position.x, y: area.position.y, width: area.size.width, height: area.size.height });
        }
    }
    let mut areas = winmon::work_areas();
    if areas.is_empty() {
        areas = window.available_monitors().ok()?.iter().map(|monitor| {
            let area = monitor.work_area();
            PhysicalRect { x: area.position.x, y: area.position.y, width: area.size.width, height: area.size.height }
        }).filter(|area| area.width > 0 && area.height > 0).collect();
    }
    monitor_for_window(current, &areas)
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
    let Some(work) = current_work_area(window, current) else { return Ok(()) };
    let next = constrain(current, work);
    // 工作区不足时降低最小尺寸，正常桌面仍保留可操作的缩放下限。
    let scale = window.scale_factor()?;
    let (min_width,min_height)=if window.label()=="usage-details" {(480.0,480.0)} else {(360.0,280.0)};
    window.set_min_size(Some(PhysicalSize::new(
        ((min_width * scale) as u32).min(work.width),
        ((min_height * scale) as u32).min(work.height),
    )))?;
    window.set_size(PhysicalSize::new(next.width, next.height))?;
    window.set_position(PhysicalPosition::new(next.x, next.y))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 最小化哨兵、零尺寸和非有限值都不能进入持久配置。
    #[test]
    fn rejects_minimized_sentinel_and_empty_geometry() {
        assert!(!stable_window(&WindowPlacement { x: -32_000.0, y: 10.0, width: 1120.0, height: 400.0 }));
        assert!(!stable_window(&WindowPlacement { x: 10.0, y: 10.0, width: 0.0, height: 400.0 }));
        assert!(!stable_window(&WindowPlacement { x: f64::NAN, y: 10.0, width: 1120.0, height: 400.0 }));
        assert!(stable_window(&WindowPlacement { x: -120.0, y: 40.0, width: 1120.0, height: 400.0 }));
    }

    /// 相同矩形不重复记脏，变化后缓存仍是最后的正常值。
    #[test]
    fn remembers_only_changed_normal_geometry() {
        let save = WindowSave::new(None);
        let first = WindowPlacement { x: 20.0, y: 30.0, width: 800.0, height: 500.0 };
        assert!(save.remember(first));
        assert!(!save.remember(first));
        assert_eq!(save.cached(), Some(first));
    }

    /// 月份严格按要求格式，未来月份与非法月份在扫描前拒绝。
    #[test]
    fn rejects_invalid_and_future_months_before_scanning() {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 10, 23).unwrap();
        let (label, start, end) = parse_month("2026-02", today).unwrap();
        assert_eq!(label, "2026-02");
        assert_eq!(start, chrono::NaiveDate::from_ymd_opt(2026, 2, 1).unwrap());
        assert_eq!(end, chrono::NaiveDate::from_ymd_opt(2026, 2, 28).unwrap());
        let (_, _, leap_end) = parse_month("2024-02", today).unwrap();
        assert_eq!(leap_end, chrono::NaiveDate::from_ymd_opt(2024, 2, 29).unwrap());
        let future_leap = parse_month("2028-02", today).unwrap_err();
        assert!(future_leap.contains("未来月份"));
        let future = parse_month("2026-11", today).unwrap_err();
        assert!(future.contains("未来月份"));
        for invalid in ["2026-13", "2026-00", "2026-1", "2026-1-", "2026/01", "2026-01x", "12026-01", "2026-1a", ""] {
            assert!(parse_month(invalid, today).is_err(), "{invalid} 不应通过");
        }
        let (_, _, december_end) = parse_month("2025-12", today).unwrap();
        assert_eq!(december_end, chrono::NaiveDate::from_ymd_opt(2025, 12, 31).unwrap());
        let (_, _, leap_ok) = parse_month("2024-02", today).unwrap();
        assert_eq!(leap_ok, chrono::NaiveDate::from_ymd_opt(2024, 2, 29).unwrap());
    }
}
