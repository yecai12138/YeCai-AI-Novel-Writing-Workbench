//! 全书级文风统计：只出**计数事实**，不出裁定。
//!
//! 为什么需要它：单章评审天然看不见「全书级」的固化模式——同一个句式 tic 一章出现
//! 三五次，每一处单看都正常；只有把它在几十万字里的次数摊开，人和模型才看得见。
//!
//! 纪律（与 docs/纪律清单.md 第 9 条一致）：
//! - 本模块只读正文、只算数字，不判断好坏、不生成修改建议；
//! - 统计落在这里，裁定交给 writer/editor 与作者；
//! - 正则只做近似计数，用途是「本书自身的纵向基线」，不是跨书绝对评分。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::Serialize;

/// 少于此章数不出统计：样本太小，频率没有意义。
pub const MIN_CHAPTERS: usize = 5;
/// 动态短语只挖最近 N 章：要抓的是「现在的口头禅」。
pub const PHRASE_WINDOW: usize = 20;
/// 跨章重复句的最短长度（按字符数），太短的句子重复是正常的。
const REPEAT_MIN_CHARS: usize = 12;
/// 章末「短结尾」的判定阈值（字符数）。
const SHORT_ENDING_CHARS: usize = 15;

// ---------------------------------------------------------------- 数据结构

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PatternStat {
    pub name: String,
    pub total: usize,
    pub per_chapter: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PhraseStat {
    pub text: String,
    pub count: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SentenceStat {
    pub text: String,
    pub chapters: usize,
    pub count: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EndingStat {
    pub short_ratio: f64,
    pub median_chars: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TitleStat {
    pub with_prefix: usize,
    pub without_prefix: usize,
}

/// 全书文风统计结果。所有字段都是计数，不含任何裁定或指令。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StyleStats {
    pub chapters: usize,
    pub total_chars: usize,
    pub avg_chars: usize,
    pub patterns: Vec<PatternStat>,
    pub top_phrases: Vec<PhraseStat>,
    pub repeated_sentences: Vec<SentenceStat>,
    pub ending: EndingStat,
    pub titles: TitleStat,
}

/// 一章的正文（已去掉标题行）。
#[derive(Debug, Clone)]
pub struct Chapter {
    pub no: u32,
    pub title: String,
    pub body: String,
}

// ---------------------------------------------------------------- 读取

/// 从 正文/ 读全部章节，按章号升序。
///
/// 只认项目自己生成的文件名（第NNN章_标题.md）：认不出章号的文件不参与统计，
/// 因为「这一章是第几章」是统计的坐标，猜错比漏掉更糟。
pub fn load_chapters(book_root: &Path) -> Vec<Chapter> {
    let dir = book_root.join("正文");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out: Vec<Chapter> = Vec::new();
    for entry in entries.flatten() {
        let path: PathBuf = entry.path();
        if !path.is_file() {
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let Some(no) = chapter_no(&name) else {
            continue;
        };
        let Ok(text) = crate::services::atomic_write::read_text(&path) else {
            continue;
        };
        let (title, body) = split_title(&text);
        out.push(Chapter { no, title, body });
    }
    out.sort_by_key(|c| c.no);
    out
}

/// 文件名里的章号：第012章_标题.md 得到 12。只认「第 + 数字 + 章」。
pub fn chapter_no(name: &str) -> Option<u32> {
    let rest = name.strip_prefix('第')?;
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() || !rest[digits.len()..].starts_with('章') {
        return None;
    }
    digits.parse().ok().filter(|n| *n > 0)
}

/// 拆出首行标题与正文。标题行不计入统计，否则「第001章」会被算成高频短语。
fn split_title(text: &str) -> (String, String) {
    let mut title = String::new();
    let mut body = String::new();
    for (i, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if i == 0 && (trimmed.starts_with('#') || trimmed.starts_with('第')) {
            title = trimmed.trim_start_matches('#').trim().to_string();
            continue;
        }
        body.push_str(line);
        body.push('\n');
    }
    if title.is_empty() {
        title = String::from("（无标题）");
    }
    (title, body)
}

// ---------------------------------------------------------------- 统计

/// 纯函数：给定章节，算出统计。不做任何 IO（可被测试离线驱动）。
pub fn compute(chapters: &[Chapter]) -> StyleStats {
    let n = chapters.len();
    let bodies: Vec<&str> = chapters.iter().map(|c| c.body.as_str()).collect();
    let total_chars: usize = bodies.iter().map(|b| count_chars(b)).sum();

    let patterns = pattern_defs()
        .iter()
        .map(|(name, re)| {
            let total: usize = bodies.iter().map(|b| re.find_iter(b).count()).sum();
            PatternStat {
                name: name.to_string(),
                total,
                per_chapter: if n == 0 { 0.0 } else { round2(total as f64 / n as f64) },
            }
        })
        .collect();

    let start = n.saturating_sub(PHRASE_WINDOW);
    let top_phrases = mine_phrases(&bodies[start..], 12);
    let repeated = repeated_sentences(&bodies, 10);

    StyleStats {
        chapters: n,
        total_chars,
        avg_chars: if n == 0 { 0 } else { total_chars / n },
        patterns,
        top_phrases,
        repeated_sentences: repeated,
        ending: ending_shape(&bodies),
        titles: title_shape(chapters),
    }
}

/// 读盘 + 统计。章数不足 MIN_CHAPTERS 时返回 None（样本太小，频率没有意义）。
pub fn stats_of(book_root: &Path) -> Option<StyleStats> {
    let chapters = load_chapters(book_root);
    if chapters.len() < MIN_CHAPTERS {
        return None;
    }
    Some(compute(&chapters))
}

/// 给模型看的简报：只陈述计数，最后一行是「别复用」这一句请求。
///
/// 措辞刻意保持「事实 + 一句请求」：判断（写得像不像、该不该改）不在这里做。
pub fn brief(stats: &StyleStats, top: usize) -> String {
    let mut out = String::new();
    out.push_str("【本书文风体检 · 只出计数，不代表好坏】\n");
    out.push_str(&format!(
        "已统计 {} 章，合计 {} 字，章均 {} 字。\n",
        stats.chapters, stats.total_chars, stats.avg_chars
    ));
    let mut hot: Vec<&PatternStat> = stats.patterns.iter().filter(|p| p.total > 0).collect();
    hot.sort_by(|a, b| b.total.cmp(&a.total));
    if !hot.is_empty() {
        out.push_str("高频句式（全书次数 / 每章次数）：\n");
        for p in hot.iter().take(top) {
            out.push_str(&format!("- {}：{} 次 / {:.2} 次每章\n", p.name, p.total, p.per_chapter));
        }
    }
    if !stats.top_phrases.is_empty() {
        let list: Vec<String> = stats
            .top_phrases
            .iter()
            .take(8)
            .map(|p| format!("{}（{} 次）", p.text, p.count))
            .collect();
        out.push_str(&format!(
            "最近 {} 章高频短语：{}\n",
            PHRASE_WINDOW.min(stats.chapters),
            list.join("、")
        ));
    }
    if !stats.repeated_sentences.is_empty() {
        out.push_str("跨章逐字重复的句子（同样的字被反复用来交代）：\n");
        for s in stats.repeated_sentences.iter().take(3) {
            out.push_str(&format!(
                "- 「{}」出现 {} 次，横跨 {} 章\n",
                clip(&s.text, 40),
                s.count,
                s.chapters
            ));
        }
    }
    out.push_str(&format!(
        "章末：短结尾占比 {:.0}%，章末行中位数 {} 字。\n",
        stats.ending.short_ratio * 100.0,
        stats.ending.median_chars
    ));
    if stats.titles.with_prefix > 0 && stats.titles.without_prefix > 0 {
        out.push_str(&format!(
            "章标题前缀混用：带「第N章」{} 个，不带 {} 个。\n",
            stats.titles.with_prefix, stats.titles.without_prefix
        ));
    }
    out.push_str("以上都是本书已经出现过的事实，本章不要再复用这些句式与短语。\n");
    out
}

// ---------------------------------------------------------------- 细节

fn count_chars(text: &str) -> usize {
    text.chars().filter(|c| !c.is_whitespace()).count()
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

fn clip(s: &str, max: usize) -> String {
    let mut out: String = s.chars().take(max).collect();
    if s.chars().count() > max {
        out.push('…');
    }
    out
}

/// 中文 AI 文风的固定句式模式。命名用「判据」而不是「错误」：这是计数口径。
fn pattern_defs() -> &'static Vec<(String, regex::Regex)> {
    static DEFS: OnceLock<Vec<(String, regex::Regex)>> = OnceLock::new();
    DEFS.get_or_init(|| {
        let raw: [(&str, &str); 8] = [
            ("矫正句「不是…而是…」", r"不是[^。！？\n]{1,24}?[，、]?(?:而)?是"),
            ("计时量词「X息 / X瞬」", r"[一两二三四五六七八九十几数半][息瞬]"),
            ("明喻「像一 / 仿佛 / 如同 / 宛如」", r"像一|仿佛|如同|宛如"),
            ("沉默节拍「沉默了 / 没有说话 / 没有回头」", r"沉默了|没有说话|没有回头"),
            (
                "神态模板「眼中闪过 / 嘴角勾起 / 咬了咬唇」",
                r"眼[中底]闪过|目光一凝|瞳孔一缩|眼眶微红|嘴角[微轻一]?[勾扬翘]|咬了咬唇|不可置信",
            ),
            (
                "躯体反应「心头一紧 / 身子一颤 / 倒吸凉气」",
                r"心头一[紧沉颤]|身子一[颤震僵]|倒吸(?:了)?一口凉气",
            ),
            ("思维标记「心想 / 意识到 / 感到 / 觉得」", r"心想|意识到|感到|觉得"),
            (
                "抽象套话「一种说不出的 / 的意义在于」",
                r"一种说不出的|说不清[的道]|的意义在于|真正的[^。！？\n]{1,10}是",
            ),
        ];
        raw.iter()
            .filter_map(|(name, pat)| regex::Regex::new(pat).ok().map(|re| (name.to_string(), re)))
            .collect()
    })
}

/// 常见虚词：含这些字的 n-gram 不是「口头禅」，是中文本身。
fn is_stop_gram(gram: &str) -> bool {
    const STOP: &str = "的了着过是在和与就都也又很更最不没有他她我你您它们这那其之为以于而但所以如果";
    gram.chars().any(|c| STOP.contains(c))
}

/// 滑动窗口挖 3 字高频短语。只统计汉字连续段，避免把标点拼进来。
fn mine_phrases(bodies: &[&str], limit: usize) -> Vec<PhraseStat> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for body in bodies {
        let chars: Vec<char> = body.chars().collect();
        if chars.len() < 3 {
            continue;
        }
        for i in 0..=(chars.len() - 3) {
            let gram: String = chars[i..i + 3].iter().collect();
            if gram.chars().all(is_han) && !is_stop_gram(&gram) {
                *counts.entry(gram).or_insert(0) += 1;
            }
        }
    }
    let mut out: Vec<PhraseStat> = counts
        .into_iter()
        .filter(|(_, c)| *c >= 3)
        .map(|(text, count)| PhraseStat { text, count })
        .collect();
    out.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.text.cmp(&b.text)));
    out.truncate(limit);
    out
}

fn is_han(c: char) -> bool {
    matches!(c as u32, 0x4E00..=0x9FFF)
}

/// 跨章逐字重复的句子：同一句（去标点后）出现在 ≥2 章里。
fn repeated_sentences(bodies: &[&str], limit: usize) -> Vec<SentenceStat> {
    let mut by_sentence: HashMap<String, (usize, usize)> = HashMap::new();
    let mut seen: HashSet<(usize, String)> = HashSet::new();
    for (idx, body) in bodies.iter().enumerate() {
        for sentence in sentences(body) {
            let key: String = sentence
                .chars()
                .filter(|c| !c.is_whitespace() && !"，。！？；：、…—·“”‘’（）《》".contains(*c))
                .collect();
            if key.chars().count() < REPEAT_MIN_CHARS {
                continue;
            }
            let entry = by_sentence.entry(key.clone()).or_insert((0, 0));
            entry.1 += 1;
            if seen.insert((idx, key)) {
                entry.0 += 1;
            }
        }
    }
    let mut out: Vec<SentenceStat> = by_sentence
        .into_iter()
        .filter(|(_, (chapters, _))| *chapters >= 2)
        .map(|(text, (chapters, count))| SentenceStat { text, chapters, count })
        .collect();
    out.sort_by(|a, b| b.chapters.cmp(&a.chapters).then_with(|| b.count.cmp(&a.count)));
    out.truncate(limit);
    out
}

fn sentences(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for ch in body.chars() {
        if matches!(ch, '。' | '！' | '？' | '!' | '?' | '\n') {
            let t = cur.trim().trim_matches(|c: char| {
                matches!(c, '“' | '”' | '"' | '「' | '」' | '『' | '』')
            });
            if !t.is_empty() {
                out.push(t.to_string());
            }
            cur.clear();
        } else {
            cur.push(ch);
        }
    }
    let t = cur.trim();
    if !t.is_empty() {
        out.push(t.to_string());
    }
    out
}

/// 章末形态：最后一行是不是短句。短结尾本身合法，全书同构才是问题。
fn ending_shape(bodies: &[&str]) -> EndingStat {
    let mut lens: Vec<usize> = Vec::new();
    for body in bodies {
        let last = body
            .lines()
            .rev()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or("");
        if last.is_empty() {
            continue;
        }
        lens.push(last.chars().filter(|c| !c.is_whitespace()).count());
    }
    if lens.is_empty() {
        return EndingStat { short_ratio: 0.0, median_chars: 0 };
    }
    let short = lens.iter().filter(|n| **n <= SHORT_ENDING_CHARS).count();
    lens.sort_unstable();
    let median = lens[lens.len() / 2];
    EndingStat {
        short_ratio: round2(short as f64 / lens.len() as f64),
        median_chars: median,
    }
}

fn title_shape(chapters: &[Chapter]) -> TitleStat {
    let mut with = 0;
    let mut without = 0;
    for c in chapters {
        if c.title.trim_start().starts_with('第') && c.title.contains('章') {
            with += 1;
        } else {
            without += 1;
        }
    }
    TitleStat { with_prefix: with, without_prefix: without }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chapter(no: u32, body: &str) -> Chapter {
        Chapter { no, title: format!("第{:03}章 测试", no), body: body.to_string() }
    }

    /// 规格镜像：手写期望值来自 docs/文风统计.md 的判据表，
    /// 不调用被测代码的任何辅助函数——实现重构后行为若漂移，本测试立刻红。
    #[test]
    fn spec_pattern_counts_match_hand_counted_fixture() {
        let text = "他不是害怕，而是冷。他心头一紧，眼中闪过一丝光。仿佛有人在看着他。\
                    我心想，这件事说不清道不明。安静了一瞬，他沉默了。";
        let s = compute(&[chapter(1, text)]);
        let get = |name: &str| {
            s.patterns
                .iter()
                .find(|p| p.name.contains(name))
                .map(|p| p.total)
                .unwrap_or(0)
        };
        assert_eq!(get("矫正句"), 1, "{:?}", s.patterns);
        assert_eq!(get("明喻"), 1, "{:?}", s.patterns);
        assert_eq!(get("躯体反应"), 1, "{:?}", s.patterns);
        assert_eq!(get("思维标记"), 1, "{:?}", s.patterns);
        assert_eq!(get("神态模板"), 1, "{:?}", s.patterns);
        assert_eq!(get("沉默节拍"), 1, "{:?}", s.patterns);
        assert_eq!(get("计时量词"), 1, "{:?}", s.patterns);
        assert_eq!(get("抽象套话"), 1, "{:?}", s.patterns);
    }

    #[test]
    fn chapter_no_only_accepts_project_naming() {
        assert_eq!(chapter_no("第012章_标题.md"), Some(12));
        assert_eq!(chapter_no("第1章_x.md"), Some(1));
        assert_eq!(chapter_no("012.md"), None);
        assert_eq!(chapter_no("第000章_x.md"), None);
        assert_eq!(chapter_no("番外_第3章.md"), None);
    }

    #[test]
    fn repeated_sentence_needs_two_chapters() {
        let body = "他把门推开，看见桌上放着一封没有署名的信。";
        let s = compute(&[chapter(1, body), chapter(2, body)]);
        assert_eq!(s.repeated_sentences.len(), 1);
        assert_eq!(s.repeated_sentences[0].chapters, 2);
    }

    #[test]
    fn brief_never_claims_quality() {
        let s = compute(&[chapter(1, "他心头一紧。"), chapter(2, "他心头一紧。")]);
        let text = brief(&s, 3);
        assert!(text.contains("只出计数"));
        assert!(!text.contains("优秀") && !text.contains("很差"));
    }
}
