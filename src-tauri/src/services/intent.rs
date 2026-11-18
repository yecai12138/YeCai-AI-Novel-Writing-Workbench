//! 需求意图识别：把作者的一句口语归到「写正文 / 排大纲 / 只读检查 / 补设定 / 拆对标 / 找资料」。
//!
//! 为什么要单独一层：模型没给出合法 JSON 时会退到 [`crate::commands::master::rules_plan`]，
//! 而规则以前散在函数体里，命中不了「写一本古代农民起事的小说」这种说法——含「设定」就被判成
//! 设定类，什么都没命中就兜底给建筑师「给出第一步」，建筑师为了交差写了份细纲盖掉旧的。
//!
//! 这里把判定集中成**一张带权重的表**：命中几个词、各几分、平手怎么裁、什么时候认怂，
//! 全部写在一处，规则可预测，也能被单测钉住。
//!
//! 判定的产物有两层：
//! - [`Intent`]：意图 + 置信度 + 章节号 + 题材 + 清洗过的主题句；
//! - [`Intent::describe`]：一句给人看的话（「我理解为：写正文 · 正文/ · 第 1 章」），
//!   计划卡上会显示，用户点确认前就知道 AI 到底理解成了什么。

use std::sync::OnceLock;

use regex::Regex;

/// 需求类型。顺序即「平手时的优先级」：写正文 > 只读检查 > 排大纲 > 补设定 > 拆对标 > 找资料。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Prose,
    Check,
    Outline,
    Settings,
    Benchmark,
    Research,
    Unknown,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Prose => "prose",
            Kind::Check => "check",
            Kind::Outline => "outline",
            Kind::Settings => "settings",
            Kind::Benchmark => "benchmark",
            Kind::Research => "research",
            Kind::Unknown => "unknown",
        }
    }

    /// 人话标签，计划卡与日志都用它。
    pub fn label(self) -> &'static str {
        match self {
            Kind::Prose => "写正文",
            Kind::Check => "只读检查",
            Kind::Outline => "排大纲 / 细纲",
            Kind::Settings => "补设定",
            Kind::Benchmark => "拆对标",
            Kind::Research => "找资料",
            Kind::Unknown => "没看懂要干什么",
        }
    }

    /// 这一步默认该落哪个目录。
    pub fn target_dir(self) -> &'static str {
        match self {
            Kind::Prose => "正文",
            Kind::Outline => "大纲",
            Kind::Settings => "设定",
            Kind::Benchmark => "对标",
            Kind::Research => "参考资料",
            Kind::Check | Kind::Unknown => "追踪",
        }
    }

    /// 只有「检查」与「没看懂」是只读的；其余都可能要写文件。
    pub fn read_only(self) -> bool {
        matches!(self, Kind::Check | Kind::Unknown)
    }
}

/// 大纲意图的层级：全书级（`大纲/大纲.md` / 卷纲）还是章级（`细纲_第N章.md`）。
///
/// 阶段 11.6 之前 `Kind::Outline` 不分层级，于是「把大纲写了，整本故事的大纲写好」
/// 被直接翻译成「生成第 1 章细纲」——用户要的是全书大纲，拿到的却是章级产物（还沿用旧的）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutlineScope {
    /// 全书大纲 / 卷纲，目标文件 `大纲/大纲.md`。
    Book,
    /// 章级细纲，目标文件 `大纲/细纲_第N章.md`。
    Chapter,
}

/// 置信度。低置信度只派一步「先读文件给结论」，其余一律等用户点确认。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence {
    High,
    Low,
}

#[derive(Debug, Clone)]
pub struct Intent {
    pub kind: Kind,
    pub confidence: Confidence,
    /// 需求里点到的章号；没写就是 `None`（调用方自己补默认值）。
    pub chapter: Option<u32>,
    /// 章号区间（「001-045 的细纲」）。有区间时 `chapter` 取区间起点，
    /// 计划必须覆盖区间里的每一章，而不是缩水成第 1 章。
    pub chapter_range: Option<(u32, u32)>,
    /// 相对章数（「接着最新写 3 章」）：起点 = 现有最大章号 + 1，由调用方换算。
    pub pending_count: Option<u32>,
    /// 用户规定的正文字数范围（「每章 6000-8000 字」），透传给写正文与细纲任务。
    pub word_count: Option<(u32, u32)>,
    /// 章节范围没说清（「大概几章吧」）：必须反问澄清，不许猜区间开跑。
    pub needs_clarification: bool,
    /// 题材，例如「古代的农民起事」。
    pub genre: Option<String>,
    /// 去掉「帮我/把…给我写了」这类外壳之后的主题句。
    pub subject: String,
    /// 是不是「写一整本」（用户还没指定章号的那种）。
    pub whole_book: bool,
    /// 用户说的是「没写全就补充一下」：要的是增量补写，绝不重写既有内容。
    /// 真实事故：「大纲内没有写七个卷每个卷有多少章补充一下」被覆盖成
    /// 「重新生成全书大纲，旧的大纲与细纲一律作废」，把作者自己的大纲冲了。
    pub supplement: bool,
    /// 大纲层级（只有 `kind == Outline` 时有意义）。
    pub outline_scope: OutlineScope,
    /// 命中的证据：`[("prose","正文"=3), ("settings","设定"=3)]`，只用于日志与报告。
    pub hits: Vec<(Kind, &'static str, u32)>,
    /// 这次请求要「收尾/完结整本书」。
    ///
    /// 以前每个调用点各自 `is_ending_request(request)` 判一次，同一句话在不同环节
    /// 可能得出不同结论（真实事故：计划卡写「整本从头写」，收尾从句却贴到了第 335 章）。
    /// 现在只在 [`classify`] 里判一次，需要的地方读这个字段。
    pub ending: bool,
    /// 这次请求要改「写作规矩」（口径/比例/禁忌），不是写内容。
    pub rule_edit: bool,
}

impl Intent {
    pub fn read_only(&self) -> bool {
        self.kind.read_only()
    }

    pub fn is_low(&self) -> bool {
        self.confidence == Confidence::Low
    }

    /// 给人看的一句话。低置信度会自带「把握不大」的提示。
    pub fn describe(&self) -> String {
        // 阶段 11.6：全书大纲不是「第 N 章细纲」，计划卡必须直说目标文件。
        if self.kind == Kind::Outline && self.outline_scope == OutlineScope::Book {
            let mut text = if self.supplement {
                "补充大纲 · 大纲/大纲.md（增量补写，不重写旧内容）".to_string()
            } else {
                "写整本大纲 · 大纲/大纲.md".to_string()
            };
            if let Some(genre) = &self.genre {
                text.push_str(&format!(" · 题材「{genre}」"));
            }
            if self.is_low() {
                text.push_str("（把握不大，确认前请核对）");
            }
            return text;
        }
        let dir = self.kind.target_dir();
        // 章号只对「章级」任务有意义：补设定 / 对标拆解 / 找资料这类与章号无关的
        // 意图，之前也硬拼一个「章号待定」，用户看着就是描述错了。
        let chapter_aware = matches!(self.kind, Kind::Outline | Kind::Prose | Kind::Check);
        let chapter = match self.chapter_range {
            Some((s, e)) if chapter_aware => {
                format!(" · 第 {s:03}-{e:03} 章（共 {} 章）", e - s + 1)
            }
            _ => match self.chapter {
                Some(no) if chapter_aware => format!(" · 第 {no} 章"),
                _ if chapter_aware => " · 章号待定".to_string(),
                _ => String::new(),
            },
        };
        let mut text = format!("{} · {}/{chapter}", self.kind.label(), dir);
        if let Some(count) = self.pending_count {
            text.push_str(&format!(" · 接现有进度写 {count} 章（起点=最新章+1）"));
        }
        if self.needs_clarification {
            text.push_str(" · 范围没说清，先反问");
        }
        if let Some(genre) = &self.genre {
            text.push_str(&format!(" · 题材「{genre}」"));
        }
        if self.whole_book {
            text.push_str(" · 整本从头写");
        }
        if self.read_only() {
            text.push_str(" · 只读不改文件");
        }
        if self.is_low() {
            text.push_str("（把握不大，确认前请核对）");
        }
        text
    }

    /// 计划卡上的短标签（多意图拼接用）。
    fn short_label(&self) -> String {
        match self.kind {
            Kind::Outline if self.outline_scope == OutlineScope::Book => {
                if self.supplement {
                    "补充大纲（大纲/大纲.md · 增量）".to_string()
                } else {
                    "写整本大纲（大纲/大纲.md）".to_string()
                }
            }
            Kind::Outline => match self.pending_count {
                Some(count) => format!("接现有进度写 {count} 章细纲"),
                None => match self.chapter_range {
                    Some((s, e)) => format!("排第 {s:03}-{e:03} 章细纲（共 {} 章）", e - s + 1),
                    None => format!("排第 {} 章细纲", self.chapter.unwrap_or(1)),
                },
            },
            Kind::Settings => "补全设定（设定/）".to_string(),
            other => other.label().to_string(),
        }
    }

    /// 证据串，写日志用：`prose=4>settings=3`。
    pub fn evidence(&self) -> String {
        if self.hits.is_empty() {
            return "无命中".to_string();
        }
        self.hits
            .iter()
            .take(4)
            .map(|(kind, word, score)| format!("{}({}={})", kind.as_str(), word, score))
            .collect::<Vec<_>>()
            .join(" + ")
    }
}

/// 关键词表：`(类型, 词, 权重)`。权重越大越能代表意图，「正文」是 3，「写了」只有 1。
///
/// 加词的门槛：**它在书里出现时，用户大概率真的要这件事**。像「拆」这种孤字只配 1 分，
/// 因为「拆解对标」要算对标、「拆细纲」要算大纲，靠后面的词再拉高分。
const KEYWORDS: &[(Kind, &str, u32)] = &[
    // ---- 写正文 ----
    (Kind::Prose, "正文", 3),
    (Kind::Prose, "落盘", 3),
    (Kind::Prose, "成稿", 3),
    (Kind::Prose, "扩写", 3),
    (Kind::Prose, "码字", 3),
    (Kind::Prose, "写一章", 3),
    (Kind::Prose, "写一版", 3),
    (Kind::Prose, "写一本", 3),
    (Kind::Prose, "写一部", 3),
    (Kind::Prose, "写小说", 3),
    (Kind::Prose, "写故事", 3),
    (Kind::Prose, "写出来", 2),
    (Kind::Prose, "开写", 3),
    (Kind::Prose, "动笔", 2),
    (Kind::Prose, "本章", 2),
    (Kind::Prose, "这一章", 2),
    (Kind::Prose, "下一章", 2),
    (Kind::Prose, "写第", 3),
    (Kind::Prose, "写了", 1),
    (Kind::Prose, "写一下", 1),
    (Kind::Prose, "写成", 1),
    // ---- 只读检查 ----
    (Kind::Check, "检查", 3),
    (Kind::Check, "核对", 3),
    (Kind::Check, "盘点", 3),
    (Kind::Check, "梳理", 3),
    (Kind::Check, "评估", 3),
    (Kind::Check, "审查", 3),
    (Kind::Check, "对账", 3),
    (Kind::Check, "有没有问题", 3),
    (Kind::Check, "是否一致", 3),
    (Kind::Check, "冲突", 2),
    (Kind::Check, "挑错", 3),
    (Kind::Check, "伏笔", 3),
    (Kind::Check, "时间线", 3),
    (Kind::Check, "欠账", 3),
    (Kind::Check, "前后矛盾", 3),
    (Kind::Check, "列出", 2),
    (Kind::Check, "分析", 2),
    // ---- 大纲 / 细纲 ----
    (Kind::Outline, "细纲", 3),
    (Kind::Outline, "大纲", 3),
    (Kind::Outline, "卷纲", 3),
    (Kind::Outline, "章纲", 3),
    (Kind::Outline, "结构", 2),
    (Kind::Outline, "规划", 2),
    (Kind::Outline, "分章", 2),
    (Kind::Outline, "拆解大纲", 3),
    (Kind::Outline, "拆", 1),
    // ---- 设定 ----
    (Kind::Settings, "设定", 3),
    (Kind::Settings, "人设", 3),
    (Kind::Settings, "世界观", 3),
    (Kind::Settings, "势力", 3),
    (Kind::Settings, "术语", 3),
    (Kind::Settings, "小传", 3),
    (Kind::Settings, "人物", 2),
    (Kind::Settings, "角色", 2),
    // ---- 对标 ----
    (Kind::Benchmark, "对标", 3),
    (Kind::Benchmark, "爆款", 2),
    (Kind::Benchmark, "文风", 2),
    (Kind::Benchmark, "节奏", 2),
    // ---- 资料 ----
    (Kind::Research, "考据", 3),
    (Kind::Research, "资料", 2),
    (Kind::Research, "素材", 2),
    (Kind::Research, "参考文献", 3),
];

/// 强写入动词：出现这些词时，「检查」类的只读结论要让位给生成。
const STRONG_WRITE: &[&str] = &[
    "写本章正文",
    "写第",
    "写一章",
    "落盘",
    "扩写",
    "开写",
    "写一本",
    "写小说",
];

/// 清洗主题句时要丢掉的壳子（按长度降序匹配，长壳先拆）。
const FILLER: &[&str] = &[
    "把设定里的几个文件内容给我写了",
    "把设定里的文件内容给我写了",
    "内容给我写了",
    "给我写了",
    "帮我写",
    "帮忙写",
    "请你",
    "麻烦你",
    "麻烦",
    "帮我",
    "帮我把",
    "给我",
    "我想要",
    "我要",
    "要写",
    "写一下",
    "写了",
    "吧",
    "一下",
];

/// 章节号：`第3章` / `第三章` / `第 003 章` / `第一百零一章`。
fn chapter_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"第\s*([0-9]{1,4}|[零一二三四五六七八九十百两]{1,7})\s*章")
            .expect("章号正则编译失败")
    })
}

/// 不带「第」的阿拉伯章号：`002章` / `2章的正文`。
///
/// 真实事故：用户写「完成002章的正文」，旧正则只认「第002章」，
/// 章号丢失后编排层退回「下一章」，于是写手去改了第 1 章。
/// 只认阿拉伯数字——「写三章」是章数不是第 3 章。
fn bare_chapter_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?:^|[^0-9])([0-9]{1,4})\s*章").expect("裸章号正则编译失败"))
}

/// 明确的「第 N 章」（不带第的裸章号不算）。
fn explicit_chapter_of(text: &str) -> Option<u32> {
    chapter_re()
        .captures(text)
        .and_then(|caps| caps.get(1))
        .and_then(|m| cn_to_u32(m.as_str()))
}

/// 裸阿拉伯章号；排除「共 45 章 / 全 45 章 / 前 45 章」这类数量表达。
fn bare_chapter_of(text: &str) -> Option<u32> {
    let caps = bare_chapter_re().captures(text)?;
    let m = caps.get(1)?;
    // 跳过空格后再看前一个字：「共 45 章 / 前 3 章」是数量，不是第 45/3 章。
    let before = text[..m.start()].chars().rev().find(|c| !c.is_whitespace());
    if matches!(
        before,
        Some('共' | '全' | '总' | '前' | '后' | '这' | '那' | '几' | '多' | '第')
    ) {
        return None;
    }
    let no = m.as_str().parse::<u32>().ok()?;
    (no > 0).then_some(no)
}

/// 口语变体归一：真实事故——「完成第一卷各个章节的细章」里的「细章」不在关键词表，
/// 整句被来源词「大纲」劫持成写整本大纲。同义词在入口统一折算，下游判词只认标准词。
pub fn normalize_lexical(text: &str) -> String {
    text.replace("细章", "细纲").replace("详纲", "细纲")
}

/// 中文数字（1—99 够用）转整数；阿拉伯数字直接解析。
fn cn_num(s: &str) -> Option<u32> {
    if let Ok(n) = s.parse::<u32>() {
        return (n > 0).then_some(n);
    }
    const D: &[(char, u32)] = &[
        ('一', 1),
        ('二', 2),
        ('三', 3),
        ('四', 4),
        ('五', 5),
        ('六', 6),
        ('七', 7),
        ('八', 8),
        ('九', 9),
    ];
    let digit = |c: char| D.iter().find(|(d, _)| *d == c).map(|(_, n)| *n);
    let chars: Vec<char> = s.chars().collect();
    match chars.as_slice() {
        [u] => digit(*u),
        [t, o] if *t == '十' => Some(10 + digit(*o).unwrap_or(0)),
        [u, t] if *t == '十' => digit(*u).map(|u| u * 10 + 10),
        [u, t, o] if *t == '十' => digit(*u).and_then(|u| digit(*o).map(|o| u * 10 + 10 + o)),
        _ => None,
    }
}

/// 整数转中文数字（1—99，用于匹配大纲里的「卷三」写法）。
fn int_to_cn(n: u32) -> Option<String> {
    const D: [&str; 10] = ["", "一", "二", "三", "四", "五", "六", "七", "八", "九"];
    match n {
        0 => None,
        1..=9 => Some(D[n as usize].to_string()),
        10 => Some("十".to_string()),
        11..=19 => Some(format!("十{}", D[(n % 10) as usize])),
        20..=99 => Some(format!(
            "{}十{}",
            D[(n / 10) as usize],
            D[(n % 10) as usize]
        )),
        _ => None,
    }
}

fn volume_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"第\s*([0-9一二三四五六七八九十]{1,3})\s*[卷部]").expect("卷号正则编译失败")
    })
}

/// 裸卷名：「写卷一的正文」「卷二 第31~60章」→ Some(1) / Some(2)。
/// 卷名后必须紧跟「的、，。：《」、空白或句尾，防「开卷有益」这类误命中。
fn volume_bare_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"卷\s*([0-9一二三四五六七八九十]{1,3})(?:[的、，。：《\s]|$)")
            .expect("裸卷名正则编译失败")
    })
}

