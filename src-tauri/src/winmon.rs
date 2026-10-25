//! 枚举 Windows 显示器的物理像素工作区，排除任务栏；不以整屏边界冒充可用区域。

use crate::placement::PhysicalRect;

/// 枚举全部工作区。回调只复制 GetMonitorInfoW 的 rcWork，不修改系统布局。
pub fn work_areas() -> Vec<PhysicalRect> {
    #[cfg(windows)]
    {
        use windows::Win32::Foundation::{LPARAM, RECT};
        use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO};
        use windows::core::BOOL;
        let mut output = Vec::new();
        /// 把一块显示器的真实工作区写入枚举器持有的向量。
        unsafe extern "system" fn callback(monitor: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
            let mut info=MONITORINFO { cbSize:std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
            if GetMonitorInfoW(monitor,&mut info).as_bool() {
                let output=&mut *(data.0 as *mut Vec<PhysicalRect>);
                let rect=info.rcWork;
                output.push(PhysicalRect {x:rect.left,y:rect.top,width:rect.right.saturating_sub(rect.left).max(0) as u32,height:rect.bottom.saturating_sub(rect.top).max(0) as u32});
            }
            true.into()
        }
        unsafe { let _=EnumDisplayMonitors(None,None,Some(callback),LPARAM(&mut output as *mut Vec<PhysicalRect> as isize)); }
        output
    }
    #[cfg(not(windows))]
    Vec::new()
}
