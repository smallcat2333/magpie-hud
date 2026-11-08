//! Tauri 窗口、托盘与只读日志监视。单一运行状态缓存当日统计和会话，前端启动主动拉取快照。

mod backdrop;
mod config;
mod connection;
mod desktop_data;
mod remote;
mod placement;
mod single;
mod winmon;

use config::{save_config, WindowPlacement};
use desktop_data::Runtime;
use hud_core::{details, usage::RouteSnapshot};
#[cfg(test)]
use hud_core::query::parse_month;
use serde_json::{json, Value};
use placement::{constrain, monitor_for_window, PhysicalRect};
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{
    AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, PhysicalPosition, PhysicalSize,
    WebviewWindow,
};

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

/// 已有同一路径实例时唤醒其窗口；第二个实例由 main.rs 调用后退出。
pub use single::raise_existing_instance;

/// 运行独立桌面应用；不修改 Magpie，不开启 HTTP 接口。
pub fn run() {
    tauri::Builder::default()
        .manage(Arc::new(Mutex::new(Runtime::new())))
        .manage(Mutex::new(details::DetailContext::default()))
        .manage(DetailsWindowLock(tauri::async_runtime::Mutex::new(())))
        .setup(|app| {
            setup_window(app.handle())?;
            setup_tray(app.handle())?;
            desktop_data::start_source(app.handle().clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            startup_state,
            desktop_data::connection_settings,
            desktop_data::test_connection,
            desktop_data::save_connection,
            desktop_data::forget_connection,
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
                desktop_data::stop_source(&app);
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
async fn query_usage_details(app: AppHandle, start: String, end: String, filters: Option<details::DetailFilters>) -> Result<Value, String> {
    desktop_data::query(app, "query_usage_details", json!({"start":start,"end":end,"filters":filters})).await
}

#[tauri::command]
async fn query_model_costs(app: AppHandle, session: Option<String>, source_key: Option<String>) -> Result<Value, String> {
    desktop_data::query(app, "query_model_costs", json!({"session":session,"sourceKey":source_key})).await
}

#[tauri::command]
async fn query_route_history(app: AppHandle, session: String, subagent: Option<String>, limit: Option<usize>) -> Result<Value, String> {
    desktop_data::query(app, "query_route_history", json!({"session":session,"subagent":subagent,"limit":limit})).await
}

#[tauri::command]
async fn query_date_stats(app: AppHandle, date: String) -> Result<Value, String> {
    desktop_data::query(app, "query_date_stats", json!({"date":date})).await
}

#[tauri::command]
async fn query_month_usage(app: AppHandle, month: String) -> Result<Value, String> {
    desktop_data::query(app, "query_month_usage", json!({"month":month})).await
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
    desktop_data::stop_source(&app);
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
