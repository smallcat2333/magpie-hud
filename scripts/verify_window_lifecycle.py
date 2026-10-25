"""隔离验证窗口恢复、移动保存、右键菜单和最小化入托盘。

仅启动临时目录中的 exe 副本；所有配置、日志和 WebView 数据都在临时目录。
通过 Win32 和系统辅助功能操作实际窗口，不向产品加入测试入口。
"""
import argparse
import ctypes
import json
import math
import os
import shutil
import subprocess
import tempfile
import time
from ctypes import wintypes
from pathlib import Path


user32 = ctypes.WinDLL("user32", use_last_error=True)
WNDENUMPROC = ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.HWND, wintypes.LPARAM)
SW_MINIMIZE = 6
WM_CLOSE = 0x0010


class RECT(ctypes.Structure):
    """物理像素矩形。"""

    _fields_ = [(name, wintypes.LONG) for name in ("left", "top", "right", "bottom")]


class POINT(ctypes.Structure):
    """屏幕位置。"""

    _fields_ = [("x", wintypes.LONG), ("y", wintypes.LONG)]


class MONITORINFO(ctypes.Structure):
    """显示器和工作区矩形。"""

    _fields_ = [("cbSize", wintypes.DWORD), ("rcMonitor", RECT), ("rcWork", RECT), ("dwFlags", wintypes.DWORD)]


def configure_signatures():
    """声明完整句柄签名，并统一为物理坐标。"""
    for name, result, arguments in (
        ("EnumWindows", wintypes.BOOL, [WNDENUMPROC, wintypes.LPARAM]),
        ("GetWindowThreadProcessId", wintypes.DWORD, [wintypes.HWND, ctypes.POINTER(wintypes.DWORD)]),
        ("GetClassNameW", ctypes.c_int, [wintypes.HWND, wintypes.LPWSTR, ctypes.c_int]),
        ("GetWindowTextW", ctypes.c_int, [wintypes.HWND, wintypes.LPWSTR, ctypes.c_int]),
        ("IsWindowVisible", wintypes.BOOL, [wintypes.HWND]),
        ("IsIconic", wintypes.BOOL, [wintypes.HWND]),
        ("GetWindowRect", wintypes.BOOL, [wintypes.HWND, ctypes.POINTER(RECT)]),
        ("GetDpiForWindow", wintypes.UINT, [wintypes.HWND]),
        ("ShowWindow", wintypes.BOOL, [wintypes.HWND, ctypes.c_int]),
        ("SetWindowPos", wintypes.BOOL, [wintypes.HWND, wintypes.HWND, ctypes.c_int, ctypes.c_int, ctypes.c_int, ctypes.c_int, wintypes.UINT]),
        ("PostMessageW", wintypes.BOOL, [wintypes.HWND, wintypes.UINT, wintypes.WPARAM, wintypes.LPARAM]),
        ("SetForegroundWindow", wintypes.BOOL, [wintypes.HWND]),
        ("WindowFromPoint", wintypes.HWND, [POINT]),
        ("IsChild", wintypes.BOOL, [wintypes.HWND, wintypes.HWND]),
        ("ScreenToClient", wintypes.BOOL, [wintypes.HWND, ctypes.POINTER(POINT)]),
        ("GetCursorPos", wintypes.BOOL, [ctypes.POINTER(POINT)]),
        ("SetCursorPos", wintypes.BOOL, [ctypes.c_int, ctypes.c_int]),
        ("mouse_event", None, [wintypes.DWORD, wintypes.DWORD, wintypes.DWORD, wintypes.DWORD, ctypes.c_size_t]),
        ("MonitorFromWindow", wintypes.HANDLE, [wintypes.HWND, wintypes.DWORD]),
        ("GetMonitorInfoW", wintypes.BOOL, [wintypes.HANDLE, ctypes.POINTER(MONITORINFO)]),
    ):
        function = getattr(user32, name)
        function.argtypes = arguments
        function.restype = result
    user32.SetProcessDPIAware()


