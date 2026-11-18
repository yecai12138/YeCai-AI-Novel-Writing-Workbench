//! 全局上下文与硬约束抽取（阶段 10）。
//!
//! 阶段 8 之前，子智能体只能看到「自己目录里的文件 + 上一步的产出」：
//! 写手拿到的是 `正文/` 的清单和刚出生的细纲，**看不到** `设定/`、`追踪/`、`参考资料/`。
//! 结果是细纲里写着「2400–2600 字、对白四成、环境两成」，正文照样跑偏。
//!
//! 这个模块做两件事：
//! 1. [`global_context`]：按优先级挑出这一章真正该读的全局文件（细纲 → 全书大纲 →
//!    设定 → 追踪 → 参考资料），交给 `compose_agent_prompt` 写成「必须先阅读」清单；
//! 2. [`hard_constraints`]：把细纲里的字数区间与比例要求抽成一句话，塞进四行契约的
//!    `补充要求`，让「硬性指标」出现在 prompt 的最后一行，而不是埋在某个文件第 37 行。
//!
//! 两者都只做**存在即输出**：文件不在就不编，抽不到就不写。宁可少一条约束，也不给模型
//! 递一句假要求。

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use regex::Regex;

use crate::models::event::{EventCard, EventStatus, OpenItem};

/// 全局上下文最多列几个文件（再多就把 prompt 变成目录表）。
pub const GLOBAL_LIMIT: usize = 12;

/// 常驻的「核心设定」：存在就排在最前面，因为这几份几乎每章都要看。
const CORE_FILES: [&str; 6] = [
    "设定/世界概览.md",
    "设定/角色状态.md",
    "设定/人物.md",
    "设定/文风.md",
    "追踪/时间线.md",
    "追踪/伏笔.md",
];

/// 每个目录的上下文配额（总和 = [`GLOBAL_LIMIT`]）。
///
/// 以前是「所有候选全局排序后截取前 12 个」：`设定/` 里 9 个文件的字典序恰好都在
/// 「追踪」「参考资料」前面，写手拿到 12 个文件全是设定，伏笔表与资料一条都进不来，
/// 回收链条因此断在上下文这一层。现在按目录分账，每个目录自己排序、自己占自己的名额；
/// 某类文件不足时，空出来的名额再按「离正文越近越优先」的顺序补给别人。
const QUOTA_CHAPTER_OUTLINE: usize = 1;
/// 事件卡：主线 + 支线同时展开时最多两张，再多就只剩「事件」没有「正文」了。
const QUOTA_EVENT: usize = 2;
const QUOTA_CORE: usize = 4;
const QUOTA_TRACK: usize = 2;
const QUOTA_REF: usize = 2;
const QUOTA_OUTLINE: usize = 1;

/// 从 `list` 里取最多 `quota` 个还没进过 `out` 的路径（目录内顺序由调用方排好）。
fn take_quota(out: &mut Vec<String>, list: &[String], quota: usize, skip: &[String]) {
    let mut taken = 0usize;
    for rel in list {
        if taken >= quota || out.len() >= GLOBAL_LIMIT {
            return;
        }
        if skip.iter().any(|s| s == rel) || out.iter().any(|s| s == rel) {
            continue;
        }
        out.push(rel.clone());
        taken += 1;
    }
}

/// 配额没用满时，按 `pools` 的先后顺序把剩余名额补齐。
fn fill_rest(out: &mut Vec<String>, pools: &[&Vec<String>], skip: &[String]) {
    for pool in pools {
        for rel in pool.iter() {
            if out.len() >= GLOBAL_LIMIT {
                return;
            }
            if skip.iter().any(|s| s == rel) || out.iter().any(|s| s == rel) {
                continue;
            }
            out.push(rel.clone());
        }
    }
}

/// 按优先级收集这一章要读的全局文件（相对书根，正斜杠）。
///
/// 顺序本身是给模型看的暗示：越靠前越该先读。`skip` 里的路径不会重复出现
/// （调用方已经把「目标目录清单」和「上一步产出」塞进去了）。
pub fn global_context(book_root: &Path, chapter: u32, skip: &[String]) -> Vec<String> {
    global_context_of(book_root, Some(chapter), skip)
}

/// 与 [`global_context`] 相同，但章号可以是「没有」。
///
/// 补设定、追踪、资料这些步骤与具体章无关：以前它们被当成章节步骤，
/// 于是每轮都被告知去读 `大纲/细纲_第005章.md`（界面上的「带入 大纲\细纲\第005章.md」），
/// 设定管理员甚至因此去改了正文。`chapter = None` 时只给全书级别的文件。
pub fn global_context_of(book_root: &Path, chapter: Option<u32>, skip: &[String]) -> Vec<String> {
    // 1) 按目录分别收集候选，目录内部按文件名排序（= 用户自己写的顺序）。
    let mut chapter_outline: Vec<String> = Vec::new();
    if let Some(no) = chapter {
        if let Some(path) = outline_path(book_root, no) {
            if let Ok(rel) = path.strip_prefix(book_root) {
                chapter_outline.push(slash(rel));
            }
        }
    }
    // 事件卡：本章属于哪个事件，就把卡本身排进清单（模型读得到未结项与章型分工）。
    // 老书没有 `大纲/事件/`，这里就是空列表，配额让给别的目录。
    let event: Vec<String> = chapter
        .map(|no| {
            events_covering(book_root, no)
                .into_iter()
                .map(|card| card.path)
                .collect()
        })
        .unwrap_or_default();
    let core: Vec<String> = CORE_FILES
        .iter()
        .filter(|rel| book_root.join(rel).is_file())
        .map(|rel| rel.to_string())
        .collect();
    let mut settings: Vec<String> = Vec::new();
    walk(book_root, "设定", 0, &mut settings);
    settings.sort();
    let mut track: Vec<String> = Vec::new();
    walk(book_root, "追踪", 0, &mut track);
    track.sort();
    let mut reference: Vec<String> = Vec::new();
    walk(book_root, "参考资料", 0, &mut reference);
    reference.sort();
    let mut outline: Vec<String> = Vec::new();
    if book_root.join("大纲/大纲.md").is_file() {
        outline.push("大纲/大纲.md".to_string());
    }
    // 对标拆解报告：只收 对标/<书名>/拆解.md。抓来的章节原文**不进**全局清单——
    // 几万字原文会把写手的上下文淹没，写手要的是拆解结论，不是对标原文。
    let mut bench: Vec<String> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(book_root.join("对标")) {
        for entry in rd.flatten() {
            let dir = entry.path();
            if !dir.is_dir() {
                continue;
            }
            let report = dir.join("拆解.md");
            if report.is_file() {
                if let Ok(rel) = report.strip_prefix(book_root) {
                    bench.push(slash(rel));
                }
            }
        }
        bench.sort();
    }

    // 2) 分账取用：每类先吃自己的配额，谁也不许把别人挤光。
    let mut out: Vec<String> = Vec::new();
    take_quota(&mut out, &chapter_outline, QUOTA_CHAPTER_OUTLINE, skip);
    take_quota(&mut out, &event, QUOTA_EVENT, skip);
    take_quota(&mut out, &core, QUOTA_CORE, skip);
    take_quota(&mut out, &track, QUOTA_TRACK, skip);
    take_quota(&mut out, &reference, QUOTA_REF, skip);
    take_quota(&mut out, &outline, QUOTA_OUTLINE, skip);

    // 3) 还有空位就按「离正文越近越优先」补：设定余量 > 追踪余量 > 资料余量 > 对标 > 大纲余量。
    fill_rest(
        &mut out,
        &[
            &settings,
            &track,
            &reference,
            &bench,
            &outline,
            &chapter_outline,
        ],
        skip,
    );
    out
}

/// 这一章细纲里的硬性指标（字数区间 + 对白/环境比例），抽不到返回 `None`。
///
/// 只认「细纲里明写了的数字」，不猜。拿到的是能直接塞进 `补充要求` 的一小句。
pub fn hard_constraints(book_root: &Path, chapter: u32) -> Option<String> {
    hard_constraints_with(book_root, chapter, true)
}

/// 与 [`hard_constraints`] 相同，但**不包含字数**。
///
/// 用户已经规定了正文字数时，细纲里的「正文目标字数」和单场「字数建议」都不再是约束：
/// 真实事故是用户要求 9000-9500 字，应用却把细纲第一个场景的 2200-2600 字追加到
/// 用户要求后面，模型自然按低区间收尾。比例与风格仍然要保留。
pub fn hard_constraints_without_word_count(book_root: &Path, chapter: u32) -> Option<String> {
    hard_constraints_with(book_root, chapter, false)
}

fn hard_constraints_with(
    book_root: &Path,
    chapter: u32,
    include_word_count: bool,
) -> Option<String> {
    let path = outline_path(book_root, chapter)?;
    // read_text：剥 BOM，「字数额」这类正则不会因为文件带 BOM 而认不到第一行。
    let text = crate::services::atomic_write::read_text(&path).ok()?;
    let mut parts: Vec<String> = Vec::new();

    if include_word_count {
        if let Some(clause) = word_count_clause(&text) {
            parts.push(clause);
        }
    }
    if let Some(clause) = ratio_clause(&text, "对白", "对白占") {
        parts.push(clause);
    }
    if let Some(clause) = ratio_clause(&text, "环境", "环境描写") {
        parts.push(clause);
    }
    if parts.is_empty() {
        return None;
    }
    Some(format!("{}。", parts.join("，")))
}

/// 可选的细纲索引：`大纲/细纲索引.md`，一行一章 → 相对书根的路径。
///
/// 合并形态的书（一卷一个文件、或把 20 章细纲写在 `大纲/卷2_第021-040章.md` 里）
/// 没有 `细纲_第NNN章.md` 可找，只能靠这份索引指路；文件不存在就一路降级到扫描。
pub const OUTLINE_INDEX_REL: &str = "大纲/细纲索引.md";

/// 扫描兜底时，该章在文件里那一段至少要这么多字（去空白）才算细纲。
///
/// 全书大纲里「第N章：一句话」通常几十字，400 字能挡住它；真细纲都在千字以上。
const OUTLINE_SEGMENT_MIN_CHARS: usize = 400;

/// 这一章的细纲文件。
///
/// 三级定位，越靠前越可信：
/// 1. `大纲/细纲索引.md` 里这一章指向的路径（合并文件只能靠它）；
/// 2. `大纲/细纲_第003章.md` / `大纲/细纲_第3章.md` 两条老候选名；
/// 3. `大纲/` 下逐个 `.md` 扫章标题，该章段落够长就算命中。
pub fn outline_path(book_root: &Path, chapter: u32) -> Option<PathBuf> {
    if let Some(path) = outline_index_entry(book_root, chapter) {
        return Some(path);
    }
    let padded = format!("{chapter:03}");
    let candidates = [
        book_root.join("大纲").join(format!("细纲_第{padded}章.md")),
        book_root
            .join("大纲")
            .join(format!("细纲_第{chapter}章.md")),
    ];
    if let Some(path) = candidates.into_iter().find(|p| p.is_file()) {
        return Some(path);
    }
    scan_outline_by_heading(book_root, chapter)
}

/// 读 `大纲/细纲索引.md`，取出这一章指向的文件（必须落在书内且真实存在）。
///
/// 认三种写法：`第8章 | 大纲/卷2_第005-012章.md`、`第8章 → 路径`、`8: 路径`。
/// 索引行写错（路径不存在、越出书根）就跳过这一行继续往后找，不让一条坏索引
/// 把整本书判成「缺细纲」。
fn outline_index_entry(book_root: &Path, chapter: u32) -> Option<PathBuf> {
    let text = std::fs::read_to_string(book_root.join(OUTLINE_INDEX_REL)).ok()?;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with("//") {
            continue;
        }
        let Some(pos) = line.find(|c: char| matches!(c, '|' | '→' | ':' | '：' | '\t')) else {
            continue;
        };
        let key = line[..pos].trim();
        let target = line[pos..]
            .trim_start_matches(|c: char| matches!(c, '|' | '→' | ':' | '：' | '\t' | ' '))
            .trim();
        let key_no = chapter_in_name(key).or_else(|| key.parse::<u32>().ok());
        if key_no != Some(chapter) {
            continue;
        }
        let Some(rel) = normalize_rel(target) else {
            continue;
        };
        let path = book_root.join(&rel);
        if path.is_file() {
            return Some(path);
        }
    }
    None
}

