//! `PlanStep` 的构造器：字段与任务文本一次生成，两边不许各写一套。
//
//! R4 第一步只做「加字段、不改行为」：每个构造器渲染出的 `task` 与 R4 之前
//! `intent::writer_task` / `outline_task` / `book_outline_task` / `outline_batch_task` /
//! `writer_batch_task` / `sources_clause` 的输出**逐字节相同**——由 `tests/golden/` 下的
//! 快照锁住（见 [`mod tests`] 的 `golden_prompts_match`）。
//
//! 为什么不把这些字段塞进 `intent` 的模板函数：模板函数的入参（chapter / genre / words）
//! 就是字段本身，构造器是两者的交点；闸层（`master.rs`）拿着 `PlanStep` 只认字段，
//! 不再从中文里重新抠一遍章号与「只读」。

use crate::models::ai::AgentRole;
use crate::models::dto::{ChapterRange, OutlineLock, PlanStep, RunKind, StepPurpose, };
use crate::models::event::EventRef;
use crate::services::intent;

// 构造器是「字段 + 任务文本」的唯一交点：谁要造一步，就用它们，别自己拼字符串。
//
// 现在生产路径还没接线（R4 第一步承诺「不改行为」，拆步器仍走 `intent::*` 模板），
// 所以非测试构建会报 `dead_code`——用 `cfg_attr(not(test), allow(dead_code))` 明确标出：
// 这些函数由 `golden_prompts_match` 覆盖，等项 1 把拆步器换成构造器后再撤掉这个属性。
#[cfg_attr(not(test), allow(dead_code))]
impl PlanStep {
    /// 单章正文。
    pub fn prose_chapter(
        agent_id: &str,
        chapter: u32,
        genre: Option<&str>,
        words: Option<(u32, u32)>,
        constraints: Option<&str>,
    ) -> Self {
        Self {
            agent_id: agent_id.to_string(),
            task: intent::writer_task(chapter, genre, words, constraints),
            role: Some(AgentRole::Writer),
            kind: Some(RunKind::Prose),
            range: Some(ChapterRange::Single(chapter)),
            lock: None,
            words,
            read_only: Some(false),
            event: None,
            ending: None,
            purpose: Some(StepPurpose::ProseBatch),
        }
    }

    /// 区间正文（`writer_batch_task`）。
    pub fn prose_span(
        agent_id: &str,
        from: u32,
        to: u32,
        genre: Option<&str>,
        words: Option<(u32, u32)>,
    ) -> Self {
        Self {
            agent_id: agent_id.to_string(),
            task: intent::writer_batch_task(from, to, genre, words),
            role: Some(AgentRole::Writer),
            kind: Some(RunKind::Prose),
            range: Some(ChapterRange::Span(from, to)),
            lock: None,
            words,
            read_only: Some(false),
            event: None,
            ending: None,
            purpose: Some(StepPurpose::ProseBatch),
        }
    }

    /// 单章细纲。
    pub fn outline_chapter(
        agent_id: &str,
        chapter: u32,
        genre: Option<&str>,
        regenerate: bool,
        reason: &str,
        words: Option<(u32, u32)>,
    ) -> Self {
        Self {
            agent_id: agent_id.to_string(),
            task: intent::outline_task(chapter, genre, regenerate, reason, words),
            role: Some(AgentRole::Architect),
            kind: Some(RunKind::Outline),
            range: Some(ChapterRange::Single(chapter)),
            lock: Some(OutlineLock::Chapters(chapter, chapter)),
            words,
            read_only: Some(false),
            event: None,
            ending: None,
            purpose: Some(StepPurpose::OutlineBatch),
        }
    }

    /// 区间细纲（`outline_batch_task`）。
    pub fn outline_span(
        agent_id: &str,
        from: u32,
        to: u32,
        genre: Option<&str>,
        words: Option<(u32, u32)>,
    ) -> Self {
        Self {
            agent_id: agent_id.to_string(),
            task: intent::outline_batch_task(from, to, genre, words),
            role: Some(AgentRole::Architect),
            kind: Some(RunKind::Outline),
            range: Some(ChapterRange::Span(from, to)),
            lock: Some(OutlineLock::Chapters(from, to)),
            words,
            read_only: Some(false),
            event: None,
            ending: None,
            purpose: Some(StepPurpose::OutlineBatch),
        }
    }

    /// 事件卡（`event_task`）：一次排 1—3 张卡，覆盖一段连续章号。
    ///
    /// 与细纲步共用 `kind = Outline` + `lock = OutlineDir`，靠 `event` 区分：
    /// 卡步的 `event` 是 `card_step()`（id 为空），细纲步的 `event` 才是某个已有事件——
    /// 这样 `RunKind` 与 `OutlineLock` 的穷尽 match 一处都不用动。
    pub fn event_card(
        agent_id: &str,
        subject: &str,
        genre: Option<&str>,
        span: Option<(u32, u32)>,
    ) -> Self {
        Self {
            agent_id: agent_id.to_string(),
            task: intent::event_task(subject, genre, span),
            role: Some(AgentRole::Architect),
            kind: Some(RunKind::Outline),
            range: span.map(|(s, e)| ChapterRange::Span(s, e)),
            lock: Some(OutlineLock::OutlineDir),
            words: None,
            read_only: Some(false),
            event: Some(EventRef::card_step()),
            ending: None,
            purpose: Some(StepPurpose::EventCard),
        }
    }