/// 需求里的卷号：「写第一卷正文」「第2卷各章细纲」「写卷一的正文」→ Some(1) / Some(2)。
pub fn volume_of(request: &str) -> Option<u32> {
    volume_re()
        .captures(request)
        .and_then(|c| cn_num(&c[1]))
        .or_else(|| {
            volume_bare_re()
                .captures(request)
                .and_then(|c| cn_num(&c[1]))
        })
}

fn vol_range_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(\d{1,4})\s*(?:[-–—~～]|到|至)\s*(?:第)?(\d{1,4})\s*章")
            .expect("卷章区间正则编译失败")
    })
}

/// 本卷起始标记：「第N卷 / 第CN卷」直接按卷号匹配；
/// 裸「卷N」把完整数字串捕获下来（捕获组 1），由 [`volume_target_line`] 比对卷号，
/// 防「卷1」误中「卷10」、「卷十」误中「卷十二」。
fn volume_target_re(volume: u32, cn: &str) -> Option<Regex> {
    Regex::new(&format!(
        r"(?:第\s*(?:{volume}|{cn})\s*[卷部]|卷\s*([0-9一二三四五六七八九十]{{1,3}}))"
    ))
    .ok()
}

/// 这一行是不是目标卷的标记行。
fn volume_target_line(re: &Regex, volume: u32, line: &str) -> bool {
    match re.captures(line) {
        None => false,
        // 无捕获组 = 「第N卷 / 第CN卷」形式，正则本身已按卷号匹配。
        Some(c) => match c.get(1) {
            None => true,
            Some(m) => cn_num(m.as_str()) == Some(volume),
        },
    }
}

/// 任意卷的标记（段边界）：扫描窗口里碰到别的卷的标记行就立即停止。
fn volume_marker_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"第\s*[0-9一二三四五六七八九十]{1,3}\s*[卷部]|卷\s*[0-9一二三四五六七八九十]{1,3}",
        )
        .expect("卷标记正则编译失败")
    })
}

/// 从全书大纲文本里找第 `volume` 卷的章号区间（如「卷一《入职》：第1—30章」→ (1,30)）。
///
/// 卷标记可能单独成行、区间写在下一行，所以命中卷标记后最多往下看 5 行；
/// 遇到别的卷的标记就停，防止把下一卷的区间当成本卷的。
/// 区间必须带「章」后缀——「D1—D30」这种天数写法不会误命中。
pub fn parse_volume_range(outline: &str, volume: u32) -> Option<(u32, u32)> {
    let cn = int_to_cn(volume)?;
    let target = volume_target_re(volume, &cn)?;
    let lines: Vec<&str> = outline.lines().collect();
    let start = lines
        .iter()
        .position(|l| volume_target_line(&target, volume, l))?;
    for line in &lines[start..lines.len().min(start + 5)] {
        if !volume_target_line(&target, volume, line) && volume_marker_re().is_match(line) {
            break;
        }
        if let Some(c) = vol_range_re().captures(line) {
            if let (Some(s), Some(e)) = (c[1].parse::<u32>().ok(), c[2].parse::<u32>().ok()) {
                if s > 0 && s <= e && e <= 5000 {
                    return Some((s, e));
                }
            }
        }
    }
    None
}

/// 卷区间缺失时的补救任务：只补该卷的章节规划，不许重写整本大纲。
///
/// 真实事故：「写第一卷正文」因为大纲没写卷章区间，缩水成只写第 001 章。
/// 正确姿势是先把大纲的卷规划补上章数，下一次请求就能整卷批量。
pub fn volume_plan_task(volume: u32, genre: Option<&str>) -> String {
    let topic = genre
        .map(|g| g.trim())
        .filter(|g| !g.is_empty())
        .map(|g| g.to_string())
        .unwrap_or_else(|| "以 设定/题材定位.md 为准".to_string());
    format!(
        "【补卷章节规划】第 {volume} 卷的章数区间在 大纲/大纲.md 里没写明（题材：{topic}），\
         「写第{volume}卷正文」无法定位要写哪些章。先读 大纲/大纲.md 与 设定/，\
         在大纲的卷结构处为第 {volume} 卷补上：章号区间（如：共30章，编号1—30）、总章数、\
         每章一句话标题与该章推进的主线点（章级规划即可，不展开三幕细纲）。\
         【文件锁定】只允许修改 大纲/大纲.md 中第 {volume} 卷的卷结构段落；\
         其它卷的规划、既有正文与细纲一律不动，绝不重写整本大纲。"
    )
}

/// 小说平台域名：需求里贴的链接命中这些站，就按「对标采集」处理。
const NOVEL_SITE_DOMAINS: &[&str] = &[
    "qidian.com",
    "zongheng.com",
    "jjwxc.net",
    "fanqienovel.com",
    "fanqie.com",
    "qimao.com",
    "shuqi.com",
    "book.qq.com",
    "yuewen.com",
];

fn url_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"https?://[^\s<>"'（）()，。；、]+"#).expect("URL 正则编译失败"))
}

/// 提取需求里的小说平台链接（去重、保序、剥掉尾部标点）。
pub fn novel_urls(request: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for m in url_re().find_iter(request) {
        let url = m
            .as_str()
            .trim_end_matches(['/', '.', '?', '!', ':', '。', '，']);
        let lower = url.to_ascii_lowercase();
        if NOVEL_SITE_DOMAINS.iter().any(|d| lower.contains(d)) && !out.iter().any(|u| u == url) {
            out.push(url.to_string());
        }
    }
    out
}

/// 题材：`写一本(古代的农民起事)的小说` / `(古代农民起事)题材`。
fn genre_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"写(?:一本|一部|个)?\s*([^，。,.！!？?\s]{2,20}?)\s*的?\s*(?:小说|故事|长篇|网文)",
        )
        .expect("题材正则编译失败")
    })
}

/// 中文数字转 u32（支持 十/百/千 进位与「零」隔位，够解析章号；阿拉伯数字直接透传）。
fn cn_to_u32(raw: &str) -> Option<u32> {
    if let Ok(no) = raw.parse::<u32>() {
        return Some(no);
    }
    const DIGITS: [(char, u32); 11] = [
        ('零', 0),
        ('一', 1),
        ('二', 2),
        ('两', 2),
        ('三', 3),
        ('四', 4),
        ('五', 5),
        ('六', 6),
        ('七', 7),
        ('八', 8),
        ('九', 9),
    ];
    let mut total = 0u32;
    let mut current = 0u32;
    for ch in raw.chars() {
        if ch == '十' || ch == '百' || ch == '千' {
            let unit = match ch {
                '十' => 10u32,
                '百' => 100,
                _ => 1000,
            };
            // 「一百」「十」这种缺位写法按 1 处理。
            let base = if current == 0 { 1 } else { current };
            total += base * unit;
            current = 0;
        } else if let Some((_, value)) = DIGITS.iter().find(|(c, _)| *c == ch) {
            // 「零」只占位不累加：「一百零一」在 百 之后 current 已归零。
            if *value > 0 {
                current = current * 10 + value;
            }
        } else {
            return None;
        }
    }
    let out = total + current;
    (out > 0).then_some(out)
}

/// 需求里写到的章号（「第3章」「第三章」「002章」都认）。
pub fn chapter_of(text: &str) -> Option<u32> {
    explicit_chapter_of(text).or_else(|| bare_chapter_of(text))
}

/// 这句话是不是在**写正文**，哪怕同时提到了大纲、细纲、设定这些名词。
///
/// 真实事故：「根据大纲、细纲、设定完成002章的正文」被判成「排大纲 / 细纲」，
/// 因为「大纲 + 细纲 + 设定」的权重压过了「正文」。判断依据必须是**动作对象**：
/// 有「正文 / 本章 / 这一章」这个宾语，又有写、完成、续写这类动作，就是写正文；
/// 只有「写细纲 / 生成大纲 / 排章纲」这种明确以大纲为宾语的请求才留给大纲意图。
fn targets_prose(text: &str) -> bool {
    let has_prose_object = ["正文", "本章", "这一章", "下一章"]
        .iter()
        .any(|k| text.contains(k));
    if !has_prose_object {
        return false;
    }
    let has_write_verb = [
        "写", "完成", "续写", "扩写", "落盘", "码字", "成稿", "产出", "输出", "开写", "动笔",
        "补写", "重写",
    ]
    .iter()
    .any(|k| text.contains(k));
    if !has_write_verb {
        return false;
    }
    // 明确以大纲为宾语（「写第3章细纲」「生成大纲」）时，不抢大纲的活。
    let writes_outline = ["细纲", "大纲", "章纲", "卷纲"].iter().any(|word| {
        text.contains(&format!("写{word}"))
            || text.contains(&format!("生成{word}"))
            || text.contains(&format!("重写{word}"))
            || text.contains(&format!("排{word}"))
            || text.contains(&format!("补{word}"))
    });
    !writes_outline
}

/// 「这一轮只做细纲，不写正文」——判意图时不许被「正文」这个词抢走。
///
/// 真实事故（《隐藏条款》331—350）：用户请求经规格化后是这样一串
/// 【本次只做一件事】章级细纲 /【字数】写进各章细纲的「正文目标字数」/
/// 【不要做】不写正文……。`targets_prose` 只认词、不认否定，看到「正文」+「写」
/// 就把整句判成写正文：计划里凭空多出 20 个逐章正文步，细纲只排了第一块 5 章，
/// 收尾从句还被钉在第 335 章。判意图必须先把「只做细纲」这种显式否定读出来。
pub fn wants_outline_only(text: &str) -> bool {
    // 【不要做】里同时写着「不写正文」和「不写大纲」时，这条判定不能再成立：
    // 真实事故（【本次只做一件事】设定）：请求里的「大纲」两个字只出现在否定句里，
    // 这里却拿它当「产物确实是大纲」的证据，于是整条请求被判成「只做大纲」，
    // 规划层随即在用户唯一要的 settings 步前面插了一步「【增量补大纲】」。
    // 判「只做大纲」必须先确认大纲本身没被禁止——细纲与全书大纲分开判。
    if forbids(text, Kind::Outline) || forbids_chapter_outline(text) {
        return false;
    }
    let forbids_prose = [
        "不写正文",
        "不要写正文",
        "别写正文",
        "不用写正文",
        "不动正文",
        "只写细纲",
        "只做细纲",
        "只产细纲",
        "只要细纲",
        "仅写细纲",
        "只生成细纲",
        "只要大纲",
        "只做大纲",
    ]
    .iter()
    .any(|k| text.contains(k));
    if !forbids_prose {
        return false;
    }
    // 产物必须确实是细纲/大纲，否则「不要写正文，把这章的对白改一下」会被误判成排细纲。
    ["细纲", "大纲", "章纲"]
        .iter()
        .any(|k| text.contains(k))
}

/// 【不要做】里的正文否定句。
///
/// 只覆盖正文与大纲两类产物，不覆盖设定：真实请求里那句
/// 「不改世界观势力角色之外的其他设定」中的「不改世界观」是**允许**改动的范围，
/// 词面否定扫描会把它读成「禁止设定」，反而丢掉用户真正要的那一步。
/// 正文与大纲才是会被自动派生出来的产物，所以否定识别只做这两类。
const PROSE_FORBID: [&str; 8] = [
    "不写正文",
    "不要写正文",
    "别写正文",
    "不用写正文",
    "不动正文",
    "不改正文",
    "不碰正文",
    "不写本章",
];

/// 全书大纲层的否定句（`大纲/大纲.md`、卷纲）。
const OUTLINE_FORBID: [&str; 10] = [
    "不写大纲",
    "不要大纲",
    "别写大纲",
    "不用大纲",
    "不动大纲",
    "不改大纲",
    "不碰大纲",
    "不补大纲",
    "不补充大纲",
    "不排大纲",
];

/// 章级细纲层的否定句（`大纲/细纲_第N章.md`、章纲）。
///
/// 与全书大纲分开：用户说「不要细纲」不代表不要全书大纲，反过来也一样。
const CHAPTER_OUTLINE_FORBID: [&str; 8] = [
    "不写细纲",
    "不要细纲",
    "不做细纲",
    "不动细纲",
    "不改细纲",
    "不补细纲",
    "不写章纲",
    "不要章纲",
];

/// 否定扫描结果：请求里被明确禁止的产物类别。
///
/// 只算正文与大纲两类。设定故意不算：真实请求里那句「不改世界观势力角色之外的其他设定」
/// 说的是改动范围，语义上恰恰是允许改设定；词面否定一扫，用户真正要的那一步就没了。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Negated {
    pub prose: bool,
    pub outline: bool,
    pub chapter_outline: bool,
}


/// 新的动作动词：命中即结束上一条的否定状态（「不写正文，把大纲补全」里的大纲不算被禁）。
const POS_EDIT_VERBS: [&str; 20] = [
    "写", "生成", "补", "补充", "更新", "改", "修改", "扩写", "整理", "输出", "给出",
    "重写", "继续", "做", "排", "加", "新增", "检查", "核对", "沿用",
];

/// 正文类产物。
const PROSE_DOMAIN: [&str; 7] = ["正文", "本章", "这一章", "章节正文", "稿子", "初稿", "成稿"];

/// 全书/卷级大纲（大纲/大纲.md）。章纲不在此列：它是章级产物，见下一张表。
const OUTLINE_DOMAIN: [&str; 6] = ["大纲", "卷纲", "章节规划", "故事结构", "总纲", "书纲"];

/// 章级细纲（大纲/细纲_第N章.md、章纲）。
const CHAPTER_OUTLINE_DOMAIN: [&str; 3] = ["细纲", "章纲", "纲目"];

/// 段标题是不是「禁止段」：【不要做】【禁止】【不做】这类标题下面的整段都算否定作用域。
/// 标题段一律按「不许产出」处理——它写的是这一轮不要做什么。
fn header_negation(title: &str) -> NegKind {
    if ["不要做", "不要写", "禁止", "不做", "别做", "禁区"]
        .iter()
        .any(|k| title.contains(k))
    {
        NegKind::Produce
    } else {
        NegKind::None
    }
}

/// 强分隔：句子边界，否定状态回到段落的默认值。
fn is_strong_sep(c: char) -> bool {
    matches!(c, '。' | '；' | ';' | '！' | '!' | '？' | '?' | '\n')
}

/// 弱分隔：条目边界，否定状态在条目之间继承（「不写正文、细纲、大纲」）。
fn is_weak_sep(c: char) -> bool {
    matches!(c, '，' | ',' | '、' | '：' | ':' | ' ' | '\t')
}

/// 否定分两种，语义完全不同：
/// 「不写大纲」是**不许产出**大纲；「不改动大纲」是**不许改动既有**大纲文件——
/// 后者在「写第331—350章细纲」这种请求里恰恰是允许的（不动 大纲/大纲.md，但要写新的细纲）。
/// 混为一谈会把真实请求判成「这次不要大纲」，然后删掉用户点名要的那一步。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NegKind {
    None,
    /// 不许产出。
    Produce,
    /// 不许改动既有文件。
    Modify,
}

/// 不许产出的动词。
const NEG_PRODUCE_VERBS: [&str; 16] = [
    "不写", "不要写", "别写", "不用写", "不需要写", "不生成", "不要生成", "不新增", "不加",
    "不做", "不要做", "不排", "不补", "不补充", "禁止", "不必",
];

/// 不许改动的动词。
const NEG_MODIFY_VERBS: [&str; 6] = ["不改", "不要改", "不动", "不碰", "不要碰", "不修改"];

/// 条目开头的动词决定本条是哪种否定；没有动词（纯名词条目）就继承上一条的状态。
fn item_is_negated(t: &str) -> Option<NegKind> {
    if NEG_PRODUCE_VERBS.iter().any(|v| t.starts_with(v)) {
        Some(NegKind::Produce)
    } else if NEG_MODIFY_VERBS.iter().any(|v| t.starts_with(v)) {
        Some(NegKind::Modify)
    } else if POS_EDIT_VERBS.iter().any(|v| t.starts_with(v)) {
        Some(NegKind::None)
    } else {
        None
    }
}

/// 结算一个条目：更新否定状态，只把「不许产出」命中产物类别记进结果。
fn flush_item(item: &str, state: NegKind, out: &mut Negated) -> NegKind {
    let t = item.trim();
    if t.is_empty() {
        return state;
    }
    let now = item_is_negated(t).unwrap_or(state);
    if now == NegKind::Produce {
        if PROSE_DOMAIN.iter().any(|w| t.contains(w)) {
            out.prose = true;
        }
        if OUTLINE_DOMAIN.iter().any(|w| t.contains(w)) {
            out.outline = true;
        }
        if CHAPTER_OUTLINE_DOMAIN.iter().any(|w| t.contains(w)) {
            out.chapter_outline = true;
        }
    }
    now
}

/// 扫一遍请求，读出被禁止的产物。
pub fn negation_scan(text: &str) -> Negated {
    let mut out = Negated::default();
    let chars: Vec<char> = text.chars().collect();
    let mut section_negating = NegKind::None;
    let mut negating = NegKind::None;
    let mut item = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '【' {
            // 段标题会重设状态，这里的返回值不用。
            flush_item(&item, negating, &mut out);
            item.clear();
            let mut title = String::new();
            let mut j = i + 1;
            while j < chars.len() && chars[j] != '】' {
                title.push(chars[j]);
                j += 1;
            }
            section_negating = header_negation(&title);
            negating = section_negating;
            i = if j < chars.len() { j + 1 } else { j };
        } else if is_strong_sep(c) {
            flush_item(&item, negating, &mut out);
            item.clear();
            negating = section_negating;
            i += 1;
        } else if is_weak_sep(c) {
            negating = flush_item(&item, negating, &mut out);
            item.clear();
            i += 1;
        } else {
            item.push(c);
            i += 1;
        }
    }
    flush_item(&item, negating, &mut out);
    out
}

