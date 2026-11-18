//! 事件卡（`大纲/事件/事件_NNN_标题.md`）的解析层。
//!
//! 为什么要有这一层：在此之前，计划里的每一步都挂在一章上（`ChapterRange`），
//! 模型写第 N 章时能看到的跨章信息只有「上一章」——事件的寿命上限因此等于一章。
//! 「一章一天」不是模型偷懒，而是输入里没有比章更长的东西。
//! 事件卡把「还没办完的事」写成带编号、带最迟结算章、带字面锚词的未结项清单；
//! 后续的上下文注入与门禁从这份清单取料，不再依赖模型自觉。
//!
//! 这里只做**纯解析**：无 IO、无路径拼接、无全局状态。同一份正文在任何调用点
//! 解析结果一致，单测不需要建临时目录。目录扫描与读文件在 `commands/event.rs`。
//!
//! 容错口径与 `context.rs` 的「存在即输出」一致：缺节就是空，猜不出来的字段留
//! `None` 并记一条 `warnings`；只有**事件号**必须能确定（文件名或标题行），
//! 否则这张卡进不了索引，直接报错。

use serde::{Deserialize, Serialize};

/// 事件的生命周期状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EventStatus {
    /// 已规划但还没开始写。
    Planned,
    /// 正在展开，章号区间内还有未写的章。
    Active,
    /// 未结项清零，已结算。
    Settled,
}

/// 事件内某一章承担的位置。起—推—转—结四种，不是每章都必须各占一个。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EventRole {
    /// 起：本事件的开场，把未结项摆上台面。
    Setup,
    /// 推：推进，消耗已知未结项、制造新的。
    Develop,
    /// 转：翻转，让本章之前的判断失效。
    Turn,
    /// 结：结算，本事件的未结项必须在本章内清零或转交。
    Payoff,
}

impl EventRole {
    /// 用于日志与界面展示的中文标签。
    pub fn label(self) -> &'static str {
        match self {
            EventRole::Setup => "起",
            EventRole::Develop => "推",
            EventRole::Turn => "转",
            EventRole::Payoff => "结",
        }
    }
}

/// 计划步指向的事件。挂在 `PlanStep::event` 上，是「这一步在为哪个事件干活」的唯一来源。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventRef {
    /// 三位事件号，如 `001`；与文件名 `事件_001_*.md` 的编号一致。
    pub id: String,
    /// 这一步在该事件里承担的位置。缺省表示「照章型分工表走」。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<EventRole>,
}

impl EventRef {
    pub fn new(id: impl Into<String>) -> Self {
        Self { id: id.into(), role: None }
    }

    /// 「去写/更新事件卡」这一步的标记：`id` 留空，因为这一刻卡片还不存在。
    ///
    /// 事件卡步与「写某章细纲」在字段上长得一样（都是 `kind = Outline` +
    /// `lock = OutlineDir`），编排层靠 `event.is_some()` 区分；卡步不排章号，
    /// 也不能被当成细纲步拆开。
    pub fn card_step() -> Self {
        Self { id: String::new(), role: None }
    }

    /// 是不是「写事件卡」这一步（而不是「这一步属于某个已有事件」）。
    pub fn is_card_step(&self) -> bool {
        self.id.trim().is_empty()
    }
}

/// 未结项：一件已经写出来、但还没办完的事。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenItem {
    /// 事件内唯一编号，如 `U-001`。
    pub code: String,
    /// 一句话说清「到底是什么事还没办完」。
    pub text: String,
    /// 在第几章埋下的。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub planted: Option<u32>,
    /// 最迟必须在第几章结算；超过仍未结就是欠账。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due: Option<u32>,
    /// 正文里能直接检索到的字面词，用于写后连续性检查。
    pub anchors: Vec<String>,
}

/// 已结算项：从「未结项」里结掉的事，留痕用于防止翻案。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettledItem {
    pub code: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at_chapter: Option<u32>,
    /// 结算方式，一句话。
    pub note: String,
}

/// 章型分工表的一行：这一章在事件里干什么。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChapterRole {
    pub chapter: u32,
    pub role: EventRole,
    /// 本章应消耗（推进/结算）的未结项编号。
    pub consumes: Vec<String>,
    /// 本章预计新埋的未结项编号。
    pub plants: Vec<String>,
}

/// 一整张事件卡。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventCard {
    /// 三位事件号。
    pub id: String,
    /// 事件名，取自标题行。
    pub title: String,
    /// 相对书根的路径，统一用 `/`。
    pub path: String,
    /// 所属卷号；没有这一节时为 None。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume: Option<u32>,
    /// 章号区间 `(start, end)`，闭区间。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span: Option<(u32, u32)>,
    /// 前置事件号：本事件要等它结算才能开始。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prev: Option<String>,
    pub status: EventStatus,
    /// 结算条件：满足这几条才算这个事件写完了。
    pub settle: Vec<String>,
    pub open_items: Vec<OpenItem>,
    pub ledger: Vec<SettledItem>,
    pub roles: Vec<ChapterRole>,
    /// 解析时的非致命问题，原样交给界面提示，不阻断生成。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

impl EventCard {
    /// 未结项里最晚的结算章号；事件规划用它判断「这个事件最多撑到第几章」。
    pub fn last_due(&self) -> Option<u32> {
        self.open_items.iter().filter_map(|item| item.due).max()
    }

    /// 本事件已占用/计划占用的章号区间；`章型分工` 与 `章号区间` 取并。
    pub fn effective_span(&self) -> Option<(u32, u32)> {
        let from_roles = self.roles.iter().map(|r| r.chapter).fold(
            None::<(u32, u32)>,
            |acc, ch| match acc {
                None => Some((ch, ch)),
                Some((lo, hi)) => Some((lo.min(ch), hi.max(ch))),
            },
        );
        match (self.span, from_roles) {
            (Some((a, b)), Some((c, d))) => Some((a.min(c), b.max(d))),
            (Some(span), None) => Some(span),
            (None, Some(span)) => Some(span),
            (None, None) => None,
        }
    }

    /// 某一章在事件里的分工。没有章型分工表时返回 None。
    pub fn role_of(&self, chapter: u32) -> Option<&ChapterRole> {
        self.roles.iter().find(|r| r.chapter == chapter)
    }

    /// 截至第 `chapter` 章仍未结算的未结项；写第 N 章时注入的就是这一份。
    pub fn open_at(&self, chapter: u32) -> Vec<&OpenItem> {
        self.open_items
            .iter()
            .filter(|item| !self.is_settled(&item.code))
            .filter(|item| item.planted.map(|p| p <= chapter).unwrap_or(true))
            .collect()
    }

    /// 已经到期、账本上却还没结的未结项；门禁用它报欠账。
    pub fn overdue_at(&self, chapter: u32) -> Vec<&OpenItem> {
        self.open_at(chapter)
            .into_iter()
            .filter(|item| item.due.map(|d| d < chapter).unwrap_or(false))
            .collect()
    }

    /// 账本里有没有这个编号。
    pub fn is_settled(&self, code: &str) -> bool {
        self.ledger.iter().any(|entry| entry.code == code)
    }
}

/// 列表用的轻量摘要：卡片列表不需要整张卡。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventSummary {
    pub id: String,
    pub title: String,
    pub path: String,
    pub status: EventStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span: Option<(u32, u32)>,
    /// 尚未结算的未结项数量。
    pub open_count: usize,
    /// 章型分工表覆盖的章数。
    pub role_count: usize,
}

impl From<&EventCard> for EventSummary {
    fn from(card: &EventCard) -> Self {
        Self {
            id: card.id.clone(),
            title: card.title.clone(),
            path: card.path.clone(),
            status: card.status,
            span: card.effective_span(),
            open_count: card
                .open_items
                .iter()
                .filter(|item| !card.is_settled(&item.code))
                .count(),
            role_count: card.roles.len(),
        }
    }
}

/// 单张卡解析失败：不是整批失败的理由，列表里带出来让用户自己改。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventParseError {
    pub path: String,
    pub message: String,
}

