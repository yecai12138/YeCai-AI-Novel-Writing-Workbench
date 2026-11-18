//! 事件层反向审计（S6）。
//!
//! 前三步做的都是「让 AI 写的时候别跑偏」：注入事件进度、按事件切章、写前拦、写后查。
//! 这一层反过来问一句话：**卡本身是不是空的**。
//!
//! 一本书最容易骗过所有检查的情况是——卡写得很漂亮，但
//! ① 每张卡只覆盖一章（等于没跨章）；② 账本从头到尾一行都没有（未结项只增不减）；
//! ③ 细纲和卡各说各的（谁也没真按它写）。
//! 这三种都不违反任何一条硬规则，只要没人回头数，就一直是"看着挺好"。
//!
//! 审计只读不写：它给的是清单，改哪张卡、改哪一章由用户决定。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::models::StoryError;
use crate::services::context;

/// 一条审计发现。`code` 是稳定标识，界面按它分色；`message` 是给人看的，必须能指导操作。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditFinding {
    pub code: String,
    /// warn 是要处理的问题；info 是提示（本书没有事件层时，细纲里的「事件 NNN」不构成错误）。
    pub severity: String,
    /// 相关事件号（`001`）；细纲声明了不存在的事件时，这里放细纲里写的那个号。
    pub event: Option<String>,
    pub chapter: Option<u32>,
    pub message: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventAudit {
    /// 读到的卡数
    pub cards: usize,
    /// 扫过的细纲份数（全书 大纲/细纲_第NNN章.md，不按章号截断）
    pub outlines_scanned: usize,
    /// 有「所属事件」声明的细纲数
    pub declared_chapters: usize,
    pub findings: Vec<AuditFinding>,
    /// 「事件 → 章节」清单：卡认哪些章、细纲又把这些章挂在谁名下。
    pub map: Vec<EventChapterMap>,
    /// 欠账列表：还没结算的未结项，各自最迟章与全书已写到的位置。
    pub debts: Vec<EventDebt>,
    /// 卡内「已结算账本」里的每一行。
    pub settled_from_card: Vec<SettlementEntry>,
    /// `追踪/` 台账里认得的结算行。
    pub settled_from_tracking: Vec<SettlementEntry>,
    /// 两本账对不上的地方：只列差异，不自动合并、不改任何一边。
    pub settlement_diff: Vec<SettlementDiff>,
    /// 正文里出现的 `[[事件:xxx]]`：只记录在哪一章，不参与归属判定。
    pub body_mentions: Vec<BodyMention>,
}

/// 一行「事件 → 章节」。卡自己认的章与细纲声明的章分两列放着，对不上就是契约漂移。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventChapterMap {
    pub event: String,
    pub title: String,
    /// planned / active / settled
    pub status: String,
    /// 卡自己认的章：章号区间展开 ∪ 章型分工
    pub chapters: Vec<u32>,
    /// 细纲里写着「所属事件」或 `[[事件:NNN]]` 归它的章
    pub declared_chapters: Vec<u32>,
    /// 还没结算的未结项条数
    pub open_items: usize,
    /// 已结算账本行数
    pub settled_items: usize,
}

/// 一条欠账（未结算的未结项）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventDebt {
    pub event: String,
    pub code: String,
    pub text: String,
    /// 卡上写的最迟结算章
    pub due: Option<u32>,
    /// 全书正文写到第几章（没有正文时 None）
    pub written_to: Option<u32>,
    /// 正文已经写到最迟章之后，这一项还没结
    pub overdue: bool,
    pub anchors: Vec<String>,
}

/// 一条结算记录（卡内账本或 `追踪/` 台账的一行）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettlementEntry {
    /// 卡内账本里的行记事件号；`追踪/` 台账里的行不知道归属，留空。
    pub event: String,
    pub code: String,
    pub chapter: Option<u32>,
    pub note: String,
}

/// 两本账的一处差异。`kind`：`card_only` / `tracking_only` / `chapter_mismatch`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettlementDiff {
    pub code: String,
    pub kind: String,
    pub message: String,
}

/// 正文里的一处 `[[事件:xxx]]` 引用。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BodyMention {
    pub chapter: u32,
    pub event: String,
}

impl AuditFinding {
    fn new(code: &str, event: Option<&str>, chapter: Option<u32>, message: String) -> Self {
        Self {
            code: code.to_string(),
            severity: "warn".to_string(),
            event: event.map(str::to_string),
            chapter,
            message,
        }
    }
}

