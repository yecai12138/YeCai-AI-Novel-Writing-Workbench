//! 文件读写与目录操作命令。
//!
//! 约定：
//! - 每个命令的第一件事都是 `path_guard::validate_path`，越界直接拒绝；
//! - 写入走 `atomic_write`（临时文件 + fsync + rename），覆盖前先 `snapshot::take` 留底；
//! - 删除不真删，移进 `.storydesk/trash/<时间戳>/<原相对路径>`，随时能捡回来。

use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use tauri::{command, AppHandle, Manager};

use crate::models::dto::{FileContent, SaveOutcome, SnapshotInfo};
use crate::models::StoryError;
use crate::services::watcher::WatcherState;
use crate::services::{atomic_write, path_guard, snapshot};

use super::blocking;

const CHAPTER_DIR: &str = "正文";

// ------------------------------------------------------------------ 基础工具

/// 校验目标路径并返回 (规范化工作区根, 绝对路径)。
fn resolve(workspace_root: &str, target: &str) -> Result<(PathBuf, PathBuf), StoryError> {
    if workspace_root.trim().is_empty() {
        return Err(StoryError::invalid("workspaceRoot", "工作区根为空"));
    }
    let root = Path::new(workspace_root);
    // 相对路径先过字符串闸：盘符相对写法（C:foo）、UNC、.. 段在进磁盘之前就拒掉，
    // 免得 Path::join 把绝对路径当基准替换；绝对路径本来就由 validate_path 兜住。
    let abs = if Path::new(target).is_absolute() {
        path_guard::validate_path(root, Path::new(target))?
    } else {
        path_guard::validate_rel(root, target)?
    };
    let canonical_root = root.canonicalize().map_err(|e| {
        StoryError::fs(workspace_root.to_string(), format!("工作区根不可访问: {e}"))
    })?;
    Ok((canonical_root, abs))
}

/// 相对工作区根的路径（`/` 分隔；拿不到相对关系时退回绝对路径）。
fn rel_of(root: &Path, abs: &Path) -> String {
    abs.strip_prefix(root)
        .unwrap_or(abs)
        .display()
        .to_string()
        .replace('\\', "/")
}

fn mtime_ms(meta: &fs::Metadata) -> Option<u64> {
    meta.modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_millis() as u64)
}

fn read_content(root: &Path, abs: &Path) -> Result<FileContent, StoryError> {
    let meta = fs::metadata(abs)
        .map_err(|e| StoryError::fs(abs.display().to_string(), format!("读取文件信息失败: {e}")))?;
    if meta.is_dir() {
        return Err(StoryError::invalid("path", "目标是目录，不是文件"));
    }
    let content = atomic_write::read_text(abs)?;
    Ok(FileContent {
        path: abs.display().to_string(),
        rel: rel_of(root, abs),
        bytes: meta.len(),
        mtime_ms: mtime_ms(&meta),
        sha1: snapshot::sha1_hex(content.as_bytes()),
        content,
    })
}

/// 文件名清洗：拒绝分隔符、Windows 保留字符与空名。
fn clean_file_name(raw: &str) -> Result<String, StoryError> {
    let name = raw.trim().trim_end_matches(['.', ' ']).to_string();
    if name.is_empty() {
        return Err(StoryError::invalid("name", "名称不能为空"));
    }
    if name == "." || name == ".." {
        return Err(StoryError::invalid("name", "名称不能是 . 或 .."));
    }
    if name.contains(['/', '\\']) {
        return Err(StoryError::invalid("name", "名称不能包含路径分隔符"));
    }
    if name.contains([':', '*', '?', '"', '<', '>', '|']) {
        return Err(StoryError::invalid(
            "name",
            "名称不能包含 : * ? \" < > | 这些字符",
        ));
    }
    Ok(name)
}

/// `第001章_开篇` → 取出章号 1。
fn parse_chapter_no(name: &str) -> Option<u32> {
    let rest = name.strip_prefix('第')?;
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() || !rest[digits.len()..].starts_with('章') {
        return None;
    }
    digits.parse().ok()
}