/// 取出某个【标题】段落的正文（到下一个标题为止）。
fn section_body(text: &str, names: &[&str]) -> Option<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '【' {
            i += 1;
            continue;
        }
        let mut title = String::new();
        let mut j = i + 1;
        while j < chars.len() && chars[j] != '】' {
            title.push(chars[j]);
            j += 1;
        }
        let start = if j < chars.len() { j + 1 } else { j };
        let mut k = start;
        while k < chars.len() && chars[k] != '【' {
            k += 1;
        }
        if names.iter().any(|n| title.contains(n)) {
            return Some(chars[start..k].iter().collect());
        }
        i = k;
    }
    None
}

/// 【产出】段里点名的文件决定这次要交付什么。
///
/// 显式产物路径是用户写死的语义，比关键词打分硬：请求里写着
/// 【产出】设定/世界观.md、设定/势力.md、设定/角色.md 时，这次就是补设定，
/// 哪怕正文里还出现过「大纲」两个字（真实事故第三、四次复现）。
/// 只有单一一类时才采纳；跨类别（既写设定又写大纲）留给打分判定。
fn deliverable_kind(text: &str) -> Option<Kind> {
    let body = section_body(text, &["产出", "交付物", "产物"])?;
    let settings = body.contains("设定/") || body.contains("设定\\");
    let outline = body.contains("大纲/") || body.contains("大纲\\");
    let prose = body.contains("正文/") || body.contains("正文\\");
    match (settings, outline, prose) {
        (true, false, false) => Some(Kind::Settings),
        (false, true, false) => Some(Kind::Outline),
        (false, false, true) => Some(Kind::Prose),
        _ => None,
    }
}

/// 用户是不是**明确不要**某一类产物（【不要做】不写正文 / 不写大纲）。
///
/// 只对正文与大纲有意义：它们是「按意图自动派生」的两类步骤，
/// 关键词打分只认词、不认否定，所以计划层必须靠这里把禁止项认出来。
pub fn forbids(text: &str, kind: Kind) -> bool {
    let words: &[&str] = match kind {
        Kind::Prose => &PROSE_FORBID,
        Kind::Outline => &OUTLINE_FORBID,
        _ => return false,
    };
    if words.iter().any(|word| text.contains(word)) {
        return true;
    }
    // 列举报（「不写正文、细纲、大纲」）词表认不出来：一个动词带着三个并列宾语。
    // 真实事故第三次复现就是这种写法，必须再走一遍作用域扫描。
    let scan = negation_scan(text);
    match kind {
        Kind::Prose => scan.prose,
        Kind::Outline => scan.outline,
        _ => false,
    }
}

/// 用户是不是明确不要章级细纲。
pub fn forbids_chapter_outline(text: &str) -> bool {
    CHAPTER_OUTLINE_FORBID.iter().any(|word| text.contains(word))
        || negation_scan(text).chapter_outline
}

/// 「本次只做一件事」：需求里带了这条硬边界时，计划里只能有一类产物。
///
/// 规格化后的需求把这句话原样写在第一行（见 engine.rs 的规划 prompt），
/// 但规划层以前从不读它，模型或规则闸多派的步照样执行。
pub fn only_one_thing(text: &str) -> bool {
    [
        "【本次只做一件事】",
        "只做一件事",
        "这一轮只做",
        "本轮只做",
        "这一次只做",
    ]
    .iter()
    .any(|k| text.contains(k))
}

/// 数字原子：阿拉伯或中文数字（含十/百/千进位），区间与章数正则共用。
const NUM_ATOM: &str = r"[0-9]{1,4}|[零一二三四五六七八九十百千两]{1,7}";

/// 章号区间：`001-045` / `第1到5章` / `10~20章` / `5至12章细纲` /
/// `第一百零一章到一百一十章`。两端中阿数字可混用。
///
/// 必须带「章 / 细纲」后缀——没有后缀的 `2020到2023年` 是年份不是章号，
/// 以前没有任何区间解析，「开始第一卷的 001-045 的细纲」直接缩水成第 1 章。
fn range_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // 尾章号与「章」之间允许一段括号注记：用户真实写法有「第1—40(030)章」
        // 「第1—40（001—040）章」，不容忍就会丢区间，整卷细纲退化成单章任务。
        Regex::new(&format!(
            r"(?:第\s*)?({num})\s*(?:章)?\s*(?:[-–—~～]|到|至)\s*(?:第\s*)?({num})\s*(?:[（(][^）)]{{1,16}}[）)])?\s*(?:章|的?\s*细纲)",
            num = NUM_ATOM
        ))
        .expect("章号区间正则编译失败")
    })
}

/// 「修改一下每个章节的规定，对白改为25%—35%」= 批量格式修订，不是创作。
/// 名词（规定/规则/占比…）+ 动词（修改/改成/统一…）同时命中才算。
pub fn wants_rule_edit(text: &str) -> bool {
    let noun = ["规定", "规则", "规范", "要求", "占比", "比例"]
        .iter()
        .any(|k| text.contains(k));
    let verb = ["修改", "改成", "改为", "调整", "统一", "换成"]
        .iter()
        .any(|k| text.contains(k));
    noun && verb
}

/// 「七个卷每个章节的细纲」「全部章节的正文」= 全书每一章都要做。
pub fn wants_all_chapters(text: &str) -> bool {
    [
        "每个章节",
        "每一章节",
        "所有章节",
        "全部章节",
        "每一章",
        "各章节",
        "每个章",
        "所有章",
    ]
    .iter()
    .any(|k| text.contains(k))
}

/// 大纲文本里所有章号区间的并集（最小起点、最大终点）：
/// 「写七个卷每个章节的细纲」按各卷区间换算出全书 1—280。
pub fn total_chapter_range(outline: &str) -> Option<(u32, u32)> {
    let mut best: Option<(u32, u32)> = None;
    for caps in range_re().captures_iter(outline) {
        let (Some(s), Some(e)) = (
            caps.get(1).and_then(|m| cn_to_u32(m.as_str())),
            caps.get(2).and_then(|m| cn_to_u32(m.as_str())),
        ) else {
            continue;
        };
        if s == 0 || e < s || e > 2000 {
            continue;
        }
        best = Some(match best {
            Some((bs, be)) => (bs.min(s), be.max(e)),
            None => (s, e),
        });
    }
    best
}

/// 需求里写到的章号区间（起点 ≥ 1 且 ≤ 终点）。
pub fn chapter_range_of(text: &str) -> Option<(u32, u32)> {
    let caps = range_re().captures(text)?;
    let start = cn_to_u32(caps.get(1)?.as_str())?;
    let end = cn_to_u32(caps.get(2)?.as_str())?;
    if start == 0 || end < start || end > 2000 {
        return None;
    }
    Some((start, end))
}

/// 「从第15章开始写10章」「接着第100章往后写5章」→ 换算成区间。
///
/// 「开始 / 起」= 含起点；「往后 / 之后 / 以后」= 从下一章起。
fn start_count_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(&format!(
            r"(?:从|自|接着?)\s*第?\s*({num})\s*章\s*(开始|往后|之后|以后|起)[^。，,；;]{{0,10}}?({num})\s*章",
            num = NUM_ATOM
        ))
        .expect("起点+章数正则编译失败")
    })
}

fn start_count_range_of(text: &str) -> Option<(u32, u32)> {
    let caps = start_count_re().captures(text)?;
    let base = cn_to_u32(caps.get(1)?.as_str())?;
    let count = cn_to_u32(caps.get(3)?.as_str())?;
    if count == 0 || count > 500 {
        return None;
    }
    let forward = matches!(caps.get(2)?.as_str(), "往后" | "之后" | "以后");
    let start = if forward { base + 1 } else { base };
    let end = start + count - 1;
    (end <= 2000).then_some((start, end))
}

/// 「接着上次写3章」「写最新的5章」「写5章正文」→ 要写的章数（起点 = 现有最大章号 + 1，
/// 由调用方用 `next_chapter_no` 换算成具体区间；intent 层保持纯函数不读盘）。
fn relative_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(&format!(
            r"(?:最新|最近|接下来|接着|继续|上次|后面|后续|再来|写)[^0-9零一二三四五六七八九十百两]{{0,8}}?({num})\s*(?:章|话)",
            num = NUM_ATOM
        ))
        .expect("相对章数正则编译失败")
    })
}

fn relative_count_of(text: &str) -> Option<u32> {
    let caps = relative_re().captures(text)?;
    let count = cn_to_u32(caps.get(1)?.as_str())?;
    (count >= 1 && count <= 500).then_some(count)
}

/// 字数区间：`6000-8000字` / `六千到八千字`（后缀必须是「字」，不会和章号区间打架）。
fn word_range_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(&format!(
            r"({num})\s*(?:[-–—~～]|到|至)\s*({num})\s*字",
            num = NUM_ATOM
        ))
        .expect("字数区间正则编译失败")
    })
}

/// 单值字数：`每章3000字` / `8000字以上` / `不少于2000字` / `5000字以内` / `4000字左右`。
fn word_single_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(&format!(
            r"(不少于|至少|不超过|不多于)?\s*({num})\s*字\s*(以上|以内|左右|上下)?",
            num = NUM_ATOM
        ))
        .expect("字数正则编译失败")
    })
}

/// 需求里规定的字数范围（正文用）。
///
/// 用户规定多少就是多少——真实事故：明确要求「每章纯字数 6000-8000 字」，
/// 任务模板里却没有这个数，第 3 章只写出 5956 字。区间原样透传；单值按限定词给容差：
/// 「以上/不少于/至少」上浮 50%、「以内/不超过」下探 25%、「左右/上下」±10%、
/// 裸数值 ±20%。低于 300 字的不算字数指标（「改两三个字」不是写作要求）。
pub fn word_count_of(text: &str) -> Option<(u32, u32)> {
    if let Some(caps) = word_range_re().captures(text) {
        let lo = cn_to_u32(caps.get(1)?.as_str())?;
        let hi = cn_to_u32(caps.get(2)?.as_str())?;
        if lo >= 300 && hi > lo && hi <= 100_000 {
            return Some((lo, hi));
        }
    }
    let caps = word_single_re().captures(text)?;
    let n = cn_to_u32(caps.get(2)?.as_str())?;
    if n < 300 || n > 100_000 {
        return None;
    }
    let qualifier = caps
        .get(1)
        .map(|m| m.as_str())
        .or_else(|| caps.get(3).map(|m| m.as_str()))
        .unwrap_or("");
    let out = match qualifier {
        "以上" | "不少于" | "至少" => (n, (n / 2 + n).min(100_000)),
        "以内" | "不超过" | "不多于" => (n * 3 / 4, n),
        "左右" | "上下" => (n * 9 / 10, n * 11 / 10),
        _ => (n * 4 / 5, n * 6 / 5),
    };
    Some(out)
}

/// 从任务文本里的「字数硬要求」从句**成对**恢复用户的字数范围。
///
/// [`word_count_of`] 是给用户口语用的：对「不少于 6000 字、不超过 8000 字」这种从句，
/// 单值容差会把 hi 估成 9000。确认执行路径拿不到用户原话时，必须用这个成对抽取，
/// 否则自动续写的目标字数就是错的。
pub fn clause_word_count_of(text: &str) -> Option<(u32, u32)> {
    static MIN_RE: OnceLock<Regex> = OnceLock::new();
    static MAX_RE: OnceLock<Regex> = OnceLock::new();
    let min_re = MIN_RE.get_or_init(|| {
        Regex::new(&format!(r"(?:不少于|至少)\s*({num})\s*字", num = NUM_ATOM))
            .expect("字数下限正则")
    });
    let max_re = MAX_RE.get_or_init(|| {
        Regex::new(&format!(
            r"(?:不超过|不多于)\s*({num})\s*字",
            num = NUM_ATOM
        ))
        .expect("字数上限正则")
    });
    let lo = min_re
        .captures_iter(text)
        .filter_map(|c| cn_to_u32(c.get(1)?.as_str()))
        // 任务模板里有「不超过八字的小标题」这种 phrase——数值太小的不算字数指标，
        // 跳过它找真正的「不少于 6000 字」。
        .find(|&n| n >= 300)?;
    let hi = max_re
        .captures_iter(text)
        .filter_map(|c| cn_to_u32(c.get(1)?.as_str()))
        .find(|&n| n > lo)?;
    if hi > lo {
        Some((lo, hi))
    } else {
        None
    }
}

/// 模糊数量信号：出现这些词却拿不到任何数字时，章节范围就是没说清——
/// 必须反问澄清，不许猜区间，更不许缩水成第 1 章。
const VAGUE_QUANTITY: &[&str] = &[
    "几章", "几话", "好几", "一些", "大概", "随便", "写点", "来点", "批量",
];

/// 需求里写到的题材（没有就 `None`）。
pub fn genre_of(text: &str) -> Option<String> {
    genre_re()
        .captures(text)
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str().trim().to_string())
        .filter(|s| !s.is_empty())
}

/// 清洗主题句：去掉「帮我 / 把…给我写了 / 吧」这类口语外壳，压成一句可执行的主题。
///
/// 不做同义改写——改写交给模型；这里只保证「派给 agent 的 task 不再是一整句用户原话」。
pub fn clean_subject(text: &str) -> String {
    let mut out = text.trim().to_string();
    for filler in FILLER {
        out = out.replace(filler, "");
    }
    let out = out
        .chars()
        .filter(|c| !matches!(c, '，' | ',' | '。' | '！' | '!' | '？' | '?' | '；' | ';'))
        .collect::<String>();
    let out = out.split_whitespace().collect::<Vec<_>>().join(" ");
    let out = out.trim().to_string();
    if out.chars().count() > 40 {
        let cut: String = out.chars().take(40).collect();
        format!("{cut}…")
    } else if out.is_empty() {
        text.trim().chars().take(40).collect()
    } else {
        out
    }
}

