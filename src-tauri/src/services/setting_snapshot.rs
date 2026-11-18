//! 设定快照：把每步都要通读的设定/追踪/大纲文件压成一份「先读这个」的浓缩版。
//!
//! 背景（R3 项 6 第二条）：`compose_agent_prompt` 每一步都要求「动笔前必须先阅读以下全局设定」，
//! 清单里是 设定/世界概览、角色状态、人物、文风、追踪/时间线、追踪/伏笔、大纲/大纲.md 的**全文**。
//! 一个 30 章的批量任务 = 30 次全新会话 × 每次重读同一批文件，其中与本步无关的句子占绝大多数。
//!
//! 现在编排器在每次 run 开始时压一份 `<书>/.storydesk/设定快照.md`：
//! - 只收标题行、清单/表格行与短句（≤ [`SHORT_LINE_CHARS`] 字），长段落留在原文；
//! - 每份文件注明「跳过 N 段长文」，不假装完整——避免模型以为快照就是全部设定；
//! - prompt 先让执行者读快照，要人名/原话/完整时间线时再回原文。
//!
//! 快照落在 `.storydesk/` 下：不进 设定/ 与 追踪/ 的文件清单（不会被当成本步目标文件），
//! 也不会被下一次快照扫描进来。源文件没变时重写会被跳过，mtime 不会被无意义地刷新。

use std::path::{Path, PathBuf};

use super::atomic_write::write_atomic;

/// 快照相对书根的路径。
pub const SNAPSHOT_REL: &str = ".storydesk/设定快照.md";

/// 整目录进来的源（书根下的目录名）。
const SOURCE_DIRS: [&str; 2] = ["设定", "追踪"];

/// 单文件进来的源（相对书根）。
const SOURCE_FILES: [&str; 2] = ["大纲/大纲.md", "记忆/摘要索引.md"];

/// 一行最多保留多少字（超出截断加省略号）。
const LINE_MAX_CHARS: usize = 120;

/// 管道行（`第3章 | 时间 | 地点 | …`）的截断宽度：一行就是一章的摘要，压到 120 只会剩下半张表。
const ROW_MAX_CHARS: usize = 180;

/// 非标题、非清单的行，多少字以内算「短句」值得收进快照。
const SHORT_LINE_CHARS: usize = 60;

/// 单个来源文件最多收多少行。
const FILE_LINE_CAP: usize = 60;

/// 单个来源文件最多占多少字（独占预算时的上限）。
const FILE_CHAR_CAP: usize = 1600;

/// 单个来源文件最少给多少字（均分后不足这个数按这个数给，宁可总长超一点也别整份丢掉）。
const MIN_FILE_CHARS: usize = 240;

/// 整份快照的字符上限（含标题与说明）。
///
/// 预算按来源文件**均分**：先把 `TOTAL_CHAR_CAP / 文件数` 当每份配额。
/// 早期版本是「先到先得」，实测《隐藏条款》18 个来源里 `设定/世界观/世界观.md`
/// 一个吃掉 6049 字，`追踪/伏笔与隐藏条款台账.md` 与 `记忆/摘要索引.md` 直接被挤掉——
/// 恰恰是续写最需要的两份。均分后每份都能露脸，长的自己截短。
const TOTAL_CHAR_CAP: usize = 8000;

/// 目录递归深度上限。
const WALK_DEPTH: usize = 3;

/// 快照生成结果（给编排层打日志、给测试断言）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotReport {
    /// 快照文件绝对路径。
    pub abs: String,
    /// 相对书根的路径（写进 prompt 用）。
    pub rel: String,
    /// 收录了几个来源文件。
    pub sources: usize,
    /// 快照正文字符数。
    pub chars: usize,
    /// 因为源文件有变化而真的写了盘。
    pub written: bool,
}

/// 快照文件路径。
pub fn snapshot_path(book_root: &Path) -> PathBuf {
    book_root.join(SNAPSHOT_REL)
}