/// `event_list` 的返回体。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventList {
    pub cards: Vec<EventSummary>,
    pub errors: Vec<EventParseError>,
}

/// 把任意输入（`12`、`012`、`事件_012`、`大纲/事件/事件_012_码头.md`）规整成三位事件号。
pub fn normalise_id(raw: &str) -> Option<String> {
    let file = raw.rsplit(|c| c == '/' || c == '\\').next().unwrap_or(raw);
    if let Some(id) = id_from_file_name(file) {
        return Some(id);
    }
    let digits: String = raw.chars().filter(|c| c.is_ascii_digit()).take(4).collect();
    if digits.is_empty() {
        return None;
    }
    Some(format!("{:03}", digits.parse::<u32>().ok()?))
}

/// 从文件名取事件号：`事件_001_码头交接.md` → `001`。
pub fn id_from_file_name(name: &str) -> Option<String> {
    let stem = name.trim();
    let stem = stem
        .strip_suffix(".md")
        .or_else(|| stem.strip_suffix(".MD"))
        .unwrap_or(stem);
    let rest = stem.strip_prefix("事件").unwrap_or(stem);
    let rest = rest.trim_start_matches(|c: char| c == '_' || c == '-' || c == ' ' || c == '·');
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() || digits.len() > 4 {
        return None;
    }
    let n = digits.parse::<u32>().ok()?;
    Some(format!("{n:03}"))
}

/// 从标题行取事件号：`# 事件 001 · 码头交接` → `001`。
fn heading_id(heading: &str) -> Option<String> {
    let t = heading.trim().trim_start_matches('#').trim();
    let rest = t.strip_prefix("事件")?;
    let digits: String = rest
        .trim_start_matches(|c: char| c == '_' || c == '-' || c == ' ' || c == '·')
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    if digits.is_empty() {
        return None;
    }
    Some(format!("{:03}", digits.parse::<u32>().ok()?))
}

/// 标题行去掉「事件 + 编号」前缀，剩下的就是事件名。
fn title_from_heading(heading: &str) -> String {
    let t = heading.trim().trim_start_matches('#').trim();
    let rest = t.strip_prefix("事件").unwrap_or(t).trim_start();
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    let rest = if digits.is_empty() { rest } else { &rest[digits.len()..] };
    rest.trim_start_matches(|c: char| {
        c == '_' || c == '-' || c == '—' || c == '·' || c == ':' || c == '：' || c.is_whitespace()
    })
    .trim()
    .to_string()
}

/// 文件名兜底的事件名：`事件_001_码头交接.md` → `码头交接`。
fn title_from_file_name(name: &str) -> Option<String> {
    let stem = name.trim();
    let stem = stem
        .strip_suffix(".md")
        .or_else(|| stem.strip_suffix(".MD"))
        .unwrap_or(stem);
    let rest = stem.strip_prefix("事件").unwrap_or(stem);
    let rest = rest.trim_start_matches(|c: char| c == '_' || c == '-' || c == ' ' || c == '·');
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    let after = &rest[digits.len().min(rest.len())..];
    let title = after.trim_start_matches(|c: char| c == '_' || c == '-' || c == '·' || c == ' ');
    if title.trim().is_empty() {
        None
    } else {
        Some(title.trim().to_string())
    }
}

/// 按 `## 小节` 切块，顺带取出唯一的 `# 标题`。
fn split_sections(body: &str) -> (Option<String>, Vec<(String, Vec<String>)>) {
    let mut heading: Option<String> = None;
    let mut sections: Vec<(String, Vec<String>)> = Vec::new();
    let mut current: Option<(String, Vec<String>)> = None;
    for raw in body.lines() {
        let line = raw.trim_end();
        if let Some(rest) = line.strip_prefix("## ") {
            if let Some(done) = current.take() {
                sections.push(done);
            }
            current = Some((rest.trim().to_string(), Vec::new()));
        } else if line.starts_with("# ") && heading.is_none() {
            heading = Some(line.trim_start_matches('#').trim().to_string());
        } else if let Some((_, lines)) = current.as_mut() {
            lines.push(line.to_string());
        }
    }
    if let Some(done) = current {
        sections.push(done);
    }
    (heading, sections)
}

/// 小节标题归一：去掉空白、全角标点与装饰符号，只留汉字与字母数字。
///
/// 手写卡的标题会带「一、」「（必填）」这类前缀后缀，也会用全角括号；
/// 归一之后 `## 一、未结项清单（必填）` 与 `## 未结项` 才认成同一节。
fn normalise_title(raw: &str) -> String {
    raw.chars()
        .filter(|c| {
            !c.is_whitespace()
                && !matches!(
                    c,
                    '：' | ':' | '（' | '）' | '(' | ')' | '、' | ',' | '，' | '。' | '·' | '-'
                        | '—' | '【' | '】' | '[' | ']' | '#' | '*' | '`' | '"' | '\'' | '“'
                        | '”' | '？' | '?' | '!'
                )
        })
        .collect()
}

/// 各节的标题别名。手写卡不会照模板一字不差，别名表按「最常见的换说法」收。
const SEC_INFO: &[&str] = &["基本信息", "基础信息", "事件信息", "卡片信息", "元信息", "事件概览"];
const SEC_SETTLE: &[&str] = &[
    "结算条件",
    "结清条件",
    "完成条件",
    "收尾条件",
    "结算标准",
    "结项条件",
    "何时算完",
];
const SEC_OPEN: &[&str] = &[
    "未结项",
    "未结事项",
    "待结项",
    "待结算",
    "开放项",
    "未了项",
    "悬念清单",
    "挂账",
];
const SEC_LEDGER: &[&str] = &["已结算", "结算账本", "账本", "已结项", "结算记录", "已回收"];
const SEC_ROLES: &[&str] = &[
    "章型分工",
    "章节分工",
    "章型表",
    "章型",
    "分章",
    "章节安排",
    "章内分工",
    "排章",
];

/// 按别名找节：先精确，再按包含（别名至少两个字，免得多字标题互相吃掉）。
fn find_section<'a>(
    sections: &'a [(String, Vec<String>)],
    aliases: &[&str],
) -> Option<&'a Vec<String>> {
    let exact = |title: &str| {
        let t = normalise_title(title);
        aliases.iter().any(|a| t == *a)
    };
    if let Some((_, lines)) = sections.iter().find(|(title, _)| exact(title)) {
        return Some(lines);
    }
    let loose = |title: &str| {
        let t = normalise_title(title);
        aliases
            .iter()
            .any(|a| a.chars().count() >= 2 && t.contains(*a))
    };
    sections
        .iter()
        .find(|(title, _)| loose(title))
        .map(|(_, lines)| lines)
}

/// 这一行是不是「已结算账本」那一节的标题。账本回写要用：手写卡写成
/// `## 结算账本` 时，回写若认不出，会另起一节，解析层只看第一节，新账就永远读不到。
pub(crate) fn is_ledger_heading(line: &str) -> bool {
    let t = line.trim();
    let t = t.strip_prefix("##").unwrap_or(t).trim();
    let n = normalise_title(t);
    SEC_LEDGER
        .iter()
        .any(|a| n == *a || (a.chars().count() >= 2 && n.contains(*a)))
}

/// 一列在表头里的位置：先精确命中，再按包含命中；已分配过的列不再给第二个字段。
///
/// 全由位置决定列的话，手写卡把「锚词」写到「最迟结算章」前面，两项会静默串位——
/// 那是最坏的一种错：能解析、不报警、账目全错。所以表头能认就按表头认。
fn column(header: &[String], aliases: &[&str], used: &mut Vec<usize>) -> Option<usize> {
    let norm: Vec<String> = header.iter().map(|h| normalise_title(h)).collect();
    let free = |i: usize| !used.contains(&i);
    if let Some(i) = (0..norm.len()).find(|&i| free(i) && aliases.iter().any(|a| norm[i] == *a)) {
        used.push(i);
        return Some(i);
    }
    if let Some(i) = (0..norm.len()).find(|&i| {
        free(i) && aliases.iter().any(|a| a.chars().count() >= 2 && norm[i].contains(*a))
    }) {
        used.push(i);
        return Some(i);
    }
    None
}