fn next_chapter_no(dir: &Path) -> u32 {
    let mut max = 0u32;
    if let Ok(rd) = fs::read_dir(dir) {
        for entry in rd.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if let Some(no) = parse_chapter_no(&name) {
                max = max.max(no);
            }
        }
    }
    max + 1
}

// -------------------------------------------------------------------- 读写

/// 读文件（UTF-8 文本）。
#[command]
pub async fn read_file(workspace_root: String, path: String) -> Result<FileContent, StoryError> {
    blocking(move || {
        let (root, abs) = resolve(&workspace_root, &path)?;
        read_content(&root, &abs)
    })
    .await
}

/// 写文件：先给旧内容留快照，再原子替换。
#[command]
pub async fn write_file(
    workspace_root: String,
    path: String,
    content: String,
) -> Result<SaveOutcome, StoryError> {
    blocking(move || {
        let (root, abs) = resolve(&workspace_root, &path)?;
        let rel = rel_of(&root, &abs);

        if abs.is_dir() {
            return Err(StoryError::invalid("path", "目标是目录，不能写入"));
        }

        // 覆盖前留底：只有已存在的文件才有“旧内容”
        let previous = if abs.exists() {
            Some(fs::read(&abs).map_err(|e| {
                StoryError::fs(abs.display().to_string(), format!("读取旧内容失败: {e}"))
            })?)
        } else {
            None
        };
        let snap = snapshot::take(&root, &rel, previous.as_deref())?;

        atomic_write::write_atomic(&abs, content.as_bytes())?;

        let meta = fs::metadata(&abs).map_err(|e| {
            StoryError::fs(abs.display().to_string(), format!("写入后读取失败: {e}"))
        })?;

        Ok(SaveOutcome {
            file: FileContent {
                path: abs.display().to_string(),
                rel,
                bytes: meta.len(),
                mtime_ms: mtime_ms(&meta),
                sha1: snapshot::sha1_hex(content.as_bytes()),
                content,
            },
            snapshot: snap,
            saved_at: snapshot::iso_now(),
        })
    })
    .await
}

// ---------------------------------------------------------------- 新建条目

/// 新建章节：`正文/第NNN章_标题.md`。`chapter_no` 传 None 时自动取下一个号。
#[command]
pub async fn create_chapter(
    workspace_root: String,
    book_root: String,
    chapter_no: Option<u32>,
    title: String,
) -> Result<FileContent, StoryError> {
    blocking(move || {
        let (root, book) = resolve(&workspace_root, &book_root)?;
        let dir = book.join(CHAPTER_DIR);
        fs::create_dir_all(&dir)
            .map_err(|e| StoryError::fs(dir.display().to_string(), format!("创建正文目录失败: {e}")))?;

        let no = match chapter_no {
            Some(0) => next_chapter_no(&dir),
            Some(n) => n,
            None => next_chapter_no(&dir),
        };
        let title = clean_file_name(if title.trim().is_empty() { "未命名" } else { &title })?;
        let name = format!("第{no:03}章_{title}.md");
        let target = dir.join(&name);

        if target.exists() {
            return Err(StoryError::invalid(
                "title",
                format!("{CHAPTER_DIR}/{name} 已存在，换个章号或标题"),
            ));
        }

        let body = format!(
            "# 第{no:03}章 {title}\n\n> 状态：草稿（StoryDesk 新建）\n> 提示：正文写完后可跑 storyctl.py chapter check 做体检。\n\n"
        );
        atomic_write::write_atomic(&target, body.as_bytes())?;
        read_content(&root, &target)
    })
    .await
}

