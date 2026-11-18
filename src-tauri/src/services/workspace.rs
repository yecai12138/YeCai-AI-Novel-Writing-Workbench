//! 工作区与书目录：新建 / 打开 / 目录树 / 目录选择器
//!
//! 目录布局以 oh-story `docs/architecture.md` 为准：
//! - 工作区根（用户选的文件夹）：`.active-book`、`.storydesk/`、`长篇/<书名>/`
//! - 书根（`长篇/<书名>/`）：`设定/ 大纲/ 正文/ 追踪/ 对标/ 参考资料/ .story/`
//!
//! 应用私有数据放在 `.storydesk/`；`.story/` 归 oh-story（作者记忆、排纲笔记、章节草稿区），
//! 本模块只为它创建空目录，绝不写入内容。

use std::fs;
use std::path::{Path, PathBuf};

use crate::models::dto::{DirEntryInfo, DirListing, ProjectInfo, TreeNode, TreeNodeKind};
use crate::models::StoryError;

use super::path_guard::validate_path;

/// 当前活跃书的指针文件名。内容是**相对工作区根**的路径，如 `长篇/我的小说`
pub const ACTIVE_BOOK_FILE: &str = ".active-book";
/// 应用私有目录名
pub const APP_DIR: &str = ".storydesk";
/// 长篇小说目录名
pub const LONG_DIR: &str = "长篇";
/// 短篇小说目录名
pub const SHORT_DIR: &str = "短篇";

/// 书根下要建的目录（相对书根），顺序即创建顺序。
const BOOK_DIRS: &[&str] = &[
    "设定",
    "设定/世界观",
    "设定/角色",
    "设定/势力",
    "大纲",
    "正文",
    "追踪",
    "追踪/逐章记录",
    "追踪/角色状态",
    "追踪/时间线",
    "对标",
    "参考资料",
    ".story/排纲笔记",
];

/// 工作区根下要建的应用私有目录。
const APP_DIRS: &[&str] = &["snapshots", "runs", "logs", "prompts/templates"];

/// 目录树里跳过的名字：应用私有区、oh-story 内部草稿区、VCS / 依赖 / 系统垃圾。
const SKIP_NAMES: &[&str] = &[
    ".git",
    ".storydesk",
    ".story",
    "node_modules",
    "target",
    ".DS_Store",
    "Thumbs.db",
];

const TPL_SYNOPSIS: &str = "# 全书梗概\n\n> 一句话核心冲突：\n> 主角初始状态 → 最终状态：\n\n## 三幕结构\n\n1. 起：\n2. 承：\n3. 转 / 合：\n";
const TPL_STYLE: &str = "# 文风\n\n> 本书的叙事人称、句式节奏、禁用词、章末钩子习惯。oh-story 的写作流程会读这个文件。\n\n- 人称：\n- 句式节奏：\n- 禁用词 / 毒句式：\n- 章末钩子习惯：\n";
const TPL_OUTLINE: &str =
    "# 大纲\n\n> 全书主线与卷级推进。细纲另存 `大纲/细纲_第NNN章.md`。\n\n## 主线\n\n## 分卷\n\n";
const TPL_GENRE: &str = "# 题材定位\n\n- 主对标书：\n- 目标读者：\n- 核心卖点：\n- 情绪基调：\n";
const TPL_APP_CONFIG: &str = "{\n  \"app\": \"storydesk\",\n  \"version\": 1,\n  \"bookName\": \"\",\n  \"engine\": {\n    \"kind\": \"host-cli\",\n    \"command\": \"\",\n    \"args\": []\n  },\n  \"autosave\": {\n    \"enabled\": true,\n    \"delayMs\": 1200\n  }\n}\n";

/// 文件名里不允许出现的字符（Windows 规则 + 手工排掉控制字符）
const ILLEGAL_NAME_CHARS: &[char] = &['\\', '/', ':', '*', '?', '"', '<', '>', '|'];

// ---------------------------------------------------------------- 新建 / 打开