const COL_CODE: &[&str] = &["编号", "项号", "代号", "代码", "序号"];
const COL_TEXT: &[&str] = &["内容", "说明", "描述", "事项", "是什么", "问题"];
const COL_PLANTED: &[&str] = &["埋于", "埋于章", "起始", "首次", "出现于"];
const COL_DUE: &[&str] = &["最迟结算章", "最迟", "期限", "截止", "到期", "结算章"];
const COL_ANCHORS: &[&str] = &["锚词", "锚点", "关键词", "字面", "标记"];

const COL_CHAPTER: &[&str] = &["章", "章号", "章节", "第几章", "章次"];
const COL_ROLE: &[&str] = &["章型", "类型", "位置", "角色", "作用", "分工"];
const COL_CONSUMES: &[&str] = &["本章消耗", "消耗", "收回", "回收"];
const COL_PLANTS: &[&str] = &["本章新增", "新增", "新开", "埋"];

const COL_LEDGER_AT: &[&str] = &["结算于", "结算章", "第几章", "章", "位置"];
const COL_LEDGER_NOTE: &[&str] = &["方式", "说明", "备注", "方法", "怎么结的"];

/// 未结项的列位置。表头认不出来时退回模板顺序：编号|内容|埋于|最迟结算章|锚词。
#[derive(Clone, Copy)]
struct OpenCols {
    code: Option<usize>,
    text: Option<usize>,
    planted: Option<usize>,
    due: Option<usize>,
    anchors: Option<usize>,
}

impl OpenCols {
    fn positional() -> Self {
        Self {
            code: Some(0),
            text: Some(1),
            planted: Some(2),
            due: Some(3),
            anchors: Some(4),
        }
    }

    fn from_header(header: &[String]) -> Self {
        if header.is_empty() {
            return Self::positional();
        }
        let mut used = Vec::new();
        let code = column(header, COL_CODE, &mut used).or(Some(0));
        let text = column(header, COL_TEXT, &mut used)
            .or_else(|| (0..header.len()).find(|i| Some(*i) != code));
        let due = column(header, COL_DUE, &mut used);
        let planted = column(header, COL_PLANTED, &mut used);
        let anchors = column(header, COL_ANCHORS, &mut used);
        Self {
            code,
            text,
            planted,
            due,
            anchors,
        }
    }
}

#[derive(Clone, Copy)]
struct RoleCols {
    chapter: Option<usize>,
    role: Option<usize>,
    consumes: Option<usize>,
    plants: Option<usize>,
}

impl RoleCols {
    fn positional() -> Self {
        Self {
            chapter: Some(0),
            role: Some(1),
            consumes: Some(2),
            plants: Some(3),
        }
    }

    fn from_header(header: &[String]) -> Self {
        if header.is_empty() {
            return Self::positional();
        }
        let mut used = Vec::new();
        Self {
            chapter: column(header, COL_CHAPTER, &mut used).or(Some(0)),
            role: column(header, COL_ROLE, &mut used).or(Some(1)),
            consumes: column(header, COL_CONSUMES, &mut used),
            plants: column(header, COL_PLANTS, &mut used),
        }
    }
}

#[derive(Clone, Copy)]
struct LedgerCols {
    code: Option<usize>,
    at: Option<usize>,
    note: Option<usize>,
}

impl LedgerCols {
    fn positional() -> Self {
        Self {
            code: Some(0),
            at: Some(1),
            note: Some(2),
        }
    }

    fn from_header(header: &[String]) -> Self {
        if header.is_empty() {
            return Self::positional();
        }
        let mut used = Vec::new();
        Self {
            code: column(header, COL_CODE, &mut used).or(Some(0)),
            at: column(header, COL_LEDGER_AT, &mut used),
            note: column(header, COL_LEDGER_NOTE, &mut used),
        }
    }
}

/// 一行里按竖线切格：半角 `|` 与全角 `｜` 都认（中文输入法下全角是常态）。
fn split_pipes(line: &str) -> Vec<String> {
    let converted = line.replace('｜', "|");
    let cells: Vec<String> = converted.split('|').map(|c| c.trim().to_string()).collect();
    let start = cells.iter().position(|c| !c.is_empty());
    let Some(start) = start else {
        return Vec::new();
    };
    let end = cells
        .iter()
        .rposition(|c| !c.is_empty())
        .map(|i| i + 1)
        .unwrap_or(start);
    cells[start..end].to_vec()
}

/// 编号去掉尾随注记：`U-001（第 7 章）` → `U-001`。手写喜欢在编号后面补一句说明，
/// 不去掉的话账本匹配（按编号相等）永远对不上。
fn clean_code(raw: &str) -> String {
    let t = raw.trim();
    let cut = t.find(|c| c == '（' || c == '(').unwrap_or(t.len());
    t[..cut]
        .trim()
        .trim_end_matches(|c| c == '：' || c == ':')
        .trim()
        .to_string()
}

/// 一节里的列表行（非表格行），已去掉 `- `、`1. ` 这类前缀。
fn bullet_lines(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .filter(|line| {
            let t = line.trim_start();
            !t.is_empty() && !t.starts_with('|') && !t.starts_with('｜') && !t.starts_with("```")
        })
        .map(|line| strip_bullet(line).to_string())
        .filter(|line| !line.is_empty())
        .collect()
}

/// 拆出行尾括号里的字段：`…（埋于 12，最迟 15，锚词：蓝色字迹）`。
fn split_tail_parenthetical(raw: &str) -> (String, Option<String>) {
    let trimmed = raw.trim_end();
    let Some(pos) = trimmed.rfind(|c| c == '（' || c == '(') else {
        return (trimmed.to_string(), None);
    };
    let open = trimmed[pos..].chars().next().unwrap_or('（');
    let close = if open == '（' { '）' } else { ')' };
    if !trimmed.ends_with(close) {
        return (trimmed.to_string(), None);
    }
    let inner = trimmed[pos + open.len_utf8()..trimmed.len() - close.len_utf8()].to_string();
    (trimmed[..pos].trim().to_string(), Some(inner))
}

/// 括号里的 `键：值` 对，全角逗号/冒号/竖线都当分隔符。
fn meta_fields(inner: &str) -> Vec<(String, String)> {
    inner
        .split(|c| matches!(c, '，' | ',' | '；' | ';' | '｜' | '|'))
        .filter_map(|part| {
            let p = part.trim();
            if p.is_empty() {
                return None;
            }
            if let Some((k, v)) = p.split_once('：').or_else(|| p.split_once(':')) {
                return Some((normalise_title(k), v.trim().to_string()));
            }
            // `埋于 7`、`最迟 10` 这种不带冒号的写法：第一个词当键，其余当值。
            let mut it = p.splitn(2, char::is_whitespace);
            let key = it.next().unwrap_or("").trim();
            let value = it.next().unwrap_or("").trim();
            if key.is_empty() || value.is_empty() {
                return None;
            }
            Some((normalise_title(key), value.to_string()))
        })
        .collect()
}