/// 新建普通 Markdown 文件（设定、大纲、角色卡等）：在 `dir_path` 下建 `<name>.md`。
#[command]
pub async fn create_note(
    workspace_root: String,
    dir_path: String,
    name: String,
) -> Result<FileContent, StoryError> {
    blocking(move || {
        let (root, dir) = resolve(&workspace_root, &dir_path)?;
        if !dir.is_dir() {
            return Err(StoryError::invalid("dirPath", "目标不是目录"));
        }
        let stem = clean_file_name(&name)?;
        let stem = stem.trim_end_matches(".md").to_string();
        let target = dir.join(format!("{stem}.md"));

        if target.exists() {
            return Err(StoryError::invalid("name", format!("{stem}.md 已存在")));
        }

        let body = format!("# {stem}\n\n");
        atomic_write::write_atomic(&target, body.as_bytes())?;
        read_content(&root, &target)
    })
    .await
}

// ------------------------------------------------------------ 改名 / 移动 / 删除

/// 改名（同目录内）。
#[command]
pub async fn rename_entry(
    workspace_root: String,
    path: String,
    new_name: String,
) -> Result<FileContent, StoryError> {
    blocking(move || {
        let (root, abs) = resolve(&workspace_root, &path)?;
        let parent = abs
            .parent()
            .ok_or_else(|| StoryError::invalid("path", "目标没有父目录"))?;
        let mut name = clean_file_name(&new_name)?;
        // 只改了主名、没带扩展名时沿用原扩展名：避免把 `第001章_x.md` 改成没有后缀的文件
        if abs.is_file() && Path::new(&name).extension().is_none() {
            if let Some(ext) = abs.extension() {
                name.push('.');
                name.push_str(&ext.to_string_lossy());
            }
        }
        let target = parent.join(&name);

        if target == abs {
            return read_content(&root, &abs);
        }
        if target.exists() {
            return Err(StoryError::invalid("name", format!("{name} 已存在")));
        }

        fs::rename(&abs, &target).map_err(|e| {
            StoryError::fs(
                abs.display().to_string(),
                format!("改名失败（文件可能被其他程序占用）: {e}"),
            )
        })?;
        read_content(&root, &target)
    })
    .await
}

/// 移动到另一个目录（保留文件名）。
#[command]
pub async fn move_entry(
    workspace_root: String,
    path: String,
    dest_dir: String,
) -> Result<FileContent, StoryError> {
    blocking(move || {
        let (root, abs) = resolve(&workspace_root, &path)?;
        let (_, dest) = resolve(&workspace_root, &dest_dir)?;
        if !dest.is_dir() {
            return Err(StoryError::invalid("destDir", "目标不是目录"));
        }
        let name = abs
            .file_name()
            .ok_or_else(|| StoryError::invalid("path", "目标没有文件名"))?;
        let target = dest.join(name);
        if target == abs {
            return read_content(&root, &abs);
        }
        if target.exists() {
            return Err(StoryError::invalid("destDir", "目标目录已有同名文件"));
        }
        fs::rename(&abs, &target)
            .map_err(|e| StoryError::fs(abs.display().to_string(), format!("移动失败: {e}")))?;
        read_content(&root, &target)
    })
    .await
}

/// 删除 = 移进 `.storydesk/trash/<时间戳>/<原相对路径>`，返回垃圾桶里的路径。
#[command]
pub async fn delete_entry(workspace_root: String, path: String) -> Result<String, StoryError> {
    blocking(move || {
        let (root, abs) = resolve(&workspace_root, &path)?;
        if !abs.exists() {
            return Err(StoryError::invalid("path", "目标不存在"));
        }
        let rel = rel_of(&root, &abs);
        let trash_dir = root.join(".storydesk/trash").join(snapshot::iso_now());
        let target = trash_dir.join(&rel);

        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                StoryError::fs(
                    parent.display().to_string(),
                    format!("创建回收目录失败: {e}"),
                )
            })?;
        }
        fs::rename(&abs, &target).map_err(|e| {
            StoryError::fs(
                abs.display().to_string(),
                format!("移入回收站失败（文件可能被占用）: {e}"),
            )
        })?;
        Ok(target.display().to_string())
    })
    .await
}

// ---------------------------------------------------------------- 快照命令