/// 主判定：给一句话，返回意图 + 置信度 + 证据。
///
/// 判定顺序（先特殊后打分）：
/// 1. 有强写入动词 → 生成类（「写本章正文并检查错别字」是写正文，不是检查）；
/// 2. 只有检查词、没有强写入动词 → 只读检查；
/// 3. 否则按关键词权重表打分，取最高分；平手按 [`Kind`] 的声明顺序（写正文优先）；
/// 4. 最高分 < 3 或与第二名差距 < 2 → 低置信度（调用方只派一步「先读给结论」）。
pub fn classify(request: &str) -> Intent {
    let text = &normalize_lexical(request.trim());
    let explicit_chapter = explicit_chapter_of(text);
    let bare_chapter = bare_chapter_of(text);
    // 绝对区间：「001-045 的细纲」「第一百零一章到一百一十章」「从第15章开始写10章」。
    let chapter_range = start_count_range_of(text).or_else(|| chapter_range_of(text));
    // 相对章数（「接着上次写5章细纲」）：只在拿不到绝对章号时生效——
    // 否则「继续写第十章正文」会被误读成「再写 10 章」。
    //
    // 注意：裸章号（`5章`）会抢在相对章数前面把「写5章细纲」读成「第 5 章」。
    // 因此相对章数先判；命中时忽略裸章号，只保留显式「第 N 章」。
    let pending_count = if chapter_range.is_none() && explicit_chapter.is_none() {
        relative_count_of(text)
    } else {
        None
    };
    let raw_chapter = if pending_count.is_some() {
        explicit_chapter
    } else {
        explicit_chapter.or(bare_chapter)
    };
    // 区间请求里「001-045」的 045 也会被裸章号正则抓到；区间起点才是本章章号。
    let chapter = explicit_chapter
        .or_else(|| chapter_range.map(|(s, _)| s))
        .or(raw_chapter);
    let genre = genre_of(text);
    let word_count = word_count_of(text);
    let subject = clean_subject(text);
    // 「整本」这个词本身不足以判定「整本从头写」：作者说「这 20 章作为**整本书的结尾**」
    // 也会命中，于是计划卡被写成「整本从头写」、还去走重排全书大纲的分支。
    // 只有真正表达「从头写一本」的措辞才算整本。
    let whole_book = ["写一本", "写一部", "从头写", "整本从头", "整本重写"]
        .iter()
        .any(|k| text.contains(k));
    let outline_scope = if chapter_range.is_some() {
        // 带区间的必然是章级批量，绝不能再被判成「整本大纲」。
        OutlineScope::Chapter
    } else {
        outline_scope_of(text, raw_chapter)
    };
    // 动作对象是正文时，大纲/设定这些名词只是依据，不许抢走意图。
    // 但「不写正文」也是「正文 + 写」——否定扫描命中时不能算写正文。
    let negated = negation_scan(text);
    let prose_targeted = targets_prose(text) && !negated.prose;

    let mut scores: Vec<(Kind, &'static str, u32)> = Vec::new();
    for (kind, word, weight) in KEYWORDS {
        if text.contains(word) {
            scores.push((*kind, word, *weight));
        }
    }

    let sum = |kind: Kind| -> u32 {
        scores
            .iter()
            .filter(|(k, _, _)| *k == kind)
            .map(|(_, _, w)| *w)
            .sum()
    };
    let has_strong_write = STRONG_WRITE.iter().any(|k| text.contains(k));

    // 1 & 2：检查 / 生成的优先级。
    // 只在「检查词够硬、而且压过其它意图」时才短路成只读——否则
    // 「生成第 1 章细纲：目标、冲突、转折」里那个「冲突」会把整句判成检查。
    let check_score = sum(Kind::Check);
    let prose_score = sum(Kind::Prose);
    let other_max = [
        Kind::Outline,
        Kind::Settings,
        Kind::Benchmark,
        Kind::Research,
    ]
    .into_iter()
    .map(sum)
    .max()
    .unwrap_or(0);

    // 阶段 11.6：「核对一下人物设定」这类请求，检查动词优先——只要检查信号
    // 不比别的意图弱太多（差 2 分以内）就判只读，否则「设定/人物」的名词权重
    // 会把「核对设定」吞成补设定。
    // 【产出】点名了 设定/ 或 大纲/ 或 正文/ 下的文件时，用户已经把交付物写死了：
    // 这类显式路径优先于关键词打分，也优先于「只做大纲」的启发式。
    let deliverable = deliverable_kind(text);
    let (kind, confidence) = if let Some(pinned) = deliverable {
        (pinned, Confidence::High)
    } else if wants_outline_only(text) {
        // 「只做细纲，不写正文」优先于正文关键词：否则会凭空派出一批正文步。
        (Kind::Outline, Confidence::High)
    } else if prose_targeted {
        // 「根据大纲、细纲、设定完成002章的正文」：依据不是任务，正文才是。
        (Kind::Prose, Confidence::High)
    } else if check_score >= 3
        && !has_strong_write
        && prose_score == 0
        && check_score + 3 > other_max
    {
        (Kind::Check, Confidence::High)
    } else {
        let mut ranked: Vec<(Kind, u32)> = [
            Kind::Prose,
            Kind::Check,
            Kind::Outline,
            Kind::Settings,
            Kind::Benchmark,
            Kind::Research,
        ]
        .into_iter()
        .map(|k| (k, sum(k)))
        .filter(|(_, score)| *score > 0)
        .collect();
        ranked.sort_by(|a, b| b.1.cmp(&a.1));

        match ranked.split_first() {
            None => (Kind::Unknown, Confidence::Low),
            Some((top, rest)) => {
                let margin = top.1 - rest.first().map(|(_, score)| *score).unwrap_or(0);
                let kind = top.0;
                // 「写一部小说」这种整本请求，除非用户点名细纲，一律按写正文处理。
                let kind = if whole_book && kind == Kind::Settings && prose_score > 0 {
                    Kind::Prose
                } else {
                    kind
                };
                // 整本从头写时不必再跟「设定」这种词掰手腕：作者说「写一本…小说」就是要正文。
                let confidence = if kind == Kind::Prose && whole_book && prose_score >= 3 {
                    Confidence::High
                } else if top.1 >= 3 && margin >= 2 {
                    Confidence::High
                } else {
                    Confidence::Low
                };
                (kind, confidence)
            }
        }
    };

    // 证据按分数降序，日志里一眼能看出为什么这么判
    scores.sort_by(|a, b| b.2.cmp(&a.2));

    // 显式数字模式兜底：「从第15章开始写10章」「写接下来5章」没有任何意图关键词，
    // 但「写 N 章」就是要写正文，不该掉进「没看懂」瞎澄清。
    let derived_prose = kind == Kind::Unknown
        && (pending_count.is_some() || (chapter_range.is_some() && text.contains('写')));
    let kind = if derived_prose { Kind::Prose } else { kind };
    let confidence = if derived_prose {
        Confidence::High
    } else {
        confidence
    };

    // 对标采集：贴了小说平台链接，这一步就是「抓公开章节 + 拆解」，别的词不许抢。
    // 唯一让位的是写正文（「照着这本写第 3 章」判给 Prose 更稳，采集交给用户单说）。
    let collect = !novel_urls(text).is_empty() && kind != Kind::Prose;
    let kind = if collect { Kind::Benchmark } else { kind };
    let confidence = if collect {
        Confidence::High
    } else {
        confidence
    };
    if collect {
        scores.push((Kind::Benchmark, "书籍链接", 3));
    }

    // 模糊章数：说了要章级产物却没有任何可定位的数字——必须反问，
    // 不许猜区间，更不许缩水成第 1 章（真实事故：「大概几章吧」被默认成第 1 章）。
    let needs_clarification = !whole_book
        && chapter_range.is_none()
        && pending_count.is_none()
        && raw_chapter.is_none()
        && matches!(kind, Kind::Outline | Kind::Prose)
        && !(kind == Kind::Outline && outline_scope == OutlineScope::Book)
        && VAGUE_QUANTITY.iter().any(|w| text.contains(w));

    Intent {
        kind,
        confidence,
        chapter,
        chapter_range,
        pending_count,
        word_count,
        needs_clarification,
        genre,
        subject,
        whole_book,
        supplement: kind == Kind::Outline && wants_supplement(text),
        outline_scope,
        hits: scores,
        // 唯一语义出口：收尾 / 改规矩都只在这里判一次，别处读字段。
        ending: is_ending_request(text),
        rule_edit: wants_rule_edit(text),
    }
}

/// 大纲层级判定：
/// - 明说「细纲 / 章纲」，带章号（「写第3章大纲」），或点名卷号（「第一卷的卷纲」）→ 章级；
/// - 明说「整本故事 / 全书 / 总纲 / 卷纲（没带卷号）/ 故事的大纲」，或只说了「大纲」没点名章 → 全书级
///   （「把大纲写了」默认指整本的大纲；说了「第X卷」就是要这一卷的规划，绝不重写整本）。
fn outline_scope_of(text: &str, chapter: Option<u32>) -> OutlineScope {
    // 明说「整本 / 全书 / 总纲」或点名 大纲/大纲.md：这是全书级目标，
    // 不能被请求里任何一个「细纲」字样压成章级（实机事故：《山海不夜》「全书大纲 /【产出】大纲/大纲.md /
    // 【不要做】不写正文、不写章级细纲」被判成章级 → 模型的整本大纲任务被改写成「第 1 章细纲」）。
    let book_signal = ["全书", "整本", "总纲", "书纲", "大纲/大纲.md"]
        .iter()
        .any(|k| text.contains(k));
    // 「细纲 / 章纲」出现在否定句里时不算目标：「不写章级细纲」是禁止项，不是要做的事。
    let negated = negation_scan(text);
    let chapter_mention =
        (text.contains("细纲") || text.contains("章纲")) && !negated.chapter_outline;
    if chapter_mention || (chapter.is_some() && !book_signal) {
        return OutlineScope::Chapter;
    }
    // 点名了卷号（「第一卷的卷纲」）：目标是这一卷的章节规划，不是整本大纲；
    // 只有明说「全书 / 整本」才压回全书级。
    if volume_of(text).is_some() && !text.contains("全书") && !text.contains("整本") {
        return OutlineScope::Chapter;
    }
    if [
        "整本故事",
        "整本书",
        "全书大纲",
        "总纲",
        "卷纲",
        "故事的大纲",
        "整本的大纲",
        "整本大纲",
        "书的大纲",
        "整体大纲",
    ]
    .iter()
    .any(|k| text.contains(k))
        || text.contains("大纲")
    {
        return OutlineScope::Book;
    }
    OutlineScope::Chapter
}

/// 用户是不是明确要求「沿用现有文件」。
///
/// 阶段 11.6：强生成词（写了 / 写好 / 生成 / 重写）出现时必须重新生成，
/// 「沿用」只留给「按现有细纲写 / 继续写正文」这类明确表态。
pub fn wants_reuse(text: &str) -> bool {
    [
        "按现有",
        "现有的细纲",
        "现有细纲",
        "现有的大纲",
        "现有大纲",
        "按当前",
        "沿用现有",
        "不要覆盖",
        "别覆盖",
        "继续写正文",
    ]
    .iter()
    .any(|k| text.contains(k))
}

/// 用户是不是要「补充没写全的内容」而不是「推倒重写」。
///
/// 「大纲内没有写七个卷每个卷有多少章补充一下」= 补充；
/// 「把大纲重新写一遍」= 重写（出现重新 / 重写 / 推倒这类词时不算补充）。
pub fn wants_supplement(text: &str) -> bool {
    if ["重新", "重写", "重来", "推倒"]
        .iter()
        .any(|w| text.contains(w))
    {
        return false;
    }
    // 「不补充大纲」「大纲先不补」这类禁止项里的「补充」不是要补内容，是明确别补：
    // 否则【不要做】一写「不补充大纲」，计划照样带上「增量补大纲」标记。
    if negation_scan(text).outline {
        return false;
    }
    [
        "补充",
        "补上",
        "补全",
        "补一下",
        "没有写",
        "没写",
        "未写",
        "缺少",
        "缺失",
        "写明",
        "标上",
        "标出",
    ]
    .iter()
    .any(|k| text.contains(k))
}

/// 增量补写全书大纲的任务：只把缺的信息补进 大纲/大纲.md，既有内容一字不动。
///
/// 与 `book_outline_task` 的本质区别：那个是新书启动（旧内容作废），
/// 这个是给已有大纲补缺项（如每卷章数），人物、主线、卷结构必须原样保留。
pub fn outline_patch_task(genre: Option<&str>) -> String {
    let topic = genre
        .map(|g| g.trim())
        .filter(|g| !g.is_empty())
        .map(|g| g.to_string())
        .unwrap_or_else(|| "以 设定/题材定位.md 为准".to_string());
    format!(
        "【增量补大纲】用户说 大纲/大纲.md 里有内容没写全（题材：{topic}），要的是补充、不是重写。\
         先完整读 大纲/大纲.md 与 设定/，只把缺失的信息补进对应位置（例如每个卷的章数与章号区间、\
         全书总章数、卷与卷之间的衔接说明）；既有人物、主线、卷结构、卷末钩子一字不动，\
         绝不重写整本大纲、旧大纲与既有细纲一律不作废。\
         【文件锁定】只允许修改 大纲/大纲.md；不展开章节细纲，绝不改动 正文/ 与 大纲/细纲_第N章.md。\
         【执行铁律】对 大纲/大纲.md 只做最小插入修改，禁止凭记忆整文件重写覆盖既有内容；\
         补充完成后立即停止，不得继续写细纲或正文。"
    )
}

/// 分句里的「条件前缀」：这些词开头是在交代依据（根据什么写），不是要去做这件事。
/// 「根据参考资料」不该凭空多出一步找资料，「根据大纲」不该凭空多出一步排大纲。
const CONTEXT_PREFIXES: &[&str] = &["根据", "按照", "依照", "基于", "结合", "围绕", "参考"];

/// 分句里必须有一个「动作词」才算独立意图。只有名词的碎句（「大纲」「设定」）
/// 多半是列举的上下文（「根据参考资料，大纲，设定，追踪，开始…细纲」），
/// 把它们当成任务就会凭空多出几步——这是实测事故里「计划缩水 + 过度发散」的根源之一。
const ASK_VERBS: &[&str] = &[
    "写", "生成", "补", "重写", "整理", "输出", "给出", "开始", "检查", "核对", "盘点", "梳理",
    "评估", "审查", "对账", "挑错", "拆", "收集", "找", "查", "列出", "分析", "规划", "排", "扩写",
];

/// 这一分句是不是在说「不要做这件事」。
///
/// 只按分句自己的文字判否定，不继承整条请求的状态：并列的否定项
/// （「不写正文、不写章级细纲」）各自成立，但不能连累后面真正要做的分句。
/// 否则计划卡会把禁止项读成任务——「我理解为：先写正文，再排第 1 章细纲」就是这么来的。
fn clause_negated(clause: &str, found: &Intent) -> bool {
    let negated = negation_scan(clause);
    match found.kind {
        Kind::Prose => negated.prose,
        Kind::Outline => {
            if found.outline_scope == OutlineScope::Chapter {
                negated.chapter_outline
            } else {
                negated.outline
            }
        }
        _ => false,
    }
}

/// 多意图拆解：把一句话按分句切开，逐句判定意图，按出现顺序去重返回。
///
/// 阶段 11.6 之前 `classify` 只能返回一个 `Kind`，于是「把大纲写了……再把设定里
/// 这几个都写好」这种顺序多意图被折叠成单一步骤，后面的意图直接丢失。
/// 这里按标点与「再 / 然后」切分，每个分句独立打分，并过三道闸防曲解：
/// - **条件前缀剥离**：「根据参考资料」先剥掉「根据」，剩下的是依据不是任务；
/// - **动词闸**：剥完只剩名词碎句（「大纲」「设定」）不算独立意图；
/// - **同类去重取更具体**：同一 `Kind` 只留一条；后到分句带章号/区间而先到的不带
///   → 换成更具体的那条（真实事故：「大纲」碎句抢走了「001-045的细纲」的区间信息）。
pub fn intent_sequence(request: &str) -> Vec<Intent> {
    // 口语连接词当分隔符：「先 A 然后 B」「写完 A 再写 B」都拆成两个分句。
    let normalized = normalize_lexical(request)
        .replace("然后", "，")
        .replace("再把", "，把")
        .replace("再给", "，给")
        .replace("再补", "，补")
        .replace("再写", "，写")
        .replace("再来", "，来");
    let mut out: Vec<Intent> = Vec::new();
    let mut buffer = String::new();
    let flush = |buffer: &mut String, out: &mut Vec<Intent>| {
        let clause = buffer.trim();
        if !clause.is_empty() {
            // 剥掉条件前缀：「根据参考资料」→「参考资料」。
            let mut rest = clause;
            while let Some((_prefix, tail)) = CONTEXT_PREFIXES
                .iter()
                .find_map(|p| Some((*p, rest.strip_prefix(p)?)))
            {
                if tail.is_empty() {
                    break;
                }
                rest = tail.trim_start();
            }
            // 动词闸：没有动作词的名词碎句是上下文列举，不是任务。
            if ASK_VERBS.iter().any(|v| rest.contains(v)) {
                let found = classify(rest);
                if found.kind != Kind::Unknown && !found.is_low() && !clause_negated(rest, &found) {
                    if let Some(pos) = out.iter().position(|i| i.kind == found.kind) {
                        let specific = found.chapter_range.is_some() || found.chapter.is_some();
                        let old = &out[pos];
                        let old_specific = old.chapter_range.is_some() || old.chapter.is_some();
                        if specific && !old_specific {
                            out[pos] = found;
                        }
                    } else {
                        out.push(found);
                    }
                }
            }
        }
        buffer.clear();
    };
    for ch in normalized.chars() {
        if matches!(
            ch,
            '，' | ',' | '。' | '．' | '！' | '!' | '？' | '?' | '；' | ';' | '、' | '\n' | '\r'
        ) {
            flush(&mut buffer, &mut out);
        } else {
            buffer.push(ch);
        }
    }
    flush(&mut buffer, &mut out);
    out
}

/// 多意图的计划卡描述：「先写整本大纲（大纲/大纲.md），再补全设定（设定/）」。
/// 少于两个意图时返回 `None`，调用方退回单意图的 [`Intent::describe`]。
pub fn describe_sequence(seq: &[Intent]) -> Option<String> {
    if seq.len() < 2 {
        return None;
    }
    let mut text = String::new();
    for (i, it) in seq.iter().enumerate() {
        if i == 0 {
            text.push_str("先");
        } else {
            text.push_str("，再");
        }
        text.push_str(&it.short_label());
    }
    Some(text)
}

/// 计划卡「我理解为」那一句话：多意图按序列说，写正文按阶段 11.5 的老口径说。
pub fn plan_summary(request: &str) -> String {
    let found = classify(request);
    if found.kind != Kind::Prose {
        let mut seq = intent_sequence(request);
        if wants_outline_only(request) {
            // 「不写正文」的分句会在序列里造出一条 Prose 意图，计划卡于是写成
            // 「先写正文，再排第 1 章细纲」，与实际派发的计划自相矛盾（《隐藏条款》331—350 现场）。
            seq.retain(|i| i.kind != Kind::Prose);
        }
        if let Some(multi) = describe_sequence(&seq) {
            return multi;
        }
    }
    found.describe()
}

/// 分步任务：把意图变成「这一步要交付什么」。
///
/// `note` 是给人看的补充（题材、章号），会以「（…）」的形式挂在任务末尾。
pub fn step_task(kind: Kind, subject: &str, chapter: u32, genre: Option<&str>) -> String {
    let extra = match genre {
        Some(g) => format!("（题材：{g}）"),
        None => String::new(),
    };
    match kind {
        Kind::Prose => format!("按第 {chapter} 章细纲写本章正文{extra}"),
        // 阶段 11.6：大纲请求不再默认「沿用」——用户点名要大纲就是要生成，
        // 「沿用」只留给 wants_reuse 命中的场景（调用方会用 outline_task 传 regenerate=false）。
        Kind::Outline => outline_task(chapter, genre, true, "用户点名要大纲", None),
        Kind::Check => format!(
            "【只读模式】{subject}：只输出报告，不要调用 Write 工具，不要修改任何文件"
        ),
        // 阶段 11.6：Settings 意图 = 补设定（写），检查设定一致性会判成 Check 走只读。
        Kind::Settings => settings_fill_task(genre),
        Kind::Benchmark => format!("拆解对标作品的节奏与情绪模块（主题：{subject}）"),
        Kind::Research => format!("收集这一章需要的考据与素材（主题：{subject}）"),
        Kind::Unknown => format!(
            "【只读模式】先读 设定/ 与 大纲/，用三行说清你理解的需求与建议动作，不要动笔（原话：{subject}）"
        ),
    }
}

/// 对标步骤的任务：需求里带了小说平台链接 → 「采集 + 拆解」；没带 → 只拆 对标/ 里已有的文本。
pub fn benchmark_task(request: &str, subject: &str, chapter: u32, genre: Option<&str>) -> String {
    let urls = novel_urls(request);
    if urls.is_empty() {
        return step_task(Kind::Benchmark, subject, chapter, genre);
    }
    benchmark_collect_task(&urls, subject)
}

/// 「对标采集」任务文案：webfetch 抓公开章节 → 落盘 对标/<书名>/ → 出拆解报告。
///
/// 红线写死在任务里：只存平台公开可见的原文；抓不到就如实报告并引导走「导入对标文本」，
/// 绝不许模型凭印象编造章节内容——编出来的拆解比没有拆解更有害。
pub fn benchmark_collect_task(urls: &[String], subject: &str) -> String {
    let list = urls.join("　");
    format!(
        "【对标采集】拆解对标作品（主题：{subject}）。用户给了书籍链接：{list}\n\
         用 webfetch 打开链接，确认书名与作者，抓取**公开免费**的章节（能抓多少抓多少，上限 20 章）：\n\
         1. 每章存为一个文件：对标/<书名>/第NNN章_<章名>.md（NNN 为三位章号），文件头标注来源 URL 与抓取时间；\n\
         2. 抓完输出 对标/<书名>/拆解.md：黄金三章结构、爽点与情绪模块、章节节奏曲线、钩子落点、\
            对白占比与场景密度、可复用的开局与升级套路；\n\
         3. 【采集红线】VIP/付费章节抓不到就跳过；页面打不开就如实报告哪几章没拿到，\
            并提示作者改用「导入对标文本」自己导出 txt 再拆——**严禁凭印象编造原文或章节内容**。"
    )
}

/// 写正文这一步的任务。
///
/// 阶段 11 的版本只写「按第 N 章细纲写本章正文（题材：X）」——实测下来模型照样沿用旧章名、
/// 不读设定、字数不达标。这里把必须发生的事写死进任务：读设定、题材一致、文件名规则，
/// 以及用户规定的字数范围（规定了就必须严格套用，见 [`word_count_clause`]）。
pub fn writer_task(
    chapter: u32,
    genre: Option<&str>,
    word_count: Option<(u32, u32)>,
    constraints: Option<&str>,
) -> String {
    let topic = genre
        .map(|g| g.trim())
        .filter(|g| !g.is_empty())
        .map(|g| g.to_string())
        .unwrap_or_else(|| "以 设定/题材定位.md 为准".to_string());
    let mut task = format!(
        "按第 {chapter} 章细纲写本章正文（题材：{topic}）。\
         必须先完整阅读 设定/ 下的人物、世界观、势力与文风文件，以及 大纲/细纲_第{chapter}章.md，\
         正文里的时代、地名、人物、事件都要与它们一致，禁止套用其它题材的旧稿。\
         文件名用「第{chapter:03}章_<与本章内容相关、不超过八字的小标题>.md」，不要沿用与题材无关的旧章名。\
         【章号锁定】只允许写「第{chapter:03}章_」开头的这一个文件；\
         正文/ 下已有的其它章节不要修改、续写或重写，也绝不转去写别的章号。\
         【对白格式】人物对白一律使用中文弯双引号“”，禁止使用直角引号「」与『』；\
         引号内句末不加句号，用逗号、问号或叹号收束。 【格式】正文不写任何小标题或分节标记（「## 一」「## 场景一」都不要），分场只靠空行与场景转换；章首直接接上一章的落点，不 recap；章末停在具体动作或半句话上，不写设问句与「他不知道的是…」式断言悬念。"
    );
    // 字数：用户规定了就严格套用；没规定且书里也没有硬约束时才给保底从句，
    // 两处字数说法打架会让模型无所适从。用户字数放在最后，作为最高优先级。
    if let Some(extra) = constraints.map(str::trim).filter(|e| !e.is_empty()) {
        let label = if word_count.is_some() {
            "硬约束（比例/风格，不含字数）"
        } else {
            "细纲硬约束"
        };
        task.push_str(&format!("{label}：{extra}"));
    }
    // 细纲硬约束里已经带了全章字数时，不再追加「至少 1500 字」的保底从句，
    // 否则模型会挑更低的那个执行。
    let constraints_has_word_count = constraints.map(|c| c.contains("字")).unwrap_or(false);
    if word_count.is_some() || !constraints_has_word_count {
        task.push_str(&word_count_clause(word_count, false));
    }
    // 滚动记忆第一步：每章完稿留一条结构化摘要，后续章节的前情提要靠它兜住。
    // R1：摘要行只要求在**回复里**给出，不再让写手改 记忆/摘要索引.md——
    // 那份文件是所有章共用的唯一一份跨章记忆，交给模型整文件重写等于没有闸；
    // 现在由编排器在正文验收通过后解析回复、统一 upsert（services/memory.rs）。
    task.push_str(&format!(
        "【章节摘要】写完本章并自查字数后，在回复末尾另起一行给出本章摘要，\
         只给这一行、不要写进任何文件（记忆/摘要索引.md 由编排器统一维护，禁止改动）：\
         第{chapter}章 | 故事时间 | 地点 | 出场人物 | 关键事件（40字内） | 新埋伏笔 | 回收伏笔 | 结尾钩子。\
         人物一律用全名。"
    ));
    task
}

/// 字数硬要求从句。
///
/// - 用户规定了范围（「每章 6000-8000 字」）：原样写进任务，并要求模型写完自查、
///   不足就续写——光写「目标 X 字」模型会提前收尾，实测第 3 章只写出 5956 字。
/// - 没规定：给一条保底要求，避免再写出几百字的「半章」。
pub fn word_count_clause(word_count: Option<(u32, u32)>, per_chapter: bool) -> String {
    let subject = if per_chapter {
        "每章纯正文（不计标点、标题与空白）"
    } else {
        "本章纯正文（不计标点、标题与空白）"
    };
    match word_count {
        Some((lo, hi)) => format!(
            "字数硬要求（最高优先级）：{subject}必须不少于 {lo} 字、不超过 {hi} 字；\
             细纲或大纲里写的任何字数（全章「正文目标字数」与每个场景的「字数建议」、「本章约 N 字」\
             这类旧区间）一律作废、不构成要求，一律以本条用户要求为准；\
             因此不得按细纲的更低区间提前收尾，也不得按细纲的更高区间灌水拉长；\
             写到 {lo} 字之前不要结束本章场景，情节不够就按细纲继续展开动作、环境与对白细节；\
             到了 {hi} 字要及时收束，禁止为凑字数重复灌水；\
             写完自查字数：不足 {lo} 字继续写到达标，超过 {hi} 字就删减枝蔓、合并重复场景压回区间。"
        ),
        None => format!(
            "字数要求：{subject}至少 1500 字，写足一个完整章节的体量，不要交几百字的半章。"
        ),
    }
}

/// 生成 / 重新生成细纲这一步的任务。
///
/// `regenerate` 为真时把「旧细纲作废」写进任务里，避免建筑师看到旧文件又「沿用」。
pub fn outline_task(
    chapter: u32,
    genre: Option<&str>,
    regenerate: bool,
    reason: &str,
    word_count: Option<(u32, u32)>,
) -> String {
    let topic = genre
        .map(|g| g.trim())
        .filter(|g| !g.is_empty())
        .map(|g| g.to_string())
        .unwrap_or_else(|| "以 设定/题材定位.md 为准".to_string());
    if regenerate {
        return format!(
            "重新生成第 {chapter} 章细纲（题材：{topic}）：{reason}。\
             旧细纲的人物与情节一律作废，不要沿用。\
             新细纲必须写清时代与地点、出场人物及其动机、本章核心冲突、按事件推进切分的场景（不标幕次、不给正文写分节标题）、章末落点（落在动作或半句话上，不写设问句与断言式悬念），\
             并给出本章字数区间与对白比例。{}写细纲时开头单起一行写「所属事件」：大纲/事件/ 下确有事件卡、且本章属于其中一张，就写「所属事件：事件 NNN · 标题」；没有事件卡（或本章不属于任何事件）就写「所属事件：无」——没有卡的时候不要自己编事件编号。有卡时本章消耗与新增的未结项必须与事件卡的「章型分工」一致。【章号锁定】只允许写 大纲/细纲_第{chapter:03}章.md 这一个细纲文件；\
             大纲/ 下其它章节的细纲与全书大纲一律不要修改、覆盖或重写。",
            outline_word_count_clause(word_count)
        );
    }
    format!(
        "生成第 {chapter} 章细纲（题材：{topic}）：写清时代与地点、出场人物、核心冲突、按事件推进切分的场景（不标幕次、不给正文写分节标题）、\
         章末落点（落在动作或半句话上，不写设问句与断言式悬念），并给出本章字数区间与对白比例；已有细纲就沿用，不要覆盖。{}写细纲时开头单起一行写「所属事件」：大纲/事件/ 下确有事件卡、且本章属于其中一张，就写「所属事件：事件 NNN · 标题」；没有事件卡（或本章不属于任何事件）就写「所属事件：无」——没有卡的时候不要自己编事件编号。有卡时本章消耗与新增的未结项必须与事件卡的「章型分工」一致。【章号锁定】只允许写 大纲/细纲_第{chapter:03}章.md 这一个细纲文件；\
         大纲/ 下其它章节的细纲与全书大纲一律不要修改、覆盖或重写。",
        outline_word_count_clause(word_count)
    )
}

/// 细纲任务里的字数从句：用户规定了每章字数时，细纲的场景量必须按这个体量设计——
/// 否则细纲按 3000 字设计场景，写手照着细纲也写不到 6000 字。
pub fn outline_word_count_clause(word_count: Option<(u32, u32)>) -> String {
    match word_count {
        Some((lo, hi)) => format!(
            "用户指定每章正文纯字数 {lo} 到 {hi} 字（不计标点与空白）：\
             细纲里的「正文目标字数」必须原样写成 {lo}—{hi} 字，\
             各场景「字数建议」之和必须达到 {lo} 字以上；\
             不得按更低的旧区间设计，也不得把单场字数建议当成全章目标。"
        ),
        None => String::new(),
    }
}

/// 全书大纲这一步的任务（阶段 11.6 新增）。
///
/// 目标文件固定写死 `大纲/大纲.md`，并明确「写到卷级为止」——否则建筑师会把全书大纲
/// 展开成第 1 章细纲，正好是阶段 11.6 要修的降级问题。
pub fn book_outline_task(genre: Option<&str>, regenerate: bool) -> String {
    let topic = genre
        .map(|g| g.trim())
        .filter(|g| !g.is_empty())
        .map(|g| g.to_string())
        .unwrap_or_else(|| "以 设定/题材定位.md 为准".to_string());
    if regenerate {
        return format!(
            "重新生成整本故事的全书大纲，写入 大纲/大纲.md（题材：{topic}）：\
             先完整阅读 设定/ 下的现有设定，旧的大纲与细纲一律作废，其中的人物与情节不得保留。\
             全书大纲必须写清：一句话故事梗概、主题与核心冲突、卷结构（每一卷的主线事件与转折点，\
             并且每一卷必须标明章数与章号区间，如「卷一 共30章（编号1—30）」，供「写第N卷正文」按区间批量执行）、\
             主要人物关系与成长线、关键伏笔与回收安排、结局走向。\
             写到卷级为止，不要展开成任何章节细纲——但卷的章号区间必须写死数字，不许含糊。\
             【文件锁定】只允许写 大纲/大纲.md，绝不要写 大纲/细纲_第N章.md。"
        );
    }
    format!(
        "生成整本故事的全书大纲，写入 大纲/大纲.md（题材：{topic}）：\
         先完整阅读 设定/ 下的现有设定。全书大纲要写清：一句话故事梗概、主题与核心冲突、\
         卷结构（每一卷标明主线事件与章数区间，如「卷一 共30章（编号1—30）」）、主要人物关系、\
         关键伏笔与结局走向。写到卷级为止，不要展开成章节细纲——但卷的章号区间必须写死数字。\
         【文件锁定】只允许写 大纲/大纲.md，绝不要写 大纲/细纲_第N章.md。"
    )
}

/// 补全设定这一步的任务（阶段 11.6 新增）。
///
/// 与 [`settings_bootstrap_task`] 的区别：那个是「设定太薄、写正文前先垫底」的自动步骤；
/// 这个是用户明确要求「把设定里这几个都写好」的独立步骤——先读大纲与已有设定，再补空缺。
pub fn settings_fill_task(genre: Option<&str>) -> String {
    // 题材槽位只允许两种值：明确的题材，或「以 设定/题材定位.md 为准」。
    // 以前拿 subject（≈清洗后的用户原话）兜底，整句「根据参考资料，大纲，设定…」
    // 被当成题材注进了子任务 Prompt，模型跟着原话跑偏。
    let topic = genre
        .map(|g| g.trim())
        .filter(|g| !g.is_empty())
        .unwrap_or("以 设定/题材定位.md 为准");
    format!(
        "【补设定】先读 大纲/ 下的全书大纲（没有大纲就读现有细纲），再盘点 设定/ 目录里已有的文件；\
         然后把缺的设定文件补全写好：世界观写时代背景、地理与权力结构，势力写各方势力的目标、\
         手段与冲突关系，人物写主角与关键配角的身份、动机与人物关系，文风写叙事人称、节奏与语言风格示例。\
         题材以「{topic}」与现有设定为准，人物与事件必须与大纲一致。\
         已有内容的文件只做增补，不要清空重写；写完列出补了哪些文件。\
         【目录锁定】只允许写 设定/ 目录下的文件，绝不要写正文、大纲、追踪或参考资料目录。"
    )
}

/// 模糊章数的澄清步：只读，反问用户，绝不猜区间开跑。
pub fn clarify_task(subject: &str) -> String {
    format!(
        "【只读模式 · 需要澄清】这句话没有明确的章节范围（{subject}）。\
         不要猜测区间，也不要默认写第 1 章；请用一两句话向作者反问：要写哪几章？\
         可以给示例：「001-045 的细纲」「接着最新一章写 5 章」「从第 15 章开始写 10 章」。\
         在作者补齐范围之前，不要写任何文件。"
    )
}

/// 批量写正文这一步的任务（区间 / 相对章数请求专用）。
///
/// 与 [`writer_task`] 的区别：覆盖一整段区间，逐章读对应细纲再动笔，
/// 文件名按各章三位数编号，不许跳章。
pub fn writer_batch_task(
    start: u32,
    end: u32,
    genre: Option<&str>,
    word_count: Option<(u32, u32)>,
) -> String {
    let topic = genre
        .map(|g| g.trim())
        .filter(|g| !g.is_empty())
        .unwrap_or("以 设定/题材定位.md 为准");
    let mut task = format!(
        "按第 {start:03} 章到第 {end:03} 章的细纲，逐章写出正文（题材：{topic}）。\
         每章动笔前先读对应的 大纲/细纲_第NNN章.md，以及 设定/ 下的人物、世界观、势力与文风文件；\
         正文里的时代、地名、人物、事件都要与它们一致，禁止套用其它题材的旧稿。\
         文件名用「第NNN章_<与本章内容相关、不超过八字的小标题>.md」，每章一份，不许跳章。\
         【章号锁定】只允许写第 {start:03} 章到第 {end:03} 章的文件，区间外的章节一律不要动。\
         【对白格式】人物对白一律使用中文弯双引号“”，禁止使用直角引号「」与『』。"
    );
    task.push_str(&word_count_clause(word_count, true));
    // 滚动记忆第一步：每章完稿留一条结构化摘要，后续章节的前情提要靠它兜住。
    // R1：与 writer_task 同口径——只在回复里给行，不改 记忆/摘要索引.md。
    task.push_str(
        "【章节摘要】每写完一章，在回复末尾另起一行给出该章摘要，一章一行，\
         行首写各自的章号（第N章），不要写进任何文件（记忆/摘要索引.md 由编排器统一维护，禁止改动）：\
         第N章 | 故事时间 | 地点 | 出场人物 | 关键事件（40字内） | 新埋伏笔 | 回收伏笔 | 结尾钩子。\
         人物一律用全名。",
    );
    task
}

/// 全书收尾标记：写进任务文本，编排层与写手 prompt 都靠它认出「这是最后一章」。
///
/// 事故链（《隐藏条款》第 300 章）：用户说「没有结尾，再写第300至320章进行结尾处理」，
/// 计划被拆成 21 个逐章正文步，每一步都按写手硬规则「章末落在一句没答完的话上」，
/// 细纲第 320 章自己还写着「后续如续写（事件307）…」——于是「结尾」永远收不住，
/// 用户看到的就是「让它写结尾，它一直往下续写、停不下来」。
pub const ENDING_MARK: &str = "【全书收尾】";

/// 这句话是不是在要「结尾」。
///
/// 只认真正的收尾说法（「结尾处理」「写个结尾」「大结局」「完结」「终章」「完本」…），
/// 不认「每章结尾钩子」这类关于章末写法的要求——否则任意一句提到「结尾」的话
/// 都会把整轮计划标成收尾。
pub fn is_ending_request(request: &str) -> bool {
    let text = normalize_lexical(request.trim());
    const STRONG: [&str; 19] = [
        "结尾处理",
        "写结尾",
        "写个结尾",
        "做个结尾",
        "做结尾",
        "结尾章",
        "最后一章",
        "大结局",
        "完结",
        "终章",
        "完本",
        "全书收束",
        "结束全书",
        // 「收尾小说」「给小说收尾」这类说法：旧表一个都不认，因为下面那条
        // 「收尾 + 全书/整本」的判定要求句子里有范围词。用户直接说「收尾小说」
        // 时整轮计划没有收尾从句，写手照旧每章留钩子——书永远收不住。
        "收尾小说",
        "小说收尾",
        "写收尾",
        "全书收尾",
        "整本收尾",
        "收尾完结",
    ];
    if STRONG.iter().any(|k| text.contains(k)) {
        return true;
    }
    // 名词化说法：「写第301至330章的细纲作为整本书的结尾」「以这一章作为全书结尾」——
    // 这类句子没有一个强动词，但「整本/全书 + 结尾」合起来就是收尾请求。
    // 真实事故：用户原话是「…细纲作为整本书的结尾」，旧列表一个都没命中，
    // 计划里没有收尾从句，细纲照旧写「后续如续写」，用户看到的就是「没按我的意思写结尾」。
    let asks_ending = text.contains("结尾") || text.contains("收束") || text.contains("收尾");
    let book_scope = [
        "全书",
        "整本",
        "整本故事",
        "这部",
        "这本书",
        "最后一章",
        "大结局",
        // 「收尾小说」也会从这里过一遍（STRONG 已经先判）；保留是为了容忍
        // 「这几个章节收尾，小说就完了」这种写法。单说「写小说」不含收尾词，不会误判。
        "小说",
    ]
    .iter()
    .any(|k| text.contains(k));
    if asks_ending && book_scope {
        // 「每章结尾钩子」「章末留钩」是在说写法，不是要收尾。
        return !(text.contains("钩子") && !text.contains("全书完"));
    }
    false
}

/// 收尾章的正文任务从句：本章就是全书最后一章。
pub fn ending_prose_clause(chapter: u32) -> String {
    format!(
        "{ENDING_MARK}第 {chapter} 章是全书最后一章：\n\
         - 本章写完即收束：不得再开新人物、新线索、新悬念，已铺开的事件线在本章内结清，\
           或当场明确交代去向，不许推到「以后」。\n\
         - 章末必须闭合：最后一个画面停住就结束，不留「下一章」的钩子，不写「而这一切才刚开始」\
           「真正的危险才刚刚到来」这类续写口。\n\
         - 正文最后另起一行写「（全书完）」。\n\
         - 本章不按字数下限追加续写：写到目标体量即收，宁可提前收束，也不要为了凑字再开一段新情节。\n\
         - 只允许写 正文/第{chapter:03}章_*.md 这一个文件。"
    )
}

/// 收尾章的细纲任务从句：细纲自己也不许再邀请续写。
pub fn ending_outline_clause(chapter: u32) -> String {
    format!(
        "{ENDING_MARK}第 {chapter} 章是全书最后一章的细纲：\n\
         - 这一章要把主线与各条已开的事件线收干净，章末闭合；\n\
         - 细纲里不许出现「后续如续写」「可以为下一卷开场」这类续写邀请，也不许留未结的钩子；\n\
         - 在细纲末尾明确写出「本章为全书最后一章，正文收在（全书完）」。\n\
         - 只允许写 大纲/细纲_第{chapter:03}章.md 这一个文件。"
    )
}

/// 字数不足时的自动续写任务（编排层在每章写完后对账纯字数，不足就派这一步）。
///
/// `current == 0` 表示这一章根本没落盘——那不是「续写」而是「补写整章」，
/// 任务措辞必须区分，否则模型会去找一个不存在的文件还要「从结尾续写」。
pub fn continuation_task(chapter: u32, current: usize, min: u32, max: u32) -> String {
    if current == 0 {
        return format!(
            "第 {chapter} 章正文缺失（正文/ 目录下没有「第{chapter:03}章_」开头的文件）。\
             请按 大纲/细纲_第{chapter}章.md 完整写出本章正文，\
             文件名用「第{chapter:03}章_<与本章内容相关、不超过八字的小标题>.md」。\
             字数硬要求：纯正文（不计标点、标题与空白）必须不少于 {min} 字、不超过 {max} 字；\
             细纲里的单场「字数建议」低于这个范围时，一律以本条为准；\
             写完自查字数，不足 {min} 字就继续写到达标为止。\
             只允许写这一个文件，不要动其它章节。\
             对白一律使用中文弯双引号“”，禁止使用直角引号「」与『』。"
        );
    }
    format!(
        "第 {chapter} 章正文目前纯字数约 {current} 字，未达到用户硬要求（不少于 {min} 字）。\
         请读取 正文/ 目录下「第{chapter:03}章_」开头的文件，从现有结尾无缝续写：\
         延续已有情节、人称与文风，不要重复已有内容，也不要重写或改动开头；\
         往下写的仍是细纲里还没落地的那些场景，顺序按细纲本身——\
         不要把后面的场景提到前面，也不要回头重写已经写过的场景；补足动作、环境与对白细节，\
         写到纯正文不少于 {min} 字、不超过 {max} 字为止，写完自查字数。\
         只允许改这一个文件，不要动其它章节。\
         对白一律使用中文弯双引号“”，禁止使用直角引号「」与『』。"
    )
}

/// 收尾章的补写任务：与普通补写**不是同一个模式**。
///
/// `continuation_task` 要求「不要重写或改动开头、按细纲顺序往下写」，它解决的是「章还没写完」；
/// 收尾章要解决的是「章写长了但没收口」——为了让全书在同一章收束，必须允许压缩已写的支线、
/// 调整段落顺序、把悬念收掉。把两种诉求塞进同一条指令，会让写手一边被要求「不要改动已有内容」、
/// 一边被要求「收束全书」，两次自我推翻之后只交出几十字的增量（实测就是这样卡住的）。
/// 所以收尾补写单独给一条：明确允许就地改写、只要结果收口且字数到位。
pub fn closing_task(chapter: u32, current: usize, soft_min: u32, min: u32, max: u32) -> String {
    format!(
        "第 {chapter} 章是全书最后一章，目前纯字数约 {current} 字，还不到收尾章要求的 {soft_min} 字。\
         请就地改写 正文/ 目录下「第{chapter:03}章_」开头的这一个文件，把它写成真正的全书收束章：\
         1) 把本章要交代的主线冲突交代完，前文埋下的关键钩子给出落点；\
         2) 未展开的支线可以压缩成一段交代，不要新开任何线索或新角色；\
         3) 章节末尾落在收束上，并在文末单独一行标注「（全书完）」；\
         4) 允许调整段落顺序、删改铺垫，把篇幅补到纯正文不少于 {soft_min} 字（上限 {max} 字，\
            全书硬要求是 {min}-{max} 字，收尾章按软下限 {soft_min} 字验收）；\
         5) 只允许改这一个文件，不要动其它章节，不要新建细纲或大纲。\
         写完自查纯字数（不计标点、标题与空白）。\
         对白一律使用中文弯双引号“”，禁止使用直角引号「」与『』。"
    )
}

/// 字数超出用户上限时的自动精简任务（编排层对账发现 `count > hi` 就派这一步）。
///
/// 以前超出上限只记一笔「请人工压缩」完事：用户要求 9000-9500，写手交出 10761 字，
/// 应用照样报「执行完成」。上下限必须对称——不足自动续写，超出自动精简。
pub fn trim_task(chapter: u32, current: usize, min: u32, max: u32) -> String {
    format!(
        "第 {chapter} 章正文当前纯字数约 {current} 字，超过用户硬要求的上限 {max} 字。\
         请就地精简 正文/ 目录下「第{chapter:03}章_」开头的这一个文件：\
         保留本章全部关键情节、转折与章末落点，删掉重复铺垫、可合并的场景、反复交代的情绪与啰嗦形容，\
         把纯正文压回 {min}-{max} 字的区间（目标 {max} 字以内）。\
         不要新增情节，不要改章名与文件名，不要动其它章节。\
         压完自查纯字数（不计标点、标题与空白），仍超过 {max} 字就继续删。"
    )
}

/// 设定太薄时先补的一步。
pub fn settings_bootstrap_task(genre: Option<&str>) -> String {
    // 同 settings_fill_task：题材槽位绝不用用户原话兜底。
    let topic = genre
        .map(|g| g.trim())
        .filter(|g| !g.is_empty())
        .unwrap_or("以 设定/题材定位.md 为准");
    format!(
        "【补设定】当前设定不足以支撑正文。请先按题材「{topic}」补齐 设定/世界概览.md 与 设定/人物.md：\
         世界概览写时代、地理、经济与权力结构；人物写主角与三个关键配角的身份、动机与关系。\
         每份不超过 600 字，写完说明补了哪些文件。\
         【目录锁定】只允许写 设定/ 目录下的文件，绝不要写正文、大纲或追踪目录。"
    )
}

/// 批量章级细纲这一步的任务（区间请求专用，如「001-045 的细纲」）。
///
/// 以前没有任何区间解析，这类请求缩水成第 1 章。批量必须落成一批
/// `细纲_第NNN章.md`：一份任务说清范围与文件名规则，派一步跑完——
/// 不按章拆 45 步（MAX_STEPS 也装不下，还会烧 45 次 Token）。
pub fn outline_batch_task(
    start: u32,
    end: u32,
    genre: Option<&str>,
    word_count: Option<(u32, u32)>,
) -> String {
    let topic = genre
        .map(|g| g.trim())
        .filter(|g| !g.is_empty())
        .unwrap_or("以 设定/题材定位.md 为准");
    format!(
        "批量生成第 {start:03} 章到第 {end:03} 章的章级细纲，共 {} 份（题材：{topic}）。\
         先读 大纲/大纲.md 的卷结构（没有就读 设定/题材定位.md）与 设定/ 下的现有设定，再逐章展开。\
         每章一份文件，命名 大纲/细纲_第NNN章.md（三位数编号，如 大纲/细纲_第{start:03}章.md 到 大纲/细纲_第{end:03}章.md），一份都不许跳过。\
         每份细纲写清：时代与地点、出场人物及其动机、本章核心冲突、按事件推进切分的场景（不标幕次、不给正文写分节标题）、章末落点（落在动作或半句话上，不写设问句与断言式悬念）、本章字数区间与对白比例；\
         相邻章的情节要衔接，上一章的落点直接接进下一章的开场——不重述、不另起悬案。每份细纲开头单起一行写「所属事件」：大纲/事件/ 下确有事件卡、且这几章属于其中一张，就写「所属事件：事件 NNN · 标题」；没有事件卡（或不属于任何事件）就写「所属事件：无」——没有卡的时候不要自己编事件编号；有卡时同一事件跨的那几章共用一张事件卡，逐章推进它的未结项，不许每章另起一个新事件。只写细纲，不要写正文。{}\
         【章号锁定】只允许写第 {start:03} 章到第 {end:03} 章的细纲文件；\
         区间外的章节细纲与全书大纲一律不要修改、覆盖或重写。",
        end - start + 1,
        outline_word_count_clause(word_count)
    )
}

/// 「只补缺的章」版批量细纲任务：已存在的细纲一句都不许重写。
///
/// 事故：重发同一句「写第 301-330 章细纲」，整批 30 份被原样覆盖，旧稿全丢
/// （文件 mtime 19:48-20:00 全部重写，旧版「所属事件：事件 301」被换成「无」）。
/// 现在只把缺的章交给模型，并在任务里点名「已存在的一个字都不要动」。
pub fn outline_gap_task(
    gaps: &[u32],
    start: u32,
    end: u32,
    genre: Option<&str>,
    word_count: Option<(u32, u32)>,
) -> String {
    let topic = genre
        .map(|g| g.trim())
        .filter(|g| !g.is_empty())
        .unwrap_or("以 设定/题材定位.md 为准");
    let names: Vec<String> = gaps
        .iter()
        .map(|c| format!("大纲/细纲_第{c:03}章.md"))
        .collect();
    let mut parts: Vec<String> = Vec::new();
    parts.push(format!(
        "补齐第 {start:03} 章到第 {end:03} 章区间里缺的 {} 份章级细纲（题材：{topic}）。",
        gaps.len()
    ));
    parts.push(format!(
        "这个区间里其余章节的细纲已经在位：那几份一个字都不要改、不要重写、不要覆盖，只写下面这几个文件——{}。",
        names.join("、")
    ));
    parts.push(
        "先读 大纲/大纲.md 的卷结构（没有就读 设定/题材定位.md）与 设定/ 下的现有设定，再逐章展开。".to_string(),
    );
    parts.push(
        "每份细纲写清：时代与地点、出场人物及其动机、本章核心冲突、按事件推进切分的场景（不标幕次、不给正文写分节标题）、章末落点（落在动作或半句话上，不写设问句与断言式悬念）、本章字数区间与对白比例；".to_string(),
    );
    parts.push(
        "相邻章的情节要衔接，上一章的落点直接接进下一章的开场——不重述、不另起悬案。每份细纲开头单起一行写「所属事件」：大纲/事件/ 下确有事件卡、且这几章属于其中一张，就写「所属事件：事件 NNN · 标题」；没有事件卡（或不属于任何事件）就写「所属事件：无」——没有卡的时候不要自己编事件编号。只写细纲，不要写正文。".to_string(),
    );
    parts.push(outline_word_count_clause(word_count));
    parts.push(format!(
        "【章号锁定】只允许写上面列出的 {} 个文件；区间内已有的细纲、区间外的章节细纲与全书大纲一律不要修改、覆盖或重写。",
        names.len()
    ));
    parts.join(" ")
}

/// 生成 / 更新事件卡这一步的任务。
///
/// 事件层是「一章一天」的病根解药：章被设成结算单位，事件就只能活一章；
/// 把事件单独落成一张可结算的卡，跨章展开才有地方挂。
/// 所以模板必须逼出三样东西——**跨章区间**、**带锚词与最迟结算章的未结项**、
/// **逐章的消耗/新增分工**；缺一样，模型又会退回「每章一个新悬念」。
/// 这一步的任务文本是不是「排事件卡」。
///
/// 判的是文本而不是 `EventRef`：`EventRef::card_step()` 的空 id 只是个标记，
/// 闸层把任务换成批量细纲后标记就是过期数据，靠它认卡步会把整批细纲当成一步放过。
/// 任务文本是不是「排事件卡」那一种。
///
/// 只认事件卡任务自己的措辞：「大纲/事件/」这种路径在细纲模板里也会出现
/// （「大纲/事件/ 下确有事件卡就写所属事件」），拿它当判据会把批量细纲误认成卡步。
pub fn is_event_task(task: &str) -> bool {
    task.contains("排事件卡") || task.contains("建 1—3 张事件卡")
}

pub fn event_task(subject: &str, genre: Option<&str>, span: Option<(u32, u32)>) -> String {
    let subject = subject.trim();
    let topic = genre
        .map(|g| g.trim())
        .filter(|g| !g.is_empty())
        .unwrap_or("以 设定/题材定位.md 为准");
    let cover = match span {
        Some((s, e)) => format!("（本次覆盖第 {s:03}—{e:03} 章）"),
        None => String::new(),
    };
    format!(
        "为「{subject}」排事件卡（题材：{topic}）：在 大纲/事件/ 下按 事件_NNN_标题.md 建 1—3 张事件卡，\
         每张覆盖连续 2—8 章{cover}。一张卡就是一个事件，六节缺一不可：\
         ## 基本信息（表格：所属卷 | 章号区间 | 前置事件 | 状态）、\
         ## 结算条件（写成可判定的动作或物证，全部满足才算结）、\
         ## 未结项（表格：编号 | 内容 | 埋于 | 最迟结算章 | 锚词）、\
         ## 已结算账本（表格：编号 | 结算于 | 方式）、\
         ## 章型分工（表格：章 | 章型 | 本章消耗 | 本章新增，章型只能写 起/推/转/结）。\
         三条硬规则：① 一个事件至少跨 2 章，同一事件在不同章各推进一格，不许每章换一个新事件；\
         ② 每个未结项必须有锚词（正文里能被找到的具体物件或说法）与最迟结算章，没有锚词的不算未结项；\
         ③ 新增的未结项必须服务于该事件的结算条件，不许为制造悬念临时加单据、加失踪、加身份线索。\
         【文件锁定】只允许写 大纲/事件/ 下的文件；大纲/大纲.md、各章细纲与正文一律不要改。"
    )
}

/// 把用户原话里「结合/根据 X」点名的素材目录，翻成给执行者的读取硬指令。
///
/// 规划与执行脱节的根治：闸层模板只固定读 设定/ 与 细纲，用户说
/// 「结合参考资料、追踪等内容」时，这句要求在任务被改写成模板的那一刻蒸发。
/// 整句原话不许注入（模型会把需求句当成细纲内容写进文件——修过的 bug），
/// 这里只提取目录名，落成一句可执行的读取要求。大纲/设定/细纲模板本来就要求读，不重复。
pub fn sources_clause(request: &str) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();
    if request.contains("参考资料") || request.contains("考据") || request.contains("素材")
    {
        parts.push("参考资料/ 中的考据素材");
    }
    if request.contains("追踪") || request.contains("伏笔") || request.contains("时间线") {
        parts.push("追踪/ 下的伏笔与时间线账本");
    }
    if request.contains("对标") {
        parts.push("对标/ 下的节奏与文风拆解");
    }
    if parts.is_empty() {
        return None;
    }
    Some(format!(
        "【作者指定结合素材】动笔前必须先读：{}，产出必须能体现引用到的细节，不要凭印象写。",
        parts.join("、")
    ))
}