/// 扫一遍 `大纲/` 下的细纲，取每章的「所属事件」声明。
fn declarations(book_root: &Path) -> Vec<(u32, Option<String>)> {
    let dir = book_root.join("大纲");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out: Vec<(u32, Option<String>)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        // 只认逐章细纲；大纲.md、细纲索引.md 这类没有章号的会被 chapter_in_name 滤掉
        let Some(chapter) = context::chapter_in_name(name) else {
            continue;
        };
        if !name.starts_with("细纲") {
            continue;
        }
        // 读不出来 / 非 UTF-8 就跳过这一份：from_utf8_lossy + 默认空串会把坏编码变成
        // 一屏 U+FFFD 去撞事件编号，等于让审计读假数据；读失败也不该被当成「空账本」。
        let Ok(body) = crate::services::atomic_write::read_text(&path) else {
            continue;
        };
        out.push((chapter, context::declared_event_in(&body)));
    }
    out.sort_by_key(|(chapter, _)| *chapter);
    out
}

/// 这一章是否已经有正文落地——判断「账本空着」是不是真的没写过。
fn chapter_written(book_root: &Path, chapter: u32) -> bool {
    context::chapter_prose_path(book_root, chapter).is_some()
}

/// 审计一本书的事件层。没有事件卡时返回 `cards: 0` 与空 findings（老书零噪音）。
pub fn audit(book_root: &Path) -> Result<EventAudit, StoryError> {
    let cards = context::all_event_cards(book_root);
    let declared = declarations(book_root);
    let mut findings: Vec<AuditFinding> = Vec::new();

    for card in &cards {
        // ① 跨章展开退化：只排了一章，或区间只有一章
        let span_len = card
            .effective_span()
            .map(|(from, to)| to.saturating_sub(from) + 1)
            .unwrap_or(0);
        if card.roles.len() < 2 && span_len < 2 {
            findings.push(AuditFinding::new(
                "single_chapter",
                Some(&card.id),
                card.roles.first().map(|role| role.chapter),
                format!(
                    "事件 {}「{}」只覆盖 {} 章——这等于一章一事，跨章展开没发生。把区间拉到至少 2 章，并在「章型分工」里给它排 起→推（或推→结）。",
                    card.id,
                    card.title,
                    span_len.max(card.roles.len() as u32)
                ),
            ));
        }

        // ② 账本一行都没有：未结项只增不减
        if !card.open_items.is_empty() && card.ledger.is_empty() {
            let started = card
                .roles
                .iter()
                .any(|role| chapter_written(book_root, role.chapter));
            if started {
                findings.push(AuditFinding::new(
                    "no_ledger_progress",
                    Some(&card.id),
                    None,
                    format!(
                        "事件 {}「{}」已经写到正文了，但「已结算账本」还是空的——未结项只增不减。要么这一章真没结算任何项（那就检查细纲的「本章消耗」是不是写错了），要么账本没记上（看 追踪/写作备注.md 里有没有账本提示）。",
                        card.id, card.title
                    ),
                ));
            }
        }

        // ③ 卡排了某章，细纲却说别的事件（或什么都没说）
        for role in &card.roles {
            match declared.iter().find(|(chapter, _)| *chapter == role.chapter) {
                None => findings.push(AuditFinding::new(
                    "outline_absent",
                    Some(&card.id),
                    Some(role.chapter),
                    format!(
                        "事件 {} 给第 {} 章排了「{}」，但这一章还没有细纲。先生成细纲，再让它写「所属事件：事件 {}」。",
                        card.id, role.chapter, role.role.label(), card.id
                    ),
                )),
                Some((_, None)) => findings.push(AuditFinding::new(
                    "outline_silent",
                    Some(&card.id),
                    Some(role.chapter),
                    format!(
                        "事件 {} 把第 {} 章排进了章型分工，但这一章细纲没有「所属事件」行——旧细纲不写这行也能写正文，但它和事件卡之间就没有契约了。补一行「所属事件：事件 {}」。",
                        card.id, role.chapter, card.id
                    ),
                )),
                Some((_, Some(other))) if other != &card.id => findings.push(AuditFinding::new(
                    "declared_elsewhere",
                    Some(&card.id),
                    Some(role.chapter),
                    format!(
                        "事件 {} 说第 {} 章归它，但第 {} 章细纲写的是事件 {}。两边必须改一边——通常是卡排错了章，或细纲挂错了事件。",
                        card.id, role.chapter, role.chapter, other
                    ),
                )),
                _ => {}
            }
        }
    }

    // ④ 细纲声明了卡不存在 / 卡里没排它
    for (chapter, declared_id) in &declared {
        let Some(id) = declared_id else { continue };
        match context::find_event_card(book_root, id) {
            None => findings.push(AuditFinding::new(
                "missing_card",
                Some(id),
                Some(*chapter),
                format!(
                    "第 {} 章细纲写「所属事件：事件 {}」，但 大纲/事件/ 下没有这张卡。要么让架构师补卡，要么把这一章改成「所属事件：无」。",
                    chapter, id
                ),
            )),
            Some(card) => {
                let planned = card.role_of(*chapter).is_some();
                let covered = card
                    .effective_span()
                    .map(|(from, to)| (from..=to).contains(chapter))
                    .unwrap_or(false);
                if !planned && !covered {
                    findings.push(AuditFinding::new(
                        "unplanned_in_card",
                        Some(id),
                        Some(*chapter),
                        format!(
                            "第 {} 章细纲挂到事件 {}，但卡里的「章型分工」没有这一章、区间也不覆盖它。把这一章补进分工（章型只能写 起/推/转/结），或者把细纲改成「所属事件：无」。",
                            chapter, id
                        ),
                    ));
                }
            }
        }
    }

    // 本书一张事件卡都没有时，「细纲声明了事件 NNN」不是错误：没有事件层就按一章一事写，
    // 门禁也只提示不拦。这里跟着降级为 info，否则界面会一边说「还没有事件卡」一边列 N 条问题。
    if cards.is_empty() {
        for finding in &mut findings {
            if finding.code == "missing_card" {
                finding.severity = "info".to_string();
            }
        }
    }

    findings.sort_by(|a, b| {
        a.chapter
            .unwrap_or(u32::MAX)
            .cmp(&b.chapter.unwrap_or(u32::MAX))
            .then_with(|| a.code.cmp(&b.code))
    });

    let written_to = context::book_end_hint(book_root).map(|hint| hint.chapter);

    let mut map: Vec<EventChapterMap> = Vec::new();
    let mut debts: Vec<EventDebt> = Vec::new();
    for card in &cards {
        let mut chapters: Vec<u32> = card.roles.iter().map(|role| role.chapter).collect();
        if let Some((from, to)) = card.effective_span() {
            chapters.extend(from..=to);
        }
        chapters.sort_unstable();
        chapters.dedup();

        let declared_chapters: Vec<u32> = declared
            .iter()
            .filter(|(_, id)| id.as_deref() == Some(card.id.as_str()))
            .map(|(chapter, _)| *chapter)
            .collect();

        let unsettled: Vec<&crate::models::event::OpenItem> = card
            .open_items
            .iter()
            .filter(|item| !card.is_settled(&item.code))
            .collect();

        map.push(EventChapterMap {
            event: card.id.clone(),
            title: card.title.clone(),
            status: match &card.status {
                crate::models::event::EventStatus::Planned => "planned",
                crate::models::event::EventStatus::Active => "active",
                crate::models::event::EventStatus::Settled => "settled",
            }
            .to_string(),
            chapters,
            declared_chapters,
            open_items: unsettled.len(),
            settled_items: card.ledger.len(),
        });

        for item in unsettled {
            debts.push(EventDebt {
                event: card.id.clone(),
                code: item.code.clone(),
                text: item.text.clone(),
                due: item.due,
                written_to,
                overdue: matches!((item.due, written_to), (Some(due), Some(written)) if written > due),
                anchors: item.anchors.clone(),
            });
        }
    }

    let mut settled_from_card: Vec<SettlementEntry> = Vec::new();
    for card in &cards {
        for item in &card.ledger {
            settled_from_card.push(SettlementEntry {
                event: card.id.clone(),
                code: item.code.clone(),
                chapter: item.at_chapter,
                note: item.note.clone(),
            });
        }
    }
    let settled_from_tracking = tracking_settlements(book_root);
    let settlement_diff = settlement_diffs(&settled_from_card, &settled_from_tracking);
    let body_mentions = body_mentions(book_root);

    Ok(EventAudit {
        cards: cards.len(),
        outlines_scanned: declared.len(),
        declared_chapters: declared.iter().filter(|(_, d)| d.is_some()).count(),
        map,
        debts,
        settled_from_card,
        settled_from_tracking,
        settlement_diff,
        body_mentions,
        findings,
    })
}

