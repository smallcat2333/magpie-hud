//! 本机目录解析。与桌面配置使用同一规则，供标题扫描和未注入缓存时的兼容回退。
//! 服务端注入缓存目录后不会走到桌面配置路径。

use std::path::PathBuf;

pub fn directories_base() -> PathBuf {
    #[cfg(windows)]
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"));
    #[cfg(not(windows))]
    let home = std::env::var_os("HOME");
    home.map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn config_path() -> PathBuf {
    #[cfg(target_os = "macos")]
    let base = directories_base()
        .join("Library")
        .join("Application Support");
    #[cfg(not(target_os = "macos"))]
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(directories_base);
    base.join("magpie-hud").join("config.json")
}
