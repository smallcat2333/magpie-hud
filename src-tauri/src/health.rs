//! 实时通道独立恢复；诊断日志只记录来源和故障/恢复时间，不保存原始响应或账号。
use crate::usage::RouteSnapshot;
use chrono::Local;
use serde::Serialize;
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{sync_channel, SyncSender};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Trace,
    Concurrency,
    Watcher,
    UsageLog,
}

impl Source {
    fn label(self) -> &'static str {
        match self {
            Self::Trace => "路由轨迹",
            Self::Concurrency => "活跃请求",
            Self::Watcher => "文件监听",
            Self::UsageLog => "用量日志",
        }
    }
}

struct Fault {
    message: String,
    since: String,
}

#[derive(Serialize)]
struct Event {
    at: String,
    source: Source,
    state: &'static str,
}

#[derive(Default)]
pub struct Health {
    faults: BTreeMap<Source, Fault>,
    log_path: Option<PathBuf>,
    sender: Option<SyncSender<Event>>,
}

impl Health {
    pub fn with_log(path: PathBuf) -> Self {
        let (sender, receiver) = sync_channel(128);
        let destination = path.clone();
        std::thread::spawn(move || {
            while let Ok(event) = receiver.recv() {
                if append_event(&destination, &event, 64 * 1024).is_err() {
                    eprintln!("Magpie HUD 诊断日志写入失败");
                }
            }
        });
        Self {
            log_path: Some(path),
            sender: Some(sender),
            ..Self::default()
        }
    }

    pub fn set(&mut self, source: Source, error: Option<String>) {
        let state = match error {
            Some(message) => {
                if let Some(fault) = self.faults.get_mut(&source) {
                    fault.message = message;
                    return;
                }
                self.faults.insert(
                    source,
                    Fault {
                        message,
                        since: Local::now().to_rfc3339(),
                    },
                );
                "failed"
            }
            None if self.faults.remove(&source).is_some() => "recovered",
            None => return,
        };
        if let Some(sender) = &self.sender {
            let _ = sender.try_send(Event {
                at: Local::now().to_rfc3339(),
                source,
                state,
            });
        }
    }

    pub fn apply(&self, view: &mut RouteSnapshot) {
        if self.faults.is_empty() {
            return;
        }
        let network = self.faults.contains_key(&Source::Trace)
            || self.faults.contains_key(&Source::Concurrency);
        let mut messages: Vec<String> = view.error.iter().cloned().collect();
        messages.extend(self.faults.iter().map(|(source, fault)| {
            format!(
                "{}异常（自 {}）：{}",
                source.label(),
                fault.since,
                fault.message
            )
        }));
        if let Some(path) = &self.log_path {
            messages.push(format!("诊断日志：{}", path.display()));
        }
        view.state = "warning".into();
        view.watch_label = if network {
            "实时连接异常 · 保留上次状态"
        } else if self.faults.contains_key(&Source::Watcher) {
            "文件监听异常 · 轮询读取"
        } else {
            "用量日志读取异常"
        }
        .into();
        view.error = Some(messages.join("；"));
        if network {
            for session in &mut view.sessions {
                for card in session.routes.iter_mut().chain(&mut session.subagents) {
                    if matches!(card.state.as_str(), "running" | "waiting") {
                        card.detail
                            .push_str(" · 实时连接异常，显示上次状态，可能已过期");
                    }
                }
            }
        }
    }
}

/// 单独线程写盘；当前和上一份各最多 64 KiB，不在 Runtime 锁内执行文件 I/O。
fn append_event(path: &Path, event: &Event, max_bytes: u64) -> std::io::Result<()> {
    let mut line = serde_json::to_vec(event)?;
    line.push(b'\n');
    if line.len() as u64 > max_bytes {
        return Err(std::io::Error::other("诊断事件超出上限"));
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    if fs::metadata(path).is_ok_and(|meta| meta.len().saturating_add(line.len() as u64) > max_bytes)
    {
        let previous = path.with_extension("jsonl.1");
        match fs::metadata(&previous) {
            Ok(_) => fs::remove_file(&previous)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        fs::rename(path, previous)?;
    }
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?
        .write_all(&line)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage::UsageLedger;

    #[test]
    fn channels_recover_independently_and_keep_first_failure_time() {
        let mut health = Health::default();
        health.set(Source::Trace, Some("first".into()));
        let since = health.faults[&Source::Trace].since.clone();
        health.set(Source::Trace, Some("second".into()));
        health.set(Source::Concurrency, Some("offline".into()));
        health.set(Source::Concurrency, None);
        health.set(Source::Watcher, Some("watcher".into()));
        assert_eq!(health.faults[&Source::Trace].since, since);
        let ledger = UsageLedger::new(Local::now().date_naive());
        let mut view = ledger.snapshot("fixture", "ready", Some("config error".into()));
        health.apply(&mut view);
        assert!(view.error.as_deref().unwrap().contains("config error"));
        assert!(view.error.as_deref().unwrap().contains("second"));
        health.set(Source::Trace, None);
        let mut view = ledger.snapshot("fixture", "ready", None);
        health.apply(&mut view);
        assert_eq!(view.watch_label, "文件监听异常 · 轮询读取");
        health.set(Source::Watcher, None);
        let mut view = ledger.snapshot("fixture", "ready", None);
        health.apply(&mut view);
        assert!(view.error.is_none());
    }

    #[test]
    fn diagnostic_events_are_bounded_and_never_include_raw_errors() {
        let path = std::env::temp_dir()
            .join(format!("hud-health-{}", std::process::id()))
            .join("diagnostics.jsonl");
        let (sender, receiver) = sync_channel(8);
        let mut health = Health {
            sender: Some(sender),
            ..Health::default()
        };
        health.set(Source::Trace, Some("PRIVATE-ACCOUNT-AND-RESPONSE".into()));
        health.set(Source::Trace, Some("PRIVATE-CHANGED".into()));
        health.set(Source::Trace, None);
        health.set(Source::Trace, None);
        let events: Vec<_> = receiver.try_iter().collect();
        assert_eq!(events.len(), 2, "相同故障不反复写日志");
        for _ in 0..50 {
            for event in &events {
                append_event(&path, event, 1024).unwrap();
            }
        }
        let previous = path.with_extension("jsonl.1");
        for file in [&path, &previous] {
            assert!(fs::metadata(file).unwrap().len() <= 1024);
            let text = fs::read_to_string(file).unwrap();
            assert!(!text.contains("PRIVATE"));
            for line in text.lines() {
                assert!(serde_json::from_str::<serde_json::Value>(line).is_ok());
            }
        }
        assert!(fs::read_to_string(&path).unwrap().contains("recovered"));
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
}