/// 索引里的路径清洗：去掉引号/反引号，统一分隔符，拒绝绝对路径与 `..`。
fn normalize_rel(raw: &str) -> Option<String> {
    let cleaned = raw.trim().trim_matches(|c: char| {
        matches!(
            c,
            '`' | '"' | '\'' | '「' | '」' | '『' | '』' | '(' | ')' | '（' | '）'
        )
    });
    if cleaned.is_empty() {
        return None;
    }
    let unified = cleaned.replace('\\', "/");
    let unified = unified.trim_start_matches("./").to_string();
    // 以前这里只挡开头的 / 和 ".."，于是盘符（C:/x）与 UNC 能过；
    // 而 book_root.join(绝对路径) 会替换掉基准，索引里写一行绝对路径就能
    // 让上下文层去读工作区外的文件。口径统一到 path_guard。
    if !crate::services::path_guard::rel_is_safe(&unified) {
        return None;
    }
    Some(unified)
}

/// 兜底扫描：`大纲/` 下按 `.md` 文件名排序逐份看，哪份里「第 N 章」这一段够长就算命中。
fn scan_outline_by_heading(book_root: &Path, chapter: u32) -> Option<PathBuf> {
    let dir = book_root.join("大纲");
    let entries = std::fs::read_dir(&dir).ok()?;
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|e| e.to_str()) == Some("md"))
        .collect();
    paths.sort();
    for path in paths {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        if chapter_segment_chars(&text, chapter) >= OUTLINE_SEGMENT_MIN_CHARS {
            return Some(path);
        }
    }
    None
}

/// 文件里「第 N 章」那一段的字数（去空白）。
///
/// 段落边界是下一个 `# 第M章` 标题（M ≠ N）或文件结束；没有这一章的标题返回 0。
fn chapter_segment_chars(text: &str, chapter: u32) -> usize {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"^\s{0,3}#{1,6}\s*第\s*([0-9]{1,4})\s*章").expect("细纲章标题正则")
    });
    let mut inside = false;
    let mut count = 0usize;
    for line in text.lines() {
        if let Some(caps) = re.captures(line) {
            let no = caps.get(1).and_then(|m| m.as_str().parse::<u32>().ok());
            if no == Some(chapter) {
                inside = true;
                continue;
            }
            if inside {
                break;
            }
            continue;
        }
        if inside {
            count += line.chars().filter(|c| !c.is_whitespace()).count();
        }
    }
    count
}

// ---------------- 事件层（S2）：把「事件的进度」变成跨章上下文 ----------------
//
// 在此之前，跨章信息只有「上一章」：细纲要点 + 上章正文开头/结尾 + 最近六条摘要。
// 事件的寿命因此被压到一章——模型看不到「这件事还没办完」，只能每天开一件新事。
// 这里补的是同一件事的**账**：事件卡里登记了未结项、最迟结算章、锚词与章型分工，
// 写第 N 章时把这些念给它听，事件才可能跨章延续。

/// 事件卡目录（相对书根）。
///
/// 老书没有这个目录：所有取料点都必须能对着空目录正常工作，不能把「没有事件层」
/// 当成错误——S1 之前写出来的书就是这个形状。
pub const EVENT_DIR_REL: &str = "大纲/事件";

/// 读全部事件卡。解析失败的卡直接跳过：写正文时不该被一张坏卡挡住。
fn read_events(book_root: &Path) -> Vec<EventCard> {
    let dir = book_root.join(EVENT_DIR_REL);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut cards: Vec<EventCard> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .map(|ext| ext.eq_ignore_ascii_case("md"))
                .unwrap_or(false)
        })
        .filter_map(|path| {
            let body = crate::services::atomic_write::read_text(&path).ok()?;
            let rel = path.strip_prefix(book_root).ok()?;
            crate::models::event::parse_card(&slash(rel), &body).ok()
        })
        .collect();
    cards.sort_by(|a, b| a.id.cmp(&b.id));
    cards
}

fn span_start(card: &EventCard) -> u32 {
    card.effective_span()
        .map(|(from, _)| from)
        .unwrap_or(u32::MAX)
}

/// 覆盖本章的事件卡：章型分工里排了本章，或章号区间盖住本章。
///
/// 主线 + 支线同时命中是正常的；排序即优先级：未结算的在前，其次区间起点、事件号。
pub fn events_covering(book_root: &Path, chapter: u32) -> Vec<EventCard> {
    let cards = read_events(book_root);
    covering(&cards, chapter)
        .into_iter()
        .cloned()
        .collect()
}

/// 这张卡管不管第 `chapter` 章：章型分工里排过它，或者区间盖住它。
fn covering<'a>(cards: &'a [EventCard], chapter: u32) -> Vec<&'a EventCard> {
    let mut hits: Vec<&EventCard> = cards
        .iter()
        .filter(|card| {
            let listed = card.role_of(chapter).is_some();
            let covered = card
                .effective_span()
                .map(|(from, to)| chapter >= from && chapter <= to)
                .unwrap_or(false);
            listed || covered
        })
        .collect();
    hits.sort_by(|a, b| {
        let settled = |card: &EventCard| matches!(card.status, EventStatus::Settled);
        settled(a)
            .cmp(&settled(b))
            .then_with(|| span_start(a).cmp(&span_start(b)))
            .then_with(|| a.id.cmp(&b.id))
    });
    hits
}

/// 这本书有没有事件卡：决定排细纲前要不要先引导排一次事件。
/// 按事件号取一张卡（编号宽松：`12` / `012` / `事件_012` / `事件 012 · 标题` 都行）。
/// 这本书的全部事件卡（审计用；写作路径按章取用走 [`events_covering`]）。
pub fn all_event_cards(book_root: &Path) -> Vec<EventCard> {
    read_events(book_root)
}

pub fn find_event_card(book_root: &Path, id: &str) -> Option<EventCard> {
    let wanted = crate::models::event::normalise_id(id)?;
    read_events(book_root)
        .into_iter()
        .find(|card| card.id == wanted)
}

/// 细纲首段声明的「所属事件：事件 012 · 码头交接」。
///
/// 写「无」、写了空、或者干脆没这一行，都返回 `None`——老细纲没有这个字段，
/// 不能让它们因为缺一行就写不了正文。
pub fn declared_event(book_root: &Path, chapter: u32) -> Option<String> {
    let path = outline_path(book_root, chapter)?;
    let body = std::fs::read_to_string(path).ok()?;
    declared_event_in(&body)
}

/// [`declared_event`] 的纯文本版本（便于测试，不碰盘）。
pub fn declared_event_in(body: &str) -> Option<String> {
    // ① 固定标记「所属事件：NNN」是主写法。写「无」是作者明确表态，
    //    这时不再拿别处的双链去补，否则细纲说「不挂事件」会被正文的引用推回事件层。
    if let Some(line) = body.lines().find(|l| l.contains("所属事件")) {
        let after = line.split(['：', ':']).nth(1).unwrap_or("").trim();
        if !after.is_empty() {
            if after.starts_with('无') {
                return None;
            }
            if let Some(id) = crate::models::event::normalise_id(after) {
                return Some(id);
            }
        }
    }
    // ② 双链 `[[事件:001]]`：交叉引用写法，允许带标题，但必须带事件号。
    wiki_event_link(body)
}

/// 取正文/细纲里第一个 `[[事件:001]]` 的事件号。
///
/// 只认第一种命中：一段文字里挂了好几个链接时，第一个通常就是这一章的归属。
fn wiki_event_link(body: &str) -> Option<String> {
    wiki_event_links(body).into_iter().next()
}

/// 取正文/细纲里所有 `[[事件:xxx]]` 的事件号（保序去重）。
///
/// 只解析，不判定：谁的章、归属谁，仍由「所属事件」那一行说了算（见 [`declared_event_in`]）。
/// 审计层拿它做「正文提到了哪些事件」的记录，正文引用不会倒过来改归属。
pub fn wiki_event_links(body: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for chunk in body.split("[[").skip(1) {
        let Some(end) = chunk.find("]]") else { continue };
        let inner = chunk[..end].trim();
        let inner = inner.strip_prefix("事件").unwrap_or(inner);
        let inner = inner.trim_start_matches(|c: char| c == '：' || c == ':' || c.is_whitespace());
        if let Some(id) = crate::models::event::normalise_id(inner) {
            if !out.iter().any(|seen| seen == &id) {
                out.push(id);
            }
        }
    }
    out
}

pub fn has_event_cards(book_root: &Path) -> bool {
    !read_events(book_root).is_empty()
}

/// 细纲里到底有没有写「所属事件」这一行。
///
/// 写「无」也算写过：作者明确说过这章不挂事件，那就别拿「被哪张卡盖住」去补。
/// 只有整行缺失（老细纲全都没补）时，才允许按覆盖关系兜底。
pub fn outline_mentions_event(book_root: &Path, chapter: u32) -> bool {
    let Some(path) = outline_path(book_root, chapter) else {
        return false;
    };
    std::fs::read_to_string(path)
        .map(|body| body.lines().any(|line| line.contains("所属事件")))
        .unwrap_or(false)
}

/// 一段章号各自属于哪个事件（取排序后的第一张，没有就是 `None`）。
///
/// 只读一次盘：批量拆步按事件切块时，不要每章读一遍目录。
pub fn event_ids_by_chapter(book_root: &Path, from: u32, to: u32) -> Vec<Option<String>> {
    let cards = read_events(book_root);
    (from..=to)
        .map(|chapter| {
            covering(&cards, chapter)
                .first()
                .map(|card| card.id.clone())
        })
        .collect()
}

/// 事件进度：写第 N 章时，这个事件走到哪、还剩什么没办完、本章该干什么。
///
/// 只输出卡里明写的东西，不推断：
/// 1. 位置（事件号、标题、第几章 / 共几章、章型）；
/// 2. 未结项（编号 + 内容 + 最迟结算章 + 锚词），逾期的单独拎出来；
/// 3. 本章该消耗 / 该新增 + 事件结算条件；
/// 4. 上游事件（`prev`）还没结掉的事——跨事件承接。
///
/// 一章同时盖着多张卡时最多注入 [`QUOTA_EVENT`] 张：以前只取排序第一张，
/// 第二张起的事件约束整段消失，写手照着第一张写完才发现接不上第二张（方案七的注入漏洞）。
///
/// 没有事件卡、本章也不属于任何事件时返回 `None`，调用方不注入这一段。
/// 细纲点了名但卡缺失/没盖住本章时，返回一句说明——注入层不解决，至少要让人知道。
///
/// 只输出卡里明写的东西，不推断：
pub fn event_digest(book_root: &Path, chapter: u32) -> Option<String> {
    let live: Vec<EventCard> = events_covering(book_root, chapter)
        .into_iter()
        .filter(|card| card.status != EventStatus::Settled)
        .collect();

    // 卡都不在、或没盖住本章，但细纲点了名：门禁会拦（或放行提示），写手侧也必须知道，
    // 否则他照着普通章节写完，事件线就断在这里。
    if live.is_empty() {
        let declared = declared_event(book_root, chapter)?;
        return Some(match find_event_card(book_root, &declared) {
            None => format!(
                "【事件内约束】细纲写着「所属事件」{declared}，但 大纲/事件/ 下没有这张卡：本章按单独一章写，别自己发明事件编号。"
            ),
            Some(_) => format!(
                "【事件内约束】细纲把本章挂在事件 {declared} 上，但那张卡的「章号区间」与「章型分工」都没盖住第 {chapter} 章：先把卡改到涵盖本章，或改细纲的所属事件。"
            ),
        });
    }

    let mut blocks: Vec<String> = Vec::new();
    for card in live.iter().take(QUOTA_EVENT) {
        blocks.extend(event_block(book_root, card, chapter));
    }
    if live.len() > QUOTA_EVENT {
        let rest = live
            .iter()
            .skip(QUOTA_EVENT)
            .map(|card| format!("{} · {}", card.id, card.title))
            .collect::<Vec<_>>()
            .join("、");
        blocks.push(format!(
            "（本章还盖着 {} 张事件卡没注入：{rest}。先按上面写，这几张本章别加章。）",
            live.len() - QUOTA_EVENT
        ));
    }
    Some(blocks.join("\n"))
}