/// 扫 `追踪/` 下的 Markdown，捞「编号 + 章号 + 说明」形状的台账行。
///
/// 认两种写法：表格行 `| U-001 | 7 | 第三联 |`、列表行 `- U-001 第7章 交易完成`。
/// 同一编号只记第一次出现的那一行；两本账的差异交给 [`settlement_diffs`] 列出来，这里不合并。
fn tracking_settlements(book_root: &Path) -> Vec<SettlementEntry> {
    let dir = book_root.join("追踪");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|e| e.to_str()) == Some("md"))
        .collect();
    paths.sort();

    let mut out: Vec<SettlementEntry> = Vec::new();
    for path in paths {
        let file = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("追踪")
            .to_string();
        // 读不出来 / 非 UTF-8 就跳过这一份：from_utf8_lossy + 默认空串会把坏编码变成
        // 一屏 U+FFFD 去撞事件编号，等于让审计读假数据；读失败也不该被当成「空账本」。
        let Ok(body) = crate::services::atomic_write::read_text(&path) else {
            continue;
        };
        for line in body.lines() {
            let Some((code, rest)) = tracking_code(line) else {
                continue;
            };
            if out.iter().any(|entry| entry.code == code) {
                continue;
            }
            out.push(SettlementEntry {
                event: String::new(),
                code,
                chapter: number_after(rest),
                note: format!("{}（{}）", line.trim(), file),
            });
        }
    }
    out
}

