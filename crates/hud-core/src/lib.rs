//! 桌面悬浮窗与无界面服务共用的只读采集核心。不依赖 Tauri，不写桌面配置目录。

pub mod detail_view;
pub mod details;
pub mod health;
pub mod history;
pub mod hourly;
pub mod http;
pub mod live;
pub mod paths;
pub mod query;
pub mod reader;
pub mod service;
pub mod tail;
pub mod telemetry;
pub mod title_index;
pub mod usage;

pub use service::{Service, ServiceConfig};
