//! 路径守卫：所有来自前端的路径都必须先过这里。
//!
//! 目标：即使前端被注入恶意路径，也只能命中工作区根以内的真实文件。
//! 规则（按顺序执行）：
//! 1. 工作区根必须存在且可规范化；
//! 2. 相对路径按“工作区根内相对路径”解释；
//! 3. 出现 `..` 直接拒绝（不做“规范化后再比较”的宽容处理，避免越界被规范化掩盖）；
//! 4. 任一普通段含 `:` 直接拒绝（Windows 备用数据流 ADS，如 `readme.md:hidden`）；
//! 5. 取最深的已存在祖先做 canonicalize，再把剩余不存在的段拼回去，最后与根做**按路径组件**比较。

use std::path::{Component, Path, PathBuf};

use crate::models::StoryError;

/// 校验并规范化 `target`，返回它的绝对路径；越界时返回 [`StoryError::PathEscape`]。
pub fn validate_path(workspace_root: &Path, target: &Path) -> Result<PathBuf, StoryError> {
    if workspace_root.as_os_str().is_empty() {
        return Err(StoryError::invalid("workspaceRoot", "工作区根为空"));
    }
    let root = workspace_root.canonicalize().map_err(|e| {
        StoryError::fs(
            workspace_root.display().to_string(),
            format!("工作区根不可访问: {e}"),
        )
    })?;

    if target.as_os_str().is_empty() {
        return Err(StoryError::invalid("target", "目标路径为空"));
    }

    let raw = target.to_string_lossy().to_string();
    if raw.contains('\0') {
        return Err(StoryError::invalid("target", "路径包含非法字符 NUL"));
    }

    let absolute = if target.is_absolute() {
        target.to_path_buf()
    } else {
        root.join(target)
    };

    for comp in absolute.components() {
        match comp {
            Component::ParentDir => {
                return Err(StoryError::PathEscape {
                    target: raw,
                    root: root.display().to_string(),
                })
            }
            Component::Normal(part) => {
                let part = part.to_string_lossy();
                if part.contains(':') {
                    return Err(StoryError::invalid(
                        "target",
                        format!("路径段 `{part}` 含冒号（疑似 Windows 备用数据流）"),
                    ));
                }
            }
            _ => {}
        }
    }

    // 找到最深的已存在祖先（目标本身可以尚不存在，例如新建章节）
    let mut existing = absolute.clone();
    let mut rest: Vec<std::ffi::OsString> = Vec::new();
    while !existing.exists() {
        match existing.file_name() {
            Some(name) => {
                rest.push(name.to_os_string());
                if !existing.pop() {
                    break;
                }
            }
            None => break,
        }
    }

    let canonical = existing.canonicalize().map_err(|e| {
        StoryError::fs(
            existing.display().to_string(),
            format!("路径不存在或不可解析: {e}"),
        )
    })?;

    if !canonical.starts_with(&root) {
        return Err(StoryError::PathEscape {
            target: raw,
            root: root.display().to_string(),
        });
    }

    let mut result = canonical;
    for name in rest.into_iter().rev() {
        result.push(name);
    }

    // 再校验一次：拼回不存在的段后仍然必须在根内（防御符号链接祖先 + 大小写差异）
    if !result.starts_with(&root) {
        return Err(StoryError::PathEscape {
            target: raw,
            root: root.display().to_string(),
        });
    }

    Ok(result)
}

/// 只做归属判断，不规范化（用于「两边都已经 canonicalize 过」的场景）。
///
/// 调用点：删除快照时用它把已规范化的绝对路径钉死在已规范化的快照目录里。
/// 它不解引用符号链接、不处理大小写差异，所以只能用在 base 与被比较方都
/// 已经 canonicalize 之后——否则应走 validate_path。
pub fn is_within(root: &Path, candidate: &Path) -> bool {
    candidate.starts_with(root)
}

/// 工作区根本身的口径（唯一权威）：非空、存在、是目录，返回规范化后的绝对路径。
pub fn validate_root(root: &Path) -> Result<PathBuf, StoryError> {
    if root.as_os_str().is_empty() {
        return Err(StoryError::invalid("workspaceRoot", "工作区根为空"));
    }
    let canonical = root.canonicalize().map_err(|e| {
        StoryError::fs(
            root.display().to_string(),
            format!("工作区根不可访问: {e}"),
        )
    })?;
    if !canonical.is_dir() {
        return Err(StoryError::invalid(
            "workspaceRoot",
            format!("工作区根不是目录: {}", canonical.display()),
        ));
    }
    Ok(canonical)
}