/// 校验并规范化书名。书名的最终形态会直接作为目录名，所以规则从严。
pub fn clean_book_name(book_name: &str) -> Result<String, StoryError> {
    let name = book_name.trim();

    if name.is_empty() {
        return Err(StoryError::invalid("bookName", "请填写书名"));
    }
    if name.chars().count() > 80 {
        return Err(StoryError::invalid("bookName", "书名过长（上限 80 字）"));
    }
    if name == "." || name == ".." {
        return Err(StoryError::invalid("bookName", "书名不能是 . 或 .."));
    }
    if let Some(bad) = name.chars().find(|c| ILLEGAL_NAME_CHARS.contains(c)) {
        return Err(StoryError::invalid(
            "bookName",
            format!("书名不能包含 `{bad}` 等路径非法字符"),
        ));
    }
    if name.ends_with('.') || name.ends_with(' ') {
        return Err(StoryError::invalid("bookName", "书名不能以点或空格结尾"));
    }

    Ok(name.to_string())
}

/// 新建项目：建 `长篇/<书名>/` 全套目录 + `.storydesk/` + 模板 + `.active-book`。
pub fn create_project(workspace_root: &Path, book_name: &str) -> Result<ProjectInfo, StoryError> {
    let name = clean_book_name(book_name)?;

    fs::create_dir_all(workspace_root).map_err(|e| {
        StoryError::fs(
            workspace_root.display().to_string(),
            format!("无法创建项目目录: {e}"),
        )
    })?;
    let root = canonical_dir(workspace_root, "项目目录不可访问")?;

    let book_root = root.join(LONG_DIR).join(&name);
    fs::create_dir_all(&book_root).map_err(|e| {
        StoryError::fs(
            book_root.display().to_string(),
            format!("创建书目录失败: {e}"),
        )
    })?;

    for rel in BOOK_DIRS {
        let dir = book_root.join(rel);
        fs::create_dir_all(&dir)
            .map_err(|e| StoryError::fs(dir.display().to_string(), format!("创建目录失败: {e}")))?;
    }

    let app_dir = root.join(APP_DIR);
    for rel in APP_DIRS {
        let dir = app_dir.join(rel);
        fs::create_dir_all(&dir).map_err(|e| {
            StoryError::fs(dir.display().to_string(), format!("创建应用目录失败: {e}"))
        })?;
    }
    let config_path = app_dir.join("config.json");
    if !config_path.exists() {
        let config =
            TPL_APP_CONFIG.replace("\"bookName\": \"\"", &format!("\"bookName\": \"{name}\""));
        write_file(&config_path, &config)?;
    }

    write_template(&book_root.join("设定/全书梗概.md"), TPL_SYNOPSIS)?;
    write_template(&book_root.join("设定/文风.md"), TPL_STYLE)?;
    write_template(&book_root.join("设定/题材定位.md"), TPL_GENRE)?;
    write_template(&book_root.join("大纲/大纲.md"), TPL_OUTLINE)?;

    let active_book = format!("{LONG_DIR}/{name}");
    // `.active-book` 的内容是「相对工作区根的路径」，oh-story 的 hook 与写作 skill 靠它定位当前书。
    // 不写尾随换行：钩子脚本会直接 cat 这个文件拼路径。
    write_file(&root.join(ACTIVE_BOOK_FILE), &active_book)?;

    Ok(ProjectInfo {
        workspace_dir: root.display().to_string(),
        book_root: book_root.display().to_string(),
        book_name: name,
        active_book,
        created: true,
    })
}

/// 打开项目：优先读 `.active-book`；缺失时按 `长篇/` 下唯一书目回填。
pub fn open_project(workspace_root: &Path) -> Result<ProjectInfo, StoryError> {
    let root = canonical_dir(workspace_root, "项目目录不可访问")?;

    if let Some(active_book) = read_active_book(&root)? {
        let book_root = resolve_book_root(&root, &active_book)?;
        return Ok(ProjectInfo {
            workspace_dir: root.display().to_string(),
            book_root: book_root.display().to_string(),
            book_name: book_name_of(&book_root),
            active_book,
            created: false,
        });
    }

    // 兼容：已经是 oh-story 项目但没有 .active-book 时，用 长篇/ 下唯一书目回填
    if let Some(active_book) = find_single_book(&root)? {
        write_file(&root.join(ACTIVE_BOOK_FILE), &active_book)?;
        let book_root = resolve_book_root(&root, &active_book)?;
        return Ok(ProjectInfo {
            workspace_dir: root.display().to_string(),
            book_root: book_root.display().to_string(),
            book_name: book_name_of(&book_root),
            active_book,
            created: false,
        });
    }

    Err(StoryError::invalid(
        "workspaceDir",
        "该目录既没有 .active-book，也没有 长篇/<书名>/ 结构。请先新建项目，或选择包含 .active-book 的项目目录。",
    ))
}