/// 一行里的编号（`U-001` 这种：大写字母 + `-` + 1~4 位数字）与编号之后的部分。
fn tracking_code(line: &str) -> Option<(String, &str)> {
    let chars: Vec<char> = line.chars().collect();
    let mut start = 0;
    while start + 2 < chars.len() {
        let hit = chars[start].is_ascii_uppercase()
            && chars[start + 1] == '-'
            && chars[start + 2].is_ascii_digit();
        if !hit {
            start += 1;
            continue;
        }
        let mut end = start + 2;
        while end < chars.len() && chars[end].is_ascii_digit() && end - start - 2 < 4 {
            end += 1;
        }
        // 后面紧跟字母数字的（`AK-47式`）不算编号
        if end == chars.len() || !chars[end].is_ascii_alphanumeric() {
            let byte_end: usize = chars[..end].iter().map(|c| c.len_utf8()).sum();
            return Some((chars[start..end].iter().collect(), &line[byte_end..]));
        }
        start = end;
    }
    None
}

/// 一段文字里的第一个阿拉伯数字（当章号用）。
///
/// 上限 999：台账里混进的日期（`2026-11-01`）不是章号，宁可记成「章号没写」，
/// 也不要拿它去跟卡内账本比出假差异。
fn number_after(text: &str) -> Option<u32> {
    let digits: String = text
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit())
        .collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse().ok().filter(|value| *value <= 999)
}

/// 卡内账本 / 追踪台账：只列差异，不合并、不自动改任何一边。
fn settlement_diffs(
    card_rows: &[SettlementEntry],
    tracking_rows: &[SettlementEntry],
) -> Vec<SettlementDiff> {
    let mut out: Vec<SettlementDiff> = Vec::new();
    for row in card_rows {
        match tracking_rows.iter().find(|other| other.code == row.code) {
            None => out.push(SettlementDiff {
                code: row.code.clone(),
                kind: "card_only".to_string(),
                message: format!(
                    "{} 只在卡内「已结算账本」里结了（{}），追踪/ 台账没有这一行。",
                    row.code,
                    chapter_hint(row.chapter)
                ),
            }),
            Some(other) => {
                if let (Some(card_chapter), Some(track_chapter)) = (row.chapter, other.chapter) {
                    if card_chapter != track_chapter {
                        out.push(SettlementDiff {
                            code: row.code.clone(),
                            kind: "chapter_mismatch".to_string(),
                            message: format!(
                                "{} 卡内记第 {card_chapter} 章结算，追踪/ 记第 {track_chapter} 章——两本账的章号对不上。",
                                row.code
                            ),
                        });
                    }
                }
            }
        }
    }
    for row in tracking_rows {
        if !card_rows.iter().any(|other| other.code == row.code) {
            out.push(SettlementDiff {
                code: row.code.clone(),
                kind: "tracking_only".to_string(),
                message: format!(
                    "{} 只在追踪/ 台账里结了（{}），卡内「已结算账本」没有这一行。",
                    row.code,
                    chapter_hint(row.chapter)
                ),
            });
        }
    }
    out
}

fn chapter_hint(chapter: Option<u32>) -> String {
    chapter
        .map(|value| format!("记在第 {value} 章"))
        .unwrap_or_else(|| "章号也没写".to_string())
}