def main_window(pid):
    """只匹配本次启动进程的 Tauri 窗口；启动未就绪时返回 None。"""
    found = []

    @WNDENUMPROC
    def callback(hwnd, _lparam):
        owner = wintypes.DWORD()
        user32.GetWindowThreadProcessId(hwnd, ctypes.byref(owner))
        if owner.value == pid:
            name = ctypes.create_unicode_buffer(128)
            user32.GetClassNameW(hwnd, name, 128)
            if name.value == "Tauri Window":
                found.append(hwnd)
        return True

    if not user32.EnumWindows(callback, 0):
        raise ctypes.WinError(ctypes.get_last_error())
    if len(found) > 1:
        raise AssertionError(f"本进程有多个产品窗口：{found}")
    return found[0] if found else None


def wait_for(predicate, timeout, message):
    """等待条件就绪；编程错误不作为暂态吞掉。"""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = predicate()
        if value:
            return value
        time.sleep(0.1)
    raise AssertionError(message)


def placement(hwnd):
    """读取实际外框，不读取最小化窗口的哨兵位置作历史值。"""
    rectangle = RECT()
    if not user32.GetWindowRect(hwnd, ctypes.byref(rectangle)):
        raise ctypes.WinError(ctypes.get_last_error())
    return {"x": rectangle.left, "y": rectangle.top, "width": rectangle.right - rectangle.left, "height": rectangle.bottom - rectangle.top}


def read_window(path):
    """配置写盘期间允许短暂读取失败。"""
    try:
        window = json.loads(path.read_text(encoding="utf-8")).get("window")
        if not isinstance(window, dict):
            return None
        values = [window.get(name) for name in ("x", "y", "width", "height")]
        if not all(isinstance(value, (int, float)) and math.isfinite(value) for value in values):
            return None
        if values[0] <= -30000 or values[1] <= -30000 or values[2] < 1 or values[3] < 1:
            return None
        return window
    except (OSError, json.JSONDecodeError):
        return None


def close_process(process):
    """只关闭本脚本启动的产品窗口，失败时结束同一个子进程。"""
    if process.poll() is not None:
        return
    hwnd = main_window(process.pid)
    if hwnd:
        user32.PostMessageW(hwnd, WM_CLOSE, 0, 0)
    try:
        process.wait(timeout=6)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait(timeout=5)


UIA_SCRIPT = r'''param([long]$Hwnd, [string]$Action, [string]$Name)
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
if ($Action -eq 'taskbar') {
    $condition = New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::ClassNameProperty, 'Shell_TrayWnd')
    $bar = [System.Windows.Automation.AutomationElement]::RootElement.FindFirst([System.Windows.Automation.TreeScope]::Children, $condition)
    if (-not $bar) { throw '找不到任务栏辅助功能节点' }
    $nodes = $bar.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition)
    $names = @($nodes | ForEach-Object { $_.Current.Name } | Where-Object { $_ })
    ConvertTo-Json -InputObject $names -Compress
} else {
    $root = [System.Windows.Automation.AutomationElement]::FromHandle([IntPtr]$Hwnd)
    $condition = New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::NameProperty, $Name)
    $deadline = [DateTime]::UtcNow.AddSeconds(5)
    do {
        $button = $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $condition)
        if ($button -and -not $button.Current.IsOffscreen) { break }
        Start-Sleep -Milliseconds 100
    } while ([DateTime]::UtcNow -lt $deadline)
    if (-not $button -or $button.Current.IsOffscreen) { throw "找不到菜单按钮：$Name" }
    $box = $button.Current.BoundingRectangle
    ConvertTo-Json -InputObject @{x=($box.Left+$box.Width/2);y=($box.Top+$box.Height/2)} -Compress
}
'''