/// 切换当前书：重写 `.active-book`（内容必须是相对路径）。
pub fn set_active_book(
    workspace_root: &Path,
    active_book: &str,
) -> Result<ProjectInfo, StoryError> {
    let root = canonical_dir(workspace_root, "项目目录不可访问")?;
    let normalized = normalize_rel(active_book);

    if normalized.is_empty() {
        return Err(StoryError::invalid("activeBook", "activeBook 不能为空"));
    }
    let book_root = resolve_book_root(&root, &normalized)?;
    write_file(&root.join(ACTIVE_BOOK_FILE), &normalized)?;

    Ok(ProjectInfo {
        workspace_dir: root.display().to_string(),
        book_root: book_root.display().to_string(),
        book_name: book_name_of(&book_root),
        active_book: normalized,
        created: false,
    })
}

/// 读 `.active-book`，返回规范化后的相对路径；文件不存在或为空时返回 None。
pub fn read_active_book(workspace_root: &Path) -> Result<Option<String>, StoryError> {
    let path = workspace_root.join(ACTIVE_BOOK_FILE);
    if !path.is_file() {
        return Ok(None);
    }
    let raw = fs::read_to_string(&path).map_err(|e| {
        StoryError::fs(
            path.display().to_string(),
            format!("读取 .active-book 失败: {e}"),
        )
    })?;
    let normalized = normalize_rel(&raw);
    if normalized.is_empty() {
        return Ok(None);
    }
    Ok(Some(normalized))
}

// ---------------------------------------------------------------- 书目录枚举

/// 列出工作区内所有可识别的书（`长篇/<x>` / `短篇/<x>`，必须含书特征目录）。
pub fn list_books(workspace_root: &Path) -> Result<Vec<String>, StoryError> {
    let root = canonical_dir(workspace_root, "项目目录不可访问")?;
    let mut books = Vec::new();

    for group in [LONG_DIR, SHORT_DIR] {
        let group_dir = root.join(group);
        if !group_dir.is_dir() {
            continue;
        }
        let entries = match fs::read_dir(&group_dir) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() || is_symlink(&entry) {
                continue;
            }
            if is_book_dir(&path) {
                books.push(format!("{group}/{}", entry.file_name().to_string_lossy()));
            }
        }
    }

    books.sort_by(|a, b| natural_cmp(a, b));
    Ok(books)
}

/// 书特征：至少有一个 oh-story 的标记目录
fn is_book_dir(path: &Path) -> bool {
    path.join("设定").is_dir() || path.join("正文").is_dir() || path.join("大纲").is_dir()
}

fn find_single_book(workspace_root: &Path) -> Result<Option<String>, StoryError> {
    let books = list_books(workspace_root)?;
    if books.len() == 1 {
        Ok(Some(books.into_iter().next().unwrap_or_default()))
    } else {
        Ok(None)
    }
}

fn resolve_book_root(workspace_root: &Path, active_book: &str) -> Result<PathBuf, StoryError> {
    let raw = PathBuf::from(active_book);
    if raw.is_absolute() {
        return Err(StoryError::invalid(
            "activeBook",
            ".active-book 必须是相对工作区根的路径，不能是绝对路径",
        ));
    }

    let joined = workspace_root.join(&raw);
    let resolved = validate_path(workspace_root, &joined)?;
    if !resolved.is_dir() {
        return Err(StoryError::fs(
            resolved.display().to_string(),
            format!("`.active-book` 指向的书目录不存在: {active_book}"),
        ));
    }
    Ok(resolved)
}

fn book_name_of(book_root: &Path) -> String {
    book_root
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "未命名".to_string())
}

// ---------------------------------------------------------------- 目录树