/// 单张事件卡的注入正文。老 [`event_digest`] 的那一段原样搬进来——多卡只是把它们拼起来。
fn event_block(book_root: &Path, card: &EventCard, chapter: u32) -> Vec<String> {
    let (start, end) = card.effective_span().unwrap_or((chapter, chapter));
    let total = end.saturating_sub(start) + 1;
    let pos = chapter.saturating_sub(start) + 1;

    let mut lines: Vec<String> = Vec::new();
    let mut head = format!(
        "【事件内约束】事件 {} · {}（第 {pos}/{total} 章",
        card.id, card.title
    );
    if let Some(role) = card.role_of(chapter) {
        head.push_str(&format!("，章型：{}", role.role.label()));
    }
    head.push('）');
    lines.push(head);

    let open = card.open_at(chapter);
    if open.is_empty() {
        lines.push(
            "本事件未结项：无。本章要么把事件收掉，要么登记新开的事（不许凭空多出一条线）。"
                .to_string(),
        );
    } else {
        lines.push(format!("本事件未结项（{} 项）：", open.len()));
        for item in open.iter().take(8) {
            let mut row = format!("- {}", item.code);
            if !item.text.trim().is_empty() {
                row.push(' ');
                row.push_str(item.text.trim());
            }
            if let Some(due) = item.due {
                if due < chapter {
                    row.push_str(&format!("（最迟第 {due} 章结算，已逾期）"));
                } else {
                    row.push_str(&format!("（最迟第 {due} 章结算）"));
                }
            }
            if !item.anchors.is_empty() {
                row.push_str(&format!("〔锚词：{}〕", item.anchors.join("、")));
            }
            lines.push(row);
        }
        if open.len() > 8 {
            lines.push(format!(
                "- …（还有 {} 项未列出，动笔前自己翻一遍事件卡）",
                open.len() - 8
            ));
        }
    }

    let overdue = card.overdue_at(chapter);
    if !overdue.is_empty() {
        let codes = overdue
            .iter()
            .map(|item| item.code.as_str())
            .collect::<Vec<_>>()
            .join("、");
        lines.push(format!(
            "⚠ 已过最迟结算章：{codes}。本章要么结算，要么把最迟结算章往后改并写明理由——不许当作没看见。"
        ));
    }

    match card.role_of(chapter) {
        Some(role) => {
            let consumes = if role.consumes.is_empty() {
                "（无指定）".to_string()
            } else {
                role.consumes.join("、")
            };
            let plants = if role.plants.is_empty() {
                "（无新增）".to_string()
            } else {
                role.plants.join("、")
            };
            lines.push(format!(
                "本章该做的：{}；消耗：{consumes}；新增：{plants}",
                role.role.label()
            ));
        }
        None => lines.push(
            "本章还没在「章型分工」里排章：先给本章定一个章型（起/推/转/结）再动笔，否则事件会退回一章一步。"
                .to_string(),
        ),
    }

    lines.extend(upstream_leftovers(book_root, card, chapter));

    if !card.settle.is_empty() {
        lines.push(format!(
            "事件结算条件（全部满足才算结）：{}",
            card.settle.join("；")
        ));
    }

    lines
}

/// 上游事件还没结掉的事：本事件是它的后续，这些线要接住，不能凭空断掉。
///
/// `prev` 指向前置事件，但注入里以前从不提它——写手只看见自己这一张卡，
/// 上一起事件留下的未结项就成了「写着写着没了」。
fn upstream_leftovers(book_root: &Path, card: &EventCard, chapter: u32) -> Vec<String> {
    let Some(prev) = card.prev.as_deref() else {
        return Vec::new();
    };
    if prev == card.id {
        return Vec::new();
    }
    let Some(upstream) = find_event_card(book_root, prev) else {
        return vec![format!(
            "↳ 前置事件 {prev} 在 大纲/事件/ 下找不到对应卡：本事件的前史无处可查，动笔前先确认事件号。"
        )];
    };
    let mine: Vec<&str> = card
        .open_items
        .iter()
        .map(|item| item.code.as_str())
        .collect();
    let left: Vec<&OpenItem> = upstream
        .open_at(chapter)
        .into_iter()
        .filter(|item| !mine.iter().any(|code| *code == item.code))
        .collect();
    if left.is_empty() {
        return Vec::new();
    }
    let mut lines = vec![format!(
        "↳ 上游事件 {} · {} 还剩 {} 项没结（跨事件承接，别当没发生）：",
        upstream.id,
        upstream.title,
        left.len()
    )];
    for item in left.iter().take(4) {
        let mut row = format!("- {}", item.code);
        if !item.text.trim().is_empty() {
            row.push(' ');
            row.push_str(item.text.trim());
        }
        if !item.anchors.is_empty() {
            row.push_str(&format!("〔锚词：{}〕", item.anchors.join("、")));
        }
        lines.push(row);
    }
    if left.len() > 4 {
        lines.push(format!(
            "- …（还有 {} 项，见 {}）",
            left.len() - 4,
            upstream.path
        ));
    }
    lines
}

/// 前情提要：写第 N 章时给执行智能体的「上一章记忆」。
///
/// 背景（阶段 13，用户提出的「记忆功能」）：模型没有跨章记忆，写到后面就忘了前面
/// 写了什么。逐字塞前几章全文会把 prompt 撑爆，这里取三层，存在才输出：
/// 1. 上一章**细纲**的「本章目标/结尾钩子」段（短，模型自己写的规划）；
/// 2. 上一章**正文的开头** ~300 字（人物与场景怎么接的）；
/// 3. 上一章**正文的结尾** ~800 字（情节停在哪、悬念是什么）。
///
/// 最前面还有一段**事件进度**（[`event_digest`]）：这一章属于哪个事件、还剩哪些未结项、
/// 本章该消耗/该新增什么。它不依赖上一章——第 1 章照样有，所以放在最前先算。
///
/// 返回 `None` 表示既没有上一章可讲、也没有事件约束（老书、或第 1 章且无事件卡），
/// 调用方不注入这一段。
pub fn prev_chapter_digest(book_root: &Path, chapter: u32) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();

    // 事件进度排在最前：模型先知道「这件事走到哪、还剩什么」，再去接上一章的字面落点。
    // 第 1 章没有上一章，但同样有事件约束，所以这一段先算。
    if let Some(event) = event_digest(book_root, chapter) {
        parts.push(event);
    }
    if chapter <= 1 {
        return if parts.is_empty() {
            None
        } else {
            Some(parts.join("\n"))
        };
    }
    let prev = chapter - 1;

    if let Some(path) = outline_path(book_root, prev) {
        if let Ok(text) = crate::services::atomic_write::read_text(&path) {
            // 细纲通常很短，直接全要不行——取前 500 字（目标/冲突/钩子都在开头）。
            let head: String = text
                .chars()
                .filter(|c| !c.is_whitespace())
                .take(500)
                .collect();
            if !head.is_empty() {
                parts.push(format!("上一章（第 {prev} 章）细纲要点：{head}"));
            }
        }
    }

    // 滚动摘要索引：上一章开头/结尾只盖得住一章，跨章的人物状态与伏笔
    // 靠 记忆/摘要索引.md 的最近几条兜住（写手每章完稿自行更新，见 intent.rs 摘要规则）。
    if let Ok(index) = std::fs::read_to_string(book_root.join("记忆").join("摘要索引.md")) {
        // 文件里的行序不等于章节顺序：补写旧章节时（先写 9、10、11，再补 8）旧版把新行
        // 追在末尾，直接取「最后六行」会得到 9、10、11、8、12、13 这种乱跳的序列，
        // 模型据此接戏必然错位。这里按章号重排后再取最近六章，位置信息完全不参与。
        let mut rows: Vec<(u32, &str)> = Vec::new();
        for line in index.lines().map(str::trim) {
            // 只认「第N章」开头的章节行；表头、手写备注、空行一律不进上下文。
            let Some(ch) = super::memory::row_chapter(line) else {
                continue;
            };
            match rows.iter_mut().find(|(row_ch, _)| *row_ch == ch) {
                // 同一章重复行：后出现的为准，避免把同一章六次塞满名额。
                Some(slot) => slot.1 = line,
                None => rows.push((ch, line)),
            }
        }
        rows.sort_by_key(|(ch, _)| *ch);
        let take = rows.len().min(6);
        if take > 0 {
            let text = rows[rows.len() - take..]
                .iter()
                .map(|(_, line)| *line)
                .collect::<Vec<_>>()
                .join("\n");
            parts.push(format!(
                "近期章节滚动摘要（按章号排序，越靠后越新）：\n{text}"
            ));
        }
    }

    let Some(prose) = chapter_prose_path(book_root, prev) else {
        return if parts.is_empty() {
            None
        } else {
            Some(parts.join("\n"))
        };
    };
    let Ok(text) = std::fs::read_to_string(&prose) else {
        return if parts.is_empty() {
            None
        } else {
            Some(parts.join("\n"))
        };
    };
    if text.trim().is_empty() {
        return if parts.is_empty() {
            None
        } else {
            Some(parts.join("\n"))
        };
    }
    let chars: Vec<char> = text.chars().filter(|c| !c.is_whitespace()).collect();
    let head: String = chars.iter().take(300).collect();
    let tail: String = if chars.len() > 300 {
        chars[chars.len().saturating_sub(800)..].iter().collect()
    } else {
        String::new()
    };
    if !head.is_empty() {
        parts.push(format!("上一章正文开头：{head}"));
    }
    if !tail.is_empty() {
        parts.push(format!("上一章正文结尾（本章要从这里接着写）：{tail}"));
    }
    if parts.is_empty() {
        return None;
    }
    Some(parts.join("\n"))
}

/// 从文件名/相对路径里抽章号：`第004章_血路.md` / `细纲_第12章.md` → 4 / 12。
///
/// 必须按解析出的数字比较，不能用 `starts_with("第2章")`——那会把「第20章」
/// 误当成第 2 章，写第 2 章时把第 20 章也塞进上下文或算进字数。
pub fn chapter_in_name(name: &str) -> Option<u32> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"第\s*([0-9]{1,4})\s*章").expect("文件名章号正则"));
    let caps = re.captures(name)?;
    caps.get(1)?.as_str().parse::<u32>().ok().filter(|n| *n > 0)
}

/// 一个名字里出现的所有章号：`第308章_名片.md` → [308]，`卷2_第021-040章.md` → [21, 40]。
///
/// 区间写法必须单独认：`第021-040章` 里 `021` 后面跟的是 `-`，
/// 单章正则匹配不到，只能靠区间正则取两端（缺了它，合并卷文件会被当成没章号）。
fn chapter_numbers_in_name(name: &str) -> Vec<u32> {
    static SINGLE: OnceLock<Regex> = OnceLock::new();
    static RANGE: OnceLock<Regex> = OnceLock::new();
    let single = SINGLE.get_or_init(|| Regex::new(r"第\s*([0-9]{1,4})\s*章").expect("章号正则"));
    let range = RANGE.get_or_init(|| {
        Regex::new(r"第\s*([0-9]{1,4})\s*[-~—–至到]\s*([0-9]{1,4})\s*章").expect("章区间正则")
    });
    let mut out: Vec<u32> = Vec::new();
    for caps in range.captures_iter(name) {
        for idx in [1usize, 2] {
            if let Some(n) = caps
                .get(idx)
                .and_then(|m| m.as_str().parse::<u32>().ok())
                .filter(|n| *n > 0)
            {
                out.push(n);
            }
        }
    }
    for caps in single.captures_iter(name) {
        if let Some(n) = caps
            .get(1)
            .and_then(|m| m.as_str().parse::<u32>().ok())
            .filter(|n| *n > 0)
        {
            out.push(n);
        }
    }
    out
}

/// 书名/大纲里汉字数字转阿拉伯数字（只处理 1—99，够用：卷数、章数很少更大）。
fn cn_number(text: &str) -> Option<u32> {
    fn digit(ch: char) -> Option<u32> {
        Some(match ch {
            '零' | '〇' => 0,
            '一' => 1,
            '二' | '两' => 2,
            '三' => 3,
            '四' => 4,
            '五' => 5,
            '六' => 6,
            '七' => 7,
            '八' => 8,
            '九' => 9,
            _ => return None,
        })
    }
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    if let Ok(n) = text.parse::<u32>() {
        return Some(n);
    }
    // 十 / 十七 / 二十 / 二十一
    if let Some(rest) = text.strip_prefix('十') {
        return Some(10 + rest.chars().next().and_then(digit).unwrap_or(0));
    }
    let mut chars = text.chars();
    let head = chars.next().and_then(digit)?;
    let tail: String = chars.collect();
    if tail.is_empty() {
        return Some(head);
    }
    if let Some(after) = tail.strip_prefix('十') {
        let rest = after.chars().next().and_then(digit).unwrap_or(0);
        return Some(head * 10 + rest);
    }
    None
}