def uia(script, hwnd, action, name=""):
    """通过系统辅助功能读取任务栏或点击实际按钮，所有错误都令验证失败。"""
    quote = lambda value: "'" + str(value).replace("'", "''") + "'"
    command = "& {\n" + script.read_text(encoding="utf-8-sig") + "\n} -Hwnd " + str(hwnd) + " -Action " + quote(action) + " -Name " + quote(name)
    result = subprocess.run(
        ["powershell.exe", "-NoProfile", "-Command", command],
        capture_output=True, timeout=25,
    )
    if result.returncode:
        raise AssertionError(result.stderr.decode("utf-8", "replace"))
    return json.loads(result.stdout.decode("utf-8-sig"))


def menu_action(script, hwnd, name):
    """向本次副本的 WebView 投递鼠标消息，实际打开并点击 HTML 右键菜单。"""
    bounds = placement(hwnd)
    user32.SetWindowPos(hwnd, wintypes.HWND(-1), 0, 0, 0, 0, 0x0003)
    user32.SetForegroundWindow(hwnd)
    time.sleep(1.2)  # 原生窗口可见早于 WebView 完成加载和绑定事件。
    point = POINT(bounds["x"] + bounds["width"] * 2 // 3, bounds["y"] + min(180, bounds["height"] - 70))
    webview = user32.WindowFromPoint(point)
    assert webview == hwnd or user32.IsChild(hwnd, webview), "右键命中其他窗口，停止操作"
    user32.ScreenToClient(webview, ctypes.byref(point))
    coordinates = (point.y & 0xFFFF) << 16 | (point.x & 0xFFFF)
    user32.PostMessageW(webview, 0x0204, 2, coordinates)
    time.sleep(0.05)
    user32.PostMessageW(webview, 0x0205, 0, coordinates)
    time.sleep(0.6)
    center = uia(script, webview, "locate", name)
    point = POINT(round(center["x"]), round(center["y"]))
    user32.ScreenToClient(webview, ctypes.byref(point))
    coordinates = (point.y & 0xFFFF) << 16 | (point.x & 0xFFFF)
    user32.PostMessageW(webview, 0x0201, 1, coordinates)
    time.sleep(0.05)
    user32.PostMessageW(webview, 0x0202, 0, coordinates)


def main():
    """验证真正的 release 窗口与菜单，并重启副本核对位置恢复。"""
    parser = argparse.ArgumentParser()
    parser.add_argument("--exe", required=True)
    args = parser.parse_args()
    source = Path(args.exe).resolve()
    if not source.is_file():
        raise SystemExit(f"exe 不存在：{source}")
    scratch = Path(tempfile.mkdtemp(prefix="magpie-hud-window-"))
    appdata = scratch / "appdata"
    profile = scratch / "profile"
    (appdata / "magpie-hud").mkdir(parents=True)
    profile.mkdir()
    copied = scratch / source.name
    shutil.copy2(source, copied)
    script = scratch / "window-uia.ps1"
    script.write_text(UIA_SCRIPT, encoding="utf-8-sig")
    config_path = appdata / "magpie-hud" / "config.json"
    initial = {"x": 80, "y": 90, "width": 760, "height": 430}
    config_path.write_text(json.dumps({"usageJsonlPath": str(scratch / "usage.jsonl"), "magpieHost": "127.0.0.1:1", "window": initial, "alwaysOnTop": False}), encoding="utf-8")
    env = os.environ.copy()
    env.update(APPDATA=str(appdata), USERPROFILE=str(profile), LOCALAPPDATA=str(scratch / "local"), WEBVIEW2_USER_DATA_FOLDER=str(scratch / "webview"))
    process = subprocess.Popen([str(copied)], cwd=scratch, env=env)
    report = {"passed": False, "scratch": str(scratch), "pid": process.pid}
    try:
        hwnd = wait_for(lambda: main_window(process.pid), 30, "窗口未创建")
        wait_for(lambda: user32.IsWindowVisible(hwnd), 15, "窗口未显示")
        scale = (user32.GetDpiForWindow(hwnd) or 96) / 96
        restored = placement(hwnd)
        assert all(abs(restored[key] / scale - value) <= 2 for key, value in initial.items()), restored
        report["restored"] = restored
        title = ctypes.create_unicode_buffer(160)
        user32.GetWindowTextW(hwnd, title, 160)
        names = uia(script, hwnd, "taskbar")
        assert not any(title.value in name for name in names), names
        report["taskbarButtonAbsentWhileVisible"] = True
        moved = {"x": restored["x"] + 47, "y": restored["y"] + 29, "width": round(812 * scale), "height": round(456 * scale)}
        if not user32.SetWindowPos(hwnd, None, moved["x"], moved["y"], moved["width"], moved["height"], 0x0014):
            raise ctypes.WinError(ctypes.get_last_error())

        def moved_saved():
            saved = read_window(config_path)
            return saved if saved and all(abs(saved[key] * scale - value) <= 2 for key, value in moved.items()) else None

        saved = wait_for(moved_saved, 5, "移动缩放后未保存正常位置")
        report["movedAndSaved"] = saved
        # 不等 250ms 写盘，立即最小化仍须保存刚移动到的位置。
        moved["x"] += 23
        moved["y"] += 17
        user32.SetWindowPos(hwnd, None, moved["x"], moved["y"], moved["width"], moved["height"], 0x0014)
        user32.ShowWindow(hwnd, SW_MINIMIZE)
        wait_for(lambda: not user32.IsWindowVisible(hwnd) and not user32.IsIconic(hwnd), 5, "最小化后未隐藏到托盘")
        saved = wait_for(moved_saved, 5, "移动后立即最小化丢失了最后正常位置")
        report["minimizeHidesAndKeepsNormalPosition"] = saved
        close_process(process)
        process = subprocess.Popen([str(copied)], cwd=scratch, env=env)
        hwnd = wait_for(lambda: main_window(process.pid), 30, "重启后窗口未创建")
        wait_for(lambda: user32.IsWindowVisible(hwnd), 15, "重启后窗口未显示")
        restored = placement(hwnd)
        assert all(abs(restored[key] - value) <= 2 for key, value in moved.items()), restored
        report["restoredAfterRestart"] = restored
        # 最后一次移动之后立即关闭，关闭路径必须同步保存。
        moved["x"] += 19
        user32.SetWindowPos(hwnd, None, moved["x"], moved["y"], moved["width"], moved["height"], 0x0014)
        close_process(process)
        assert moved_saved(), read_window(config_path)
        report["immediateCloseKeepsLastPosition"] = True
        process = subprocess.Popen([str(copied)], cwd=scratch, env=env)
        hwnd = wait_for(lambda: main_window(process.pid), 30, "再次重启后窗口未创建")
        wait_for(lambda: user32.IsWindowVisible(hwnd), 15, "再次重启后窗口未显示")
        menu_action(script, hwnd, "初始化位置")
        info = MONITORINFO(cbSize=ctypes.sizeof(MONITORINFO))
        if not user32.GetMonitorInfoW(user32.MonitorFromWindow(hwnd, 2), ctypes.byref(info)):
            raise ctypes.WinError(ctypes.get_last_error())
        width = min(round(1120 * scale), info.rcWork.right - info.rcWork.left)
        height = min(round(400 * scale), info.rcWork.bottom - info.rcWork.top)
        expected = {"x": info.rcWork.left + (info.rcWork.right - info.rcWork.left - width) // 2,
                    "y": info.rcWork.top + (info.rcWork.bottom - info.rcWork.top - height) // 2,
                    "width": width, "height": height}
        wait_for(lambda: all(abs(placement(hwnd)[key] - value) <= 2 for key, value in expected.items()), 5, "右键初始化未恢复默认尺寸并居中")
        report["contextMenuReset"] = placement(hwnd)
        menu_action(script, hwnd, "隐藏到托盘")
        wait_for(lambda: not user32.IsWindowVisible(hwnd), 5, "右键隐藏未生效")
        report["contextMenuHide"] = True
        report["passed"] = True
        print(json.dumps(report, ensure_ascii=False))
    finally:
        close_process(process)


if __name__ == "__main__":
    configure_signatures()
    main()
