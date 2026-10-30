//! Windows 桌面磨砂使用 Accent 合成，不随 DWM 的非激活状态退为不透明背景。

/// 只在建窗时设置当前 HWND；不修改系统透明效果或其他应用的窗口。
pub fn apply(window: &tauri::WebviewWindow) -> tauri::Result<()> {
    #[cfg(windows)]
    {
        use std::ffi::c_void;
        use windows::core::{s, w, BOOL};
        use windows::Win32::Foundation::HWND;
        use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_SYSTEMBACKDROP_TYPE};
        use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};

        #[repr(C)]
        struct AccentPolicy {
            state: i32,
            flags: u32,
            color: u32,
            animation: u32,
        }
        #[repr(C)]
        struct CompositionData {
            attribute: i32,
            data: *const c_void,
            size: usize,
        }
        type SetComposition = unsafe extern "system" fn(HWND, *const CompositionData) -> BOOL;

        let hwnd = HWND(window.hwnd()?.0);
        // 此导出未包含在 Windows SDK 导入库中；不可用时保留 Tauri 的系统磨砂。
        let set_composition: SetComposition = unsafe {
            let Ok(module) = GetModuleHandleW(w!("user32.dll")) else { return Ok(()) };
            let Some(function) = GetProcAddress(module, s!("SetWindowCompositionAttribute")) else { return Ok(()) };
            std::mem::transmute(function)
        };
        // 禁用 Windows 11 按焦点切换的系统材质，再使用桌面合成 Acrylic。
        // 颜色为 ABGR；极低的原生染色让 CSS 的 50% 底色负责遮罩，文字保持不透明。
        let no_backdrop = 1i32;
        let policy = AccentPolicy { state: 4, flags: 0, color: 0x011e1917, animation: 0 };
        let data = CompositionData {
            attribute: 19,
            data: &policy as *const AccentPolicy as *const c_void,
            size: std::mem::size_of::<AccentPolicy>(),
        };
        unsafe {
            let _ = DwmSetWindowAttribute(hwnd, DWMWA_SYSTEMBACKDROP_TYPE,
                &no_backdrop as *const i32 as *const c_void, std::mem::size_of::<i32>() as u32);
            if !set_composition(hwnd, &data).as_bool() {
                // 合成接口失败则恢复原生 Acrylic，不阻止监视窗口启动。
                let acrylic = 3i32;
                let _ = DwmSetWindowAttribute(hwnd, DWMWA_SYSTEMBACKDROP_TYPE,
                    &acrylic as *const i32 as *const c_void, std::mem::size_of::<i32>() as u32);
            }
        }
    }
    #[cfg(target_os = "macos")]
    window.set_effects(tauri::utils::config::WindowEffectsConfig {
        effects: vec![tauri::utils::WindowEffect::HudWindow],
        ..Default::default()
    })?;
    #[cfg(not(any(windows, target_os = "macos")))]
    let _ = window;
    Ok(())
}
