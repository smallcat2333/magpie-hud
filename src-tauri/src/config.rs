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
    /// 仅供界面显示，不用于发起网络连接。
    pub magpie_host: String,
    /// 上次正常退出前的窗口位置；首次运行为空。
    pub window: Option<WindowPlacement>,
    /// 是否置顶，默认 true。
    pub always_on_top: bool,
}

impl Default for AppConfig {
    /// 返回首次运行配置：默认路径、本机显示地址、无保存位置、置顶。
    fn default() -> Self {
        Self { usage_jsonl_path: String::new(), magpie_host: "127.0.0.1:3425".to_string(), window: None, always_on_top: true }
    }
}

/// 解析当前用户的默认日志路径 `%USERPROFILE%\.config\magpie\usage.jsonl`。
pub fn default_usage_path() -> PathBuf {
    directories_base().join(".config").join("magpie").join("usage.jsonl")
}

/// 解析 `%APPDATA%\magpie-hud\config.json`。
pub fn config_path() -> PathBuf {
    std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(directories_base).join("magpie-hud").join("config.json")
}

/// 获取用户主目录；环境变量缺失时退回当前目录，便于测试显式传入其他配置路径。
fn directories_base() -> PathBuf {
    std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
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