/// 从任务文本里把「【作者指定结合素材】…。」这段捞回来（批量拆步重写任务时不能丢）。
pub fn sources_clause_of(task: &str) -> Option<String> {
    let start = task.find("【作者指定结合素材】")?;
    let rest = &task[start..];
    let end = rest.find('。')? + "。".len();
    Some(rest[..end].to_string())
}

#[cfg(test)]
mod tests {

    /// 补写与收尾补写必须是两种模式：普通补写不许改写开头，收尾补写必须允许就地改写并收口。
    #[test]
    fn closing_task_is_not_the_same_mode_as_continuation() {
        let cont = continuation_task(331, 4200, 6000, 6500);
        assert!(cont.contains("不要重写或改动开头"), "普通补写必须禁止改写开头");
        assert!(!cont.contains("（全书完）"), "普通补写不该要求收尾标记");
        assert!(
            !cont.contains("优先展开细纲里还没写到的场景"),
            "「不改开头」与「优先展开未写场景」并列会自相矛盾，已改成按细纲顺序往下写"
        );
        assert!(cont.contains("顺序按细纲本身"));

        let closing = closing_task(350, 4200, 4500, 6000, 6500);
        assert!(closing.contains("（全书完）"), "收尾补写必须要求标（全书完）");
        assert!(closing.contains("允许调整段落顺序"), "收尾补写必须允许就地改写");
        assert!(closing.contains("软下限"), "收尾补写必须说明按软下限验收");
        assert!(closing.contains("4500") && closing.contains("6500"));
        assert!(
            !closing.contains("不要重写或改动开头"),
            "收尾补写正是要能改本章内容，不能沿用普通补写的禁改约束"
        );
        assert_ne!(cont, closing);
    }

