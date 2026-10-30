//! 悬浮窗本地配置。
//!
//! 配置只保存路径、显示地址、窗口位置和置顶开关，不保存 usage 记录或凭据。
//! 外部配置损坏时返回明确错误，由调用方显示并恢复默认可操作窗口。

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// 逻辑像素表示的窗口位置与尺寸。
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct WindowPlacement {
    /// 窗口左边，单位为逻辑像素。
    pub x: f64,
    /// 窗口顶边，单位为逻辑像素。
    pub y: f64,
    /// 窗口宽度，单位为逻辑像素。
    pub width: f64,
    /// 窗口高度，单位为逻辑像素。
    pub height: f64,
}

/// 可持久化的全部产品配置。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppConfig {
    /// 为空时使用当前用户主目录下的默认 usage.jsonl。
    pub usage_jsonl_path: String,
    /// 本机 Magpie 网关地址，用于显示和只读实时路由；仅允许回环 HTTP。
    pub magpie_host: String,
    /// 上次可见的正常窗口位置与尺寸；首次运行为空。
    /// 只保存稳定矩形，不保存最小化哨兵坐标、零尺寸或隐藏态。
    pub window: Option<WindowPlacement>,
    /// 用量详情最后的正常位置与尺寸；旧配置缺少此项时首次居中。
    #[serde(default)]
    pub details_window: Option<WindowPlacement>,
    /// 是否置顶，默认 true。
    pub always_on_top: bool,
}

impl Default for AppConfig {
    /// 返回首次运行配置：默认路径、本机显示地址、无保存位置、置顶。
    fn default() -> Self {
        Self { usage_jsonl_path: String::new(), magpie_host: "127.0.0.1:3425".to_string(), window: None, details_window: None, always_on_top: true }
    }
}

/// 解析当前用户的默认日志路径 `%USERPROFILE%\.config\magpie\usage.jsonl`。
pub fn default_usage_path() -> PathBuf {
    directories_base().join(".config").join("magpie").join("usage.jsonl")
}

/// Windows 使用 APPDATA；macOS 使用用户的 Application Support。
pub fn config_path() -> PathBuf {
    #[cfg(target_os = "macos")]
    let base = directories_base().join("Library").join("Application Support");
    #[cfg(not(target_os = "macos"))]
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(directories_base);
    base.join("magpie-hud").join("config.json")
}

/// 获取用户主目录；不因某个平台没有 Windows 环境变量而中断后台线程。
pub(crate) fn directories_base() -> PathBuf {
    #[cfg(windows)]
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"));
    #[cfg(not(windows))]
    let home = std::env::var_os("HOME");
    home.map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
}

/// 读取配置。
///
/// 文件不存在返回默认值；JSON 损坏或字段类型错误返回 `Err`，不在这里静默覆盖。
pub fn load_config(path: &Path) -> Result<AppConfig, String> {
    if !path.exists() { return Ok(AppConfig::default()); }
    let text = fs::read_to_string(path).map_err(|error| format!("无法读取配置：{error}"))?;
    serde_json::from_str(&text).map_err(|error| format!("配置格式无效：{error}"))
}

/// 原子替换配置文件。
///
/// 先写入同目录临时文件，再替换目标，避免程序中断留下半份 JSON。
pub fn save_config(path: &Path, config: &AppConfig) -> Result<(), String> {
    if let Some(parent) = path.parent() { fs::create_dir_all(parent).map_err(|error| error.to_string())?; }
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(config).map_err(|error| error.to_string())?).map_err(|error| error.to_string())?;
    fs::rename(&temporary, path).map_err(|error| error.to_string())
}

/// 解析实际监视路径：配置为空使用默认路径，相对路径按当前工作目录解析。
pub fn usage_path(config: &AppConfig) -> PathBuf {
    if config.usage_jsonl_path.trim().is_empty() { default_usage_path() } else { PathBuf::from(&config.usage_jsonl_path) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_config_uses_application_support() {
        assert_eq!(config_path(), directories_base().join("Library/Application Support/magpie-hud/config.json"));
    }

    #[test]
    fn default_usage_stays_in_magpie_home() {
        assert_eq!(default_usage_path(), directories_base().join(".config/magpie/usage.jsonl"));
    }

    #[test]
    fn details_geometry_roundtrips_and_old_config_still_loads() {
        let old=r#"{"usageJsonlPath":"","magpieHost":"127.0.0.1:3425","window":null,"alwaysOnTop":true}"#;
        let mut config:AppConfig=serde_json::from_str(old).unwrap();
        assert_eq!(config.details_window,None);
        config.details_window=Some(WindowPlacement{x:-120.0,y:80.0,width:960.0,height:720.0});
        let restored:AppConfig=serde_json::from_slice(&serde_json::to_vec(&config).unwrap()).unwrap();
        assert_eq!(restored.details_window,config.details_window);
        assert_eq!(restored.window,None);
    }

    /// 空路径回到默认日志，空白地址与关闭置顶必须原样保存。
    #[test]
    fn preserves_empty_path_blank_host_and_pin_choice() {
        let directory = std::env::temp_dir().join(format!("magpie-hud-config-fields-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        let path = directory.join("config.json");
        let mut config = AppConfig::default();
        config.usage_jsonl_path = "   ".into();
        config.magpie_host = "   ".into();
        config.always_on_top = false;
        save_config(&path, &config).unwrap();
        let loaded = load_config(&path).unwrap();
        assert_eq!(loaded.magpie_host, "   ");
        assert!(!loaded.always_on_top);
        assert_eq!(usage_path(&loaded), default_usage_path());
        fs::remove_dir_all(directory).unwrap();
    }

    /// 验证默认配置、损坏配置报错和自定义路径往返。
    #[test]
    fn roundtrips_without_hiding_invalid_json() {
        let directory = std::env::temp_dir().join(format!("magpie-hud-config-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        let path = directory.join("config.json");
        assert!(load_config(&path).unwrap().always_on_top);
        let mut config = AppConfig::default();
        config.usage_jsonl_path = "sample.jsonl".to_string();
        save_config(&path, &config).unwrap();
        assert_eq!(load_config(&path).unwrap().usage_jsonl_path, "sample.jsonl");
        fs::write(&path, "{").unwrap();
        assert!(load_config(&path).is_err());
        fs::remove_dir_all(directory).unwrap();
    }
}
