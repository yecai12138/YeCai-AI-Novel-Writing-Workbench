//! 滚动摘要索引（`记忆/摘要索引.md`）的唯一写入方。
//!
//! 背景：摘要行原先靠写手在任务末尾「自觉」更新（intent.rs 的【章节摘要】条款），
//! 但没有任何代码校验它到底写了没有。实测《隐藏条款》13 章正文只有 10 行摘要，
//! 第 8/12/13 章缺失——`prev_chapter_digest` 的跨章前情提要就此断供，全程无人知晓。
//!
//! 现在所有权收回编排器：
//! 1. 写手只在**回复正文**里给一行摘要（不再写文件，见 intent.rs 条款）；
//! 2. 每步正文验收通过后由 [`sync_chapter`] 解析回复并 upsert 进索引（atomic_write）；
//! 3. 回复里没有可用摘要行时退化为「正文开头 + 结尾」摘录，缺失字段留空，行照样落盘；
//! 4. [`missing_chapters`] / [`backfill`] 供编排层做缺口告警与一次性补齐。
//!
//! 行格式固定 8 列：`第N章 | 故事时间 | 地点 | 出场人物 | 关键事件（40字内） | 新埋伏笔 | 回收伏笔 | 结尾钩子`。

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use regex::Regex;

use super::atomic_write::{read_text, write_atomic};
use super::context;
use crate::models::StoryError;

/// 摘要索引相对书根的路径。
pub const SUMMARY_INDEX_REL: &str = "记忆/摘要索引.md";

/// 索引标题行（文件里已有标题就不重复加）。
pub const INDEX_TITLE: &str = "# 摘要索引";

/// 每行列数：章 / 时间 / 地点 / 人物 / 事件 / 新埋伏笔 / 回收伏笔 / 结尾钩子。
///
/// 是 8 列不是 7：intent.rs 的格式串是「第N章 | 故事时间 | 地点 | 出场人物 |
/// 关键事件 | 新埋伏笔 | 回收伏笔 | 结尾钩子」，章号也算一列；按 7 截会把钩子列切掉。
pub const ROW_COLUMNS: usize = 8;

/// 退化摘录取正文开头多少字（去空白后）。
const FALLBACK_HEAD_CHARS: usize = 40;

/// 退化摘录取正文结尾多少字（去空白后）。
const FALLBACK_TAIL_CHARS: usize = 60;

/// 退化摘录写进「事件」列时的前缀，方便人一眼看出这行不是模型写的。
pub const FALLBACK_MARK: &str = "（自动摘录）";

/// 这一行摘要是怎么来的。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowSource {
    /// 写手回复里给了摘要行，原样（规整列数后）收录。
    Writer,
    /// 写手没给，退化为正文开头+结尾摘录。
    Prose,
    /// 索引里已经有这一章的行，这一步没有新内容可写。
    AlreadyPresent,
}

/// 一次同步的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncReport {
    pub chapter: u32,
    pub source: RowSource,
    /// 是否真的改了磁盘文件。
    pub wrote: bool,
    /// 落盘的那一行（`AlreadyPresent` 时为空）。
    pub row: String,
}

/// 索引文件路径。
pub fn index_path(book_root: &Path) -> PathBuf {
    book_root.join(SUMMARY_INDEX_REL)
}

/// 读索引正文；文件不存在当作空串（不是错误：第一本书还没有索引）。
pub fn index_text(book_root: &Path) -> String {
    read_text(&index_path(book_root)).unwrap_or_default()
}

/// 从一行里读章号：`第8章 | ...` → 8。只认行首，避免把正文里的引用当摘要行。
pub fn row_chapter(line: &str) -> Option<u32> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"^第\s*([0-9]{1,4})\s*章").expect("摘要行章号正则"));
    let caps = re.captures(line.trim())?;
    caps.get(1)?.as_str().parse::<u32>().ok().filter(|n| *n > 0)
}

/// 索引里有没有这一章的行。
///
/// 按解析出的数字比，不用 `starts_with("第1章")`——那会把「第11章」当成第 1 章。
pub fn has_chapter_row(index_text: &str, chapter: u32) -> bool {
    index_text
        .lines()
        .any(|line| row_chapter(line) == Some(chapter))
}

/// 把任意形态的一行摘要规整成 7 列：章号列强制重写成 `第{chapter}章`，缺列补空。
fn normalize_row(line: &str, chapter: u32) -> Option<String> {
    let line = line.trim().trim_start_matches(['-', '*', '>']).trim();
    if line.is_empty() {
        return None;
    }
    let unified = line.replace('｜', "|");
    if !unified.contains('|') {
        return None;
    }
    let mut cols: Vec<String> = unified.split('|').map(|c| c.trim().to_string()).collect();
    if cols.len() < ROW_COLUMNS {
        cols.resize(ROW_COLUMNS, String::new());
    }
    cols.truncate(ROW_COLUMNS);
    // 事件列可能被模型用「|」以外的分隔符写成一整段，这里不再二次解析，原样保留。
    cols[0] = format!("第{chapter}章");
    Some(cols.join(" | "))
}

/// 从写手回复里找本章摘要行。
///
/// 回复里除了摘要还会有交付说明、文件清单、自查字数——只认「行首是第N章 且 有管道分隔」
/// 的第一行；行首章号对不上本章的一律跳过（模型偶尔会顺手把别的章也列一遍）。
pub fn row_from_reply(reply: &str, chapter: u32) -> Option<String> {
    for line in reply.lines() {
        let probe = line.trim().trim_start_matches(['-', '*', '>']).trim();
        if !probe.starts_with('第') {
            continue;
        }
        if row_chapter(probe) != Some(chapter) {
            continue;
        }
        if let Some(row) = normalize_row(probe, chapter) {
            return Some(row);
        }
    }
    None
}

/// 正文里「事件」列的内容：优先【章节摘要】段里的管道行，否则退化为开头+结尾摘录。
fn prose_event_cell(prose: &str, chapter: u32) -> Option<String> {
    if let Some(pos) = prose.find("【章节摘要】") {
        let tail = &prose[pos..];
        for line in tail.lines().take(12) {
            let probe = line.trim();
            if !probe.starts_with('第') || row_chapter(probe) != Some(chapter) {
                continue;
            }
            if let Some(row) = normalize_row(probe, chapter) {
                // 段内已有完整行：整行直接可用，交由调用方判断。
                return Some(row);
            }
        }
    }
    // 正文末尾常挂着作者自己的「（本章完）」+ 写作说明/自查清单，直接取「最后 60 字」
    // 会把元信息当成剧情写进摘要（实测第 13 章尾巴抓到了「为假设台词」这类话），
    // 所以先掐掉正文之后的部分，再算开头/结尾。
    let mut body = String::new();
    for line in prose.lines() {
        let probe = line.trim();
        if probe.contains("（本章完）") {
            body.push_str(probe.split("（本章完）").next().unwrap_or(""));
            break;
        }
        // 标题行分两种：章节标题 / 场景小标题是结构，跳过但继续往下读；
        // 「写作说明」这类标题才是正文的终点。
        if probe.starts_with('#') {
            if is_meta_heading(probe) {
                break;
            }
            continue;
        }
        if is_prose_meta_line(probe) {
            break;
        }
        body.push_str(probe);
    }
    let chars: Vec<char> = body.chars().filter(|c| !c.is_whitespace()).collect();
    if chars.len() < 40 {
        return None;
    }
    let head: String = chars.iter().take(FALLBACK_HEAD_CHARS).collect();
    let tail: String = if chars.len() > FALLBACK_TAIL_CHARS {
        chars[chars.len() - FALLBACK_TAIL_CHARS..].iter().collect()
    } else {
        String::new()
    };
    let mut cell = format!("{FALLBACK_MARK}开场：{head}…");
    if !tail.is_empty() {
        cell.push_str(&format!(" 收尾：{tail}…"));
    }
    Some(cell)
}