    use super::*;

    // ---- 计划扩写回归（【本次只做一件事】+【不要做】） ----

    #[test]
    fn forbids_reads_the_do_not_list() {
        let spec = "【本次只做一件事】设定\n【产出】设定/设定.md\n【不要做】不写正文、不写大纲、不改世界观势力角色之外的其他设定";
        assert!(forbids(spec, Kind::Outline), "「不写大纲」必须被认成禁止大纲");
        assert!(forbids(spec, Kind::Prose), "「不写正文」必须被认成禁止正文");
        assert!(forbids_chapter_outline("这次不要细纲"), "「不要细纲」必须被认成禁止章级细纲");
        // 设定不参与否定识别：「不改世界观」是允许改动的范围，不能当成禁止设定。
        assert!(!forbids(spec, Kind::Settings));
    }

    #[test]
    fn wants_outline_only_rejects_requests_that_forbid_outline() {
        let spec = "【本次只做一件事】设定\n【不要做】不写正文、不写大纲";
        assert!(
            !wants_outline_only(spec),
            "「不写大纲」的请求不能因为出现过「大纲」两个字被判成只做大纲"
        );
    }
    #[test]
    fn negated_list_is_read_as_forbidden_products() {
        // 真实事故第三次复现：【不要做】写的是「不写正文、细纲、大纲」——
        // 一个动词带三个并列宾语，紧邻词表（只认「不写大纲」）完全认不出来。
        let spec = "【本次只做一件事】设定\n【产出】设定/世界观.md、设定/势力.md、设定/角色.md\n【范围】补充世界观、势力、角色三部分的设定内容\n【不要做】不写正文、细纲、大纲；不改用户已确认的既有设定内容\n【原话】把设定里世界观、势力、角色补充好";
        assert!(forbids(spec, Kind::Prose), "「不写正文」在列举报里也要认出来");
        assert!(forbids(spec, Kind::Outline), "「、大纲」继承前面的「不写」");
        assert!(forbids_chapter_outline(spec), "「、细纲」同样被禁止");
        assert!(!forbids(spec, Kind::Settings), "「不改…设定内容」是改动范围，不是禁止设定");
        assert!(!wants_outline_only(spec), "禁止了大纲就不能判成只做大纲");
        assert_eq!(classify(spec).kind, Kind::Settings, "产出是 设定/ 下的文件，意图必须是补设定");
    }

