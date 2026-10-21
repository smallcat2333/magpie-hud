//! Windows 可执行入口；业务与测试位于库 crate。

#![windows_subsystem = "windows"]

fn main() {
    magpie_hud_lib::run();
}