/// 标题行里带这些字样的，是正文之后的写作说明，不是场景小标题。
fn is_meta_heading(line: &str) -> bool {
    line.contains("写作说明")
        || line.contains("本章说明")
        || line.contains("写作自查")
        || line.contains("自查清单")
}

/// 这一行是不是「正文之后」的元信息（作者备注、写作说明、自查清单）。
fn is_prose_meta_line(line: &str) -> bool {
    if line.is_empty() {
        return false;
    }
    line.starts_with("---")
        || line.starts_with("【")
        || (line.starts_with('（') && line.contains("本章"))
        || line.starts_with("备注")
        || line.starts_with("写作说明")
        || line.starts_with("自查")
        || line.contains("本章写作说明")
}

/// 从本章正文生成一个可直接落盘的摘要行（字段缺失留空）。
pub fn row_from_prose(prose: &str, chapter: u32) -> Option<String> {
    let cell = prose_event_cell(prose, chapter)?;
    // 【章节摘要】段里就是完整一行时，normalize 后列数已够 7 列，直接用。
    if cell.starts_with(&format!("第{chapter}章")) {
        return Some(cell);
    }
    let mut cols = vec![
        format!("第{chapter}章"),
        String::new(),
        String::new(),
        String::new(),
    ];
    cols.push(cell);
    while cols.len() < ROW_COLUMNS {
        cols.push(String::new());
    }
    Some(cols.join(" | "))
}

/// 把一行写进索引文本：已有本章行就整行替换，没有就按章号插到正确位置。
///
/// 索引是「按章节排序的清单」，不是追加日志：先写了第 9、10、11 章，再补第 8 章，
/// 旧实现把第 8 章追在末尾，读取端取「最后六行」就得到 9、10、11、8、12、13
/// 这种章节乱跳的序列。写入端直接维护有序表，文件本身就是章节顺序。
///
/// 排序只看解析出来的章号（不是字符串排序，第 10 章不会排到第 2 章前面）；
/// 没有章号的行（手写备注之类）统一留在末尾，保持原有相对顺序。
/// 行尾换行风格跟随原文件（CRLF / LF），避免整文件重写时把换行改掉。
pub fn upsert_row(index_text: &str, chapter: u32, row: &str) -> String {
    let newline = if index_text.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let mut lines: Vec<String> = index_text
        .lines()
        .map(|l| l.trim_end_matches('\r').to_string())
        .collect();
    // 去掉文件末尾读出来的空尾行，避免每次 upsert 堆一行空白。
    while lines.last().map(|l| l.trim().is_empty()).unwrap_or(false) {
        lines.pop();
    }
    if lines.is_empty() {
        lines.push(INDEX_TITLE.to_string());
        lines.push(String::new());
    }
    // 表头（标题与空行）保持原样，从第一条章节行起才参与排序。
    let first_row = lines
        .iter()
        .position(|line| row_chapter(line).is_some())
        .unwrap_or(lines.len());
    let head: Vec<String> = lines[..first_row].to_vec();
    let mut rows: Vec<(u32, String)> = Vec::new();
    let mut orphans: Vec<String> = Vec::new();
    for line in lines[first_row..].iter() {
        match row_chapter(line) {
            Some(ch) => match rows.iter_mut().find(|(c, _)| *c == ch) {
                // 同一章出现多行：后出现的覆盖先出现的（沿用旧的整行替换语义）
                Some(slot) => slot.1 = line.clone(),
                None => rows.push((ch, line.clone())),
            },
            None => orphans.push(line.clone()),
        }
    }
    match rows.iter_mut().find(|(c, _)| *c == chapter) {
        Some(slot) => slot.1 = row.to_string(),
        None => rows.push((chapter, row.to_string())),
    }
    rows.sort_by_key(|(ch, _)| *ch);
    let mut out_lines = head;
    out_lines.extend(rows.into_iter().map(|(_, line)| line));
    out_lines.extend(orphans);
    let mut out = out_lines.join(newline);
    out.push_str(newline);
    out
}

/// 同步一章的摘要行。
///
/// 判定顺序（顺序就是优先级，改之前先想清楚谁会覆盖谁）：
/// 1. 写手回复里有本章行 → 用它。写手的行是读过整章写出来的，比自动摘录准，
///    续写/精简之后也会覆盖掉旧行；
/// 2. 回复里没有行、但索引里已经有这一章 → `AlreadyPresent`，**不动**。手写的、
///    上一轮写的都算数；自动摘录没有资格把它盖掉；
/// 3. 回复里没有、索引里也没有 → 用正文生成（【章节摘要】段优先，退化摘录兜底），
///    保证「正文在、索引缺」这个缺口一定被堵上。
pub fn sync_chapter(book_root: &Path, chapter: u32, reply: &str) -> Result<SyncReport, StoryError> {
    let existing = index_text(book_root);
    let present = has_chapter_row(&existing, chapter);

    let prose =
        context::chapter_prose_path(book_root, chapter).and_then(|path| read_text(&path).ok());

    let (row, source) = match row_from_reply(reply, chapter) {
        Some(row) => (Some(row), RowSource::Writer),
        None if present => (None, RowSource::AlreadyPresent),
        None => match prose.as_deref().and_then(|p| row_from_prose(p, chapter)) {
            Some(row) => (Some(row), RowSource::Prose),
            None => (None, RowSource::AlreadyPresent),
        },
    };

    let Some(row) = row else {
        return Ok(SyncReport {
            chapter,
            source: RowSource::AlreadyPresent,
            wrote: false,
            row: String::new(),
        });
    };

    let next = upsert_row(&existing, chapter, &row);
    write_atomic(&index_path(book_root), next.as_bytes())?;
    Ok(SyncReport {
        chapter,
        source,
        wrote: true,
        row,
    })
}