/// 一本书的规模：大纲里**声明**的篇幅，与盘上**实际**写到的章。
///
/// 为什么要分开记：这本书的大纲写着「共七卷、300 章」（`大纲/大纲.md` 卷结构一节），
/// 而 `大纲/` 里已经排到 `细纲_第350章.md`、正文停在 `第308章_一张名片.md`。
/// 三个数字互不相认，收尾就永远说不清「全书结尾到底在第几章」——
/// 谁是权威、差多少，必须先算出来给作者看，再决定收尾钉在哪。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BookScope {
    /// 大纲里声明「共 N 章」。
    pub declared_total: Option<u32>,
    /// 大纲里声明「共 N 卷」。
    pub declared_volumes: Option<u32>,
    /// 大纲卷结构里每一卷的末章（`卷一 第1—40章` → 40 …）。
    pub volume_ends: Vec<u32>,
    /// `大纲/` 文件名与索引里出现的最远章号。
    pub outline_end: Option<u32>,
    /// `正文/` 文件名里出现的最远章号。
    pub prose_end: Option<u32>,
    /// `大纲/` 下细纲文件数量。
    pub outline_count: usize,
    /// `正文/` 下正文文件数量。
    pub prose_count: usize,
}

fn max_chapter_in_dir(dir: &Path) -> (Option<u32>, usize) {
    let mut best: Option<u32> = None;
    let mut count = 0usize;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (None, 0);
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        count += 1;
        for chapter in chapter_numbers_in_name(name) {
            if best.is_none_or(|current| chapter > current) {
                best = Some(chapter);
            }
        }
    }
    (best, count)
}

/// 算出这本书「大纲声明多少、实际写到多少」。
pub fn book_scope(book_root: &Path) -> BookScope {
    let mut scope = BookScope::default();
    if let Ok(text) = std::fs::read_to_string(book_root.join("大纲/大纲.md")) {
        static TOTAL: OnceLock<Regex> = OnceLock::new();
        static VOLUME: OnceLock<Regex> = OnceLock::new();
        let total = TOTAL.get_or_init(|| {
            Regex::new(r"共\s*([0-9一二三四五六七八九十百]+)\s*卷[、,，和\s]*([0-9]{1,4})\s*章")
                .expect("卷章总数正则")
        });
        if let Some(caps) = total.captures(&text) {
            scope.declared_volumes = caps.get(1).and_then(|m| cn_number(m.as_str()));
            scope.declared_total = caps
                .get(2)
                .and_then(|m| m.as_str().parse::<u32>().ok());
        }
        let volume = VOLUME.get_or_init(|| {
            Regex::new(r"卷[一二三四五六七八九十]+\s*第\s*([0-9]{1,4})\s*[-—~至到]\s*([0-9]{1,4})\s*章")
                .expect("卷区间正则")
        });
        for caps in volume.captures_iter(&text) {
            if let Some(end) = caps.get(2).and_then(|m| m.as_str().parse::<u32>().ok()) {
                scope.volume_ends.push(end);
            }
        }
        scope.volume_ends.sort_unstable();
        scope.volume_ends.dedup();
    }
    let (outline_end, outline_count) = max_chapter_in_dir(&book_root.join("大纲"));
    scope.outline_end = outline_end;
    scope.outline_count = outline_count;
    if let Ok(text) = std::fs::read_to_string(book_root.join(OUTLINE_INDEX_REL)) {
        for line in text.lines() {
            for chapter in chapter_numbers_in_name(line) {
                if scope.outline_end.is_none_or(|current| chapter > current) {
                    scope.outline_end = Some(chapter);
                }
            }
        }
    }
    let (prose_end, prose_count) = max_chapter_in_dir(&book_root.join("正文"));
    scope.prose_end = prose_end;
    scope.prose_count = prose_count;
    scope
}

/// 把「声明」与「实际」的差距写成给作者看的一句话（没有差距返回空）。
///
/// 只陈述事实与依据，不改任何文件：一致性校验不该悄悄改作者的大纲。
pub fn scope_warnings(scope: &BookScope) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if let Some(declared) = scope.declared_total {
        if let Some(end) = scope.outline_end.filter(|end| *end != declared) {
            if end > declared {
                out.push(format!(
                    "大纲/大纲.md 声明全书 {declared} 章，但细纲已经排到第 {end} 章（多出 {} 章）",
                    end - declared
                ));
            } else {
                out.push(format!(
                    "大纲/大纲.md 声明全书 {declared} 章，但细纲只排到第 {end} 章（差 {} 章）",
                    declared - end
                ));
            }
        }
        if let Some(end) = scope.prose_end.filter(|end| *end < declared) {
            out.push(format!(
                "正文写到第 {end} 章，比大纲声明的 {declared} 章少 {} 章",
                declared - end
            ));
        }
    }
    if let (Some(outline), Some(prose)) = (scope.outline_end, scope.prose_end) {
        if outline > prose {
            out.push(format!(
                "正文落后细纲 {} 章：细纲已到第 {outline} 章，正文到第 {prose} 章",
                outline - prose
            ));
        }
    }
    if let (Some(last), Some(declared)) = (scope.volume_ends.last().copied(), scope.declared_total) {
        if last != declared {
            out.push(format!(
                "大纲/大纲.md 的卷结构最后一卷收在第 {last} 章，与标题声明的 {declared} 章不一致"
            ));
        }
    }
    out
}

/// 全书结尾章号的推断结果：章号 + 依据（计划卡上用它说明这个数字从哪来）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookEndHint {
    pub chapter: u32,
    /// 相对书根的路径，例如 `大纲/细纲_第350章.md`。
    pub source: String,
}

/// 从书目录里**已有的文件**推断「这本书应该在哪一章结束」。
///
/// 为什么需要它：收尾请求里不一定写了章号（用户的原话就是「收尾小说」）。
/// 这时编排层要么把「本份计划覆盖到的最远章」当全书结尾——可计划常常只是从中间截的一段，
/// 于是第 335 章就被写成「全书最后一章」；要么根本不知道该往哪收。
/// 按已有证据取最远的章号，才是这本书实际写到的位置：
/// - `正文/第NNN章_*.md` 里最靠后的章；
/// - `大纲/` 下所有 `.md` 文件名里的章号（含 `细纲_第NNN章.md` 与合并卷 `卷2_第021-040章.md`）；
/// - `大纲/细纲索引.md` 每行的章号键。
///
/// 三者取最大值。空目录返回 `None`，调用方退回「计划覆盖到的最远章」。
pub fn book_end_hint(book_root: &Path) -> Option<BookEndHint> {
    fn consider(best: &mut Option<(u32, String)>, chapter: u32, source: &str) {
        if chapter == 0 {
            return;
        }
        match best {
            Some((current, _)) if *current >= chapter => {}
            _ => *best = Some((chapter, source.to_string())),
        }
    }

    let mut best: Option<(u32, String)> = None;
    for dir in ["正文", "大纲"] {
        let Ok(entries) = std::fs::read_dir(book_root.join(dir)) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            for chapter in chapter_numbers_in_name(name) {
                consider(&mut best, chapter, &format!("{dir}/{name}"));
            }
        }
    }
    // 合并形态的书：章号只写在索引里（`第350章 | 大纲/卷七_第341-350章.md`）。
    if let Ok(text) = std::fs::read_to_string(book_root.join(OUTLINE_INDEX_REL)) {
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let key = line
                .split(|c: char| matches!(c, '|' | '→' | ':' | '：' | '\t'))
                .next()
                .unwrap_or("");
            for chapter in chapter_numbers_in_name(key) {
                consider(&mut best, chapter, OUTLINE_INDEX_REL);
            }
        }
    }
    // 大纲里声明的总章数也算证据：细纲还没写到结尾章、但大纲已经声明了全书篇幅时，
    // 「结尾在第 N 章」这件事写在 `大纲/大纲.md` 里，不该因为盘上暂时没有文件就丢掉。
    // 取最大值，所以声明比实际小（300 章 vs 已排到 350 章）时不会把结尾往回拉。
    if let Some(declared) = book_scope(book_root).declared_total {
        consider(&mut best, declared, "大纲/大纲.md（声明的总章数）");
    }
    best.map(|(chapter, source)| BookEndHint { chapter, source })
}

/// 第 N 章正文的路径（`正文/第NNN章_*.md`，兼容不带前导零的写法）。
pub fn chapter_prose_path(book_root: &Path, chapter: u32) -> Option<PathBuf> {
    let dir = book_root.join("正文");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return None;
    };
    let mut found: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            let Some(name) = p.file_name().and_then(|n| n.to_str()) else {
                return false;
            };
            chapter_in_name(name) == Some(chapter)
        })
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("md"))
        .collect();
    found.sort();
    found.into_iter().next()
}

/// 第 N 章正文的「纯字数」：与 [`word_count_clause`] 的口径一致——
/// 去掉 Markdown 标题行、引用行、分隔线，其余行只数**文字与数字**，标点与空白不计。
/// 编排层拿它对账用户的字数硬要求：不足就自动派续写步骤。
///
/// 真实事故：旧口径把标点也算进去，应用显示 9251 字、模型自查「去标点 8902 字」，
/// 两边对不上，模型按自己的口径继续写、应用按自己的口径验收，永远谈不拢。
pub fn chapter_word_count(book_root: &Path, chapter: u32) -> Option<usize> {
    let text = std::fs::read_to_string(chapter_prose_path(book_root, chapter)?).ok()?;
    Some(prose_char_count(&text))
}

/// 正文纯字数：去掉 Markdown 标题行、引用行、分隔线，其余行只数**文字与数字**，
/// 标点与空白不计。`gate` 的体检与编排层验收必须共用这一个口径。
pub fn prose_char_count(text: &str) -> usize {
    let mut total = 0usize;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#')
            || trimmed.starts_with('>')
            || trimmed.starts_with("```")
            || trimmed.starts_with("---")
            || trimmed.starts_with("***")
        {
            continue;
        }
        total += trimmed.chars().filter(|c| c.is_alphanumeric()).count();
    }
    total
}

/// 现有细纲能不能复用。
///
/// 阶段 11 之前只判「文件在不在」，于是换了题材也照样沿用上一本书的细纲：
/// 作者说「写一本古代的农民起事的小说」，系统翻出旧的「雨夜来客」细纲，
/// 写出 672 字节的旧章名正文——计划卡是对的，落盘结果完全不相干。
///
/// 现在多一层题材一致性：拿题材词与细纲正文比，命中率只影响 `reason` 里的说法；
/// 一个都不命中记警告，不再判作废（见 [`check_outline`]）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutlineCheck {
    /// 文件在不在。
    pub exists: bool,
    /// 能不能复用（不存在时为 false）。
    pub reusable: bool,
    /// 给人看的一句话：为什么复用 / 为什么作废。
    pub reason: String,
}

impl OutlineCheck {
    fn missing() -> Self {
        Self {
            exists: false,
            reusable: false,
            reason: "还没有细纲".to_string(),
        }
    }
}

/// 全书章号区间：取 大纲/大纲.md 里所有卷章数区间的并集（最小起点、最大终点）。
/// 「写七个卷每个章节的细纲」这类需求靠它换算出全书范围。
pub fn total_chapter_range(book_root: &Path) -> Option<(u32, u32)> {
    let text = std::fs::read_to_string(book_root.join("大纲").join("大纲.md")).ok()?;
    crate::services::intent::total_chapter_range(&text)
}

/// 读 `大纲/大纲.md`，把第 `volume` 卷换算成章号区间（「写第一卷正文」→ 第1—30章）。
///
/// 大纲没写卷章区间时返回 `None`——调用方据此走「补卷章节规划」，而不是缩水成只写一章。
pub fn volume_chapter_range(book_root: &Path, volume: u32) -> Option<(u32, u32)> {
    let text = std::fs::read_to_string(book_root.join("大纲").join("大纲.md")).ok()?;
    super::intent::parse_volume_range(&text, volume)
}