    #[test]
    fn deliverable_section_pins_the_intent() {
        // 【产出】点名 设定/ 下的文件：哪怕正文里到处是「大纲」，这次也只补设定。
        let spec = "【产出】设定/世界观.md\n【范围】补世界观\n【不要做】不写正文、细纲、大纲";
        assert_eq!(classify(spec).kind, Kind::Settings);
        // 反过来同样成立：产出点名 正文/ 时不能被「大纲」两个字抢走。
        let prose = "【产出】正文/第005章.md\n【不要做】不写大纲";
        assert_eq!(classify(prose).kind, Kind::Prose);
        // 跨类别的产出不裁决，交回关键词打分。
        let mixed = "【产出】设定/世界观.md、大纲/大纲.md";
        assert_ne!(deliverable_kind(mixed), Some(Kind::Settings), "跨类别不裁决");
    }

    #[test]
    fn only_one_thing_reads_the_marker() {
        assert!(only_one_thing("【本次只做一件事】设定"));
        assert!(only_one_thing("这一轮只做设定，别的不动"));
        assert!(!only_one_thing("把设定和大纲都写了"));
    }

    /// 「收尾小说」这类不带「全书/整本」的收尾指令，以前判不出来，收尾从句根本不注入。
    #[test]
    fn ending_request_accepts_bare_shouwei() {
        for request in [
            "收尾小说",
            "小说收尾",
            "写收尾",
            "这本书收尾吧",
            "写第331至350章的细纲作为整本书的结尾",
            "在第350章完成全书的收尾与完结",
        ] {
            assert!(is_ending_request(request), "{request}");
        }
        for request in ["写小说", "写第 5 章正文", "改一下这章的对白比例"] {
            assert!(!is_ending_request(request), "{request}");
        }
        // 明确说「留钩子续写」不算收尾，除非同一句里点名要「全书完」。
        assert!(!is_ending_request("这章结尾留个钩子，方便以后续写"));
        assert!(is_ending_request("这章结尾不写钩子，直接标（全书完）"));
    }

    /// 「只做细纲，不写正文」必须判成 Outline：事故里被「正文目标字数」「不写正文」带成 Prose，
    /// 计划凭空派生 20 个正文步。
    #[test]
    fn outline_only_request_is_not_prose() {
        let spec = "【本次只做一件事】章级细纲\n【产出】大纲/细纲_第331章.md 至 大纲/细纲_第350章.md\n\
【范围】第 331—350 章（共 20 章；一步只写一章）\n【字数】6000-6500 字（写进各章细纲的「正文目标字数」）\n【不要做】不写正文；不改动大纲、设定、追踪文件";
        let found = classify(spec);
        assert_eq!(found.kind, Kind::Outline, "{:?}", found);
        assert_eq!(found.chapter_range, Some((331, 350)));
        assert!(wants_outline_only(spec));
        // 正常写正文的请求不许被这条否定规则吃掉。
        let prose = classify("根据大纲写第331至350章的正文");
        assert_eq!(prose.kind, Kind::Prose, "{:?}", prose);
        assert!(!wants_outline_only("根据大纲写第331至350章的正文"));
        // 「不要写正文，把这章的对白改一下」不是排细纲。
        assert!(!wants_outline_only("不要写正文，把这章的对白改一下"));
        // 计划卡不许写成「先写正文，再排细纲」：那和「不写正文」自相矛盾。
        let summary = plan_summary(spec);
        assert!(!summary.contains("写正文"), "{summary}");
        assert!(summary.contains("细纲"), "{summary}");
    }

    /// 「整本书的结尾」不是「整本从头写」：事故里计划卡被标成「整本从头写」，
    /// 还顺手走了一遍整本重写的分支。裸「整本」不再算整本重写。
    #[test]
    fn ending_request_is_not_whole_book_rewrite() {
        let spec = "【本次只做一件事】章级细纲\n【范围】第 331—350 章（共 20 章）\n\
【收尾】按用户要求，这 20 章作为整本书的结尾处理，细纲需覆盖全书收束安排\n【不要做】不写正文";
        let found = classify(spec);
        assert!(!found.whole_book, "「整本书的结尾」不该判成整本从头写：{:?}", found.evidence());
        assert_eq!(found.kind, Kind::Outline);
        assert_eq!(found.chapter_range, Some((331, 350)));
        let summary = plan_summary(spec);
        assert!(!summary.contains("整本从头写"), "{summary}");
        assert!(!summary.contains("整本"), "{summary}");
        // 真要整本重写才允许走这条分支。
        assert!(classify("把这本书整本重写一遍").whole_book);
        assert!(classify("从头写一遍这本书").whole_book);
        assert!(classify("写一本小说").whole_book);
    }

    /// 收尾 / 改规矩各判一次，别处只读字段：同一句话在计划卡与收尾逻辑里得出
    /// 不同结论，是「计划卡写着整本从头写、收尾却落在第 335 章」那类事故的根。
    #[test]
    fn classify_is_the_single_semantic_exit() {
        let ending = classify("写第331至350章的正文作为整本书的结尾，强调字数要求:6000-6500字");
        assert!(ending.ending, "这份请求就是收尾请求");
        assert!(!ending.whole_book, "「整本书的结尾」不是「整本从头写」");
        assert!(!ending.rule_edit);

        assert!(!classify("帮我写第7章正文").ending);
        assert!(!classify("检查每章的结尾钩子是否太硬").ending);

        let rule = classify("修改一下每个章节的规定，对白修改为25%至35%，生效的设定你来修改");
        assert!(rule.rule_edit);
        assert!(!rule.ending);
        assert!(!classify("写第5章细纲").rule_edit);
    }

    /// 事件卡任务的判定不能靠「大纲/事件/」路径——细纲模板里也有这个路径。
    #[test]
    fn event_task_marker_does_not_match_outline_batch() {
        let card = event_task("都市悬疑", None, Some((331, 335)));
        assert!(is_event_task(&card));
        let batch = outline_batch_task(331, 350, None, Some((6000, 6500)));
        assert!(batch.contains("大纲/事件/"), "模板确实带这个路径");
        assert!(!is_event_task(&batch), "批量细纲不是事件卡任务");
    }

    /// 「修改每个章节的规定」= 格式修订；单纯写某一章不算。
    #[test]
    fn rule_edit_detection() {
        assert!(wants_rule_edit(
            "修改一下每个章节的规定，对白修改为25%至35%，生效的设定你来修改"
        ));
        assert!(!wants_rule_edit("写第5章细纲"));
        assert!(!wants_rule_edit("根据大纲写正文，注意对白比例"));
    }

    /// 括号注记容错 + 全书章数换算 + 滚动摘要规则（真实事故见截图原话）。
    #[test]
    fn parenthetical_range_and_all_chapters() {
        assert_eq!(chapter_range_of("写卷一第1—40(030)章的细纲"), Some((1, 40)));
        assert_eq!(chapter_range_of("第1—40（001—040）章"), Some((1, 40)));
        assert!(wants_all_chapters("开始写七个卷对应章的每个章节的细纲"));
        assert!(!wants_all_chapters("写第5章细纲"));
        assert_eq!(
            total_chapter_range("卷一：第1—40章\n卷二：第41—80章\n卷七：第241—280章"),
            Some((1, 280))
        );
        assert!(writer_task(3, None, None, None).contains("【章节摘要】"));
        assert!(writer_batch_task(1, 5, None, None).contains("【章节摘要】"));
    }

    // ---- 修复回归：「细章」变体 + 卷名→章号区间（真实事故见截图原话） ----

    #[test]
    fn xizhang_variant_is_chapter_outline_not_book_outline() {
        let req = "结合资料，对标，大纲，设定，追踪等内容来完成第一卷各个章节的细章";
        let found = classify(req);
        assert_eq!(found.kind, Kind::Outline, "{:?}", found.evidence());
        assert_eq!(
            found.outline_scope,
            OutlineScope::Chapter,
            "「细章」必须按章级细纲判"
        );
    }

    #[test]
    fn volume_of_parses_chinese_and_digits() {
        assert_eq!(
            volume_of("根据大纲，追踪，设定的内容开始写第一卷的正文"),
            Some(1)
        );
        assert_eq!(volume_of("写第2卷的细纲"), Some(2));
        assert_eq!(volume_of("第十一卷开篇"), Some(11));
        assert_eq!(volume_of("写卷一的正文"), Some(1));
        assert_eq!(volume_of("开卷有益"), None);
        assert_eq!(volume_of("继续写下一章正文"), None);
    }

    #[test]
    fn parse_volume_range_reads_outline_lines() {
        let outline = "| 卷一《入职》 | D1—D30 | 90 天 |\n卷一：第1—30章，共30章\n卷二 第31~60章\n";
        assert_eq!(parse_volume_range(outline, 1), Some((1, 30)));
        assert_eq!(parse_volume_range(outline, 2), Some((31, 60)));
        assert_eq!(parse_volume_range(outline, 3), None);
        // 只有天数「D1—D30」的行不算章区间。
        assert_eq!(parse_volume_range("| 卷一《入职》 | D1—D30 |", 1), None);
    }

    #[test]
    fn parse_volume_range_scans_segment_and_guards() {
        // 区间在下一行：卷标记命中后最多往下看 5 行。
        let outline =
            "## 第一卷 入职\n主线：D1—D30。\n章区间：第1—30章。\n## 第二卷\n章区间：第31—60章。\n";
        assert_eq!(parse_volume_range(outline, 1), Some((1, 30)));
        assert_eq!(parse_volume_range(outline, 2), Some((31, 60)));
        // 本卷段内没有区间、下一卷标记出现：不能拿下一卷的区间凑数。
        let gap = "## 卷一\n铺垫\n## 卷二\n第31—60章\n";
        assert_eq!(parse_volume_range(gap, 1), None);
        // 裸「卷1」不得误中「卷10」。
        let tens = "卷1：第1—9章\n卷10：第91—100章\n";
        assert_eq!(parse_volume_range(tens, 1), Some((1, 9)));
        assert_eq!(parse_volume_range(tens, 10), Some((91, 100)));
    }

    #[test]
    fn volume_outline_with_number_is_chapter_scope() {
        let found = classify("第一卷的卷纲");
        assert_eq!(found.kind, Kind::Outline, "{:?}", found.evidence());
        assert_eq!(
            found.outline_scope,
            OutlineScope::Chapter,
            "点名卷号≠重写整本大纲"
        );
        assert_eq!(classify("写全书卷纲").outline_scope, OutlineScope::Book);
    }

    #[test]
    fn supplement_outline_request_is_incremental_patch() {
        // 真实事故：这句话被覆盖成「重新生成整本故事的全书大纲，旧的大纲一律作废」。
        let req = "大纲内没有写七个卷每个卷有多少章补充一下";
        let found = classify(req);
        assert_eq!(found.kind, Kind::Outline, "{:?}", found.evidence());
        assert!(found.supplement, "「没有写…补充」必须判成增量补写");
        assert!(
            found.describe().contains("补充大纲"),
            "{}",
            found.describe()
        );
        let task = outline_patch_task(None);
        assert!(task.contains("【增量补大纲】"), "{task}");
        assert!(task.contains("一字不动"), "{task}");
        assert!(task.contains("【执行铁律】"), "{task}");
        assert!(task.contains("禁止凭记忆整文件重写"), "{task}");
        assert!(
            !classify("把大纲重新写一遍").supplement,
            "「重新写」不是补充"
        );
    }

    #[test]
    fn volume_plan_task_locks_to_one_volume() {
        let task = volume_plan_task(1, Some("悬疑规则怪谈"));
        assert!(task.contains("【补卷章节规划】"), "{task}");
        assert!(task.contains("编号1—30"), "{task}");
        assert!(task.contains("绝不重写整本大纲"), "{task}");
    }

    #[test]
    fn book_outline_task_requires_volume_chapter_range() {
        let task = book_outline_task(None, true);
        assert!(task.contains("章号区间"), "{task}");
        assert!(task.contains("不要展开"), "{task}");
    }

    #[test]
    fn novel_site_url_is_benchmark_collect() {
        let req = "拆一下这本 https://book.qidian.com/info/1234567/ 的节奏";
        let intent = classify(req);
        assert_eq!(intent.kind, Kind::Benchmark);
        assert_eq!(intent.confidence, Confidence::High);
        assert_eq!(
            novel_urls(req),
            vec!["https://book.qidian.com/info/1234567"]
        );
        let task = benchmark_task(req, "对标", 1, None);
        assert!(task.contains("【对标采集】"), "{task}");
        assert!(task.contains("qidian.com"), "{task}");
        assert!(task.contains("严禁凭印象编造"), "{task}");
    }

    #[test]
    fn bare_novel_url_without_keywords_is_still_benchmark() {
        let intent = classify("https://www.jjwxc.net/onebook.php?novelid=123456");
        assert_eq!(intent.kind, Kind::Benchmark);
        assert_eq!(intent.confidence, Confidence::High);
    }

    #[test]
    fn benchmark_without_url_stays_plain_teardown() {
        let task = benchmark_task("拆解对标作品的文风", "对标", 1, None);
        assert!(!task.contains("【对标采集】"), "{task}");
        assert!(task.contains("拆解对标作品"), "{task}");
    }

    #[test]
    fn non_novel_url_does_not_trigger_collect() {
        assert!(novel_urls("看看 https://www.baidu.com/s?wd=测试 这个网页").is_empty());
    }

    #[test]
    fn sources_clause_translates_named_materials() {
        let clause = sources_clause(
            "强制规定每个章节纯字数要写9000-9500字，结合参考资料，大纲，每个章节的细纲，设定，追踪等内容来完成第026至045章的正文",
        )
        .expect("点名了参考资料与追踪，应生成结合素材段");
        assert!(clause.starts_with("【作者指定结合素材】"), "{clause}");
        assert!(clause.contains("参考资料/"), "{clause}");
        assert!(clause.contains("追踪/"), "{clause}");
        // 大纲/设定/细纲模板本来就要求读，素材段不重复点名
        assert!(!clause.contains("大纲"), "{clause}");
        assert!(sources_clause("帮我写第一章正文").is_none());
    }

    #[test]
    fn sources_clause_of_roundtrips_from_task_text() {
        let clause = sources_clause("结合参考资料与追踪写正文").expect("有素材段");
        let task = format!("批量生成细纲。\n{clause}");
        assert_eq!(sources_clause_of(&task).as_deref(), Some(clause.as_str()));
        assert!(sources_clause_of("没有任何素材段的旧任务").is_none());
    }

    #[test]
    fn novel_writing_request_is_prose_not_settings() {
        let intent = classify("把设定里的几个文件内容给我写了，要写一本古代的农民起事的小说");
        assert_eq!(intent.kind, Kind::Prose);
        assert_eq!(intent.confidence, Confidence::High);
        assert_eq!(intent.genre.as_deref(), Some("古代的农民起事"));
        assert!(intent.whole_book);
        // 主题句不该再带着「把设定里的几个文件内容给我写了」这层壳
        assert!(!intent.subject.contains("给我写了"));
    }

    #[test]
    fn write_first_chapter_is_prose_with_chapter() {
        let intent = classify("帮我写第一章正文");
        assert_eq!(intent.kind, Kind::Prose);
        assert_eq!(intent.chapter, Some(1));
        assert!(!intent.read_only());
    }

    #[test]
    fn chapter_outline_request_routes_to_outline() {
        let intent = classify("生成第3章细纲");
        assert_eq!(intent.kind, Kind::Outline);
        assert_eq!(intent.chapter, Some(3));
        assert!(!intent.read_only());
    }

    #[test]
    fn check_request_is_read_only() {
        let intent = classify("检查伏笔与时间线");
        assert_eq!(intent.kind, Kind::Check);
        assert!(intent.read_only());
        assert_eq!(intent.confidence, Confidence::High);
    }

    #[test]
    fn strong_write_beats_check_words() {
        let intent = classify("写本章正文并检查错别字");
        assert_eq!(intent.kind, Kind::Prose);
        assert!(!intent.read_only());
    }

    #[test]
    fn vague_request_is_low_confidence() {
        let intent = classify("随便看看");
        assert_eq!(intent.kind, Kind::Unknown);
        assert!(intent.is_low());
        assert!(intent.read_only());
    }

    #[test]
    fn describe_mentions_dir_and_chapter() {
        let text = classify("写第2章正文").describe();
        assert!(text.contains("正文/"), "{text}");
        assert!(text.contains("第 2 章"), "{text}");
    }