    /// 全书大纲（`book_outline_task`）。
    pub fn book_outline(agent_id: &str, genre: Option<&str>, regenerate: bool) -> Self {
        Self {
            agent_id: agent_id.to_string(),
            task: intent::book_outline_task(genre, regenerate),
            role: Some(AgentRole::Architect),
            kind: Some(RunKind::Outline),
            range: Some(ChapterRange::Book),
            lock: Some(OutlineLock::Book),
            words: None,
            read_only: Some(false),
            event: None,
            ending: None,
            purpose: None,
        }
    }

    /// 只读步（核对 / 盘点 / 报告），文案与 `step_task(Kind::Check, …)` 一致。
    pub fn read_only(agent_id: &str, role: AgentRole, subject: &str) -> Self {
        Self {
            agent_id: agent_id.to_string(),
            task: format!(
                "【只读模式】{subject}：只输出报告，不要调用 Write 工具，不要修改任何文件"
            ),
            role: Some(role),
            kind: None,
            range: None,
            lock: None,
            words: None,
            read_only: Some(true),
            event: None,
            ending: None,
            purpose: None,
        }
    }

    /// 补一句「作者指定结合素材」（`sources_clause`），并同步把字段补齐。
    pub fn with_sources_clause(mut self, request: &str) -> Self {
        if let Some(clause) = intent::sources_clause(request) {
            if !self.task.contains("【作者指定结合素材】") {
                self.task.push_str(&clause);
            }
        }
        self
    }

