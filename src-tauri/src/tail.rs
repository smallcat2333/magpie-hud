//! 只读增量 JSONL 游标。启动流式扫描，后续每次只消费新增字节，单块最多 64KB。
//! 保留未写完的末行；文件轮转、截断或覆盖后重建当日账本，不重复累计。

use crate::usage::UsageLedger;
use chrono::{Local, NaiveDate};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

const CHUNK_SIZE: usize = 64 * 1024;
const ANCHOR_SIZE: u64 = 256;

/// 游标只保存文件边界信息和一条未完成的行，不缓存整个原始日志。
#[derive(Default)]
pub struct LogCursor {
    offset: u64,
    pending: Vec<u8>,
    anchor: Vec<u8>,
    created: Option<std::time::SystemTime>,
    skipping_long_line: bool,
    pub missing: bool,
    day: Option<NaiveDate>,
}

impl LogCursor {
    /// 处理文件的新完整行；返回是否观察到字节、日期或文件存在状态变化。
    pub fn update(&mut self, path: &Path, ledger: &mut UsageLedger) -> Result<bool, String> {
        let day = Local::now().date_naive();
        let day_changed = self.day != Some(day);
        if day_changed {
            ledger.change_day(day);
            self.offset = 0;
            self.pending.clear();
            self.anchor.clear();
            self.skipping_long_line = false;
            self.day = Some(day);
        }
        let mut file = match File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let changed = !self.missing || day_changed;
                self.missing = true;
                return Ok(changed);
            }
            Err(error) => return Err(format!("无法读取日志：{error}")),
        };
        let metadata = file.metadata().map_err(|e| e.to_string())?;
        let created = metadata.created().ok();
        let length = metadata.len();
        let mut replaced = self.offset > length || (self.offset > 0 && self.created != created);
        if self.offset > 0 && !replaced && !self.anchor.is_empty() {
            file.seek(SeekFrom::Start(self.offset - self.anchor.len() as u64))
                .map_err(|e| e.to_string())?;
            let mut anchor = vec![0; self.anchor.len()];
            replaced = file.read_exact(&mut anchor).is_err() || anchor != self.anchor;
        }
        if replaced {
            ledger.reset();
            self.offset = 0;
            self.pending.clear();
            self.anchor.clear();
            self.skipping_long_line = false;
        }
        let changed = replaced || day_changed || self.missing || length != self.offset;
        self.missing = false;
        self.created = created;
        file.seek(SeekFrom::Start(self.offset))
            .map_err(|e| e.to_string())?;
        let mut remaining = length - self.offset;
        let mut buffer = vec![0; CHUNK_SIZE];
        while remaining > 0 {
            let amount = remaining.min(CHUNK_SIZE as u64) as usize;
            let count = file
                .read(&mut buffer[..amount])
                .map_err(|e| e.to_string())?;
            if count == 0 {
                break;
            }
            self.feed(&buffer[..count], ledger);
            self.offset += count as u64;
            remaining -= count as u64;
        }
        let size = self.offset.min(ANCHOR_SIZE) as usize;
        file.seek(SeekFrom::Start(self.offset - size as u64))
            .map_err(|e| e.to_string())?;
        self.anchor.resize(size, 0);
        file.read_exact(&mut self.anchor)
            .map_err(|e| e.to_string())?;
        Ok(changed)
    }

    /// 分块拆行，半行留给下一次读取；单行超过 64KB 时跳过并报告坏行。
    fn feed(&mut self, bytes: &[u8], ledger: &mut UsageLedger) {
        for segment in bytes.split_inclusive(|byte| *byte == b'\n') {
            let complete = segment.last() == Some(&b'\n');
            if !self.skipping_long_line {
                if self.pending.len() + segment.len() > CHUNK_SIZE {
                    self.pending.clear();
                    self.skipping_long_line = true;
                    ledger.ingest(b"invalid oversized jsonl record");
                } else {
                    self.pending.extend_from_slice(segment);
                }
            }
            if complete {
                if !self.skipping_long_line {
                    ledger.ingest(&self.pending);
                }
                self.pending.clear();
                self.skipping_long_line = false;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write;

    /// 使用独立目录验证半行跨写、重复事件、截断和替换，不写真实日志。
    #[test]
    fn incrementally_reads_partial_and_replaced_files() {
        let directory =
            std::env::temp_dir().join(format!("magpie-hud-stream-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("usage.jsonl");
        let line = json!({"route_id":1,"t":Local::now().to_rfc3339(),"session":"one","status":200,"in":10,"out":20,"cache_read":30}).to_string();
        let mut ledger = UsageLedger::new(Local::now().date_naive());
        let mut cursor = LogCursor::default();
        let half = line.len() / 2;
        std::fs::write(&path, &line[..half]).unwrap();
        cursor.update(&path, &mut ledger).unwrap();
        assert_eq!(ledger.snapshot("h", "ready", None).daily.requests, 0);
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(file, "{}", &line[half..]).unwrap();
        drop(file);
        cursor.update(&path, &mut ledger).unwrap();
        assert_eq!(ledger.snapshot("h", "ready", None).daily.tokens, 60);
        assert!(!cursor.update(&path, &mut ledger).unwrap());
        std::fs::write(&path, b"").unwrap();
        cursor.update(&path, &mut ledger).unwrap();
        assert_eq!(ledger.snapshot("h", "ready", None).daily.requests, 0);
        std::fs::write(&path, format!("{line}\n")).unwrap();
        cursor.update(&path, &mut ledger).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert!(cursor.update(&path, &mut ledger).unwrap());
        assert!(cursor.missing);
        std::fs::write(&path, format!("{line}\n")).unwrap();
        cursor.update(&path, &mut ledger).unwrap();
        assert_eq!(ledger.snapshot("h", "ready", None).daily.requests, 1);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
