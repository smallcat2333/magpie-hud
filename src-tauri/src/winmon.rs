//! 枚举 Windows 显示器工作区。
//!
//! 返回物理像素矩形，包含任务栏被排除后的可用区域；非 Windows 编译目标返回空列表。

use crate::placement::PhysicalRect;

/// 枚举全部显示器工作区。
///
/// Windows 回调只复制系统给出的矩形，不在枚举期间移动窗口。
pub fn work_areas() -> Vec<PhysicalRect> {
    #[cfg(windows)]
    {
        use windows::Win32::Foundation::{LPARAM, RECT};
        use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, HDC, HMONITOR};
        let mut output = Vec::new();
        unsafe extern "system" fn callback(monitor: HMONITOR, _: HDC, rectangle: *mut RECT, data: LPARAM) -> i32 {
            let _ = monitor;
            if !rectangle.is_null() {
                let output = &mut *(data.0 as *mut Vec<PhysicalRect>);
                let rect = &*rectangle;
                output.push(PhysicalRect { x: rect.left, y: rect.top, width: rect.right.saturating_sub(rect.left).max(0) as u32, height: rect.bottom.saturating_sub(rect.top).max(0) as u32 });
            }
            1
        }
        unsafe { let _ = EnumDisplayMonitors(None, None, Some(std::mem::transmute(callback as unsafe extern "system" fn(HMONITOR, HDC, *mut RECT, LPARAM) -> i32)), LPARAM(&mut output as *mut Vec<PhysicalRect> as isize)); }
        output
    }
    #[cfg(not(windows))]
    Vec::new()
}
