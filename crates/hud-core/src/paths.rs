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

/// 本机 IANA 时区名。优先取非空且为合法 IANA 名称的 `TZ` 环境变量
/// （服务进程可在启动时按配置注入）；否则回退到系统检测，再退到 UTC。
pub fn local_timezone() -> String {
    if let Some(tz) = std::env::var("TZ").ok().map(|value| value.trim().to_string()) {
        if is_iana_name(&tz) {
            return tz;
        }
    }
    iana_time_zone::get_timezone().unwrap_or_else(|_| "UTC".to_string())
}

/// 宽松校验：`Area/Location` 形式，只含字母数字与 `_+-/`，不以 `/` 开头结尾，
/// 且至少含一个 `/`（如 `Asia/Shanghai`）。拒绝 `CST8` 这类 POSIX 偏移写法。
fn is_iana_name(value: &str) -> bool {
    if value.len() < 3 || !value.contains('/') {
        return false;
    }
    if value.starts_with('/') || value.ends_with('/') || value.contains("//") {
        return false;
    }
    value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '+' | '/'))
}