/// 检查第 N 章细纲能不能复用（路径按 [`outline_path`] 三级定位）。
///
/// - 没有 `genre`（用户没说题材）：只要文件在就复用——不能因为判不出题材就把旧稿全废掉；
/// - 有 `genre`：细纲正文里命中题材词就复用；**一个都没命中只记警告、照样复用**
///   （R1 之前这里判「作废重写」，合并细纲会被误判成缺细纲，进而多跑一轮覆盖已有稿）。
pub fn check_outline(book_root: &Path, chapter: u32, genre: Option<&str>) -> OutlineCheck {
    let Some(path) = outline_path(book_root, chapter) else {
        return OutlineCheck::missing();
    };
    // 读不出来 ≠ 空的。以前这里 unwrap_or_default()：文件被别的程序占着、或存成了 GBK，
    // 都会变成空串 → 判「细纲是空的」→ master 里 !reusable 会直接 force_architect 重写一份，
    // 把作者已经写好的细纲顶掉。读失败一律按「沿用、不动」返回（最安全的默认），
    // 只把原因回报给前端；真的 0 字节文件仍然按空细纲处理。
    let text = match crate::services::atomic_write::read_text(&path) {
        Ok(text) => text,
        Err(e) => {
            return OutlineCheck {
                exists: true,
                reusable: true,
                reason: format!("细纲文件读不出来（{e}），本次按沿用处理，不自动重写"),
            };
        }
    };
    if text.trim().is_empty() {
        return OutlineCheck {
            exists: true,
            reusable: false,
            reason: "细纲是空的".to_string(),
        };
    }
    let Some(genre) = genre.map(str::trim).filter(|g| !g.is_empty()) else {
        return OutlineCheck {
            exists: true,
            reusable: true,
            reason: "沿用现有细纲（这次没说题材）".to_string(),
        };
    };

    // 阶段 11.6：题材只跟「细纲自己的文字」比。之前把 设定/题材定位.md 也拼进
    // 比较文本，等于让细纲借设定文件的题材词——另一本书的旧细纲照样被判「可沿用」，
    // 恰好是阶段 11.5 要修的「旧细纲被错误沿用」。
    let hits: Vec<String> = topic_tokens(genre)
        .into_iter()
        .filter(|token| text.contains(token.as_str()))
        .collect();
    if hits.is_empty() {
        // 阶段 R1：题材零命中不再直接判「作废重写」。
        // 合并细纲（一卷一份）里某一章的段落本来就可能一个题材词都不出现，
        // 判死的代价是多跑一轮补细纲、把用户已有的稿子覆盖掉；
        // 而误留用的代价只是正文可能跑偏，作者在验收时看得见。降级为警告。
        return OutlineCheck {
            exists: true,
            reusable: true,
            reason: format!("沿用现有细纲（警告：题材「{genre}」零命中，未判作废）"),
        };
    }
    OutlineCheck {
        exists: true,
        reusable: true,
        reason: format!("沿用现有细纲（题材命中：{}）", hits.join("、")),
    }
}

/// 设定够不够写正文。
///
/// 实测场景：`设定/世界观/`、`设定/势力/` 是空目录，只有三份共 700 字的小文件，
/// 系统照样直接开写——用户拿到的正文跟设定毫无关系。这里先量一下，太薄就先补。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingHealth {
    /// `设定/` 下的 .md 数量。
    pub files: usize,
    /// 这些文件的总字数（按字符数算）。
    pub chars: usize,
    /// 核心设定是否缺失（世界观 / 人物 / 世界概览 一个都没有）。
    pub core_missing: bool,
    /// 结论：设定太薄，建议先补再写。
    pub thin: bool,
    /// 一句话说明（直接进计划卡与日志）。
    pub note: String,
}

/// 设定文件名里出现这些词，就算「有核心设定」。
///
/// 以前只认 4 个写死的文件名（世界概览 / 世界观 / 人物 / 人物小传）。实测一本
/// `设定/` 下有 `全书梗概.md` 19468 字 + `题材定位.md` + `文风.md` + `术语表.md` 的书，
/// 永远被判「缺核心 → 设定偏薄」，于是每一轮都在计划头强插一步【补设定】：
/// 白烧一次 Token，模型还会在那一步去改正文（截图 1-2 的「补设定」步就是这么来的）。
const CORE_SETTING_WORDS: [&str; 12] = [
    "世界观",
    "世界概览",
    "世界",
    "人物",
    "角色",
    "主角",
    "势力",
    "梗概",
    "题材",
    "文风",
    "术语",
    "设定",
];

/// 这一份设定文件算不算「核心」。
fn is_core_setting_file(rel: &str) -> bool {
    let name = rel.replace('\\', "/");
    let name = name.rsplit('/').next().unwrap_or(&name);
    CORE_SETTING_WORDS.iter().any(|word| name.contains(word))
}

/// 量一量 `设定/` 的分量。
pub fn setting_health(book_root: &Path) -> SettingHealth {
    let mut files = 0usize;
    let mut chars = 0usize;
    let mut found: Vec<String> = Vec::new();
    walk(book_root, "设定", 0, &mut found);
    for rel in &found {
        files += 1;
        if let Ok(text) = std::fs::read_to_string(book_root.join(rel)) {
            chars += text.chars().filter(|c| !c.is_whitespace()).count();
        }
    }
    // 核心设定 = 任何一份名字对得上的设定文件（见 [`CORE_SETTING_WORDS`]）。
    // 阶段 11.6：把「角色状态.md」从核心名单里拿掉——那是逐章更新的状态账本，不算人物小传。
    let core_missing = !found.iter().any(|rel| is_core_setting_file(rel));
    // 「薄」的门槛：缺核心文件，或核心文件写出来还是空壳（全部设定不足 20 字）。
    // 阶段 11.5 的 400 字门槛实测收不了尾（补设定写两份三百来字，下一轮仍然判薄，
    // 又插一步补设定）；核心文件在就该放行。
    let thin = core_missing || chars < 20;
    let note = if files == 0 {
        "设定/ 还是空的".to_string()
    } else if thin {
        format!(
            "设定偏薄：{files} 个文件共 {chars} 字{}",
            if core_missing {
                "，缺世界观/人物"
            } else {
                ""
            }
        )
    } else {
        format!("设定 {files} 个文件共 {chars} 字")
    };
    SettingHealth {
        files,
        chars,
        core_missing,
        thin,
        note,
    }
}

/// 从题材里切出可比较的词：先按非汉字切开，再取两字组合。
///
/// 「古代的农民起事」→ `古代 / 代的 / 的农 / 农民 / 民起 / 起事`。
/// 不追求分词准确，只要求「细纲里一个都不出现」能判出来——漏判比误判危险。
fn topic_tokens(genre: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for chunk in genre.split(|c: char| !c.is_alphanumeric() && c != '·') {
        let chars: Vec<char> = chunk.chars().collect();
        if chars.len() < 2 {
            continue;
        }
        for pair in chars.windows(2) {
            let token: String = pair.iter().collect();
            if !out.contains(&token) {
                out.push(token);
            }
        }
    }
    out
}

/// 细纲定的全章字数口径（数字形态）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WordSpec {
    /// 细纲写死了区间：`正文目标字数：6200 字（区间 6000—6500）`。
    Range(u32, u32),
    /// 细纲只给了一个目标值：`正文目标字数：6200 字`。
    Single(u32),
}

/// 从细纲里抽「全章字数」——优先看带总字数标签的行，单场「字数建议」一律不算。
///
/// 真实事故：细纲先写「场景一 字数建议 2200—2600」，末尾才写「正文目标字数 8000—9500」。
/// 旧实现取全文第一个区间，于是把单场建议当成全章目标，写手写出 2600 字就收尾。
///
/// 回归事故（2026-11-10）：细纲一直写着 `正文目标字数：6200 字（区间 6000—6500）`，
/// 但编排层只用户点名字数时才验收，写手交出 5389 字照样「执行完成」——
/// 所以这份口径除了拼 prompt，还要给验收层当阈值（[`outline_word_range`]）。
fn word_spec(text: &str) -> Option<WordSpec> {
    static RANGE: OnceLock<Regex> = OnceLock::new();
    static BAND: OnceLock<Regex> = OnceLock::new();
    static SINGLE: OnceLock<Regex> = OnceLock::new();
    let range = RANGE.get_or_init(|| {
        Regex::new(r"(\d{3,5})\s*[-–—~～至到]\s*(\d{3,5})\s*字").expect("字数区间正则编译失败")
    });
    // 真实细纲还有这种写法：`正文目标字数：6200 字（全章总字数；区间 6000—6500）`——
    // 区间后面不带「字」，只用「区间/范围」引出。漏认它就会把 6200 当成单点目标。
    let band = BAND.get_or_init(|| {
        Regex::new(
            r"(?:区间|范围|字数区间|字数范围)\s*[:：]?\s*(\d{3,5})\s*[-–—~～至到]\s*(\d{3,5})",
        )
        .expect("字数区间正则编译失败")
    });
    let single = SINGLE
        .get_or_init(|| Regex::new(r"字数[^0-9]{0,10}(\d{3,5})\s*字").expect("字数正则编译失败"));
    const TOTAL_LABELS: [&str; 6] = [
        "正文目标字数",
        "本章字数",
        "全章字数",
        "本章目标字数",
        "总字数",
        "字数目标",
    ];

    let parse_band = |caps: regex::Captures<'_>| -> Option<WordSpec> {
        let lo: u32 = caps.get(1)?.as_str().parse().ok()?;
        let hi: u32 = caps.get(2)?.as_str().parse().ok()?;
        let (lo, hi) = if hi >= lo { (lo, hi) } else { (hi, lo) };
        if lo == 0 {
            return None;
        }
        Some(WordSpec::Range(lo, hi))
    };

    let parse_line = |line: &str| -> Option<WordSpec> {
        if let Some(caps) = band.captures(line) {
            if let Some(spec) = parse_band(caps) {
                return Some(spec);
            }
        }
        if let Some(caps) = range.captures(line) {
            if let Some(spec) = parse_band(caps) {
                return Some(spec);
            }
        }
        let caps = single.captures(line)?;
        let n: u32 = caps.get(1)?.as_str().parse().ok()?;
        if n == 0 {
            return None;
        }
        Some(WordSpec::Single(n))
    };

    // 第一优先：明确的总字数标签。即使同一行后面跟着场景比例，也以总字数为准。
    // 但单场行即使碰巧写了「字数目标」也不能算全章——先排除场景级建议。
    for line in text.lines() {
        let line = line.trim();
        if is_scene_level(line) {
            continue;
        }
        if TOTAL_LABELS.iter().any(|label| line.contains(label)) {
            if let Some(spec) = parse_line(line) {
                return Some(spec);
            }
        }
    }
    // 第二优先：任意「字数」行，但排除场景/分幕级的建议。
    for line in text.lines() {
        let line = line.trim();
        if is_scene_level(line) {
            continue;
        }
        if line.contains("字数") {
            if let Some(spec) = parse_line(line) {
                return Some(spec);
            }
        }
    }
    // 没有全章口径就返回 None：宁可少一条约束，也不能把单场建议冒充全章目标。
    None
}

/// 从细纲里抽「全章字数」的一句话（塞进 `补充要求`）。
fn word_count_clause(text: &str) -> Option<String> {
    match word_spec(text)? {
        WordSpec::Range(lo, hi) => {
            Some(format!("本章字数必须在 {lo}-{hi} 字之间（不计标点与空白）"))
        }
        WordSpec::Single(n) => Some(format!("本章字数约 {n} 字（不计标点与空白）")),
    }
}

/// 这一章细纲规定的正文纯字数区间。
///
/// 编排层拿它做验收阈值：用户没点名字数时，细纲的「正文目标字数」就是本轮的达标线
/// （旧行为是根本不验收，细纲写 6200、正文 5389 也报「执行完成」）。
pub fn outline_word_range(book_root: &Path, chapter: u32) -> Option<(u32, u32)> {
    let path = outline_path(book_root, chapter)?;
    let text = std::fs::read_to_string(path).ok()?;
    word_range_of(&text)
}

/// [`outline_word_range`] 的纯文本版本：只给目标值时按 ±5% 折算成区间。
pub(crate) fn word_range_of(text: &str) -> Option<(u32, u32)> {
    match word_spec(text)? {
        WordSpec::Range(lo, hi) => Some((lo, hi)),
        WordSpec::Single(n) => {
            let pad = (n / 20).max(50);
            Some((n.saturating_sub(pad), n + pad))
        }
    }
}

/// 这一行是不是「单场/单幕」级别的建议。
///
/// 注意不能见到「场景」就跳过：像「三段场景按 3:4:3 分配；对白占四成」这种
/// 整章节奏行也带「场景」二字，它恰恰是我们要的总口径。
fn is_scene_level(line: &str) -> bool {
    [
        "字数建议",
        "单场",
        "每幕",
        "分场",
        "场景一",
        "场景二",
        "场景三",
        "场景四",
    ]
    .iter()
    .any(|marker| line.contains(marker))
}