    #[test]
    fn clean_subject_strips_the_ask_shell() {
        let subject = clean_subject("帮我把设定里的几个文件内容给我写了，要写一本小说");
        assert!(!subject.contains("帮我"), "{subject}");
        assert!(!subject.contains("给我写了"), "{subject}");
    }

    #[test]
    fn writer_task_forces_reading_settings_and_a_new_filename() {
        let task = writer_task(1, Some("古代的农民起事"), None, Some("对白占四成。"));
        assert!(task.contains("古代的农民起事"), "{task}");
        assert!(task.contains("设定/"), "{task}");
        assert!(task.contains("细纲_第1章.md"), "{task}");
        assert!(task.contains("不要沿用"), "{task}");
        assert!(task.contains("细纲硬约束：对白占四成。"), "{task}");
        assert!(task.contains("第001章_"), "{task}");
        // 对白统一中文弯双引号，避免正文里混入直角引号。
        assert!(task.contains("使用中文弯双引号“”"), "{task}");
        assert!(task.contains("禁止使用直角引号「」"), "{task}");
    }

    /// 用户点名了字数时，细纲里的旧区间必须让位：任务里只保留用户区间，
    /// 并且以「最高优先级」放在最后，模型不会按更低的细纲区间提前收尾。
    #[test]
    fn user_word_count_overrides_outline_word_count_in_writer_task() {
        let task = writer_task(
            2,
            Some("古代的农民起事"),
            Some((9000, 9500)),
            Some("对白占四成。"),
        );
        assert!(task.contains("不少于 9000 字"), "{task}");
        assert!(task.contains("不超过 9500 字"), "{task}");
        assert!(task.contains("以本条用户要求为准"), "{task}");
        assert!(!task.contains("2200"), "{task}");
        // 硬约束段落不能再带细纲的字数，只保留比例/风格
        let hard = task
            .find("硬约束（比例/风格，不含字数）")
            .expect("有硬约束");
        let wc = task.find("字数硬要求（最高优先级）").expect("有用户字数");
        assert!(hard < wc, "用户字数必须在硬约束之后：{task}");
    }

    #[test]
    fn regenerating_outline_drops_the_old_one() {
        let task = outline_task(1, Some("古代的农民起事"), true, "现有细纲与题材不符", None);
        assert!(task.contains("重新生成第 1 章细纲"), "{task}");
        assert!(task.contains("作废"), "{task}");
        assert!(task.contains("古代的农民起事"), "{task}");
        // 不重新生成时仍然允许沿用
        let keep = outline_task(1, None, false, "", None);
        assert!(keep.contains("已有细纲就沿用"), "{keep}");
    }

    // ---- 阶段 11.6：全书大纲 vs 章细纲、多意图拆解、强生成词 ----

    #[test]
    fn whole_book_outline_request_is_book_scope() {
        let intent = classify("把大纲写了，整本故事的大纲写好，再把设定里这几个都写好");
        assert_eq!(intent.kind, Kind::Outline);
        assert_eq!(intent.outline_scope, OutlineScope::Book);
        // 单说「把大纲写了」也默认整本
        assert_eq!(classify("把大纲写了").outline_scope, OutlineScope::Book);
        assert_eq!(classify("整理一份总纲").outline_scope, OutlineScope::Book);
    }

    #[test]
    fn chapter_outline_stays_chapter_scope() {
        assert_eq!(
            classify("生成第3章细纲").outline_scope,
            OutlineScope::Chapter
        );
        assert_eq!(
            classify("帮我写第3章大纲").outline_scope,
            OutlineScope::Chapter
        );
        // 章级细纲的意图与章号不受影响
        let intent = classify("生成第3章细纲");
        assert_eq!(intent.kind, Kind::Outline);
        assert_eq!(intent.chapter, Some(3));
    }

    #[test]
    fn sequence_splits_outline_then_settings() {
        let seq = intent_sequence("把大纲写了，整本故事的大纲写好，再把设定里这几个都写好");
        assert_eq!(
            seq.len(),
            2,
            "{:?}",
            seq.iter().map(|i| i.kind).collect::<Vec<_>>()
        );
        assert_eq!(seq[0].kind, Kind::Outline);
        assert_eq!(seq[0].outline_scope, OutlineScope::Book);
        assert_eq!(seq[1].kind, Kind::Settings);
        // 计划卡要说清先后
        let text = describe_sequence(&seq).expect("多意图应有描述");
        assert!(text.contains("先"), "{text}");
        assert!(text.contains("写整本大纲"), "{text}");
        assert!(text.contains("补全设定"), "{text}");
    }

    #[test]
    fn sequence_dedupes_same_kind_across_clauses() {
        let seq = intent_sequence("把大纲写了，整本故事的大纲写好");
        assert_eq!(seq.len(), 1);
        assert_eq!(seq[0].kind, Kind::Outline);
    }

    #[test]
    fn sequence_ignores_fragments_and_single_intents() {
        // 碎句没有独立意图；单一意图的请求不该被拆出东西
        assert!(intent_sequence("检查伏笔与时间线").len() <= 1);
        assert!(intent_sequence("随便看看").is_empty());
    }

    #[test]
    fn wants_reuse_only_for_explicit_keep_requests() {
        assert!(!wants_reuse("把大纲写了"));
        assert!(!wants_reuse("重新生成大纲"));
        assert!(wants_reuse("按现有细纲写"));
        assert!(wants_reuse("继续写正文，不要覆盖现有细纲"));
    }

    #[test]
    fn book_outline_task_targets_the_master_file() {
        let task = book_outline_task(Some("古代的农民起事"), true);
        assert!(task.contains("大纲/大纲.md"), "{task}");
        assert!(task.contains("卷"), "{task}");
        assert!(task.contains("作废"), "{task}");
        assert!(task.contains("不要展开"), "{task}");
        assert!(!task.contains("沿用"), "{task}");
    }

    #[test]
    fn settings_fill_task_reads_outline_and_fills_gaps() {
        let task = settings_fill_task(None);
        assert!(task.contains("【补设定】"), "{task}");
        assert!(task.contains("大纲/"), "{task}");
        assert!(task.contains("设定/"), "{task}");
        assert!(task.contains("补全"), "{task}");
        assert!(task.contains("不要清空重写"), "{task}");
    }

    // ---- 修复回归：章号区间解析 + 分句清洗 + 原话不进子任务 ----

    /// 真实事故原话：被拆成「整本大纲 + 补设定」且只写第 1 章、原话还被当成题材。
    const RANGE_REQUEST: &str = "根据参考资料，大纲，设定，追踪开始第一卷的001-045的细纲";

    #[test]
    fn chapter_range_is_parsed_from_nnn_mmm() {
        let intent = classify(RANGE_REQUEST);
        assert_eq!(intent.kind, Kind::Outline);
        assert_eq!(intent.chapter_range, Some((1, 45)));
        assert_eq!(intent.chapter, Some(1));
        assert_eq!(intent.outline_scope, OutlineScope::Chapter);
        assert_eq!(intent.confidence, Confidence::High);
        // 计划卡要能看出这是 1-45 章的批量，而不是「第 1 章」或「整本大纲」
        let text = intent.describe();
        assert!(text.contains("001-045"), "{text}");
        assert!(text.contains("共 45 章"), "{text}");
        assert!(!text.contains("第 1 章"), "{text}");
    }

    #[test]
    fn range_parsing_rejects_years_and_bad_order() {
        assert_eq!(chapter_range_of("2020-2023 年的大事记"), None);
        assert_eq!(chapter_range_of("45-1 章倒着写"), None);
        assert_eq!(chapter_range_of("第2-5章"), Some((2, 5)));
        assert_eq!(chapter_range_of("第 5 到 12 章"), Some((5, 12)));
        assert_eq!(chapter_range_of("1～3的细纲"), Some((1, 3)));
    }

    /// 真实事故：用户写「完成002章的正文」，旧解析只认「第002章」，章号丢失后
    /// 编排层退回「下一章」，写手去改了第 1 章。裸阿拉伯章号必须认出来。
    #[test]
    fn bare_chapter_number_is_parsed_and_wins_over_outline_nouns() {
        let intent = classify("根据大纲、细纲、设定完成002章的正文");
        assert_eq!(intent.kind, Kind::Prose);
        assert_eq!(intent.chapter, Some(2));
        assert_eq!(intent.confidence, Confidence::High);
        assert_eq!(classify("2章的正文").chapter, Some(2));
        // 「共 45 章 / 前 3 章」是数量不是章号
        assert_eq!(classify("共 45 章的正文").chapter, None);
        assert_eq!(classify("前 3 章的正文").chapter, None);
    }

    /// 相对章数不能被裸章号抢走：「接着上次写5章细纲」是 5 章，不是第 5 章。
    #[test]
    fn relative_chapter_count_is_not_mistaken_for_a_bare_chapter() {
        let intent = classify("接着上次写5章细纲");
        assert_eq!(intent.pending_count, Some(5));
        assert_eq!(intent.chapter, None);
        let write = classify("写5章正文");
        assert_eq!(write.pending_count, Some(5));
        assert_eq!(write.chapter, None);
        // 显式「第 N 章」仍然优先
        assert_eq!(classify("继续写第十章正文").chapter, Some(10));
        assert_eq!(classify("继续写第十章正文").pending_count, None);
    }

    #[test]
    fn range_request_sequence_has_one_specific_outline() {
        let seq = intent_sequence(RANGE_REQUEST);
        assert_eq!(
            seq.len(),
            1,
            "{:?}",
            seq.iter()
                .map(|i| (i.kind, i.chapter_range))
                .collect::<Vec<_>>()
        );
        assert_eq!(seq[0].kind, Kind::Outline);
        assert_eq!(seq[0].chapter_range, Some((1, 45)));
        // 裸名词「大纲」「设定」碎句不是任务；「根据参考资料」不是找资料任务
        assert!(!seq.iter().any(|i| i.kind == Kind::Settings));
        assert!(!seq.iter().any(|i| i.kind == Kind::Research));
    }

    #[test]
    fn later_clause_with_range_replaces_vaguer_outline() {
        let seq = intent_sequence("把大纲写了，然后开始第2-5章的细纲");
        assert_eq!(
            seq.len(),
            1,
            "{:?}",
            seq.iter()
                .map(|i| (i.kind, i.chapter_range))
                .collect::<Vec<_>>()
        );
        assert_eq!(seq[0].chapter_range, Some((2, 5)));
    }

    #[test]
    fn batch_task_covers_every_chapter_and_names_files() {
        let task = outline_batch_task(1, 45, None, None);
        assert!(task.contains("第 001"), "{task}");
        assert!(task.contains("第 045"), "{task}");
        assert!(task.contains("共 45 份"), "{task}");
        assert!(task.contains("细纲_第001章.md"), "{task}");
        assert!(task.contains("细纲_第045章.md"), "{task}");
        assert!(task.contains("不要写正文"), "{task}");
        // 没给题材时只允许「以设定为准」，绝不允许空着或塞原话
        assert!(task.contains("以 设定/题材定位.md 为准"), "{task}");
    }

    #[test]
    fn settings_tasks_never_inject_the_raw_request() {
        // 题材槽位只能是题材或「以设定为准」——签名里已经没有 subject 位置，
        // 这里锁死模板内容，防止将来把原话加回来。
        for task in [settings_fill_task(None), settings_bootstrap_task(None)] {
            assert!(task.contains("以 设定/题材定位.md 为准"), "{task}");
            assert!(task.contains("【补设定】"), "{task}");
        }
    }

    // ---- 泛化升级回归：中阿混用 / 中文数字 / 相对表达 / 模糊澄清 ----

    #[test]
    fn chinese_numeral_ranges_and_single_chapters() {
        // 中文数字区间（含零隔位与百位进位）
        let range = classify("第一百零一章到一百一十章的细纲");
        assert_eq!(range.chapter_range, Some((101, 110)));
        assert_eq!(range.kind, Kind::Outline);
        assert_eq!(range.outline_scope, OutlineScope::Chapter);
        // 中文单章
        assert_eq!(classify("生成第一百章的细纲").chapter, Some(100));
        assert_eq!(classify("写第10章细纲").chapter, Some(10));
        // 中阿混用区间
        assert_eq!(classify("第1到5章的细纲").chapter_range, Some((1, 5)));
        assert_eq!(classify("10~20章细纲").chapter_range, Some((10, 20)));
        assert_eq!(classify("5至12章细纲").chapter_range, Some((5, 12)));
    }

    #[test]
    fn years_are_not_chapter_ranges() {
        assert_eq!(chapter_range_of("2020到2023年的剧情"), None);
        let intent = classify("把2020到2023年的剧情理成时间线");
        assert_eq!(intent.chapter_range, None);
        assert_eq!(intent.chapter, None);
        assert_eq!(intent.kind, Kind::Check, "带年份的剧情梳理是检查，不是排章");
    }

    #[test]
    fn vague_quantity_needs_clarification_not_chapter_one() {
        let intent = classify("帮我写点接下来的细纲，大概几章吧");
        assert!(intent.needs_clarification, "{:?}", intent.kind);
        assert_eq!(intent.chapter_range, None);
        assert_eq!(intent.chapter, None);
        assert_eq!(intent.pending_count, None);
    }

    #[test]
    fn relative_count_parses_and_avoids_absolute_conflicts() {
        assert_eq!(relative_count_of("写接下来5章"), Some(5));
        assert_eq!(relative_count_of("接着上次写3章"), Some(3));
        assert_eq!(relative_count_of("写最新的5章"), Some(5));
        // 有绝对章号时相对表达不再叠加：「继续写第十章正文」是第 10 章，不是再写 10 章
        let abs = classify("继续写第十章正文");
        assert_eq!(abs.chapter, Some(10));
        assert_eq!(abs.pending_count, None);
        assert_eq!(abs.chapter_range, None);
        // 起点+章数：开始=含起点；往后=从下一章起
        assert_eq!(classify("从第15章开始写10章").chapter_range, Some((15, 24)));
        assert_eq!(
            classify("接着第100章往后写5章").chapter_range,
            Some((101, 105))
        );
    }

    // ---- 字数要求透传 ----

    #[test]
    fn word_count_ranges_and_qualifiers_are_parsed() {
        // 区间原样透传（真实事故原话）
        assert_eq!(
            word_count_of("每章纯字数要写6000-8000字"),
            Some((6000, 8000))
        );
        assert_eq!(word_count_of("每章写六千到八千字"), Some((6000, 8000)));
        // 限定词容差
        assert_eq!(word_count_of("每章8000字以上"), Some((8000, 12000)));
        assert_eq!(word_count_of("每章不少于2000字"), Some((2000, 3000)));
        assert_eq!(word_count_of("每章5000字以内"), Some((3750, 5000)));
        assert_eq!(word_count_of("写一章3000字左右的正文"), Some((2700, 3300)));
        assert_eq!(word_count_of("每章写3000字"), Some((2400, 3600)));
        // 「三两个字」这类口语不是字数指标；年份更不是
        assert_eq!(word_count_of("改两三个字就行"), None);
        assert_eq!(word_count_of("2020到2023年的剧情"), None);
    }

    #[test]
    fn writer_and_outline_tasks_carry_the_word_count() {
        let task = writer_task(3, None, Some((6000, 8000)), None);
        assert!(task.contains("必须不少于 6000 字"), "{task}");
        assert!(task.contains("不超过 8000 字"), "{task}");
        assert!(task.contains("自查字数"), "{task}");
        // 区间批量：每章都要达标
        let batch = writer_batch_task(2, 12, None, Some((6000, 8000)));
        assert!(batch.contains("每章纯正文"), "{batch}");
        assert!(batch.contains("不少于 6000 字"), "{batch}");
        // 细纲按这个体量设计场景量，否则写手照细纲也写不够
        let outline = outline_batch_task(2, 45, None, Some((6000, 8000)));
        assert!(outline.contains("字数 6000 到 8000 字"), "{outline}");
        assert!(outline.contains("各场景「字数建议」之和"), "{outline}");
        // 用户字数必须原样写进细纲的「正文目标字数」，不能只写单场建议
        assert!(outline.contains("正文目标字数"), "{outline}");
    }

    /// 字数不足的续写任务：缺文件是「补写整章」，不够长是「无缝续写」，
    /// 两种措辞不能混，否则模型会去找一个不存在的文件还要「从结尾续写」。
    #[test]
    fn continuation_task_distinguishes_missing_file_from_shortfall() {
        let missing = continuation_task(7, 0, 6000, 8000);
        assert!(missing.contains("缺失"), "{missing}");
        assert!(missing.contains("第007章_"), "{missing}");
        assert!(missing.contains("不少于 6000 字"), "{missing}");
        let short = continuation_task(7, 5900, 6000, 8000);
        assert!(short.contains("5900"), "{short}");
        assert!(short.contains("无缝续写"), "{short}");
        assert!(short.contains("不要重复已有内容"), "{short}");
        assert!(short.contains("不要动其它章节"), "{short}");
        // 成对抽取能从续写任务里拿回同样的范围（续写步之后可能还要再续）
        assert_eq!(clause_word_count_of(&short), Some((6000, 8000)));
    }

    /// 成对抽取：口语区间仍归 word_count_of，从句必须拿回精确范围而不是容差估算。
    #[test]
    fn clause_word_count_pairs_beat_tolerance_estimates() {
        let clause = "字数硬要求：每章纯正文（不含标题与空行）必须不少于 6000 字、不超过 8000 字";
        assert_eq!(clause_word_count_of(clause), Some((6000, 8000)));
        // word_count_of 对同一句会估成 (6000, 9000)——这就是为什么要成对抽取
        assert_ne!(word_count_of(clause), Some((6000, 8000)));
        // 只有下限没有上限：不成对，返回 None（退回 word_count_of 的容差）
        assert_eq!(clause_word_count_of("至少 3000 字"), None);
        // 中文数字也能抽
        assert_eq!(
            clause_word_count_of("必须不少于六千字、不超过八千字"),
            Some((6000, 8000))
        );
    }
}