/// 列出某个文件的历史快照（新的在前）。
#[command]
pub async fn list_snapshots(
    workspace_root: String,
    path: String,
) -> Result<Vec<SnapshotInfo>, StoryError> {
    blocking(move || {
        let (root, abs) = resolve(&workspace_root, &path)?;
        let rel = rel_of(&root, &abs);
        snapshot::list(&root, &rel)
    })
    .await
}

/// 把某一版快照写回文件（写回前同样先给当前内容留底，因此可以反复后悔）。
#[command]
pub async fn restore_snapshot(
    workspace_root: String,
    path: String,
    snapshot_id: String,
) -> Result<SaveOutcome, StoryError> {
    blocking(move || {
        let (root, abs) = resolve(&workspace_root, &path)?;
        let rel = rel_of(&root, &abs);
        let content = snapshot::read(&root, &rel, &snapshot_id)?;

        let previous = if abs.exists() {
            Some(fs::read(&abs).map_err(|e| {
                StoryError::fs(abs.display().to_string(), format!("读取当前内容失败: {e}"))
            })?)
        } else {
            None
        };
        let snap = snapshot::take(&root, &rel, previous.as_deref())?;
        atomic_write::write_atomic(&abs, content.as_bytes())?;

        let meta = fs::metadata(&abs).map_err(|e| {
            StoryError::fs(abs.display().to_string(), format!("写回后读取失败: {e}"))
        })?;

        Ok(SaveOutcome {
            file: FileContent {
                path: abs.display().to_string(),
                rel,
                bytes: meta.len(),
                mtime_ms: mtime_ms(&meta),
                sha1: snapshot::sha1_hex(content.as_bytes()),
                content,
            },
            snapshot: snap,
            saved_at: snapshot::iso_now(),
        })
    })
    .await
}

/// 校验「这条路径是不是这个工作区里的一版快照」，返回可删的绝对路径。
///
/// 抽成纯函数是为了能被单测直接调用：delete_snapshot 是 async command，
/// 包着 spawn_blocking，测试里跑不起来；规则本身必须可回归。
fn resolve_snapshot_for_delete(workspace_root: &str, raw: &str) -> Result<PathBuf, StoryError> {
    if raw.trim().is_empty() {
        return Err(StoryError::invalid("snapshotPath", "快照路径为空"));
    }
    let raw = raw.trim();
    let root = path_guard::validate_root(Path::new(workspace_root))?;
    // validate_path：拒绝 .. / 冒号段，并把最深的已存在祖先 canonicalize，
    // 越界（含盘符、UNC、符号链接祖先）一律 PathEscape。
    let abs = path_guard::validate_path(&root, Path::new(raw))?;

    let snap_root = root.join(".storydesk").join("snapshots");
    let snap_root = snap_root.canonicalize().map_err(|e| {
        StoryError::fs(
            snap_root.display().to_string(),
            format!("快照目录不存在或无法访问: {e}"),
        )
    })?;
    // 两边都已 canonicalize，可以直接按前缀比较；不再用「路径里出现过
    // .storydesk/snapshots 就算数」这种可以伪造的判据。
    if !path_guard::is_within(&snap_root, &abs) {
        return Err(StoryError::invalid(
            "snapshotPath",
            format!("拒绝删除：{} 不在快照目录内", abs.display()),
        ));
    }
    if !abs.is_file() {
        return Err(StoryError::invalid(
            "snapshotPath",
            format!("拒绝删除：{} 不是文件", abs.display()),
        ));
    }
    if abs.extension().and_then(|e| e.to_str()) != Some("md") {
        return Err(StoryError::invalid(
            "snapshotPath",
            "拒绝删除：快照文件必须是 .md",
        ));
    }
    Ok(abs)
}