/// 正文里出现的 `[[事件:xxx]]`：只记录在哪一章，不参与归属判定。
fn body_mentions(book_root: &Path) -> Vec<BodyMention> {
    let dir = book_root.join("正文");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|e| e.to_str()) == Some("md"))
        .collect();
    paths.sort();

    let mut out: Vec<BodyMention> = Vec::new();
    for path in paths {
        let chapter = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(context::chapter_in_name);
        let Some(chapter) = chapter else { continue };
        // 读不出来 / 非 UTF-8 就跳过这一份：from_utf8_lossy + 默认空串会把坏编码变成
        // 一屏 U+FFFD 去撞事件编号，等于让审计读假数据；读失败也不该被当成「空账本」。
        let Ok(body) = crate::services::atomic_write::read_text(&path) else {
            continue;
        };
        for event in context::wiki_event_links(&body) {
            if out
                .iter()
                .any(|mention| mention.chapter == chapter && mention.event == event)
            {
                continue;
            }
            out.push(BodyMention { chapter, event });
        }
    }
    out.sort_by(|a, b| a.chapter.cmp(&b.chapter).then_with(|| a.event.cmp(&b.event)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn book(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("storydesk-audit-{name}-{nanos}"));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("大纲").join("事件")).unwrap();
        fs::create_dir_all(root.join("正文")).unwrap();
        root
    }

    fn write_card(root: &Path, id: &str, span: &str, roles: &str, ledger: &str) {
        let body = format!(
            "\
# 事件 {id} · 测试事件

## 基本信息

| 所属卷 | 章号区间 | 前置事件 | 状态 |
| --- | --- | --- | --- |
| 1 | {span} | — | active |

## 结算条件

- 东西到手

## 未结项

| 编号 | 内容 | 埋于 | 最迟结算章 | 锚词 |
| --- | --- | --- | --- | --- |
| U-001 | 单据不在 | 2 | 5 | 第三联 |

## 已结算账本

| 编号 | 结算于 | 方式 |
| --- | --- | --- |
{ledger}

## 章型分工

| 章 | 章型 | 本章消耗 | 本章新增 |
| --- | --- | --- | --- |
{roles}
"
        );
        fs::write(root.join("大纲").join("事件").join(format!("事件_{id}_测试.md")), body).unwrap();
    }

    fn write_outline(root: &Path, chapter: u32, declared: &str) {
        let body = format!(
            "## 基本信息\n\n正文目标字数：1200 字\n\n所属事件：{declared}\n\n## 本章目标\n\n写点东西。\n"
        );
        fs::write(
            root.join("大纲").join(format!("细纲_第{chapter:03}章.md")),
            body,
        )
        .unwrap();
    }

    fn write_prose(root: &Path, chapter: u32) {
        fs::write(
            root.join("正文").join(format!("第{chapter:03}章_测试.md")),
            "正文正文正文正文正文正文正文正文。",
        )
        .unwrap();
    }

    #[test]
    fn a_book_without_cards_scans_every_outline_and_only_hints() {
        // 截图事故：「审不出问题」与 5 条 finding 同时出现，且看不出到底扫了什么。
        // 没有事件层时缺卡只是提示（info），扫描范围必须是全书细纲。
        let root = book("nocards");
        let _ = fs::remove_dir_all(root.join("大纲").join("事件"));
        for chapter in 316..=320u32 {
            write_outline(&root, chapter, &format!("事件 {}", 305 + chapter - 316));
        }
        let audit = audit(&root).unwrap();
        assert_eq!(audit.cards, 0);
        assert_eq!(audit.outlines_scanned, 5, "五份细纲都要扫到：{audit:?}");
        assert_eq!(audit.declared_chapters, 5, "{audit:?}");
        let missing: Vec<&AuditFinding> = audit
            .findings
            .iter()
            .filter(|f| f.code == "missing_card")
            .collect();
        assert_eq!(missing.len(), 5, "每一章的声明都要单独列出：{:?}", codes(&audit));
        assert!(
            missing.iter().all(|f| f.severity == "info"),
            "没有事件层时缺卡是提示：{:?}",
            missing
        );
    }

    fn codes(audit: &EventAudit) -> Vec<&str> {
        audit.findings.iter().map(|f| f.code.as_str()).collect()
    }

    #[test]
    fn flags_a_single_chapter_event() {
        let root = book("single");
        write_card(&root, "001", "3-3", "| 3 | 起 | — | U-001 |", "");
        write_outline(&root, 3, "事件 001");

        let audit = audit(&root).unwrap();
        assert_eq!(audit.cards, 1);
        assert!(codes(&audit).contains(&"single_chapter"), "{:?}", codes(&audit));
    }

    #[test]
    fn flags_an_empty_ledger_once_the_event_has_prose() {
        let root = book("ledger");
        write_card(&root, "001", "2-5", "| 2 | 起 | — | U-001 |\n| 4 | 推 | U-001 | — |", "");
        write_outline(&root, 2, "事件 001");
        write_outline(&root, 4, "事件 001");
        write_prose(&root, 2);

        let audit = audit(&root).unwrap();
        assert!(
            codes(&audit).contains(&"no_ledger_progress"),
            "{:?}",
            codes(&audit)
        );
    }

    #[test]
    fn a_written_ledger_silences_the_progress_finding() {
        let root = book("progress");
        write_card(
            &root,
            "001",
            "2-5",
            "| 2 | 起 | — | U-001 |\n| 4 | 推 | U-001 | — |",
            "| U-001 | 3 | 写手摘要回收：第三联拿到了 |",
        );
        write_outline(&root, 2, "事件 001");
        write_outline(&root, 4, "事件 001");
        write_prose(&root, 2);

        let audit = audit(&root).unwrap();
        assert!(
            !codes(&audit).contains(&"no_ledger_progress"),
            "{:?}",
            codes(&audit)
        );
    }

    #[test]
    fn flags_outline_declaring_a_missing_card() {
        let root = book("missing");
        write_outline(&root, 7, "事件 009");

        let audit = audit(&root).unwrap();
        assert!(codes(&audit).contains(&"missing_card"), "{:?}", codes(&audit));
    }

    #[test]
    fn flags_a_chapter_the_card_never_planned() {
        let root = book("unplanned");
        write_card(&root, "002", "2-4", "| 2 | 起 | — | U-001 |\n| 4 | 结 | U-001 | — |", "");
        write_outline(&root, 9, "事件 002");

        let audit = audit(&root).unwrap();
        assert!(
            codes(&audit).contains(&"unplanned_in_card"),
            "{:?}",
            codes(&audit)
        );
    }

    #[test]
    fn flags_a_card_chapter_whose_outline_is_missing() {
        let root = book("absent");
        write_card(&root, "001", "2-4", "| 2 | 起 | — | U-001 |\n| 4 | 结 | U-001 | — |", "");

        let audit = audit(&root).unwrap();
        assert!(codes(&audit).contains(&"outline_absent"), "{:?}", codes(&audit));
    }

    #[test]
    fn a_consistent_book_has_no_findings() {
        let root = book("clean");
        write_card(
            &root,
            "001",
            "2-5",
            "| 2 | 起 | — | U-001 |\n| 5 | 结 | U-001 | — |",
            "| U-001 | 4 | 写手摘要回收：第三联 |",
        );
        write_outline(&root, 2, "事件 001");
        write_outline(&root, 5, "事件 001");
        write_prose(&root, 2);

        let audit = audit(&root).unwrap();
        assert_eq!(audit.cards, 1);
        assert_eq!(audit.declared_chapters, 2);
        assert!(audit.findings.is_empty(), "{:?}", codes(&audit));
    }

    #[test]
    fn an_old_book_without_events_stays_silent() {
        let root = book("old");
        fs::remove_dir_all(root.join("大纲").join("事件")).unwrap();
        write_outline(&root, 1, "无");

        let audit = audit(&root).unwrap();
        assert_eq!(audit.cards, 0);
        assert!(audit.findings.is_empty());
    }

    // —— 方案五契约靶子 ——
    //
    // 每个靶子对应方案五原文的一条约定。靶子挂了，就说明「手写卡的 Markdown 约定」
    // 和「方案一的实现」脱钩了——这正是方案五当校验靶子的用处。
    // 约定原文：`大纲/事件/` 的卡片格式；正文/细纲里用固定标记（`所属事件：001`）、
    // 双链 `[[事件:001]]` 交叉引用；`追踪/` 里维持台账；
    // 只读索引脚本扫目录生成「事件 → 章节」清单和欠账列表。

    fn write_outline_raw(root: &Path, chapter: u32, body: &str) {
        fs::write(
            root.join("大纲").join(format!("细纲_第{chapter:03}章.md")),
            body,
        )
        .unwrap();
    }

    /// 靶子 P5-1：卡只从 `大纲/事件/*.md` 读，别处的 Markdown 不算卡。
    #[test]
    fn p5_card_dir_is_the_only_source() {
        let root = book("p5dir");
        write_card(&root, "001", "2-4", "| 2 | 起 | — | U-001 |\n| 4 | 结 | U-001 | — |", "");
        fs::create_dir_all(root.join("大纲").join("事件").join("草稿")).unwrap();
        fs::write(
            root.join("大纲").join("事件").join("草稿").join("事件_002_草稿.md"),
            "# 事件 002 · 草稿\n",
        )
        .unwrap();
        fs::write(
            root.join("大纲").join("事件_009_错位.md"),
            "# 事件 009 · 放错目录\n",
        )
        .unwrap();

        let audit = audit(&root).unwrap();
        assert_eq!(audit.cards, 1, "只认 大纲/事件/ 一层：{:?}", audit.map);
        assert_eq!(audit.map[0].event, "001");
    }

    /// 靶子 P5-3：细纲里 `所属事件：001` 就是把章节挂到卡上的标记。
    #[test]
    fn p5_declared_marker_wires_the_chapter() {
        let root = book("p5decl");
        write_card(&root, "001", "2-4", "| 2 | 起 | — | U-001 |\n| 4 | 结 | U-001 | — |", "");
        write_outline(&root, 2, "事件 001");
        write_outline(&root, 4, "事件 001");

        let audit = audit(&root).unwrap();
        let row = &audit.map[0];
        assert_eq!(row.declared_chapters, vec![2, 4], "{audit:?}");
        assert_eq!(row.chapters, vec![2, 3, 4], "区间要展开成章号：{row:?}");
        assert!(!codes(&audit).contains(&"outline_silent"), "{:?}", codes(&audit));
    }

    /// 靶子 P5-3/P5-4（纯文本层）：三种标记都能解析出事件号，写「无」优先于双链。
    #[test]
    fn p5_markers_parse_to_an_event_id() {
        use crate::services::context::declared_event_in;
        assert_eq!(declared_event_in("所属事件：001\n"), Some("001".to_string()));
        assert_eq!(
            declared_event_in("所属事件：事件 001 · 码头交接"),
            Some("001".to_string())
        );
        assert_eq!(declared_event_in("见 [[事件:001]]"), Some("001".to_string()));
        assert_eq!(
            declared_event_in("见 [[事件:事件 002 · 交接]]"),
            Some("002".to_string())
        );
        assert_eq!(
            declared_event_in("所属事件：无\n\n[[事件:001]]"),
            None,
            "明确写「无」时不被双链推回事件层"
        );
        assert_eq!(declared_event_in("正文里没有任何标记"), None);
    }

    /// 靶子 P5-4：只有双链、没有「所属事件」行的细纲也算挂上了。
    #[test]
    fn p5_wiki_link_wires_the_chapter() {
        let root = book("p5wiki");
        write_card(&root, "001", "2-4", "| 2 | 起 | — | U-001 |\n| 4 | 结 | U-001 | — |", "");
        write_outline_raw(&root, 2, "## 本章目标\n\n按 [[事件:001]] 的未结项写。\n");
        write_outline_raw(&root, 4, "## 本章目标\n\n收束 [[事件:001]]。\n");

        let audit = audit(&root).unwrap();
        assert_eq!(audit.declared_chapters, 2, "{audit:?}");
        assert_eq!(audit.map[0].declared_chapters, vec![2, 4]);
        assert!(!codes(&audit).contains(&"outline_silent"), "{:?}", codes(&audit));
    }

    /// 靶子 P5-5：账本记在卡里的「已结算账本」；`追踪/` 的表格不参与结算判定。
    #[test]
    fn p5_ledger_is_read_from_the_card_not_from_tracking() {
        let root = book("p5ledger");
        write_card(
            &root,
            "001",
            "2-5",
            "| 2 | 起 | — | U-001 |\n| 5 | 结 | U-001 | — |",
            "| U-001 | 4 | 写手摘要回收：第三联 |",
        );
        write_outline(&root, 2, "事件 001");
        write_outline(&root, 5, "事件 001");
        write_prose(&root, 2);
        fs::create_dir_all(root.join("追踪")).unwrap();
        fs::write(
            root.join("追踪").join("伏笔台账.md"),
            "| U-001 | 3 | 追踪里写的结算 |\n",
        )
        .unwrap();

        let audit = audit(&root).unwrap();
        assert_eq!(audit.map[0].settled_items, 1, "账本只认卡内那一节");
        assert!(audit.debts.is_empty(), "卡里已结，追踪里另有表格也不算欠账：{:?}", audit.debts);
    }

    /// 靶子 P5-6：审计要顶得上方案五那支「只读索引脚本」——
    /// 既给「事件 → 章节」清单，也给欠账列表（含逾期标记）。
    #[test]
    fn p5_index_lists_events_and_debts() {
        let root = book("p5index");
        // 001 的 U-001 最迟第 5 章结算，正文已经写到第 6 章 → 逾期
        write_card(&root, "001", "2-6", "| 2 | 起 | — | U-001 |\n| 6 | 结 | U-001 | — |", "");
        // 002 已结算，不该出现在欠账里
        write_card(
            &root,
            "002",
            "7-9",
            "| 7 | 起 | — | U-001 |\n| 9 | 结 | U-001 | — |",
            "| U-001 | 8 | 写手摘要回收：钥匙 |",
        );
        write_prose(&root, 6);

        let audit = audit(&root).unwrap();
        let events: Vec<&str> = audit.map.iter().map(|row| row.event.as_str()).collect();
        assert_eq!(events, vec!["001", "002"], "{audit:?}");
        assert_eq!(audit.map[0].open_items, 1);
        assert_eq!(audit.map[0].settled_items, 0);
        assert_eq!(audit.map[1].open_items, 0);
        assert_eq!(audit.map[1].settled_items, 1);

        assert_eq!(audit.debts.len(), 1, "{:?}", audit.debts);
        let debt = &audit.debts[0];
        assert_eq!((debt.event.as_str(), debt.code.as_str()), ("001", "U-001"));
        assert_eq!(debt.due, Some(5));
        assert_eq!(debt.written_to, Some(6));
        assert!(debt.overdue, "正文过了最迟章还没结：{debt:?}");
        assert_eq!(debt.anchors, vec!["第三联".to_string()]);
    }

    /// 缺口 G3：方案五说「正文/细纲里用固定标记」，而 S1 只读细纲。
    /// 这个靶子钉住当前行为——正文里的双链不参与归属判定，写手在正文里补引用不会生效。
    #[test]
    fn p5_prose_wiki_link_does_not_wire_the_chapter_yet() {
        let root = book("p5prose");
        write_card(&root, "001", "2-4", "| 2 | 起 | — | U-001 |\n| 4 | 结 | U-001 | — |", "");
        write_outline_raw(&root, 2, "## 本章目标\n\n没写归属。\n");
        write_prose(&root, 2);
        fs::write(root.join("正文").join("第002章_测试.md"), "见 [[事件:001]]。\n").unwrap();

        let audit = audit(&root).unwrap();
        assert_eq!(audit.declared_chapters, 0, "正文标记不参与：{audit:?}");
        assert!(codes(&audit).contains(&"outline_silent"), "{:?}", codes(&audit));
    }

    /// 两本账各列一遍，对不上的地方单独列差异；谁都不改谁。
    #[test]
    fn p6_settlements_are_listed_from_both_books_without_merging() {
        let root = book("p6twoBooks");
        write_card(
            &root,
            "002",
            "2-5",
            "| 3 | 推 | — | — |",
            "| U-001 | 3 | 第三联交出去了 |",
        );
        fs::create_dir_all(root.join("追踪")).unwrap();
        fs::write(
            root.join("追踪/伏笔台账.md"),
            "| 编号 | 结算于 | 方式 |\n| --- | --- | --- |\n| U-001 | 4 | 台账记的章不同 |\n| U-009 | 5 | 只在追踪里结过 |\n",
        )
        .unwrap();

        let audit = audit(&root).unwrap();
        assert_eq!(audit.settled_from_card.len(), 1, "{audit:?}");
        assert_eq!(audit.settled_from_card[0].event, "002");
        assert_eq!(audit.settled_from_card[0].code, "U-001");
        assert_eq!(audit.settled_from_card[0].chapter, Some(3));
        assert_eq!(audit.settled_from_tracking.len(), 2, "{audit:?}");
        let kinds: Vec<&str> = audit
            .settlement_diff
            .iter()
            .map(|diff| diff.kind.as_str())
            .collect();
        assert!(
            kinds.contains(&"chapter_mismatch"),
            "{:?}",
            audit.settlement_diff
        );
        assert!(kinds.contains(&"tracking_only"), "{:?}", audit.settlement_diff);
        // 不自动合并：卡内账本还是那一行，追踪也没有被改。
        assert_eq!(audit.settled_from_card.len(), 1, "{audit:?}");
        assert_eq!(audit.settled_from_tracking.len(), 2, "{audit:?}");
    }

    /// 台账里的日期不是章号，别拿它比出假差异。
    #[test]
    fn p6_tracking_dates_are_not_mistaken_for_chapters() {
        let root = book("p6dates");
        write_card(&root, "002", "2-5", "| 3 | 推 | — | — |", "| U-001 | 3 | 结过 |");
        fs::create_dir_all(root.join("追踪")).unwrap();
        fs::write(
            root.join("追踪/时间线.md"),
            "- 2026-11-01 U-001 第三联交出去\n",
        )
        .unwrap();

        let audit = audit(&root).unwrap();
        let row = audit
            .settled_from_tracking
            .first()
            .expect("台账里那一行要认得出来");
        assert_eq!(row.code, "U-001");
        assert_eq!(row.chapter, None, "日期不是章号：{row:?}");
        assert!(audit.settlement_diff.is_empty(), "{:?}", audit.settlement_diff);
    }

    /// 正文双链只记录在哪一章出现，不把人算给那个事件。
    #[test]
    fn p6_body_mentions_are_recorded_without_wiring_the_chapter() {
        let root = book("p6bodyMentions");
        write_card(&root, "002", "2-5", "| 3 | 推 | — | — |", "");
        write_outline(&root, 3, "无");
        fs::write(
            root.join("正文").join("第003章_码头.md"),
            "他翻出那张 [[事件:002]]，又想起 [[事件:007]]；同一章再提一次 [[事件:002]]。\n",
        )
        .unwrap();

        let audit = audit(&root).unwrap();
        assert_eq!(
            audit.body_mentions,
            vec![
                BodyMention {
                    chapter: 3,
                    event: "002".to_string(),
                },
                BodyMention {
                    chapter: 3,
                    event: "007".to_string(),
                },
            ],
            "{audit:?}"
        );
        let row = audit
            .map
            .iter()
            .find(|row| row.event == "002")
            .expect("002 在清单里");
        assert!(
            !row.declared_chapters.contains(&3),
            "正文标记不改归属：{row:?}"
        );
    }
}
