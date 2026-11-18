//! 事件账本同步：从写手回复里认出「这一章结算了哪些未结项」，写回事件卡的 `## 已结算账本`。
//!
//! 唯一写入者原则照抄 R1（`memory::sync_chapter`）：模型只在回复末尾的摘要行里声明
//! 「回收伏笔」，落盘由编排层做。让模型直接改卡的话，它每次都会整篇重写，
//! 账本的历史（哪一章结的、怎么结的）就一次一次被抹掉，逾期判断跟着失效。

use std::path::Path;

use crate::models::event::{self, EventCard};
use crate::models::StoryError;
use crate::services::{atomic_write, context};

/// 一次账本同步的结果，编排层用它决定要不要往时间线上说一句。
#[derive(Debug, Clone, Default)]
pub struct LedgerReport {
    pub chapter: u32,
    /// 这一章挂在哪个事件上（细纲「所属事件」）
    pub event: Option<String>,
    /// 这次新记进账本的编号或锚词
    pub settled: Vec<String>,
    pub wrote: bool,
    /// 该说但没写的情况（卡不存在、摘要行缺失）
    pub note: Option<String>,
}

/// 从写手回复里抽「回收伏笔」那一列。
///
/// 摘要行的格式由 `intent::writer_task` 钉死：八段竖线分隔，第 7 段是回收伏笔。
/// 找不到就返回空——不猜、不拿正文里出现的锚词硬凑，那会把「提到」当成「结算」。
fn recap_cell(reply: &str) -> Option<String> {
    reply.lines().find_map(|line| {
        let cells: Vec<&str> = line.split('|').map(str::trim).collect();
        if cells.len() < 8 || !cells[0].starts_with('第') {
            return None;
        }
        cells.get(6).map(|cell| cell.to_string())
    })
}

/// 摘要行里声明回收的项：编号直接命中，或者任一锚词出现在这一列里。
fn declared_settlements(card: &EventCard, cell: &str) -> Vec<String> {
    if cell.is_empty() || cell.contains('无') || cell.contains("没有") {
        return Vec::new();
    }
    card.open_items
        .iter()
        .filter(|item| {
            !card.is_settled(&item.code)
                && (cell.contains(item.code.as_str())
                    || item
                        .anchors
                        .iter()
                        .any(|anchor| !anchor.is_empty() && cell.contains(anchor.as_str())))
        })
        .map(|item| item.code.clone())
        .collect()
}

/// 表格行转列表行：`| U-001 | 7 | 说明 |` → `- U-001 | 7 | 说明`。
fn bulletize(row: &str) -> String {
    let inner = row.trim().trim_matches('|').trim();
    format!("- {inner}")
}

