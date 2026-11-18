//! 快照：每次覆盖写盘前，把磁盘上的旧内容留一份。
//!
//! 目录形状：`.storydesk/snapshots/<sha1(相对路径)>/<ISO 时间戳>.md`
//!
//! - 用 `sha1(相对路径)` 当分片目录名：文件名里的中文/空格不会变成奇怪字符，也不会因为改名而串档。
//! - 时间戳形如 `2026-11-05T09-52-13.123Z`：**冒号换成短横**，因为 Windows 文件名不允许 `:`。
//! - 每个分片只保留最近 [`SNAPSHOT_KEEP`] 份，老的按文件名（即时间）从旧到新清理。

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use sha1::{Digest, Sha1};

use crate::models::dto::SnapshotInfo;
use crate::models::StoryError;
use crate::services::atomic_write;

/// 保留份数：滚动窗口，旧的自动删。
pub const SNAPSHOT_KEEP: usize = 20;

/// 快照根目录（相对工作区根）。
pub const SNAPSHOT_ROOT_REL: &str = ".storydesk/snapshots";

/// 记录该分片对应哪个文件，方便用户直接翻目录时认得出。
const META_FILE: &str = "path.txt";

/// 相对路径的 sha1（小写十六进制）。
pub fn sha1_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha1::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// 当前 UTC 时间戳，用作快照文件名。
///
/// 形如 `2026-11-05T09-52-13.123Z`：ISO 8601 的可做文件名的变体（`:` 换成 `-`），
/// 字典序 == 时间序，方便排序与清理。
pub fn iso_now() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs() as i64;
    let millis = now.subsec_millis();

    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let (hour, minute, second) = (rem / 3600, (rem % 3600) / 60, rem % 60);

    format!("{year:04}-{month:02}-{day:02}T{hour:02}-{minute:02}-{second:02}.{millis:03}Z")
}

/// Howard Hinnant 的 `civil_from_days`：把「1970-01-01 起的天数」换成公历年月日。
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// 某个相对路径对应的快照分片目录。
pub fn shard_dir(workspace_root: &Path, rel: &str) -> PathBuf {
    workspace_root
        .join(SNAPSHOT_ROOT_REL)
        .join(sha1_hex(rel.as_bytes()))
}

/// 覆盖前留底。`previous` 是即将被覆盖的磁盘内容（新文件传 `None`）。
///
/// 返回新快照的信息；`None` 表示这次没有旧内容可存。
pub fn take(
    workspace_root: &Path,
    rel: &str,
    previous: Option<&[u8]>,
) -> Result<Option<SnapshotInfo>, StoryError> {
    let Some(bytes) = previous else {
        return Ok(None);
    };

    let dir = shard_dir(workspace_root, rel);
    fs::create_dir_all(&dir)
        .map_err(|e| StoryError::fs(dir.display().to_string(), format!("创建快照目录失败: {e}")))?;

    let mut stamp = iso_now();
    let mut candidate = dir.join(format!("{stamp}.md"));
    let mut suffix = 2;
    // 同一毫秒内两次保存（脚本化操作很常见）不能互相覆盖
    while candidate.exists() {
        candidate = dir.join(format!("{stamp}-{suffix}.md"));
        suffix += 1;
    }
    stamp = candidate
        .file_name()
        .map(|n| n.to_string_lossy().trim_end_matches(".md").to_string())
        .unwrap_or(stamp);

    atomic_write::write_atomic(&candidate, bytes)?;

    let meta = dir.join(META_FILE);
    if !meta.exists() {
        let _ = atomic_write::write_atomic(&meta, rel.as_bytes());
    }

    prune(&dir)?;

    Ok(Some(SnapshotInfo {
        id: candidate
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default(),
        path: candidate.display().to_string(),
        rel: rel.to_string(),
        created_at: stamp,
        bytes: bytes.len() as u64,
    }))
}

/// 列出某个文件的全部快照，新的在前。
pub fn list(workspace_root: &Path, rel: &str) -> Result<Vec<SnapshotInfo>, StoryError> {
    let dir = shard_dir(workspace_root, rel);
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut items: Vec<SnapshotInfo> = Vec::new();
    let read = fs::read_dir(&dir)
        .map_err(|e| StoryError::fs(dir.display().to_string(), format!("读取快照目录失败: {e}")))?;

    for entry in read.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.ends_with(".md") {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        items.push(SnapshotInfo {
            id: name.clone(),
            path: entry.path().display().to_string(),
            rel: rel.to_string(),
            created_at: name.trim_end_matches(".md").to_string(),
            bytes: meta.len(),
        });
    }

    items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    Ok(items)
}

/// 读某个快照的正文。
pub fn read(workspace_root: &Path, rel: &str, id: &str) -> Result<String, StoryError> {
    let path = snapshot_path(workspace_root, rel, id)?;
    atomic_write::read_text(&path)
}

/// 快照文件的绝对路径（`id` 只允许是纯文件名）。
pub fn snapshot_path(workspace_root: &Path, rel: &str, id: &str) -> Result<PathBuf, StoryError> {
    if id.is_empty()
        || id.contains('/')
        || id.contains('\\')
        || id.contains("..")
        || !id.ends_with(".md")
    {
        return Err(StoryError::invalid(
            "id",
            "快照 id 必须是快照目录里的 .md 文件名",
        ));
    }
    Ok(shard_dir(workspace_root, rel).join(id))
}

/// 只保留最近 [`SNAPSHOT_KEEP`] 份。
fn prune(dir: &Path) -> Result<(), StoryError> {
    let read = fs::read_dir(dir)
        .map_err(|e| StoryError::fs(dir.display().to_string(), format!("读取快照目录失败: {e}")))?;

    let mut names: Vec<String> = read
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            (name.ends_with(".md") && e.path().is_file()).then_some(name)
        })
        .collect();

    if names.len() <= SNAPSHOT_KEEP {
        return Ok(());
    }

    names.sort();
    let drop_count = names.len() - SNAPSHOT_KEEP;
    for name in names.into_iter().take(drop_count) {
        let _ = fs::remove_file(dir.join(name));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_conversion_matches_known_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_454), (2026, 1, 1));
        assert_eq!(civil_from_days(20_728), (2026, 10, 2));
        assert_eq!(civil_from_days(20_762), (2026, 11, 5));
    }

    #[test]
    fn keeps_only_twenty() {
        let root = std::env::temp_dir().join("storydesk-snapshot-test");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let rel = "正文/第001章_测试.md";
        for i in 0..25 {
            let body = format!("第 {i} 版");
            take(&root, rel, Some(body.as_bytes())).unwrap();
        }
        let items = list(&root, rel).unwrap();
        assert_eq!(items.len(), SNAPSHOT_KEEP);
        // 最新的一份是最后写入的
        assert_eq!(read(&root, rel, &items[0].id).unwrap(), "第 24 版");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn rejects_path_like_ids() {
        let root = std::env::temp_dir();
        assert!(snapshot_path(&root, "a.md", "../x.md").is_err());
        assert!(snapshot_path(&root, "a.md", "x/y.md").is_err());
        assert!(snapshot_path(&root, "a.md", "ok.md").is_ok());
    }
}
