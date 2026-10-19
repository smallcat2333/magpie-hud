//! MagpieHUD 的 Tauri 入口。
//!
//! 负责窗口、托盘、配置和 usage.jsonl 的只读监视；日志解析与坐标计算分别由子模块完成。
//! 后台线程只发送快照，不直接操作 WebView。

mod config;
mod placement;
mod tail;
mod usage;
mod winmon;

use config::{load_config, save_config, usage_path, AppConfig, WindowPlacement};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use placement::{constrain, monitor_for_window, PhysicalRect};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;
use tail::{read_tail, Tail};
use tauri::{AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, PhysicalPosition, PhysicalSize, WebviewWindow};
use usage::{empty_snapshot, latest_record, snapshot_from_record, RouteSnapshot};

/// 最近一次已发给前端的记录标识，避免相同 route_id/时间重复刷新。
struct WatchState { identity: String }

/// 前端启动时拉取的当前状态，解决事件早于页面监听的竞态。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StartupState { snapshot: RouteSnapshot, pinned: bool }

/// 创建托盘、恢复窗口并启动日志监视。
pub fn run() {
    tauri::Builder::default().manage(Mutex::new(WatchState { identity: String::new() })).setup(|app| {
        let config = setup_window(app.handle())?;
        setup_tray(app.handle())?;
        start_watcher(app.handle().clone(), usage_path(&config));
        Ok(())
    }).invoke_handler(tauri::generate_handler![startup_state, set_pinned]).run(tauri::generate_context!()).expect("MagpieHUD 启动失败");
}

/// 读取配置、约束窗口并在准备好后显示。
///
/// 配置损坏时使用默认值，同时向前端发送可见错误；正常位置在退出时保存。
fn setup_window(app: &AppHandle) -> tauri::Result<AppConfig> {
    let path = config::config_path();
    let loaded = load_config(&path);
    let config = loaded.clone().unwrap_or_default();
    let window = app.get_webview_window("main").expect("缺少 main 窗口");
    window.set_always_on_top(config.always_on_top)?;
    if let Some(placement) = config.window { apply_placement(&window, placement)?; }
    constrain_current(&window)?;
    window.show()?;
    if let Err(error) = loaded { let _ = window.emit("config-error", error); }
    let app_handle = app.clone();
    window.on_window_event(move |event| if let tauri::WindowEvent::CloseRequested { .. } = event { let _ = persist_window(&app_handle); });
    Ok(config)
}

/// 创建显示、隐藏和退出三个托盘菜单。
fn setup_tray(app: &AppHandle) -> tauri::Result<()> {
    let show = tauri::menu::MenuItem::with_id(app, "show", "显示窗口", true, None::<&str>)?;
    let hide = tauri::menu::MenuItem::with_id(app, "hide", "隐藏窗口", true, None::<&str>)?;
    let exit = tauri::menu::MenuItem::with_id(app, "exit", "退出", true, None::<&str>)?;
    let menu = tauri::menu::Menu::with_items(app, &[&show, &hide, &exit])?;
    let tray = tauri::tray::TrayIconBuilder::with_id("main").tooltip("MagpieHUD").menu(&menu).icon(app.default_window_icon().unwrap().clone()).on_menu_event(|app, event| match event.id.as_ref() {
        "show" => { if let Some(window) = app.get_webview_window("main") { let _ = constrain_current(&window); let _ = window.show(); let _ = window.set_focus(); } }
        "hide" => { if let Some(window) = app.get_webview_window("main") { let _ = window.hide(); } }
        "exit" => { let _ = persist_window(app); app.exit(0); }
        _ => {}
    });
    tray.build(app)?;
    Ok(())
}

/// 启动目录监视，并立即发布一次当前尾部快照。
///
/// 连续写入在 100ms 内合并；文件被替换或重建时重新挂接监视。
fn start_watcher(app: AppHandle, path: PathBuf) {
    publish(&app, &path, false);
    std::thread::spawn(move || {
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut watcher = RecommendedWatcher::new(sender, notify::Config::default()).expect("无法创建文件监视");
        let parent = path.parent().expect("usage 路径缺少目录").to_path_buf();
        std::fs::create_dir_all(&parent).ok();
        watcher.watch(&parent, RecursiveMode::NonRecursive).expect("无法监视 usage 目录");
        loop {
            let Ok(result) = receiver.recv() else { break; };
            if result.is_err() { continue; }
            while receiver.recv_timeout(Duration::from_millis(100)).is_ok() {}
            publish(&app, &path, true);
        }
    });
}

