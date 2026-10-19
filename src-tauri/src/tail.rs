//! usage.jsonl 的有界尾部读取。
//!
//! 输入为外部日志路径；只读最后 64KB，不跟随文件增长做全量扫描。
//! 文件不存在、权限失败和读取失败分别返回状态，不写入目标文件。

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// 尾部读取上限，与需求规定的单次 64KB 一致。
pub const TAIL_LIMIT: u64 = 64 * 1024;

/// 一次尾部读取结果。
#[derive(Debug, PartialEq, Eq)]
pub enum Tail {
    /// 文件不存在。
    Missing,
    /// 文件存在但没有完整记录。
    Empty,
    /// 成功读到的尾部文本，可能包含被边界截断的首行。
    Ready(String),
    /// 读取或定位失败。
    Error(String),
}

/// 只读打开文件并读取末尾最多 64KB。
///
/// 小于上限时返回全文；恰好以换行结尾的文本可由解析器保留最后一条完整记录。
pub fn read_tail(path: &Path) -> Tail {
    if !path.exists() { return Tail::Missing; }
    let mut file = match File::open(path) { Ok(file) => file, Err(error) => return Tail::Error(error.to_string()) };
    let length = match file.seek(SeekFrom::End(0)) { Ok(length) => length, Err(error) => return Tail::Error(error.to_string()) };
    let start = length.saturating_sub(TAIL_LIMIT);
    if file.seek(SeekFrom::Start(start)).is_err() { return Tail::Error("无法定位日志尾部".to_string()); }
    let mut bytes = Vec::new();
    if file.take(TAIL_LIMIT).read_to_end(&mut bytes).is_err() { return Tail::Error("无法读取日志尾部".to_string()); }
    if bytes.is_empty() { Tail::Empty } else { Tail::Ready(String::from_utf8_lossy(&bytes).into_owned()) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// 验证大文件只取末尾 64KB，且保留尾部最后的完整标记。
    #[test]
    fn reads_only_tail_limit() {
        let path = std::env::temp_dir().join(format!("magpie-hud-tail-{}.jsonl", std::process::id()));
        let mut file = File::create(&path).unwrap();
        file.write_all(&vec![b'x'; TAIL_LIMIT as usize + 100]).unwrap();
        file.write_all(b"\nTAIL-MARKER\n").unwrap();
        let Tail::Ready(text) = read_tail(&path) else { panic!("expected tail"); };
        assert!(text.len() <= TAIL_LIMIT as usize);
        assert!(text.contains("TAIL-MARKER"));
        std::fs::remove_file(path).unwrap();
    }
}