fn meta_lookup(meta: &[(String, String)], keys: &[&str]) -> Option<String> {
    meta.iter()
        .find(|(k, _)| keys.iter().any(|key| k == key || k.contains(*key)))
        .map(|(_, v)| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// 列表行开头的编号：`U-001 谁补的字` → (`U-001`, `谁补的字`)。
///
/// 只认「字母+数字」或带 `-`/`_` 的前缀；纯数字开头是章节号/序号，不当编号。
fn split_code_prefix(head: &str) -> (String, String) {
    let head = head.trim();
    let cut = head
        .find(|c: char| c.is_whitespace() || c == '：' || c == ':')
        .unwrap_or(head.len());
    let (first, rest) = head.split_at(cut);
    let has_digit = first.chars().any(|c| c.is_ascii_digit());
    let has_alpha = first.chars().any(|c| c.is_ascii_alphabetic());
    let shaped = has_digit && (has_alpha || first.contains('-') || first.contains('_'));
    if shaped && first.len() <= 24 {
        let text = rest
            .trim_start_matches(|c: char| c.is_whitespace() || c == '：' || c == ':')
            .to_string();
        (clean_code(first), text)
    } else {
        (String::new(), head.to_string())
    }
}

/// 没写编号的未结项按出现顺序补 `U-001` 起，跳过已经占用的号。
fn auto_code(existing: &[OpenItem], seq: usize) -> String {
    let mut n = seq;
    loop {
        let candidate = format!("U-{n:03}");
        if !existing.iter().any(|item| item.code == candidate) {
            return candidate;
        }
        n += 1;
    }
}

/// 章列里可能是一段区间（`12—13`），展开成逐章。
fn chapters_in(raw: &str) -> Vec<u32> {
    let Some((from, to)) = parse_span(raw) else {
        return Vec::new();
    };
    if to < from {
        return Vec::new();
    }
    let cap = (to - from).min(200);
    (from..=from + cap).collect()
}

fn is_cn_digit(c: char) -> bool {
    matches!(
        c,
        '零' | '〇'
            | '一'
            | '二'
            | '两'
            | '三'
            | '四'
            | '五'
            | '六'
            | '七'
            | '八'
            | '九'
    )
}

fn cn_digit_value(c: char) -> Option<u32> {
    match c {
        '零' | '〇' => Some(0),
        '一' => Some(1),
        '二' | '两' => Some(2),
        '三' => Some(3),
        '四' => Some(4),
        '五' => Some(5),
        '六' => Some(6),
        '七' => Some(7),
        '八' => Some(8),
        '九' => Some(9),
        _ => None,
    }
}

/// 中文数字：`十二` → 12、`一百零五` → 105。只认几乎全由数字构成的串，
/// `三天后` 这种正文句子必须原样返回 None——把它当第 3 章比认不出来更糟。
fn cn_number(raw: &str) -> Option<u32> {
    let structural = |c: char| {
        matches!(
            c,
            '第' | '章' | '卷' | '节' | '回' | ' ' | '\t' | '　' | '、' | ',' | '，' | '—' | '-'
        )
    };
    let core: String = raw.chars().filter(|c| !structural(*c)).collect();
    if core.is_empty() {
        return None;
    }
    if !core
        .chars()
        .all(|c| is_cn_digit(c) || matches!(c, '十' | '百' | '千'))
    {
        return None;
    }
    let mut total = 0u32;
    let mut current = 0u32;
    for ch in core.chars() {
        match ch {
            '十' | '百' | '千' => {
                let base = if current == 0 { 1 } else { current };
                total += base
                    * match ch {
                        '十' => 10,
                        '百' => 100,
                        _ => 1000,
                    };
                current = 0;
            }
            other => current = cn_digit_value(other)?,
        }
    }
    Some(total + current)
}

/// 章号：先取阿拉伯数字，取不到再试中文数字。
fn first_chapter(raw: &str) -> Option<u32> {
    first_number(raw).or_else(|| cn_number(raw))
}

/// 结算条件写成表格时，取每行去掉序号的那几格，拼成一句话。
fn settle_from_table(lines: &[String]) -> Vec<String> {
    let (_, rows) = table_rows(lines);
    rows.into_iter()
        .filter_map(|row| {
            let parts: Vec<String> = row
                .into_iter()
                .filter(|cell| {
                    let t = cell.trim();
                    !t.is_empty()
                        && !is_placeholder(t)
                        && !t
                            .chars()
                            .all(|ch| ch.is_ascii_digit() || ch == '.' || ch == '、' || ch == ')')
                })
                .collect();
            if parts.is_empty() {
                None
            } else {
                Some(parts.join("，"))
            }
        })
        .collect()
}

/// 一条列表形态的未结项。两种写法都认：
/// `U-001 | 谁补的字 | 12 | 15 | 蓝色字迹、第二页`
/// `U-001 谁补的字（埋于 12，最迟 15，锚词：蓝色字迹、第二页）`
fn open_item_from_bullet(raw: &str) -> Option<OpenItem> {
    let cells = split_pipes(raw);
    if cells.len() >= 2 {
        let code = clean_code(&cells[0]);
        if is_placeholder(&code) || cells[1].trim().is_empty() {
            return None;
        }
        return Some(OpenItem {
            code,
            text: cells[1].trim().to_string(),
            planted: cells.get(2).and_then(|c| first_chapter(c)),
            due: cells.get(3).and_then(|c| first_chapter(c)),
            anchors: cells.get(4).map(|c| parse_code_list(c)).unwrap_or_default(),
        });
    }
    let (head, meta) = split_tail_parenthetical(raw);
    if head.trim().is_empty() {
        return None;
    }
    let (code, text) = split_code_prefix(&head);
    let meta = meta_fields(meta.as_deref().unwrap_or(""));
    Some(OpenItem {
        code,
        text: text.trim().to_string(),
        planted: meta_lookup(&meta, &["埋于", "起始", "出现", "首次"])
            .and_then(|v| first_chapter(&v)),
        due: meta_lookup(&meta, &["最迟", "期限", "截止", "到期", "结算"])
            .and_then(|v| first_chapter(&v)),
        anchors: meta_lookup(&meta, &["锚词", "锚点", "关键词", "字面"])
            .map(|v| parse_code_list(&v))
            .unwrap_or_default(),
    })
}

fn settled_from_row(row: &[String], cols: &LedgerCols) -> Option<SettledItem> {
    let code = cols
        .code
        .and_then(|i| row.get(i))
        .map(|c| clean_code(c))
        .unwrap_or_default();
    if code.is_empty() || is_placeholder(&code) {
        return None;
    }
    Some(SettledItem {
        code,
        at_chapter: cols
            .at
            .and_then(|i| row.get(i))
            .and_then(|c| first_chapter(c)),
        note: cols
            .note
            .and_then(|i| row.get(i))
            .map(|c| c.trim().to_string())
            .unwrap_or_default(),
    })
}

/// 账本的列表形态：`- U-001 | 7 | 当场对质` 或 `- U-001 第7章 当场对质`。
fn settled_from_bullet(raw: &str) -> Option<SettledItem> {
    let cells = split_pipes(raw);
    if cells.len() >= 2 {
        let code = clean_code(&cells[0]);
        if code.is_empty() || is_placeholder(&code) {
            return None;
        }
        return Some(SettledItem {
            code,
            at_chapter: first_chapter(&cells[1]),
            note: cells.get(2).map(|c| c.trim().to_string()).unwrap_or_default(),
        });
    }
    let (head, _) = split_tail_parenthetical(raw);
    let mut parts = head.split_whitespace();
    let code = clean_code(parts.next()?);
    if code.is_empty() || is_placeholder(&code) {
        return None;
    }
    let rest = parts.collect::<Vec<_>>().join(" ");
    Some(SettledItem {
        code,
        at_chapter: first_chapter(&rest),
        note: rest,
    })
}

/// 章型分工的列表形态：`- 12 | 起 | — | U-001` 或 `- 第12章 起（消耗 U-001）`。
fn roles_from_bullet(raw: &str, warnings: &mut Vec<String>) -> Vec<ChapterRole> {
    let cells = split_pipes(raw);
    if cells.len() >= 2 {
        let chapters = chapters_in(&cells[0]);
        if chapters.is_empty() {
            return Vec::new();
        }
        let label = cells[1].clone();
        let role = parse_role(&label).unwrap_or_else(|| {
            warnings.push(format!(
                "第 {} 章的章型 `{label}` 不认识，按「推」处理",
                chapters[0]
            ));
            EventRole::Develop
        });
        let consumes = cells.get(2).map(|c| parse_code_list(c)).unwrap_or_default();
        let plants = cells.get(3).map(|c| parse_code_list(c)).unwrap_or_default();
        return chapters
            .into_iter()
            .map(|chapter| ChapterRole {
                chapter,
                role,
                consumes: consumes.clone(),
                plants: plants.clone(),
            })
            .collect();
    }
    let (head, meta) = split_tail_parenthetical(raw);
    let mut parts = head.split_whitespace();
    let Some(first) = parts.next() else {
        return Vec::new();
    };
    let chapters = chapters_in(first);
    if chapters.is_empty() {
        return Vec::new();
    }
    let label = parts.collect::<Vec<_>>().join(" ");
    let role = parse_role(&label).unwrap_or_else(|| {
        warnings.push(format!(
            "第 {} 章的章型 `{label}` 不认识，按「推」处理",
            chapters[0]
        ));
        EventRole::Develop
    });
    let tail = meta.as_deref().unwrap_or("");
    let meta = meta_fields(tail);
    let consumes = meta_lookup(&meta, &["消耗", "收回", "回收"])
        .or_else(|| meta_after(tail, &["消耗", "收回", "回收"]))
        .map(|v| parse_code_list(&v))
        .unwrap_or_default();
    let plants = meta_lookup(&meta, &["新增", "新开", "埋"])
        .or_else(|| meta_after(tail, &["新增", "新开", "埋"]))
        .map(|v| parse_code_list(&v))
        .unwrap_or_default();
    chapters
        .into_iter()
        .map(|chapter| ChapterRole {
            chapter,
            role,
            consumes: consumes.clone(),
            plants: plants.clone(),
        })
        .collect()
}

/// `消耗 U-001` 这种不带冒号的写法：从关键词后面直接取到下一个分隔符。
fn meta_after(inner: &str, keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Some(pos) = inner.find(*key) {
            let rest = &inner[pos + key.len()..];
            let rest =
                rest.trim_start_matches(|c: char| c.is_whitespace() || c == '：' || c == ':');
            let end = rest
                .find(|c: char| matches!(c, '，' | ',' | '；' | ';' | '|' | '｜' | '）' | ')'))
                .unwrap_or(rest.len());
            let value = rest[..end].trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

fn cells_of(line: &str) -> Vec<String> {
    let converted = line.trim().replace('｜', "|");
    let t = converted.trim();
    if !t.starts_with('|') {
        return Vec::new();
    }
    let inner = t.trim_matches('|');
    if inner.trim().is_empty() {
        return Vec::new();
    }
    inner.split('|').map(|c| c.trim().to_string()).collect()
}

fn is_separator_cell(cell: &str) -> bool {
    !cell.is_empty() && cell.chars().all(|c| c == '-' || c == ':' || c.is_whitespace())
}

/// 表格 → (表头, 数据行)。分隔行跳过；没有表格时两端都为空。
fn table_rows(lines: &[String]) -> (Vec<String>, Vec<Vec<String>>) {
    let mut header: Vec<String> = Vec::new();
    let mut header_seen = false;
    let mut rows: Vec<Vec<String>> = Vec::new();
    for line in lines {
        let cells = cells_of(line);
        if cells.is_empty() {
            continue;
        }
        if !header_seen {
            header = cells;
            header_seen = true;
            continue;
        }
        if cells.iter().all(|c| is_separator_cell(c)) {
            continue;
        }
        rows.push(cells);
    }
    (header, rows)
}

/// 「基本信息」这一节：优先读两列表格，退化成 `键：值` 列表。
fn parse_pairs(lines: &[String]) -> Vec<(String, String)> {
    if lines.iter().any(|line| line.trim_start().starts_with('|')) {
        let (header, rows) = table_rows(lines);
        // 横排写法：表头是字段名，底下只有一行值——`| 所属卷 | 章号区间 | 状态 |` +
        // `| 1 | 2-5 | active |`。事件卡模板用的就是这种，不认的话整张卡的元信息全是空。
        if header.len() >= 3 && rows.len() == 1 && rows[0].len() == header.len() {
            return header.into_iter().zip(rows[0].clone()).collect();
        }
        return rows
            .into_iter()
            .filter_map(|row| {
                if row.len() < 2 {
                    return None;
                }
                Some((row[0].clone(), row[1].clone()))
            })
            .collect();
    }
    let mut out = Vec::new();
    for line in lines {
        let t = line
            .trim()
            .trim_start_matches('-')
            .trim_start_matches('*')
            .trim();
        if let Some((key, value)) = t.split_once('：').or_else(|| t.split_once(':')) {
            let key = key.trim();
            let value = value.trim();
            if !key.is_empty() && !value.is_empty() {
                out.push((key.to_string(), value.to_string()));
            }
        }
    }
    out
}

/// 去掉列表项前缀（`- `、`1. `、`1、`、`1) `），保留正文。
fn strip_bullet(line: &str) -> &str {
    let mut t = line.trim();
    for prefix in ["- ", "* ", "+ "] {
        if let Some(rest) = t.strip_prefix(prefix) {
            t = rest.trim();
            break;
        }
    }
    let digits: String = t.chars().take_while(|c| c.is_ascii_digit()).collect();
    if !digits.is_empty() {
        let rest = &t[digits.len()..];
        if let Some(r) = rest
            .strip_prefix('.')
            .or_else(|| rest.strip_prefix('、'))
            .or_else(|| rest.strip_prefix(')'))
            .or_else(|| rest.strip_prefix('）'))
        {
            t = r.trim();
        }
    }
    t
}

fn parse_list(lines: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for line in lines {
        let t = line.trim();
        if t.is_empty() || t.starts_with("```") || t.starts_with('|') {
            continue;
        }
        let t = strip_bullet(t);
        if !t.is_empty() {
            out.push(t.to_string());
        }
    }
    out
}

fn first_number(s: &str) -> Option<u32> {
    let mut digits = String::new();
    for ch in s.chars() {
        if ch.is_ascii_digit() {
            if digits.len() >= 6 {
                break;
            }
            digits.push(ch);
        } else if !digits.is_empty() {
            break;
        }
    }
    digits.parse::<u32>().ok()
}

/// 卷号：优先取阿拉伯数字，其次认中文数字（`卷一`、`第三卷`）。
fn parse_volume(raw: &str) -> Option<u32> {
    first_number(raw).or_else(|| cn_number(raw))
}

/// 章号区间：支持 `12—16`、`12-16`、`第12章—第16章`、`第十二章至第十六章`、`16`（单章）。
fn parse_span(raw: &str) -> Option<(u32, u32)> {
    let normalized: String = raw
        .chars()
        .map(|c| match c {
            '\u{2014}' | '\u{2013}' | '\u{FF5E}' | '~' | '至' | '到' => '-',
            other => other,
        })
        .collect();
    let numbers: Vec<u32> = normalized.split('-').filter_map(first_chapter).collect();
    match numbers.as_slice() {
        [] => None,
        [only] => Some((*only, *only)),
        [a, b, ..] => Some((*a, *b)),
    }
}

fn parse_status(raw: &str) -> EventStatus {
    let t = raw.trim().to_ascii_lowercase();
    if t.contains("settle")
        || t.contains("done")
        || t.contains("closed")
        || t.contains("已结算")
        || t.contains("结完")
        || t.contains("完结")
        || t.contains("结案")
        || t.contains("收束")
        || t.contains("关闭")
    {
        EventStatus::Settled
    } else if t.contains("active")
        || t.contains("进行")
        || t.contains("展开")
        || t.contains("在写")
        || t.contains("连载")
        || t.contains("写作中")
    {
        EventStatus::Active
    } else {
        EventStatus::Planned
    }
}

fn parse_role(raw: &str) -> Option<EventRole> {
    let t = raw.trim().to_ascii_lowercase();
    let exact = match t.as_str() {
        "起" | "setup" | "起势" | "开场" | "开端" => Some(EventRole::Setup),
        "推" | "develop" | "推进" | "中段" | "承接" => Some(EventRole::Develop),
        "转" | "turn" | "转折" | "反转" => Some(EventRole::Turn),
        "结" | "payoff" | "结算" | "收束" | "收尾" => Some(EventRole::Payoff),
        _ => None,
    };
    if exact.is_some() {
        return exact;
    }
    if t.contains("结算") || t.contains("收束") || t.contains("收尾") || t.contains("结") {
        Some(EventRole::Payoff)
    } else if t.contains("转") {
        Some(EventRole::Turn)
    } else if t.contains("起") || t.contains("开场") {
        Some(EventRole::Setup)
    } else if t.contains("推") || t.contains("承") {
        Some(EventRole::Develop)
    } else {
        None
    }
}

fn parse_prev(raw: &str) -> Option<String> {
    let t = raw.trim();
    let empty_shape = matches!(t, "" | "—" | "-" | "–" | "无" | "无前置" | "暂无" | "／" | "/");
    if empty_shape || t.eq_ignore_ascii_case("none") || t.eq_ignore_ascii_case("n/a") {
        return None;
    }
    Some(t.to_string())
}

/// 编号列：`U-001、U-002` / `U-001,U-002` / `—`。
fn parse_code_list(raw: &str) -> Vec<String> {
    raw.split(|c: char| {
        c == '、' || c == ',' || c == '，' || c == '/' || c == ';' || c == '；' || c.is_whitespace()
    })
    .map(|s| s.trim())
    .filter(|s| !s.is_empty() && *s != "—" && *s != "-" && *s != "–" && *s != "无")
    .map(|s| s.to_string())
    .collect()
}

fn is_placeholder(s: &str) -> bool {
    let t = s.trim();
    if matches!(t, "" | "—" | "-" | "–" | "无" | "编号" | "示例" | "模板" | "待填" | "…" | "...")
    {
        return true;
    }
    let lower = t.to_ascii_lowercase();
    matches!(lower.as_str(), "todo" | "tbd" | "xxx" | "example" | "id" | "？" | "?")
}

/// 解析一张事件卡。`rel_path` 只用于取事件号与回填 `path`，不读取磁盘。
pub fn parse_card(rel_path: &str, body: &str) -> Result<EventCard, String> {
    let path = rel_path.replace('\\', "/");
    let file_name = path
        .rsplit(|c| c == '/' || c == '\\')
        .next()
        .unwrap_or(&path)
        .to_string();
    let (heading, sections) = split_sections(body);

    let info = find_section(&sections, SEC_INFO)
        .map(|lines| parse_pairs(lines))
        .unwrap_or_default();
    let info_of = |keys: &[&str]| -> Option<String> {
        info.iter()
            .find(|(k, _)| {
                let n = normalise_title(k);
                keys.iter().any(|key| n == *key || n.contains(*key))
            })
            .map(|(_, v)| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };

    // 事件号三处可取：文件名、`# 事件 NNN` 标题、基本信息里的「事件号：」。
    // 手写卡多半先起名后补号，文件名不带数字是常态，所以后两处要兜底。
    let id = id_from_file_name(&file_name)
        .or_else(|| heading.as_deref().and_then(heading_id))
        .or_else(|| info_of(&["事件号", "事件编号"]).and_then(|v| normalise_id(&v)))
        .ok_or_else(|| {
            format!(
                "文件名 `{file_name}` 里没有事件号，应形如 `事件_001_标题.md`，\
                 或在「基本信息」里写 `事件号：001`"
            )
        })?;

    let title = heading
        .as_deref()
        .map(title_from_heading)
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| title_from_file_name(&file_name).unwrap_or_else(|| format!("事件 {id}")));

    let mut warnings: Vec<String> = Vec::new();

    let volume = info_of(&["所属卷", "卷号", "卷"]).and_then(|v| parse_volume(&v));

    let span = info_of(&["章号区间", "章节区间", "章范围", "区间", "章号"]).and_then(|raw| {
        match parse_span(&raw) {
            None => {
                warnings.push(format!("章号区间 `{raw}` 解析不出数字，按「未声明」处理"));
                None
            }
            Some((a, b)) if b < a => {
                warnings.push(format!(
                    "章号区间 `{raw}` 首尾颠倒（{a} > {b}），按「未声明」处理"
                ));
                None
            }
            Some(span) => Some(span),
        }
    });

    let status = info_of(&["状态"])
        .map(|v| parse_status(&v))
        .unwrap_or(EventStatus::Planned);
    let prev = info_of(&["前置事件", "前置", "承接事件", "上接"]).and_then(|v| parse_prev(&v));

    let settle: Vec<String> = match find_section(&sections, SEC_SETTLE) {
        Some(lines) => {
            let listed = parse_list(lines);
            if listed.is_empty() {
                // 结算条件写成表格（`| 条件 | 说明 |`）也认：取每行去掉序号后的文字。
                settle_from_table(lines)
            } else {
                listed
            }
        }
        None => Vec::new(),
    };
    if settle.is_empty() {
        warnings.push("没有「结算条件」：这个事件什么时候算写完，生成时无从判断".to_string());
    }

    let mut open_items: Vec<OpenItem> = Vec::new();
    match find_section(&sections, SEC_OPEN) {
        None => warnings.push("没有「未结项」一节：本事件没有可跨章传递的东西".to_string()),
        Some(lines) => {
            let (header, rows) = table_rows(lines);
            if !rows.is_empty() || !header.is_empty() {
                let cols = OpenCols::from_header(&header);
                if rows.is_empty() {
                    warnings.push("「未结项」只有表头没有行：这一节没挂住任何东西".to_string());
                }
                for row in &rows {
                    let code = cols
                        .code
                        .and_then(|i| row.get(i))
                        .map(|c| clean_code(c))
                        .unwrap_or_default();
                    if is_placeholder(&code) {
                        continue;
                    }
                    if code.is_empty() {
                        warnings.push("「未结项」有一行没有编号，已跳过".to_string());
                        continue;
                    }
                    let text = cols
                        .text
                        .and_then(|i| row.get(i))
                        .map(|c| c.trim().to_string())
                        .unwrap_or_default();
                    if text.is_empty() {
                        warnings.push(format!("未结项 `{code}` 没有内容，已跳过"));
                        continue;
                    }
                    open_items.push(OpenItem {
                        code,
                        text,
                        planted: cols
                            .planted
                            .and_then(|i| row.get(i))
                            .and_then(|c| first_chapter(c)),
                        due: cols
                            .due
                            .and_then(|i| row.get(i))
                            .and_then(|c| first_chapter(c)),
                        anchors: cols
                            .anchors
                            .and_then(|i| row.get(i))
                            .map(|c| parse_code_list(c))
                            .unwrap_or_default(),
                    });
                }
            } else {
                // 纯列表写法：`- U-001 谁补的字（埋于 12，最迟 15，锚词：蓝色字迹、第二页）`
                let mut auto = 0;
                for raw in bullet_lines(lines) {
                    let Some(mut item) = open_item_from_bullet(&raw) else {
                        continue;
                    };
                    if item.text.is_empty() {
                        continue;
                    }
                    if item.code.is_empty() {
                        item.code = auto_code(&open_items, auto + 1);
                        auto += 1;
                    }
                    if is_placeholder(&item.code) {
                        continue;
                    }
                    open_items.push(item);
                }
                if auto > 0 {
                    warnings.push(format!(
                        "有 {auto} 条未结项没写编号，已按出现顺序补成 U-001 起；账本按编号记账，建议写死编号"
                    ));
                }
            }
            for item in &open_items {
                if item.anchors.is_empty() {
                    warnings.push(format!("未结项 `{}` 没有锚词：写后连续性检查对它无效", item.code));
                }
                if item.due.is_none() {
                    warnings.push(format!("未结项 `{}` 没有最迟结算章：永远不会被判欠账", item.code));
                }
            }
        }
    }

    let mut ledger: Vec<SettledItem> = Vec::new();
    if let Some(lines) = find_section(&sections, SEC_LEDGER) {
        let (header, rows) = table_rows(lines);
        if !rows.is_empty() {
            let cols = LedgerCols::from_header(&header);
            for row in &rows {
                if let Some(item) = settled_from_row(row, &cols) {
                    ledger.push(item);
                }
            }
        } else if !header.is_empty() {
            warnings
                .push("「已结算账本」只有表头没有行：结过的项按 编号|结算于|方式 补进来".to_string());
        } else {
            for raw in bullet_lines(lines) {
                if let Some(item) = settled_from_bullet(&raw) {
                    ledger.push(item);
                }
            }
            if ledger.is_empty() && lines.iter().any(|line| !line.trim().is_empty()) {
                warnings.push(
                    "「已结算账本」既不是表格也不是列表，已忽略；应为 编号|结算于|方式".to_string(),
                );
            }
        }
    }

    let mut roles: Vec<ChapterRole> = Vec::new();
    match find_section(&sections, SEC_ROLES) {
        None => warnings
            .push("没有「章型分工」：事件内排章会退回一章一步，跨章展开不会生效".to_string()),
        Some(lines) => {
            let (header, rows) = table_rows(lines);
            if !rows.is_empty() {
                let cols = RoleCols::from_header(&header);
                for row in &rows {
                    let raw_chapters = cols
                        .chapter
                        .and_then(|i| row.get(i))
                        .cloned()
                        .unwrap_or_default();
                    let chapters = chapters_in(&raw_chapters);
                    if chapters.is_empty() {
                        continue;
                    }
                    let label = cols
                        .role
                        .and_then(|i| row.get(i))
                        .cloned()
                        .unwrap_or_default();
                    let role = parse_role(&label).unwrap_or_else(|| {
                        warnings.push(format!(
                            "第 {} 章的章型 `{label}` 不认识，按「推」处理",
                            chapters[0]
                        ));
                        EventRole::Develop
                    });
                    let consumes = cols
                        .consumes
                        .and_then(|i| row.get(i))
                        .map(|c| parse_code_list(c))
                        .unwrap_or_default();
                    let plants = cols
                        .plants
                        .and_then(|i| row.get(i))
                        .map(|c| parse_code_list(c))
                        .unwrap_or_default();
                    for chapter in chapters {
                        roles.push(ChapterRole {
                            chapter,
                            role,
                            consumes: consumes.clone(),
                            plants: plants.clone(),
                        });
                    }
                }
            } else if !header.is_empty() {
                warnings.push("「章型分工」只有表头没有行：事件内排章会退回一章一步".to_string());
            } else {
                // 列表写法：`- 12 | 起 | — | U-001` 或 `- 第12章 起`
                for raw in bullet_lines(lines) {
                    roles.extend(roles_from_bullet(&raw, &mut warnings));
                }
            }
            roles.sort_by_key(|r| r.chapter);
            // 同一章排两次只认第一条：手写补行很常见，重复不该把章型翻掉。
            let before = roles.len();
            roles.dedup_by_key(|r| r.chapter);
            if roles.len() < before {
                warnings.push(format!(
                    "「章型分工」有 {} 章重复排了，只用了每章第一条",
                    before - roles.len()
                ));
            }
            if roles.is_empty() {
                warnings.push("「章型分工」表里没有可用行：事件内排章会退回一章一步".to_string());
            }
        }
    }

    // 账本认了、清单没销：按已结算算，但要提示把清单划掉，避免同一件事被写两遍。
    for item in &open_items {
        if ledger.iter().any(|entry| entry.code == item.code) {
            warnings.push(format!(
                "未结项 `{}` 在已结算账本里也出现：按已结算处理，建议从清单里划掉",
                item.code
            ));
        }
    }

    Ok(EventCard {
        id,
        title,
        path,
        volume,
        span,
        prev,
        status,
        settle,
        open_items,
        ledger,
        roles,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str = "\
# 事件 012 · 码头交接

## 基本信息

| 字段 | 值 |
|---|---|
| 所属卷 | 卷一 |
| 章号区间 | 12—16 |
| 前置事件 | — |
| 状态 | 进行中 |

## 结算条件

1. 交接单上的名字被当着第三个人念出来
2. 林砚承认自己改过第二页

## 未结项

| 编号 | 内容 | 埋于 | 最迟结算章 | 锚词 |
|---|---|---|---|---|
| U-001 | 第二页的蓝色字迹是谁补的 | 12 | 15 | 蓝色字迹、第二页 |
| U-002 | 交接单背面为什么少一个章 | 13 | 16 | 背面、骑缝章 |
| — | — | — | — | — |

## 已结算账本

| 编号 | 结算于 | 方式 |
|---|---|---|
| U-000 | 11 | 前台登记簿对上时间 |

## 章型分工

| 章 | 章型 | 本章消耗 | 本章新增 |
|---|---|---|---|
| 12 | 起 | — | U-001 |
| 13 | 推 | U-001 | U-002 |
| 14 | 转 | U-002 | — |
| 16 | 结 | U-002 | — |
";

    #[test]
    fn parses_a_full_card() {
        let card = parse_card("大纲/事件/事件_012_码头交接.md", FULL).expect("应能解析");
        assert_eq!(card.id, "012");
        assert_eq!(card.title, "码头交接");
        assert_eq!(card.path, "大纲/事件/事件_012_码头交接.md");
        assert_eq!(card.volume, Some(1));
        assert_eq!(card.span, Some((12, 16)));
        assert_eq!(card.prev, None);
        assert_eq!(card.status, EventStatus::Active);
        assert_eq!(card.settle.len(), 2);
        assert_eq!(card.open_items.len(), 2, "占位行必须被跳过");
        let first = &card.open_items[0];
        assert_eq!(first.code, "U-001");
        assert_eq!(first.planted, Some(12));
        assert_eq!(first.due, Some(15));
        assert_eq!(
            first.anchors,
            vec!["蓝色字迹".to_string(), "第二页".to_string()]
        );
        assert_eq!(card.last_due(), Some(16));
        assert_eq!(card.effective_span(), Some((12, 16)));
        assert_eq!(card.role_of(14).map(|r| r.role), Some(EventRole::Turn));
        assert_eq!(
            card.role_of(13).map(|r| r.consumes.clone()),
            Some(vec!["U-001".to_string()])
        );
        assert!(card.is_settled("U-000"));
        assert!(!card.warnings.iter().any(|w| w.contains("章型分工")));
        assert!(!card.warnings.iter().any(|w| w.contains("未结项")));
    }

    #[test]
    fn open_and_overdue_follow_the_ledger() {
        let card = parse_card("事件_012_码头交接.md", FULL).unwrap();
        assert_eq!(card.open_at(13).len(), 2);
        assert_eq!(card.overdue_at(18).len(), 2);
        assert!(
            card.overdue_at(15).is_empty(),
            "第 15 章是最后期限，当天不算过期"
        );
    }

    #[test]
    fn tolerates_a_bare_card() {
        let card =
            parse_card("大纲/事件/事件_003_只有标题.md", "# 事件 003 · 只有标题\n").unwrap();
        assert_eq!(card.id, "003");
        assert_eq!(card.title, "只有标题");
        assert_eq!(card.span, None);
        assert_eq!(card.status, EventStatus::Planned);
        assert!(card.roles.is_empty());
        assert!(card.warnings.iter().any(|w| w.contains("未结项")));
        assert!(card.warnings.iter().any(|w| w.contains("章型分工")));
    }

    #[test]
    fn rejects_a_file_name_without_event_number() {
        let err = parse_card("大纲/事件/事件_码头.md", "# 码头\n").unwrap_err();
        assert!(err.contains("事件号"), "{err}");
    }

    #[test]
    fn normalises_event_ids_to_three_digits() {
        assert_eq!(id_from_file_name("事件_7_开场.md").as_deref(), Some("007"));
        assert_eq!(id_from_file_name("事件_0007_开场.md").as_deref(), Some("007"));
        assert_eq!(id_from_file_name("事件_012.md").as_deref(), Some("012"));
        assert_eq!(id_from_file_name("别的东西.md"), None);
    }

    #[test]
    fn parses_span_variants() {
        assert_eq!(parse_span("12—16"), Some((12, 16)));
        assert_eq!(parse_span("12-16"), Some((12, 16)));
        assert_eq!(parse_span("第12章—第16章"), Some((12, 16)));
        assert_eq!(parse_span("第12章至第16章"), Some((12, 16)));
        assert_eq!(parse_span("16"), Some((16, 16)));
        assert_eq!(parse_span("待定"), None);
    }

    #[test]
    fn reversed_span_becomes_none_with_a_warning() {
        let body = "\
# 事件 021 · 颠倒

## 基本信息

| 字段 | 值 |
|---|---|
| 章号区间 | 16—12 |
";
        let card = parse_card("事件_021_颠倒.md", body).unwrap();
        assert_eq!(card.span, None);
        assert!(card.warnings.iter().any(|w| w.contains("首尾颠倒")));
    }

    #[test]
    fn unknown_role_falls_back_to_develop() {
        let body = "\
# 事件 022 · 乱写章型

## 章型分工

| 章 | 章型 | 本章消耗 | 本章新增 |
|---|---|---|---|
| 20 | 乱写 | — | U-009 |
";
        let card = parse_card("事件_022_乱写章型.md", body).unwrap();
        assert_eq!(card.roles.len(), 1);
        assert_eq!(card.roles[0].role, EventRole::Develop);
        assert!(card.warnings.iter().any(|w| w.contains("不认识")));
        assert_eq!(card.roles[0].plants, vec!["U-009".to_string()]);
    }

    #[test]
    fn anchors_split_on_chinese_and_ascii_separators() {
        let body = "\
# 事件 023 · 锚词

## 未结项

| 编号 | 内容 | 埋于 | 最迟结算章 | 锚词 |
|---|---|---|---|---|
| U-001 | 收据上没有骑缝章 | 5 | 8 | 骑缝章、收据,第二页/背面 |
";
        let card = parse_card("事件_023_锚词.md", body).unwrap();
        assert_eq!(
            card.open_items[0].anchors,
            vec![
                "骑缝章".to_string(),
                "收据".to_string(),
                "第二页".to_string(),
                "背面".to_string()
            ]
        );
    }

    #[test]
    fn open_item_without_anchor_or_due_gets_a_warning() {
        let body = "\
# 事件 024 · 缺字段

## 未结项

| 编号 | 内容 | 埋于 | 最迟结算章 | 锚词 |
|---|---|---|---|---|
| U-001 | 谁签的字 | 5 | — | — |
";
        let card = parse_card("事件_024_缺字段.md", body).unwrap();
        assert_eq!(card.open_items.len(), 1);
        assert_eq!(card.open_items[0].due, None);
        assert!(card.warnings.iter().any(|w| w.contains("没有锚词")));
        assert!(card.warnings.iter().any(|w| w.contains("没有最迟结算章")));
    }

    #[test]
    fn ledger_wins_over_a_stale_open_row() {
        let body = "\
# 事件 025 · 重号

## 未结项

| 编号 | 内容 | 埋于 | 最迟结算章 | 锚词 |
|---|---|---|---|---|
| U-001 | 谁签的字 | 5 | 8 | 签字 |

## 已结算账本

| 编号 | 结算于 | 方式 |
|---|---|---|
| U-001 | 7 | 当场对质 |
";
        let card = parse_card("事件_025_重号.md", body).unwrap();
        assert!(card.is_settled("U-001"));
        assert!(card.open_at(9).is_empty());
        assert!(card.warnings.iter().any(|w| w.contains("也出现")));
    }

    #[test]
    fn parses_a_fully_hand_written_card() {
        let body = "# 事件 007 · 手写卡\n\n## 基础信息\n\n- 所属卷：卷二\n- 章号区间：第七至第十一章\n- 状态：进行中\n- 前置：事件 006\n\n## 完成条件\n\n1. 老周改口\n2. 账本对上\n\n## 未结事项\n\n- U-001 第三联复印件在谁手里（埋于 7，最迟 10，锚词：第三联、复印件）\n- U-002 谁在 11 章后还在提这单（埋于 9，最迟 11，锚词：这单）\n- 没有编号的那条（埋于 8，锚词：后门）\n\n## 结算账本\n\n- U-000 | 6 | 前台对表\n\n## 章节分工\n\n- 第7章 起\n- 8—9 推（消耗 U-001）\n- 10 转\n- 11 结\n";
        let card = parse_card("事件_007_手写卡.md", body).unwrap();
        assert_eq!(card.id, "007");
        assert_eq!(card.volume, Some(2));
        assert_eq!(card.span, Some((7, 11)));
        assert_eq!(card.status, EventStatus::Active);
        assert_eq!(card.prev.as_deref(), Some("事件 006"));
        assert_eq!(card.settle.len(), 2, "「完成条件」当结算条件认");
        assert_eq!(card.open_items.len(), 3);
        assert_eq!(card.open_items[0].code, "U-001");
        assert_eq!(card.open_items[0].due, Some(10));
        assert_eq!(
            card.open_items[0].anchors,
            vec!["第三联".to_string(), "复印件".to_string()]
        );
        assert_eq!(card.open_items[2].code, "U-003");
        assert!(card.warnings.iter().any(|w| w.contains("没写编号")));
        assert_eq!(card.ledger.len(), 1);
        assert_eq!(card.ledger[0].code, "U-000");
        assert_eq!(card.ledger[0].at_chapter, Some(6));
        assert_eq!(card.roles.len(), 5, "8—9 展开成两章");
        assert_eq!(card.role_of(7).map(|r| r.role), Some(EventRole::Setup));
        assert_eq!(
            card.role_of(9).map(|r| r.consumes.clone()),
            Some(vec!["U-001".to_string()])
        );
        assert_eq!(card.role_of(10).map(|r| r.role), Some(EventRole::Turn));
        assert_eq!(card.role_of(11).map(|r| r.role), Some(EventRole::Payoff));
        assert_eq!(card.effective_span(), Some((7, 11)));
    }

    #[test]
    fn column_order_is_read_from_the_header() {
        let body = "# 事件 008 · 换列\n\n## 未结项\n\n| 内容 | 编号 | 锚词 | 最迟结算章 | 埋于 |\n| --- | --- | --- | --- | --- |\n| 第三联不见了 | U-001 | 第三联、复印件 | 15 | 12 |\n";
        let card = parse_card("事件_008_换列.md", body).unwrap();
        let item = &card.open_items[0];
        assert_eq!(item.code, "U-001");
        assert_eq!(item.text, "第三联不见了");
        assert_eq!(item.planted, Some(12));
        assert_eq!(item.due, Some(15));
        assert_eq!(
            item.anchors,
            vec!["第三联".to_string(), "复印件".to_string()]
        );
    }

    #[test]
    fn accepts_full_width_pipes() {
        let body = "# 事件 009 · 全角\n\n## 未结项\n\n｜编号｜内容｜埋于｜最迟结算章｜锚词｜\n｜---｜---｜---｜---｜---｜\n｜U-001｜第三联｜12｜15｜第三联｜\n";
        let card = parse_card("事件_009_全角.md", body).unwrap();
        assert_eq!(card.open_items.len(), 1);
        assert_eq!(card.open_items[0].code, "U-001");
        assert_eq!(card.open_items[0].due, Some(15));
    }

    #[test]
    fn event_number_can_come_from_the_body() {
        let body = "# 码头交接\n\n## 基本信息\n\n- 事件号：012\n- 章号区间：12—13\n";
        let card = parse_card("大纲/事件/码头交接.md", body).unwrap();
        assert_eq!(card.id, "012");
        assert_eq!(card.title, "码头交接");
        assert_eq!(card.span, Some((12, 13)));
    }

    #[test]
    fn duplicate_chapter_rows_keep_the_first() {
        let body = "# 事件 010 · 重排\n\n## 章型分工\n\n| 章 | 章型 | 本章消耗 | 本章新增 |\n| --- | --- | --- | --- |\n| 12—13 | 推 | — | U-001 |\n| 13 | 转 | U-001 | — |\n";
        let card = parse_card("事件_010_重排.md", body).unwrap();
        assert_eq!(card.roles.len(), 2);
        assert_eq!(card.role_of(13).map(|r| r.role), Some(EventRole::Develop));
        assert!(card.warnings.iter().any(|w| w.contains("重复")));
    }
}