/// 生成/刷新快照。源文件没变、内容一致时不写盘（返回 `written: false`）。
///
/// 出错一律 `None` 并打一行日志：快照只是省注入量的优化，不能因为它让整轮跑不动。
pub fn ensure(book_root: &Path) -> Option<SnapshotReport> {
    let sources = collect_sources(book_root);
    if sources.is_empty() {
        return None;
    }
    let (text, files) = render(book_root, &sources);
    let path = snapshot_path(book_root);
    // read_text 会剥 UTF-8 BOM：带 BOM 的快照不会被判成「内容变了」而反复重写。
    let existing = crate::services::atomic_write::read_text(&path).ok();
    let written = existing.as_deref() != Some(text.as_str());
    if written {
        if let Err(e) = write_atomic(&path, text.as_bytes()) {
            eprintln!("[storydesk] 设定快照写入失败（本轮退回读原文）：{e}");
            return None;
        }
    }
    Some(SnapshotReport {
        abs: path.display().to_string(),
        rel: SNAPSHOT_REL.to_string(),
        sources: files,
        chars: text.chars().count(),
        written,
    })
}

/// 收集要压缩的源文件（相对书根，正斜杠，已排序）。
fn collect_sources(book_root: &Path) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for dir in SOURCE_DIRS {
        walk(book_root, dir, 1, &mut out);
    }
    for rel in SOURCE_FILES {
        if book_root.join(rel).is_file() {
            out.push(rel.to_string());
        }
    }
    // 同名/同路径去重（目录扫描与单文件清单可能重叠），保持扫描顺序。
    out.retain(|rel| {
        if seen.iter().any(|s| s == rel) {
            false
        } else {
            seen.push(rel.clone());
            true
        }
    });
    out
}

fn walk(book_root: &Path, rel: &str, depth: usize, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(book_root.join(rel)) else {
        return;
    };
    let mut names: Vec<_> = entries.flatten().collect();
    names.sort_by_key(|e| e.file_name());
    for entry in names {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        let child = format!("{rel}/{name}");
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            if depth < WALK_DEPTH {
                walk(book_root, &child, depth + 1, out);
            }
        } else if name.to_ascii_lowercase().ends_with(".md") {
            out.push(child);
        }
    }
}

/// 拼出整份快照；返回 (正文, 实际收录的文件数)。
fn render(book_root: &Path, sources: &[String]) -> (String, usize) {
    let mut text = String::new();
    text.push_str("# 设定快照（自动生成，勿手改）\n\n");
    text.push_str(
        "> 本文件由编排器在每次运行开始时从 设定/、追踪/、大纲/大纲.md、记忆/摘要索引.md 重新生成：\n\
         > 只保留标题、清单与短句，长段落留在原文。\n\
         > 需要人名、原话、完整时间线与细节时，读下面的来源文件——不要拿快照当全文引用。\n\n",
    );
    text.push_str("来源：");
    text.push_str(&sources.join("、"));
    text.push_str("\n");

    let mut used = 0usize;
    let mut rest: Vec<String> = Vec::new();
    // 预算按权重分配：追踪/ 与 记忆/ 是每章都在变、又决定连贯性的部分，给双份。
    let weights: Vec<usize> = sources.iter().map(|rel| source_weight(rel)).collect();
    let total_weight: usize = weights.iter().sum();
    // 高权重的先排：总预算有限时，先保证追踪/记忆进得来，剩下的余量再给设定。
    let mut order: Vec<usize> = (0..sources.len()).collect();
    order.sort_by_key(|idx| (std::cmp::Reverse(weights[*idx]), *idx));
    for idx in order {
        let rel = &sources[idx];
        // 同上：非 UTF-8 / 读失败的设定文件以前被静默丢掉，来源清单里却还列着它。
        let Ok(body) = crate::services::atomic_write::read_text(&book_root.join(rel)) else {
            continue;
        };
        let budget = per_file_budget(weights[idx], total_weight);
        let (kept, dropped) = condense(&body);
        let mut block = String::new();
        block.push_str(&format!("\n## {rel}\n"));
        block.push_str(&format!(
            "（原文 {} 字；收录 {} 行，跳过 {} 段长文）\n",
            body.chars().count(),
            kept.len(),
            dropped
        ));
        let mut lines_out = 0usize;
        for line in &kept {
            let cost = line.chars().count() + 1;
            if block.chars().count() + cost > budget {
                break;
            }
            block.push_str(line);
            block.push('\n');
            lines_out += 1;
        }
        if lines_out < kept.len() && lines_out > 0 {
            block.push_str("（本文件按配额截断，其余要点读原文）\n");
        }
        if lines_out == 0 {
            block.push_str("（配额不足，未收录，请直接读原文）\n");
        }
        if text.chars().count() + block.chars().count() > TOTAL_CHAR_CAP {
            rest.push(rel.clone());
            continue;
        }
        used += 1;
        text.push_str(&block);
    }
    if !rest.is_empty() {
        text.push_str(&format!(
            "\n（快照已达 {TOTAL_CHAR_CAP} 字上限，以下文件未收录，请直接读原文：{}）\n",
            rest.join("、")
        ));
    }
    (text, used)
}