/// 把账本行插进「已结算账本」那一节；没有这一节就整节补上。
///
/// 认节用 `event::is_ledger_heading`，与解析层同一套别名。手写卡写成 `## 结算账本` 时，
/// 这里若只认 `## 已结算账本`，就会在文件末尾另起一节；解析层只看第一节，
/// 新账写进去也永远读不出来。节里是表格就续表格，是列表就续列表，保持手写卡原样。
fn append_rows(body: &str, rows: &[String]) -> String {
    const HEADING: &str = "## 已结算账本";
    const HEADER: &str = "| 编号 | 结算于 | 方式 |\n| --- | --- | --- |";
    let mut lines: Vec<String> = body.lines().map(|l| l.to_string()).collect();

    let Some(start) = lines.iter().position(|l| event::is_ledger_heading(l)) else {
        let mut out = body.trim_end().to_string();
        out.push_str("\n\n");
        out.push_str(HEADING);
        out.push_str("\n\n");
        out.push_str(HEADER);
        out.push('\n');
        for row in rows {
            out.push_str(row);
            out.push('\n');
        }
        return out;
    };

    // 这一节到下一个 `## ` 之间。
    let end = lines
        .iter()
        .enumerate()
        .skip(start + 1)
        .find(|(_, l)| l.trim_start().starts_with("## "))
        .map(|(i, _)| i)
        .unwrap_or(lines.len());

    let is_pipe = |i: usize| {
        let t = lines[i].trim_start();
        t.starts_with('|') || t.starts_with('｜')
    };
    let is_bullet = |i: usize| {
        let t = lines[i].trim_start();
        t.starts_with("- ") || t.starts_with("* ")
    };
    let table_rows: Vec<usize> = (start + 1..end).filter(|&i| is_pipe(i)).collect();
    let bullet_rows: Vec<usize> = (start + 1..end).filter(|&i| is_bullet(i)).collect();
    let bullet_style = table_rows.is_empty() && !bullet_rows.is_empty();
    let anchor = table_rows
        .last()
        .or_else(|| bullet_rows.last())
        .copied();

    match anchor {
        Some(last) => {
            let mut at = last + 1;
            for row in rows {
                lines.insert(
                    at,
                    if bullet_style {
                        bulletize(row)
                    } else {
                        row.clone()
                    },
                );
                at += 1;
            }
        }
        None => {
            let mut at = start + 1;
            for line in HEADER.lines().chain(rows.iter().map(|r| r.as_str())) {
                lines.insert(at, line.to_string());
                at += 1;
            }
            lines.insert(at, String::new());
        }
    }

    let mut out = lines.join("\n");
    if body.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// 把这一章结算的项写进事件卡。无事件归属时静默返回。
pub fn sync_from_reply(
    book_root: &Path,
    chapter: u32,
    reply: &str,
) -> Result<LedgerReport, StoryError> {
    let mut report = LedgerReport {
        chapter,
        ..Default::default()
    };

    // 细纲写了「所属事件」就照它记；没写就看这一章被哪张卡盖住。
    // 只盖住一张的时候直接记那张——手写卡刚起步时，350 份细纲多半还没补这一行，
    // 若坚持要「所属事件」才记账，账本就永远停在空表上。
    let declared = match context::declared_event(book_root, chapter) {
        Some(id) => Some(id),
        // 细纲写了「所属事件：无」是作者的明确表态，不能拿覆盖关系去替。
        None if context::outline_mentions_event(book_root, chapter) => None,
        None => match context::events_covering(book_root, chapter).as_slice() {
            [only] => Some(only.id.clone()),
            [] => None,
            many => {
                let ids: Vec<String> = many.iter().map(|card| card.id.clone()).collect();
                report.note = Some(format!(
                    "事件账本：第 {chapter} 章盖着 {} 张事件卡（{}），细纲里没写「所属事件」，账本没记。",
                    many.len(),
                    ids.join("、")
                ));
                return Ok(report);
            }
        },
    };
    let Some(declared) = declared else {
        return Ok(report);
    };
    report.event = Some(declared.clone());

    let Some(card) = context::find_event_card(book_root, &declared) else {
        report.note = Some(format!(
            "事件账本：第 {chapter} 章挂在事件 {declared} 上，但 大纲/事件/ 下没有这张卡，账本没记。"
        ));
        return Ok(report);
    };

    let Some(cell) = recap_cell(reply) else {
        report.note = Some(format!(
            "事件账本：第 {chapter} 章的回复里没有摘要行，回收了哪些未结项无从判断，账本没记。"
        ));
        return Ok(report);
    };

    let codes = declared_settlements(&card, &cell);
    if codes.is_empty() {
        return Ok(report);
    }

    let rows: Vec<String> = codes
        .iter()
        .map(|code| format!("| {code} | {chapter} | 写手摘要回收：{cell} |"))
        .collect();
    let path = book_root.join(&card.path);
    let body = std::fs::read_to_string(&path).map_err(|e| StoryError::fs(path.display().to_string(), e.to_string()))?;
    atomic_write::write_atomic(&path, append_rows(&body, &rows).as_bytes())?;

    report.settled = codes;
    report.wrote = true;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn book(tag: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let root = std::env::temp_dir().join(format!("storydesk-ledger-{tag}-{stamp}"));
        std::fs::create_dir_all(root.join("大纲").join("事件")).unwrap();
        std::fs::create_dir_all(root.join("正文")).unwrap();
        root
    }

    const CARD: &str = "# 事件 002 · 码头交接

## 基本信息

| 所属卷 | 章号区间 | 前置事件 | 状态 |
| --- | --- | --- | --- |
| 1 | 2-5 | — | active |

## 未结项

| 编号 | 内容 | 埋于 | 最迟结算章 | 锚词 |
| --- | --- | --- | --- | --- |
| U-001 | 仓单第三联不在 | 2 | 3 | 第三联、复印件 |

## 已结算账本

| 编号 | 结算于 | 方式 |
| --- | --- | --- |

## 章型分工

| 章 | 章型 | 本章消耗 | 本章新增 |
| --- | --- | --- | --- |
| 3 | 推 | U-001 | — |
";

    fn write_card(root: &Path) {
        std::fs::write(root.join("大纲/事件/事件_002_码头交接.md"), CARD).unwrap();
    }

    fn write_outline(root: &Path, declared: &str) {
        std::fs::write(
            root.join("大纲/细纲_第003章.md"),
            format!("# 第 3 章\n\n所属事件：{declared}\n\n## 本章目标\n\n对账。\n"),
        )
        .unwrap();
    }

    const REPLY: &str = "正文写完。\n\n第3章 | 第 4 天傍晚 | 码头 | 林昭、老周 | 对账发现缺页 | — | U-001 | 有人在后门等";

    #[test]
    fn settles_a_declared_item_and_writes_the_row() {
        let root = book("hit");
        write_card(&root);
        write_outline(&root, "事件 002 · 码头交接");

        let report = sync_from_reply(&root, 3, REPLY).unwrap();
        assert!(report.wrote, "有回收声明就该写账本");
        assert_eq!(report.settled, vec!["U-001".to_string()]);

        let body = std::fs::read_to_string(root.join("大纲/事件/事件_002_码头交接.md")).unwrap();
        assert!(body.contains("| U-001 | 3 | 写手摘要回收："), "账本行没落盘：\n{body}");
        let card = crate::models::event::parse_card("大纲/事件/事件_002_码头交接.md", &body).unwrap();
        assert!(card.is_settled("U-001"));
        assert!(card.open_at(4).iter().all(|i| i.code != "U-001"), "结过的项不该再算未结");
    }

    #[test]
    fn anchor_words_count_as_a_settlement() {
        let root = book("anchor");
        write_card(&root);
        write_outline(&root, "事件 002 · 码头交接");
        let reply = "第3章 | 第 4 天 | 码头 | 林昭 | 找到第三联 | — | 第三联 | —";
        let report = sync_from_reply(&root, 3, reply).unwrap();
        assert!(report.wrote);
        assert_eq!(report.settled, vec!["U-001".to_string()]);
    }

    #[test]
    fn running_twice_does_not_duplicate_the_row() {
        let root = book("twice");
        write_card(&root);
        write_outline(&root, "事件 002 · 码头交接");
        assert!(sync_from_reply(&root, 3, REPLY).unwrap().wrote);
        let second = sync_from_reply(&root, 3, REPLY).unwrap();
        assert!(!second.wrote, "已经结过的不该再写一遍");
        let body = std::fs::read_to_string(root.join("大纲/事件/事件_002_码头交接.md")).unwrap();
        assert_eq!(body.matches("| U-001 | 3 |").count(), 1, "账本行重复了：\n{body}");
    }

    #[test]
    fn no_declared_event_means_no_write() {
        let root = book("none");
        write_card(&root);
        write_outline(&root, "无");
        let report = sync_from_reply(&root, 3, REPLY).unwrap();
        assert!(!report.wrote);
        let body = std::fs::read_to_string(root.join("大纲/事件/事件_002_码头交接.md")).unwrap();
        assert_eq!(body, CARD, "没有事件归属时不该碰卡");
    }

    #[test]
    fn missing_summary_row_is_reported_not_guessed() {
        let root = book("nosummary");
        write_card(&root);
        write_outline(&root, "事件 002 · 码头交接");
        let report = sync_from_reply(&root, 3, "正文写完，没有摘要行。").unwrap();
        assert!(!report.wrote);
        assert!(report.note.unwrap().contains("没有摘要行"));
    }

    #[test]
    fn appends_a_ledger_section_when_the_card_has_none() {
        let root = book("nosection");
        std::fs::write(
            root.join("大纲/事件/事件_002_码头交接.md"),
            "# 事件 002 · 码头交接\n\n## 未结项\n\n| 编号 | 内容 | 埋于 | 最迟结算章 | 锚词 |\n| --- | --- | --- | --- | --- |\n| U-001 | 仓单第三联不在 | 2 | 3 | 第三联 |\n",
        )
        .unwrap();
        write_outline(&root, "事件 002 · 码头交接");
        assert!(sync_from_reply(&root, 3, REPLY).unwrap().wrote);
        let body = std::fs::read_to_string(root.join("大纲/事件/事件_002_码头交接.md")).unwrap();
        assert!(body.contains("## 已结算账本"));
        assert!(body.contains("| U-001 | 3 |"));
    }

    #[test]
    fn writes_into_a_hand_written_ledger_heading() {
        let root = book("handheading");
        std::fs::write(
            root.join("大纲/事件/事件_002_码头交接.md"),
            "# 事件 002 · 码头交接\n\n## 未结项\n\n| 编号 | 内容 | 埋于 | 最迟结算章 | 锚词 |\n| --- | --- | --- | --- | --- |\n| U-001 | 仓单第三联不在 | 2 | 3 | 第三联 |\n\n## 结算账本\n\n| 编号 | 结算于 | 方式 |\n| --- | --- | --- |\n",
        )
        .unwrap();
        write_outline(&root, "事件 002 · 码头交接");
        assert!(sync_from_reply(&root, 3, REPLY).unwrap().wrote);
        let body = std::fs::read_to_string(root.join("大纲/事件/事件_002_码头交接.md")).unwrap();
        assert_eq!(
            body.matches("结算账本").count(),
            1,
            "「## 结算账本」已是账本节，不该另起一节：{body}"
        );
        assert!(body.contains("| U-001 | 3 |"), "{body}");
    }

    #[test]
    fn keeps_a_bullet_ledger_as_bullets() {
        let root = book("bulletledger");
        std::fs::write(
            root.join("大纲/事件/事件_002_码头交接.md"),
            "# 事件 002 · 码头交接\n\n## 未结项\n\n| 编号 | 内容 | 埋于 | 最迟结算章 | 锚词 |\n| --- | --- | --- | --- | --- |\n| U-001 | 仓单第三联不在 | 2 | 3 | 第三联 |\n\n## 结算账本\n\n- U-000 | 1 | 旧账\n",
        )
        .unwrap();
        write_outline(&root, "事件 002 · 码头交接");
        assert!(sync_from_reply(&root, 3, REPLY).unwrap().wrote);
        let body = std::fs::read_to_string(root.join("大纲/事件/事件_002_码头交接.md")).unwrap();
        assert!(body.contains("- U-001 | 3 |"), "列表记账要续列表：{body}");
        assert!(!body.contains("| 编号 | 结算于 | 方式 |"), "{body}");
    }

    #[test]
    fn a_single_covering_card_catches_the_ledger_without_a_declared_event() {
        let root = book("fallback");
        write_card(&root);
        std::fs::write(
            root.join("大纲/细纲_第003章.md"),
            "# 第 3 章\n\n## 本章目标\n\n对账。\n",
        )
        .unwrap();
        let report = sync_from_reply(&root, 3, REPLY).unwrap();
        assert!(report.wrote, "只盖住一张卡就该记到那张：{report:?}");
        assert_eq!(report.event.as_deref(), Some("002"));
        let body = std::fs::read_to_string(root.join("大纲/事件/事件_002_码头交接.md")).unwrap();
        assert!(body.contains("| U-001 | 3 |"), "{body}");
    }
}