/// 正文已存在、索引里却没有摘要行的章号（升序）。
pub fn missing_chapters(book_root: &Path) -> Vec<u32> {
    let index = index_text(book_root);
    let mut out: Vec<u32> = Vec::new();
    let Ok(entries) = std::fs::read_dir(book_root.join("正文")) else {
        return out;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if entry.path().extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let Some(no) = context::chapter_in_name(&name) else {
            continue;
        };
        if has_chapter_row(&index, no) {
            continue;
        }
        if !out.contains(&no) {
            out.push(no);
        }
    }
    out.sort_unstable();
    out
}

/// 一次性补齐所有缺口章（含《隐藏条款》这种历史遗留书）。
///
/// 只补缺的，不动已有行；正文都没有摘要段时退化为开头+结尾摘录，字段留空。
pub fn backfill(book_root: &Path) -> Result<Vec<SyncReport>, StoryError> {
    let mut reports = Vec::new();
    for chapter in missing_chapters(book_root) {
        reports.push(sync_chapter(book_root, chapter, "")?);
    }
    Ok(reports)
}

// ---------------------------------------------------------------- 正文末尾的创作注释

/// 写作备注文件（相对书根）：从正文里搬出来的创作注释落在这一份。
pub const WRITER_NOTES_REL: &str = "追踪/写作备注.md";

/// 备注文件的标题行。
const WRITER_NOTES_TITLE: &str = "# 写作备注";

/// 备注文件开头那句说明（只写一次，不随章节重复）。
const WRITER_NOTES_HINT: &str =
    "正文「（本章完）」之后的创作注释由写后流程搬到这里；正文只留故事。本文件由程序维护，手改也安全。";

/// 正文收尾标记：这些行之后写手爱挂「补充说明」。
const PROSE_END_MARKS: [&str; 3] = ["（本章完）", "(本章完)", "本章完"];

/// 收尾标记的规范写法：写手爱在前面加 `---` 之类的分隔装饰
/// （实测第 030–032 章写成 `---（本章完）`），搬注释时顺手规范成这一行。
const END_MARK_CANONICAL: &str = "（本章完）";

/// 创作注释的起始前缀。必须**带冒号**才算——第022章正文里有一句
/// 「没有补充说明，没有“是否本人”一栏」，那是剧情，不是注释。
///
/// 实测写手换过好几种说法：第 013–029 章写「补充说明」，第 032 章写「补写说明」，
/// 所以不能只认一个词；漏认一个，那章的注释就留在正文里被算进字数。
const WRITER_NOTE_PREFIXES: [&str; 10] = [
    "补充说明",
    "补写说明",
    "补遗说明",
    "写作说明",
    "写作补充说明",
    "本章说明",
    "创作说明",
    "正文说明",
    "作者说明",
    "作者注",
];

/// 注释块之后若还有这些小标题，它们属于正文/作者信息，不搬走。
const NOTES_TAIL_KEEP: [&str; 6] = [
    "下章预告",
    "下一章预告",
    "下一章",
    "预告",
    "作者的话",
    "后记",
];

/// 一次搬迁的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetachReport {
    pub chapter: u32,
    /// 被改写的正文文件（绝对路径）。
    pub file: PathBuf,
    /// 搬走的注释字符数（按 `char` 计，不去标点）。
    pub moved_chars: usize,
    /// 落盘位置（相对书根）。
    pub notes_rel: &'static str,
    /// 备注文件里这一章的小节本来就存在（`false` = 这次新增）。
    pub replaced: bool,
    /// 正文收尾标记被规范过（`---（本章完）` → `（本章完）`）。
    pub mark_normalized: bool,
}

/// 去掉 Markdown 装饰后判断这一行是不是创作注释的开头。
fn note_line_prefix(line: &str) -> Option<&'static str> {
    let trimmed = line
        .trim_start()
        .trim_start_matches(&['*', '>', '-', '#', ' '][..]);
    let cleaned = trimmed.trim_start();
    for prefix in WRITER_NOTE_PREFIXES {
        if let Some(rest) = cleaned.strip_prefix(prefix) {
            let rest = rest.trim_start();
            if rest.starts_with('：') || rest.starts_with(':') {
                return Some(prefix);
            }
        }
    }
    None
}

/// 去掉 Markdown 装饰（`---`、`**`、`>`、`#`）后的行首内容。
fn decoration_trimmed(line: &str) -> &str {
    line.trim().trim_matches(|c: char| {
        matches!(
            c,
            '*' | '_' | '>' | '#' | '-' | '—' | '–' | '=' | ' ' | '\t' | '　'
        )
    })
}

/// 收尾标记行（`（本章完）` 的各种写法，含 `---（本章完）` 这种带装饰的）。
///
/// 判的是「整行就是标记」，不是「行里含标记」：正文里「他说（本章完）不是这么写的」
/// 这种句子不能被当成章末。
fn is_end_mark(line: &str) -> bool {
    let cleaned = decoration_trimmed(line);
    PROSE_END_MARKS.iter().any(|mark| cleaned == *mark)
}

/// 整行只有分隔符（`---`、`***`、`___`、`—`）——章末的排版残留。
fn is_separator_line(line: &str) -> bool {
    let trimmed = line.trim();
    !trimmed.is_empty()
        && trimmed
            .chars()
            .all(|c| matches!(c, '-' | '*' | '_' | '—' | '─' | '–'))
}

/// 把带装饰的收尾标记行规范成 `（本章完）`，并去掉标记之后多余的分隔线：`(文本, 是否改过)`。
fn normalize_end_marks(text: &str) -> (String, bool) {
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut changed = false;
    let mut out: Vec<String> = Vec::new();
    for line in text.lines() {
        if is_end_mark(line) && line.trim() != END_MARK_CANONICAL {
            out.push(END_MARK_CANONICAL.to_string());
            changed = true;
        } else {
            out.push(line.to_string());
        }
    }
    // 章末只留 `（本章完）` 一行：标记之后的排版内容，以及正文最后一行与标记之间的
    // 空行、分隔线（真实数据里有 `---\n\n（本章完）\n\n---`）都属于残留，一并清掉。
    if let Some(at) = out
        .iter()
        .rposition(|line| line.trim() == END_MARK_CANONICAL)
    {
        // 标记之后若还有实质内容（「下章预告：…」这类要留在正文的小节），一个字都不能删。
        // 这里原本无条件 truncate(at + 1)：detach_writer_notes 先把注释切走、再把预告段
        // 接回正文末尾，于是「（本章完）」后面紧跟的就是预告，整段被当成排版残留删掉——
        // 作者写的下章预告 / 作者的话就此永久消失（正文被写回，快照只是旧文件，没人会去翻）。
        let tail_has_content = out[at + 1..]
            .iter()
            .any(|line| !line.trim().is_empty() && !is_separator_line(line.trim()));
        if !tail_has_content {
            if at + 1 < out.len() {
                out.truncate(at + 1);
                changed = true;
            }
            let mark_at = out.len() - 1;
            let mut cut = mark_at;
            while cut > 0 {
                let prev = out[cut - 1].trim();
                if prev.is_empty() || is_separator_line(prev) {
                    cut -= 1;
                } else {
                    break;
                }
            }
            let between: Vec<&str> = out[cut..mark_at].iter().map(|line| line.trim()).collect();
            let already_clean = between.len() == 1 && between[0].is_empty();
            if cut < mark_at && !already_clean {
                out.drain(cut..mark_at);
                if cut > 0 {
                    out.insert(cut, String::new());
                }
                changed = true;
            }
        }
    }
    if !changed {
        return (text.to_string(), false);
    }
    let mut joined = out.join(newline);
    if text.ends_with('\n') {
        joined.push_str(newline);
    }
    (joined, changed)
}