/// 每份来源文件的字数配额：按权重分总预算，夹在 [最小, 单份上限] 之间。
fn per_file_budget(weight: usize, total_weight: usize) -> usize {
    let share = TOTAL_CHAR_CAP * weight.max(1) / total_weight.max(1);
    share.clamp(MIN_FILE_CHARS, FILE_CHAR_CAP)
}

/// 来源权重：追踪/ 、记忆/ 与 大纲/大纲.md 给双份预算。
///
/// 设定/ 是稳定的世界观底稿，缺了大不了回原文；追踪/伏笔台账、角色状态、摘要索引
/// 是每章都在变的部分，大纲/大纲.md 则是每步都必须遵守的契约——实测按均分它们
/// 只能分到几百字，全被长设定挤成一句。
fn source_weight(rel: &str) -> usize {
    if rel.starts_with("追踪/") || rel.starts_with("记忆/") || rel == "大纲/大纲.md" {
        2
    } else {
        1
    }
}

/// 把一份文件压成「标题 + 清单行 + 短句」。
///
/// 返回 (保留的行, 跳过的长段落数)。代码块整段跳过——设定文件里夹的 JSON/模板
/// 摘一半比不摘更容易误导。
fn condense(body: &str) -> (Vec<String>, usize) {
    let mut kept: Vec<String> = Vec::new();
    let mut dropped = 0usize;
    let mut in_code = false;
    for raw in body.lines() {
        let line = raw.trim_end();
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_code = !in_code;
            continue;
        }
        if in_code || trimmed.is_empty() {
            continue;
        }
        if kept.len() >= FILE_LINE_CAP {
            dropped += 1;
            continue;
        }
        let is_heading = trimmed.starts_with('#');
        let is_list = is_list_line(trimmed);
        let is_table_row = trimmed.matches('|').count() >= 2;
        let short = trimmed.chars().count() <= SHORT_LINE_CHARS;
        if !(is_heading || is_list || is_table_row || short) {
            dropped += 1;
            continue;
        }
        // 管道行（摘要索引/台账的一行一章）本身很长但全是信息，放宽截断宽度。
        let cap = if is_table_row {
            ROW_MAX_CHARS
        } else {
            LINE_MAX_CHARS
        };
        kept.push(truncate(trimmed, cap));
    }
    (kept, dropped)
}