/// 删除一版快照（物理删除，不进回收站）。
///
/// 安全口径（唯一权威是 path_guard）：
/// 1. 绑定 workspace_root —— 路径必须在这个工作区根内，不接受「随便一个
///    含 .storydesk/snapshots 的字符串」；以前只做逐段比对，别的盘上任意
///    一个同名目录里的 .md 都能被它物理删掉。
/// 2. 必须落在 <根>/.storydesk/snapshots（已 canonicalize）之下；
/// 3. 必须是真实存在的 .md 文件。
#[command]
pub async fn delete_snapshot(
    workspace_root: String,
    snapshot_path: String,
) -> Result<(), StoryError> {
    blocking(move || {
        let abs = resolve_snapshot_for_delete(&workspace_root, &snapshot_path)?;

        fs::remove_file(&abs).map_err(|e| {
            StoryError::fs(
                abs.display().to_string(),
                format!("删除快照失败（可能被占用）: {e}"),
            )
        })?;

        // 分片目录空了就顺手收掉，避免 snapshots/ 下留一堆空目录
        if let Some(dir) = abs.parent() {
            let _ = fs::remove_dir(dir);
        }
        Ok(())
    })
    .await
}

// ------------------------------------------------------------------ 监听命令
/// 开始监听项目工作区（重复调用会切换根目录）。
#[command]
pub async fn watch_project(app: AppHandle, workspace_root: String) -> Result<String, StoryError> {
    blocking(move || {
        let state = app.state::<WatcherState>();
        let mut guard = state
            .0
            .lock()
            .map_err(|_| StoryError::internal("监听状态不可用（锁被污染）"))?;
        guard.start(app.clone(), Path::new(&workspace_root))
    })
    .await
}

/// 停止监听。
#[command]
pub async fn unwatch_project(app: AppHandle) -> Result<(), StoryError> {
    blocking(move || {
        let state = app.state::<WatcherState>();
        let mut guard = state
            .0
            .lock()
            .map_err(|_| StoryError::internal("监听状态不可用（锁被污染）"))?;
        guard.stop();
        Ok(())
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 造一个临时工作区：根下 .storydesk/snapshots/<分片>/x.md 与根外一份同名结构。
    fn scratch_workspace(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("storydesk-fs-{tag}"));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(".storydesk/snapshots/正文")).unwrap();
        fs::write(root.join(".storydesk/snapshots/正文/第001章.md"), b"old").unwrap();
        fs::write(root.join("正文.md"), b"book").unwrap();
        // 根外另造一套同样的目录结构：旧实现只看路径里有没有 .storydesk/snapshots。
        let outside = root.join("..").join(format!("storydesk-fs-{tag}-outside"));
        let _ = fs::remove_dir_all(&outside);
        fs::create_dir_all(outside.join(".storydesk/snapshots")).unwrap();
        fs::write(outside.join(".storydesk/snapshots/evil.md"), b"x").unwrap();
        root
    }

    #[test]
    fn snapshot_delete_stays_inside_its_own_workspace() {
        let root = scratch_workspace("snap");
        let root_s = root.display().to_string();

        // 正常一版：放行，返回规范化绝对路径。
        let ok = resolve_snapshot_for_delete(&root_s, ".storydesk/snapshots/正文/第001章.md").unwrap();
        assert!(ok.is_file());

        // 根外的同名结构：拒绝（旧实现会认，因为路径里出现过 .storydesk/snapshots）。
        let evil = root
            .join("..")
            .join("storydesk-fs-snap-outside")
            .join(".storydesk/snapshots/evil.md");
        assert!(resolve_snapshot_for_delete(&root_s, &evil.display().to_string()).is_err());

        // 根内但不是快照：拒绝。
        assert!(resolve_snapshot_for_delete(&root_s, "正文.md").is_err());

        // 穿越与绝对路径：拒绝。
        assert!(resolve_snapshot_for_delete(&root_s, "../storydesk-fs-snap-outside/.storydesk/snapshots/evil.md").is_err());
        assert!(resolve_snapshot_for_delete(&root_s, "/etc/passwd").is_err());

        // 根不存在：拒绝。
        assert!(resolve_snapshot_for_delete("", "x.md").is_err());

        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(root.join("..").join("storydesk-fs-snap-outside"));
    }
}