/// 这个小标题行是不是「注释块之后该留在正文里」的那类。
fn keeps_in_prose(line: &str) -> bool {
    let trimmed = line
        .trim_start()
        .trim_start_matches(&['*', '>', '-', '#', ' '][..]);
    NOTES_TAIL_KEEP.iter().any(|mark| trimmed.starts_with(mark))
}

/// 把正文末尾的创作注释切出来：`(剩下的正文, 切出来的注释)`。
///
/// 幂等：搬过一次之后正文里没有注释了，这里就返回 `None`。
fn split_writer_notes(text: &str) -> Option<(String, String)> {
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let lines: Vec<&str> = text.lines().collect();

    // 注释只在收尾标记之后找；没有收尾标记时只在文件后半段找，绝不动正文前半段。
    let from = match lines.iter().position(|line| is_end_mark(line)) {
        Some(at) => at + 1,
        None => lines.len() / 2,
    };

    let start = (from..lines.len()).find(|&i| note_line_prefix(lines[i]).is_some())?;

    // 注释块一直吃到文件末尾；中途遇到「下章预告」这类小节就截断，那部分留在正文里。
    let stop = (start..lines.len())
        .find(|&i| keeps_in_prose(lines[i]))
        .unwrap_or(lines.len());
    if stop <= start {
        return None;
    }

    let notes = lines[start..stop].join(newline);
    let notes = notes.trim_end().to_string();
    if notes.is_empty() {
        return None;
    }

    let mut body: Vec<&str> = Vec::with_capacity(lines.len());
    body.extend_from_slice(&lines[..start]);
    body.extend_from_slice(&lines[stop..]);
    while matches!(body.last(), Some(last) if last.trim().is_empty()) {
        body.pop();
    }
    if body.is_empty() {
        return None;
    }

    Some((format!("{}{newline}", body.join(newline)), notes))
}

/// 备注文件里某一章小节的行首：`## 第N章`。
fn notes_section_head(chapter: u32) -> String {
    format!("## 第{chapter}章")
}

/// 从 `## 第N章` 这样的行里读出章号。
fn notes_section_chapter(line: &str) -> Option<u32> {
    let trimmed = line.trim_start();
    let rest = trimmed.strip_prefix("##")?.trim_start();
    let rest = rest.strip_prefix('第')?;
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    let after = rest[digits.len()..].trim_start();
    if !after.starts_with('章') {
        return None;
    }
    digits.parse().ok()
}

/// 把某一章的注释写进备注文件：同章整段替换、其余原样、按章号升序（幂等）。
pub fn upsert_notes_section(text: &str, chapter: u32, notes: &str) -> String {
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let notes = notes.trim();

    let mut header: Vec<String> = Vec::new();
    let mut sections: Vec<(u32, String)> = Vec::new();
    let mut current: Option<(u32, Vec<String>)> = None;

    for line in text.lines() {
        match notes_section_chapter(line) {
            Some(no) => {
                if let Some((prev, body)) = current.take() {
                    sections.push((prev, body.join(newline)));
                }
                current = Some((no, Vec::new()));
            }
            None => match current.as_mut() {
                Some((_, body)) => body.push(line.to_string()),
                None => header.push(line.to_string()),
            },
        }
    }
    if let Some((prev, body)) = current.take() {
        sections.push((prev, body.join(newline)));
    }

    if header.is_empty() {
        header.push(WRITER_NOTES_TITLE.to_string());
        header.push(String::new());
        header.push(WRITER_NOTES_HINT.to_string());
    }
    while matches!(header.last(), Some(last) if last.trim().is_empty()) {
        header.pop();
    }

    match sections.iter_mut().find(|(no, _)| *no == chapter) {
        Some(slot) => slot.1 = notes.to_string(),
        None => sections.push((chapter, notes.to_string())),
    }
    sections.sort_by_key(|(no, _)| *no);

    let mut out = header.join(newline);
    for (no, body) in sections {
        out.push_str(&newline);
        out.push_str(&newline);
        out.push_str(&notes_section_head(no));
        let body = body.trim();
        if !body.is_empty() {
            out.push_str(&newline);
            out.push_str(&newline);
            out.push_str(body);
        }
        out.push_str(&newline);
    }
    out
}

/// 文件相对工作区根的 `/` 路径（不在工作区里时给 `None`，快照就跳过）。
fn rel_to_workspace(workspace_root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(workspace_root).ok()?;
    Some(rel.to_string_lossy().replace('\\', "/"))
}

