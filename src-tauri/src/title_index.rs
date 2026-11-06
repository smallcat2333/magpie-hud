//! Codex 名称索引；只读取 id/thread_name 元数据，正文行缓冲与每轮 I/O 均有预算。
use std::{
    collections::HashMap,
    fs::{File, Metadata},
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::SystemTime,
};

const SCAN_BYTES: u64 = 1024 * 1024;
const LINE_BYTES: usize = 64 * 1024;
const ANCHOR_BYTES: u64 = 256;

#[derive(Default)]
pub(crate) struct CodexTitles {
    path: PathBuf,
    modified: Option<SystemTime>,
    created: Option<SystemTime>,
    observed_len: u64,
    offset: u64,
    anchor: Vec<u8>,
    pending: Vec<u8>,
    skipping: bool,
    names: HashMap<String, String>,
    catching_up: bool,
    #[cfg(test)]
    pub bytes_read: u64,
}

impl CodexTitles {
    pub fn catching_up(&self) -> bool { self.catching_up }

    pub fn scan(&mut self, path: &Path) -> HashMap<String, String> {
        if self.path != path {
            self.reset(path);
        }
        match std::fs::metadata(path) {
            Ok(meta) if meta.is_file() => {
                // 未变化且追平时完全不读取内容；半行由 pending 保留，等追加后继续。
                if self.modified != meta.modified().ok() || self.created != meta.created().ok()
                    || self.observed_len != meta.len() || self.catching_up {
                    let _ = self.advance(path, &meta);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => self.reset(path),
            _ => {} // 暂时不可读时保留最后有效名称与游标，下次重试。
        }
        self.names.clone()
    }

    fn reset(&mut self, path: &Path) {
        #[cfg(test)]
        let bytes_read = self.bytes_read;
        *self = Self { path: path.into(), ..Self::default() };
        #[cfg(test)] { self.bytes_read = bytes_read; }
    }

    fn anchor_at(&mut self, file: &mut File, offset: u64) -> std::io::Result<Vec<u8>> {
        let count = offset.min(ANCHOR_BYTES);
        file.seek(SeekFrom::Start(offset - count))?;
        let mut anchor = vec![0; count as usize];
        file.read_exact(&mut anchor)?;
        #[cfg(test)] { self.bytes_read += count; }
        Ok(anchor)
    }

    fn advance(&mut self, path: &Path, meta: &Metadata) -> std::io::Result<()> {
        let mut file = File::open(path)?;
        let replaced = self.created != meta.created().ok()
            || meta.len() < self.offset
            || (meta.len() <= self.observed_len && self.modified != meta.modified().ok())
            || (self.offset > 0 && self.anchor_at(&mut file, self.offset)? != self.anchor);
        if replaced { self.reset(path); }
        file.seek(SeekFrom::Start(self.offset))?;
        let budget = meta.len().saturating_sub(self.offset).min(SCAN_BYTES);
        let mut bytes = Vec::with_capacity(budget as usize);
        (&mut file).take(budget).read_to_end(&mut bytes)?;
        #[cfg(test)] { self.bytes_read += bytes.len() as u64; }
        self.feed(&bytes);
        self.offset += bytes.len() as u64;
        self.anchor = self.anchor_at(&mut file, self.offset)?;
        self.created = meta.created().ok();
        self.modified = meta.modified().ok();
        self.observed_len = meta.len();
        self.catching_up = self.offset < meta.len();
        Ok(())
    }

    fn feed(&mut self, bytes: &[u8]) {
        for segment in bytes.split_inclusive(|byte| *byte == b'\n') {
            if !self.skipping {
                if self.pending.len() + segment.len() <= LINE_BYTES {
                    self.pending.extend_from_slice(segment);
                } else {
                    self.pending.clear();
                    self.skipping = true;
                }
            }
            if segment.last() == Some(&b'\n') {
                if !self.skipping {
                    if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&self.pending) {
                        if let (Some(id), Some(name)) = (value["id"].as_str(), value["thread_name"].as_str()) {
                            self.names.insert(id.into(), name.into());
                        }
                    }
                }
                self.pending.clear();
                self.skipping = false;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Write, time::Instant};

    fn drain(index: &mut CodexTitles, path: &Path) -> HashMap<String, String> {
        let mut names = index.scan(path);
        while index.catching_up() { names = index.scan(path); }
        names
    }

    #[test]
    fn incremental_index_handles_partial_bad_rows_rewrites_and_replacement() {
        let directory = std::env::temp_dir().join(format!("hud-title-index-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("session_index.jsonl");
        let first = "{\"id\":\"sample\",\"thread_name\":\"First\"}\n";
        std::fs::write(&path, first).unwrap();
        let mut index = CodexTitles::default();
        assert_eq!(index.scan(&path)["sample"], "First");
        index.bytes_read = 0;
        assert_eq!(index.scan(&path)["sample"], "First");
        assert_eq!(index.bytes_read, 0);
        let mut file = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        let partial = "{\"id\":\"sample\",\"thread_name\":\"Second";
        write!(file, "{partial}").unwrap();
        assert_eq!(index.scan(&path)["sample"], "First");
        assert!(!index.catching_up());
        assert!(index.pending.len() <= LINE_BYTES);
        writeln!(file, "\"}}\ninvalid\n{{\"id\":\"other\",\"thread_name\":\"Other\"}}").unwrap();
        index.bytes_read = 0;
        let names = index.scan(&path);
        assert_eq!(names["sample"], "Second");
        assert_eq!(names["other"], "Other");
        assert!(index.bytes_read <= 512 + 100);
        drop(file);
        // 等长重写仍保留同一个文件句柄身份，强制不同修改时间使检查不依赖时钟精度。
        let same_length = first.replace("First", "Third");
        std::fs::write(&path, first).unwrap();
        index.scan(&path);
        index.modified = Some(SystemTime::UNIX_EPOCH);
        std::fs::write(&path, same_length).unwrap();
        assert_eq!(index.scan(&path)["sample"], "Third");
        assert!(!index.names.contains_key("other"));
        std::fs::write(&path, "").unwrap();
        assert!(index.scan(&path).is_empty());
        std::fs::write(&path, first).unwrap();
        assert_eq!(index.scan(&path)["sample"], "First");
        let replacement = directory.join("replacement");
        std::fs::write(&replacement, "{\"id\":\"new\",\"thread_name\":\"New title\"}\n").unwrap();
        std::fs::remove_file(&path).unwrap();
        std::fs::rename(&replacement, &path).unwrap();
        let replaced = index.scan(&path);
        assert_eq!(replaced.len(), 1);
        assert_eq!(replaced["new"], "New title");
        std::fs::remove_file(&path).unwrap();
        assert!(index.scan(&path).is_empty());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn scan_budget_bounds_long_rows_and_keeps_newest_complete_title() {
        let directory = std::env::temp_dir().join(format!("hud-title-budget-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("session_index.jsonl");
        let mut file = File::create(&path).unwrap();
        writeln!(file, "{}", "x".repeat(SCAN_BYTES as usize * 2 + 73)).unwrap();
        writeln!(file, "{{\"id\":\"sample\",\"thread_name\":\"First\"}}").unwrap();
        writeln!(file, "{{\"id\":\"sample\",\"thread_name\":\"Last\"}}").unwrap();
        drop(file);
        let mut index = CodexTitles::default();
        assert!(index.scan(&path).is_empty());
        assert!(index.catching_up());
        assert_eq!(index.offset, SCAN_BYTES);
        assert!(index.pending.len() <= LINE_BYTES);
        assert!(index.bytes_read <= SCAN_BYTES + 512);
        index.bytes_read = 0;
        index.scan(&path);
        assert!(index.catching_up());
        assert!(index.bytes_read <= SCAN_BYTES + 512);
        assert_eq!(drain(&mut index, &path)["sample"], "Last");
        let mut file = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(file, "{{\"id\":\"sample\",\"thread_name\":\"\"}}").unwrap();
        drop(file);
        assert_eq!(index.scan(&path)["sample"], "");
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    #[ignore]
    fn codex_title_index_benchmark() {
        let directory = std::env::temp_dir().join(format!("hud-title-perf-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("session_index.jsonl");
        let mut file = std::io::BufWriter::new(std::fs::File::create(&path).unwrap());
        for row in 0..100_000 {
            writeln!(file, "{}", serde_json::json!({"id":format!("sample-{}", row % 1000), "thread_name":format!("Sample title {row}"), "updated_at":"2026-11-06T12:00:00Z"})).unwrap();
        }
        file.flush().unwrap();
        drop(file);
        let bytes = std::fs::metadata(&path).unwrap().len();
        let mut index = CodexTitles::default();
        let expected = drain(&mut index, &path);
        assert_eq!(expected.len(), 1000);
        let canonical = serde_json::to_string(&serde_json::to_value(&expected).unwrap()).unwrap();
        let fingerprint = canonical.bytes().fold(0xcbf29ce484222325u64, |hash, byte| (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3));
        index.bytes_read = 0;
        let mut samples = Vec::new();
        for _ in 0..3 {
            let start = Instant::now();
            for _ in 0..5 { assert_eq!(index.scan(&path), expected); }
            samples.push(start.elapsed().as_secs_f64() * 1000.0 / 5.0);
        }
        samples.sort_by(f64::total_cmp);
        eprintln!("TITLE_PERF rows=100000 sessions=1000 bytes={bytes} samples_ms={samples:?} median_ms={:.3} idle_bytes={} fingerprint={fingerprint:016x}", samples[1], index.bytes_read);
        assert_eq!(index.bytes_read, 0);
        let mut file = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        let append = "{\"id\":\"sample-0\",\"thread_name\":\"Renamed\"}\n";
        write!(file, "{append}").unwrap();
        drop(file);
        let start = Instant::now();
        assert_eq!(index.scan(&path)["sample-0"], "Renamed");
        eprintln!("TITLE_PERF append_bytes={} read_bytes={} append_ms={:.3}", append.len(), index.bytes_read, start.elapsed().as_secs_f64()*1000.0);
        assert!(index.bytes_read <= append.len() as u64 + 512);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