/// 读取尾部并发送快照；相同有效记录不重复发送。
fn publish(app: &AppHandle, path: &std::path::Path, deduplicate: bool) {
    let host = load_config(&config::config_path()).unwrap_or_default().magpie_host;
    let snapshot = match read_tail(path) {
        Tail::Ready(text) => match latest_record(&text) { Some(record) => snapshot_from_record(&record, &host, false), None => empty_snapshot("waiting", &path.display().to_string(), &host) },
        Tail::Empty => empty_snapshot("waiting", &path.display().to_string(), &host),
        Tail::Missing => empty_snapshot("missing", &path.display().to_string(), &host),
        Tail::Error(_) => return,
    };
    let state = app.state::<Mutex<WatchState>>();
    let mut guard = state.lock().expect("监视状态锁损坏");
    if deduplicate && snapshot.active && snapshot.identity == guard.identity { return; }
    if snapshot.active { guard.identity.clone_from(&snapshot.identity); }
    drop(guard);
    let _ = app.emit("route-update", snapshot);
}

/// 返回当前快照与置顶状态，供页面加载后主动对齐。
#[tauri::command]
fn startup_state(_app: AppHandle) -> StartupState {
    let config = load_config(&config::config_path()).unwrap_or_default();
    let path = usage_path(&config);
    let snapshot = match read_tail(&path) {
        Tail::Ready(text) => latest_record(&text).map(|record| snapshot_from_record(&record, &config.magpie_host, false)).unwrap_or_else(|| empty_snapshot("waiting", &path.display().to_string(), &config.magpie_host)),
        Tail::Missing => empty_snapshot("missing", &path.display().to_string(), &config.magpie_host),
        _ => empty_snapshot("waiting", &path.display().to_string(), &config.magpie_host),
    };
    StartupState { snapshot, pinned: config.always_on_top }
}

/// 切换置顶并立即持久化；返回实际配置值，前端不得在失败时假装成功。
#[tauri::command]
fn set_pinned(app: AppHandle, pinned: bool) -> Result<bool, String> {
    let window = app.get_webview_window("main").ok_or("窗口不存在")?;
    window.set_always_on_top(pinned).map_err(|error| error.to_string())?;
    let path = config::config_path();
    let mut config = load_config(&path).unwrap_or_default();
    config.always_on_top = pinned;
    save_config(&path, &config)?;
    Ok(pinned)
}

/// 读取当前窗口位置并保存；关闭和托盘退出共用。
fn persist_window(app: &AppHandle) -> tauri::Result<()> {
    let Some(window) = app.get_webview_window("main") else { return Ok(()); };
    let position = window.outer_position()?;
    let size = window.outer_size()?;
    let scale = window.scale_factor()?;
    let path = config::config_path();
    let mut config = load_config(&path).unwrap_or_default();
    config.window = Some(WindowPlacement { x: position.x as f64 / scale, y: position.y as f64 / scale, width: size.width as f64 / scale, height: size.height as f64 / scale });
    let _ = save_config(&path, &config);
    Ok(())
}

/// 按逻辑坐标设置窗口，Tauri 再按当前显示器缩放。
fn apply_placement(window: &WebviewWindow, placement: WindowPlacement) -> tauri::Result<()> {
    window.set_position(LogicalPosition::new(placement.x, placement.y))?;
    window.set_size(LogicalSize::new(placement.width, placement.height))
}

/// 用 Windows 工作区约束当前窗口，避免恢复坐标盖住任务栏或落在屏外。
fn constrain_current(window: &WebviewWindow) -> tauri::Result<()> {
    let position = window.outer_position()?;
    let size = window.outer_size()?;
    let current = PhysicalRect { x: position.x, y: position.y, width: size.width, height: size.height };
    let monitors = winmon::work_areas();
    let Some(work_area) = monitor_for_window(current, &monitors) else { return Ok(()); };
    let next = constrain(current, work_area);
    window.set_position(PhysicalPosition::new(next.x, next.y))?;
    window.set_size(PhysicalSize::new(next.width, next.height))
}