/// 书根下的完整目录树。跳过 `.story` / `.storydesk` / VCS，绝不跟随符号链接。
pub fn build_tree(book_root: &Path) -> Result<TreeNode, StoryError> {
    let root = canonical_dir(book_root, "书目录不存在")?;
    let name = book_name_of(&root);

    let mut node = TreeNode {
        name,
        path: root.display().to_string(),
        kind: TreeNodeKind::Dir,
        rel: String::new(),
        children: Vec::new(),
        size: None,
    };

    // 防御：异常深的目录或超大仓库不应把内存吃光
    let mut budget: usize = 6000;
    fill_dir(&root, &root, &mut node, 0, &mut budget);

    Ok(node)
}

fn fill_dir(root: &Path, dir: &Path, node: &mut TreeNode, depth: usize, budget: &mut usize) {
    if depth >= 10 || *budget == 0 {
        return;
    }

    let Ok(rd) = fs::read_dir(dir) else {
        return;
    };

    let mut entries: Vec<(String, PathBuf, bool, Option<u64>)> = Vec::new();
    for entry in rd.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if SKIP_NAMES.contains(&name.as_str()) {
            continue;
        }
        let path = entry.path();
        let Ok(ft) = entry.file_type() else {
            continue;
        };
        // 不跟随符号链接，避免树里出现指向工作区外的节点
        if ft.is_symlink() {
            continue;
        }
        if ft.is_dir() {
            entries.push((name, path, true, None));
        } else if ft.is_file() {
            let size = entry.metadata().ok().map(|m| m.len());
            entries.push((name, path, false, size));
        }
    }

    entries.sort_by(|a, b| {
        // 目录优先，其次按自然序
        b.2.cmp(&a.2).then_with(|| natural_cmp(&a.0, &b.0))
    });

    for (name, path, is_dir, size) in entries {
        if *budget == 0 {
            return;
        }
        *budget -= 1;

        let rel_path = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .display()
            .to_string()
            .replace('\\', "/");

        let mut child = TreeNode {
            name,
            path: path.display().to_string(),
            kind: if is_dir {
                TreeNodeKind::Dir
            } else {
                TreeNodeKind::File
            },
            rel: rel_path,
            children: Vec::new(),
            size,
        };

        if is_dir {
            fill_dir(root, &path, &mut child, depth + 1, budget);
        }
        node.children.push(child);
    }
}

// ---------------------------------------------------------------- 目录选择器

/// 列出某个目录下的子目录，供「新建项目 / 打开项目」选择文件夹。空字符串表示列举盘符。
pub fn list_dirs(path: &str) -> Result<DirListing, StoryError> {
    let trimmed = path.trim();

    if trimmed.is_empty() {
        let mut entries = Vec::new();
        for letter in b'C'..=b'Z' {
            let candidate = format!("{}:\\", letter as char);
            if Path::new(&candidate).exists() {
                entries.push(DirEntryInfo {
                    name: format!("{}:", letter as char),
                    path: candidate,
                    is_empty: false,
                });
            }
        }
        return Ok(DirListing {
            current: String::new(),
            parent: None,
            entries,
        });
    }

    let current = canonical_dir(Path::new(trimmed), "目录不可访问")?;
    let mut entries = Vec::new();

    if let Ok(rd) = fs::read_dir(&current) {
        for entry in rd.flatten() {
            let Ok(ft) = entry.file_type() else {
                continue;
            };
            if !ft.is_dir() || ft.is_symlink() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            let path = entry.path();
            let is_empty = fs::read_dir(&path)
                .map(|it| it.take(1).count() == 0)
                .unwrap_or(false);
            entries.push(DirEntryInfo {
                name,
                path: path.display().to_string(),
                is_empty,
            });
        }
    }

    entries.sort_by(|a, b| natural_cmp(&a.name, &b.name));

    Ok(DirListing {
        current: current.display().to_string(),
        parent: current.parent().map(|p| p.display().to_string()),
        entries,
    })
}

// ---------------------------------------------------------------- 工具函数

fn canonical_dir(path: &Path, message: &str) -> Result<PathBuf, StoryError> {
    let resolved = fs::canonicalize(path)
        .map_err(|e| StoryError::fs(path.display().to_string(), format!("{message}: {e}")))?;
    Ok(strip_verbatim(resolved))
}