/// 只做字符串层面的「相对路径是否安全」，不碰磁盘。
///
/// 拒绝：空串、NUL、绝对路径（含 UNC）、盘符（C:foo 与 C:/foo）、任一空段、
/// 含冒号的段、以及 . 与 .. 段。放行 ./a/b 这种写法（只剥一层 ./）。
///
/// 为什么要一个不碰磁盘的版本：上下文清单、聚焦目录这类「书根下的相对名字」
/// 在内存里只拼字符串，没有可 canonicalize 的锚点；它们以前各写一套挡法，
/// 有的只挡 .. 和开头的 /，于是 C:/x 与 UNC 直接溜过去——而 Path::join 遇到
/// 绝对路径会**替换**掉基准，越界就没人发现了。
pub fn rel_is_safe(rel: &str) -> bool {
    if rel.is_empty() || rel.contains('\0') {
        return false;
    }
    let unified = rel.replace('\\', "/");
    let mut rest = unified.as_str();
    if let Some(stripped) = rest.strip_prefix("./") {
        rest = stripped;
    }
    // 开头的 / 同时覆盖 UNC：\\host\share 改完分隔符就是 //host/share。
    if rest.is_empty() || rest.starts_with('/') {
        return false;
    }
    rest.split('/')
        .all(|seg| !seg.is_empty() && seg != "." && seg != ".." && !seg.contains(':'))
}

/// 相对路径的唯一入口：先过 rel_is_safe 的字符串闸，再走 validate_path 的磁盘闸。
pub fn validate_rel(workspace_root: &Path, rel: &str) -> Result<PathBuf, StoryError> {
    if !rel_is_safe(rel) {
        return Err(StoryError::invalid(
            "path",
            format!("路径不合法（只接受工作区内的相对路径）: {rel}"),
        ));
    }
    validate_path(workspace_root, Path::new(rel))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_parent_dir_escape() {
        let tmp = std::env::temp_dir();
        let root = tmp.join("storydesk-guard-test");
        std::fs::create_dir_all(&root).unwrap();
        let err = validate_path(&root, Path::new("../outside.txt")).unwrap_err();
        assert!(matches!(err, StoryError::PathEscape { .. }));
        let err = validate_path(&root, &root.join("a/../../b.txt")).unwrap_err();
        assert!(matches!(err, StoryError::PathEscape { .. }));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn allows_nested_nonexistent_target() {
        let tmp = std::env::temp_dir();
        let root = tmp.join("storydesk-guard-test2");
        std::fs::create_dir_all(&root).unwrap();
        let target = root.join("正文/第001章_开篇.md");
        let got = validate_path(&root, &target).unwrap();
        assert!(got.starts_with(root.canonicalize().unwrap()));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 纯字符串闸：绝对路径、盘符、UNC、.. 一律不放行。
    /// 这条规则是 context::normalize_rel 与 ai::pick_dir 共用的口径。
    #[test]
    fn rel_is_safe_rejects_absolute_drive_and_unc() {
        assert!(rel_is_safe("正文/第001章_开篇.md"));
        assert!(rel_is_safe("./设定/人物.md"));
        assert!(rel_is_safe("记忆/第009章要点.md"));

        assert!(!rel_is_safe(""));
        assert!(!rel_is_safe("../outside.md"));
        assert!(!rel_is_safe("正文/../../outside.md"));
        assert!(!rel_is_safe("正文/./第001章.md"));
        assert!(!rel_is_safe("正文//第001章.md"));
        assert!(!rel_is_safe("/etc/passwd"));
        assert!(!rel_is_safe("C:/book/正文/第001章.md"));
        assert!(!rel_is_safe("C:正文"));
        assert!(!rel_is_safe("\\\\host\\share\\a.md"));
        assert!(!rel_is_safe("readme.md:hidden"));
        assert!(!rel_is_safe("正文/第001章.md\0.png"));
    }

    /// 相对路径入口：字符串闸之外还要过磁盘闸（盘符即使被字符串放过，
    /// validate_path 也会因为 canonicalize 后不在根内而报 PathEscape）。
    #[test]
    fn validate_rel_rejects_mailicious_forms() {
        let tmp = std::env::temp_dir();
        let root = tmp.join("storydesk-guard-test3");
        std::fs::create_dir_all(&root).unwrap();

        let ok = validate_rel(&root, "正文/第001章_开篇.md").unwrap();
        assert!(ok.starts_with(root.canonicalize().unwrap()));

        for bad in [
            "../outside.md",
            "/etc/passwd",
            "C:/book/正文/第001章.md",
            "C:正文",
            "\\\\host\\share\\a.md",
        ] {
            assert!(validate_rel(&root, bad).is_err(), "{bad} 不该被放行");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 根口径：不存在的根、文件当根，都要在入口就拒掉。
    #[test]
    fn validate_root_requires_an_existing_directory() {
        let tmp = std::env::temp_dir();
        let missing = tmp.join("storydesk-guard-test4-missing");
        assert!(validate_root(&missing).is_err());
        assert!(validate_root(Path::new("")).is_err());

        let file = tmp.join("storydesk-guard-test4-file");
        std::fs::write(&file, b"x").unwrap();
        assert!(validate_root(&file).is_err());
        let _ = std::fs::remove_file(&file);
    }
}
