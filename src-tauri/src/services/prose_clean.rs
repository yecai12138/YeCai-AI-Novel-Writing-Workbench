//! 正文格式后处理：去掉章内小标题标记。
//!
//! 模型会把细纲的「三到四幕」结构照抄进正文，产出「## 一」「## 场景一」这类 markdown
//! 标题。这些标记是写作过程的残留，不是故事内容——读者看到的应该是段落，不是提纲。
//! 这里做确定性清洗，只动格式不动文字：保留含「第 N 章」的章标题行，其余 `#`~`######`
//! 整行去掉，并把去掉后留下的连续空行收拢成一个。
//!
//! 调用点在 `commands/ai.rs` 的正文后处理（写完一章、体检之前），不回改历史正文。

use std::path::Path;

use crate::models::StoryError;
use crate::services::{atomic_write, context};

/// 清洗结果；没有可删的标记时返回 `Ok(None)`，不写盘。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadingReport {
    pub chapter: u32,
    pub path: String,
    pub removed: usize,
    /// 被删掉的行，最多留 5 条给界面看。
    pub samples: Vec<String>,
}

/// 去掉第 `chapter` 章正文里的章内小标题。
///
/// 返回 `Ok(None)`：这一章没有正文文件，或本来就没有可去的标记。
pub fn strip_chapter_headings(
    book_root: &Path,
    chapter: u32,
) -> Result<Option<HeadingReport>, StoryError> {
    let Some(path) = context::chapter_prose_path(book_root, chapter) else {
        return Ok(None);
    };
    let body = std::fs::read_to_string(&path)
        .map_err(|e| StoryError::fs(path.display().to_string(), format!("读正文失败：{e}")))?;
    let (cleaned, removed) = strip_headings(&body);
    if removed.is_empty() {
        return Ok(None);
    }
    atomic_write::write_atomic(&path, cleaned.as_bytes())?;
    let samples: Vec<String> = removed.iter().take(5).cloned().collect();
    Ok(Some(HeadingReport {
        chapter,
        path: path.display().to_string(),
        removed: removed.len(),
        samples,
    }))
}

/// 纯函数：返回清洗后的文本与被删掉的行。
///
/// 只保留「第 N 章」形态的标题行——那是书的结构，界面靠它认章；
/// 其余标题（幕序、场景名、任意自造小标题）一律算残留。
pub fn strip_headings(body: &str) -> (String, Vec<String>) {
    let mut kept: Vec<&str> = Vec::new();
    let mut removed: Vec<String> = Vec::new();
    let mut blanks = 0usize;
    for line in body.lines() {
        if let Some(text) = heading_text(line) {
            let is_chapter_title = text.starts_with('第') && text.contains('章');
            if !is_chapter_title {
                removed.push(line.trim().to_string());
                continue;
            }
        }
        if line.trim().is_empty() {
            blanks += 1;
            if blanks > 1 {
                continue;
            }
        } else {
            blanks = 0;
        }
        kept.push(line);
    }
    let mut text = kept.join("\n").trim_end().to_string();
    if !text.is_empty() {
        text.push('\n');
    }
    (text, removed)
}

/// `## 一` → `Some("一")`；`#1 号仓` 与正文里的零散 `#` 不算标题。
fn heading_text(line: &str) -> Option<&str> {
    let t = line.trim_start();
    let hashes = t.chars().take_while(|c| *c == '#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let rest = &t[hashes..];
    if rest.is_empty() {
        return Some("");
    }
    if !rest.starts_with(' ') && !rest.starts_with('\t') {
        return None;
    }
    Some(rest.trim())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn book(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "storydesk-prose-clean-{tag}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("正文")).unwrap();
        dir
    }

    #[test]
    fn keeps_the_chapter_title_and_drops_scene_headings() {
        let body = "# 第294章 这个盖你拿着\n\n## 一\n\n天亮得慢。\n\n## 二\n\n他进了屋。\n";
        let (out, removed) = strip_headings(body);
        assert_eq!(removed, vec!["## 一".to_string(), "## 二".to_string()]);
        assert_eq!(out, "# 第294章 这个盖你拿着\n\n天亮得慢。\n\n他进了屋。\n");
    }

    #[test]
    fn collapses_the_blank_run_left_behind() {
        let body = "正文一段。\n\n## 一\n\n\n下一段。\n";
        let (out, _) = strip_headings(body);
        assert_eq!(out, "正文一段。\n\n下一段。\n");
    }

    #[test]
    fn a_hash_without_a_space_is_not_a_heading() {
        let body = "#1 号仓的门开着。\n\n他数到#3 才停。\n";
        let (out, removed) = strip_headings(body);
        assert!(removed.is_empty());
        assert_eq!(out, body);
    }

    #[test]
    fn prose_without_trailing_newline_gets_exactly_one() {
        let (out, removed) = strip_headings("只有一段。");
        assert!(removed.is_empty());
        assert_eq!(out, "只有一段。\n");
    }

    #[test]
    fn writes_back_only_when_something_was_removed() {
        let root = book("write");
        let path = root.join("正文").join("第007章_试.md");
        fs::write(&path, "# 第7章 试\n\n## 一\n\n正文。\n").unwrap();
        let report = strip_chapter_headings(&root, 7).unwrap().expect("应当清洗");
        assert_eq!(report.removed, 1);
        assert_eq!(report.samples, vec!["## 一".to_string()]);
        assert_eq!(fs::read_to_string(&path).unwrap(), "# 第7章 试\n\n正文。\n");
        // 第二次没有可删的：返回 None，不重复写盘
        assert!(strip_chapter_headings(&root, 7).unwrap().is_none());
    }

    #[test]
    fn a_missing_chapter_is_silent() {
        let root = book("missing");
        assert!(strip_chapter_headings(&root, 42).unwrap().is_none());
    }
}