fn is_list_line(trimmed: &str) -> bool {
    let mut chars = trimmed.chars();
    match chars.next() {
        Some('-' | '*' | '+' | '|' | '>') => true,
        Some(c) if c.is_ascii_digit() => {
            let rest: String = chars.collect();
            rest.contains(". ") || rest.contains('、') || rest.contains(')')
        }
        _ => false,
    }
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let head: String = text.chars().take(max.saturating_sub(1)).collect();
    format!("{head}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn book(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let root = std::env::temp_dir().join(format!("storydesk-snap-{name}-{nanos}"));
        std::fs::create_dir_all(root.join("设定")).unwrap();
        root
    }

    #[test]
    fn snapshot_keeps_headings_and_lists_and_drops_long_paragraphs() {
        let root = book("condense");
        std::fs::write(
            root.join("设定/角色状态.md"),
            "# 角色状态\n\n- 陈砚：项目经理，欠款 30 万\n* 苏晚：法务\n\n这是一段很长的人物小传，\
             讲的是他小时候在县城长大、后来考进省城大学、毕业进了公司、一路做到项目经理，\
             中间还牵扯到父亲工伤赔偿那笔钱到底去了哪里，写得很细很长，不适合塞进快照。\n",
        )
        .unwrap();
        let rep = ensure(&root).expect("有源文件就该出快照");
        assert!(rep.written);
        let text = std::fs::read_to_string(snapshot_path(&root)).unwrap();
        assert!(text.contains("## 设定/角色状态.md"), "{text}");
        assert!(text.contains("- 陈砚：项目经理，欠款 30 万"));
        assert!(text.contains("* 苏晚：法务"));
        assert!(text.contains("# 角色状态"));
        assert!(text.contains("跳过 1 段长文"), "{text}");
        assert!(!text.contains("父亲工伤赔偿"), "长段落应该留在原文");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn snapshot_is_skipped_when_sources_did_not_change() {
        let root = book("idempotent");
        std::fs::write(root.join("设定/人物.md"), "- 陈砚\n- 苏晚\n").unwrap();
        let first = ensure(&root).expect("第一次");
        assert!(first.written);
        let second = ensure(&root).expect("第二次");
        assert!(!second.written, "源文件没变不该重写");
        std::fs::write(root.join("设定/人物.md"), "- 陈砚\n- 苏晚\n- 老周\n").unwrap();
        let third = ensure(&root).expect("第三次");
        assert!(third.written, "源文件变了必须重写");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn snapshot_includes_tracker_outline_and_summary_index() {
        let root = book("sources");
        std::fs::write(root.join("设定/角色状态.md"), "- 陈砚\n").unwrap();
        std::fs::create_dir_all(root.join("追踪")).unwrap();
        std::fs::write(root.join("追踪/伏笔.md"), "- 欠条在抽屉夹层（第 3 章埋）\n").unwrap();
        std::fs::create_dir_all(root.join("大纲")).unwrap();
        std::fs::write(root.join("大纲/大纲.md"), "# 全书大纲\n第一幕：讨债\n").unwrap();
        std::fs::create_dir_all(root.join("记忆")).unwrap();
        std::fs::write(root.join("记忆/摘要索引.md"), "# 摘要索引\n").unwrap();
        let rep = ensure(&root).expect("有源文件");
        assert!(rep.sources >= 4, "来源数 {}", rep.sources);
        let text = std::fs::read_to_string(snapshot_path(&root)).unwrap();
        assert!(text.contains("追踪/伏笔.md"), "{text}");
        assert!(text.contains("大纲/大纲.md"), "{text}");
        assert!(text.contains("记忆/摘要索引.md"), "{text}");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn snapshot_is_none_for_a_book_without_any_source() {
        let root = book("empty");
        assert!(ensure(&root).is_none());
        assert!(!snapshot_path(&root).exists());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn snapshot_never_swallows_itself() {
        let root = book("self");
        std::fs::write(root.join("设定/角色状态.md"), "- 陈砚\n").unwrap();
        ensure(&root).expect("第一次");
        let rep = ensure(&root).expect("第二次");
        let text = std::fs::read_to_string(snapshot_path(&root)).unwrap();
        assert!(!text.contains("## .storydesk/设定快照.md"), "{text}");
        assert_eq!(rep.sources, 1);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn long_lines_are_truncated_not_dropped_from_a_list() {
        let root = book("truncate");
        let long = format!("- {}", "很".repeat(300));
        std::fs::write(root.join("设定/文风.md"), &long).unwrap();
        ensure(&root).expect("快照");
        let text = std::fs::read_to_string(snapshot_path(&root)).unwrap();
        assert!(text.contains('…'), "{text}");
        assert!(
            text.lines().all(|l| l.chars().count() <= LINE_MAX_CHARS),
            "每行都不该超过 {LINE_MAX_CHARS} 字"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn code_blocks_are_left_out_whole() {
        let root = book("codefence");
        std::fs::write(
            root.join("设定/文风.md"),
            "# 文风\n```json\n{\"a\": 1}\n```\n- 短句收尾\n",
        )
        .unwrap();
        ensure(&root).expect("快照");
        let text = std::fs::read_to_string(snapshot_path(&root)).unwrap();
        assert!(!text.contains("\"a\": 1"), "{text}");
        assert!(text.contains("- 短句收尾"));
        std::fs::remove_dir_all(&root).ok();
    }

    /// 摘要索引的一行一章远超过短句阈值，但它就是全章摘要，不能被当「长段落」丢掉。
    #[test]
    fn summary_index_rows_survive_condensing() {
        let root = book("rows");
        std::fs::create_dir_all(root.join("记忆")).unwrap();
        std::fs::write(
            root.join("记忆/摘要索引.md"),
            "# 摘要索引\n\
             第8章 | 第9天下午 | 长明大厦仓库 | 陈砚、王浩 | （自动摘录）开场：十一点半，仓库门口的雨棚还在滴水。收尾：他把伞收了。 | 王浩的去向 | 旧合同的签名 | 灯灭之前必须拿到名册\n\
             第9章 | 第10天上午 | 法援社 | 苏晚 | 苏晚翻出七年前的旧档，发现签名页被人换过，复印件在沈鹤手里。 | 签名页 | 无名录 | 沈鹤约见\n",
        )
        .unwrap();
        ensure(&root).expect("快照");
        let text = std::fs::read_to_string(snapshot_path(&root)).unwrap();
        assert!(text.contains("第8章 | 第9天下午"), "{text}");
        assert!(text.contains("第9章 | 第10天上午"), "{text}");
        assert!(!text.contains("跳过 2 段长文"), "{text}");
        std::fs::remove_dir_all(&root).ok();
    }

    /// 追踪/记忆 的配额是设定文件的两倍：续写靠的是它们。
    #[test]
    fn tracker_and_memory_get_more_budget_than_settings() {
        let root = book("weight");
        std::fs::create_dir_all(root.join("追踪")).unwrap();
        std::fs::create_dir_all(root.join("记忆")).unwrap();
        std::fs::write(
            root.join("设定/世界观.md"),
            format!("# 世界观\n{}", "- 设定条目\n".repeat(200)),
        )
        .unwrap();
        std::fs::write(
            root.join("追踪/伏笔.md"),
            format!("# 伏笔\n{}", "- 第3章埋：欠条在抽屉夹层\n".repeat(60)),
        )
        .unwrap();
        std::fs::write(
            root.join("记忆/摘要索引.md"),
            format!("# 摘要索引\n{}", "- 摘要行\n".repeat(60)),
        )
        .unwrap();
        ensure(&root).expect("快照");
        let text = std::fs::read_to_string(snapshot_path(&root)).unwrap();
        let tracker = text
            .split("## 追踪/伏笔.md")
            .nth(1)
            .and_then(|rest| rest.split("## ").next())
            .unwrap_or_default();
        let setting = text
            .split("## 设定/世界观.md")
            .nth(1)
            .and_then(|rest| rest.split("## ").next())
            .unwrap_or_default();
        assert!(
            tracker.chars().count() > setting.chars().count(),
            "追踪 {} 字 vs 设定 {} 字",
            tracker.chars().count(),
            setting.chars().count()
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// 真实书验证（默认不跑）：
    ///
    /// ```powershell
    /// $env:STORYDESK_BOOK="D:\桌面\大制作\xs\我的作品集\长篇\《隐藏条款》"
    /// cargo test --lib setting_snapshot_real_book -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore]
    fn setting_snapshot_real_book_from_env() {
        let Ok(book) = std::env::var("STORYDESK_BOOK") else {
            println!("跳过：需要 STORYDESK_BOOK");
            return;
        };
        let book = PathBuf::from(book);
        let sources = collect_sources(&book);
        let origin: usize = sources
            .iter()
            .filter_map(|rel| crate::services::atomic_write::read_text(&book.join(rel)).ok())
            .map(|body| body.chars().count())
            .sum();
        let first = ensure(&book).expect("有源文件就该出快照");
        let second = ensure(&book).expect("第二次");
        println!(
            "来源 {} 个、原文 {} 字 → 快照 {} 字（压缩到 {:.0}%），第一次 written={}，第二次 written={}",
            first.sources,
            origin,
            first.chars,
            if origin == 0 { 0.0 } else { first.chars as f64 * 100.0 / origin as f64 },
            first.written,
            second.written
        );
        for rel in &sources {
            println!("  - {rel}");
        }
        assert!(first.chars > 0);
    }
}