/// Windows 的 canonicalize 会带 `\\?\` 前缀，直接展示给用户很难看；去掉它。
///
/// 公开给 `commands::ai`：送进 CLI 的 prompt 里必须是干净路径，否则宿主模型
/// 读到的「项目目录」会带一层 verbatim 前缀。
pub fn strip_verbatim(path: PathBuf) -> PathBuf {
    let raw = path.as_os_str().to_string_lossy().to_string();
    if let Some(rest) = raw.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    if let Some(rest) = raw.strip_prefix(r"\\?\") {
        return PathBuf::from(rest.to_string());
    }
    path
}

fn is_symlink(entry: &fs::DirEntry) -> bool {
    entry.file_type().map(|ft| ft.is_symlink()).unwrap_or(false)
}

fn normalize_rel(raw: &str) -> String {
    raw.trim()
        .trim_start_matches('\u{feff}')
        .replace('\\', "/")
        .trim_matches('/')
        .to_string()
}

fn write_file(path: &Path, content: &str) -> Result<(), StoryError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| {
            StoryError::fs(parent.display().to_string(), format!("创建目录失败: {e}"))
        })?;
    }
    fs::write(path, content.as_bytes())
        .map_err(|e| StoryError::fs(path.display().to_string(), format!("写入文件失败: {e}")))
}

/// 模板只在不存在时写入，避免覆盖用户已经改过的内容
fn write_template(path: &Path, content: &str) -> Result<(), StoryError> {
    if path.exists() {
        return Ok(());
    }
    write_file(path, content)
}

/// 自然排序：把 `第2章` 排在 `第10章` 前面。
pub fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;

    let left: Vec<char> = a.chars().collect();
    let right: Vec<char> = b.chars().collect();
    let (mut i, mut j) = (0usize, 0usize);

    while i < left.len() && j < right.len() {
        if left[i].is_ascii_digit() && right[j].is_ascii_digit() {
            let (num_a, next_i) = read_number(&left, i);
            let (num_b, next_j) = read_number(&right, j);
            if num_a != num_b {
                return num_a.cmp(&num_b);
            }
            if next_i != next_j {
                return next_i.cmp(&next_j);
            }
            i = next_i;
            j = next_j;
            continue;
        }

        let ca = left[i].to_ascii_lowercase();
        let cb = right[j].to_ascii_lowercase();
        if ca != cb {
            return ca.cmp(&cb);
        }
        i += 1;
        j += 1;
    }

    left.len().cmp(&right.len()).then_with(|| Ordering::Equal)
}

fn read_number(chars: &[char], start: usize) -> (u128, usize) {
    let mut value: u128 = 0;
    let mut index = start;
    while index < chars.len() && chars[index].is_ascii_digit() {
        value = value * 10 + (chars[index] as u8 - b'0') as u128;
        index += 1;
    }
    (value, index)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn book_name_rules() {
        assert!(clean_book_name("  山海不夜  ").is_ok());
        assert_eq!(clean_book_name(" 山海不夜 ").unwrap(), "山海不夜");
        assert!(clean_book_name("").is_err());
        assert!(clean_book_name("..").is_err());
        assert!(clean_book_name("a/b").is_err());
        assert!(clean_book_name("a:b").is_err());
        assert!(clean_book_name("书名. ").is_err());
    }

    #[test]
    fn natural_order() {
        let mut names = vec!["第10章", "第2章", "第1章"];
        names.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(names, vec!["第1章", "第2章", "第10章"]);
    }

    #[test]
    fn create_and_open_roundtrip() {
        let root = std::env::temp_dir().join(format!("storydesk-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);

        let info = create_project(&root, "测试书").expect("create_project 失败");
        assert!(Path::new(&info.book_root).join("正文").is_dir());
        assert!(root.join(".storydesk").join("snapshots").is_dir());

        let active = fs::read_to_string(root.join(ACTIVE_BOOK_FILE)).unwrap();
        assert_eq!(active, "长篇/测试书");
        assert!(!active.ends_with('\n'));

        let opened = open_project(&root).expect("open_project 失败");
        assert_eq!(opened.active_book, "长篇/测试书");
        assert_eq!(opened.book_name, "测试书");

        let tree = build_tree(Path::new(&opened.book_root)).expect("build_tree 失败");
        assert!(tree.children.iter().any(|c| c.name == "正文"));

        let _ = fs::remove_dir_all(&root);
    }
}
