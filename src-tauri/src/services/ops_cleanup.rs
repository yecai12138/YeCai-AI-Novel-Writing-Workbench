//! 运维面板的落地动作：量目录、清日志、清旧 repos、给会话库做 VACUUM。
//!
//! 这些函数只碰三类东西：
//!   1. 本程序的日志目录（`%LOCALAPPDATA%\com.storydesk.app\logs`）；
//!   2. 第 5 项隔离出来的 opencode 数据目录（`…/opencode/data/opencode/**`）；
//!   3. 上面那份会话库本身。
//!
//! 桌面版 opencode 的 `~/.local/share/opencode` 一概不碰——那是别人的地盘。
//! 所有删除都先由调用方报出「预计释放 N」，再调这里的函数真删。

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// engine.log 涨到这个体量就轮转（本地日志只在排查引擎故障时用，不需要留太久）。
pub const ENGINE_LOG_ROTATE_AT: u64 = 4 * 1024 * 1024;
/// 轮转后保留的份数（engine.log.1 … engine.log.3）。
pub const ENGINE_LOG_KEEP: usize = 3;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DirUsage {
    pub path: String,
    /// 目录内文件数（递归，不含目录本身）。
    pub files: usize,
    pub bytes: u64,
}

/// 递归量一个目录：文件数与总字节。
pub fn dir_usage(dir: &Path) -> DirUsage {
    let mut usage = DirUsage {
        path: dir.display().to_string(),
        files: 0,
        bytes: 0,
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return usage;
    };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            let inner = dir_usage(&entry.path());
            usage.files += inner.files;
            usage.bytes += inner.bytes;
        } else if file_type.is_file() {
            usage.files += 1;
            usage.bytes += entry.metadata().map(|m| m.len()).unwrap_or(0);
        }
    }
    usage
}

/// 只清目录里的文件（含子目录里的文件），目录结构原样保留——
/// 下一轮日志要能直接写回同一个位置。
pub fn clean_dir_files(dir: &Path) -> (usize, u64) {
    let mut removed = 0usize;
    let mut freed = 0u64;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (removed, freed);
    };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if file_type.is_dir() {
            let (inner_removed, inner_freed) = clean_dir_files(&path);
            removed += inner_removed;
            freed += inner_freed;
            // 空目录也清掉，但根目录不动。
            let _ = std::fs::remove_dir(&path);
        } else if file_type.is_file() {
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            if std::fs::remove_file(&path).is_ok() {
                removed += 1;
                freed += size;
            }
        }
    }
    (removed, freed)
}

/// repos 的「年龄」按目录里最新一个**文件**的 mtime 算；一个文件都没有时才退回目录自身 mtime。
///
/// 不能把目录 mtime 也算进「最新活动」：删掉文件、重命名子目录都会刷新父目录 mtime，
/// 那样一个只剩旧 pack 的老仓库会因为某个空目录被点过而永远显得「刚用过」。
pub fn newest_activity(dir: &Path) -> Option<SystemTime> {
    fn newest_file(dir: &Path, found: &mut Option<SystemTime>, saw_file: &mut bool) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                newest_file(&entry.path(), found, saw_file);
            } else if file_type.is_file() {
                *saw_file = true;
                if let Ok(stamp) = entry.metadata().and_then(|m| m.modified()) {
                    *found = Some(match *found {
                        Some(current) if current >= stamp => current,
                        _ => stamp,
                    });
                }
            }
        }
    }

    let mut found = None;
    let mut saw_file = false;
    newest_file(dir, &mut found, &mut saw_file);
    if saw_file {
        found
    } else {
        std::fs::metadata(dir).and_then(|m| m.modified()).ok()
    }
}

