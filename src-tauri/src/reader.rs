//! 日志游标、账本及统计底稿使用独立锁。持有此锁时不得取得 Runtime 锁。
use crate::{
    tail::LogCursor,
    usage::{RouteSnapshot, UsageLedger},
};
use chrono::NaiveDate;
use std::{path::Path, sync::Arc};

#[derive(PartialEq, Eq)]
struct SnapshotKey {
    ledger: (u64, Option<NaiveDate>),
    missing: bool,
    path: std::path::PathBuf,
    host: String,
    config_error: Option<String>,
}

pub(crate) struct ReadUpdate {
    pub base: Arc<RouteSnapshot>,
    pub error: Option<String>,
}

pub(crate) struct LogReader {
    pub ledger: UsageLedger,
    cursor: LogCursor,
    cached: Option<(SnapshotKey, Arc<RouteSnapshot>)>,
}

impl LogReader {
    pub fn new(day: NaiveDate) -> Self {
        Self {
            ledger: UsageLedger::new(day),
            cursor: LogCursor::default(),
            cached: None,
        }
    }

    /// Runtime 发布 reader 前已完成首次读取；日期与同锁内账本保持一致。
    pub fn date(&self) -> &str {
        &self.cached.as_ref().expect("日志 reader 尚未初始化").1.date
    }

    pub fn update(&mut self, path: &Path, host: &str, config_error: Option<String>) -> ReadUpdate {
        let error = self.cursor.update(path, &mut self.ledger).err();
        self.finish_update(path, host, config_error, error)
    }

    #[cfg(test)]
    fn update_for_day(
        &mut self,
        path: &Path,
        host: &str,
        config_error: Option<String>,
        day: NaiveDate,
    ) -> ReadUpdate {
        let error = self
            .cursor
            .update_for_day(path, &mut self.ledger, day)
            .err();
        self.finish_update(path, host, config_error, error)
    }

    fn finish_update(
        &mut self,
        path: &Path,
        host: &str,
        config_error: Option<String>,
        error: Option<String>,
    ) -> ReadUpdate {
        let key = SnapshotKey {
            ledger: self.ledger.snapshot_key(),
            missing: self.cursor.missing,
            path: path.into(),
            host: host.into(),
            config_error,
        };
        if self
            .cached
            .as_ref()
            .is_none_or(|(previous, _)| previous != &key)
        {
            let mut snapshot = self.ledger.snapshot(
                host,
                if key.missing { "missing" } else { "ready" },
                key.config_error.clone(),
            );
            if snapshot.sessions.is_empty() {
                snapshot.notes[1].text = path.display().to_string();
            }
            self.cached = Some((key, Arc::new(snapshot)));
        }
        ReadUpdate {
            base: Arc::clone(&self.cached.as_ref().unwrap().1),
            error,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write;

    #[test]
    fn cache_tracks_metadata_errors_rollover_and_replacements() {
        let directory = std::env::temp_dir().join(format!("hud-reader-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("usage.jsonl");
        let day = NaiveDate::from_ymd_opt(2026, 11, 6).unwrap();
        let row = json!({"route_id":1,"t":"2026-11-06T12:00:00","session":"sample","status":200,"in":10,"out":20});
        // 带本地偏移，避免测试依赖机器时区。
        let mut row = row;
        row["t"] = json!(format!(
            "2026-11-06T12:00:00{}",
            chrono::Local::now().format("%:z")
        ));
        let mut reader = LogReader::new(day);
        std::fs::write(&path, format!("{row}\n")).unwrap();
        let first = reader.update_for_day(&path, "h", None, day).base;
        assert_eq!(first.daily.requests, 1);
        let cached = reader.update_for_day(&path, "h", None, day).base;
        assert!(Arc::ptr_eq(&first, &cached));
        // 重复行及半行都不重建，完整坏行必须更新警告。
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(file, "{row}").unwrap();
        assert!(Arc::ptr_eq(
            &first,
            &reader.update_for_day(&path, "h", None, day).base
        ));
        write!(file, "invalid").unwrap();
        assert!(Arc::ptr_eq(
            &first,
            &reader.update_for_day(&path, "h", None, day).base
        ));
        writeln!(file).unwrap();
        let bad = reader.update_for_day(&path, "h", None, day).base;
        assert_eq!(bad.state, "warning");
        // 当日外的旧行仍能更新最早日期，即使 ledger revision 未增长。
        let mut old = row.clone();
        old["t"] = json!(format!(
            "2026-11-01T12:00:00{}",
            chrono::Local::now().format("%:z")
        ));
        writeln!(file, "{old}").unwrap();
        let older = reader.update_for_day(&path, "h", None, day).base;
        assert_eq!(older.earliest_date, "2026-11-01");
        assert_eq!(older.daily.requests, 1);
        let configured = reader
            .update_for_day(&path, "other", Some("config test".into()), day)
            .base;
        assert_eq!(configured.magpie_host, "other");
        assert_eq!(configured.error.as_deref(), Some("config test"));
        drop(file);
        std::fs::remove_file(&path).unwrap();
        let missing = reader.update_for_day(&path, "h", None, day).base;
        assert_eq!(missing.notes[0].kind, "文件");
        assert_eq!(missing.daily.requests, 1);
        std::fs::write(&path, "").unwrap();
        let reset = reader.update_for_day(&path, "h", None, day).base;
        assert_eq!(reset.daily.requests, 0);
        assert_eq!(reset.state, "waiting");
        std::fs::write(&path, format!("{row}\n")).unwrap();
        let recovered = reader.update_for_day(&path, "h", None, day).base;
        assert_eq!(recovered.daily.requests, 1);
        let tomorrow = reader
            .update_for_day(&path, "h", None, day.succ_opt().unwrap())
            .base;
        assert_eq!(tomorrow.date, "2026-11-07");
        assert_eq!(tomorrow.daily.requests, 0);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
