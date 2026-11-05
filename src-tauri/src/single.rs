//! 启动单实例检查：第二个实例唤醒已运行窗口并退出。

/// 已有实例运行时唤醒它并返回 true；否则返回 false 继续正常启动。
/// Windows 用命名互斥锁判断同一路径实例；macOS/Linux 暂不做单实例，返回 false。
pub fn raise_existing_instance() -> bool {
    #[cfg(windows)]
    {
        raise_windows_instance()
    }
    #[cfg(not(windows))]
    {
        false
    }
}

#[cfg(windows)]
fn raise_windows_instance() -> bool {
    use windows::Win32::Foundation::{CloseHandle, GetLastError, HWND, LPARAM};
    use windows::Win32::System::Threading::CreateMutexW;
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetForegroundWindow, GetWindowThreadProcessId, IsIconic,
        IsWindowVisible, PostMessageW, SetForegroundWindow, ShowWindowAsync,
        WM_USER, SW_RESTORE,
    };
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };

    // 同一路径的实例独占互斥锁；名字含反斜杠在 Windows 内核里是合法对象名。
    let current_exe = std::env::current_exe().ok().map(|path| {
        path.to_string_lossy().replace('\\', "|")
    }).unwrap_or_else(|| "magpie-hud".into());
    let name = format!("Local\\MagpieHUD-{}", current_exe);
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    let handle = unsafe { CreateMutexW(None, true, windows::core::PCWSTR(wide.as_ptr())) };
    let already_running = match handle {
        Ok(_) => unsafe { GetLastError().0 == 183 },
        Err(_) => false,
    };
    if !already_running {
        // 拿到互斥锁：我们是第一个实例，hold 住让后续实例发现。
        std::mem::forget(handle);
        return false;
    }
    if let Ok(handle) = handle {
        unsafe { let _ = CloseHandle(handle); }
    }

    // 收集同名 exe 的其他进程，在主窗口发 WM_USER+0x4D45 ("ME") 请求置顶。
    let own_pid = std::process::id();
    let own_name = std::env::current_exe().ok()
        .and_then(|path| path.file_name().map(|name| name.to_string_lossy().into_owned()))
        .unwrap_or_default();
    let mut targets = Vec::new();
    if let Ok(snapshot) = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) } {
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        if unsafe { Process32FirstW(snapshot, &mut entry).is_ok() } {
            loop {
                let exe = unsafe { windows::core::PWSTR(entry.szExeFile.as_ptr() as *mut u16)
                    .to_string().unwrap_or_default() };
                if exe.eq_ignore_ascii_case(&own_name) && entry.th32ProcessID != own_pid {
                    targets.push(entry.th32ProcessID);
                }
                if unsafe { Process32NextW(snapshot, &mut entry).is_err() } { break; }
            }
        }
        unsafe { let _ = windows::Win32::Foundation::CloseHandle(snapshot); }
    }
    if targets.is_empty() { return false; }

    // 枚举顶层窗口，命中属于同名进程的可见窗口即可置前。
    extern "system" fn find_window(hwnd: HWND, param: LPARAM) -> windows::core::BOOL {
        unsafe {
            let (targets, found) = &mut *(param.0 as *mut (Vec<u32>, Vec<HWND>));
            let mut pid = 0u32;
            let _ = GetWindowThreadProcessId(hwnd, Some(&mut pid));
            if targets.contains(&pid) && IsWindowVisible(hwnd).as_bool() {
                found.push(hwnd);
                return false.into();
            }
            true.into()
        }
    }
    let mut params = (targets, Vec::new());
    let _ = unsafe {
        EnumWindows(Some(find_window), LPARAM(&mut params as *mut _ as isize))
    };
    let (_, mut candidates) = params;
    if let Some(hwnd) = candidates.pop() {
        unsafe {
            if IsIconic(hwnd).as_bool() {
                let _ = ShowWindowAsync(hwnd, SW_RESTORE);
            }
            // 置顶失败不阻塞退出；窗口可能在别的会话或已关闭。
            let _ = SetForegroundWindow(hwnd);
            let _ = PostMessageW(Some(hwnd), WM_USER + 0x4D45, windows::Win32::Foundation::WPARAM(0), LPARAM(0));
            let _ = GetForegroundWindow();
        }
        return true;
    }
    false
}