/// 删掉 `root` 下「最近活动早于 keep_days 天」的子目录。
pub fn clean_old_repos(root: &Path, keep_days: u32, now: SystemTime) -> (usize, u64) {
    let mut removed = 0usize;
    let mut freed = 0u64;
    // 0 天不该等于「删光」：至少要闲置一整天，否则用户刚点过一次清理就会把
    // 今天还在用的仓库一起带走。
    let days = keep_days.max(1);
    let horizon = Duration::from_secs(u64::from(days) * 24 * 60 * 60);
    let Ok(entries) = std::fs::read_dir(root) else {
        return (removed, freed);
    };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let path = entry.path();
        let stale = match newest_activity(&path) {
            Some(stamp) => now
                .duration_since(stamp)
                .map(|age| age >= horizon)
                .unwrap_or(false),
            // 时间读不出来时不动它——宁可少删。
            None => false,
        };
        if !stale {
            continue;
        }
        let size = dir_usage(&path).bytes;
        if std::fs::remove_dir_all(&path).is_ok() {
            removed += 1;
            freed += size;
        }
    }
    (removed, freed)
}

/// repos 目录下有多少个子仓库、占多少字节。
pub fn repos_usage(root: &Path) -> (usize, u64) {
    let mut dirs = 0usize;
    let mut bytes = 0u64;
    let Ok(entries) = std::fs::read_dir(root) else {
        return (dirs, bytes);
    };
    for entry in entries.flatten() {
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            dirs += 1;
            bytes += dir_usage(&entry.path()).bytes;
        }
    }
    (dirs, bytes)
}

/// engine.log 超过阈值就轮转；返回一句给人看的结果（没轮转就返回 None）。
pub fn rotate_engine_log(dir: &Path) -> Option<String> {
    let path = dir.join("engine.log");
    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    if size <= ENGINE_LOG_ROTATE_AT {
        return None;
    }
    let _ = std::fs::remove_file(dir.join(format!("engine.log.{ENGINE_LOG_KEEP}")));
    for index in (1..ENGINE_LOG_KEEP).rev() {
        let from = dir.join(format!("engine.log.{index}"));
        let to = dir.join(format!("engine.log.{}", index + 1));
        if from.exists() {
            let _ = std::fs::rename(&from, &to);
        }
    }
    let _ = std::fs::rename(&path, dir.join("engine.log.1"));
    Some(format!(
        "engine.log 已轮转（{} MB，保留 {ENGINE_LOG_KEEP} 份）",
        size / 1024 / 1024
    ))
}

/// 给会话库做整理的 python 脚本。
///
/// 走 python 的 sqlite3 是因为工程里没有 sqlite 绑定，而 gate 侧本来就依赖 python；
/// 顺序是「先 checkpoint 把 WAL 收回去 → VACUUM → integrity_check」，
/// 任何一步抛异常都会以非零码退出，界面就能如实报错而不是假装成功。
pub fn vacuum_script() -> &'static str {
    r#"
import sqlite3, sys, json, os
path = sys.argv[1]
before = os.path.getsize(path) if os.path.exists(path) else 0
conn = sqlite3.connect(path)
try:
    conn.execute("PRAGMA wal_checkpoint(TRUNCATE)")
    conn.commit()
    conn.execute("VACUUM")
    conn.commit()
    ok = conn.execute("PRAGMA integrity_check").fetchone()
    verdict = str(ok[0]) if ok else "unknown"
finally:
    conn.close()
after = os.path.getsize(path) if os.path.exists(path) else 0
print(json.dumps({"before": before, "after": after, "integrity": verdict}, ensure_ascii=False))
"#
}