/// 「对白占四成」「环境描写不超过两成」这类比例要求。
///
/// `keyword` 用来在原文里定位（对白 / 环境），`label` 是写进 prompt 的说法。
/// 先找「全章」口径，再找不带「场景」的整行——旧实现取第一个「对白占」，
/// 经常拿到场景一的 35%，而不是全章的 45%—50%。
fn ratio_clause(text: &str, keyword: &str, label: &str) -> Option<String> {
    let value_after = |line: &str| -> Option<String> {
        let idx = line.find(keyword)?;
        let rest = &line[idx + keyword.len()..];
        let end = rest
            .find(['。', '；', '，', ',', '\n'])
            .unwrap_or(rest.len());
        let mut clause = rest[..end].trim();
        clause = clause.trim_start_matches(['*', '：', ':', ' ']);
        let clause = clause.strip_prefix("比例").unwrap_or(clause);
        let clause = clause.trim_start_matches(['*', '：', ':', ' ']);
        if clause.is_empty() || clause.chars().count() > 40 {
            return None;
        }
        Some(clause.to_string())
    };
    let with_label = |value: String| -> String {
        // 「对白占四成」里的 value 已带「占」，避免拼成「对白占占四成」；
        // 「环境描写不超过两成」里的 value 已带「描写」，同理。
        let prefix = if value.starts_with('占') {
            label.trim_end_matches('占')
        } else if value.starts_with("描写") && label.ends_with("描写") {
            label.trim_end_matches("描写")
        } else {
            label
        };
        format!("{prefix}{value}")
    };

    // 1) 全章口径优先。
    for line in text.lines() {
        if line.contains("全章") && line.contains(keyword) {
            if let Some(value) = value_after(line) {
                return Some(with_label(value));
            }
        }
    }
    // 2) 不带「场景」的整行。
    for line in text.lines() {
        if is_scene_level(line) {
            continue;
        }
        if line.contains(keyword) {
            if let Some(value) = value_after(line) {
                return Some(with_label(value));
            }
        }
    }
    None
}

fn walk(book_root: &Path, dir: &str, depth: usize, out: &mut Vec<String>) {
    if depth > 1 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(book_root.join(dir)) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        if path.is_dir() {
            walk(book_root, &format!("{dir}/{name}"), depth + 1, out);
        } else if name.ends_with(".md") {
            if let Ok(rel) = path.strip_prefix(book_root) {
                out.push(slash(rel));
            }
        }
    }
}

