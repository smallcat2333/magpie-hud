//! Windows 可执行入口；业务与测试位于库 crate。

#![windows_subsystem = "windows"]

fn main() {
    // 第二个实例只把已运行的窗口置前，不重复注册托盘、文件监视和实时轮询。
    if magpie_hud_lib::raise_existing_instance() {
        return;
    }
    magpie_hud_lib::run();
}