    /// 只填字段、不改文本：给「已经是人话、不需要模板」的步骤补语义。
    pub fn with_shape(mut self, range: ChapterRange, lock: Option<OutlineLock>) -> Self {
        self.range = Some(range);
        if lock.is_some() {
            self.lock = lock;
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// 快照目录：`src-tauri/tests/golden/`。
    fn golden_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("golden")
    }

    /// 待锁的十四种渲染：(文件名, 渲染结果)。
    ///
    /// 每一项都是「同一组真实入参下当前代码会产出的任务文本」——R4 之前它们由
    /// `intent::*` 直接生成，R4 之后由字段构造器生成，两边必须一模一样。
    fn golden_cases() -> Vec<(&'static str, String)> {
        let genre = Some("古代的农民起事");
        let words = Some((6000u32, 8000u32));
        let mut cases: Vec<(&'static str, String)> = vec![
            (
                "prose_chapter_full.txt",
                PlanStep::prose_chapter("writer", 8, genre, words, None).task,
            ),
            (
                "prose_chapter_plain.txt",
                PlanStep::prose_chapter("writer", 3, None, None, None).task,
            ),
            (
                "prose_chapter_with_constraints.txt",
                PlanStep::prose_chapter(
                    "writer",
                    8,
                    genre,
                    words,
                    Some("对白占 25%—35%，每场景不超过 800 字"),
                )
                .task,
            ),
            (
                "prose_span.txt",
                PlanStep::prose_span("writer", 1, 3, genre, words).task,
            ),
            (
                "outline_chapter_regenerate.txt",
                PlanStep::outline_chapter("architect", 12, genre, true, "用户点名要大纲", words)
                    .task,
            ),
            (
                "outline_chapter_reuse.txt",
                PlanStep::outline_chapter("architect", 12, genre, false, "用户点名要大纲", None)
                    .task,
            ),
            (
                "outline_span.txt",
                PlanStep::outline_span("architect", 6, 10, genre, words).task,
            ),
            (
                "event_card.txt",
                PlanStep::event_card("architect", "码头交接与仓单的去向", genre, Some((6, 10))).task,
            ),
            (
                "event_card_no_span.txt",
                PlanStep::event_card("architect", "码头交接与仓单的去向", genre, None).task,
            ),
            (
                "event_card_no_genre.txt",
                PlanStep::event_card("architect", "码头交接与仓单的去向", None, Some((6, 10))).task,
            ),
            (
                "book_outline.txt",
                PlanStep::book_outline("architect", genre, true).task,
            ),
            (
                "book_outline_reuse.txt",
                PlanStep::book_outline("architect", None, false).task,
            ),
            (
                "sources_clause.txt",
                PlanStep::prose_chapter("writer", 8, genre, words, None)
                    .with_sources_clause("按细纲写第8章，结合参考资料、追踪与对标")
                    .task,
            ),
            (
                "read_only.txt",
                PlanStep::read_only("tracker", AgentRole::Tracker, "核对第 8 章的伏笔与时间线")
                    .task,
            ),
        ];
        // 顺序稳定，方便人读 diff
        cases.sort_by(|a, b| a.0.cmp(b.0));
        cases
    }

    /// 生成快照：`cargo test --lib write_golden_files -- --ignored`。
    ///
    /// 只在「确实想更新基准」时跑；平时它不会执行，避免把回归当基准。
    #[test]
    #[ignore]
    fn write_golden_files() {
        let dir = golden_dir();
        std::fs::create_dir_all(&dir).expect("建 golden 目录");
        for (name, text) in golden_cases() {
            std::fs::write(dir.join(name), text.as_bytes()).expect("写 golden 文件");
        }
        eprintln!("[storydesk] golden 已写入 {}", dir.display());
    }

    /// 零 diff：构造器渲染的文本必须与 `tests/golden/*.txt` 逐字节相同。
    #[test]
    fn golden_prompts_match() {
        let dir = golden_dir();
        let mut checked = 0usize;
        for (name, text) in golden_cases() {
            let path = dir.join(name);
            let want = std::fs::read_to_string(&path).unwrap_or_else(|e| {
                panic!(
                    "缺少快照 {}（{}）；先跑 `cargo test --lib write_golden_files -- --ignored`",
                    path.display(),
                    e
                )
            });
            assert_eq!(
                text, want,
                "任务文本与快照不一致：{}\n--- 现在 ---\n{}\n--- 基准 ---\n{}",
                name, text, want
            );
            checked += 1;
        }
        assert_eq!(checked, 14, "快照数量变了，先确认这是故意的");
    }

    #[test]
    fn constructors_fill_the_fields_the_gates_read() {
        let prose =
            PlanStep::prose_span("writer", 4, 6, Some("古代的农民起事"), Some((6000, 8000)));
        assert_eq!(prose.kind, Some(RunKind::Prose));
        assert_eq!(prose.role, Some(AgentRole::Writer));
        assert_eq!(prose.range, Some(ChapterRange::Span(4, 6)));
        assert_eq!(prose.lock, None, "正文步没有细纲文件锁");
        assert_eq!(prose.words, Some((6000, 8000)));
        assert_eq!(prose.read_only, Some(false));

        let outline = PlanStep::outline_chapter("architect", 12, None, true, "重做", None);
        assert_eq!(outline.kind, Some(RunKind::Outline));
        assert_eq!(
            outline.lock,
            Some(OutlineLock::Chapters(12, 12)),
            "单章细纲必须锁该章"
        );
        assert_eq!(outline.range, Some(ChapterRange::Single(12)));

        let book = PlanStep::book_outline("architect", None, true);
        assert_eq!(book.lock, Some(OutlineLock::Book));
        assert_eq!(book.range, Some(ChapterRange::Book));

        let check = PlanStep::read_only("tracker", AgentRole::Tracker, "盘点伏笔");
        assert_eq!(check.read_only, Some(true));
        assert_eq!(check.kind, None, "只读步不属于正文/细纲流水线");
        assert!(check.task.starts_with("【只读模式】"));
    }

    #[test]
    fn old_json_without_the_new_fields_still_deserializes() {
        // 历史 masterEvents / chats/*.json 里只有 agentId 与 action
        let raw = r#"{"agentId":"writer","action":"按第 3 章细纲写本章正文"}"#;
        let step: PlanStep = serde_json::from_str(raw).expect("旧数据必须能读");
        assert_eq!(step.agent_id, "writer");
        assert_eq!(step.task, "按第 3 章细纲写本章正文");
        assert_eq!(step.role, None);
        assert_eq!(step.kind, None);
        assert_eq!(step.range, None);
        assert_eq!(step.lock, None);
        assert_eq!(step.words, None);
        assert_eq!(step.read_only, None);

        // 反向：字段全空时序列化回去仍是老两字段，旧前端读到的东西一字不多。
        let back = serde_json::to_string(&step).expect("序列化");
        assert_eq!(
            back,
            r#"{"agentId":"writer","task":"按第 3 章细纲写本章正文"}"#
        );
    }

    #[test]
    fn new_fields_round_trip_through_json() {
        let step = PlanStep::outline_span("architect", 6, 10, Some("古代的农民起事"), None);
        let json = serde_json::to_string(&step).expect("序列化");
        let back: PlanStep = serde_json::from_str(&json).expect("反序列化");
        assert_eq!(back.kind, Some(RunKind::Outline));
        assert_eq!(back.range, Some(ChapterRange::Span(6, 10)));
        assert_eq!(back.lock, Some(OutlineLock::Chapters(6, 10)));
        assert_eq!(back.task, step.task);
    }

    #[test]
    fn sources_clause_is_not_appended_twice() {
        let once = PlanStep::prose_chapter("writer", 8, None, None, None)
            .with_sources_clause("结合参考资料写第8章");
        let twice = once.clone().with_sources_clause("结合参考资料、追踪、对标");
        assert_eq!(once.task, twice.task, "已有素材句就不再追加");
        assert_eq!(once.task.matches("【作者指定结合素材】").count(), 1);
    }
}