fn slash(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// 事件卡：一张覆盖 2—5 章的事件，未结项带最迟结算章与锚词。
    fn event_book(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("storydesk-ctx-event-{tag}"));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("大纲/事件")).unwrap();
        fs::create_dir_all(root.join("正文")).unwrap();
        fs::write(
            root.join("大纲/事件/事件_002_码头交接.md"),
            "# 事件 002 · 码头交接\n\n## 基本信息\n\n| 项 | 值 |\n| --- | --- |\n| 所属卷 | 1 |\n| 章号区间 | 2-5 |\n| 前置事件 | — |\n| 状态 | 进行 |\n\n## 结算条件\n\n1. 仓单原件入库\n2. 老周改口\n\n## 未结项\n\n| 编号 | 内容 | 埋于 | 最迟结算章 | 锚词 |\n| --- | --- | --- | --- | --- |\n| U-000 | 旧账（已经在账本里结过） | 2 | 9 | 旧账 |\n| U-001 | 仓单第三联不在 | 2 | 3 | 第三联、复印件 |\n| U-002 | 老周的手机换了号 | 2 | 9 | 手机号 |\n\n## 已结算账本\n\n| 编号 | 结算于 | 方式 |\n| --- | --- | --- |\n| U-000 | 2 | 交代过了 |\n\n## 章型分工\n\n| 章 | 章型 | 本章消耗 | 本章新增 |\n| --- | --- | --- | --- |\n| 2 | 起 | — | U-001 |\n| 3 | 推 | U-001 | U-002 |\n| 4 | 转 | — | — |\n| 5 | 结 | U-002 | — |\n",
        )
        .unwrap();
        root
    }

    /// 事件进度：位置、未结项、锚词、本章该消耗/该新增，一条都不能少。
    #[test]
    fn event_digest_carries_open_items_and_the_chapter_role() {
        let root = event_book("digest");
        let text = event_digest(&root, 3).expect("第 3 章在事件里");
        assert!(text.contains("【事件内约束】事件 002 · 码头交接"), "{text}");
        assert!(text.contains("第 2/4 章"), "{text}");
        assert!(text.contains("章型：推"), "{text}");
        assert!(text.contains("U-001 仓单第三联不在"), "{text}");
        assert!(text.contains("最迟第 3 章结算"), "{text}");
        assert!(text.contains("锚词：第三联、复印件"), "{text}");
        assert!(text.contains("消耗：U-001"), "{text}");
        assert!(text.contains("新增：U-002"), "{text}");
        assert!(text.contains("仓单原件入库"), "{text}");
        let _ = fs::remove_dir_all(&root);
    }

    /// 账本赢过未结项：已结的编号不再出现在清单里；逾期项单独提示。
    #[test]
    fn event_digest_drops_settled_items_and_flags_overdue() {
        let root = event_book("overdue");
        let text = event_digest(&root, 5).expect("第 5 章在事件里");
        assert!(!text.contains("U-000"), "账本里已结的不能再来：{text}");
        assert!(text.contains("已过最迟结算章：U-001"), "{text}");
        assert!(!text.contains("最迟第 3 章结算）"), "逾期项不再重复列一遍：{text}");
        let _ = fs::remove_dir_all(&root);
    }

    /// 已结算的事件整张退出注入：结算过的事件不能被写手从注入里重新展开一遍。
    #[test]
    fn a_settled_card_keeps_its_open_items_out_of_the_digest() {
        let root = event_book("settled-out");
        let path = root.join("大纲/事件/事件_002_码头交接.md");
        let card = fs::read_to_string(&path)
            .unwrap()
            .replace("| 状态 | 进行 |", "| 状态 | 已结算 |");
        fs::write(&path, card).unwrap();
        fs::write(
            root.join("大纲/事件/事件_003_余波.md"),
            "# 事件 003 · 余波\n\n## 基本信息\n\n| 项 | 值 |\n| --- | --- |\n| 章号区间 | 2-6 |\n| 状态 | 进行 |\n\n## 未结项\n\n| 编号 | 内容 | 埋于 | 最迟结算章 | 锚词 |\n| --- | --- | --- | --- | --- |\n| V-001 | 尾款没到账 | 2 | 6 | 尾款 |\n",
        )
        .unwrap();

        let text = event_digest(&root, 3).expect("第 3 章还盖着 003");
        assert!(text.contains("事件 003 · 余波"), "{text}");
        assert!(text.contains("V-001"), "{text}");
        assert!(!text.contains("码头交接"), "已结算的卡不该再被注入：{text}");
        assert!(!text.contains("U-001"), "它的未结项更不该跟着下来：{text}");
        assert!(!text.contains("U-002"), "{text}");
        let _ = fs::remove_dir_all(&root);
    }

    /// 一章盖着三张卡：前两张都注入，第三张点名但不注入。
    /// 以前只取排序第一张，第二张起的事件约束整段消失。
    #[test]
    fn event_digest_injects_two_cards_and_names_the_rest() {
        let root = event_book("multi");
        fs::write(
            root.join("大纲/事件/事件_003_码头余波.md"),
            "# 事件 003 · 码头余波\n\n## 基本信息\n\n| 项 | 值 |\n| --- | --- |\n| 章号区间 | 3-6 |\n| 状态 | 进行 |\n\n## 未结项\n\n| 编号 | 内容 | 埋于 | 最迟结算章 | 锚词 |\n| --- | --- | --- | --- | --- |\n| V-001 | 尾款没到账 | 3 | 5 | 尾款 |\n\n## 章型分工\n\n| 章 | 章型 |\n| --- | --- |\n| 3 | 推 |\n",
        )
        .unwrap();
        fs::write(
            root.join("大纲/事件/事件_004_额外的线.md"),
            "# 事件 004 · 额外的线\n\n## 基本信息\n\n| 项 | 值 |\n| --- | --- |\n| 章号区间 | 3-4 |\n| 状态 | 进行 |\n\n## 未结项\n\n| 编号 | 内容 | 埋于 | 最迟结算章 | 锚词 |\n| --- | --- | --- | --- | --- |\n| W-001 | 不该被注入 | 3 | 4 | 别的 |\n",
        )
        .unwrap();

        let text = event_digest(&root, 3).expect("第 3 章盖着三张卡");
        assert!(text.contains("事件 002 · 码头交接"), "{text}");
        assert!(text.contains("事件 003 · 码头余波"), "第二张卡也要注入：{text}");
        assert!(text.contains("V-001"), "第二张卡的未结项要带上：{text}");
        assert!(text.contains("还盖着 1 张事件卡没注入"), "{text}");
        assert!(!text.contains("W-001"), "第三张卡不注入正文：{text}");
        assert!(text.contains("004 · 额外的线"), "要点名没注入的那张：{text}");
        let _ = fs::remove_dir_all(&root);
    }

    /// 前置事件还剩的事跟着注入：跨事件承接不能指望写手自己翻上一张卡。
    #[test]
    fn event_digest_carries_the_upstream_event_leftovers() {
        let root = event_book("upstream");
        fs::write(
            root.join("大纲/事件/事件_003_尾声.md"),
            "# 事件 003 · 尾声\n\n## 基本信息\n\n| 项 | 值 |\n| --- | --- |\n| 章号区间 | 4-6 |\n| 前置事件 | 002 |\n| 状态 | 进行 |\n\n## 未结项\n\n| 编号 | 内容 | 埋于 | 最迟结算章 | 锚词 |\n| --- | --- | --- | --- | --- |\n| V-001 | 尾款没到账 | 4 | 6 | 尾款 |\n",
        )
        .unwrap();

        let text = event_digest(&root, 4).expect("第 4 章盖着两张卡");
        assert!(text.contains("上游事件 002 · 码头交接"), "{text}");
        assert!(text.contains("跨事件承接"), "{text}");
        assert!(text.contains("U-002"), "上游没结的项要列出：{text}");
        assert!(!text.contains("U-000"), "账本里已结的不算剩余：{text}");
        let _ = fs::remove_dir_all(&root);
    }

    /// 细纲点了名但没有卡：注入层要明说，别让写手裸写。
    #[test]
    fn event_digest_names_the_declared_event_even_without_a_card() {
        let root = std::env::temp_dir().join("storydesk-ctx-event-nocard");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("大纲/事件")).unwrap();
        fs::create_dir_all(root.join("正文")).unwrap();
        fs::write(
            root.join("大纲/细纲_第003章.md"),
            "# 第 3 章\n\n所属事件：事件 007 · 失踪的账本\n\n## 本章目标\n\n找账本。\n",
        )
        .unwrap();

        let text = event_digest(&root, 3).expect("点了名就该有话说");
        assert!(text.contains("007"), "{text}");
        assert!(text.contains("没有这张卡"), "{text}");
        let _ = fs::remove_dir_all(&root);
    }

    /// 事件卡进全局清单；章号落在区间外就不进。
    #[test]
    fn event_card_enters_the_global_context_only_inside_its_span() {
        let root = event_book("quota");
        let inside = global_context(&root, 3, &[]);
        assert!(
            inside.contains(&"大纲/事件/事件_002_码头交接.md".to_string()),
            "{inside:?}"
        );
        let outside = global_context(&root, 9, &[]);
        assert!(
            !outside.iter().any(|p| p.starts_with("大纲/事件/")),
            "{outside:?}"
        );
        let _ = fs::remove_dir_all(&root);
    }

    /// 事件位是硬配额：三张卡同时覆盖本章，只进两张，总量仍守住 12。
    #[test]
    fn event_quota_keeps_two_cards() {
        let root = event_book("quota2");
        for n in [3u32, 4] {
            fs::write(
                root.join(format!("大纲/事件/事件_{n:03}_支线.md")),
                format!(
                    "# 事件 {n:03} · 支线\n\n## 基本信息\n\n| 项 | 值 |\n| --- | --- |\n| 章号区间 | 2-6 |\n| 状态 | 进行 |\n\n## 章型分工\n\n| 章 | 章型 | 本章消耗 | 本章新增 |\n| --- | --- | --- | --- |\n| 3 | 推 | — | — |\n"
                ),
            )
            .unwrap();
        }
        let list = global_context(&root, 3, &[]);
        let events = list.iter().filter(|p| p.starts_with("大纲/事件/")).count();
        assert_eq!(events, QUOTA_EVENT, "{list:?}");
        assert!(list.len() <= GLOBAL_LIMIT, "{list:?}");
        let _ = fs::remove_dir_all(&root);
    }

    /// 第 1 章没有上一章，但有事件约束——不能因为「没有上一章」把事件也一起丢掉。
    #[test]
    fn the_first_chapter_still_gets_the_event_constraint() {
        let root = event_book("first");
        let digest = prev_chapter_digest(&root, 2).expect("第 2 章应有事件约束");
        assert!(digest.contains("【事件内约束】"), "{digest}");
        assert!(!digest.contains("上一章正文结尾"), "{digest}");
        let _ = fs::remove_dir_all(&root);
    }

    /// 收尾请求里没写章号时，结尾章号要从书目录已有文件推断：
    /// 正文写到 308、细纲写到 350 → 全书结尾是 350，依据是那份细纲。
    /// 一本没写过的新书不许编出章号。
    #[test]
    /// 书级一致性：声明的篇幅、细纲排到的章、正文写到的章，三个数字各自算清。
    fn book_scope_reports_declared_versus_actual() {
        let root = std::env::temp_dir().join("storydesk-book-scope");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("大纲")).unwrap();
        std::fs::create_dir_all(root.join("正文")).unwrap();
        std::fs::write(
            root.join("大纲/大纲.md"),
            "## 四、卷结构\n\n> 全书共七卷、300 章：卷一 第1—40章，卷二 第41—85章，卷三 第86—125章，卷四 第126—173章，卷五 第174—218章，卷六 第219—260章，卷七 第261—300章（含尾声）。\n",
        )
        .unwrap();
        std::fs::write(root.join("大纲/细纲_第330章.md"), "# 330").unwrap();
        std::fs::write(root.join("大纲/细纲_第350章.md"), "# 350").unwrap();
        std::fs::write(root.join("正文/第307章_一.md"), "# 307").unwrap();
        std::fs::write(root.join("正文/第308章_一张名片.md"), "# 308").unwrap();

        let scope = book_scope(&root);
        assert_eq!(scope.declared_total, Some(300));
        assert_eq!(scope.declared_volumes, Some(7));
        assert_eq!(scope.volume_ends.last().copied(), Some(300));
        assert_eq!(scope.outline_end, Some(350));
        assert_eq!(scope.prose_end, Some(308));
        assert_eq!(scope.outline_count, 3, "大纲.md 也算一份大纲文件");
        assert_eq!(scope.prose_count, 2);

        let warnings = scope_warnings(&scope);
        assert!(
            warnings.iter().any(|w| w.contains("300") && w.contains("350")),
            "必须指出声明 300 章但细纲排到 350：{warnings:?}"
        );
        assert!(
            warnings.iter().any(|w| w.contains("350") && w.contains("308")),
            "必须指出正文落后细纲：{warnings:?}"
        );
        assert!(
            warnings.iter().all(|w| !w.contains("卷结构最后一卷")),
            "卷结构末章 300 与声明 300 一致，不该报不一致：{warnings:?}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// 空书不该冒出一堆「差 N 章」的噪音。
    #[test]
    fn empty_book_has_no_consistency_warnings() {
        let root = std::env::temp_dir().join("storydesk-book-scope-empty");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let scope = book_scope(&root);
        assert_eq!(scope, BookScope::default());
        assert!(scope_warnings(&scope).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 盘上还没有结尾章文件时，大纲里声明的总章数就是结尾章号的依据。
    #[test]
    fn declared_total_counts_as_ending_evidence() {
        let root = std::env::temp_dir().join("storydesk-declared-ending");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("大纲")).unwrap();
        std::fs::write(
            root.join("大纲/大纲.md"),
            "> 全书共七卷、350 章：卷一 第1—40章，卷七 第341—350章（含尾声）。",
        )
        .unwrap();
        std::fs::write(root.join("大纲/细纲_第330章.md"), "# 330").unwrap();
        let hint = book_end_hint(&root).expect("必须有推断结果");
        assert_eq!(hint.chapter, 350);
        assert!(hint.source.contains("大纲.md"), "{}", hint.source);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn book_end_hint_takes_the_farthest_chapter_on_disk() {
        let root = std::env::temp_dir().join("storydesk-ctx-endhint");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("大纲")).unwrap();
        fs::create_dir_all(root.join("正文")).unwrap();
        fs::write(root.join("正文/第308章_一张名片.md"), "正文").unwrap();
        fs::write(root.join("大纲/细纲_第330章.md"), "细纲").unwrap();
        fs::write(root.join("大纲/细纲_第350章.md"), "细纲").unwrap();
        let hint = book_end_hint(&root).expect("应推出结尾章");
        assert_eq!(hint.chapter, 350);
        assert_eq!(hint.source, "大纲/细纲_第350章.md");

        // 合并形态：章号只写在卷文件名里，区间末端也要认（`第021-040章`）。
        let range_root = std::env::temp_dir().join("storydesk-ctx-endhint-range");
        let _ = fs::remove_dir_all(&range_root);
        fs::create_dir_all(range_root.join("大纲")).unwrap();
        fs::write(range_root.join("大纲/卷七_第341-350章.md"), "卷细纲").unwrap();
        let hint = book_end_hint(&range_root).expect("卷文件也要能推出章号");
        assert_eq!(hint.chapter, 350, "{hint:?}");

        // 章号只写在索引里时靠索引行。
        let index_root = std::env::temp_dir().join("storydesk-ctx-endhint-index");
        let _ = fs::remove_dir_all(&index_root);
        fs::create_dir_all(index_root.join("大纲")).unwrap();
        fs::write(
            index_root.join(OUTLINE_INDEX_REL),
            "# 细纲索引\n第352章 | 大纲/卷八_第351章.md\n",
        )
        .unwrap();
        let hint = book_end_hint(&index_root).expect("索引行也要能推出章号");
        assert_eq!(hint.chapter, 352, "{hint:?}");

        // 空书：没有任何章号可推，不许编一个。
        let empty = std::env::temp_dir().join("storydesk-ctx-endhint-empty");
        let _ = fs::remove_dir_all(&empty);
        fs::create_dir_all(&empty).unwrap();
        assert!(book_end_hint(&empty).is_none());
    }

    /// 老书没有 `大纲/事件/`：行为与 S2 之前完全一致。
    #[test]
    fn no_event_dir_keeps_the_old_digest() {
        let root = std::env::temp_dir().join("storydesk-ctx-noevent");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("大纲")).unwrap();
        fs::create_dir_all(root.join("正文")).unwrap();
        fs::write(root.join("大纲/细纲_第002章.md"), "本章目标：谈判升级。").unwrap();
        fs::write(
            root.join("正文/第002章_谈判.md"),
            "谈判桌前的对话。".repeat(50),
        )
        .unwrap();
        let digest = prev_chapter_digest(&root, 3).expect("仍应有上一章摘要");
        assert!(!digest.contains("【事件内约束】"), "{digest}");
        assert!(digest.contains("上一章正文结尾"), "{digest}");
        let _ = fs::remove_dir_all(&root);
    }

    /// 滚动记忆：前情提要必须带上 记忆/摘要索引.md 的最近几条。
    #[test]
    fn prev_digest_includes_rolling_summaries() {
        let root = std::env::temp_dir().join("storydesk-ctx-rolling");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("大纲")).unwrap();
        fs::create_dir_all(root.join("正文")).unwrap();
        fs::create_dir_all(root.join("记忆")).unwrap();
        fs::write(
            root.join("大纲").join("细纲_第002章.md"),
            "本章目标：谈判升级。",
        )
        .unwrap();
        fs::write(
            root.join("正文").join("第002章_谈判.md"),
            "谈判桌前的对话。".repeat(50),
        )
        .unwrap();
        fs::write(
            root.join("记忆").join("摘要索引.md"),
            "第1章 | D1 | 公司 | 林成 | 签约 | 无 | 无 | 钩子1\n第2章 | D2 | 会议室 | 林成、周慕 | 摊牌 | 伏笔A | 无 | 钩子2\n",
        ).unwrap();
        let digest = prev_chapter_digest(&root, 3).expect("应有前情提要");
        assert!(digest.contains("近期章节滚动摘要"), "{digest}");
        assert!(digest.contains("第2章 | D2"), "{digest}");
        let _ = fs::remove_dir_all(&root);
    }

    /// 滚动记忆：索引文件行序乱掉（补写旧章节）时，前情提要仍按章号取最近六章。
    #[test]
    fn prev_digest_sorts_the_index_by_chapter_number() {
        let root = std::env::temp_dir().join("storydesk-ctx-digest-order");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("记忆")).unwrap();
        // 真实数据形状：文件里是 9、10、11、8、12、13（第 8 章是后补的）。
        let mut index = String::from("# 摘要索引\n\n手写备注：这一行没有章号\n");
        for ch in [9u32, 10, 11, 8, 12, 13] {
            index.push_str(&format!(
                "第{ch}章 | 时间{ch} | 地点{ch} | 人物{ch} | 事件{ch} | 伏笔{ch} | 回收{ch} | 钩子{ch}\n"
            ));
        }
        fs::write(root.join("记忆").join("摘要索引.md"), index).unwrap();
        let digest = prev_chapter_digest(&root, 14).expect("应有前情提要");
        let mut seen: Vec<u32> = Vec::new();
        for line in digest.lines() {
            if let Some(ch) = crate::services::memory::row_chapter(line.trim()) {
                seen.push(ch);
            }
        }
        assert_eq!(seen, vec![8, 9, 10, 11, 12, 13], "{digest}");
        assert!(
            !digest.contains("手写备注"),
            "没有章号的行不能混进来：{digest}"
        );
        let _ = fs::remove_dir_all(&root);
    }

    /// 目录配额：设定塞再多，也不能把追踪 / 参考资料 / 本章细纲的名单吃光。
    ///
    /// 真实事故：`设定/` 9 个文件的排序刚好压过 `追踪/`（7 个）与 `参考资料/`（9 个），
    /// 全局截断之后写手拿到的 12 个文件全是设定，伏笔表与时间线一次都没进过 prompt。
    #[test]
    fn global_context_shares_the_budget_across_directories() {
        let root = std::env::temp_dir().join("storydesk-ctx-quota");
        let _ = fs::remove_dir_all(&root);
        for dir in ["设定", "追踪", "参考资料", "大纲"] {
            fs::create_dir_all(root.join(dir)).unwrap();
        }
        for i in 1..=9 {
            fs::write(root.join(format!("设定/设定{i:02}.md")), "# 设定\n").unwrap();
            fs::write(root.join(format!("参考资料/资料{i:02}.md")), "# 资料\n").unwrap();
        }
        for i in 1..=7 {
            fs::write(root.join(format!("追踪/追踪{i:02}.md")), "# 追踪\n").unwrap();
        }
        for i in 1..=60 {
            fs::write(root.join(format!("大纲/卷_第{i:03}章.md")), "# 大纲\n").unwrap();
        }
        fs::write(root.join("大纲/细纲_第005章.md"), "# 第005章细纲\n").unwrap();

        let list = global_context(&root, 5, &[]);
        let count = |prefix: &str| list.iter().filter(|p| p.starts_with(prefix)).count();
        assert!(list.len() <= GLOBAL_LIMIT, "总量必须守住 12：{list:?}");
        assert_eq!(
            list.first().map(String::as_str),
            Some("大纲/细纲_第005章.md"),
            "本章细纲永远排第一"
        );
        assert_eq!(count("追踪/"), QUOTA_TRACK, "追踪信息不能被挤掉：{list:?}");
        assert_eq!(
            count("参考资料/"),
            QUOTA_REF,
            "参考资料不能被挤掉：{list:?}"
        );
        assert!(count("设定/") >= 1, "设定也要有位置：{list:?}");
        // 余量分配完仍然不能超总额。
        assert_eq!(
            count("设定/") + count("追踪/") + count("参考资料/") + count("大纲/"),
            list.len(),
            "只该出现这几类目录里的文件：{list:?}"
        );
        let _ = fs::remove_dir_all(&root);
    }

    fn book(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("storydesk-ctx-{tag}"));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("大纲")).unwrap();
        fs::create_dir_all(root.join("设定")).unwrap();
        fs::create_dir_all(root.join("追踪")).unwrap();
        fs::write(
            root.join("大纲/细纲_第002章.md"),
            "# 第002章\n\n## 节奏与字数\n字数目标：2400–2600 字。三段场景按 3:4:3 分配；对白占四成，环境描写不超过两成。\n",
        )
        .unwrap();
        fs::write(root.join("设定/角色状态.md"), "# 角色\n").unwrap();
        fs::write(root.join("追踪/时间线.md"), "# 时间线\n").unwrap();
        root
    }

    /// 细纲的「正文目标字数」就是验收层的达标线（用户没点名字数时）。
    #[test]
    fn outline_word_range_reads_the_chapter_target() {
        let root = book("target");
        fs::write(
            root.join("大纲/细纲_第002章.md"),
            "# 第002章\n\n正文目标字数：6200 字（全章总字数；区间 6000—6500）\n",
        )
        .unwrap();
        assert_eq!(outline_word_range(&root, 2), Some((6000, 6500)));
        assert_eq!(outline_word_range(&root, 9), None, "没有细纲就没有达标线");
    }

    /// 只给一个目标值时按 ±5% 折算；单场「字数建议」不算全章口径。
    #[test]
    fn a_single_target_value_becomes_a_five_percent_band() {
        assert_eq!(word_range_of("正文目标字数：6200 字\n"), Some((5890, 6510)));
        assert_eq!(word_range_of("场景一 字数建议 2200—2600 字\n"), None);
    }

    #[test]
    fn global_context_puts_outline_first_and_skips_dupes() {
        let root = book("order");
        let list = global_context(&root, 2, &["设定/角色状态.md".to_string()]);
        assert_eq!(
            list.first().map(String::as_str),
            Some("大纲/细纲_第002章.md")
        );
        assert!(list.iter().any(|p| p == "追踪/时间线.md"));
        assert!(
            !list.iter().any(|p| p == "设定/角色状态.md"),
            "skip 里的路径不该出现"
        );
        assert!(list.len() <= GLOBAL_LIMIT);
    }

    #[test]
    fn hard_constraints_reads_numbers_from_the_outline() {
        let root = book("constraints");
        let got = hard_constraints(&root, 2).expect("应该抽到约束");
        assert!(got.contains("2400-2600 字"), "{got}");
        assert!(got.contains("对白占四成"), "{got}");
        assert!(got.contains("环境描写不超过两成"), "{got}");
        assert!(got.ends_with('。'));
    }

    #[test]
    fn hard_constraints_is_none_without_an_outline() {
        let root = book("empty");
        assert!(hard_constraints(&root, 9).is_none());
    }

    /// 纯字数口径：标题行、引用行、分隔线、空白、标点都不算，只数文字与数字。
    /// 这是编排层字数验收的依据，口径错了验收就是摆设。
    #[test]
    fn chapter_word_count_ignores_headings_punctuation_and_whitespace() {
        let root = book("wordcount");
        fs::create_dir_all(root.join("正文")).unwrap();
        fs::write(
            root.join("正文/第003章_风雪.md"),
            "# 第003章 风雪\n\
             \n\
             风雪夜归人。\n\
             \n\
             > 引用行不算\n\
             ---\n\
             床前 明月光，疑是地上霜。\n",
        )
        .unwrap();
        let count = chapter_word_count(&root, 3).expect("文件在，应有字数");
        let expect: usize = "风雪夜归人".chars().count() + "床前明月光疑是地上霜".chars().count();
        assert_eq!(count, expect, "标题/引用/分隔线/空格/标点都不计入");
        // 兼容不带前导零的文件名；没有文件的章返回 None
        assert_eq!(chapter_word_count(&root, 9), None);
    }

    #[test]
    fn chapter_word_count_does_not_confuse_chapter_2_with_chapter_20() {
        let root = book("wordcount-prefix");
        fs::create_dir_all(root.join("正文")).unwrap();
        fs::write(
            root.join("正文/第020章_旧章.md"),
            "第二十章正文".repeat(200),
        )
        .unwrap();
        assert_eq!(
            chapter_word_count(&root, 2),
            None,
            "第 2 章不能把第 20 章的文件算进来"
        );
        assert_eq!(chapter_in_name("正文/第020章_旧章.md"), Some(20));
        assert_eq!(chapter_in_name("正文/第2章_新章.md"), Some(2));
    }

    #[test]
    fn hard_constraints_prefers_total_word_count_over_scene_suggestions() {
        let root = book("constraints-total");
        fs::write(
            root.join("大纲/细纲_第002章.md"),
            "# 第002章\n\n\
             ### 场景一\n- 字数建议：2200—2600字。\n- 对白比例：约35%。\n\n\
             ### 场景二\n- 字数建议：3400—4000字。\n\n\
             ## 九、本章字数与对白比例\n\
             - **正文目标字数**：8000—9500字（不计标点与空白）。\n\
             - **对白比例**：全章约45%—50%；场景一35%，场景二55%。\n",
        )
        .unwrap();
        let got = hard_constraints(&root, 2).expect("应该抽到约束");
        assert!(got.contains("8000-9500 字"), "{got}");
        assert!(
            !got.contains("2200-2600"),
            "单场建议不能冒充全章目标：{got}"
        );
        assert!(got.contains("对白占全章约45%—50%"), "{got}");
        assert!(
            !got.contains("对白占35%"),
            "场景比例不能冒充全章比例：{got}"
        );
    }

    #[test]
    fn hard_constraints_can_exclude_word_count_for_user_override() {
        let root = book("constraints-no-words");
        fs::write(
            root.join("大纲/细纲_第002章.md"),
            "# 第002章\n- **正文目标字数**：8000—9500字。\n- **对白比例**：全章约45%—50%。\n",
        )
        .unwrap();
        let got = hard_constraints_without_word_count(&root, 2).expect("比例仍在");
        assert!(!got.contains("8000"), "{got}");
        assert!(!got.contains("字数"), "{got}");
        assert!(got.contains("对白"), "{got}");
    }

    #[test]
    fn outline_from_another_book_is_kept_with_a_warning() {
        let root = book("mismatch");
        fs::write(
            root.join("设定/题材定位.md"),
            "古代农民起事，群像，冷兵器。\n",
        )
        .unwrap();
        // 旧细纲写的是另一本书：雨夜来客
        fs::write(
            root.join("大纲/细纲_第001章.md"),
            "# 第001章 雨夜来客\n\n## 场景\n客栈的雨夜，来客敲开木门。\n",
        )
        .unwrap();
        let check = check_outline(&root, 1, Some("古代的农民起事"));
        // R1：题材零命中降级为警告，不再判作废——合并细纲要靠这条活下来。
        assert!(check.exists && check.reusable, "{check:?}");
        assert!(check.reason.contains("警告"), "{}", check.reason);
        assert!(check.reason.contains("古代的农民起事"), "{}", check.reason);
    }

    /// 合并细纲（一卷一份）：没有 `细纲_第NNN章.md`，靠扫章标题找到第 8 章。
    #[test]
    fn outline_in_a_merged_volume_file_is_found_by_scanning() {
        let root = book("merged");
        let body: String = "陈砚在仓库对账，老赵递来饭票。".repeat(40);
        fs::write(
            root.join("大纲/卷1_第001-010章.md"),
            format!(
                "# 卷1\n\n## 第1章 开门\n{}\n\n## 第8章 教不是代\n{}\n\n## 第9章 卯时\n{}\n",
                "开篇。", body, "收束。"
            ),
        )
        .unwrap();
        let check = check_outline(&root, 8, None);
        assert!(check.exists && check.reusable, "{check:?}");
        assert_eq!(
            outline_path(&root, 8).map(|p| p.file_name().unwrap().to_string_lossy().to_string()),
            Some("卷1_第001-010章.md".to_string())
        );
    }

    /// 全书大纲里「第N章：一句话」不算细纲——扫到了也不能复用。
    #[test]
    fn one_line_chapter_mention_is_not_an_outline() {
        let root = book("outline-only");
        fs::write(
            root.join("大纲/大纲.md"),
            "# 全书大纲\n\n第8章：陈砚查出账本有问题。\n第12章：老赵失约。\n",
        )
        .unwrap();
        assert!(!check_outline(&root, 8, None).exists);
        assert!(outline_path(&root, 8).is_none());
    }

    /// 有索引时以索引为准，索引优先于固定候选名与扫描。
    #[test]
    fn outline_index_wins_over_candidate_names() {
        let root = book("index-first");
        // 候选名存在且题材对不上（若被采用只会得到「零命中」警告）
        fs::write(
            root.join("大纲/细纲_第008章.md"),
            "# 第008章 雨夜来客\n\n客栈的雨夜。\n",
        )
        .unwrap();
        // 索引指向的合并文件里，第 8 章的段落命中了题材
        let body: String = "古代的农民起事，村里的余粮被抢，李敢带人围了官仓。".repeat(20);
        fs::write(
            root.join("大纲/卷2_第008章起.md"),
            format!("## 第8章 围仓\n{body}\n"),
        )
        .unwrap();
        fs::write(
            root.join(OUTLINE_INDEX_REL),
            "# 细纲索引\n8 | 大纲/卷2_第008章起.md\n",
        )
        .unwrap();
        let check = check_outline(&root, 8, Some("古代的农民起事"));
        assert!(check.reusable, "{check:?}");
        // 零命中分支的 reason 里也带着题材原句（含「农民」），所以必须断言「题材命中」，
        // 否则索引被忽略、退回候选名时这条测试照样会假通过。
        assert!(
            check.reason.contains("题材命中"),
            "索引没被采信：{}",
            check.reason
        );
        assert_eq!(
            outline_path(&root, 8).map(|p| p.file_name().unwrap().to_string_lossy().to_string()),
            Some("卷2_第008章起.md".to_string())
        );
    }

    #[test]
    fn outline_matching_the_genre_is_reusable() {
        let root = book("match");
        fs::write(
            root.join("大纲/细纲_第001章.md"),
            "# 第001章 秋收之前\n\n## 时代\n古代的农民起事前夜，村里的余粮被抢。\n",
        )
        .unwrap();
        let check = check_outline(&root, 1, Some("古代的农民起事"));
        assert!(check.reusable, "{check:?}");
        assert!(check.reason.contains("农民"), "{}", check.reason);
        // 没说题材时不作废旧稿
        assert!(check_outline(&root, 1, None).reusable);
    }

    #[test]
    fn setting_health_is_thin_when_core_files_are_missing() {
        let root = book("thin");
        // book() 预置的 设定/角色状态.md 名字里有「角色」，算核心文件；先删掉再验「缺核心」。
        let _ = fs::remove_file(root.join("设定/角色状态.md"));
        // 只有一份名字对不上核心名单、内容也几乎为空的设定文件。
        fs::write(root.join("设定/杂项.md"), "# 杂项\n一段话。\n").unwrap();
        let health = setting_health(&root);
        assert!(health.thin, "{health:?}");
        assert!(health.core_missing);
        assert!(health.note.starts_with("设定偏薄"), "{}", health.note);
    }

    /// 真实事故：`设定/` 里是 全书梗概 + 题材定位 + 文风 + 术语表（没有 世界概览/人物），
    /// 老判定把它当「缺核心」，每轮都强插一步【补设定】。
    #[test]
    fn setting_health_accepts_chinese_core_file_names() {
        let root = book("chinese-core");
        fs::write(
            root.join("设定/全书梗概.md"),
            "# 梗概\n乱世枭雄的前四十章走向。\n",
        )
        .unwrap();
        fs::write(root.join("设定/题材定位.md"), "# 题材\n古代的农民起事。\n").unwrap();
        fs::write(root.join("设定/文风.md"), "# 文风\n白描，短句。\n").unwrap();
        fs::write(root.join("设定/术语表.md"), "# 术语\n驿丞：驿站主官。\n").unwrap();
        let health = setting_health(&root);
        assert_eq!(
            health.files, 5,
            "book() 预置的角色状态.md + 四份中文设定：{health:?}"
        );
        assert!(!health.core_missing, "{health:?}");
        assert!(!health.thin, "{health:?}");
    }

    #[test]
    fn global_context_skips_chapter_outline_when_step_has_no_chapter() {
        let root = book("no-chapter-scope");
        fs::create_dir_all(root.join("大纲")).unwrap();
        fs::write(root.join("大纲/细纲_第005章.md"), "# 第五章细纲\n").unwrap();
        fs::write(root.join("大纲/大纲.md"), "# 全书大纲\n").unwrap();
        let without = global_context_of(&root, None, &[]);
        assert!(
            !without.iter().any(|rel| rel.contains("细纲_第005章")),
            "{without:?}"
        );
        assert!(
            without.iter().any(|rel| rel == "大纲/大纲.md"),
            "{without:?}"
        );
        let with = global_context_of(&root, Some(5), &[]);
        assert!(
            with.iter().any(|rel| rel.contains("细纲_第005章")),
            "{with:?}"
        );
    }

    #[test]
    fn setting_health_passes_with_a_world_overview() {
        let root = book("thick");
        fs::write(
            root.join("设定/世界概览.md"),
            "# 世界\n古平原，连年水患，官仓与义仓并存。\n",
        )
        .unwrap();
        fs::write(root.join("设定/人物.md"), "# 人物\n李敢，佃户。\n").unwrap();
        let health = setting_health(&root);
        assert!(!health.core_missing, "{health:?}");
        assert!(!health.thin, "{health:?}");
    }
}
