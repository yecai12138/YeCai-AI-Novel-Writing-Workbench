//! 原子写入。
//!
//! 写文件的顺序固定为：同目录临时文件 → `write_all` → `sync_all`（落盘）→ rename 覆盖目标。
//! 崩溃或断电只会留下一个临时文件，不会出现“写了一半的正文”。
//!
//! 临时文件统一带 [`TEMP_PREFIX`] 前缀，watcher 与前端都按这个前缀忽略，
//! 避免自己的写入把监听器刷爆。

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::models::StoryError;

/// 临时文件前缀。任何以它开头的文件名都不该被当成用户内容。
pub const TEMP_PREFIX: &str = ".storydesk-tmp-";

/// 原子写入 `content` 到 `path`（父目录不存在时自动创建）。
pub fn write_atomic(path: &Path, content: &[u8]) -> Result<(), StoryError> {
    let parent = path
        .parent()
        .ok_or_else(|| StoryError::invalid("path", "目标路径没有父目录"))?;

    if !parent.exists() {
        fs::create_dir_all(parent).map_err(|e| {
            StoryError::fs(parent.display().to_string(), format!("创建目录失败: {e}"))
        })?;
    }

    let tmp = temp_sibling(parent);

    // 1) 写临时文件并 fsync：这一步失败时目标文件还没被碰过
    let write_result = (|| -> std::io::Result<()> {
        let mut file = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        file.write_all(content)?;
        file.sync_all()?;
        Ok(())
    })();

    if let Err(e) = write_result {
        let _ = fs::remove_file(&tmp);
        return Err(StoryError::fs(
            path.display().to_string(),
            format!("写入临时文件失败: {e}"),
        ));
    }

    // 2) rename 覆盖：Windows 上 std 用 MoveFileEx(REPLACE_EXISTING)，同样是原子替换
    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(StoryError::fs(
            path.display().to_string(),
            format!("替换目标文件失败: {e}"),
        ));
    }

    // 3) 目录项本身也要落盘，否则崩溃后可能看不到 rename 的结果
    sync_dir(parent);
    Ok(())
}

/// 读文本文件：只接受 UTF-8（允许 BOM）。
///
/// 不做 `lossy` 解码——编码错了就明确报错，而不是悄悄把内容换成替换字符再写回去。
pub fn read_text(path: &Path) -> Result<String, StoryError> {
    let bytes = fs::read(path)
        .map_err(|e| StoryError::fs(path.display().to_string(), format!("读取失败: {e}")))?;
    decode_utf8(&bytes).map_err(|message| StoryError::fs(path.display().to_string(), message))
}

fn decode_utf8(bytes: &[u8]) -> Result<String, String> {
    let body = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    String::from_utf8(body.to_vec()).map_err(|e| {
        let offset = e.utf8_error().valid_up_to();
        format!("文件不是合法的 UTF-8 文本（第 {offset} 字节起解码失败）")
    })
}

fn temp_sibling(parent: &Path) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    parent.join(format!("{TEMP_PREFIX}{}-{nanos}.tmp", std::process::id()))
}

#[cfg(unix)]
fn sync_dir(dir: &Path) {
    if let Ok(handle) = std::fs::File::open(dir) {
        let _ = handle.sync_all();
    }
}

/// Windows 上 std 无法直接打开目录句柄做 FlushFileBuffers（需要 FILE_FLAG_BACKUP_SEMANTICS），
/// rename 本身已经是替换语义，这里不再额外处理。
#[cfg(not(unix))]
fn sync_dir(_dir: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_and_replaces() {
        let dir = std::env::temp_dir().join("storydesk-atomic-test");
        let _ = fs::create_dir_all(&dir);
        let file = dir.join("a.md");
        write_atomic(&file, "第一版".as_bytes()).unwrap();
        write_atomic(&file, "第二版".as_bytes()).unwrap();
        assert_eq!(read_text(&file).unwrap(), "第二版");

        // 不留临时文件
        let leftovers = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with(TEMP_PREFIX))
            .count();
        assert_eq!(leftovers, 0);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_invalid_utf8() {
        assert!(decode_utf8(&[0xFFu8, 0xFE, 0x00]).is_err());
        assert_eq!(decode_utf8(b"\xEF\xBB\xBFhello").unwrap(), "hello");
    }
}