/// 把字节数说成人话（面板上直接显示）。
pub fn human_bytes(bytes: u64) -> String {
    const MB: u64 = 1024 * 1024;
    const GB: u64 = 1024 * MB;
    if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= 1024 {
        format!("{:.0} KB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

/// 面板要展示的一项：目录 + 用量 + 一句说明。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanelItem {
    pub label: String,
    pub path: PathBuf,
    pub files: usize,
    pub bytes: u64,
}

impl PanelItem {
    pub fn new(label: &str, path: PathBuf) -> Self {
        let usage = dir_usage(&path);
        Self {
            label: label.to_string(),
            path,
            files: usage.files,
            bytes: usage.bytes,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("storydesk-ops-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn dir_usage_counts_nested_files() {
        let root = scratch("usage");
        std::fs::create_dir_all(root.join("a/b")).unwrap();
        std::fs::write(root.join("top.log"), vec![0u8; 100]).unwrap();
        std::fs::write(root.join("a/mid.log"), vec![0u8; 50]).unwrap();
        std::fs::write(root.join("a/b/deep.log"), vec![0u8; 25]).unwrap();
        let usage = dir_usage(&root);
        assert_eq!(usage.files, 3);
        assert_eq!(usage.bytes, 175);
    }

    #[test]
    fn clean_log_files_keeps_the_directory() {
        let root = scratch("clean");
        std::fs::create_dir_all(root.join("inner")).unwrap();
        std::fs::write(root.join("engine.log"), vec![0u8; 10]).unwrap();
        std::fs::write(root.join("inner/old.log.1"), vec![0u8; 40]).unwrap();
        let (removed, freed) = clean_dir_files(&root);
        assert_eq!(removed, 2);
        assert_eq!(freed, 50);
        assert!(root.is_dir(), "根目录必须还在，下一轮日志要写回来");
        assert_eq!(dir_usage(&root).files, 0);
    }

    #[test]
    fn old_repos_are_removed_and_fresh_ones_kept() {
        let root = scratch("repos");
        let stale = root.join("repo-old");
        let fresh = root.join("repo-new");
        std::fs::create_dir_all(&stale).unwrap();
        std::fs::create_dir_all(&fresh).unwrap();
        let old_file = stale.join("objects.pack");
        std::fs::write(&old_file, vec![0u8; 1000]).unwrap();
        std::fs::write(fresh.join("objects.pack"), vec![0u8; 2000]).unwrap();

        let now = SystemTime::now();
        let handle = std::fs::OpenOptions::new().write(true).open(&old_file).unwrap();
        handle
            .set_modified(now - Duration::from_secs(40 * 24 * 60 * 60))
            .unwrap();
        drop(handle);

        let (removed, freed) = clean_old_repos(&root, 30, now);
        assert_eq!(removed, 1);
        assert_eq!(freed, 1000);
        assert!(!stale.exists(), "超过 30 天没动的仓库应当被删");
        assert!(fresh.exists(), "新仓库必须保留");
    }

    #[test]
    fn keep_days_zero_still_needs_activity_to_be_old() {
        let root = scratch("repos-zero");
        let fresh = root.join("repo-new");
        std::fs::create_dir_all(&fresh).unwrap();
        std::fs::write(fresh.join("HEAD"), b"ref").unwrap();
        let (removed, _) = clean_old_repos(&root, 0, SystemTime::now());
        assert_eq!(removed, 0, "刚写过的仓库不算旧，不许删");
        assert!(fresh.exists());
    }

    #[test]
    fn human_bytes_speaks_in_units() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(2048), "2 KB");
        assert_eq!(human_bytes(5 * 1024 * 1024), "5.0 MB");
        assert_eq!(human_bytes(3 * 1024 * 1024 * 1024), "3.00 GB");
    }

    #[test]
    fn engine_log_rotates_only_when_big() {
        let dir = scratch("rotate");
        std::fs::write(dir.join("engine.log"), vec![0u8; 1024]).unwrap();
        assert!(rotate_engine_log(&dir).is_none(), "小日志不该轮转");
        assert!(dir.join("engine.log").exists());

        let big = dir.join("engine.log");
        std::fs::write(&big, vec![0u8; (ENGINE_LOG_ROTATE_AT + 1) as usize]).unwrap();
        let note = rotate_engine_log(&dir).expect("超过阈值必须轮转");
        assert!(note.contains("已轮转"));
        assert!(dir.join("engine.log.1").exists());
        assert!(!big.exists(), "轮转后新日志从空文件重新开始");
    }
}