/// 写后把正文末尾的创作注释搬到 `追踪/写作备注.md`。
///
/// 背景：《隐藏条款》第 013–029 章每章末尾都挂一段「补充说明：①②③」，
/// 那是写手对细纲缺口的假设说明，不是故事。它被算进字数体检，导出与发布时
/// 也会混进去；书自己的账本已经把它判成「不应留在正文文本里」的创作过程注释。
///
/// 规矩：
/// - 只切 `（本章完）` 之后、以「补充说明/补写说明/写作说明… + 冒号」开头的那一段，
///   `下章预告` 这类小节留在正文；
/// - 落盘顺序是「先写备注文件，再快照 + 覆盖正文」：崩在中间只会多一份备注，不会丢字；
/// - 幂等：没有注释就什么都不做，返回 `None`；
/// - 收尾标记顺手规范：`---（本章完）` → `（本章完）`（写手爱加分隔线，导出时很脏）；
/// - 只碰 `正文/` 里按章号命中的那一个文件，加自己那一个备注文件。
pub fn detach_writer_notes(
    workspace_root: &Path,
    book_root: &Path,
    chapter: u32,
) -> Result<Option<DetachReport>, StoryError> {
    let Some(file) = context::chapter_prose_path(book_root, chapter) else {
        return Ok(None);
    };
    let old = read_text(&file)?;
    // 编码已经坏了的正文一个字都不要动：这里要「切掉尾巴再写回整个文件」，
    // 万一碰上 cp936 乱码，写回只会把损坏的文件再保存一遍（第037章事故）。
    if super::encoding_guard::looks_like_mojibake(&old) {
        return Ok(None);
    }
    // 先切注释再规范标记：规范标记会把标记之后的内容整段清掉，顺序反过来会把注释一起删掉。
    let split = split_writer_notes(&old);
    let (body, notes, mark_normalized) = match split {
        Some((body, notes)) => {
            let (body, normalized) = normalize_end_marks(&body);
            (body, Some(notes), normalized)
        }
        None => {
            let (text, normalized) = normalize_end_marks(&old);
            if !normalized {
                return Ok(None);
            }
            (text, None, true)
        }
    };

    let mut replaced = false;
    if let Some(notes) = &notes {
        let notes_path = book_root.join(WRITER_NOTES_REL);
        let existing = if notes_path.exists() {
            read_text(&notes_path)?
        } else {
            String::new()
        };
        replaced = existing
            .lines()
            .any(|line| notes_section_chapter(line) == Some(chapter));
        let next = upsert_notes_section(&existing, chapter, notes);
        // 先写备注：这一步失败就整段放弃，正文一个字节都不动。
        write_atomic(&notes_path, next.as_bytes())?;
    }

    // 正文覆盖前留快照：这是已经写好的正文，必须留回滚点。
    //
    // 以前是 let _ = take(...)：快照失败照样往下覆盖，等于「注释搬走了、正文没了、
    // 连回滚点都没有」。现在拿不到快照就中止，正文保持原样（备注文件里已经多出来的
    // 那份注释是幂等 upsert，重试时就地替换，不会重复堆积）。
    // 【待实机验证】书根不在工作区内时 rel_to_workspace 给 None，旧代码是「跳过快照继续写」，
    // 现在同样按中止处理：正常目录结构走不到这条分支，真踩到时宁可报错也不静默覆盖。
    let Some(rel) = rel_to_workspace(workspace_root, &file) else {
        return Err(StoryError::invalid(
            "workspace_root",
            format!(
                "正文不在工作区内，留不了回滚快照，已放弃搬注释：{}",
                file.display()
            ),
        ));
    };
    super::snapshot::take(workspace_root, &rel, Some(old.as_bytes()))?;
    write_atomic(&file, body.as_bytes())?;

    Ok(Some(DetachReport {
        chapter,
        file,
        moved_chars: notes.map(|n| n.chars().count()).unwrap_or(0),
        notes_rel: WRITER_NOTES_REL,
        replaced,
        mark_normalized,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn book(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("storydesk-memory-{name}"));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("正文")).unwrap();
        fs::create_dir_all(root.join("记忆")).unwrap();
        root
    }

    #[test]
    fn upsert_appends_then_replaces_the_same_chapter_row() {
        let start = "# 摘要索引\n\n第1章 | 深夜 | 码头 | 陈砚 | 拿到账本 | 蓝色字迹 | 老赵失约\n";
        let appended = upsert_row(
            start,
            2,
            "第2章 | 清晨 | 仓库 | 陈砚、老赵 | 对账 | 无 | 门外有人",
        );
        assert!(appended.contains("第2章 | 清晨 | 仓库"), "{appended}");
        assert!(
            appended.contains("第1章 | 深夜"),
            "已有行不能被吞：{appended}"
        );

        let replaced = upsert_row(
            &appended,
            2,
            "第2章 | 黄昏 | 仓库 | 陈砚 | 对账改了 | 无 | 门被锁",
        );
        assert!(replaced.contains("对账改了"), "{replaced}");
        assert!(!replaced.contains("清晨"), "同一章只能留一行：{replaced}");
        assert_eq!(replaced.matches("第2章").count(), 1);
    }

    fn row(ch: u32) -> String {
        format!(
            "第{ch}章 | 时间{ch} | 地点{ch} | 人物{ch} | 事件{ch} | 伏笔{ch} | 钩子{ch} | 备注{ch}"
        )
    }

    fn chapter_order(index: &str) -> Vec<u32> {
        index.lines().filter_map(row_chapter).collect()
    }

    #[test]
    fn out_of_order_writes_land_in_chapter_order() {
        // 真实数据：先写了 9、10、11，回头补 8，再写 12、13。
        let mut index = String::from("# 摘要索引\n\n");
        for ch in [9u32, 10, 11, 8, 12, 13] {
            index = upsert_row(&index, ch, &row(ch));
        }
        assert_eq!(chapter_order(&index), vec![8, 9, 10, 11, 12, 13], "{index}");
        assert_eq!(index.lines().next(), Some("# 摘要索引"), "{index}");
    }

    #[test]
    fn rewriting_an_existing_chapter_keeps_the_file_sorted() {
        let mut index = String::from("# 摘要索引\n\n");
        for ch in [1u32, 2, 3] {
            index = upsert_row(&index, ch, &row(ch));
        }
        let again = upsert_row(&index, 2, "第2章 | 改过的 | b | c | d | e | f | g");
        assert_eq!(chapter_order(&again), vec![1, 2, 3], "{again}");
        assert!(again.contains("改过的"), "{again}");
        assert!(!again.contains("时间2"), "旧行整行替换：{again}");
        assert_eq!(again.matches("第2章").count(), 1, "{again}");
    }

    #[test]
    fn new_chapter_is_inserted_in_the_middle_not_appended() {
        let mut index = String::from("# 摘要索引\n\n");
        for ch in [1u32, 3, 5] {
            index = upsert_row(&index, ch, &row(ch));
        }
        let mid = upsert_row(&index, 2, &row(2));
        assert_eq!(chapter_order(&mid), vec![1, 2, 3, 5], "{mid}");
        let last = upsert_row(&mid, 4, &row(4));
        assert_eq!(chapter_order(&last), vec![1, 2, 3, 4, 5], "{last}");
    }

    #[test]
    fn rows_without_a_chapter_number_stay_at_the_end() {
        let mut index = String::from(
            "# 摘要索引\n\n第1章 | a | b | c | d | e | f | g\n手写备注：第 2 章要补一场雨\n",
        );
        index = upsert_row(&index, 3, &row(3));
        assert_eq!(chapter_order(&index), vec![1, 3], "{index}");
        let lines: Vec<&str> = index.lines().collect();
        assert_eq!(
            lines.last(),
            Some(&"手写备注：第 2 章要补一场雨"),
            "{index}"
        );
        assert_eq!(lines.first(), Some(&"# 摘要索引"), "{index}");
    }

    #[test]
    fn chapters_sort_numerically_not_as_text() {
        let mut index = String::from("# 摘要索引\n\n");
        for ch in [2u32, 10, 1] {
            index = upsert_row(&index, ch, &row(ch));
        }
        let order = chapter_order(&index);
        assert_eq!(
            order,
            vec![1, 2, 10],
            "第 10 章不能排在第 2 章前面：{index}"
        );
    }

    #[test]
    fn duplicate_rows_for_one_chapter_collapse_to_the_last_one() {
        let index = "# 摘要索引\n\n第2章 | 旧 | b | c | d | e | f | g\n第1章 | a | b | c | d | e | f | g\n第2章 | 新 | b | c | d | e | f | g\n";
        let out = upsert_row(index, 7, &row(7));
        assert_eq!(out.matches("第2章").count(), 1, "{out}");
        assert!(out.contains("第2章 | 新"), "{out}");
        assert_eq!(chapter_order(&out), vec![1, 2, 7], "{out}");
    }

    #[test]
    fn chapter_numbers_are_compared_as_numbers_not_prefixes() {
        let index = "第11章 | 时间 | 地点 | 人物 | 事件 | 伏笔 | 钩子\n";
        assert!(has_chapter_row(index, 11));
        assert!(!has_chapter_row(index, 1), "第11章不能当成第1章");
        assert_eq!(row_chapter("第 7 章 | x"), Some(7));
        assert_eq!(row_chapter("正文第7章"), None, "只认行首");
    }

    #[test]
    fn reply_row_wins_and_columns_are_padded() {
        let reply =
            "已完成第8章。\n\n- 第8章 | 卯时 | 仓库后门 | 陈砚、老赵 | 饭票背面的字\n\n字数 6120。";
        let row = row_from_reply(reply, 8).expect("应认出摘要行");
        let cols: Vec<&str> = row.split('|').map(str::trim).collect();
        assert_eq!(cols.len(), ROW_COLUMNS, "{row}");
        assert_eq!(cols[0], "第8章");
        assert_eq!(cols[4], "饭票背面的字");
        assert!(row_from_reply(reply, 9).is_none(), "章号对不上不能误收");
    }

    #[test]
    fn sync_writes_a_row_from_the_writer_reply() {
        let root = book("reply");
        fs::write(
            root.join("正文/第008章_教不是代.md"),
            format!(
                "# 第008章 教不是代\n\n{}",
                "仓门口的雨一直没停。".repeat(40)
            ),
        )
        .unwrap();
        let report = sync_chapter(
            &root,
            8,
            "第8章 | 卯时 | 仓库 | 陈砚 | 认出字迹 | 蓝色墨 | 门被锁\n",
        )
        .unwrap();
        assert_eq!(report.source, RowSource::Writer);
        assert!(report.wrote);
        let saved = index_text(&root);
        assert!(saved.contains("第8章 | 卯时 | 仓库"), "{saved}");
    }

    #[test]
    fn missing_summary_falls_back_to_prose_excerpt_and_still_writes() {
        let root = book("fallback");
        fs::write(
            root.join("正文/第012章_水汽.md"),
            format!(
                "# 第012章 水汽\n\n{}",
                "雨棚在滴水，台阶积着浅水。".repeat(30)
            ),
        )
        .unwrap();
        let report = sync_chapter(&root, 12, "").unwrap();
        assert_eq!(report.source, RowSource::Prose, "{report:?}");
        assert!(report.row.contains(FALLBACK_MARK), "{}", report.row);
        assert!(has_chapter_row(&index_text(&root), 12));
    }

    #[test]
    fn missing_chapters_detects_the_gap_and_backfill_closes_it() {
        let root = book("gap");
        for n in [8u32, 12, 13] {
            fs::write(
                root.join(format!("正文/第{n:03}章_测试.md")),
                format!("# 第{n:03}章 测试\n\n{}", "正文内容反复写。".repeat(30)),
            )
            .unwrap();
        }
        fs::write(
            root.join(SUMMARY_INDEX_REL),
            "# 摘要索引\n\n第7章 | 夜 | 仓库 | 陈砚 | 旧事 | 无 | 雨\n",
        )
        .unwrap();
        assert_eq!(missing_chapters(&root), vec![8, 12, 13]);

        let reports = backfill(&root).unwrap();
        assert_eq!(reports.len(), 3, "{reports:?}");
        assert!(missing_chapters(&root).is_empty());
        let saved = index_text(&root);
        assert!(saved.contains("第7章 | 夜"), "旧行要保留：{saved}");
        for n in [8u32, 12, 13] {
            assert!(
                saved.contains(&format!("第{n}章 |")),
                "第{n}章缺行：{saved}"
            );
        }
    }

    #[test]
    fn existing_row_is_not_rewritten_when_there_is_nothing_new() {
        let root = book("stable");
        fs::write(
            root.join("正文/第009章_旧稿.md"),
            format!("# 第009章 旧稿\n\n{}", "旧稿内容。".repeat(40)),
        )
        .unwrap();
        fs::write(
            root.join(SUMMARY_INDEX_REL),
            "# 摘要索引\n\n第9章 | 夜 | 码头 | 陈砚 | 手写摘要 | 无 | 无\n",
        )
        .unwrap();
        let report = sync_chapter(&root, 9, "").unwrap();
        assert_eq!(report.source, RowSource::AlreadyPresent);
        assert!(!report.wrote);
        assert!(index_text(&root).contains("手写摘要"));
    }

    #[test]
    fn index_without_title_is_created_with_one() {
        let root = book("title");
        fs::write(
            root.join("正文/第001章_开篇.md"),
            format!("# 第001章 开篇\n\n{}", "开篇正文。".repeat(40)),
        )
        .unwrap();
        sync_chapter(&root, 1, "").unwrap();
        let saved = index_text(&root);
        assert!(saved.starts_with(INDEX_TITLE), "{saved}");
    }

    /// 退化摘录不能把正文后面的「写作说明/自查」当成剧情收进摘要。
    #[test]
    fn prose_excerpt_stops_before_author_notes() {
        let root = book("meta");
        let story = "陈砚推门出去，雨点砸在伞面上，祠门在身后合拢。".repeat(6);
        fs::write(
            root.join("正文/第013章_上谱.md"),
            format!(
                "# 第013章 上谱\n\n{story}（本章完）\n\n## 写作说明\n1. 柳三娘的话为假设台词。\n"
            ),
        )
        .unwrap();
        let report = sync_chapter(&root, 13, "").unwrap();
        assert!(report.row.contains("雨点砸在伞面"), "{}", report.row);
        assert!(
            !report.row.contains("假设台词"),
            "写作说明漏进了摘要：{}",
            report.row
        );
    }

    // ------------------------------------------------ 正文末尾创作注释的搬迁

    #[test]
    fn writer_notes_are_split_off_after_the_end_mark() {
        let text = "# 第013章 上谱\n\n陈砚把工牌翻过来。雨停在天亮之前。\n\n（本章完）\n\n补充说明：① 柳三娘的话为假设台词；② 细纲缺 0421 工号来源。\n";
        let (body, notes) = split_writer_notes(text).expect("注释要被切出来");
        assert!(body.contains("雨停在天亮之前"), "{body}");
        assert!(body.trim_end().ends_with("（本章完）"), "{body}");
        assert!(!body.contains("补充说明"), "正文里不能留注释：{body}");
        assert!(notes.starts_with("补充说明：①"), "{notes}");
        assert!(notes.contains("0421"), "{notes}");
    }

    /// 第022章正文里有一句 「没有补充说明，没有“是否本人”一栏」——剧情，不是注释。
    #[test]
    fn the_022_sentence_is_not_a_writer_note() {
        let text = "# 第022章 我接\n\n纸上再没有别的字，没有补充说明，没有“是否本人”一栏。\n\n（本章完）\n";
        assert!(split_writer_notes(text).is_none(), "不能动剧情句");
    }

    #[test]
    fn a_preview_section_after_the_notes_stays_in_prose() {
        let text = "# 第013章 上谱\n\n正文写完。\n\n（本章完）\n\n补充说明：假设台词。\n\n下章预告：工牌多一个字。\n";
        let (body, notes) = split_writer_notes(text).expect("注释要被切出来");
        assert!(body.contains("下章预告：工牌多一个字"), "{body}");
        assert!(!body.contains("假设台词"), "{body}");
        assert!(notes.contains("假设台词"), "{notes}");
        assert!(!notes.contains("下章预告"), "{notes}");
    }

    /// 回归：搬注释之后，「（本章完）」后面挂着的「下章预告」必须还在。
    ///
    /// 顺序是「先切注释 → 再规范收尾标记」：切完注释，预告段被接回正文末尾，正好落在
    /// 「（本章完）」之后；旧的 normalize_end_marks 无条件截断标记之后的一切，预告于是
    /// 被永久删掉（第 013 章实测）。旧单测只测了 split，串起来跑才看得出来。
    #[test]
    fn detach_keeps_the_preview_section_after_the_end_mark() {
        let root = book("preview");
        let file = root.join("正文/第013章_上谱.md");
        fs::write(
            &file,
            "# 第013章 上谱\n\n陈砚把工牌翻过来。雨停在天亮之前。\n\n（本章完）\n\n补充说明：① 柳三娘的话为假设台词。\n\n下章预告：工牌多一个字。\n",
        )
        .unwrap();
        let workspace = root.parent().unwrap().to_path_buf();

        let report = detach_writer_notes(&workspace, &root, 13)
            .unwrap()
            .expect("有注释就要搬");
        assert!(report.moved_chars > 0, "注释字数要记账");

        let now = fs::read_to_string(&file).unwrap();
        assert!(
            now.contains("下章预告：工牌多一个字"),
            "预告不能被收尾标记的清理顺手删掉：{now}"
        );
        assert!(!now.contains("假设台词"), "注释不能留在正文：{now}");

        let notes = fs::read_to_string(root.join(WRITER_NOTES_REL)).unwrap();
        assert!(notes.contains("假设台词"), "{notes}");
        assert!(!notes.contains("下章预告"), "{notes}");
    }

    /// 写手的第二种说法：第 032 章写的是「补写说明：」，旧前缀表不认它。
    #[test]
    fn the_alternative_note_word_is_recognised() {
        let text = "# 第032章 寻人登记\n\n灯丝嗡嗡地响。\n\n---（本章完）\n\n补写说明：本章按细纲完整落地。\n";
        let (body, notes) = split_writer_notes(text).expect("补写说明也要搬走");
        assert!(!body.contains("补写说明"), "{body}");
        assert!(notes.starts_with("补写说明："), "{notes}");
    }

    /// 收尾标记带着分隔线（`---（本章完）`）也要认出来，并规范成 `（本章完）`。
    #[test]
    fn detach_normalises_a_decorated_end_mark() {
        let root = book("decorated");
        let file = root.join("正文/第032章_寻人登记.md");
        fs::write(
            &file,
            "# 第032章 寻人登记\n\n灯丝嗡嗡地响。\n\n---（本章完）\n\n补写说明：本章按细纲落地。\n",
        )
        .unwrap();
        let workspace = root.parent().unwrap().to_path_buf();

        let report = detach_writer_notes(&workspace, &root, 32)
            .unwrap()
            .expect("有注释就要搬");
        assert!(report.mark_normalized, "收尾标记要被规范");
        assert!(report.moved_chars > 0, "注释字数要记账");

        let now = fs::read_to_string(&file).unwrap();
        assert!(now.contains("\n（本章完）"), "标记要变成规范写法：{now}");
        assert!(!now.contains("---（本章完）"), "{now}");
        assert!(!now.contains("补写说明"), "注释不能留在正文：{now}");
        let notes = fs::read_to_string(root.join(WRITER_NOTES_REL)).unwrap();
        assert!(notes.contains("## 第32章"), "{notes}");
        assert!(notes.contains("本章按细纲落地"), "{notes}");
    }

    /// 没有注释，只有脏的收尾标记：也要顺手规范一次（只改这一行）。
    #[test]
    fn detach_normalises_the_mark_even_without_notes() {
        let root = book("mark-only");
        let file = root.join("正文/第030章_常驻候选.md");
        fs::write(
            &file,
            "# 第030章 常驻候选\n\n表上没有第三栏，他要把第三栏写出来。\n\n---（本章完）\n",
        )
        .unwrap();
        let workspace = root.parent().unwrap().to_path_buf();

        let report = detach_writer_notes(&workspace, &root, 30)
            .unwrap()
            .expect("标记脏了就要规范");
        assert_eq!(report.moved_chars, 0);
        assert!(report.mark_normalized);
        let now = fs::read_to_string(&file).unwrap();
        assert!(now.contains("\n（本章完）"), "{now}");
        assert!(!now.contains("---"), "装饰线不能留：{now}");
        assert!(
            !root.join(WRITER_NOTES_REL).exists(),
            "没注释就不该建备注文件"
        );
    }

    /// 收尾标记之后多余的分隔线属于排版残留，要一起去掉。
    #[test]
    fn a_stray_separator_after_the_mark_is_dropped() {
        let text = "# 第027章 一个人的两次到\n\n名册上落下一个“到”。\n\n---\n（本章完）\n---\n";
        let (normalized, changed) = normalize_end_marks(text);
        assert!(changed);
        assert!(normalized.ends_with("（本章完）\n"), "{normalized:?}");
        assert!(!normalized.contains("---"), "{normalized:?}");
        let (again, changed_again) = normalize_end_marks(&normalized);
        assert!(!changed_again, "第二次不该再改：{again:?}");
        assert_eq!(again, normalized);
    }

    /// 真实书里的写法：标记与正文之间隔了空行的分隔线（`---\n\n（本章完）\n\n---`）也要清掉。
    #[test]
    fn separators_around_the_mark_are_dropped_even_across_blank_lines() {
        let text = "# 第029章 驻岗三班\n\n“三班满，转常驻。”她说。\n\n---\n\n（本章完）\n\n---\n";
        let (normalized, changed) = normalize_end_marks(text);
        assert!(changed);
        assert_eq!(
            normalized,
            "# 第029章 驻岗三班\n\n“三班满，转常驻。”她说。\n\n（本章完）\n"
        );
        let (again, changed_again) = normalize_end_marks(&normalized);
        assert!(!changed_again, "第二次不该再改：{again:?}");
        assert_eq!(again, normalized);
    }

    /// 已经规范过的正文，再跑一次什么都不做（幂等）。
    #[test]
    fn a_clean_chapter_is_left_alone() {
        let root = book("clean");
        let file = root.join("正文/第031章_撤岗申请.md");
        let original = "# 第031章 撤岗申请\n\n他找过的人，他也在找。\n\n（本章完）\n";
        fs::write(&file, original).unwrap();
        let workspace = root.parent().unwrap().to_path_buf();

        assert!(detach_writer_notes(&workspace, &root, 31)
            .unwrap()
            .is_none());
        assert_eq!(fs::read_to_string(&file).unwrap(), original);
    }

    /// 正文句子里的「（本章完）」「补写说明」都不能被当成章节结构。
    #[test]
    fn marks_and_notes_inside_sentences_are_plot() {
        let text = "# 第033章 页脚\n\n他说（本章完）不是这么写的，又提起补写说明四个字。\n";
        assert!(split_writer_notes(text).is_none(), "行内出现不是注释");
        let (normalized, changed) = normalize_end_marks(text);
        assert!(!changed, "{normalized}");
        assert_eq!(normalized, text);
    }

    #[test]
    fn notes_sections_are_replaced_in_place_and_sorted() {
        let start = upsert_notes_section("", 13, "补充说明：十三。");
        assert!(start.starts_with(WRITER_NOTES_TITLE), "{start}");
        let with_eight = upsert_notes_section(&start, 8, "补充说明：八。");
        let order: Vec<u32> = with_eight
            .lines()
            .filter_map(notes_section_chapter)
            .collect();
        assert_eq!(order, vec![8, 13], "{with_eight}");

        let again = upsert_notes_section(&with_eight, 13, "补充说明：十三改写。");
        assert_eq!(again.matches("## 第13章").count(), 1, "{again}");
        assert!(again.contains("十三改写"), "{again}");
        assert!(!again.contains("十三。"), "旧段整段替换：{again}");
        assert_eq!(
            again,
            upsert_notes_section(&again, 13, "补充说明：十三改写。")
        );
    }

    #[test]
    fn detach_moves_notes_keeps_a_snapshot_and_is_idempotent() {
        let root = book("detach");
        let file = root.join("正文/第013章_上谱.md");
        fs::write(
            &file,
            "# 第013章 上谱\n\n陈砚把工牌翻过来。\n\n（本章完）\n\n补充说明：① 假设台词；② 缺 0421 来源。\n",
        )
        .unwrap();
        let workspace = root.parent().unwrap().to_path_buf();

        let report = detach_writer_notes(&workspace, &root, 13)
            .unwrap()
            .expect("第一次要搬");
        assert_eq!(report.chapter, 13);
        assert!(!report.replaced, "第一次是新增小节");
        assert!(report.moved_chars > 10, "{report:?}");

        let body = fs::read_to_string(&file).unwrap();
        assert!(!body.contains("补充说明"), "正文里应该没有注释了：{body}");
        assert!(body.contains("（本章完）"), "收尾标记要留着：{body}");
        assert!(body.contains("陈砚把工牌翻过来"), "{body}");

        let notes = fs::read_to_string(root.join(WRITER_NOTES_REL)).unwrap();
        assert!(notes.contains("## 第13章"), "{notes}");
        assert!(notes.contains("0421"), "{notes}");
        assert!(notes.contains(WRITER_NOTES_HINT), "{notes}");

        // 正文被改写前必须存过底：快照分片目录里至少一份文件。
        let rel = rel_to_workspace(&workspace, &file).expect("正文在工作区里");
        let shard = super::super::snapshot::shard_dir(&workspace, &rel);
        let count = fs::read_dir(&shard)
            .map(|it| {
                it.flatten()
                    .filter(|e| e.file_name().to_string_lossy().ends_with(".md"))
                    .count()
            })
            .unwrap_or(0);
        assert!(count >= 1, "正文改写前没留快照：{}", shard.display());

        // 幂等：再跑一次没有任何变化。
        assert!(
            detach_writer_notes(&workspace, &root, 13)
                .unwrap()
                .is_none(),
            "第二次不该再搬"
        );
    }

    #[test]
    fn detach_skips_books_without_the_chapter_file() {
        let root = book("detach-missing");
        let workspace = root.parent().unwrap().to_path_buf();
        assert!(detach_writer_notes(&workspace, &root, 42)
            .unwrap()
            .is_none());
        assert!(
            !root.join(WRITER_NOTES_REL).exists(),
            "没注释就不该建备注文件"
        );
    }

    /// 一次性搬迁真实书里的历史注释（不进常规测试集，必须显式给路径才动磁盘）：
    /// `STORYDESK_BOOK=<书根> STORYDESK_WORKSPACE=<工作区> cargo test --lib detach_real_book -- --ignored --nocapture`
    ///
    /// 跑的是线上同一条 `detach_writer_notes`，不另写脚本——免得「脚本搬好了、应用里还在写」。
    #[test]
    #[ignore = "需要 STORYDESK_BOOK=<书根> 与 STORYDESK_WORKSPACE=<工作区>，会改真实书的正文"]
    fn detach_real_book_from_env() {
        let (Ok(book), Ok(ws)) = (
            std::env::var("STORYDESK_BOOK"),
            std::env::var("STORYDESK_WORKSPACE"),
        ) else {
            return;
        };
        let (book, ws) = (PathBuf::from(book), PathBuf::from(ws));

        let mut chapters: Vec<u32> = Vec::new();
        if let Ok(entries) = std::fs::read_dir(book.join("正文")) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                let Some(no) = context::chapter_in_name(&name) else {
                    continue;
                };
                if !chapters.contains(&no) {
                    chapters.push(no);
                }
            }
        }
        chapters.sort_unstable();

        let mut moved = 0;
        for chapter in chapters {
            match detach_writer_notes(&ws, &book, chapter) {
                Ok(Some(report)) => {
                    moved += 1;
                    let what = if report.moved_chars == 0 {
                        "只规范了收尾标记"
                    } else if report.replaced {
                        "替换旧小节"
                    } else {
                        "新增小节"
                    };
                    println!(
                        "第{}章：搬走 {} 字（{what}）",
                        report.chapter, report.moved_chars
                    );
                }
                Ok(None) => {}
                Err(e) => println!("第{chapter}章：没搬成（{e}）"),
            }
        }
        println!("共搬 {moved} 章");
    }

    /// 一次性补全真实书（不进常规测试集，必须显式给书根才动磁盘）：
    /// `STORYDESK_BOOK=<书根> cargo test --lib backfill_real_book -- --ignored --nocapture`
    ///
    /// 这是历史遗留缺口（《隐藏条款》第 8/12/13 章）的修复入口，跑的是线上同一条
    /// `backfill` 代码路径，不另写脚本，免得「脚本补好了、应用里还是缺」。
    #[test]
    #[ignore = "需要 STORYDESK_BOOK=<书根>，会改真实书的 记忆/摘要索引.md"]
    fn backfill_real_book_from_env() {
        let Ok(root) = std::env::var("STORYDESK_BOOK") else {
            return;
        };
        let before = missing_chapters(Path::new(&root));
        println!("缺行：{before:?}");
        let reports = backfill(Path::new(&root)).unwrap();
        for report in &reports {
            println!(
                "第{}章 {:?} wrote={} {}",
                report.chapter, report.source, report.wrote, report.row
            );
        }
        println!("补后仍缺：{:?}", missing_chapters(Path::new(&root)));
    }
}
