//! 主从智能体协作（Master-Worker）。
//!
//! 一轮编排分三段，全部在 Rust 侧完成，前端只负责显示与下指令：
//! 1. **规划**：把作者一句话交给主 AI（JSON 规划 prompt），拿回
//!    `{"plan":[{"agent_id","task"}]}`；模型不听话就退回本地规则规划器；
//! 2. **派发**：按计划顺序逐个启动子智能体（用 `AgentConfig.system_prompt` 与
//!    `target_dir` 拼 prompt，尾部仍是那四行契约），每步一个独立 run id；
//! 3. **回传**：`master://think|plan|dispatch|done` + 复用 `ai://chunk|done|hook://blocked`。
//!
//! 关键约束：子智能体是**真 CLI 子进程**，不是模拟；某一步被 hook 拦下就停下，
//! 不继续派后面的步骤（后面通常依赖前一步的产物）。

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use regex::Regex;
use serde::Deserialize;
use tauri::{AppHandle, Emitter, Manager};

use crate::commands::ai as ai_cmd;
use crate::models::ai::{self, AgentConfig};
use crate::models::dto::{
    ChapterRange, EngineKind, MasterContext, MasterDispatch, MasterDone, MasterExecuteRequest,
    MasterPlan, MasterPlanEvent, MasterPlanRequest, MasterProgress, MasterRunRequest,
    MasterRunResult, MasterStepResult, MasterThink, OutlineLock, PlanStep, RunDone, RunKind,
};
use crate::models::event::EventRef;
use crate::models::StoryError;
use crate::services::engine::{self, EngineState, RunCtx, RunOutcome};
use crate::services::{
    context, dir_guard, gate, hook_parse, intent, memory, run_state, settings, skills,
};

/// 一轮最多派几步（模型偶尔会把同一件事拆成五步，纯属浪费）。
const MAX_STEPS: usize = 4;

/// 细纲批量一步最多连写几章：一次让模型生成 40+ 份细纲必然烂尾——
/// 后面的章没有细纲，写手步不是被门禁拦下就是跳章。
const OUTLINE_CHUNK: u32 = 5;

/// 一章字数不达标时最多自动补写几次（防无限循环烧 Token）。
/// 6000-6500 字的长章每轮推进约 500 字，约需 12-13 轮；设为 15 留足余量。
const MAX_WORD_RETRIES: u32 = 15;

/// 一章字数超出用户上限时最多自动精简几次（与补写对称，但不许来回拉锯）。
const MAX_TRIM_RETRIES: u32 = 3;

/// 单次续写最小推进字数（纯字数）：单轮增量低于此值视为「卡住」，不再重试。
const MIN_PROGRESS_PER_RETRY: usize = 500;

/// 收尾章的软下限：允许比用户要求少 1/4（收尾要压缩支线，不能靠注水凑满额），但不低于 600 字。
fn ending_floor(lo: u32) -> u32 {
    if lo == 0 {
        return 0;
    }
    let soft = ((lo as u64 * 3) / 4) as u32;
    soft.max(600).min(lo)
}

/// 模型返回的 JSON 形状。
#[derive(Debug, Deserialize)]
struct PlanJson {
    plan: Vec<PlanStep>,
}

/// 从一步任务里抽「第 N 章」。主 AI 常写成「生成第 1 章细纲」，也可能写中文数字，
/// 用户原话里还可能是不带「第」的「002章」。
fn chapter_in_task(task: &str) -> Option<u32> {
    intent::chapter_of(task).filter(|n| *n > 0)
}

/// 一轮的章号：用户原话 > 计划步骤里写的 > 前端带来的 > 正文目录的下一章。
///
/// 真实事故（截图 1-3）：用户说「完成 001 章的正文」，计划卡也写着「第 001 章」，
/// 派发却按 005 章跑——前端把 store 里上一轮遗留的 `chapter` 传了上来，
/// 而确认执行时 `request` 是空串，这一层只认 `req.chapter`，用户的话根本没参与。
/// 用户写下来的章号永远压过前端缓存的章号。
fn resolve_chapter(
    request: &str,
    plan_chapter: Option<u32>,
    req_chapter: Option<u32>,
    next_chapter: u32,
) -> u32 {
    intent::chapter_of(request)
        .or(plan_chapter)
        .or(req_chapter)
        .unwrap_or(next_chapter)
}

/// 心跳间隔：足够短，作者能看见秒数在走；也足够长，不至于把事件通道灌满。
const HEARTBEAT_MS: u64 = 2000;

/// 一步的心跳把手。`stop()` 之后不再发事件，`forget_activity` 顺手清登记。
struct StepHeartbeat {
    stop: Arc<std::sync::atomic::AtomicBool>,
    handle: Option<tokio::task::JoinHandle<()>>,
}

impl StepHeartbeat {
    /// 正常路径显式收尾。真正的释放动作在 Drop 里，所以这里不重复写一遍：
    /// 提前 return、`?`、panic、future 被丢弃时同样会走到 Drop。
    fn stop(self) {}
}

impl Drop for StepHeartbeat {
    fn drop(&mut self) {
        // 置位就够了：任务每轮都看这个标志；abort 是为了连「正在 sleep」的那一轮也立刻结束。
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
    }
}

/// 这一步的身份信息（心跳每次都要带上，前端据此更新对应卡片）。
#[derive(Clone)]
struct BeatInfo {
    run_id: String,
    step_run_id: String,
    index: usize,
    total: usize,
    agent_id: String,
    agent_name: String,
    target_dir: String,
    chapter: Option<u32>,
    /// 起这一步时已经完成的步数（每步开始时重算一次，同一轮里不会变）。
    done: usize,
    /// 从第几步能续跑（1 基）；全部完成时为 None。
    resume_from: Option<usize>,
    /// 运行状态文件是否已经在磁盘上。
    persisted: bool,
}

impl BeatInfo {
    fn payload(&self, status: &str, elapsed_ms: u64) -> MasterProgress {
        let activity = engine::activity_of(&self.step_run_id);
        MasterProgress {
            run_id: self.run_id.clone(),
            index: self.index,
            total: self.total,
            agent_id: self.agent_id.clone(),
            agent_name: self.agent_name.clone(),
            target_dir: self.target_dir.clone(),
            chapter: self.chapter,
            status: status.to_string(),
            elapsed_ms,
            chunks: activity.map(|(chunks, _)| chunks).unwrap_or(0),
            idle_ms: activity.map(|(_, idle)| idle),
            produced: Vec::new(),
            done: self.done,
            resume_from: self.resume_from,
            persisted: self.persisted,
        }
    }
}

/// 起一个每 2 秒发一次 `master://progress` 的后台任务。
///
/// 派发卡片原本只在派发那一刻说一句话，之后子智能体写 10 分钟，界面一个字都不动。
/// 实测事故：写手步静默跑完 10 分 22 秒，作者以为卡死，去关窗口，把这一轮掐了。
fn spawn_step_heartbeat(app: AppHandle, info: BeatInfo, started: Instant) -> StepHeartbeat {
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = stop.clone();
    let handle = tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(HEARTBEAT_MS)).await;
            if flag.load(std::sync::atomic::Ordering::SeqCst) {
                break;
            }
            let elapsed = started.elapsed().as_millis() as u64;
            let _ = app.emit(
                engine::MASTER_PROGRESS_EVENT,
                info.payload("running", elapsed),
            );
        }
    });
    StepHeartbeat {
        stop,
        handle: Some(handle),
    }
}

/// 前端带了就用前端的，没带就读 settings（settings 里已经 normalize 过）。
fn agents_now(app: &AppHandle, given: &[AgentConfig]) -> Vec<AgentConfig> {
    if given.is_empty() {
        settings::load(app).agents
    } else {
        ai::normalized_agents(given.to_vec())
    }
}

/// 每个子步骤用的环境注入计划（与 `ai_start` 同一口径，Key 只进 `Command::env`）。
/// provider 本体一并带出：opencode 引擎要按它生成 opencode.json。
fn env_plan(
    app: &AppHandle,
) -> (
    engine::EnvPlan,
    Vec<String>,
    Option<crate::models::settings::ProviderConfig>,
) {
    let mut notes = Vec::new();
    let (plan, provider) = match settings::active_provider_with_key(app) {
        Some((provider, key)) => {
            let plan = engine::EnvPlan::for_provider(&provider, key);
            if let Some(note) = plan.note.clone() {
                notes.push(note);
            }
            (plan, Some(provider))
        }
        None => {
            notes.push("还没选择模型提供商，本次不注入任何环境变量".to_string());
            (engine::EnvPlan::none(), None)
        }
    };
    (plan, notes, provider)
}

fn think(app: &AppHandle, run_id: &str, phase: &str, text: &str) {
    let _ = app.emit(
        engine::MASTER_THINK_EVENT,
        MasterThink {
            run_id: run_id.to_string(),
            phase: phase.to_string(),
            text: text.to_string(),
        },
    );
}

/// 从模型输出里抠出第一个完整的 JSON 对象（模型常带 ```json 围栏或前导解释）。
pub(crate) fn extract_json(raw: &str) -> Option<&str> {
    let start = raw.find('{')?;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (i, ch) in raw[start..].char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&raw[start..start + i + 1]);
                }
            }
            _ => {}
        }
    }
    None
}

/// 解析模型给的计划；agent_id 必须在当前配置里存在，最多 4 步。
///
/// R4：模型带回来的结构化字段（`range` / `words` / `lock` / `read_only` …）必须原样
/// 带到生产链路上。以前这里用 `PlanStep { agent_id, task, ..Default::default() }` 重建，
/// 模型写的 `range: Span(3,12)` 在解析这一步就被扔掉，整条链只剩任务文本，
/// `PlanStep` 的那套构造器因此长期只有测试在调。
pub(crate) fn parse_plan(raw: &str, agents: &[AgentConfig]) -> Option<Vec<PlanStep>> {
    let json = extract_json(raw)?;
    let parsed: PlanJson = serde_json::from_str(json).ok()?;
    let mut steps: Vec<PlanStep> = Vec::new();
    for mut step in parsed.plan {
        let id = step.agent_id.trim().to_string();
        let task = step.task.trim().to_string();
        if task.is_empty() || !agents.iter().any(|a| a.id == id) {
            continue;
        }
        step.agent_id = id;
        step.task = task;
        steps.push(normalize_step(step, agents));
        if steps.len() >= MAX_STEPS {
            break;
        }
    }
    if steps.is_empty() {
        None
    } else {
        Some(steps)
    }
}

/// 生产标准化层：把一步计划补齐成「带结构化字段」的形态。
///
/// 生产链路上「谁给 `PlanStep` 填字段」只有这一处，三条来源按优先级合并：
/// 1. 模型/前端给了可信的值 → 原样保留（字段优先，不再被文本解析覆盖）；
/// 2. 字段缺失或非法（`words: [0,0]`、`range: Span(9,3)`）→ 从任务文本解析补；
/// 3. 文本也解析不出来 → 留空，让下游走「没有章号」的路径，绝不猜一个章号。
///
/// `words` 不从文本补：模型写的任务里常带着细纲的「字数建议 2200-2600」，那不是用户
/// 规定的验收目标，补进字段会把验收标准压到单场字数（见 `master_run` 的字数注释）。
pub(crate) fn normalize_step(step: PlanStep, agents: &[AgentConfig]) -> PlanStep {
    let agent = ai::resolve_agent(agents, &step.agent_id);
    let readonly = step
        .read_only
        .unwrap_or_else(|| ai::is_read_only_task(&step.task));
    let kind = kind_for(&agent, Some(&step));
    let role = step.role.or_else(|| {
        ai::AgentRole::parse(&step.agent_id).or_else(|| match agent.target_dir.as_str() {
            "正文" => Some(ai::AgentRole::Writer),
            "大纲" => Some(ai::AgentRole::Architect),
            "设定" => Some(ai::AgentRole::Settings),
            "追踪" => Some(ai::AgentRole::Tracker),
            "参考资料" => Some(ai::AgentRole::Researcher),
            "对标" => Some(ai::AgentRole::Benchmarker),
            _ => None,
        })
    });
    // 区间非法（终点小于起点）时当没写处理，回落到文本解析。
    let field = match step.range {
        Some(ChapterRange::Span(from, to)) if to < from => None,
        other => other,
    };
    let span_to_range = |(from, to): (u32, u32)| match from == to {
        true => ChapterRange::Single(from),
        false => ChapterRange::Span(from, to),
    };
    let text_span = text_chapter_span(&step.task);
    // 文本里的章号与 `range` 字段打架时以**文本**为准——但只在文本把字段区间**完整包住**时。
    //
    // 闸改任务时只换 `task`、不动字段：`force_architect` 把细纲步的任务整段换成
    // 「批量生成第 331 章到第 350 章的细纲」，`range` 却停在模型自己切的 `Span(331,335)`。
    // 字段优先的老逻辑于是把这一步当成「只写 331—335」：细纲只排了第一块，收尾从句
    // 写成「第 335 章是全书最后一章」，336—350 整批不派发，全书在第 335 章就「完」了
    // （《隐藏条款》331—350 事故）。文本是闸写入的权威值，字段只是缓存。
    //
    // 反向不许赢：任务里顺手提到「参见 细纲_第005章.md」不该把 `Span(331,350)` 压成第 5 章，
    // 所以只在「文本区间 ⊇ 字段区间」时才接管；其它冲突保留字段（缺省更保守）。
    let text_wins = matches!(
        (field, text_span),
        (Some(field), Some(text))
            if !matches!(field, ChapterRange::Book)
                && field.bounds() != text
                && span_includes(text, field.bounds())
    );
    let range = match (field, text_span) {
        (Some(_), Some(text)) if text_wins => Some(span_to_range(text)),
        (Some(field), _) => Some(field),
        (None, Some(text)) => Some(span_to_range(text)),
        (None, None) => None,
    };
    // 字段里的块号作废时，`lock` 里的同一组块号也要作废，交给
    // `outline_lock_from_text` 按任务文本重算——否则执行期注入的「【章号锁定】」
    // 还是旧块，模型只能交 331—335 的细纲。
    let mut lock = step.lock;
    if text_wins {
        if let Some(OutlineLock::Chapters(from, to)) = lock {
            if Some((from, to)) != range.map(|r| r.bounds()) {
                lock = None;
            }
        }
    }
    let words = step.words.filter(|(lo, hi)| *lo > 0 && hi >= lo);
    let mut out = PlanStep {
        agent_id: step.agent_id,
        task: step.task,
        role,
        kind: Some(kind),
        range,
        lock,
        words,
        read_only: Some(readonly),
        // 事件归属由模型在计划 JSON 里给（`step.event`），这里原样透传；
        // 章号/锁的规整不碰它——事件卡步与章细纲步共用 kind/lock，靠这个字段区分。
        event: step.event,
        // 结构化语义原样透传：规整只看章号/锁，不能顺手把「这是收尾步」「这是批量细纲」
        // 这类语义抹掉，否则下游又得回去猜任务文本。
        ending: step.ending,
        purpose: step.purpose,
    };
    // 细纲锁只对「真的在写大纲目录」的步有意义；tracker/settings 的区间步也归 Outline，
    // 给它们加锁会把「写追踪文件」误判成越界写大纲。
    if kind == RunKind::Outline && agent.target_dir == "大纲" {
        let chapter = out.range.map(|r| r.bounds().0).unwrap_or(1);
        let lock = out
            .lock
            .unwrap_or_else(|| outline_lock_from_text(&out.task, chapter));
        // 标记优先：`enforce_outline_and_settings` 会把章级任务改写成全书大纲
        // （前置「【全书大纲】」或整段换成 `book_outline_task`），`lock` 字段却停在
        // 改写前的 `Chapters(1,1)`。不同步，执行期就按旧字段注入「【章号锁定】只允许写
        // 大纲/细纲_第001章.md」，把「重新生成全书大纲」压成「只写第 1 章细纲」。
        out.lock = Some(sync_lock_with_task(&out.task, lock));
    }
    out
}

/// 本地规则规划的结果：步骤之外还带上「我理解成了什么」，让计划卡能对用户直说。
pub(crate) struct LocalPlan {
    pub steps: Vec<PlanStep>,
    /// 给人看的一句话：「写正文 · 正文/ · 第 1 章 · 题材「古代的农民起事」」。
    pub intent: String,
}

/// 检查类需求派给谁：先看需求里点到的对象，再兜底给 tracker。
fn check_dir(request: &str) -> &'static str {
    let has = |words: &[&str]| words.iter().any(|w| request.contains(w));
    if has(&["伏笔", "时间线", "连贯", "欠账", "前后矛盾", "追踪"]) {
        "追踪"
    } else if has(&["设定", "人物", "世界观", "势力", "术语"]) {
        "设定"
    } else if has(&["对标", "文风", "节奏"]) {
        "对标"
    } else if has(&["考据", "资料", "素材"]) {
        "参考资料"
    } else {
        "追踪"
    }
}

/// 入队一步已经构造好字段的计划（事件卡步靠它保住 `event` 与 `lock`，
/// `push_step` 只收任务文本，会把两者丢掉）。
fn push_plan_step(steps: &mut Vec<PlanStep>, step: Option<PlanStep>) {
    if let Some(step) = step {
        steps.push(step);
    }
}

fn push_step(steps: &mut Vec<PlanStep>, agent: Option<String>, task: String) {
    if let Some(agent_id) = agent {
        steps.push(PlanStep {
            agent_id,
            task,
            ..Default::default()
        });
    }
}

/// 从任务文本里捞「题材：X」——确认执行路径拿不到用户原话（request 为空），
/// 只能从写手任务里恢复题材；「以 设定/题材定位.md 为准」这种占位说法不算题材。
fn genre_from_task(task: &str) -> Option<String> {
    let re = Regex::new(r"题材[：:]\s*([^（）()。；;]{2,30})").expect("任务题材正则");
    let raw = re.captures(task)?.get(1)?.as_str().trim();
    if raw.is_empty() || raw.starts_with("以 设定") || raw.starts_with("以设定") {
        return None;
    }
    Some(raw.to_string())
}

/// 从任务文本里抠章号区间：先找「第 5 至第 8 章」这种区间，再退到单个章号。
fn text_chapter_span(task: &str) -> Option<(u32, u32)> {
    if let Some((start, end)) = intent::chapter_range_of(task).filter(|(s, e)| e >= s) {
        return Some((start, end));
    }
    chapter_in_task(task).map(|c| (c, c))
}

/// 一步覆盖的章号区间（写手批量步可能是「第 5 至第 8 章」这种区间）。
///
/// R4：先看 `PlanStep::range` 字段；字段为空（旧数据 / 模型直接给的中文任务）
/// 才回到从任务文本里抠章号——两条路的结果对同一句话是一致的，字段只是省掉重解析。
/// 区间里还没有可复用细纲的章：缺文件、太薄、题材不符都算缺。
fn missing_outlines(book: Option<&Path>, from: u32, to: u32, genre: Option<&str>) -> Vec<u32> {
    let Some(root) = book else {
        return (from..=to).collect();
    };
    (from..=to)
        .filter(|c| !context::check_outline(root, *c, genre).reusable)
        .collect()
}

/// [301,302,303,307] -> 第 301-303、307 章（计划卡上给人看的写法）。
fn chapters_brief(chapters: &[u32]) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut i = 0;
    while i < chapters.len() {
        let start = chapters[i];
        let mut end = start;
        while i + 1 < chapters.len() && chapters[i + 1] == end + 1 {
            i += 1;
            end = chapters[i];
        }
        parts.push(if end == start {
            format!("{start}")
        } else {
            format!("{start}-{end}")
        });
        i += 1;
    }
    format!("第 {} 章", parts.join("、"))
}

/// 批量细纲步的任务文本；返回 None 表示这一步根本不用派发。
///
/// 只有一处判断：区间里还有没有缺的章。全齐 -> 不派发（重发同一句请求不会覆盖旧稿）；
/// 部分缺 -> 只把缺的章交给模型。以前这里无条件重写整段区间，是「已生成的细纲又被重写」的直接原因。
fn outline_gap_plan(
    book: Option<&Path>,
    from: u32,
    to: u32,
    genre: Option<&str>,
    words: Option<(u32, u32)>,
    notes: &mut Vec<String>,
) -> Option<String> {
    let gaps = missing_outlines(book, from, to, genre);
    let total = to.saturating_sub(from) + 1;
    if gaps.is_empty() {
        notes.push(format!(
            "细纲齐备（{}）：沿用现有细纲，本轮不重写",
            chapters_brief(&(from..=to).collect::<Vec<_>>())
        ));
        return None;
    }
    if gaps.len() as u32 == total {
        return Some(intent::outline_batch_task(from, to, genre, words));
    }
    notes.push(format!(
        "细纲已在位 {} 章，本轮只补 {} 章：{}（已存在的细纲一律不重写）",
        total - gaps.len() as u32,
        gaps.len(),
        chapters_brief(&gaps)
    ));
    Some(intent::outline_gap_task(&gaps, from, to, genre, words))
}

/// 完成话术按真实产出写：被跳过 / 没产出的步不算「完成」。
///
/// 以前一律写「N 个步骤全部执行完成」，而 30 个正文步可能全部在门禁处跳过
/// （payload 里 stepsDone=1/31），用户据此以为这轮没生效，只能重发——重发又会覆盖已有细纲。
fn steps_done_summary(total: usize, notes: &[String]) -> String {
    let skipped = notes
        .iter()
        .filter(|n| n.contains("跳过") || n.contains("未写") || n.contains("没有产出"))
        .count();
    if skipped == 0 {
        format!("{total} 个步骤全部执行完成。")
    } else {
        format!("{total} 个步骤执行完毕，其中 {skipped} 步没有产出（原因见下）。")
    }
}

fn step_chapter_span(step: &PlanStep) -> Option<(u32, u32)> {
    match step.range {
        Some(ChapterRange::Book) => return None,
        // 区间反了就当没写：宁可回落到文本解析，也不能让 Span(9,3) 变成「9 到 3」的空区间。
        Some(ChapterRange::Span(from, to)) if to < from => {}
        Some(range) => return Some(range.bounds()),
        None => {}
    }
    text_chapter_span(&step.task)
}

/// 这一步在处理第几章：字段 → 任务文本 → 请求里的章号。
fn step_chapter_of(step: &PlanStep, fallback: u32) -> u32 {
    match step.range {
        Some(ChapterRange::Single(chapter)) => chapter,
        Some(ChapterRange::Span(from, _)) => from,
        Some(ChapterRange::Book) => fallback,
        None => chapter_in_task(&step.task).unwrap_or(fallback),
    }
}

/// 这一步覆盖的章号区间：**任务文本优先**（仅当文本完整包住字段时），`range` 字段兜底。
///
/// 与 `normalize_step` 同一口径。闸改任务时（`force_architect` / `enforce_*`）
/// 只换 `task`、不动 `range` 字段，字段停在模型自己切的块上；谁先读字段谁就拿到
/// 过期章号——《隐藏条款》331—350 事故：任务是 331—350、字段是 331—335，
/// 于是细纲只排一块、收尾从句钉在第 335 章。
fn step_span_authoritative(step: &PlanStep) -> Option<(u32, u32)> {
    let text = text_chapter_span(&step.task);
    let field = match step.range {
        // 区间反了当没写；Book 级没有章号区间。
        Some(ChapterRange::Span(from, to)) if to < from => None,
        Some(ChapterRange::Book) | None => None,
        Some(range) => Some(range.bounds()),
    };
    match (field, text) {
        (Some(field), Some(text)) if span_includes(text, field) => Some(text),
        (Some(field), _) => Some(field),
        (None, text) => text,
    }
}

/// `outer` 是否完整包住 `inner`（章号区间归一化用）。
///
/// 只让「更宽的文本区间」接管字段，避免任务里顺手提到的一章（「参见细纲_第005章.md」）
/// 把整段批量区间压成单章。
fn span_includes(outer: (u32, u32), inner: (u32, u32)) -> bool {
    outer.0 <= inner.0 && inner.1 <= outer.1
}

fn span_covers(span: Option<(u32, u32)>, chapter: u32) -> bool {
    span.map(|(s, e)| s <= chapter && chapter <= e)
        .unwrap_or(false)
}

/// 一步的字数目标：`PlanStep::words` 优先，其次才是从任务文本里抠。
///
/// R4 之前这里只有 `clause_word_count_of` / `word_count_of` 两段正则，同一件事
/// 换一种说法（「这三万字」/「每章 3000 字」/「3000 字左右」）就会算出不同目标，
/// 用户看到的是「目标字数」在执行时飘。字段是模型/前端给出的显式值，先信它；
/// 字段无效（0 或反向区间）时当没写，继续走文本解析，别让一个坏字段把字数验收整个关掉。
fn step_words(step: &PlanStep) -> Option<(u32, u32)> {
    step.words
        .filter(|(lo, hi)| *lo > 0 && hi >= lo)
        .or_else(|| intent::clause_word_count_of(&step.task))
        .or_else(|| intent::word_count_of(&step.task))
}

/// 这一步是否覆盖某一章：`range` 字段能直接回答时就用它，否则退回文本区间。
fn step_covers_chapter(step: &PlanStep, chapter: u32) -> bool {
    match step.range {
        Some(range) => range.covers(chapter),
        None => span_covers(step_chapter_span(step), chapter),
    }
}

/// 只读步：`PlanStep::read_only` 字段优先，没写字段才看任务文本。
///
/// `RunKind` 只有 Outline / Prose / Deslop 三档，没有独立的 Check 档；一步是不是
/// 「核对/审查」由 `read_only` 字段承载，字段缺省时退回 [`ai::is_read_only_task`]
/// 的文本判定（旧计划与模型临时给的说法都走这条）。
fn step_is_readonly(step: &PlanStep) -> bool {
    step.read_only
        .unwrap_or_else(|| ai::is_read_only_task(&step.task))
}

/// 拆步后的顺序修正：只读核对步不许跑在它要核对的那一章之前。
///
/// 拆步把「写第 3-12 章正文」摊成 10 个写步，原本挂在批量步后面的
/// 「核对第 12 章」仍旧停在原位——它会先于第 12 章执行，核对的对象还不存在。
/// 规则只有两条：
/// 1. 只读步引用的章号大于此前已写到的最大章号，且同批次里有覆盖该章的写步
///    → 挪到那一步之后；
/// 2. 写步之间保持原相对顺序（不做拓扑排序：谁先谁后是作者/主 AI 定的）。
fn normalize_step_order(steps: Vec<PlanStep>) -> Vec<PlanStep> {
    let spans: Vec<Option<(u32, u32)>> = steps.iter().map(step_chapter_span).collect();
    let readonly: Vec<bool> = steps.iter().map(step_is_readonly).collect();

    // 每个只读步的锚点：它该排在哪个写步之后（None = 原地不动）。
    let mut anchor: Vec<Option<usize>> = vec![None; steps.len()];
    let mut written_max: u32 = 0;
    for i in 0..steps.len() {
        if readonly[i] {
            if let Some((_, end)) = spans[i] {
                if end > written_max {
                    let found = (i + 1..steps.len())
                        .rev()
                        .find(|&j| !readonly[j] && step_covers_chapter(&steps[j], end));
                    anchor[i] = found;
                }
            }
            continue; // 只读步不推进「已写到的章号」
        }
        if let Some((_, end)) = spans[i] {
            written_max = written_max.max(end);
        }
    }

    let mut followers: Vec<Vec<usize>> = vec![Vec::new(); steps.len()];
    for (i, target) in anchor.iter().enumerate() {
        if let Some(j) = *target {
            followers[j].push(i);
        }
    }
    let mut slots: Vec<Option<PlanStep>> = steps.into_iter().map(Some).collect();
    let mut out = Vec::with_capacity(slots.len());
    for i in 0..slots.len() {
        if anchor[i].is_some() {
            continue; // 挪走的等锚点输出时再放出来
        }
        if let Some(step) = slots[i].take() {
            out.push(step);
        }
        for k in followers[i].clone() {
            if let Some(step) = slots[k].take() {
                out.push(step);
            }
        }
    }
    out
}

/// 拆步产生的子步骤：任务文本由拆步重写，结构化字段必须跟着父步走。
///
/// 旧实现用 `..Default::default()` 构造子步，`role` / `kind` / `range` / `lock` /
/// `words` / `read_only` 全部被清零：
/// - 丢了 `range`，下游 [`step_chapter_span`] 只能再去任务文本里重抠一次章号；
/// - 丢了 `words`，字数验收退回文本正则，父步的批量总量被当成单章目标；
/// - 丢了 `read_only` / `role`，只读核对步有被当成写步派发的风险。
///
/// 这里逐字段显式继承父步，只把「子步自己覆盖的章节」换成本步区间。
fn batch_child(
    parent: &PlanStep,
    task: String,
    kind: RunKind,
    range: ChapterRange,
    chapters: (u32, u32),
    parent_chapters: u32,
) -> PlanStep {
    let (first, last) = chapters;
    // 父步的 words 是整段任务的总量（「第 3-12 章，共三万字」），子步只负责自己那几章：
    // 先按章均摊再乘自己的章数，否则每章都拿三万字当目标，验收永远不过。
    let words = parent.words.map(|(lo, hi)| {
        let per = |v: u32| v.div_ceil(parent_chapters.max(1)).max(1);
        let count = last.saturating_sub(first) + 1;
        let a = per(lo) * count;
        let b = per(hi.max(lo)) * count;
        (a.min(b), a.max(b))
    });
    PlanStep {
        agent_id: parent.agent_id.clone(),
        task,
        // 父步没写 role 时按子步语义补：正文步是写手，细纲步是架构师。
        role: parent.role.or(Some(match kind {
            RunKind::Prose => ai::AgentRole::Writer,
            _ => ai::AgentRole::Architect,
        })),
        kind: Some(kind),
        range: Some(range),
        // 细纲锁只管大纲目录的写入范围，正文步没有这个概念；
        // 父步带锁时收窄到子步区间，父步没锁时按子步章号补一把，避免子步「无锁」放开整个大纲。
        lock: match kind {
            RunKind::Outline => match parent.lock {
                Some(OutlineLock::OutlineDir) => Some(OutlineLock::OutlineDir),
                _ => Some(OutlineLock::Chapters(first, last)),
            },
            _ => None,
        },
        words,
        // 只读步在拆步前就被跳过了，能走到这里的父子步都是写步：父步没写字段时补 `false`，
        // 免得子步再去任务文本里撞上「只读」字样被误判成核对步。
        read_only: parent.read_only.or(Some(false)),
        // 事件归属随子步继承：拆步只改章号范围，不改变「这些章在为哪个事件干活」，
        // 否则批量展开后上下文注入就找不到事件，事件又退回一章一步。
        // 但「卡步标记」（空 id）绝不继承：它只说明父步当时要去排事件卡，
        // 子步已经被改写成带章号的具体任务，继承下去会让它们下次拆步时被当成卡步跳过。
        event: parent.event.clone().filter(|event| !event.is_card_step()),
        // 收尾标记也父传子：整批里最后那一步才是收尾，由 `attach_ending_clauses` 逐块判定；
        // 这里只传「父步是不是收尾批」，具体哪一块收尾由章号决定。
        ending: parent.ending,
        // 用途随 kind 走：拆步后的子步就是批量细纲/批量正文本身。
        purpose: parent.purpose,
    }
}

/// 把「第X章到第Y章」的批量任务拆成可验收的小步——跳章与字数不达标的根治手段。
///
/// 实测事故：「写第 003-045 章正文」被当成一整步塞给一个子进程，模型自己挑着写：
/// 006-010 直接消失、写出来的章还不到 6000 字。一次 CLI 会话写不完 43 章 × 6000 字
/// 是模型的能力边界，编排层必须接管节奏：
/// - 写手批量 → **每章一步**：任务原文保留，前面加「本次只写第 N 章」的硬限定；
/// - 细纲批量 → 每 [`OUTLINE_CHUNK`] 章一步：子范围重新生成任务（范围本身必须改写）。
///
/// 返回：(拆后的步骤，来自批量拆步的章号集合，给人看的说明)。
/// 章号集合给派发循环用：这些章失败/缺细纲时跳过继续，而不是像单章请求那样整个停摆。
/// 任务里从【全书收尾】到结尾的那一段（收尾从句）。
/// 任务里从【全书收尾】起、到收尾从句结束为止的字节区间。
///
/// 从句固定以「- 只允许写 … 这一个文件。」收尾；同一段任务后面还可能跟着
/// 「【作者指定结合素材】」，甚至不止一段。旧实现从标记一直截到任务末尾，于是
/// 拆步时把素材段一起塞进了收尾从句——《隐藏条款》331—335 那条细纲子步里
/// 素材段出现两遍就是这么来的。
fn ending_clause_span(task: &str) -> Option<(usize, usize)> {
    const SOURCES_MARK: &str = "【作者指定结合素材】";
    let start = task.find(intent::ENDING_MARK)?;
    let tail = &task[start..];
    let cut = tail
        .find("- 只允许写")
        .map(|at| tail[at..].find('\n').map_or(tail.len(), |i| at + i))
        .or_else(|| tail.find(SOURCES_MARK))
        .unwrap_or(tail.len());
    Some((start, start + cut))
}

/// 任务里从【全书收尾】到从句结尾的那一段（收尾从句）。
fn ending_clause_of(task: &str) -> Option<&str> {
    let (start, end) = ending_clause_span(task)?;
    Some(task[start..end].trim_end())
}

/// 去掉收尾从句、保留从句后面其它段落的原任务文本。
///
/// 未修时的真实后果：批量步拆成逐章子步后，第 301 章的子步也带着「【全书收尾】…（全书完）」，
/// 写手会在第 301 章就收尾；字数对账又因为 `ending_step` 把补写上限压成 1 次，
/// 整批章节的续写能力被一起关掉。
fn without_ending_clause(task: &str) -> String {
    match ending_clause_span(task) {
        Some((start, end)) => {
            format!("{}{}", task[..start].trim_end(), &task[end..])
                .trim_end()
                .to_string()
        }
        None => task.to_string(),
    }
}

pub(crate) fn expand_batch_steps(
    steps: Vec<PlanStep>,
    agents: &[AgentConfig],
    genre: Option<&str>,
    word_count: Option<(u32, u32)>,
    book: Option<&Path>,
) -> (Vec<PlanStep>, std::collections::HashSet<u32>, Vec<String>) {
    let mut out: Vec<PlanStep> = Vec::new();
    let mut batch_chapters = std::collections::HashSet::new();
    let mut notes: Vec<String> = Vec::new();
    let mut prose_count = 0u32;
    for step in steps {
        // 事件卡步不拆：它一次产出 1—3 张卡，本身没有章号可切；拆开只会让
        // 几张卡各带一半的未结项。判据必须同时看两样：`event` 是空 id 的卡步，
        // **并且**任务文本真的是排事件卡（见 `intent::is_event_task`）。
        // 事故：没有 大纲/事件/ 的书会先排一步「建事件卡」，闸层随后把它的任务换成
        // 批量细纲却只清了区间与锁、没清 event，于是 20 章细纲被当成卡步整块放过 → 计划只剩 1 步。
        // 判据优先级：结构化 `purpose` > （event 空 id + 任务文本）。
        // 有了 `purpose`，「这是不是事件卡步」不再依赖任务文本有没有被别的流程改写。
        let card_step = match step.purpose {
            Some(crate::models::dto::StepPurpose::EventCard) => true,
            Some(_) => false,
            None => {
                step.event.as_ref().is_some_and(|event| event.is_card_step())
                    && intent::is_event_task(&step.task)
            }
        };
        if card_step {
            out.push(step);
            continue;
        }
        // 全书级任务一律不拆步。真实事故：【增量补大纲】的任务文本里带着
        // 「第一卷：第1—40章，共40章」的示例字样，章号正则先命中，
        // 一步补写被拆成 8 步「生成第 1—40 章章级细纲」，原任务整个被替换丢掉。
        if is_book_outline_task(&step.task) {
            out.push(step);
            continue;
        }
        // 只读步一律不拆：它不产出文件，拆步只会重复出报告。真实事故：
        // tracker 的「【只读模式】连贯核对第 012—040 章」被当成细纲批量拆了 6 块，
        // 任务文本还被整段替换成「批量生成第 012—016 章章级细纲」——
        // 连贯通查追员突然开始批量写细纲。
        if step_is_readonly(&step) {
            out.push(step);
            continue;
        }
        // 只有「起点 < 终点」的真批量才拆；单章任务原样保留。
        // R4：结构化 `range` 优先。模型给的「task: 按细纲写这三万字 / range: Span(3,12)」
        // 里没有任何章号，只从任务文本抠会得到 None，整步就被当成一步执行——
        // 模型自己挑着写十章，正是这次要修的事故。字段缺省才回到文本解析。
        let range = step_span_authoritative(&step).filter(|(s, e)| e > s);
        let Some((s, e)) = range else {
            out.push(step);
            continue;
        };
        let span_len = e.saturating_sub(s) + 1;
        let agent = ai::resolve_agent(agents, &step.agent_id);
        match kind_for(&agent, Some(&step)) {
            RunKind::Prose => {
                prose_count += span_len;
                for ch in s..=e {
                    batch_chapters.insert(ch);
                    out.push(batch_child(
                        &step,
                        format!(
                            "【编排拆步 · 本次只写第 {ch} 章】{}\n\
                             （本次执行只允许产出或修改「第{ch:03}章_」开头的一个文件；\
                             其余章节由编排的后续步骤逐章派发，不要代写，也不要跳章。）",
                            // 收尾从句只属于末章：其余子步必须抹掉，否则每章都被要求「全书完」。
                            if ch < e {
                                without_ending_clause(&step.task)
                            } else {
                                step.task.clone()
                            }
                        ),
                        RunKind::Prose,
                        ChapterRange::Single(ch),
                        (ch, ch),
                        span_len,
                    ));
                }
            }
            RunKind::Outline => {
                // 细纲拆步只属于大纲架构师：kind_for 把「非正文」全归 Outline，
                // tracker/settings 等角色带章号区间的任务一旦进来，任务文本会被
                // 整段替换成「批量生成细纲」，角色却不变——真实事故：
                // 「写第012-040章正文」时 tracker 的核对步被拆成 6 块细纲批量任务。
                // 非大纲角色的区间步原样保留，由它自己一步做完。
                if agent.target_dir != "大纲" {
                    out.push(step);
                    continue;
                }
                // 子区间重写任务时，原任务里的「作者指定结合素材」段不能丢。
                let carried = intent::sources_clause_of(&step.task);
                // 事件边界优先：同一事件覆盖的连续章归一步，子步带上事件归属，
                // 写这几章细纲时取料层就能把事件卡的未结项一并塞进去。
                for (chunk_from, chunk_to, event_id) in outline_chunks(book, s, e) {
                    // 已齐的块直接不派发：重发同一句请求时，旧逻辑会把整批细纲原样覆盖。
                    let Some(mut task) =
                        outline_gap_plan(book, chunk_from, chunk_to, genre, word_count, &mut notes)
                    else {
                        continue;
                    };
                    batch_chapters.extend(chunk_from..=chunk_to);
                    if let Some(clause) = &carried {
                        task.push('\n');
                        task.push_str(clause);
                    }
                    // 收尾从句跟着末章走：只有含最后一章的那一块才说「本章是全书最后一章的细纲」。
                    if chunk_to == e {
                        if let Some(clause) = ending_clause_of(&step.task) {
                            task.push('\n');
                            task.push_str(clause);
                        }
                    }
                    let mut child = batch_child(
                        &step,
                        task,
                        RunKind::Outline,
                        ChapterRange::Span(chunk_from, chunk_to),
                        (chunk_from, chunk_to),
                        span_len,
                    );
                    if let Some(id) = event_id {
                        child.event = Some(EventRef::new(id));
                    }
                    out.push(child);
                }
            }
            _ => out.push(step),
        }
    }
    if prose_count > 0 {
        notes.push(format!(
            "正文逐章派发（共 {prose_count} 步，每章写完自动校验字数，不足自动续写）"
        ));
    }
    (out, batch_chapters, notes)
}

/// 把一段章号切成「一步能写完」的块，事件边界优先。
///
/// 没有事件卡时就是原来的「每 [`OUTLINE_CHUNK`] 章一块」；有事件卡时，同一事件
/// 覆盖的连续章归一块。块长上限仍是 `OUTLINE_CHUNK`——事件通常 2—8 章，一次写完
/// 整个事件的细纲，才谈得上「几章写同一件事」，同时又不会一口气写三十章。
fn outline_chunks(book: Option<&Path>, s: u32, e: u32) -> Vec<(u32, u32, Option<String>)> {
    let ids = book.map(|root| context::event_ids_by_chapter(root, s, e));
    let id_at = |chapter: u32| -> Option<String> {
        ids.as_ref()
            .and_then(|list| list.get((chapter - s) as usize))
            .cloned()
            .flatten()
    };
    let mut out: Vec<(u32, u32, Option<String>)> = Vec::new();
    let mut cur = s;
    while cur <= e {
        let event_id = id_at(cur);
        let mut end = cur;
        while end < e {
            let next = end + 1;
            if id_at(next) != event_id || next - cur + 1 > OUTLINE_CHUNK {
                break;
            }
            end = next;
        }
        out.push((cur, end, event_id));
        cur = end + 1;
    }
    out
}

/// 这本书还没有事件卡时，先排一步「建事件卡」再排细纲。
///
/// 条件故意收得很窄：书必须存在、这一步确实落在章号区间上、且书里一张事件卡
/// 都没有（用户点名「事件」时例外）。之后事件层由作者与模型维护，不再每章插一步——
/// 否则每张计划卡上都多一条「排事件」，很快就没人看了。
fn event_bootstrap_step(
    agents: &[AgentConfig],
    book: Option<&Path>,
    request: &str,
    subject: &str,
    genre: Option<&str>,
    span: Option<(u32, u32)>,
    chapter_level: bool,
) -> Option<PlanStep> {
    let root = book?;
    let forced = request.contains("事件");
    // 章级细纲才需要事件层；全书大纲 / 补大纲跟事件无关。
    if !forced && !chapter_level {
        return None;
    }
    if !forced && context::has_event_cards(root) {
        return None;
    }
    let agent = agents.iter().find(|a| a.target_dir == "大纲")?;
    Some(PlanStep::event_card(&agent.id, subject, genre, span))
}

/// 本地规则规划：模型没给出合法 JSON 时兜底，保证「派发动作」永远看得见。
///
/// 阶段 11 之前这里是散装关键词判断，`wants_prose` 只认「正文/写第/本章」，
/// 于是「把设定里的几个文件内容给我写了，要写一本古代的农民起事的小说」——
/// 只命中「设定」→ 只派设定类；什么都没命中就走兜底「给 architect 一句话：理解需求并给出第一步」，
/// 建筑师为了交差直接写了份细纲盖掉旧的。
///
/// 现在意图判定全部交给 [`crate::services::intent`]（带权重表、平手规则、置信度），
/// 这个函数只负责把意图翻译成「派谁、给他什么任务」：
/// - 写正文：有细纲 → 一步只派 writer；没细纲 → 先 architect 补细纲，再 writer；
/// - 只读检查：一步，派最贴题的角色，task 强制以 `【只读模式】` 开头；
/// - 排大纲 / 补设定 / 拆对标 / 找资料：一步，派对应角色；
/// - 没看懂：一步，只读，让最近的智能体「先说清你理解成了什么」，不动笔。
pub(crate) fn plan_locally(
    agents: &[AgentConfig],
    request: &str,
    chapter_hint: u32,
    book: Option<&Path>,
) -> LocalPlan {
    let mut found = intent::classify(request);
    let chapter = found.chapter.unwrap_or_else(|| chapter_hint.max(1));
    let subject = if found.subject.trim().is_empty() {
        request.trim().to_string()
    } else {
        found.subject.clone()
    };
    let genre = found.genre.as_deref();
    let pick = |dir: &str| {
        agents
            .iter()
            .find(|a| a.target_dir == dir)
            .map(|a| a.id.clone())
    };
    let task = |kind: intent::Kind| intent::step_task(kind, &subject, chapter, genre);

    // 模糊章数先反问：不许猜区间，也不许缩水成第 1 章。
    if found.needs_clarification {
        eprintln!("[storydesk] 本地规划：章节范围不明确，先反问澄清");
        let agent = pick("追踪").or_else(|| agents.first().map(|a| a.id.clone()));
        let steps = agent
            .map(|id| {
                vec![PlanStep {
                    agent_id: id,
                    task: intent::clarify_task(&found.subject),
                    ..Default::default()
                }]
            })
            .unwrap_or_default();
        return LocalPlan {
            steps,
            intent: "需要澄清 · 没说清要写哪几章：已暂停生成，请补充范围（例：001-045 的细纲）"
                .to_string(),
        };
    }

    let mut steps: Vec<PlanStep> = Vec::new();
    // 计划卡上要额外说明的事（题材切换、设定偏薄），拼到「我理解为」后面。
    let mut notes: Vec<String> = Vec::new();

    // 阶段 11.6：多意图请求按分句顺序拆成多步，不再折叠成单一步骤。
    // 「把大纲写了……再把设定里这几个都写好」→ 先 architect 写整本大纲，再 settings 补设定。
    // 写正文（Prose）不参与拆步——阶段 11.5 的整本启动流程已经自己管细纲与设定的先后。
    let sequence = intent::intent_sequence(request);
    let multi = found.kind != intent::Kind::Prose && sequence.len() >= 2;

    // 卷→章号区间：「写第一卷正文」「第一卷各章细纲」只给了卷名没给章号时，
    // 按 大纲/大纲.md 的卷规划换算；换算不出来就走「补卷章节规划」，绝不缩水成只写一章。
    let asked_volume = if found.chapter_range.is_none() && found.pending_count.is_none() {
        intent::volume_of(request)
    } else {
        None
    };
    let volume_range =
        asked_volume.and_then(|v| book.and_then(|root| context::volume_chapter_range(root, v)));
    // 「七个卷每个章节的细纲/正文」：没点卷名没给区间时，按大纲各卷章数换算全书区间。
    let all_range = if found.chapter_range.is_none()
        && found.pending_count.is_none()
        && asked_volume.is_none()
        && intent::wants_all_chapters(request)
    {
        book.and_then(context::total_chapter_range)
    } else {
        None
    };

    // 卷换算回填（单意图）：换算出的区间直接写进 chapter_range，
    // 后续分支不必再各自判 volume_range，计划卡也不会挂着「章号待定」。
    if !multi {
        if let (Some(v), Some((s, e))) = (asked_volume, volume_range) {
            found.chapter_range = Some((s, e));
            notes.push(format!("按大纲卷规划：第 {v} 卷 = 第 {s:03}-{e:03} 章"));
        } else if let Some((s, e)) = all_range {
            found.chapter_range = Some((s, e));
            notes.push(format!("按大纲卷规划：全书 = 第 {s:03}-{e:03} 章"));
        }
    }

    if multi {
        for clause in &sequence {
            match clause.kind {
                intent::Kind::Outline => {
                    let regenerate = !intent::wants_reuse(request) && !found.supplement;
                    // 卷规划换算只对章级细纲生效；明说「全书大纲」的 clause 不抢卷区间。
                    let vol_batch = if clause.outline_scope == intent::OutlineScope::Chapter {
                        volume_range.or(all_range)
                    } else {
                        None
                    };
                    let span = clause.chapter_range.or(vol_batch);
                    if !steps.iter().any(|s| s.event.is_some()) {
                        let chapter_level =
                            clause.outline_scope == intent::OutlineScope::Chapter;
                        if let Some(step) = event_bootstrap_step(
                            &agents,
                            book,
                            request,
                            &subject,
                            genre,
                            span,
                            chapter_level,
                        ) {
                            push_plan_step(&mut steps, Some(step));
                        }
                    }
                    let mut outline_skipped = false;
                    let outline = if let Some((s, e)) = span {
                        // 「001-045 的细纲」或「第一卷各章细纲」= 批量章级细纲，一步跑完整个区间。
                        if clause.chapter_range.is_none() {
                            notes.push(format!(
                                "按大纲卷规划：第 {} 卷 = 第 {s:03}-{e:03} 章",
                                asked_volume.unwrap_or(0)
                            ));
                        }
                        match outline_gap_plan(book, s, e, genre, found.word_count, &mut notes) {
                            Some(task) => task,
                            None => {
                                outline_skipped = true;
                                String::new()
                            }
                        }
                    } else if clause.outline_scope == intent::OutlineScope::Book {
                        if found.supplement {
                            intent::outline_patch_task(genre)
                        } else {
                            intent::book_outline_task(genre, regenerate)
                        }
                    } else if let Some(v) = asked_volume {
                        notes.push(format!("大纲没写第 {v} 卷的章数区间：先补该卷章节规划，补完再说「写第{v}卷正文」就能整卷执行"));
                        intent::volume_plan_task(v, genre)
                    } else {
                        intent::outline_task(
                            clause.chapter.unwrap_or(chapter),
                            genre,
                            regenerate,
                            "用户点名要大纲",
                            found.word_count,
                        )
                    };
                    if !outline_skipped {
                        push_step(&mut steps, pick("大纲"), outline);
                    }
                }
                intent::Kind::Settings => {
                    push_step(&mut steps, pick("设定"), intent::settings_fill_task(genre))
                }
                intent::Kind::Check => push_step(
                    &mut steps,
                    pick(check_dir(&clause.subject)),
                    intent::step_task(intent::Kind::Check, &clause.subject, chapter, genre),
                ),
                intent::Kind::Benchmark => push_step(
                    &mut steps,
                    pick("对标"),
                    intent::benchmark_task(request, &clause.subject, chapter, genre),
                ),
                intent::Kind::Research => push_step(
                    &mut steps,
                    pick("参考资料"),
                    intent::step_task(intent::Kind::Research, &clause.subject, chapter, genre),
                ),
                intent::Kind::Prose | intent::Kind::Unknown => {}
            }
        }
    } else {
        match found.kind {
            intent::Kind::Prose => {
                // 相对章数换算：「接着最新写 3 章」→ 起点=下一章（next_chapter_no），终点=起点+章数-1。
                let relative = found.pending_count.map(|count| {
                    let start = chapter_hint.max(1);
                    (start, start + count - 1)
                });
                if let Some((s, e)) = found.chapter_range.or(relative) {
                    // 区间写正文（含相对章数）：细纲缺失才批量补，齐了就沿用（用户点名「结合每个章节的细纲」）。
                    let health = book.map(context::setting_health);
                    if let Some(h) = health.as_ref().filter(|h| h.thin) {
                        notes.push(format!("{}：先补基础设定", h.note));
                        push_step(
                            &mut steps,
                            pick("设定"),
                            intent::settings_bootstrap_task(genre),
                        );
                    }
                    let outlines_ready = book
                        .map(|root| {
                            (s..=e).all(|c| context::check_outline(root, c, genre).reusable)
                        })
                        .unwrap_or(false);
                    if outlines_ready {
                        notes.push(format!(
                            "细纲齐备（第 {s:03}-{e:03} 章）：沿用现有细纲，跳过补细纲"
                        ));
                    } else {
                        if let Some(step) = event_bootstrap_step(
                            &agents,
                            book,
                            request,
                            &subject,
                            genre,
                            Some((s, e)),
                            true,
                        ) {
                            notes.push(
                                "事件层：这本书还没有事件卡，先排一次事件卡再排这批细纲"
                                    .to_string(),
                            );
                            push_plan_step(&mut steps, Some(step));
                        }
                        push_step(
                            &mut steps,
                            pick("大纲"),
                            intent::outline_batch_task(s, e, genre, found.word_count),
                        );
                    }
                    push_step(
                        &mut steps,
                        pick("正文"),
                        intent::writer_batch_task(s, e, genre, found.word_count),
                    );
                } else if let Some(v) = asked_volume {
                    // 说了「第X卷」但大纲没写章数区间：先补该卷章节规划，
                    // 绝不缩水成只写下一章——那正是「词不达意」的事故现场。
                    notes.push(format!("大纲没写第 {v} 卷的章数区间：本次先补该卷章节规划，补完再说「写第{v}卷正文」即可整卷批量写"));
                    push_step(&mut steps, pick("大纲"), intent::volume_plan_task(v, genre));
                } else {
                    // 阶段 11.5：细纲能不能沿用要按题材判，不能只看文件在不在。
                    // 实测事故：换了题材也沿用上一本书的细纲，正文写出 672 字节的旧章名。
                    let check = match book {
                        Some(root) => context::check_outline(root, chapter, genre),
                        None => context::OutlineCheck {
                            exists: false,
                            reusable: false,
                            reason: "还没有细纲".to_string(),
                        },
                    };
                    let constraints = book.and_then(|root| {
                        // 用户已经规定字数时，细纲里的字数（尤其是单场建议）必须让位；
                        // 只保留对白/环境比例，避免写手收到两个互相打架的目标。
                        if found.word_count.is_some() {
                            context::hard_constraints_without_word_count(root, chapter)
                        } else {
                            context::hard_constraints(root, chapter)
                        }
                    });
                    let health = book.map(context::setting_health);

                    // 「写一本/写一部/整本从头」= 新书启动：旧细纲一律作废，重新生成。
                    let regenerate = found.whole_book || !check.reusable;
                    if found.whole_book {
                        notes.push("题材切换：整本从头写，第 1 章细纲重新生成".to_string());
                    } else if !check.reusable {
                        notes.push(format!("题材核对：{}", check.reason));
                    } else if check.reason.contains("警告") {
                        // R1：题材零命中不再判作废，但要让作者看见这次是「带警告沿用」。
                        notes.push(format!("题材核对：{}", check.reason));
                    }

                    // 设定太薄就先补一步，别硬写：实测只有 3 份共 700 字、世界观/势力是空目录，
                    // 系统照样开写，用户拿到的正文跟设定毫无关系。
                    if let Some(health) = health.as_ref().filter(|h| h.thin) {
                        notes.push(format!("{}：先补基础设定", health.note));
                        push_step(
                            &mut steps,
                            pick("设定"),
                            intent::settings_bootstrap_task(genre),
                        );
                    }

                    if regenerate {
                        if let Some(step) = event_bootstrap_step(
                            &agents,
                            book,
                            request,
                            &subject,
                            genre,
                            None,
                            true,
                        ) {
                            notes.push(
                                "事件层：这本书还没有事件卡，先排一次事件卡再写细纲".to_string(),
                            );
                            push_plan_step(&mut steps, Some(step));
                        }
                        push_step(
                            &mut steps,
                            pick("大纲"),
                            intent::outline_task(
                                chapter,
                                genre,
                                true,
                                &check.reason,
                                found.word_count,
                            ),
                        );
                    }
                    push_step(
                        &mut steps,
                        pick("正文"),
                        intent::writer_task(
                            chapter,
                            genre,
                            found.word_count,
                            constraints.as_deref(),
                        ),
                    );
                }
            }
            intent::Kind::Check => push_step(
                &mut steps,
                pick(check_dir(request)),
                task(intent::Kind::Check),
            ),
            // 阶段 11.6：大纲分全书级（大纲/大纲.md）与章级（细纲_第N章.md）；
            // 点名要大纲就生成——只有明确说「按现有写」才允许沿用。
            intent::Kind::Outline => {
                let regenerate = !intent::wants_reuse(request) && !found.supplement;
                // 相对章数换算（「接着上次写 5 章细纲」）：起点=下一章。
                let relative = found.pending_count.map(|count| {
                    let start = chapter_hint.max(1);
                    (start, start + count - 1)
                });
                let was_relative = relative.is_some();
                let span = found.chapter_range.or(relative);
                let chapter_level =
                    found.outline_scope != intent::OutlineScope::Book && !found.supplement;
                if let Some(step) = event_bootstrap_step(
                    &agents,
                    book,
                    request,
                    &subject,
                    genre,
                    span,
                    chapter_level,
                ) {
                    notes.push("事件层：这本书还没有事件卡，先排一次事件卡再排章".to_string());
                    push_plan_step(&mut steps, Some(step));
                }
                let mut outline_skipped = false;
                let outline = if let Some((s, e)) = span {
                    // 区间 / 相对章数 / 卷规划换算 = 批量章级细纲，不许缩水成第 1 章。
                    if was_relative {
                        notes.push(format!("相对章数：按第 {s:03}-{e:03} 章执行"));
                    }
                    match outline_gap_plan(book, s, e, genre, found.word_count, &mut notes) {
                        Some(task) => task,
                        None => {
                            outline_skipped = true;
                            String::new()
                        }
                    }
                } else if found.outline_scope == intent::OutlineScope::Book {
                    if found.supplement {
                        intent::outline_patch_task(genre)
                    } else {
                        intent::book_outline_task(genre, regenerate)
                    }
                } else if let Some(v) = asked_volume {
                    notes.push(format!("大纲没写第 {v} 卷的章数区间：先补该卷章节规划，补完再说「第{v}卷各章细纲」就能批量排"));
                    intent::volume_plan_task(v, genre)
                } else {
                    intent::outline_task(
                        chapter,
                        genre,
                        regenerate,
                        "用户点名要大纲",
                        found.word_count,
                    )
                };
                if !outline_skipped {
                    push_step(&mut steps, pick("大纲"), outline);
                }
            }
            // 阶段 11.6：Settings 意图 = 补全设定（写文件），检查一致性会判成 Check 走只读。
            intent::Kind::Settings => {
                push_step(&mut steps, pick("设定"), intent::settings_fill_task(genre))
            }
            intent::Kind::Benchmark => {
                // 贴了书籍链接 → 先采集公开章节再拆解；计划卡上把这件事说破，别让用户以为只是空拆。
                if !intent::novel_urls(request).is_empty() {
                    notes.push("检测到书籍链接：先抓公开章节存进 对标/，再出拆解报告".to_string());
                }
                push_step(
                    &mut steps,
                    pick("对标"),
                    intent::benchmark_task(request, &subject, chapter, genre),
                );
            }
            intent::Kind::Research => {
                push_step(&mut steps, pick("参考资料"), task(intent::Kind::Research))
            }
            intent::Kind::Unknown => {
                push_step(&mut steps, pick("追踪"), task(intent::Kind::Unknown))
            }
        }
    }

    if steps.is_empty() {
        // 角色被删光了：退到第一个智能体，任务仍然是「先说清楚」，不猜着写。
        if let Some(agent) = agents.first() {
            steps.push(PlanStep {
                agent_id: agent.id.clone(),
                task: intent::step_task(intent::Kind::Unknown, &subject, chapter, genre),
                ..Default::default()
            });
        }
    }

    steps.truncate(MAX_STEPS);
    let read_only = found.read_only();
    // 阶段 11.6：多意图请求的计划卡要能看出先后（「先写整本大纲，再补全设定」）。
    let mut describe = intent::plan_summary(request);
    // plan_summary 按原话重新 classify，不知道卷换算：把「章号待定」换成换算出的区间。
    if let Some((s, e)) = volume_range {
        if describe.contains("章号待定") {
            describe = describe.replace(
                "章号待定",
                &format!("第 {s:03}-{e:03} 章（共 {} 章）", e - s + 1),
            );
        }
    }
    for note in &notes {
        describe.push_str(" · ");
        describe.push_str(note);
    }
    eprintln!(
        "[storydesk] 本地规划：{} · 证据 {} · {} 步{}",
        describe,
        found.evidence(),
        steps.len(),
        if read_only { " · 只读" } else { "" }
    );

    LocalPlan {
        steps,
        intent: describe,
    }
}

/// 调一次模型做规划；拿不到合法计划就返回 `None`（调用方退回规则）。
async fn llm_plan(
    app: &AppHandle,
    engine_kind: EngineKind,
    ws: &std::path::Path,
    book: &std::path::Path,
    agents: &[AgentConfig],
    request: &str,
    run_id: &str,
) -> Option<(Vec<PlanStep>, String)> {
    let (plan_env, _, plan_provider) = env_plan(app);
    let adapter = engine::build(
        engine_kind,
        plan_env,
        plan_provider.as_ref(),
        settings::load(app).opencode_path.as_deref(),
    )
    .ok()?;

    let prompt = engine::compose_master_prompt(agents, request);
    let capture: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
    let state = app.state::<EngineState>();
    let handle = state.insert(run_id);

    let ctx = RunCtx {
        workspace_root: ws.to_path_buf(),
        book_root: book.to_path_buf(),
        task_packet: None,
        kind: RunKind::Outline,
        chapter: None,
        cancel: handle.cancel.clone(),
        pid: handle.pid.clone(),
        capture: Some(capture.clone()),
    };

    // 规划也要有心跳：模型端不出字时，界面原本只会停在「思考中」，作者只能猜。
    let started = Instant::now();
    let beat = BeatInfo {
        run_id: run_id.to_string(),
        step_run_id: run_id.to_string(),
        index: 0,
        total: 1,
        agent_id: "planner".to_string(),
        agent_name: "主 AI".to_string(),
        target_dir: String::new(),
        chapter: None,
        done: 0,
        resume_from: None,
        persisted: false,
    };
    let mut planning_beat = beat.payload("planning", 0);
    planning_beat.status = "planning".to_string();
    let _ = app.emit(engine::MASTER_PROGRESS_EVENT, planning_beat);
    let heartbeat = spawn_step_heartbeat(app.clone(), beat.clone(), started);

    let outcome = adapter
        .run(prompt, ctx, app.clone(), run_id.to_string())
        .await
        .ok();
    heartbeat.stop();
    state.take(run_id);
    let planning_done = matches!(outcome, Some(RunOutcome::Done { .. }));
    let tail = beat.payload(
        if planning_done { "done" } else { "failed" },
        started.elapsed().as_millis() as u64,
    );
    let _ = app.emit(engine::MASTER_PROGRESS_EVENT, tail);
    engine::forget_activity(run_id);

    let got = capture.lock().map(|b| b.chars().count()).unwrap_or(0);
    eprintln!("[storydesk] 主 AI 规划：outcome={outcome:?}，文本流 {got} 字");

    if !matches!(outcome, Some(RunOutcome::Done { .. })) {
        return None;
    }

    let raw = capture.lock().ok().map(|b| b.clone()).unwrap_or_default();
    let steps = match parse_plan(&raw, agents) {
        Some(steps) => steps,
        None => {
            let head: String = raw.chars().take(400).collect();
            eprintln!("[storydesk] 主 AI 规划：没解析出计划，原始输出：{head}");
            return None;
        }
    };
    Some((steps, raw))
}

/// 只做规划，不执行（半自动模式：前端把步骤渲染成看板，用户点「确认执行」再跑）。
#[tauri::command]
pub async fn master_plan(app: AppHandle, req: MasterPlanRequest) -> Result<MasterPlan, StoryError> {
    let (ws, book) = ai_cmd::resolve_pair(&req.workspace_root, &req.book_root)?;
    let agents = agents_now(&app, &req.agents);
    let engine_kind = req.engine.unwrap_or(EngineKind::Claude);
    let run_id = ai_cmd::run_id_now();

    think(&app, &run_id, "planning", "主 AI 正在拆解任务…");

    let chapter =
        intent::chapter_of(&req.request).unwrap_or_else(|| ai_cmd::next_chapter_no(&book));
    let llm = llm_plan(
        &app,
        engine_kind,
        &ws,
        &book,
        &agents,
        &req.request,
        &run_id,
    )
    .await;

    let mut plan = match llm {
        Some((steps, raw)) => MasterPlan {
            source: "llm".to_string(),
            intent: describe_request(&req.request, &steps),
            steps,
            raw,
        },
        None => {
            let local = plan_locally(&agents, &req.request, chapter, Some(&book));
            MasterPlan {
                source: "rules".to_string(),
                intent: Some(local.intent),
                steps: local.steps,
                raw: String::new(),
            }
        }
    };
    // 阶段 11.5：不管计划是谁给的，题材与设定这两道闸都要过。
    enforce_topic_and_settings(&mut plan, &book, &agents, &req.request, chapter);
    // 规则闸会「按意图补步」，这一步再按用户的【不要做】/【本次只做一件事】把它多派的步摘掉。
    let found = intent::classify(&req.request);
    enforce_requested_scope(Some(&app), &mut plan, &req.request, &found);
    // 用户点名的结合素材（参考资料/追踪/对标）写进大纲与正文步骤，计划卡所见即执行所得。
    attach_sources_clause(&mut plan, &agents, &req.request);
    // 阶段 11.6（R4 标准化层）：规划这条路也必须把六个结构化字段补齐——
    // 模型带了字段就用模型的，没带就从任务文本解析。以前只在执行前做标准化，
    // 于是「计划卡上看到的」和「真正执行的」是两份不同结构。
    plan.steps = std::mem::take(&mut plan.steps)
        .into_iter()
        .map(|step| normalize_step(step, &agents))
        .collect();

    let _ = app.emit(
        engine::MASTER_PLAN_EVENT,
        MasterPlanEvent {
            run_id,
            source: plan.source.clone(),
            intent: plan.intent.clone(),
            steps: plan.steps.clone(),
            kind: None,
        },
    );
    Ok(plan)
}

/// 一步该走哪条流水线。
///
/// R4：`PlanStep::kind` 优先；字段为空（旧计划、模型直接给的中文任务、拆步器产出的子步）
/// 才回到「按角色目录猜」——写正文目录 → Prose（要过前置门禁），其余 → Outline。
fn kind_for(agent: &AgentConfig, step: Option<&PlanStep>) -> RunKind {
    if let Some(kind) = step.and_then(|s| s.kind) {
        return kind;
    }
    if agent.target_dir == "正文" {
        RunKind::Prose
    } else {
        RunKind::Outline
    }
}

/// 计划卡上那句「我理解为」。
///
/// 作者原话是第一手证据：先按原话判定（多意图按 [`intent::plan_summary`] 的序列说），
/// 判不出来（Unknown）才退到「第一步派给了谁」。
fn describe_request(request: &str, steps: &[PlanStep]) -> Option<String> {
    let found = intent::classify(request);
    if found.needs_clarification {
        return Some(
            "章节范围不明确 · 需要澄清：请补一句要写哪几章（例：001-045 的细纲）".to_string(),
        );
    }
    if found.kind != intent::Kind::Unknown {
        return Some(intent::plan_summary(request));
    }
    intent_hint(steps)
}

/// 模型给的计划没有「意图说明」，就从第一步派给谁反推一句给用户看的话。
///
/// 为什么不用 task 文本去 `classify`：模型写的 task 里常带着「冲突」「分析」这类词，
/// 按关键词判会把「生成细纲」误读成「只读检查」。派给哪个角色，比那句任务描述更可信。
fn intent_hint(steps: &[PlanStep]) -> Option<String> {
    let first = steps.first()?;
    let kind = first
        .role
        .or_else(|| ai::AgentRole::parse(&first.agent_id))
        .map(|role| match role {
            ai::AgentRole::Writer => intent::Kind::Prose,
            ai::AgentRole::Architect => intent::Kind::Outline,
            ai::AgentRole::Tracker => intent::Kind::Check,
            ai::AgentRole::Settings => intent::Kind::Settings,
            ai::AgentRole::Benchmarker => intent::Kind::Benchmark,
            ai::AgentRole::Researcher => intent::Kind::Research,
        })
        .unwrap_or_else(|| intent::classify(&first.task).kind);
    let read_only = step_is_readonly(first);
    Some(format!(
        "{}（按第一步派给的「{}」推断{})",
        kind.label(),
        first.agent_id,
        if read_only {
            "，只读不改文件"
        } else {
            ""
        }
    ))
}

/// 从「我理解为」那句里把题材捞回来（`… · 题材「古代的农民起事」 · …`）。
///
/// 为什么不重新 classify 一遍 request：`master_execute` 拿到的是用户确认过的计划，
/// request 可能已经不在手边；意图说明本身带着题材，直接读它最省事。
fn topic_from_intent(intent: Option<&str>) -> Option<String> {
    let text = intent?;
    let start = text.find("题材「")? + "题材「".len();
    let rest = &text[start..];
    let end = rest.find('」')?;
    let topic = rest[..end].trim();
    if topic.is_empty() {
        None
    } else {
        Some(topic.to_string())
    }
}

/// 意图类别 → 该由哪个角色出这一步。只用于「本次只做一件事」收窄计划。
fn role_of_kind(kind: intent::Kind) -> ai::AgentRole {
    match kind {
        intent::Kind::Prose => ai::AgentRole::Writer,
        intent::Kind::Outline => ai::AgentRole::Architect,
        intent::Kind::Settings => ai::AgentRole::Settings,
        intent::Kind::Benchmark => ai::AgentRole::Benchmarker,
        intent::Kind::Research => ai::AgentRole::Researcher,
        intent::Kind::Check | intent::Kind::Unknown => ai::AgentRole::Tracker,
    }
}

/// 计划成型后的最后一道边界过滤：用户明说不要的产物、以及「本次只做一件事」之外的步骤。
///
/// `enforce_outline_and_settings` 是「按意图补步」的规则闸：它只看 `found.kind`，
/// 不读用户写的【不要做】。实测事故：用户写「【本次只做一件事】设定 /【不要做】不写正文、不写大纲」，
/// 模型只给了一步 settings，规则闸却把整条请求判成「排大纲」，凭空插了一步
/// architect「【增量补大纲】」放在第 0 位——「只补设定」变成了「先补大纲再补设定」。
/// prompt 里写一句「不要派生」治不了它（那是代码在派生），所以必须在计划成型后过滤。
/// 过滤结果写进 plan.intent（界面可见）并落引擎日志，绝不静默改计划。
fn enforce_requested_scope(
    // 拿不到 AppHandle（单测、非 Tauri 上下文）时退回 eprintln，不静默吞掉收窄结果。
    app: Option<&AppHandle>,
    plan: &mut MasterPlan,
    request: &str,
    found: &intent::Intent,
) {
    if request.trim().is_empty() {
        return;
    }
    let log = |scope: &str, note: &str| match app {
        Some(app) => engine::log_failure(app, scope, note),
        None => eprintln!("[storydesk] {scope}：{note}"),
    };
    let forbids_prose = intent::forbids(request, intent::Kind::Prose);
    let single = intent::only_one_thing(request).then(|| role_of_kind(found.kind));
    let before = plan.steps.clone();
    let mut dropped: Vec<&str> = Vec::new();
    let mut kept: Vec<PlanStep> = Vec::with_capacity(plan.steps.len());
    for step in std::mem::take(&mut plan.steps) {
        let role = ai::AgentRole::parse(&step.agent_id);
        if forbids_prose && role == Some(ai::AgentRole::Writer) {
            dropped.push("写正文");
            continue;
        }
        if role == Some(ai::AgentRole::Architect) {
            // 大纲分两层：全书大纲与章级细纲。用户说「不要细纲」不等于「不要大纲」，
            // 所以按这一步自己的产物判，而不是一刀切。
            // 点名 大纲/大纲.md 且不带章号锁定的是全书大纲：即使 task 里顺带提了
            // 「不要写章级细纲」也不能因此按章级口径判禁。
            let book_level =
                step.task.contains("大纲/大纲.md") && !step.task.contains("细纲_第");
            let chapter_level =
                !book_level && (step.task.contains("细纲") || step.task.contains("章纲"));
            let banned = if chapter_level {
                intent::forbids_chapter_outline(request)
            } else {
                intent::forbids(request, intent::Kind::Outline)
            };
            if banned {
                dropped.push(if chapter_level { "细纲" } else { "大纲" });
                continue;
            }
        }
        if let Some(allow) = single {
            // 「本次只做一件事」= 计划里只能有这一类产物，其它角色的步一律摘掉。
            if role.is_some() && role != Some(allow) {
                dropped.push("与本次唯一交付物无关的步骤");
                continue;
            }
        }
        kept.push(step);
    }
    if kept.is_empty() {
        // 过滤把计划摘空了：与其让作者对着空计划发愣，不如保留原计划并把原因说清楚。
        plan.steps = before;
        let note = "没能按【不要做】/【本次只做一件事】收窄计划（过滤后会一步不剩），已按原计划执行，请核对步骤";
        plan.intent = Some(match plan.intent.take() {
            Some(text) if !text.contains(note) => format!("{text} · {note}"),
            Some(text) => text,
            None => note.to_string(),
        });
        log("计划收窄失败", note);
        return;
    }
    plan.steps = kept;
    if dropped.is_empty() {
        return;
    }
    dropped.sort_unstable();
    dropped.dedup();
    let note = format!(
        "按【不要做】/【本次只做一件事】移除了无关步骤（{}）",
        dropped.join("、")
    );
    plan.intent = Some(match plan.intent.take() {
        Some(text) if !text.contains(&note) => format!("{text} · {note}"),
        Some(text) => text,
        None => note.clone(),
    });
    log("计划已收窄", &note);
}

/// 模型给的计划也要过一遍题材与设定这两道闸。
///
/// 阶段 11 只修了本地规则（`plan_locally`），但真跑的是模型给的计划——
/// 模型照样写「已有细纲就沿用」，于是旧细纲又活过来了。这一层是对**任何来源**的计划都生效的补丁：
/// 1. 细纲与题材不符 / 整本从头写 → 把「沿用」改成「重新生成」，没有 architect 步就补一步；
/// 2. 设定太薄 → 补一步补设定，并在「我理解为」后面写明原因。
fn enforce_topic_and_settings(
    plan: &mut MasterPlan,
    book: &Path,
    agents: &[AgentConfig],
    request: &str,
    chapter_hint: u32,
) {
    let found = intent::classify(request);
    // 模糊章数：模型自己猜区间也一样是瞎跑——整份计划换成一步「反问澄清」。
    if found.needs_clarification {
        plan.steps = match agents
            .iter()
            .find(|a| a.target_dir == "追踪")
            .or_else(|| agents.first())
        {
            Some(agent) => vec![PlanStep {
                agent_id: agent.id.clone(),
                task: intent::clarify_task(&found.subject),
                ..Default::default()
            }],
            None => Vec::new(),
        };
        plan.intent = Some("需要澄清 · 章节范围没说清，已拦截所有生成类步骤".to_string());
        return;
    }
    let genre = found.genre.clone();
    let chapter = found.chapter.unwrap_or_else(|| {
        plan.steps
            .iter()
            .find_map(|s| chapter_in_task(&s.task))
            .unwrap_or(chapter_hint)
    });
    let has_writer = plan
        .steps
        .iter()
        .any(|s| ai::AgentRole::parse(&s.agent_id) == Some(ai::AgentRole::Writer));

    // 写正文请求：不管模型派了谁，最终必须落到「用户点名的章 + 用户字数」上。
    // 真实事故：用户要第 002 章，模型派 architect 写第 5 章细纲、writer 改第 1 章正文。
    if found.kind == intent::Kind::Prose {
        enforce_prose_plan(plan, book, agents, &found, chapter, genre.as_deref());
    } else if found.kind == intent::Kind::Outline || !has_writer {
        // 阶段 11.6：没有写手步的计划也要过「大纲 / 设定」这两道闸。
        // 实测事故：用户要「整本故事的大纲 + 补设定」，模型给了一步
        // 「生成第 1 章细纲（已有细纲就沿用，不要覆盖）」——层级降级 + 静默沿用 + 丢意图，
        // 因为老的闸只认有写手的计划，这一步直接放行了。
        // 细纲意图**哪怕带着写手步也必须过大纲闸**。真实事故（《隐藏条款》331—350）：
        // 模型给「第 331 到 335 章细纲」的 arch 步 + 一条批量写手步，写手步让计划漏到
        // 「非写正文却带写手步」分支，大纲闸根本没跑：arch 区间停在 331—335、
        // 收尾从句被钉在第 335 章，336—350 的细纲整批没派发。
        enforce_outline_and_settings(plan, agents, request, &found, chapter_hint, Some(book));
        if found.kind == intent::Kind::Outline && intent::wants_outline_only(request) {
            // 用户明说「不写正文」：写手步是模型自带的噪声，别把几十章正文白跑一遍。
            let before = plan.steps.len();
            plan.steps
                .retain(|s| ai::AgentRole::parse(&s.agent_id) != Some(ai::AgentRole::Writer));
            if plan.steps.len() != before {
                let note = "本请求只做细纲（不写正文）：已移除写正文步骤";
                plan.intent = Some(match plan.intent.take() {
                    Some(text) if !text.contains(note) => format!("{text} · {note}"),
                    Some(text) => text,
                    None => note.to_string(),
                });
            }
        }
        attach_ending_clauses(plan, agents, Some(book), request);
        attach_scope_warnings(plan, Some(book));
        enforce_rule_edit_pass(plan, request);
        return;
    } else if found.chapter.is_some() {
        // 非写正文意图却带写手步：至少把章号钉到用户点名的那一章。
        let writer = intent::writer_task(
            chapter,
            genre.as_deref(),
            found.word_count,
            prose_constraints(book, chapter, found.word_count).as_deref(),
        );
        force_writer(plan, agents, writer, true);
    }

    // 设定太薄：补一步，别硬写。模型不爱派设定这一步，所以在计划层强制加。
    let health = context::setting_health(book);
    if health.thin {
        let note = format!("{}：先补基础设定", health.note);
        plan.intent = Some(match plan.intent.take() {
            Some(text) if !text.contains("先补基础设定") => format!("{text} · {note}"),
            Some(text) => text,
            None => note,
        });
        let already = plan
            .steps
            .iter()
            .any(|s| ai::AgentRole::parse(&s.agent_id) == Some(ai::AgentRole::Settings));
        if !already {
            if let Some(settings_agent) = agents.iter().find(|a| a.target_dir == "设定") {
                plan.steps.insert(
                    0,
                    PlanStep {
                        agent_id: settings_agent.id.clone(),
                        task: intent::settings_bootstrap_task(genre.as_deref()),
                        ..Default::default()
                    },
                );
                plan.steps.truncate(MAX_STEPS);
            }
        }
    }
    attach_ending_clauses(plan, agents, Some(book), request);
    attach_scope_warnings(plan, Some(book));
    enforce_rule_edit_pass(plan, request);
}

/// 批量格式修订（如「把每章对白占比统一改成 25%-35%」）：模型爱逐个打开细纲
/// 整文件重写，而章级文件锁只放行第一个文件，第二个起全被当越界回滚（真实事故）。
/// 统一钉上目录级锁标记，并追加「只替换相关行、用脚本批量处理」的铁律。
fn enforce_rule_edit_pass(plan: &mut MasterPlan, request: &str) {
    if !intent::wants_rule_edit(request) {
        return;
    }
    for step in plan.steps.iter_mut() {
        if ai::AgentRole::parse(&step.agent_id) != Some(ai::AgentRole::Architect) {
            continue;
        }
        if !["细纲", "每章", "各章", "每一章", "卷纲"]
            .iter()
            .any(|k| step.task.contains(k))
        {
            continue;
        }
        if !step.task.contains("【大纲目录级】") {
            step.task = format!("【大纲目录级】{}", step.task);
        }
        if !step.task.contains("【格式修订铁律】") {
            step.task.push_str(
                "\n【格式修订铁律】本步是格式修订，不是创作：对每个文件只替换与本次规定相关的行，\
                 禁止重写整文件、禁止改动剧情内容、禁止新建细纲或正文文件；\
                 文件多时先用命令行脚本批量替换匹配行，不要逐个打开重写；\
                 全局规定优先落在 设定/ 与 大纲/大纲.md 的卷级规定上，逐章文件里只改对应条目。",
            );
        }
    }
}

/// 把用户原话点名的结合素材（参考资料/追踪/对标）写进大纲与正文步骤的任务文本。
///
/// 闸层模板只固定读 设定/ 与 细纲——用户「结合参考资料、追踪」的要求以前在
/// 任务被改写成模板的那一刻蒸发，计划卡与派发任务就此脱节（实测事故）。
/// 确认执行路径 request 为空：段落已经嵌在任务文本里，随任务原样带走，这里天然 no-op。
fn attach_sources_clause(plan: &mut MasterPlan, agents: &[AgentConfig], request: &str) {
    let Some(clause) = intent::sources_clause(request) else {
        return;
    };
    let mut attached = 0usize;
    for step in &mut plan.steps {
        let dir = ai::resolve_agent(agents, &step.agent_id).target_dir;
        if (dir == "大纲" || dir == "正文") && !step.task.contains("【作者指定结合素材】")
        {
            step.task.push('\n');
            step.task.push_str(&clause);
            attached += 1;
        }
    }
    if attached > 0 {
        eprintln!("[storydesk] 结合素材注入 {attached} 步：{clause}");
    }
}

/// 用户已经规定字数时，细纲里的字数（尤其是单场建议）必须让位；只保留比例/风格。
fn prose_constraints(book: &Path, chapter: u32, word_count: Option<(u32, u32)>) -> Option<String> {
    if word_count.is_some() {
        context::hard_constraints_without_word_count(book, chapter)
    } else {
        context::hard_constraints(book, chapter)
    }
}

/// 写正文计划的总闸：锁定章号、锁定用户字数、该补的细纲补上、多余的错章步骤删掉。
fn enforce_prose_plan(
    plan: &mut MasterPlan,
    book: &Path,
    agents: &[AgentConfig],
    found: &intent::Intent,
    chapter: u32,
    genre: Option<&str>,
) {
    if let Some((s, e)) = found.chapter_range {
        // 细纲齐备就不再强制批量重生成：用户说「结合每个章节的细纲来完成正文」时，
        // 细纲是**已有输入**。以前这个分支无条件补一步批量细纲，既多跑 N/5 步，
        // 又可能把用户已有的细纲整段覆盖——规划与执行就此脱节（实测事故）。
        let outlines_ready = (s..=e).all(|c| context::check_outline(book, c, genre).reusable);
        if outlines_ready {
            plan.steps
                .retain(|st| ai::AgentRole::parse(&st.agent_id) != Some(ai::AgentRole::Architect));
        } else {
            force_architect(
                plan,
                agents,
                intent::outline_batch_task(s, e, genre, found.word_count),
            );
        }
        force_writer(
            plan,
            agents,
            intent::writer_batch_task(s, e, genre, found.word_count),
            false,
        );
        let lock = if outlines_ready {
            format!("区间锁定：只写第 {s:03}-{e:03} 章 · 细纲齐备：沿用现有细纲，跳过补细纲")
        } else {
            format!("区间锁定：只写第 {s:03}-{e:03} 章")
        };
        plan.intent = Some(match plan.intent.take() {
            Some(text) if !text.contains("区间锁定") => format!("{text} · {lock}"),
            Some(text) => text,
            None => lock,
        });
        return;
    }

    let check = context::check_outline(book, chapter, genre);
    let regenerate = found.whole_book || !check.reusable;
    if regenerate {
        let outline = intent::outline_task(chapter, genre, true, &check.reason, found.word_count);
        force_architect(plan, agents, outline);
        let note = if found.whole_book {
            "题材切换：整本从头写，第 1 章细纲重新生成".to_string()
        } else {
            format!("题材核对：{}", check.reason)
        };
        plan.intent = Some(match plan.intent.take() {
            Some(text) if !text.contains(&note) => format!("{text} · {note}"),
            Some(text) => text,
            None => note,
        });
    } else {
        // 细纲能沿用：所有 architect 步都摘掉。它跑一趟只会读一遍再回一句「沿用」，
        // 更危险的是模型会顺手去写别的章号（真实事故：要第 2 章，建筑师写了第 5 章细纲）。
        plan.steps
            .retain(|s| ai::AgentRole::parse(&s.agent_id) != Some(ai::AgentRole::Architect));
    }

    let constraints = prose_constraints(book, chapter, found.word_count);
    let writer = intent::writer_task(chapter, genre, found.word_count, constraints.as_deref());
    force_writer(plan, agents, writer, true);
    // 计划卡上明确写出章号锁定，用户确认前就能看出 AI 到底要写哪一章。
    let lock = format!("章号锁定：只写第 {chapter:03} 章");
    plan.intent = Some(match plan.intent.take() {
        Some(text) if !text.contains("章号锁定") => format!("{text} · {lock}"),
        Some(text) => text,
        None => lock,
    });
}

/// 只保留一个 architect 步，并把任务换成权威版本；没有就补在第一步。
fn force_architect(plan: &mut MasterPlan, agents: &[AgentConfig], task: String) {
    let mut seen = false;
    plan.steps.retain(|s| {
        if ai::AgentRole::parse(&s.agent_id) == Some(ai::AgentRole::Architect) {
            if seen {
                return false;
            }
            seen = true;
        }
        true
    });
    if seen {
        for step in plan.steps.iter_mut() {
            if ai::AgentRole::parse(&step.agent_id) == Some(ai::AgentRole::Architect) {
                step.task = task.clone();
                // 文本换了，模型/旧计划留下的区间与文件锁就是过期数据：
                // 事故现场 arch 步文本是「第 331 到 350 章」，字段却还是模型给的 331—335，
                // 收尾从句与拆步都按字段取终点 → 全书完被钉在第 335 章。留着就会复发。
                // `event` 同理：任务已经不是排事件卡了，卡步标记留着会让拆步跳过这一步。
                step.event = None;
                step.range = None;
                step.lock = None;
            }
        }
    } else if let Some(architect) = agents.iter().find(|a| a.target_dir == "大纲") {
        plan.steps.insert(
            0,
            PlanStep {
                agent_id: architect.id.clone(),
                task,
                ..Default::default()
            },
        );
    }
}

/// 只保留一个写手步（单章）或换成一个权威批量步，并把任务换成权威版本。
fn force_writer(plan: &mut MasterPlan, agents: &[AgentConfig], task: String, single: bool) {
    if !single {
        // 区间写正文只需要一个批量任务，派发时再逐章拆；模型给的多条写手步一律收掉。
        plan.steps
            .retain(|s| ai::AgentRole::parse(&s.agent_id) != Some(ai::AgentRole::Writer));
        if let Some(writer) = agents.iter().find(|a| a.target_dir == "正文") {
            plan.steps.push(PlanStep {
                agent_id: writer.id.clone(),
                task,
                ..Default::default()
            });
        }
        return;
    }
    let mut seen = false;
    plan.steps.retain(|s| {
        if ai::AgentRole::parse(&s.agent_id) == Some(ai::AgentRole::Writer) {
            if seen {
                return false;
            }
            seen = true;
        }
        true
    });
    if seen {
        for step in plan.steps.iter_mut() {
            if ai::AgentRole::parse(&step.agent_id) == Some(ai::AgentRole::Writer) {
                step.task = task.clone();
                // 同上：任务文本换了，旧的章号区间/文件锁全部作废，交给 normalize_step 按文本重算。
                step.range = None;
                step.lock = None;
                step.event = None;
            }
        }
    } else if let Some(writer) = agents.iter().find(|a| a.target_dir == "正文") {
        plan.steps.push(PlanStep {
            agent_id: writer.id.clone(),
            task,
            ..Default::default()
        });
    }
}

/// 阶段 11.6：没有写手步的计划（写大纲 / 补设定 / 检查）也要被修正。
///
/// 实测事故（真实失败案例）：用户说「把大纲写了，整本故事的大纲写好，再把设定里
/// 这几个都写好」，模型只给了一步 architect「生成第 1 章细纲（已有细纲就沿用，
/// 不要覆盖）」。三个问题一次全犯：全书大纲降级成章细纲、强生成词下静默沿用、
/// 设定意图整个丢掉。这里对**任何来源**的计划打三块补丁：
/// 1. 大纲意图 → architect 的任务按层级重写（全书 → `大纲/大纲.md`，章级 → `细纲_第N章.md`），
///    「已有…就沿用」必须换掉（除非用户明说要沿用）；没有 architect 步就补一步；
/// 2. 请求里还带着设定意图 → architect 步之后必须跟一步 settings 补设定；
/// 3. 设定意图为主的 → 至少要有一个 settings 步。
fn enforce_outline_and_settings(
    plan: &mut MasterPlan,
    agents: &[AgentConfig],
    request: &str,
    found: &intent::Intent,
    chapter_hint: u32,
    book_root: Option<&std::path::Path>,
) {
    let genre = found.genre.as_deref();
    let sequence = intent::intent_sequence(request);
    let wants_settings = sequence.iter().any(|i| i.kind == intent::Kind::Settings);

    if found.kind == intent::Kind::Outline {
        let regenerate = !intent::wants_reuse(request) && !found.supplement;
        let book = found.outline_scope == intent::OutlineScope::Book;
        let chapter = found.chapter.unwrap_or_else(|| {
            plan.steps
                .iter()
                .find_map(|s| chapter_in_task(&s.task))
                .unwrap_or(chapter_hint)
        });
        let range = found.chapter_range.or_else(|| {
            // 「七个卷每个章节的细纲」没给章号区间时，按 大纲/大纲.md 的各卷章数换算全书区间。
            if !book && intent::wants_all_chapters(request) {
                book_root.and_then(context::total_chapter_range)
            } else {
                None
            }
        });
        let outline = if let Some((s, e)) = range {
            // 模型把「001-045 的细纲」降级成「生成第 1 章细纲」时，这里改写成批量任务。
            intent::outline_batch_task(s, e, genre, found.word_count)
        } else if book {
            if found.supplement {
                intent::outline_patch_task(genre)
            } else {
                intent::book_outline_task(genre, regenerate)
            }
        } else {
            intent::outline_task(
                chapter,
                genre,
                regenerate,
                "用户点名要大纲",
                found.word_count,
            )
        };
        let is_arch =
            |s: &PlanStep| ai::AgentRole::parse(&s.agent_id) == Some(ai::AgentRole::Architect);
        let arch_count = plan.steps.iter().filter(|s| is_arch(s)).count();
        // 真实事故：模型把全书细纲拆成 4 步（索引 + 分段区间），修正器把同一条
        // 单章规则任务逐步盖章，计划卡变成 4×「重新生成第 1 章细纲」。
        // 规则任务只有在「批量 / 全书级」时才代表整体，才允许收编替换并去重；
        // 章级多步而规则给不出批量任务时，保留模型的分步，只钉目录级锁标记。
        let mut has_architect = false;
        if range.is_some() || book {
            let mut seen = false;
            let mut drop: Vec<usize> = Vec::new();
            for (i, step) in plan.steps.iter_mut().enumerate() {
                if !is_arch(step) {
                    continue;
                }
                has_architect = true;
                if seen {
                    drop.push(i);
                    continue;
                }
                seen = true;
                // 全书级请求里，模型任务已经对准 大纲/大纲.md、没降级成章细纲、
                // 也没和用户意图打架（要补充却写「作废 / 重新生成」、要强生成却写「沿用」）：
                // 保留原任务——它往往比规则模板更贴合原话。
                // 真实事故：模型写对的「为七个卷逐一补充章数」被覆盖成「重写整本、旧大纲作废」。
                let fights_intent = (found.supplement
                    && (step.task.contains("作废") || step.task.contains("重新生成")))
                    || (regenerate && step.task.contains("沿用"));
                let on_target = book
                    && step.task.contains("大纲/大纲.md")
                    && !step.task.contains("细纲_第")
                    && !fights_intent;
                if !on_target {
                    step.task = outline.clone();
                    // 换成本次请求的批量任务后，旧区间（模型给的「331—335」这类分块）必须作废。
                    step.range = None;
                    step.lock = None;
                    step.event = None;
                } else if found.supplement {
                    // 保留模型任务不等于放任执行：事故现场里模型补完章数后
                    // 自己加写了细纲，又凭记忆整文件重写 大纲/大纲.md 把补充冲掉。
                    // 必须前置锁标记：outline_lock_for 按标记认锁，
                    // 裸模型任务没有标记会被当成章级细纲锁，写对的 大纲.md 反被恢复回旧版。
                    step.task = format!("【增量补大纲】{}", step.task);
                    step.task.push_str(
                        "\n【执行铁律】本步唯一交付物是 大纲/大纲.md：禁止创建或修改 大纲/细纲_第N章.md 与 正文/ 下任何文件；\
                         对 大纲/大纲.md 只做最小插入修改，禁止凭记忆整文件重写覆盖既有内容；补充完成后立即停止，不得继续做别的任务。",
                    );
                } else {
                    // 同上：非补充的全书级保留任务也要有标记，否则合法的大纲.md 写入会被越界检查冲掉。
                    step.task = format!("【全书大纲】{}", step.task);
                }
            }
            for i in drop.into_iter().rev() {
                plan.steps.remove(i);
            }
        } else if arch_count > 1 || intent::wants_rule_edit(request) {
            // 多步分章计划、或「修改每章规定」这类格式修订：保留模型任务
            // （它更懂要改哪一条），目录级标记与铁律由 enforce_rule_edit_pass 统一钉。
            has_architect = true;
            for step in plan.steps.iter_mut() {
                if is_arch(step) && !step.task.contains("【大纲目录级】") {
                    step.task = format!("【大纲目录级】{}", step.task);
                }
            }
        } else {
            for step in plan.steps.iter_mut() {
                if is_arch(step) {
                    has_architect = true;
                    // 模型已经把任务对准 大纲/大纲.md（整本大纲），而意图被判成章级：
                    // 这是把全书大纲压成单章细纲的降级事故（《山海不夜》2026-11-18 现场：
                    // 「全书大纲」被改写成「重新生成第 1 章细纲」）。宁可保留模型的整本
                    // 大纲任务，也不拿章级模板覆盖它。
                    if step.task.contains("大纲/大纲.md") && !step.task.contains("细纲_第") {
                        continue;
                    }
                    step.task = outline.clone();
                    step.range = None;
                    step.lock = None;
                    step.event = None;
                }
            }
        }
        if !has_architect {
            if let Some(architect) = agents.iter().find(|a| a.target_dir == "大纲") {
                plan.steps.insert(
                    0,
                    PlanStep {
                        agent_id: architect.id.clone(),
                        task: outline,
                        ..Default::default()
                    },
                );
            }
        }
    }

    // 请求里还带着「把设定写好」→ 计划里必须有这一步，排在 architect 之后。
    if (found.kind == intent::Kind::Settings || wants_settings)
        && !plan
            .steps
            .iter()
            .any(|s| ai::AgentRole::parse(&s.agent_id) == Some(ai::AgentRole::Settings))
    {
        if let Some(settings_agent) = agents.iter().find(|a| a.target_dir == "设定") {
            plan.steps.push(PlanStep {
                agent_id: settings_agent.id.clone(),
                task: intent::settings_fill_task(genre),
                ..Default::default()
            });
            plan.steps.truncate(MAX_STEPS);
        }
    }

    // 任务文本刚被闸改过：锁字段必须跟着文本走，否则执行期按旧章级锁注入
    // 「【章号锁定】只允许写 大纲/细纲_第00N章.md」，全书大纲任务被压成单章细纲。
    resync_outline_locks(plan, agents);
}

/// 一整轮主从编排：规划 → 逐步派发 → 回传终局。
#[tauri::command]
pub async fn master_run(
    app: AppHandle,
    req: MasterRunRequest,
) -> Result<MasterRunResult, StoryError> {
    let (ws, book) = ai_cmd::resolve_pair(&req.workspace_root, &req.book_root)?;
    let agents = agents_now(&app, &req.agents);
    // 阶段 9：工具白名单是全局设置（角色自己写了 `tools` 则优先用角色的）
    let app_settings = settings::load(&app);
    let allowed_tools = app_settings.allowed_tools.clone();
    let retry_pref = app_settings.retry.clone();
    let engine_kind = req.engine.unwrap_or(EngineKind::Claude);
    let run_id = req.run_id.clone().unwrap_or_else(ai_cmd::run_id_now);
    // 断点续跑：从这个下标往后跑，之前的步读状态文件跳过（`None` = 全新一轮）。
    let resume_from = req.resume_from.unwrap_or(0);
    // 章号优先级：用户原话点名的章 > 计划步骤里点名的章 > 前端传的章号 > 正文目录的下一章。
    // 步骤里的章号同样字段优先：模型给的 range 比任务文本更可信（文本可能压根没写章号）。
    let plan_chapter = req.steps.as_ref().and_then(|steps| {
        steps
            .iter()
            .find_map(|s| step_chapter_span(s).map(|(first, _)| first))
    });
    let chapter = resolve_chapter(
        &req.request,
        plan_chapter,
        req.chapter,
        ai_cmd::next_chapter_no(&book),
    );

    // ---- 0) 滚动摘要缺口补齐
    // R1：摘要索引归编排器所有（写手只在回复里给一行，见 intent.rs 的【章节摘要】条款）。
    // 开工前先把历史缺口补上：正文在、索引里却没有这一章的行，前情提成就永久缺章，
    // 而此前没有任何一处会把它报出来（实测《隐藏条款》缺第 8/12/13 章）。
    let missing_summaries = memory::missing_chapters(&book);
    if !missing_summaries.is_empty() {
        match memory::backfill(&book) {
            Ok(reports) => {
                let filled: Vec<String> = reports
                    .iter()
                    .filter(|report| report.wrote)
                    .map(|report| format!("第{}章", report.chapter))
                    .collect();
                let text = if filled.is_empty() {
                    format!(
                        "摘要索引缺 {} 章，但正文里取不到可摘录内容，本次先照旧跑。",
                        missing_summaries.len()
                    )
                } else {
                    format!(
                        "摘要索引补齐：{}（写手没给行，按正文开头+结尾摘录）。",
                        filled.join("、")
                    )
                };
                eprintln!("[storydesk] {text}");
                think(&app, &run_id, "thinking", &text);
            }
            Err(e) => engine::log_failure(&app, "摘要索引补齐失败", &e),
        }
    }

    // ---- 0.5) 设定快照
    // R3 项 6：每一步的 prompt 都要求「先通读 设定/追踪 全部全文」，30 章 = 30 次重复注入。
    // 每轮开工前压一份 .storydesk/设定快照.md，prompt 改为先读快照、细节按需回原文。
    let snapshot_abs = crate::services::setting_snapshot::ensure(&book).map(|report| {
        eprintln!(
            "[storydesk] 设定快照{}：{}（{} 个来源，{} 字）",
            if report.written {
                "已更新"
            } else {
                "命中"
            },
            report.rel,
            report.sources,
            report.chars
        );
        report.abs
    });

    // ---- 1) 规划
    // 阶段 8：前端已经确认过的计划（`master_execute`）直接执行，不再调模型规划，
    // 否则「确认执行」等于把规划再烧一遍 Token，用户确认了个寂寞。
    let mut plan = if let Some(steps) = req.steps.clone().filter(|steps| !steps.is_empty()) {
        think(&app, &run_id, "planning", "计划已确认，开始派发。");
        MasterPlan {
            source: req
                .source
                .clone()
                .unwrap_or_else(|| "confirmed".to_string()),
            intent: describe_request(&req.request, &steps),
            steps,
            raw: String::new(),
        }
    } else {
        think(&app, &run_id, "planning", "主 AI 正在拆解任务…");
        let llm = llm_plan(
            &app,
            engine_kind,
            &ws,
            &book,
            &agents,
            &req.request,
            &run_id,
        )
        .await;
        match llm {
            Some((steps, raw)) => MasterPlan {
                source: "llm".to_string(),
                intent: describe_request(&req.request, &steps),
                steps,
                raw,
            },
            None => {
                think(
                    &app,
                    &run_id,
                    "thinking",
                    "模型没有给出合法计划，改用本地规则拆解。",
                );
                let local = plan_locally(&agents, &req.request, chapter, Some(&book));
                MasterPlan {
                    source: "rules".to_string(),
                    intent: Some(local.intent),
                    steps: local.steps,
                    raw: String::new(),
                }
            }
        }
    };
    // 阶段 11.5：只有「这一轮刚规划出来」才补闸；用户确认过的计划原样执行（他改过的东西不能再被改）。
    if req.steps.as_ref().map(|s| s.is_empty()).unwrap_or(true) {
        enforce_topic_and_settings(&mut plan, &book, &agents, &req.request, chapter);
        // 规则闸多派的步（典型：用户明说不要大纲，仍插一步「【增量补大纲】」）在这里摘掉。
        let found = intent::classify(&req.request);
        enforce_requested_scope(Some(&app), &mut plan, &req.request, &found);
        // 新规划路径补注入结合素材；确认执行路径的段落本来就嵌在任务文本里，原样带走。
        attach_sources_clause(&mut plan, &agents, &req.request);
    }
    // 阶段 11.6（R4 标准化层）：不管是模型给的、本地规则给的，还是用户在前端改过的计划，
    // 进编排前统一补齐 role/kind/range/lock/words/read_only。模型只写一句「按细纲写这三万字」
    // 时 range 能从文本解析出来；模型给了 `range: Span(3,12)` 而任务文本没写章号时，
    // 字段本身才是唯一可信来源——两者都在这里合成同一份结构。
    // 放在「emit 计划卡片」之前：用户看到的计划卡片与真正执行的结构必须是同一份。
    plan.steps = std::mem::take(&mut plan.steps)
        .into_iter()
        .map(|step| normalize_step(step, &agents))
        .collect();
    let _ = app.emit(
        engine::MASTER_PLAN_EVENT,
        MasterPlanEvent {
            run_id: run_id.clone(),
            source: plan.source.clone(),
            intent: plan.intent.clone(),
            steps: plan.steps.clone(),
            kind: None,
        },
    );

    // ---- 1.5) 批量任务拆步（跳章 / 字数不达标的根治）。
    // 「写第 003-045 章正文」以前是一整步塞给一个子进程，模型自己挑着写：
    // 006-010 直接消失。这里把写手批量拆成每章一步、细纲批量按 5 章分组，
    // 派发节奏回到编排层手里。
    let req_found = intent::classify(&req.request);
    let mut word_count = req_found.word_count;
    let mut genre = req_found.genre.clone();
    // 确认执行路径 request 为空：从写手任务文本里恢复字数硬要求与题材。
    // 初次规划路径绝不能从任务文本里「恢复」字数——LLM 写的任务可能带着细纲的
    // 单场「字数建议 2200-2600」，一恢复就会把用户没规定的字数当成硬要求，
    // 反而压掉细纲的全章目标。用户没规定时，字数交给细纲硬约束。
    let confirmed_plan = req.request.trim().is_empty();
    // 用户这次明说不要大纲/细纲：门禁的「可修复」补步不能再插大纲步（见下面的 Repairable 分支）。
    let forbid_outline_fix = intent::forbids(&req.request, intent::Kind::Outline)
        || intent::forbids_chapter_outline(&req.request);
    if (confirmed_plan && word_count.is_none()) || genre.is_none() {
        for step in &plan.steps {
            if ai::AgentRole::parse(&step.agent_id) != Some(ai::AgentRole::Writer) {
                continue;
            }
            if confirmed_plan && word_count.is_none() {
                // R4：字段优先——确认执行时 steps 是前端传回来的，`words` 就是用户在计划卡片上
                // 认可过的字数目标；文本解析只是旧数据 / 模型只写自然语言的回退。
                // 成对抽取优先：从句「不少于 X 字、不超过 Y 字」用单值容差解析会把 hi 估错。
                word_count = step_words(&step);
            }
            if genre.is_none() {
                genre = intent::genre_of(&step.task).or_else(|| genre_from_task(&step.task));
            }
        }
    }
    let before_expand = plan.steps.len();
    let (expanded, mut batch_chapters, expand_notes) = expand_batch_steps(
        std::mem::take(&mut plan.steps),
        &agents,
        genre.as_deref(),
        word_count,
        Some(&book),
    );
    let was_expanded = expanded.len() != before_expand;
    plan.steps = normalize_step_order(expanded);
    if !expand_notes.is_empty() {
        let suffix = expand_notes.join("；");
        plan.intent = Some(match plan.intent.take() {
            Some(text) if !text.contains(&suffix) => format!("{text} · {suffix}"),
            text => text.unwrap_or(suffix),
        });
    }
    // 计划卡上的步数要与实际执行的步数一致：第一次 plan 事件发在拆步之前
    // （用户要先看到「我理解成了什么」才点确认），拆完再补一条终版。
    let _ = app.emit(
        engine::MASTER_PLAN_EVENT,
        MasterPlanEvent {
            run_id: run_id.clone(),
            source: "编排拆步".to_string(),
            intent: plan.intent.clone(),
            steps: plan.steps.clone(),
            kind: Some("plan-final".to_string()),
        },
    );
    // 派发卡片与收尾事件用的步数：拆步之后是真实步数（如 45 步），不是拆前的 2 步。
    let total = plan.steps.len();
    if was_expanded {
        think(
            &app,
            &run_id,
            "thinking",
            &format!("批量任务已拆成 {total} 小步（正文逐章派发），开始执行。"),
        );
    } else {
        think(
            &app,
            &run_id,
            "thinking",
            &format!("已拆成 {total} 步，开始派发。"),
        );
    }

    // ---- 2) 逐步派发
    let scripts = gate::resolve(&ws);
    let (plan_env, notes, plan_provider) = env_plan(&app);
    let mut results: Vec<MasterStepResult> = Vec::new();
    let mut status = "done".to_string();
    let mut summary = String::new();
    // 阶段 8：上一步的产出 / 它的序号，用来给下一步当上下文。
    let mut produced: Vec<String> = Vec::new();
    let mut producer: usize = 0;
    // 阶段 11.5：题材从计划里的「我理解为」那句里捞出来，每一步都写进 prompt 的最高优先级段。
    let topic = topic_from_intent(plan.intent.as_deref());
    if let Some(topic) = &topic {
        eprintln!("[storydesk] 本次题材：{topic}");
    }

    // 阶段 8：把目录隔离 hook 落进这本书的 `.claude/`（幂等，失败只记日志）。
    match dir_guard::ensure(&book) {
        Err(e) => engine::log_failure(&app, "目录隔离 hook 没写成", &e),
        // hook 写了但跑起来没 python：隔离是 fail-open 的，界面不能继续说「已硬隔离」。
        Ok(_) => {
            if let Some(note) = dir_guard::isolation_degraded(&book) {
                engine::log_failure(&app, "目录隔离降级", note);
            }
        }
    }
    // 预装开源写作 skill（去AI味 / 长篇拆文）：同样幂等，已存在不覆盖。
    for rel in skills::ensure(&book) {
        eprintln!("[storydesk] 预装写作 skill：{rel}");
    }

    // 拆步后用下标循环：字数验收不达标时要往里追加「续写」步骤，接着跑。
    let mut steps = std::mem::take(&mut plan.steps);
    // 断点续跑的状态文件：每步开始/结束都落盘，失败停下后「从第 N 步继续」靠它。
    let mut run_state =
        run_state::begin(&book, &run_id, &req.request, req.source.as_deref(), &plan);
    if resume_from > 0 {
        let text = format!(
            "断点续跑：从第 {} 步继续（前 {} 步已完成）。",
            resume_from + 1,
            run_state.done_count()
        );
        eprintln!("[storydesk] {text}");
        think(&app, &run_id, "thinking", &text);
    }
    // 收尾追加进 summary 的账目：跳过的章、补写后仍不足的章。
    let mut extra_notes: Vec<String> = Vec::new();
    // 每章已自动补写的次数（上限 MAX_WORD_RETRIES，防无限循环烧 Token）。
    let mut word_retries: HashMap<u32, u32> = HashMap::new();
    // 每章上一次续写后的字数，用于判断是否「卡住」（增量低于 MIN_PROGRESS_PER_RETRY）。
    let mut last_word_counts: HashMap<u32, usize> = HashMap::new();
    // 每章已自动精简的次数（上限 MAX_TRIM_RETRIES，超出上限的返工账）。
    let mut trim_retries: HashMap<u32, u32> = HashMap::new();
    // 已经为哪些章插过补步：同一章最多补一次，否则门禁一直不过就会无限插步。
    let mut repaired_chapters: std::collections::HashSet<u32> = std::collections::HashSet::new();
    let mut index = 0usize;
    while let Some(step) = steps.get(index).cloned() {
        let total = steps.len();
        // 续跑：状态文件说这步已经成了，就不再派一遍（重复派发会重写同一章）。
        run_state.sync(&steps);
        if index < resume_from || run_state.is_done(index) {
            eprintln!("[storydesk] 第 {} 步已完成，续跑跳过。", index + 1);
            index += 1;
            continue;
        }
        let agent = ai::resolve_agent(&agents, &step.agent_id);
        let step_run_id = format!("{run_id}-step{}", index + 1);
        let step_kind = kind_for(&agent, Some(&step));
        let step_role = step.role.or_else(|| ai::AgentRole::parse(&step.agent_id));
        // 主 AI 可能把一句话拆成跨章任务（先出第 1 章细纲再写第 1 章正文），
        // 所以章号以这一步任务里写的「第 N 章」为准，没写才退回请求里的章号。
        let step_chapter = step_chapter_of(&step, chapter);
        // 「这一步真的在处理某一章」只对大纲架构师与写手成立。设定/追踪/资料这些角色与章无关：
        // 以前它们被当成章节步（`kind_for` 只分 正文/非正文），于是每轮都吃到
        // 「生成 大纲/细纲_第005章.md」的任务包与本章细纲上下文（截图 2-3：设定管理员回报
        // 「任务包目标文件在 大纲/，超出本角色只写 设定/ 的权限边界」，界面显示「带入 大纲\细纲\第005章.md」）。
        let acts_on_chapter = matches!(
            step_role,
            Some(ai::AgentRole::Architect) | Some(ai::AgentRole::Writer)
        );
        let chapter_scope = if acts_on_chapter {
            Some(step_chapter)
        } else {
            None
        };

        // 按「离这一步的章号多远」挑目录清单：`大纲/` 有 301 个细纲时，
        // 写第 200 章必须看到第 200 章附近的细纲，而不是文件名排在最前的 40 个。
        let mut files =
            ai_cmd::collect_context_files_near(&book, &agent.target_dir, &[], Some(step_chapter));
        // 写正文的步骤只看「自己这一章」的正文文件：把别的章全塞进上下文，
        // 写手就会读一读、然后「贴心地」决定去写下一章（实测事故：点名 001 却写了 004）。
        // 前情提要已由 global_context 注入，别的章正文在这里只会诱导跑偏。
        if step_kind == RunKind::Prose {
            files.retain(|rel| {
                rel.replace('\\', "/").starts_with("正文/")
                    && prose_file_chapter(rel).is_some_and(|no| no == step_chapter)
                    || !rel.replace('\\', "/").starts_with("正文/")
            });
        }
        // 阶段 8：把上一步刚产出的文件带进这一步的上下文（"写正文"要看见"细纲"）。
        if index > 0 && !produced.is_empty() {
            let shown: Vec<String> = produced
                .iter()
                .map(|p| p.rsplit('/').next().unwrap_or(p).to_string())
                .collect();
            let text = format!(
                "[系统] 正在将步骤 {} 的产出『{}』作为上下文传递给下一步…",
                producer + 1,
                shown.join("、")
            );
            eprintln!("[storydesk] {text}");
            let _ = app.emit(
                engine::MASTER_CONTEXT_EVENT,
                MasterContext {
                    run_id: run_id.clone(),
                    index,
                    total,
                    from_index: producer,
                    files: produced.clone(),
                    text,
                },
            );
            for file in &produced {
                if !files.iter().any(|existing| existing == file) {
                    files.push(file.clone());
                }
            }
        }
        // 验收凭据：这一步的 Prompt 里到底带了哪些文件（第二行应能看到上一步的细纲）
        eprintln!(
            "[storydesk] 第 {}/{} 步上下文文件（{} 个）：{}",
            index + 1,
            total,
            files.len(),
            if files.is_empty() {
                "（无）".to_string()
            } else {
                files.join(", ")
            }
        );
        // 阶段 10：全局上下文（本章细纲 / 设定 / 追踪 / 参考资料）——之前完全没注入，
        // 于是细纲写着「2400–2600 字、对白四成」，正文照样跑偏。
        // `chapter_scope` 为空 = 这一步与章无关（补设定/追踪/资料），不要塞本章细纲。
        let global = context::global_context_of(&book, chapter_scope, &files);
        eprintln!(
            "[storydesk] 第 {}/{} 步全局上下文（{} 个）：{}",
            index + 1,
            total,
            global.len(),
            if global.is_empty() {
                "（无）".to_string()
            } else {
                global.join(", ")
            }
        );
        let read_only = step_is_readonly(&step);
        if read_only {
            eprintln!(
                "[storydesk] 第 {}/{} 步判定为只读：摘掉 Write/Edit/MultiEdit",
                index + 1,
                total
            );
        }
        // 大纲任务包只发给大纲架构师：任务包里写死了「在 大纲/ 下生成 细纲_第N章.md」，
        // 发给设定/追踪/资料角色就是让它去写不属于自己目录的文件。
        let packet = if step_kind == RunKind::Outline
            && step_role == Some(ai::AgentRole::Architect)
            && scripts.brief_ready()
        {
            let chapters = match outline_lock_for(&step, step_chapter) {
                OutlineLock::Chapters(start, end) => Some((start, end)),
                OutlineLock::Book | OutlineLock::OutlineDir => None,
            };
            gate::build_task_packet(&ws, &book, "outline", None, chapters)
                .ok()
                .map(|p| std::path::PathBuf::from(p.path))
        } else {
            None
        };
        // 阶段 10：写正文的步骤，把细纲里的字数/比例要求抽成硬约束，接到「补充要求」末尾。
        // 用户点名了字数时，细纲里的字数（尤其是单场建议）必须让位；用户字数最后再钉一次，
        // 保证它排在所有细纲要求之后，模型不会按更低的区间提前收尾。
        let mut note = step.task.clone();
        // R4：字数优先级 = 用户本轮点名的字数 > 这一步的结构化 words > 任务文本里写的字数。
        // words 为 0 或区间反了视为没给，不能让一个坏字段把字数验收整个废掉。
        let step_word_count = word_count.or_else(|| step_words(&step));
        if step_kind == RunKind::Prose && !read_only {
            if let Some(extra) = prose_constraints(&book, step_chapter, step_word_count) {
                eprintln!("[storydesk] 第 {}/{} 步硬约束：{extra}", index + 1, total);
                note.push('；');
                note.push_str(&extra);
            }
            if let Some(wc) = step_word_count {
                if !note.contains("字数硬要求（最高优先级）") {
                    note.push('；');
                    note.push_str(&intent::word_count_clause(Some(wc), false));
                }
            }
            // 章号锁定（编排层注入，不管计划来自本地模板还是 LLM）：
            // 实测事故——任务写明「撰写 001 章」，写手看到 正文/ 里已有别的章，
            // 自己决定去写/覆盖 第004章_血路.md。章号必须在 prompt 里钉死。
            if !note.contains("章号锁定") {
                note.push_str(&format!(
                    "；【章号锁定】本步只允许产出或覆盖「第{step_chapter:03}章_」开头的这一个文件；\
                     正文/ 下的其它章节文件一律不要写入、修改、续写或重写；\
                     如果第 {step_chapter} 章已有旧稿，就在「第{step_chapter:03}章_」文件上重写到达标，\
                     绝不因为已有后续章而转去写别的章。"
                ));
            }
        }
        if step_kind == RunKind::Outline
            && step_role == Some(ai::AgentRole::Architect)
            && !read_only
        {
            let lock = outline_lock_for(&step, step_chapter);
            if let OutlineLock::Chapters(start, end) = lock {
                if let Some(wc) = step_word_count {
                    if !note.contains("正文目标字数") {
                        note.push('；');
                        note.push_str(&intent::outline_word_count_clause(Some(wc)));
                    }
                }
                if !note.contains("章号锁定") {
                    if start == end {
                        note.push_str(&format!(
                            "；【章号锁定】本步只允许写 大纲/细纲_第{start:03}章.md 这一个细纲文件；\
                             其它章节的细纲与全书大纲一律不要修改、覆盖或重写。"
                        ));
                    } else {
                        note.push_str(&format!(
                            "；【章号锁定】本步只允许写第 {start:03} 章到第 {end:03} 章的细纲文件；\
                             区间外的章节细纲与全书大纲一律不要修改、覆盖或重写。"
                        ));
                    }
                }
            } else if !note.contains("文件锁定") {
                note.push_str(
                    "；【文件锁定】本步只允许写 大纲/大纲.md；\
                     绝不要写 大纲/细纲_第N章.md，也不要改其它章节的细纲。",
                );
            }
        }
        let contract = ai_cmd::build_prompt(&book, packet.as_deref(), Some(&note));
        let mut step_tools = ai::effective_tools(&agent, allowed_tools.as_deref());
        if read_only {
            step_tools = ai::read_only_tools(&step_tools);
        }
        // 对标采集步骤：联网抓页是任务本身的一部分，白名单里必须给到 WebFetch/WebSearch，
        // 否则 prompt 里「除此之外不要调用其它工具」会把采集步骤锁死成只能干瞪眼。
        if agent.target_dir == "对标" && step.task.contains("对标采集") && !read_only {
            for t in ["WebFetch", "WebSearch"] {
                if !step_tools.iter().any(|x| x == t) {
                    step_tools.push(t.to_string());
                }
            }
            step_tools = ai::normalize_tools(&step_tools);
        }
        // 阶段 13（记忆功能）：写正文/细纲的步骤注入上一章前情提要（细纲要点 + 开头/结尾）。
        // 与章无关的步骤（补设定/追踪/资料）不注入：给它别的章的前情只会诱导它去写正文。
        let prev_digest = if acts_on_chapter {
            context::prev_chapter_digest(&book, step_chapter)
        } else {
            None
        };
        if let Some(digest) = prev_digest.as_deref() {
            eprintln!(
                "[storydesk] 第 {}/{} 步注入前情提要（{} 字）",
                index + 1,
                total,
                digest.chars().count()
            );
        }
        let prompt = engine::compose_agent_prompt(
            &agent,
            &step_tools,
            &files,
            &global,
            read_only,
            topic.as_deref(),
            &contract,
            prev_digest.as_deref(),
            snapshot_abs.as_deref(),
        );
        eprintln!(
            "[storydesk] 第 {}/{} 步允许的工具：{}",
            index + 1,
            total,
            if step_tools.is_empty() {
                "（一个都没勾）".to_string()
            } else {
                step_tools.join("、")
            }
        );

        // 派发卡片：前端拿它渲染「📤 已将任务派发给 …」
        let _ = app.emit(
            engine::MASTER_DISPATCH_EVENT,
            MasterDispatch {
                run_id: run_id.clone(),
                index,
                total,
                agent_id: agent.id.clone(),
                agent_name: agent.name.clone(),
                avatar: agent.avatar.clone(),
                target_dir: agent.target_dir.clone(),
                task: step.task.clone(),
                step_run_id: step_run_id.clone(),
            },
        );
        eprintln!(
            "[storydesk] 主 AI 派发：[{}] {} → {}（第 {}/{} 步）",
            agent.id,
            step.task,
            step_kind_label(step_kind),
            index + 1,
            total
        );
        // 改过「智能体角色」里的提示词之后，这行就是验收凭据：必须是新内容。
        eprintln!(
            "[storydesk] 角色提示词：[{}] {}",
            agent.id, agent.system_prompt
        );
        eprintln!(
            "[storydesk] 子智能体提示词（前 2 行）：\n{}",
            first_lines(&prompt, 2)
        );

        // 写正文：先过前置门禁（细纲不在就别启动引擎）
        if step_kind == RunKind::Prose && !read_only {
            match gate::precheck_prose(&ws, &book, step_chapter) {
                Ok(out) if !out.ok => {
                    // 门禁分级：不是所有拦截都值得让整轮编排停在这里。
                    // 可修复（缺细纲 / 细纲太空 / 事件卡对不上）→ 插一步补上，然后回来重试本步；
                    // 必须人工（细纲不止一份、脚本报错）→ 照旧停下并说明原因。
                    let (verdict, remedy) = gate::classify_block(step_chapter, &out);
                    let block = hook_parse::gate_block(&step_run_id, &out, step_chapter);
                    let _ = app.emit(engine::BLOCKED_EVENT, block);
                    results.push(MasterStepResult {
                        index,
                        agent_id: agent.id.clone(),
                        agent_name: agent.name.clone(),
                        run_id: step_run_id,
                        status: "blocked".to_string(),
                        exit: 2,
                        duration_ms: 0,
                        stderr: Some(out.stderr.clone()),
                        attempts: 0,
                        error: engine::error_brief(&out.stderr),
                        resumable: !batch_chapters.contains(&step_chapter),
                    });
                    run_state.set(
                        index,
                        "blocked",
                        0,
                        engine::error_brief(&out.stderr),
                    );
                    run_state.sync(&steps);
                    if let Err(e) = run_state.write(&book) {
                        engine::log_failure(&app, "运行状态没写进去", &e);
                    }
                    // 可修复门禁：插一步把缺的东西补上，然后原地重试写正文。
                    // 以前这里一律 `break`（非批量）或跳过该章（批量），把「差一份细纲」
                    // 当成「整轮计划作废」——修好细纲还得从头再确认一遍计划。
                    if verdict == crate::models::dto::GateVerdict::Repairable
                        && !repaired_chapters.contains(&step_chapter)
                    {
                        let arch_id = agents
                            .iter()
                            .find(|a| a.target_dir == "大纲")
                            .map(|a| a.id.clone())
                            .unwrap_or_else(|| "architect".to_string());
                        // 用户明说不要大纲/细纲：缺细纲也不再自动插一步 architect，
                        // 否则「不要大纲」的请求照样被派一次大纲任务。事件卡补步与大纲无关，照旧放行。
                        if forbid_outline_fix
                            && matches!(remedy, crate::models::dto::GateRemedy::WriteOutline(_))
                        {
                            let note = format!(
                                "第 {step_chapter} 章缺细纲，但这次请求明说不要大纲/细纲：不自动补大纲步，已停下并说明原因。"
                            );
                            engine::log_failure(&app, "门禁缺细纲不再自动补", &note);
                            think(&app, &run_id, "thinking", &note);
                            break;
                        }
                        let fix = match remedy {
                            crate::models::dto::GateRemedy::WriteOutline(ch) => Some((
                                intent::outline_batch_task(ch, ch, None, step.words),
                                crate::models::dto::StepPurpose::OutlineBatch,
                                Some(OutlineLock::Chapters(ch, ch)),
                                format!("第 {ch} 章缺细纲"),
                            )),
                            crate::models::dto::GateRemedy::FixEventCard(ch) => Some((
                                intent::event_task(
                                    topic.as_deref().unwrap_or("本书"),
                                    None,
                                    Some((ch, ch)),
                                ),
                                crate::models::dto::StepPurpose::EventCard,
                                Some(OutlineLock::OutlineDir),
                                format!("第 {ch} 章的事件卡与细纲对不上"),
                            )),
                            crate::models::dto::GateRemedy::None => None,
                        };
                        if let Some((task, purpose, lock, why)) = fix {
                            repaired_chapters.insert(step_chapter);
                            let fix_step = PlanStep {
                                agent_id: arch_id,
                                task,
                                role: Some(ai::AgentRole::Architect),
                                kind: Some(RunKind::Outline),
                                range: Some(ChapterRange::Single(step_chapter)),
                                lock,
                                purpose: Some(purpose),
                                read_only: Some(false),
                                ..Default::default()
                            };
                            steps.insert(index, fix_step.clone());
                            run_state.insert_step(index, &fix_step);
                            run_state.sync(&steps);
                            if let Err(e) = run_state.write(&book) {
                                engine::log_failure(&app, "运行状态没写进去", &e);
                            }
                            let text = format!(
                                "门禁拦下第 {step_chapter} 章（{why}）：先插一步补上，补完自动回来写这一章的正文。"
                            );
                            eprintln!("[storydesk] {text}");
                            think(&app, &run_id, "thinking", &text);
                            // 不推进 index：下一轮先跑这条补步，跑完再回到本章正文。
                            continue;
                        }
                    }
                    // 批量拆步里的章：缺细纲只跳过这一章，后面的章继续——
                    // 一章缺细纲就让整批停摆，正是「写 3-45 章」只出几章的帮凶。
                    if batch_chapters.contains(&step_chapter) {
                        // 以前这里一律写「缺细纲」：真实原因（细纲太薄 / 细纲不止一份 / 事件冲突）
                        // 被抹平成同一句话，用户看不出该改哪儿。
                        let why = {
                            let stdout = out.stdout.trim();
                            if stdout.is_empty() {
                                out.stderr.trim().to_string()
                            } else {
                                stdout.to_string()
                            }
                        };
                        let text = if why.is_empty() {
                            format!("第 {step_chapter} 章未写，批量流程跳过该章继续。")
                        } else {
                            format!("第 {step_chapter} 章未写，批量流程跳过该章继续：{why}")
                        };
                        eprintln!("[storydesk] {text}");
                        extra_notes.push(text);
                        index += 1;
                        continue;
                    }
                    status = "blocked".to_string();
                    summary = format!(
                        "第 {} 步被门禁拦下（{}），后续步骤不再派发：先补细纲再写正文。",
                        index + 1,
                        out.stderr.trim()
                    );
                    break;
                }
                Ok(out) => {
                    // 提示性放行（事件层缺卡）：不拦，但要让作者看见这一章按什么规矩写。
                    let note = out.stdout.trim();
                    if note.starts_with("事件层提示") {
                        eprintln!("[storydesk] {note}");
                        extra_notes.push(note.to_string());
                    }
                }
                Err(e) => engine::log_failure(&app, "前置门禁没跑成", &e),
            }
        }

        // 阶段 8：告诉 hook「这次只允许改哪个目录」，并记下这轮开始前的磁盘状态。
        let mut step_env = plan_env.clone();
        step_env.vars.push((
            dir_guard::ALLOW_ENV.to_string(),
            book.join(&agent.target_dir).display().to_string(),
        ));
        let before = snapshot_book(&book);

        run_state.set(index, "running", 0, None);
        run_state.sync(&steps);
        if let Err(e) = run_state.write(&book) {
            engine::log_failure(&app, "运行状态没写进去", &e);
        }

        // 状态已经写成 running：从这里往下的每一条早退路径都必须收尾，
        // 否则状态文件永久停在 running，前端也永远等不到 done 事件。
        let adapter = match engine::build(
            engine_kind,
            step_env,
            plan_provider.as_ref(),
            settings::load(&app).opencode_path.as_deref(),
        ) {
            Ok(adapter) => adapter,
            Err(e) => {
                let note = format!(
                    "第 {} 步（{}）没起来：{e}。进度已保存，修好后可从第 {} 步继续。",
                    index + 1,
                    agent.name,
                    index + 1
                );
                run_state.set(index, "failed", 0, Some(e.to_string()));
                run_state.sync(&steps);
                if let Err(write_err) = run_state.write(&book) {
                    engine::log_failure(&app, "运行状态没写进去", &write_err);
                }
                let _ = app.emit(
                    engine::MASTER_DONE_EVENT,
                    MasterDone {
                        run_id: run_id.clone(),
                        status: "failed".to_string(),
                        text: note.clone(),
                        steps_done: run_state.done_count(),
                        steps_total: total,
                    },
                );
                engine::append_engine_log(
                    &app,
                    &format!("step {step_run_id} 引擎没起来：{e}"),
                );
                return Err(e);
            }
        };
        let state = app.state::<EngineState>();
        // RAII：无论这一步怎么结束（正常 / 早退 / panic），都保证从运行表里摘掉。
        let (slot, handle) = engine::RunSlot::open(&state, &step_run_id);
        // R1：摘要行只从写手的**最终回复**里取，所以这一步要把文本流留下来。
        // 以前这里是 None，回复一结束就没了，摘要只能指望写手自己去改索引文件。
        let capture: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
        let ctx = RunCtx {
            workspace_root: ws.clone(),
            book_root: book.clone(),
            task_packet: packet.clone(),
            kind: step_kind,
            chapter: Some(step_chapter),
            cancel: handle.cancel.clone(),
            pid: handle.pid.clone(),
            capture: Some(capture.clone()),
        };

        let started = Instant::now();
        // 已完成步数与续跑点直接取自刚落过盘的状态文件：界面显示的数字和「关掉窗口后
        // 还能从哪一步接着跑」永远一致，不靠前端自己数。
        let done_now = run_state.done_count();
        let resume_now = run_state.first_unfinished().map(|at| at + 1);
        let persisted_now = run_state::run_path(&book, &run_id)
            .map(|path| path.exists())
            .unwrap_or(false);
        let beat = BeatInfo {
            run_id: run_id.clone(),
            step_run_id: step_run_id.clone(),
            index,
            total,
            agent_id: agent.id.clone(),
            agent_name: agent.name.clone(),
            target_dir: agent.target_dir.clone(),
            chapter: if acts_on_chapter {
                Some(step_chapter)
            } else {
                None
            },
            done: done_now,
            resume_from: resume_now,
            persisted: persisted_now,
        };
        // 先发一条 0 秒的心跳：卡片从派发那一刻起就有活着的证据，而不是等 2 秒。
        let _ = app.emit(engine::MASTER_PROGRESS_EVENT, beat.payload("running", 0));
        let heartbeat = spawn_step_heartbeat(app.clone(), beat.clone(), started);
        // 引擎偶发失败（opencode 本地 server 500：请求根本没到模型）按策略指数退避重试。
        // 次数/基数/倍数/上限都在设置里（RetryPref），不再写死「一次 + 1.5s」：
        // 现场那种持续几分钟的故障，一次 1.5s 的重试等于没重试。
        let mut attempts: u32 = 1;
        let outcome = {
            let mut last = adapter
                .run(prompt.clone(), ctx.clone(), app.clone(), step_run_id.clone())
                .await;
            loop {
                let transient = match &last {
                    Ok(RunOutcome::Failed { stderr, .. }) => engine::is_transient_error(stderr),
                    _ => false,
                };
                if !transient || attempts >= retry_pref.attempts() {
                    if let Ok(RunOutcome::Failed { stderr, .. }) = &last {
                        if transient {
                            engine::append_engine_log(
                                &app,
                                &format!("step {step_run_id} 第 {attempts} 次仍失败：{stderr}"),
                            );
                        }
                    }
                    break last;
                }
                let wait = retry_pref.wait_ms(attempts);
                let brief: String = match &last {
                    Ok(RunOutcome::Failed { stderr, .. }) => stderr
                        .lines()
                        .next()
                        .unwrap_or("")
                        .chars()
                        .take(120)
                        .collect(),
                    _ => String::new(),
                };
                let note = format!(
                    "引擎偶发错误（{brief}），第 {}/{} 次重试，等待 {:.1}s……",
                    attempts + 1,
                    retry_pref.attempts(),
                    wait as f64 / 1000.0
                );
                eprintln!("[storydesk] {note}");
                engine::append_engine_log(
                    &app,
                    &format!("step {step_run_id} 第 {attempts} 次失败（偶发），将重试：{brief}"),
                );
                think(&app, &step_run_id, "retry", &note);
                tokio::time::sleep(std::time::Duration::from_millis(wait)).await;
                let mut fresh = ctx.clone();
                fresh.capture = Some(Arc::new(Mutex::new(String::new())));
                attempts += 1;
                last = adapter
                    .run(prompt.clone(), fresh, app.clone(), step_run_id.clone())
                    .await;
            }
        };
        heartbeat.stop();
        let duration = started.elapsed().as_millis() as u64;
        // 先摘运行表，再往下做体检：后面任何早退都不会留下僵尸条目。
        drop(slot);

        // 阶段 8：磁盘前后差分，算出这一步到底写了什么——后面两步都要用：
        // ① 交给下一步当上下文；② 越界检查（hook 没跑到时这里是唯一的证据）。
        // 同时保存修改前的内容：错章/越界覆盖了作者旧稿时要恢复，不能只移进回收站了事。
        let after = snapshot_book(&book);
        let mut produced_now: Vec<String> = after
            .mtimes
            .iter()
            .filter(|(rel, mtime)| before.mtimes.get(*rel) != Some(*mtime))
            .map(|(rel, _)| rel.clone())
            .collect();
        produced_now.sort();

        // 被误删的文件（前后快照里消失）：原样恢复，别让模型的一次删除毁掉旧稿。
        let deleted: Vec<String> = before
            .mtimes
            .keys()
            .filter(|rel| !after.mtimes.contains_key(*rel))
            .cloned()
            .collect();
        if !deleted.is_empty() {
            let restored = restore_files(&book, &deleted, &before.contents);
            if !restored.is_empty() {
                let text = format!(
                    "[系统] 检测到误删文件：{}（已按修改前内容恢复）。",
                    restored.join("、")
                );
                eprintln!("[storydesk] {text}");
                extra_notes.push(text.clone());
                let _ = app.emit(
                    engine::MASTER_CONTEXT_EVENT,
                    MasterContext {
                        run_id: run_id.clone(),
                        index,
                        total,
                        from_index: index,
                        files: restored,
                        text,
                    },
                );
            }
        }

        // 越界/错章名单：验收和摘要同步都要看它，所以提到块外（原来在 if 里，出了块就没了）。
        let mut bad: Vec<String> = Vec::new();
        let step_files = produced_now.clone();
        // 断点续跑的状态里记下这一步落了哪些文件（续跑时用于核对，不重写已完成章）。
        run_state.set_artifacts(index, step_files.clone());
        if !produced_now.is_empty() {
            eprintln!(
                "[storydesk] 第 {} 步产出：{}",
                index + 1,
                produced_now.join("、")
            );
            // 记忆/ 的豁免（只放开新增文件，摘要索引归编排器写）现在由
            // check_writes 自己判：以前这行 filter 是唯一一处，任何新调用点
            // 都会把写记忆误报成越权。规则只留一份，这里不再重复过滤。
            bad.extend(dir_guard::check_writes(
                &book,
                &agent.target_dir,
                &produced_now,
            ));
            // 章号验收：写正文被点名第 N 章，落盘却是别的章号——同样算越界。
            // 实测事故：任务写明「撰写 001 章正文」，写手看到 正文/ 里已有别的章，
            // 自己决定去写/覆盖 第004章_血路.md。错章内容必须移走或恢复，
            // 别让它冒充这一章的成果，也别让它把已有章节改得面目全非。
            if step_kind == RunKind::Prose {
                for rel in produced_now
                    .iter()
                    .filter(|r| r.replace('\\', "/").starts_with("正文/"))
                {
                    match prose_file_chapter(rel) {
                        Some(no) if no != step_chapter => bad.push(rel.clone()),
                        Some(_) => {}
                        // 文件名里没有章号（如 正文/随笔.md）：写手只该产章，也收走
                        None => bad.push(rel.clone()),
                    }
                }
            }
            // 细纲也有章号锁：点名第 3 章却写了第 5 章细纲、或顺手改全书大纲，
            // 和写错正文一样会污染后续步骤，必须按同一套规则处理。
            if step_kind == RunKind::Outline {
                let lock = outline_lock_for(&step, step_chapter);
                for rel in produced_now
                    .iter()
                    .filter(|r| r.replace('\\', "/").starts_with("大纲/"))
                {
                    if !outline_file_allowed(rel, lock) {
                        bad.push(rel.clone());
                    }
                }
            }
            bad.sort();
            bad.dedup();
            if !bad.is_empty() {
                // 修改前就存在的文件：恢复原内容，绝不移走作者旧稿。
                // 这一步新建的错章/越界文件：移进回收站，别让它冒充成果。
                let (existing, fresh): (Vec<String>, Vec<String>) = bad
                    .iter()
                    .cloned()
                    .partition(|rel| before.contents.contains_key(rel));
                let restored = restore_files(&book, &existing, &before.contents);
                let moved = quarantine_files(&book, &fresh);
                let mut parts: Vec<String> = Vec::new();
                if !restored.is_empty() {
                    parts.push(format!("{} 已恢复为修改前内容", restored.join("、")));
                }
                if !moved.is_empty() {
                    parts.push(format!(
                        "{} 已移入 .越界回收站/（可在回收站找回）",
                        moved.join("、")
                    ));
                }
                if !parts.is_empty() {
                    let text = format!(
                        "[系统] 拦截到越界/错章写入：{}，不会传给下一步。",
                        parts.join("；")
                    );
                    eprintln!("[storydesk] {text}");
                    extra_notes.push(text.clone());
                    let _ = app.emit(
                        engine::MASTER_CONTEXT_EVENT,
                        MasterContext {
                            run_id: run_id.clone(),
                            index,
                            total,
                            from_index: index,
                            files: bad.clone(),
                            text,
                        },
                    );
                }
            }
            let inside: Vec<String> = produced_now
                .into_iter()
                .filter(|rel| {
                    !bad.contains(rel) && rel.starts_with(&format!("{}/", agent.target_dir))
                })
                .collect();
            if !inside.is_empty() {
                produced = inside;
                producer = index;
            }
        }

        // 补充类大纲步骤的结果校验（真实事故：模型补完每卷章数，又凭记忆
        // 整文件重写 大纲/大纲.md，结束时文件跟开工前一模一样，补充被无声冲掉）。
        // 文件锁拦不住这种——锁只管「能写哪些文件」，不管「写回去的是不是退步版」。
        if step_kind == RunKind::Outline
            && (step.task.contains("【增量补大纲】") || step.task.contains("【执行铁律】"))
        {
            if let Some(orig) = before.contents.get("大纲/大纲.md") {
                let now = std::fs::read(book.join("大纲/大纲.md")).unwrap_or_default();
                if now.is_empty() {
                    let text =
                        "[系统] 补充未生效：这一步结束时 大纲/大纲.md 读不到内容，请重新执行本步。"
                            .to_string();
                    eprintln!("[storydesk] {text}");
                    extra_notes.push(text);
                } else if now == *orig {
                    let text = "[系统] 补充未生效：结束时 大纲/大纲.md 与开工前完全相同（补充被整文件重写冲掉了），请重新执行本步。".to_string();
                    eprintln!("[storydesk] {text}");
                    extra_notes.push(text);
                } else {
                    let orig_text = String::from_utf8_lossy(orig);
                    let now_text = String::from_utf8_lossy(&now);
                    let lost = orig_text
                        .lines()
                        .filter(|l| !l.trim().is_empty())
                        .filter(|l| !now_text.lines().any(|n| n.trim() == l.trim()))
                        .count();
                    if lost > 0 {
                        let text = format!(
                            "[系统] 补充时丢失了 {lost} 行大纲原有内容，请核对 大纲/大纲.md 或用历史快照恢复。"
                        );
                        eprintln!("[storydesk] {text}");
                        extra_notes.push(text);
                    }
                }
            }
        }

        let (step_status, exit, stderr) = match &outcome {
            Ok(RunOutcome::Done { exit }) => ("done".to_string(), *exit, None),
            Ok(RunOutcome::Blocked { exit, stderr }) => {
                let block = hook_parse::to_block(&step_run_id, stderr);
                let _ = app.emit(engine::BLOCKED_EVENT, block);
                ("blocked".to_string(), *exit, Some(stderr.clone()))
            }
            Ok(RunOutcome::Failed { exit, stderr }) => {
                let cancelled = stderr.contains(engine::CANCEL_MARK);
                (
                    if cancelled {
                        "cancelled".to_string()
                    } else {
                        "failed".to_string()
                    },
                    *exit,
                    Some(stderr.clone()),
                )
            }
            Err(e) => ("failed".to_string(), -1, Some(e.to_string())),
        };

        // 收尾心跳：告诉界面这一步已经结束、用了多久、落了哪些文件。
        // 没有这条，卡片会永远停在「running」，作者只能靠猜。
        let mut final_beat = beat.payload(&step_status, duration);
        final_beat.produced = step_files.clone();
        let _ = app.emit(engine::MASTER_PROGRESS_EVENT, final_beat);
        engine::forget_activity(&step_run_id);

        if let Ok(outcome) = &outcome {
            engine::emit_done(&app, &step_run_id, outcome, duration, None);
        } else {
            let _ = app.emit(
                engine::DONE_EVENT,
                RunDone {
                    run_id: step_run_id.clone(),
                    exit: -1,
                    status: "error".to_string(),
                    duration_ms: duration,
                    stderr: stderr.clone(),
                    detail: Some("子智能体启动失败".to_string()),
                },
            );
        }

        results.push(MasterStepResult {
            index,
            agent_id: agent.id.clone(),
            agent_name: agent.name.clone(),
            run_id: step_run_id,
            status: step_status.clone(),
            exit,
            duration_ms: duration,
            stderr: stderr.clone(),
            attempts,
            error: stderr.as_deref().and_then(engine::error_brief),
            resumable: step_status != "done",
        });

        // 断点续跑：这一步成了/废了都立刻落盘——进程被关掉也不丢进度。
        run_state.set(
            index,
            &step_status,
            attempts,
            stderr.as_deref().and_then(engine::error_brief),
        );
        run_state.sync(&steps);
        if let Err(e) = run_state.write(&book) {
            engine::log_failure(&app, "运行状态没写进去", &e);
        }

        // 字数验收：用户规定了范围且这一步跑完，就拿磁盘上的正文对账「纯字数」；
        // 不足就追加一步续写（同一写手、同一章），两次仍不足才记账收尾。
        // 光在任务里写「字数硬要求」没用——实测规定 6000-8000 照样写出 5956，
        // 编排层必须复核，模型可以糊弄 prompt，糊弄不了字数统计。
        // 缺章也在这里兜底：被点名的章连文件都没落盘（或写错章被收进回收站），
        // 不管用户有没有规定字数，都自动补一步「补写第 N 章」。
        // 这一步还会追加「续写/精简」步骤：追加了就先不同步摘要，等定稿那一轮再说。
        let mut pending_retry = false;
        if step_kind == RunKind::Prose && !read_only && step_status == "done" {
            // 达标线优先级：用户点名的字数 > 步骤自带的 words > 本章细纲的「正文目标字数」 > (1500,3000) 兜底。
            // 旧实现没有第三档，confirmed 单章步（没有 words）一律回落到 1500，
            // 细纲写 6200、写手交 5389 也判「执行完成」——用户看到的正是这个。
            let outline_target = context::outline_word_range(&book, step_chapter);
            // 优先级与上面往 prompt 里注字数的写法（step_word_count，见本步 note 的构造处）保持
            // 同一条口径：用户点名 > 这一步的 words / 任务文本字数 > 本章细纲目标 > (1500,3000)。
            // 旧实现漏了第二档，整本模式（计划里只有每步 words、用户没点名）每章都按 1500 判，
            // 细纲要求 6200 的章写完 5389 也判「不达标」，于是反复续写、越写越水。
            let (lo, hi) = word_count
                .or_else(|| step_words(&step))
                .or(outline_target)
                .unwrap_or((1500, 3000));
            let count = context::chapter_word_count(&book, step_chapter).unwrap_or(0);
            let missing = context::chapter_prose_path(&book, step_chapter).is_none();
            // 收尾章不按字数下限追加续写：为了凑字继续往下写，正是「让它结尾它却停不下来」的机械成因。
            // 只有文件根本没落盘（真失败）才补一次整章。
            let ending_step = step.task.contains(intent::ENDING_MARK);
            // 收尾章以前完全不验收字数（`!ending_step && count < lo`），只给 1 次补写配额——
            // 结果是收尾章可以只写 800 字就报「执行完成」。但收尾章本来就要压缩支线、把全书收口，
            // 按满额判死会逼写手注水，所以改成软下限：不低于要求字数的 3/4 就算过关，
            // 不够才用收尾专用补写（intent::closing_task）继续补。
            let check_lo = if ending_step { ending_floor(lo) } else { lo };
            let retry_cap = MAX_WORD_RETRIES;
            if missing || (count as u32) < check_lo {
                let retries = word_retries.entry(step_chapter).or_insert(0);
                // 「卡住」判定必须真的拦住下一次派发：上一版只往 extra_notes 里写了一句
                // 「判定卡住，停止重试」，然后照旧 *retries += 1 并追加续写步骤——
                // 于是 6000 字的长章可以一路补到 MAX_WORD_RETRIES，每轮只多几十字，
                // 用户在界面上看到的就是「第 1 步跑了一百多分钟」。
                let stuck = if *retries > 0 {
                    let last_count = last_word_counts.get(&step_chapter).copied().unwrap_or(0);
                    count.saturating_sub(last_count) < MIN_PROGRESS_PER_RETRY
                } else {
                    false
                };
                if stuck {
                    let last_count = last_word_counts.get(&step_chapter).copied().unwrap_or(0);
                    let progress = count.saturating_sub(last_count);
                    let text = format!(
                        "第 {step_chapter} 章第 {} 次补写只多出 {progress} 字（阈值 {MIN_PROGRESS_PER_RETRY}），\
                         判定卡住，停止自动补写（当前 {count} 字，要求不少于 {check_lo} 字）。",
                        *retries
                    );
                    eprintln!("[storydesk] {text}");
                    think(&app, &run_id, "thinking", &text);
                    extra_notes.push(text);
                    // 用尽这一章的配额，避免外层再派一次同样的续写。
                    *retries = retry_cap;
                } else if *retries < retry_cap {
                    // 第一次补写记下基准字数；后续补写有实质推进才更新基准。
                    last_word_counts.insert(step_chapter, count);
                    *retries += 1;
                    let retry_no = *retries;
                    let why = if missing {
                        format!("第 {step_chapter} 章没有落盘文件")
                    } else if ending_step {
                        format!(
                            "第 {step_chapter} 章是收尾章，纯字数 {count} 字，不足软下限 {check_lo} 字"
                        )
                    } else {
                        format!("第 {step_chapter} 章纯字数 {count} 字，不足 {lo} 字")
                    };
                    think(
                        &app,
                        &run_id,
                        "thinking",
                        &format!("{why}，追加补写步骤（第 {retry_no} 次）。"),
                    );
                    steps.push(PlanStep {
                        agent_id: step.agent_id.clone(),
                        task: if ending_step {
                            intent::closing_task(step_chapter, count, check_lo, lo, hi)
                        } else {
                            intent::continuation_task(step_chapter, count, lo, hi)
                        },
                        ..Default::default()
                    });
                    batch_chapters.insert(step_chapter);
                    pending_retry = true;
                } else {
                    let text = format!(
                        "第 {step_chapter} 章补写 {retry_cap} 次后仍不达标（当前 {count} 字，要求不少于 {check_lo} 字）。"
                    );
                    eprintln!("[storydesk] {text}");
                    extra_notes.push(text);
                }
            } else if word_count.is_some() && (count as u32) > hi {
                // 超出上限：与「不足」对称处理——自动追加一步精简（删改只在正文自己这一份上做，
                // 写手按原稿压缩，不会伤到别章）。以前这里只记一句「请人工压缩」，
                // 用户要求 9000-9500、写手交出 10761 字，应用照样报「执行完成」。
                let retries = trim_retries.entry(step_chapter).or_insert(0);
                if *retries < MAX_TRIM_RETRIES {
                    *retries += 1;
                    let retry_no = *retries;
                    think(
                        &app,
                        &run_id,
                        "thinking",
                        &format!(
                            "第 {step_chapter} 章纯字数 {count} 字，超过上限 {hi} 字，追加精简步骤（第 {retry_no} 次）。"
                        ),
                    );
                    steps.push(PlanStep {
                        agent_id: step.agent_id.clone(),
                        task: intent::trim_task(step_chapter, count, lo, hi),
                        ..Default::default()
                    });
                    batch_chapters.insert(step_chapter);
                    pending_retry = true;
                } else {
                    let text = format!(
                        "第 {step_chapter} 章精简 {} 次后仍超上限（当前 {count} 字，用户上限 {hi} 字），请人工压缩。",
                        MAX_TRIM_RETRIES
                    );
                    eprintln!("[storydesk] {text}");
                    extra_notes.push(text);
                }
            } else if ending_step && word_count.is_some() && (count as u32) < lo {
                // 收尾章落在「软下限 ≤ 字数 < 用户下限」之间：不补写（硬注水会毁掉收尾的密度），
                // 但必须明确记账 + 提示，别让它在界面上看起来和完全达标一模一样。
                let text = format!(
                    "第 {step_chapter} 章是收尾章，纯字数 {count} 字，按软下限 {check_lo} 字验收通过（用户要求不少于 {lo} 字）。"
                );
                eprintln!("[storydesk] {text}");
                think(&app, &run_id, "thinking", &text);
                extra_notes.push(text);
            }
        }

        // 正文格式清理：模型会把细纲的幕序照抄成「## 一」「## 场景一」这类 markdown 标题。
        // 排在搬注释之前——两份清理都幂等，先剥标题，后面摘摘要拿到的是干净文本。
        if step_kind == RunKind::Prose && !read_only && step_status == "done" && !pending_retry {
            match crate::services::prose_clean::strip_chapter_headings(&book, step_chapter) {
                Ok(Some(report)) => {
                    let text = format!(
                        "正文清理：第{}章去掉 {} 处小标题标记（{}）。",
                        report.chapter,
                        report.removed,
                        report.samples.join(" / ")
                    );
                    eprintln!("[storydesk] {text}");
                    think(&app, &run_id, "thinking", &text);
                }
                Ok(None) => {}
                Err(e) => engine::log_failure(&app, "正文清理没跑成", &e),
            }
        }

        // 正文清理：把「（本章完）」之后的创作注释（补充说明/写作说明）搬到
        // `追踪/写作备注.md`。必须排在写摘要之前——摘要的退路是摘正文文件，
        // 注释留在里面就会被摘进去。搬过一次之后再跑不会命中（幂等）。
        if step_kind == RunKind::Prose && !read_only && step_status == "done" && !pending_retry {
            match memory::detach_writer_notes(&ws, &book, step_chapter) {
                Ok(Some(report)) => {
                    let text = format!(
                        "正文清理：第{}章的创作注释（{} 字）已搬到 {}，正文只留故事。",
                        report.chapter, report.moved_chars, report.notes_rel
                    );
                    eprintln!("[storydesk] {text}");
                    think(&app, &run_id, "thinking", &text);
                }
                Ok(None) => {}
                Err(e) => engine::log_failure(&app, "正文清理没搬成", &e),
            }
        }

        // R1：正文验收过了就由编排器写摘要行——写手只负责在回复里给一行。
        // 顺序有讲究：越界/错章（bad 非空）的步不写，否则会拿一章的摘要冒充另一章；
        // 也不在续写/精简排队时写（pending_retry），免得落下一部半截章节的摘要。
        if step_kind == RunKind::Prose
            && !read_only
            && step_status == "done"
            && bad.is_empty()
            && !pending_retry
        {
            let reply = capture.lock().map(|text| text.clone()).unwrap_or_default();
            match memory::sync_chapter(&book, step_chapter, &reply) {
                Ok(report) => {
                    if report.wrote {
                        let from = if report.source == memory::RowSource::Writer {
                            "写手回复"
                        } else {
                            "正文摘录"
                        };
                        let text = format!("记忆：第{step_chapter}章摘要已写入（来源：{from}）。");
                        eprintln!("[storydesk] {text}");
                        think(&app, &run_id, "thinking", &text);
                    } else if !memory::has_chapter_row(&memory::index_text(&book), step_chapter) {
                        // 兜底告警：不阻断，但必须说出来——摘要悄悄断供比报错难查得多。
                        let text = format!(
                            "记忆：第{step_chapter}章摘要没落盘（回复里没有摘要行，正文也取不到可摘录内容），\
                             下一章的前情提要会缺这一章，建议人工补一行。"
                        );
                        eprintln!("[storydesk] {text}");
                        extra_notes.push(text);
                    }
                }
                Err(e) => engine::log_failure(&app, "摘要没写进去", &e),
            }

            // 事件账本：同一份回复里的「回收伏笔」声明，写回事件卡的已结算账本。
            // 与摘要同源同源同批落盘——两者都靠那一行摘要，缺一行就都断供，一起报更省事。
            match crate::services::event_ledger::sync_from_reply(&book, step_chapter, &reply) {
                Ok(report) if report.wrote => {
                    let text = format!(
                        "事件账本：第{step_chapter}章结算了 {}（事件 {}）。",
                        report.settled.join("、"),
                        report.event.clone().unwrap_or_default()
                    );
                    eprintln!("[storydesk] {text}");
                    think(&app, &run_id, "thinking", &text);
                }
                Ok(report) => {
                    if let Some(note) = report.note {
                        extra_notes.push(note);
                    }
                }
                Err(e) => engine::log_failure(&app, "事件账本没写进去", &e),
            }
        }

        match step_status.as_str() {
            "done" => {}
            "cancelled" => {
                status = "failed".to_string();
                summary = format!(
                    "第 {} 步被取消，编排停止。进度已保存：可从第 {} 步继续。",
                    index + 1,
                    index + 1
                );
                break;
            }
            other => {
                // 批量拆步里的章：一步失败只记这一章的账，后面的章继续。
                if batch_chapters.contains(&step_chapter) {
                    let text = format!(
                        "第 {step_chapter} 章{}，跳过该章继续后面的章节。",
                        if other == "blocked" {
                            "被 hook 拦下"
                        } else {
                            "执行失败"
                        }
                    );
                    eprintln!("[storydesk] {text}");
                    extra_notes.push(text);
                    index += 1;
                    continue;
                }
                status = if other == "blocked" {
                    "blocked"
                } else {
                    "failed"
                }
                .to_string();
                summary = format!(
                    "第 {} 步（{}）{}，后续步骤不再派发。",
                    index + 1,
                    agent.name,
                    if other == "blocked" {
                        "被 hook 拦下"
                    } else {
                        "执行失败"
                    }
                );
                // 引擎的 stderr 不能只躺在控制台：作者看到的「执行失败」必须带真实错误，
                // 否则像 opencode「Unexpected server error」那样只能靠猜。
                let detail = stderr.as_deref().unwrap_or("").trim();
                if let Some(brief) = engine::error_brief(detail) {
                    summary.push_str(&format!(" 引擎报错：{brief}"));
                }
                // 断点续跑：失败不等于这一轮作废——进度已经落盘，修好原因就能从这一步接上。
                summary.push_str(&format!(
                    " 进度已保存：修好后可从第 {} 步继续（不必重新生成计划）。",
                    index + 1
                ));
                // opencode 报 500 时它把真正的原因藏在自己的日志里：现在用 --print-logs
                // 打进了 stderr（多半是 ProviderModelNotFoundError：模型没写进
                // opencode.json，或会话目录被 PWD 带偏、读不到项目配置）。
                if detail.contains("ProviderModelNotFound") {
                    summary.push_str(
                        " （opencode 找不到这个模型：检查「设置 → 模型 API」里选中的模型是否与当前提供商一致）",
                    );
                } else if detail.contains("Unexpected server error") {
                    summary.push_str(
                        " （opencode 没能开起会话：模型可能不在 opencode.json 里，或工作目录被 PWD 带偏、读不到项目配置；详情见应用日志 engine.log）",
                    );
                }
                engine::append_engine_log(
                    &app,
                    &format!(
                        "第 {} 步（{}）失败（exit {exit}）：{}",
                        index + 1,
                        agent.name,
                        if detail.is_empty() { "(无 stderr)" } else { detail }
                    ),
                );
                break;
            }
        }
        index += 1;
    }

    // 收尾落盘要在 `steps` 交还给 plan 之前：sync 还把最新步定义写回状态文件。
    run_state.sync(&steps);
    if let Err(e) = run_state.write(&book) {
        engine::log_failure(&app, "收尾时运行状态没写进去", &e);
    }
    plan.steps = steps;
    if status == "done" {
        summary = if plan.steps.is_empty() {
            "没有可执行的步骤。".to_string()
        } else {
            steps_done_summary(plan.steps.len(), &extra_notes)
        };
    }
    if !extra_notes.is_empty() {
        summary.push(' ');
        summary.push_str(&extra_notes.join(" "));
    }
    if !notes.is_empty() {
        summary.push(' ');
        summary.push_str(&notes.join("；"));
    }
    let _ = app.emit(
        engine::MASTER_DONE_EVENT,
        MasterDone {
            run_id: run_id.clone(),
            status: status.clone(),
            text: summary.clone(),
            steps_done: results.iter().filter(|r| r.status == "done").count(),
            steps_total: plan.steps.len(),
        },
    );

    Ok(MasterRunResult {
        run_id,
        plan,
        steps: results,
        status,
        summary,
    })
}

/// 续跑起点（0 基下标）的唯一校验入口。
///
/// 合法区间是 `[0, total)`；越界一律报错。为什么不能「悄悄按 total 处理」：
/// 主循环拿到 `resume_from >= total` 时一次都不进，随后按「全部完成」收尾，
/// 把状态文件覆盖成完成态——真正没跑完的步就再也续不上了。
fn resume_start(
    from_step: Option<usize>,
    total: usize,
    first_unfinished: Option<usize>,
) -> Result<usize, StoryError> {
    if total == 0 {
        return Err(StoryError::invalid(
            "runId",
            "这一轮的运行状态里没有步骤（可能被清空了），没法续跑",
        ));
    }
    if let Some(from) = from_step {
        if from >= total {
            return Err(StoryError::invalid(
                "fromStep",
                format!(
                    "续跑起点超出范围：这一轮只有 {total} 步（可用 1..={total}），收到第 {} 步",
                    from + 1
                ),
            ));
        }
        return Ok(from);
    }
    first_unfinished.ok_or_else(|| {
        StoryError::invalid(
            "runId",
            "这一轮的所有步骤都已经完成，不需要续跑",
        )
    })
}

/// 断点续跑：读运行状态，从第一个没完成的步接着派发。
///
/// 三个要点：
/// 1. **不重新规划**——计划原样重放，任务文本、章号锁、收尾从句都不变，
///    否则「续跑」等于换了一套口径，前半本和后半本会打架；
/// 2. **不重跑已完成的步**——状态文件里 `done` 的步直接跳过，避免把同一章写第二遍；
/// 3. **沿用同一个 run_id**——计划卡、派发卡、聊天流仍然串在同一段会话里。
#[tauri::command]
pub async fn master_resume(
    app: AppHandle,
    workspace_root: String,
    book_root: String,
    run_id: String,
    from_step: Option<usize>,
    engine: Option<EngineKind>,
) -> Result<MasterRunResult, StoryError> {
    let (_ws, book) = ai_cmd::resolve_pair(&workspace_root, &book_root)?;
    let state = run_state::load(&book, &run_id).ok_or_else(|| {
        StoryError::invalid(
            "runId",
            "这本书里没有这一轮的运行状态，没法续跑（可能已清空，或这一轮从没开始过）",
        )
    })?;
    // 起点校验集中在一处：from_step 超界时必须报错，绝不能让它变成
    // 「主循环一次都不进 → 按全部完成收尾 → 状态文件被覆盖成完成态」，
    // 那样真正没跑完的步就再也续不上了。
    let start = resume_start(from_step, state.steps.len(), state.first_unfinished())?;
    let text = format!(
        "续跑：从第 {} 步继续（共 {} 步，已完成 {} 步）。",
        start + 1,
        state.steps.len(),
        state.done_count()
    );
    eprintln!("[storydesk] {text}");
    think(&app, &run_id, "thinking", &text);

    let req = MasterRunRequest {
        workspace_root,
        book_root,
        request: state.request.clone(),
        engine,
        // 空 = 用设置里的智能体档案（续跑不该依赖前端手上的那份配置）
        agents: Vec::new(),
        chapter: None,
        steps: Some(state.plan.steps.clone()),
        source: state.source.clone(),
        run_id: Some(run_id),
        resume_from: Some(start),
    };
    master_run(app, req).await
}

fn step_kind_label(kind: RunKind) -> &'static str {
    match kind {
        RunKind::Outline => "生成细纲",
        RunKind::Prose => "写本章正文",
        RunKind::Deslop => "去 AI 味",
    }
}

fn first_lines(text: &str, n: usize) -> String {
    text.lines().take(n).collect::<Vec<_>>().join("\n")
}

/// 当前生效的智能体配置（设置页与主面板共用）。
#[tauri::command]
pub fn master_agents(app: AppHandle) -> Result<Vec<AgentConfig>, StoryError> {
    Ok(settings::load(&app).agents)
}

/// 内置六个角色的默认配置（设置页「恢复默认」用，不写盘）。
#[tauri::command]
pub fn master_default_agents() -> Vec<AgentConfig> {
    ai::default_agents()
}

/// 阶段 8：执行「用户已经点过『确认执行』」的计划。
///
/// 它不做规划——`steps` 就是前端从 `master_plan` 拿回来的那份，原样回传，
/// 所以确认这一步不产生任何模型调用（省钱、也省等待）。
#[tauri::command]
pub async fn master_execute(
    app: AppHandle,
    req: MasterExecuteRequest,
) -> Result<MasterRunResult, StoryError> {
    if req.steps.is_empty() {
        return Err(StoryError::invalid(
            "steps",
            "计划是空的：先在对话流里生成一份计划，再点「确认执行」",
        ));
    }
    let run = MasterRunRequest {
        workspace_root: req.workspace_root,
        book_root: req.book_root,
        // 用户原话必须一路带下去：章号、字数、题材都要从这句话里取。
        // 以前这里塞 `String::new()`，`master_run` 只能退回「正文目录的下一章」，
        // 于是计划卡写着 001 章、派发却按 005 章跑。
        request: req.request.unwrap_or_default(),
        engine: req.engine,
        agents: req.agents,
        chapter: req.chapter,
        steps: Some(req.steps),
        source: Some(req.source.unwrap_or_else(|| "confirmed".to_string())),
        run_id: req.run_id,
        // 确认执行 = 全新一轮；续跑走 `master_resume`，它自己带 resume_from。
        resume_from: None,
    };
    master_run(app, run).await
}

/// 一步开始前/结束后的书籍快照：mtime 用来找「这一步改了哪些文件」，
/// 文件内容用来在错章覆盖或误删时把作者旧稿原样恢复。
#[derive(Default)]
struct BookSnapshot {
    mtimes: HashMap<String, u64>,
    contents: HashMap<String, Vec<u8>>,
}

/// 扫一遍书里的 `.md`（最多 4 层），记下相对路径、mtime(ms) 与原始内容。
///
/// 前后两次快照相减 = 这一步到底写了哪些文件。用它而不是解析子进程的
/// `tool_use` 流，因为后者依赖模型老实上报工具调用，而磁盘不会撒谎。
fn snapshot_book(book_root: &Path) -> BookSnapshot {
    let mut out = BookSnapshot::default();
    walk_md(book_root, book_root, 0, &mut out);
    out
}

fn walk_md(root: &Path, dir: &Path, depth: usize, out: &mut BookSnapshot) {
    if depth > 4 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') {
            continue;
        }
        if path.is_dir() {
            walk_md(root, &path, depth + 1, out);
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        if let Ok(rel) = path.strip_prefix(root) {
            let rel = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().to_string())
                .collect::<Vec<_>>()
                .join("/");
            out.mtimes.insert(rel.clone(), mtime);
            out.contents.insert(rel, bytes);
        }
    }
}

/// 把文件恢复成快照里的内容（只在内容确实不同时写，避免无谓改 mtime）。
/// 返回真正恢复过的相对路径。
fn restore_files(
    book_root: &Path,
    rels: &[String],
    contents: &HashMap<String, Vec<u8>>,
) -> Vec<String> {
    let mut restored = Vec::new();
    for rel in rels {
        let Some(bytes) = contents.get(rel) else {
            continue;
        };
        let path = book_root.join(rel.replace('/', "\\"));
        if std::fs::read(&path).ok().as_deref() == Some(bytes.as_slice()) {
            continue;
        }
        if let Some(parent) = path.parent() {
            if std::fs::create_dir_all(parent).is_err() {
                continue;
            }
        }
        if std::fs::write(&path, bytes).is_ok() {
            restored.push(rel.clone());
        }
    }
    restored
}

/// 任务是不是「全书级大纲」：文件锁按它认 Book 锁，批量拆步按它认「不许拆」。
/// 一律按标记判定——任务文本里出现「第1—40章」这类区间字样太常见了
/// （补卷章数的任务必然带），章号正则碰运气已经连出两起事故。
fn is_book_outline_task(task: &str) -> bool {
    task.contains("整本故事的全书大纲")
        || task.contains("【全书大纲】")
        || task.contains("【补卷章节规划】")
        || task.contains("【增量补大纲】")
}

/// 细纲步的文件锁：`PlanStep::lock` 优先，字段为空才从任务文本推断（旧计划全走这条路）。
fn outline_lock_for(step: &PlanStep, step_chapter: u32) -> OutlineLock {
    let lock = step
        .lock
        .unwrap_or_else(|| outline_lock_from_text(&step.task, step_chapter));
    sync_lock_with_task(&step.task, lock)
}

/// 让文件锁跟着任务文本走：文本里的标记由本地代码写入，比可能过期的字段更可信。
///
/// 事故现场（2026-11-16）：用户要求「重新生成全书大纲」，计划里 architect 步的任务
/// 已经是全书大纲，`lock` 却是改写前的 `Chapters(1,1)`。执行期按字段注入
/// 「【章号锁定】本步只允许写 大纲/细纲_第001章.md 这一个细纲文件；其它章节的细纲与
/// 全书大纲一律不要修改、覆盖或重写」，模型照锁只交付了第 1 章细纲并回报冲突——
/// 用户看到的就是「词不达意」。越界检查用的是同一个锁，即使模型硬写
/// 大纲/大纲.md 也会被恢复成旧内容，两边一起把全书大纲锁死。
fn sync_lock_with_task(task: &str, lock: OutlineLock) -> OutlineLock {
    if is_book_outline_task(task) {
        return OutlineLock::Book;
    }
    if task.contains("【大纲目录级】") {
        return match lock {
            OutlineLock::Chapters(..) => OutlineLock::OutlineDir,
            other => other,
        };
    }
    lock
}

/// 闸改完任务文本后把锁字段拉回来。
///
/// 事故现场（2026-11-16）：`enforce_outline_and_settings` 把 architect 步的章级
/// 细纲任务换成全书大纲任务（前置「【全书大纲】」或整段换成 `book_outline_task`），
/// `lock` 字段却停在改写前的 `Chapters(1,1)`；执行期按字段注入
/// 「【章号锁定】只允许写 大纲/细纲_第001章.md」，模型只能交第 1 章细纲并回报冲突。
/// 改写点之后必须重算，不能指望 normalize 在它之前跑过。
fn resync_outline_locks(plan: &mut MasterPlan, agents: &[AgentConfig]) {
    for step in plan.steps.iter_mut() {
        let agent = ai::resolve_agent(agents, &step.agent_id);
        if agent.target_dir != "大纲" {
            continue;
        }
        let chapter = step.range.map(|r| r.bounds().0).unwrap_or(1);
        let lock = step
            .lock
            .unwrap_or_else(|| outline_lock_from_text(&step.task, chapter));
        step.lock = Some(sync_lock_with_task(&step.task, lock));
    }
}

/// 收尾请求：把「这是最后一章」的规则钉到最后一个正文步与最后一个细纲步上。
///
/// 只钉最后一步，是因为收尾是**这一段区间**的终点：前面的章照常留钩子，
/// 最后一章必须把钩子全部收掉。计划卡上同时写一句「全书收尾」，
/// 用户点确认前就能看出这轮打算把书写完。
/// 书级一致性校验：把「大纲声明多少章 / 细纲排到第几章 / 正文写到第几章」的差距
/// 挂到计划卡上。
///
/// 为什么放在计划卡而不是门禁：这不是某一章的错，是整本书的账没对上——
/// 大纲写着 300 章、细纲到了 350 章、正文停在 308 章，谁都没错，但「全书结尾在第几章」
/// 就没有唯一答案，收尾从句只能靠猜。先让作者在确认计划前看见这三个数字，
/// 收尾钉在哪一章才有依据；应用侧不改作者的大纲（那属于内容，不属于软件）。
fn attach_scope_warnings(plan: &mut MasterPlan, book: Option<&Path>) {
    let Some(book) = book else { return };
    let scope = context::book_scope(book);
    let warnings = context::scope_warnings(&scope);
    if warnings.is_empty() {
        return;
    }
    let note = format!("书级一致性：{}", warnings.join("；"));
    plan.intent = Some(match plan.intent.take() {
        Some(text) if !text.contains(&note) => format!("{text} · {note}"),
        Some(text) => text,
        None => note,
    });
}

fn attach_ending_clauses(
    plan: &mut MasterPlan,
    agents: &[AgentConfig],
    book: Option<&Path>,
    request: &str,
) {
    // 收尾判定只走 `intent::classify` 这一个出口（`Intent.ending`）：
    // 各处自己再判一次，就会出现「计划卡按一种理解、收尾按另一种理解」的分裂。
    if !intent::classify(request).ending {
        return;
    }
    let mut prose: Option<usize> = None;
    let mut outline: Option<usize> = None;
    let mut prose_end = 0u32;
    let mut outline_end = 0u32;
    let mut attached = 0u32;
    for (i, step) in plan.steps.iter().enumerate() {
        if step_is_readonly(step) {
            continue;
        }
        // 收尾必须钉在**章号最靠后**的那一步上，不是数组里最后一个。
        // 计划里正文步与细纲步交错时，末位可能是一条更早的区间步，钉上去等于让
        // 「全书完」落在中间章（《隐藏条款》331—350 事故：收尾从句钉在第 335 章）。
        let end = step_span_authoritative(step).map(|(_, end)| end).unwrap_or(0);
        match ai::resolve_agent(agents, &step.agent_id).target_dir.as_str() {
            "正文" if prose.is_none() || end >= prose_end => {
                prose = Some(i);
                prose_end = end;
            }
            "大纲" if outline.is_none() || end >= outline_end => {
                outline = Some(i);
                outline_end = end;
            }
            _ => {}
        }
    }
    // 全书结尾章号：请求里声明的终点优先，其次用书目录已写到的章号，最后才用计划覆盖到的最远章。
    //
    // 收尾从句里的章号**只能**取这个值，不许取「这一步自己的区间末端」。
    // 事故（《隐藏条款》13:12 那轮）：计划里只有一条 331—335 的细纲步，正文步却排到了 350；
    // 旧代码把每一步自己的区间末端当收尾章，于是那条 331—335 细纲步被写上
    // 「第 335 章是全书最后一章的细纲」，而正文第 350 章那条写的是 350——
    // 细纲把 331—335 当结局收干净，336—350 又被当成可继续连载，收尾等于落在 335。
    //
    // 第二层：请求不说章号时（「收尾小说」），旧代码只能拿计划末端当全书结尾，
    // 于是「从中间截一段来写」的计划会把中间章标成全书完。现在先问书目录：
    // 正文/细纲/索引里最靠后的章，才是这本书实际写到的位置。
    let declared_end = intent::classify(request).chapter_range.map(|(_, end)| end);
    let planned_end = prose_end.max(outline_end);
    let hint = book
        .and_then(context::book_end_hint)
        .filter(|hint| hint.chapter > 0);
    let ending_chapter = declared_end
        .or_else(|| hint.as_ref().map(|hint| hint.chapter))
        .or(Some(planned_end))
        .filter(|chapter| *chapter > 0);
    // 结尾章没有被任何步骤覆盖：收尾无从谈起，先提示把缺的细纲/正文补进计划。
    if let Some(end) = ending_chapter.filter(|end| planned_end > 0 && *end > planned_end) {
        let basis = if declared_end == Some(end) {
            "请求声明".to_string()
        } else {
            hint.as_ref()
                .map(|hint| format!("依据 {}", hint.source))
                .unwrap_or_else(|| "计划覆盖".to_string())
        };
        let note = format!(
            "收尾提醒：计划只覆盖到第 {planned_end:03} 章，但全书结尾在第 {end:03} 章（{basis}）——\
             结尾章没有被任何步骤覆盖，先把缺的细纲/正文补进计划再收尾"
        );
        plan.intent = Some(match plan.intent.take() {
            Some(text) if !text.contains(&note) => format!("{text} · {note}"),
            Some(text) => text,
            None => note,
        });
    }
    for (target, is_prose) in [(prose, true), (outline, false)] {
        let Some(i) = target else { continue };
        // 已经带收尾语义就跳过：字段优先（结构化），老计划回退到任务文本里的标记。
        if plan.steps[i].ending.unwrap_or(false) || plan.steps[i].task.contains(intent::ENDING_MARK) {
            continue;
        }
        let step = &plan.steps[i];
        let chapter = step_span_authoritative(step)
            .map(|(_, end)| end)
            .unwrap_or_else(|| step_chapter_of(step, 1));
        // 没覆盖到全书结尾章的那一步不许被写成「这是全书最后一章」：
        // 窄区间步（只排 331—335 细纲）自称结局，会让真正的结尾章（350）失去收束语义。
        match ending_chapter {
            Some(end) if chapter == end => {}
            _ => continue,
        }
        let clause = if is_prose {
            intent::ending_prose_clause(chapter)
        } else {
            intent::ending_outline_clause(chapter)
        };
        let task = &mut plan.steps[i].task;
        task.push('\n');
        task.push_str(&clause);
        // 结构化标记同步写回：下游（引擎契约注入、拆步后的子步、续跑重放）读字段，
        // 不再靠 `contains("【全书收尾】")` 猜。
        plan.steps[i].ending = Some(true);
        attached += 1;
    }
    // 没钉上任何一条收尾从句时不许在计划卡上写「全书收尾」：那句话是给用户看的承诺，
    // 承诺了却没钉上，等于告诉用户「这轮会把书写完」而实际不会。
    if attached == 0 {
        return;
    }
    // 结尾章号从哪来，就写清哪来：用户点确认前能看出这个「最后一章」是请求说的、
    // 还是从书里已有的细纲/正文推出来的。
    let basis = match (declared_end, &hint) {
        (Some(_), _) => String::new(),
        (None, Some(hint)) => format!("（依据 {}）", hint.source),
        (None, None) => String::new(),
    };
    let end_chapter = ending_chapter.unwrap_or(planned_end);
    let note = format!(
        "全书收尾：以第 {end_chapter:03} 章收束{basis}，最后一章写完即收束（标「（全书完）」），不追加续写"
    );
    plan.intent = Some(match plan.intent.take() {
        Some(text) if !text.contains("全书收尾") => format!("{text} · {note}"),
        Some(text) => text,
        None => note.to_string(),
    });
}

/// 从任务文本推断文件锁。
///
/// 全书大纲靠标记（`is_book_outline_task`）；目录级计划靠 `【大纲目录级】`；
/// 其余按任务里的章号区间，连一章都没写就按调用方给的章号锁单章。
fn outline_lock_from_text(task: &str, step_chapter: u32) -> OutlineLock {
    if is_book_outline_task(task) {
        OutlineLock::Book
    } else if task.contains("【大纲目录级】") {
        OutlineLock::OutlineDir
    } else if let Some((start, end)) = intent::chapter_range_of(task).filter(|(s, e)| e >= s) {
        OutlineLock::Chapters(start, end)
    } else {
        OutlineLock::Chapters(step_chapter, step_chapter)
    }
}

/// 这一步产出的 `大纲/` 文件是否在锁定的交付范围内。
fn outline_file_allowed(rel: &str, lock: OutlineLock) -> bool {
    let norm = rel.replace('\\', "/");
    match lock {
        OutlineLock::Book => norm == "大纲/大纲.md",
        OutlineLock::OutlineDir => norm.starts_with("大纲/"),
        OutlineLock::Chapters(start, end) => {
            norm.starts_with("大纲/")
                && norm.contains("细纲_")
                && context::chapter_in_name(&norm).is_some_and(|no| no >= start && no <= end)
        }
    }
}

/// 从正文相对路径里抽章号：`正文/第004章_血路.md` → 4。
/// 兼容不带前导零与反斜杠分隔；抽不出章号返回 None。
fn prose_file_chapter(rel: &str) -> Option<u32> {
    let re = Regex::new(r"第\s*([0-9]{1,4})\s*章").expect("正文章号正则");
    let norm = rel.replace('\\', "/");
    let caps = re.captures(&norm)?;
    caps.get(1)?.as_str().parse::<u32>().ok().filter(|n| *n > 0)
}

/// 把越界/错章文件移进 `<书>/.越界回收站/`（保留原相对路径）。
///
/// 为什么不直接删：盖掉的可能是作者几个小时的旧稿，回收站里能人工找回。
/// 为什么必须移走：只警告不移走，错章正文会留在 正文/ 里冒充成果，
/// 下一步的上下文与字数验收全部被污染。返回真正移成功的文件。
fn quarantine_files(book_root: &Path, rels: &[String]) -> Vec<String> {
    let mut moved = Vec::new();
    for rel in rels {
        let from = book_root.join(rel.replace('/', "\\"));
        let to = book_root.join(".越界回收站").join(rel.replace('/', "\\"));
        if !from.is_file() {
            continue;
        }
        if let Some(parent) = to.parent() {
            if std::fs::create_dir_all(parent).is_err() {
                continue;
            }
        }
        // 同名冲突（上一步刚收过一份）：新内容覆盖旧回收件，保最新的
        let _ = std::fs::remove_file(&to);
        if std::fs::rename(&from, &to).is_ok() {
            moved.push(rel.clone());
        }
    }
    moved
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- 计划扩写回归（用户 m00851：只做一件事 / 不要做 被规则闸绕过） ----

    #[test]
    fn settings_only_request_never_gains_an_outline_step() {
        // 实机现场的规格化原文（截图 1）。模型只给了一步 settings，
        // 以前却被判成「排大纲」并插进第 0 位一步「【增量补大纲】」。
        let request = "【本次只做一件事】设定\n【产出】设定/设定.md\n【范围】补充世界观、势力、角色三部分的设定内容\n【不要做】不写正文、不写大纲、不改世界观势力角色之外的其他设定\n【原话】把设定里的世界观，势力，角色补充好";
        let found = intent::classify(request);
        assert_eq!(
            found.kind,
            intent::Kind::Settings,
            "「大纲」只出现在【不要做】里，意图必须是补设定"
        );
        let mut plan = MasterPlan {
            source: "llm".to_string(),
            intent: None,
            steps: vec![PlanStep {
                agent_id: "settings".to_string(),
                task: "【补设定】先读 设定/设定.md 现有内容，只补充世界观、势力、角色三部分".to_string(),
                ..Default::default()
            }],
            raw: String::new(),
        };
        enforce_outline_and_settings(&mut plan, &agents(), request, &found, 1, None);
        enforce_requested_scope(None, &mut plan, request, &found);
        assert_eq!(plan.steps.len(), 1, "只补设定的请求被扩成了：{:?}", plan.steps);
        assert_eq!(
            ai::AgentRole::parse(&plan.steps[0].agent_id),
            Some(ai::AgentRole::Settings)
        );
    }

    #[test]
    fn settings_only_request_with_list_negation_gains_no_outline_step() {
        // 真实事故第三次复现：【不要做】写成「不写正文、细纲、大纲」（一个动词带三个宾语）。
        let request = "【本次只做一件事】设定\n【产出】设定/世界观.md、设定/势力.md、设定/角色.md\n【范围】补充世界观、势力、角色三部分的设定内容\n【不要做】不写正文、细纲、大纲；不改用户已确认的既有设定内容\n【原话】把设定里世界观、势力、角色补充好";
        let found = intent::classify(request);
        assert_eq!(found.kind, intent::Kind::Settings, "这次要的是补设定：{:?}", found.hits);
        assert_eq!(found.supplement, false, "补设定请求不该带大纲增量标记");
        let mut plan = MasterPlan {
            source: "llm".to_string(),
            intent: None,
            steps: vec![PlanStep {
                agent_id: "settings".to_string(),
                task: "【设定补全】先读 设定/设定.md 现有内容，只补充世界观、势力、角色三部分".to_string(),
                ..Default::default()
            }],
            raw: String::new(),
        };
        enforce_outline_and_settings(&mut plan, &agents(), request, &found, 1, None);
        enforce_requested_scope(None, &mut plan, request, &found);
        assert_eq!(plan.steps.len(), 1, "只补设定的请求被扩成了：{:?}", plan.steps);
        assert_eq!(
            ai::AgentRole::parse(&plan.steps[0].agent_id),
            Some(ai::AgentRole::Settings)
        );
        assert!(
            !plan.steps.iter().any(|s| s.task.contains("增量补大纲")),
            "不允许自动派生大纲步"
        );
    }

    #[test]
    fn book_outline_request_is_not_downgraded_to_chapter_outline() {
        // 真实事故（《山海不夜》2026-11-18）：请求明写「全书大纲 /【产出】大纲/大纲.md」，
        // 【不要做】里有「不写章级细纲」——层级判定被这个「细纲」字样压成章级，
        // 模型写对的整本大纲任务又被 enforce 改写成「重新生成第 1 章细纲」。
        let request = "【本次只做一件事】全书大纲\n【产出】大纲/大纲.md\n【依据】先通读 参考资料、设定、追踪 各目录下的全部文件，提炼主线、世界观、人物线与已写进度后动笔\n【缺口】缺全书目标规模（如总字数、章节或卷数），每章节6500-7000字\n【不要做】不写正文、不写章级细纲、不改设定与追踪文件\n【原话】以参考资料，设定和追踪的各个目录，开始写全文总大纲";
        let found = intent::classify(request);
        assert_eq!(found.kind, intent::Kind::Outline, "命中：{:?}", found.hits);
        assert_eq!(
            found.outline_scope,
            intent::OutlineScope::Book,
            "明说全书大纲，层级不许被【不要做】里的细纲压成章级"
        );
        let summary = intent::plan_summary(request);
        assert!(
            !summary.contains("先写正文"),
            "禁止项不能被读成任务：{summary}"
        );
        let mut plan = MasterPlan {
            source: "llm".to_string(),
            intent: None,
            steps: vec![PlanStep {
                agent_id: "architect".to_string(),
                task: "先通读 参考资料/、设定/、追踪/ 各目录下的全部文件，然后重新生成 大纲/大纲.md 的全书总大纲".to_string(),
                ..Default::default()
            }],
            raw: String::new(),
        };
        enforce_outline_and_settings(&mut plan, &agents(), request, &found, 1, None);
        assert_eq!(plan.steps.len(), 1, "只做全书大纲，不该多出别的步：{:?}", plan.steps);
        let task = &plan.steps[0].task;
        assert!(task.contains("大纲/大纲.md"), "目标必须还是整本大纲：{task}");
        assert!(!task.contains("细纲_第"), "不许降级成单章细纲：{task}");
    }

    #[test]
    fn outline_patch_request_keeps_the_outline_step_and_drops_writer() {
        let request = "增量补大纲，不要写正文，也不要细纲";
        let found = intent::classify(request);
        let mut plan = MasterPlan {
            source: "llm".to_string(),
            intent: None,
            steps: vec![
                PlanStep {
                    agent_id: "architect".to_string(),
                    task: "【增量补大纲】补 大纲/大纲.md 里七个卷的章数区间".to_string(),
                    ..Default::default()
                },
                PlanStep {
                    agent_id: "writer".to_string(),
                    task: "接着写第 12 章正文".to_string(),
                    ..Default::default()
                },
            ],
            raw: String::new(),
        };
        enforce_requested_scope(None, &mut plan, request, &found);
        assert_eq!(plan.steps.len(), 1, "写手步必须被摘掉：{:?}", plan.steps);
        assert_eq!(
            ai::AgentRole::parse(&plan.steps[0].agent_id),
            Some(ai::AgentRole::Architect),
            "「不要细纲」不等于「不要全书大纲」，大纲步必须留着"
        );
    }

    #[test]
    fn settings_then_outline_request_keeps_both_steps() {
        // 用户明确要两件事：谁都不该被收窄掉。
        let request = "先补设定，再把大纲写了";
        let found = intent::classify(request);
        let mut plan = MasterPlan {
            source: "llm".to_string(),
            intent: None,
            steps: vec![
                PlanStep {
                    agent_id: "settings".to_string(),
                    task: "【补设定】补 设定/设定.md".to_string(),
                    ..Default::default()
                },
                PlanStep {
                    agent_id: "architect".to_string(),
                    task: "【全书大纲】写 大纲/大纲.md".to_string(),
                    ..Default::default()
                },
            ],
            raw: String::new(),
        };
        enforce_requested_scope(None, &mut plan, request, &found);
        assert_eq!(plan.steps.len(), 2, "明确要多步的请求不该被收窄：{:?}", plan.steps);
    }

    /// 只有任务文本的步骤：闸层优先读字段，这里故意只给文本，覆盖回退路径。
    fn text_step(task: &str) -> PlanStep {
        PlanStep {
            agent_id: "architect".to_string(),
            task: task.to_string(),
            ..Default::default()
        }
    }

    /// 字段写死了就以字段为准：任务文本里的章号区间不再参与判定。
    #[test]
    fn lock_field_beats_task_text() {
        let step = PlanStep {
            lock: Some(OutlineLock::Book),
            ..text_step("生成第 6 至第 10 章的章节级细纲")
        };
        assert_eq!(outline_lock_for(&step, 1), OutlineLock::Book);

        let chapter = PlanStep {
            lock: Some(OutlineLock::Chapters(2, 2)),
            ..text_step("生成第 6 至第 10 章的章节级细纲")
        };
        assert_eq!(outline_lock_for(&chapter, 6), OutlineLock::Chapters(2, 2));

        // 字段为空才回退文本——这是旧计划（.storydesk/chats/*.json 里没有 lock）的唯一活路。
        assert_eq!(
            outline_lock_for(&text_step("生成第 6 至第 10 章的章节级细纲"), 1),
            OutlineLock::Chapters(6, 10)
        );
    }

    /// `read_only` 字段压过文本判定：文本里有「改写」也照样算只读步。
    #[test]
    fn read_only_field_beats_task_text() {
        let readonly = PlanStep {
            read_only: Some(true),
            ..text_step("改写 正文/第003章，把对峙改成冷处理")
        };
        assert!(step_is_readonly(&readonly));

        let writing = PlanStep {
            read_only: Some(false),
            ..text_step("【只读模式】核对第 3 章与前文的连贯性")
        };
        assert!(!step_is_readonly(&writing));

        // 字段缺省 → 文本判定照旧：「只读模式」开头的算只读。
        assert!(step_is_readonly(&text_step(
            "【只读模式】核对第 3 章与前文的连贯性"
        )));
    }

    /// `kind` 字段压过角色目录：同一个 architect 也能被显式标成 Prose。
    #[test]
    fn kind_field_beats_agent_target_dir() {
        let architect = agents()
            .into_iter()
            .find(|a| a.id == "architect")
            .expect("architect 必须在 agents() 里");
        assert_eq!(kind_for(&architect, None), RunKind::Outline);

        let prose = PlanStep {
            kind: Some(RunKind::Prose),
            ..text_step("按细纲写第 3 章")
        };
        assert_eq!(kind_for(&architect, Some(&prose)), RunKind::Prose);

        let unset = text_step("按细纲写第 3 章");
        assert_eq!(kind_for(&architect, Some(&unset)), RunKind::Outline);
    }

    /// `range` 字段压过文本章号：批量步的区间以字段为准。
    #[test]
    fn chapter_range_field_beats_task_text() {
        let span = PlanStep {
            range: Some(ChapterRange::Span(6, 10)),
            ..text_step("生成第 1 章的细纲")
        };
        assert_eq!(step_chapter_span(&span), Some((6, 10)));
        assert_eq!(step_chapter_of(&span, 99), 6);
        assert!(step_covers_chapter(&span, 8));
        assert!(!step_covers_chapter(&span, 11));

        let single = PlanStep {
            range: Some(ChapterRange::Single(12)),
            ..text_step("生成第 1 章的细纲")
        };
        assert_eq!(step_chapter_span(&single), Some((12, 12)));
        assert_eq!(step_chapter_of(&single, 99), 12);

        let book = PlanStep {
            range: Some(ChapterRange::Book),
            ..text_step("【全书大纲】重写 大纲/大纲.md")
        };
        assert_eq!(step_chapter_span(&book), None);
        assert_eq!(step_chapter_of(&book, 7), 7);
        assert!(!step_covers_chapter(&book, 7));

        // 无一字字段 → 仍是文本解析。
        assert_eq!(
            step_chapter_span(&text_step("生成第 6 至第 10 章的章节级细纲")),
            Some((6, 10))
        );
    }

    /// 重发同一句请求不该重写已存在的细纲：这是「已执行步骤重复执行」的直接成因。
    fn gap_book(name: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("storydesk-gap-{name}-{nanos}"));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("大纲")).unwrap();
        root
    }

    fn gap_outline(root: &std::path::Path, chapter: u32) {
        std::fs::write(
            root.join(format!("大纲/细纲_第{chapter:03}章.md")),
            format!("# 第 {chapter} 章\n\n所属事件：无\n\n## 本章目标\n\n对账。\n"),
        )
        .unwrap();
    }

    /// 收尾章的软下限：要求 6000 字 → 收尾章按 4500 验收；小要求不被压到 600 以下。
    #[test]
    fn ending_chapter_accepts_a_soft_floor() {
        assert_eq!(ending_floor(6000), 4500);
        assert_eq!(ending_floor(800), 600, "软下限不许低于 600 字");
        assert_eq!(ending_floor(0), 0, "没有字数要求时不做软下限");
    }

    #[test]
    fn outline_gap_plan_skips_a_range_that_is_already_complete() {
        let root = gap_book("full");
        for chapter in 301..=303 {
            gap_outline(&root, chapter);
        }
        let mut notes: Vec<String> = Vec::new();
        let task = outline_gap_plan(Some(&root), 301, 303, None, None, &mut notes);
        assert!(task.is_none(), "细纲齐备就不该再派发：{task:?}");
        assert!(notes.iter().any(|n| n.contains("细纲齐备")), "{notes:?}");
    }

    #[test]
    fn outline_gap_plan_only_asks_for_the_missing_chapters() {
        let root = gap_book("partial");
        gap_outline(&root, 301);
        gap_outline(&root, 303);
        let mut notes: Vec<String> = Vec::new();
        let task = outline_gap_plan(Some(&root), 301, 303, None, None, &mut notes).unwrap();
        assert!(task.contains("大纲/细纲_第302章.md"), "{task}");
        assert!(!task.contains("大纲/细纲_第301章.md"), "已存在的章不许再写：{task}");
        assert!(!task.contains("大纲/细纲_第303章.md"), "已存在的章不许再写：{task}");
        assert!(task.contains("一个字都不要改"), "{task}");
        assert!(notes.iter().any(|n| n.contains("只补 1 章")), "{notes:?}");
    }

    #[test]
    fn outline_gap_plan_asks_for_the_whole_range_when_nothing_exists() {
        let root = gap_book("empty");
        let mut notes: Vec<String> = Vec::new();
        let task = outline_gap_plan(Some(&root), 5, 6, None, None, &mut notes).unwrap();
        assert_eq!(task, intent::outline_batch_task(5, 6, None, None));
        assert!(notes.is_empty(), "{notes:?}");
    }

    #[test]
    fn steps_done_summary_counts_the_steps_that_produced_nothing() {
        assert_eq!(steps_done_summary(3, &[]), "3 个步骤全部执行完成。");
        let notes = vec![
            "第 301 章未写，批量流程跳过该章继续：第 301 章没有细纲".to_string(),
            "第 302 章未写，批量流程跳过该章继续：第 302 章没有细纲".to_string(),
            "已写入 2 份细纲".to_string(),
        ];
        let text = steps_done_summary(3, &notes);
        assert!(text.contains("其中 2 步没有产出"), "{text}");
        assert!(!text.contains("全部执行完成"), "{text}");
    }
    fn agents() -> Vec<AgentConfig> {
        ai::default_agents()
    }

    fn step(agent: &str, task: &str) -> PlanStep {
        PlanStep {
            agent_id: agent.to_string(),
            task: task.to_string(),
            ..Default::default()
        }
    }

    /// 反向保护：任务里顺手提到的一章，不许把整段批量区间压成单章。
    ///
    /// 「参照 正文/第005章_旧稿.md 的写法把这一批写完」这种任务文本里只有一个章号，
    /// 若让文本无条件优先，`range: Span(331,350)` 会被压成第 5 章，二十章变一步。
    #[test]
    fn cited_single_chapter_does_not_shrink_range_field() {
        let parent = PlanStep {
            range: Some(ChapterRange::Span(331, 350)),
            ..step("writer", "参照 正文/第005章_旧稿.md 的写法，把这一批正文写完")
        };
        assert_eq!(
            normalize_step(parent.clone(), &agents()).range,
            Some(ChapterRange::Span(331, 350)),
            "字段区间更宽时不许被文本里的单章引用压小"
        );
        let (expanded, _, _) = expand_batch_steps(vec![parent], &agents(), None, None, None);
        assert_eq!(
            expanded.len(),
            20,
            "331—350 必须拆满 20 步：{:?}",
            expanded.iter().map(|s| s.range).collect::<Vec<_>>()
        );
    }

    /// R4：拆步必须认 `range` 字段——任务文本里一个章号都没有时也要拆成逐章子步。
    ///
    /// 真实计划里模型常写「按细纲写这三万字」+ `range: Span(3,12)`：旧实现只从文本抠
    /// 章号，抠不到就整段当一步，模型自己挑着写，用户看到的是「十章正文一次跑完」。
    #[test]
    fn batch_split_prefers_the_range_field_over_the_task_text() {
        let parent = PlanStep {
            range: Some(ChapterRange::Span(3, 12)),
            words: Some((30_000, 30_000)),
            ..step("writer", "按细纲写这三万字")
        };
        let (out, chapters, notes) = expand_batch_steps(vec![parent], &agents(), None, None, None );
        assert_eq!(out.len(), 10, "第 3 至 12 章必须拆成 10 步：{notes:?}");
        assert_eq!(chapters.len(), 10);
        for (offset, child) in out.iter().enumerate() {
            let chapter = 3 + offset as u32;
            assert_eq!(child.range, Some(ChapterRange::Single(chapter)));
            assert_eq!(child.kind, Some(RunKind::Prose));
            assert_eq!(child.role, Some(ai::AgentRole::Writer));
            assert_eq!(child.read_only, Some(false));
            assert!(
                child.task.contains(&format!("本次只写第 {chapter} 章")),
                "子步文本要锁定本章：{}",
                child.task
            );
            // 父步三万字的批量总量按章均摊：每章 3000 字，而不是每章都三万字。
            assert_eq!(child.words, Some((3000, 3000)));
        }
    }

    /// R4：细纲批量拆步同样继承结构化字段（锁、kind、区间）。
    #[test]
    fn outline_batch_children_keep_the_parent_structure() {
        let parent = PlanStep {
            range: Some(ChapterRange::Span(1, 12)),
            words: Some((3000, 3000)),
            ..step("architect", "把这一卷细化到章级大纲")
        };
        let (out, _, _) = expand_batch_steps(vec![parent], &agents(), Some("古代的农民起事"), None, None );
        assert_eq!(out.len(), 3, "12 章按每块 5 章拆成 3 步");
        let spans: Vec<Option<ChapterRange>> = out.iter().map(|s| s.range).collect();
        assert_eq!(
            spans,
            vec![
                Some(ChapterRange::Span(1, 5)),
                Some(ChapterRange::Span(6, 10)),
                Some(ChapterRange::Span(11, 12)),
            ]
        );
        assert_eq!(
            out[0].lock,
            Some(OutlineLock::Chapters(1, 5)),
            "子步必须带自己的细纲锁"
        );
        assert_eq!(out[2].lock, Some(OutlineLock::Chapters(11, 12)));
        for child in &out {
            assert_eq!(child.kind, Some(RunKind::Outline));
            assert_eq!(child.role, Some(ai::AgentRole::Architect));
        }
        // 3000 字按 12 章均摊 → 每章 250，头两块各 5 章 = 1250。
        assert_eq!(out[0].words, Some((1250, 1250)));
        assert_eq!(out[2].words, Some((500, 500)));
    }

    /// R4 标准化层：模型只写中文任务时，字段由标准化层补齐。
    #[test]
    fn normalize_step_fills_fields_for_text_only_plans() {
        let out = normalize_step(step("writer", "写第005章正文"), &agents());
        assert_eq!(out.range, Some(ChapterRange::Single(5)));
        assert_eq!(out.kind, Some(RunKind::Prose));
        assert_eq!(out.role, Some(ai::AgentRole::Writer));
        assert_eq!(out.read_only, Some(false));
        assert_eq!(out.words, None, "文本里的字数不补进字段");
    }

    /// 收尾识别：只认真正的收尾说法，不认「每章结尾钩子」这类章末写法要求。
    #[test]
    fn ending_detection_ignores_hook_talk() {
        assert!(intent::is_ending_request("没有结尾，再写第300至320章进行结尾处理"));
        assert!(intent::is_ending_request("在330章完成全书收尾与完结"));
        assert!(!intent::is_ending_request("检查每章的结尾钩子是否太硬"));
        assert!(!intent::is_ending_request("帮我写第7章正文"));
        // 事故回归（2026-11-16 用户原话）：名词化的「作为整本书的结尾」旧列表一条都不命中，
        // 于是计划里没有收尾从句，细纲照旧邀请续写——用户看到的就是「没按我的意思写结尾」。
        assert!(intent::is_ending_request(
            "根据大纲，设定，追踪的内容开始写第301至330章的细纲作为整本书的结尾，强调字数要求:6000-6500字"
        ));
        assert!(intent::is_ending_request("以这一章作为全书结尾"));
        assert!(!intent::is_ending_request("检查全书每章的结尾钩子是否太密"));
    }

    /// 事故回归（2026-11-16 第二现场）：用户说「没有结尾，再写第300至320章进行结尾处理」，
    /// 计划被拆成逐章正文步，每步都照写手硬规则「章末落在一句没答完的话上」，
    /// 细纲第 320 章自己还写着「后续如续写（事件307）…」——「写结尾」于是变成「一直续写」。
    /// 收尾请求必须把「这是最后一章」钉在终点那一步上。
    #[test]
    fn ending_request_pins_the_last_chapter_as_the_ending() {
        let request = "没有结尾，再写第300至320章进行结尾处理";
        let mut plan = MasterPlan {
            source: "rules".into(),
            intent: None,
            steps: vec![step("writer", "写第300至320章正文")],
            raw: String::new(),
        };
        enforce_topic_and_settings(&mut plan, Path::new("不存在"), &agents(), request, 1);
        assert!(
            plan.intent.as_deref().unwrap_or_default().contains("全书收尾"),
            "计划卡要写清这轮是收尾：{:?}",
            plan.intent
        );
        let marked: Vec<&PlanStep> = plan
            .steps
            .iter()
            .filter(|s| s.task.contains(intent::ENDING_MARK))
            .collect();
        // 计划里可能既有批量正文步，也有补细纲的 architect 步：两步都只钉「第 320 章」这一章。
        assert!(
            (1..=2).contains(&marked.len()),
            "收尾标记只该出现在终点那一章（正文一步、细纲一步）：{marked:#?}"
        );
        for s in &marked {
            assert!(s.task.contains("第 320 章"), "收尾要落在最后一章：{}", s.task);
        }
        let prose = marked
            .iter()
            .find(|s| ai::resolve_agent(&agents(), &s.agent_id).target_dir == "正文")
            .expect("正文步必须带收尾标记");
        assert!(prose.task.contains("（全书完）"), "收尾章必须标全书完：{}", prose.task);
        assert!(prose.task.contains("不得再开新人物"), "收尾章不许再接新线：{}", prose.task);
    }


    /// 用户真实请求（m00804 同款）：「写第301至330章的正文作为整本书的结尾」。
    /// 30 个逐章正文步里，收尾标记只许落在第 330 章那一步上。
    #[test]
    fn ending_request_marks_the_last_of_thirty_chapter_steps() {
        let request =
            "根据大纲，设定，追踪的内容开始写第301至330章的正文作为整本书的结尾，强调字数要求:6000-6500字";
        let mut plan = MasterPlan {
            source: "rules".into(),
            intent: None,
            steps: (301..=330u32)
                .map(|c| PlanStep {
                    range: Some(ChapterRange::Single(c)),
                    ..step("writer", &format!("写第 {c} 章正文"))
                })
                .collect(),
            raw: String::new(),
        };
        enforce_topic_and_settings(&mut plan, Path::new("不存在"), &agents(), request, 301);
        let marked: Vec<usize> = plan
            .steps
            .iter()
            .enumerate()
            .filter(|(_, s)| s.task.contains(intent::ENDING_MARK))
            .map(|(i, _)| i)
            .collect();
        // 计划里会留下两个批量步：补细纲的 architect 步 + 写正文的 writer 步，两者都只钉第 330 章。
        assert_eq!(
            marked.len(),
            2,
            "收尾标记只该落在终点那一步（正文一步、细纲一步）：{marked:?}"
        );
        for i in &marked {
            let st = &plan.steps[*i];
            assert!(st.task.contains("第 330 章"), "收尾必须落第 330 章：{}", st.task);
        }
        let prose = plan
            .steps
            .iter()
            .find(|s| s.task.contains(intent::ENDING_MARK) && s.agent_id == "writer")
            .expect("正文步必须带收尾标记");
        assert!(prose.task.contains("（全书完）"), "收尾章要标全书完：{}", prose.task);
        assert!(!plan.steps[0].task.contains(intent::ENDING_MARK), "第 301 章不许被当成结尾");

        // 拆步之后也只有一个子步带标记：否则第 301 章就被要求「全书完」，字数对账还会
        // 因为 ending_step 把每一章的补写上限压成 1 次。
        let parent = PlanStep {
            range: Some(ChapterRange::Span(301, 330)),
            ..step("writer", &prose.task)
        };
        let (out, _, _) = expand_batch_steps(vec![parent], &agents(), None, None, None);
        assert_eq!(out.len(), 30, "30 章要拆成 30 步");
        let marked_chapters: Vec<u32> = out
            .iter()
            .filter(|s| s.task.contains(intent::ENDING_MARK))
            .filter_map(|s| match s.range {
                Some(ChapterRange::Single(c)) => Some(c),
                _ => None,
            })
            .collect();
        assert_eq!(marked_chapters, vec![330], "只有末章子步带收尾从句：{marked_chapters:?}");
        assert!(
            !out[0].task.contains(intent::ENDING_MARK),
            "第 301 章子步不许带收尾从句：{}",
            out[0].task
        );
        assert!(
            out[0].task.contains("本次只写第 301 章"),
            "子步仍要锁定自己那一章：{}",
            out[0].task
        );
    }

    /// 收尾请求不带章号（用户原话就是「收尾小说」）：
    /// 结尾章号取自书目录（正文/细纲/索引里最靠后的章），不许把
    /// 「这份计划覆盖到的最远章」当全书结尾——那正是 335 抢收尾的第二个入口。
    #[test]
    fn ending_request_without_a_range_uses_the_book_end_hint() {
        let root = std::env::temp_dir().join("storydesk-master-ending-hint");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("大纲")).unwrap();
        std::fs::create_dir_all(root.join("正文")).unwrap();
        std::fs::write(root.join("大纲/细纲_第350章.md"), "# 第350章细纲\n").unwrap();
        std::fs::write(root.join("正文/第340章_旧章.md"), "旧正文").unwrap();

        // 计划只排到 345：够不到全书结尾（350），一步都不许自称结局。
        let mut plan = MasterPlan {
            source: "rules".into(),
            intent: None,
            steps: (341..=345u32)
                .map(|c| PlanStep {
                    range: Some(ChapterRange::Single(c)),
                    ..step("writer", &format!("写第 {c} 章正文"))
                })
                .collect(),
            raw: String::new(),
        };
        attach_ending_clauses(&mut plan, &agents(), Some(&root), "收尾小说");
        assert!(
            plan.steps.iter().all(|s| !s.task.contains(intent::ENDING_MARK)),
            "计划够不到结尾章，就不许有任何一步自称全书最后一章：{:#?}",
            plan.steps
        );
        let note = plan.intent.clone().unwrap_or_default();
        assert!(note.contains("全书结尾在第 350 章"), "要说明结尾在哪：{note}");
        assert!(note.contains("依据 大纲/细纲_第350章.md"), "要说明依据：{note}");

        // 把结尾章补进计划：收尾只落在第 350 章，且计划卡写清依据。
        plan.steps.push(PlanStep {
            range: Some(ChapterRange::Single(350)),
            ..step("writer", "写第 350 章正文")
        });
        attach_ending_clauses(&mut plan, &agents(), Some(&root), "收尾小说");
        let marked: Vec<&PlanStep> = plan
            .steps
            .iter()
            .filter(|s| s.task.contains(intent::ENDING_MARK))
            .collect();
        assert_eq!(marked.len(), 1, "只有结尾章带收尾从句：{marked:#?}");
        assert!(marked[0].task.contains("第 350 章是全书最后一章"), "{}", marked[0].task);
        let note = plan.intent.clone().unwrap_or_default();
        assert!(note.contains("以第 350 章收束"), "计划卡要写清收在哪：{note}");
        assert!(note.contains("依据 大纲/细纲_第350章.md"), "计划卡要写清依据：{note}");
    }

    /// 复现《隐藏条款》13:12 那轮的事故：细纲步只排到 335，正文步排到 350。
    ///
    /// 旧代码把「这一步自己的区间末端」当收尾章，于是 331—335 的细纲步被写成
    /// 「第 335 章是全书最后一章的细纲」，正文第 350 章那条写的是 350——
    /// 细纲把 331—335 当结局收干净，336—350 又被当成可以继续连载，收尾落到了 335。
    /// 正确行为：只有覆盖到全书结尾章（350）的那一步才配收尾从句，335 那条不许带。
    #[test]
    fn a_partial_outline_step_never_claims_to_be_the_book_ending() {
        let request = "根据大纲、设定、追踪的内容开始写第331至350章的正文，作为整本书的结尾，\
                       强调字数要求:6000-6500字";
        let mut steps: Vec<PlanStep> = Vec::new();
        steps.push(PlanStep {
            range: Some(ChapterRange::Span(331, 335)),
            ..step("architect", "生成第331至335章章级细纲")
        });
        for c in 331..=350u32 {
            steps.push(PlanStep {
                range: Some(ChapterRange::Single(c)),
                ..step("writer", &format!("写第 {c} 章正文"))
            });
        }
        let mut plan = MasterPlan {
            source: "rules".into(),
            intent: None,
            steps,
            raw: String::new(),
        };
        enforce_topic_and_settings(&mut plan, Path::new("不存在"), &agents(), request, 331);

        let marked: Vec<&PlanStep> = plan
            .steps
            .iter()
            .filter(|s| s.task.contains(intent::ENDING_MARK))
            .collect();
        assert!(!marked.is_empty(), "至少要有一个步骤带收尾从句");
        for s in &marked {
            assert!(
                s.task.contains("第 350 章是全书最后一章"),
                "收尾章必须是 350：{:#?}",
                s
            );
            assert!(
                !s.task.contains("第 335 章是全书最后一章"),
                "335 不许被当成全书最后一章：{}",
                s.task
            );
        }
        assert!(
            marked
                .iter()
                .any(|s| ai::resolve_agent(&agents(), &s.agent_id).target_dir == "正文"),
            "正文步必须带收尾从句：{marked:#?}"
        );
    }

    /// 细纲批量的收尾从句只跟「含末章的那一块」走。
    ///
    /// 30 章按每块 5 章拆成 6 块，若每块都带收尾从句，前 5 块都会被告知「本章是全书最后一章」。
    #[test]
    fn outline_chunks_keep_the_ending_clause_on_the_last_block_only() {
        let parent = PlanStep {
            range: Some(ChapterRange::Span(301, 330)),
            task: format!(
                "把第301至330章细化到章级大纲（每章 6000-6500 字）{}",
                intent::ending_outline_clause(330)
            ),
            ..step("architect", "把第301至330章细化到章级大纲")
        };
        let (out, _, _) = expand_batch_steps(vec![parent], &agents(), Some("都市悬疑"), None, None);
        assert_eq!(out.len(), 6, "30 章按每块 5 章拆成 6 块：{}", out.len());
        let marked: Vec<Option<ChapterRange>> = out
            .iter()
            .filter(|s| s.task.contains(intent::ENDING_MARK))
            .map(|s| s.range)
            .collect();
        assert_eq!(
            marked,
            vec![Some(ChapterRange::Span(326, 330))],
            "只有含末章的块带收尾从句：{marked:?}"
        );
        assert!(
            !out[0].task.contains(intent::ENDING_MARK),
            "首块不许带收尾从句：{}",
            out[0].task
        );
    }

    /// 事故回归（2026-11-16 现场）：用户要「重新生成全书大纲」，architect 步的任务
    /// 已经带本地注入的「【全书大纲】」标记，`lock` 却停在 enforce 改写前的
    /// `Chapters(1,1)`。执行期按字段注入「【章号锁定】只允许写 大纲/细纲_第001章.md」，
    /// 模型照锁只交付第 1 章细纲并回报与补充要求冲突 = 用户看到的「词不达意」。
    #[test]
    fn book_outline_marker_outranks_a_stale_chapter_lock() {
        let mut stale = step(
            "architect",
            "【全书大纲】重新生成 大纲/大纲.md 全书大纲，写到卷级为止",
        );
        stale.lock = Some(OutlineLock::Chapters(1, 1));
        stale.kind = Some(RunKind::Outline);
        let out = normalize_step(stale, &agents());
        assert_eq!(out.lock, Some(OutlineLock::Book));
        let lock = outline_lock_for(&out, 1);
        assert_eq!(lock, OutlineLock::Book);
        assert!(outline_file_allowed("大纲/大纲.md", lock));
        assert!(!outline_file_allowed("大纲/细纲_第001章.md", lock));
    }

    /// 全书大纲闸改完任务文本后，字段锁必须跟着变成 Book——事故现场里 rules 与 llm
    /// 两条计划都停在章级锁上。
    #[test]
    fn enforce_book_outline_resyncs_the_lock_field() {
        let request = "重新生成全书大纲";
        let found = intent::classify(request);
        let chapter_task = intent::outline_task(1, None, true, "用户点名要大纲", None);
        let mut plan = MasterPlan {
            source: "rules".to_string(),
            intent: None,
            steps: vec![PlanStep {
                lock: Some(OutlineLock::Chapters(1, 1)),
                range: Some(ChapterRange::Single(1)),
                ..step("architect", &chapter_task)
            }],
            raw: String::new(),
        };
        enforce_outline_and_settings(&mut plan, &agents(), request, &found, 1, None);
        let architect = plan
            .steps
            .iter()
            .find(|s| s.agent_id == "architect")
            .expect("必须留下 architect 步");
        assert_eq!(
            architect.lock,
            Some(OutlineLock::Book),
            "任务已升级为全书大纲，锁却还是章级：{}",
            architect.task
        );
    }

    /// 区间反了（`Span(9,3)`）时不能当有效范围用，要回落到文本解析。
    #[test]
    fn normalize_step_repairs_a_reversed_range() {
        let broken = PlanStep {
            range: Some(ChapterRange::Span(9, 3)),
            ..step("writer", "写第007章正文")
        };
        let out = normalize_step(broken, &agents());
        assert_eq!(out.range, Some(ChapterRange::Single(7)));
    }

    /// R4：模型带回来的结构化字段必须真正传到生产步骤上。
    #[test]
    fn parse_plan_keeps_structured_fields_from_the_model() {
        let raw = r#"好的，计划如下：
```json
{"plan":[{"agentId":"writer","task":"按细纲写这三万字","range":{"type":"span","value":[3,12]},"words":[1000,1200],"readOnly":false}]}
```"#;
        let steps = parse_plan(raw, &agents()).expect("应解析出计划");
        assert_eq!(steps.len(), 1);
        let first = &steps[0];
        assert_eq!(first.agent_id, "writer");
        assert_eq!(
            first.range,
            Some(ChapterRange::Span(3, 12)),
            "range 不能在解析层被扔掉"
        );
        assert_eq!(first.words, Some((1000, 1200)));
        assert_eq!(first.kind, Some(RunKind::Prose));
        assert_eq!(first.role, Some(ai::AgentRole::Writer));
        assert_eq!(first.read_only, Some(false));
    }

    /// 字数验收：结构化 `words` 优先于任务文本里的自然语言字数。
    #[test]
    fn words_field_beats_text_parsing() {
        let explicit = PlanStep {
            words: Some((3000, 3200)),
            ..step("writer", "写第005章正文，2500 字左右")
        };
        assert_eq!(step_words(&explicit), Some((3000, 3200)));

        // 字段无效（0 或反向）时不能让字数验收整个失效，退回文本解析。
        let broken = PlanStep {
            words: Some((0, 0)),
            ..step("writer", "写第005章正文，2500 字左右")
        };
        let parsed = step_words(&broken).expect("坏字段要回落到文本解析");
        assert!(
            parsed.0 <= 2500 && 2500 <= parsed.1,
            "文本里的 2500 字要落在解析区间内，实际 {parsed:?}"
        );

        let text_only = step("writer", "写第005章正文，不少于 3000 字不超过 3500 字");
        assert_eq!(step_words(&text_only), Some((3000, 3500)));
    }

    /// 任务 6 的后端侧：用户把「第 3 章」改成「第 5 章」时前端会清空六个字段，
    /// 后端必须只按新文本重新解析——旧范围绝不能继续参与闸层判断。
    #[test]
    fn cleared_fields_let_the_edited_text_win() {
        let edited = PlanStep {
            agent_id: "writer".into(),
            task: "写第005章正文（原来写的是第 3 章）".into(),
            ..Default::default()
        };
        let out = normalize_step(edited, &agents());
        assert_eq!(out.range, Some(ChapterRange::Single(5)));
        assert!(step_covers_chapter(&out, 5));
        assert!(
            !step_covers_chapter(&out, 3),
            "旧的第 3 章范围必须失效，否则第 5 章的稿子会被判越界回滚"
        );
    }

    /// 生产链顺序：解析 → 标准化 → 拆步 → 排序，拆出来的十步都要能被闸层认领。
    #[test]
    fn production_pipeline_splits_the_edited_span_into_ten_covered_steps() {
        let raw = r#"{"plan":[{"agentId":"writer","task":"按细纲写这三万字","range":{"type":"span","value":[3,12]},"words":[30000,30000]}]}"#;
        let parsed = parse_plan(raw, &agents()).expect("解析失败");
        let all_agents = agents();
        let normalized: Vec<PlanStep> = parsed
            .into_iter()
            .map(|s| normalize_step(s, &all_agents))
            .collect();
        let (children, _covered, _notes) = expand_batch_steps(normalized, &all_agents, None, None, None );
        let ordered = normalize_step_order(children);
        assert_eq!(ordered.len(), 10, "第 3–12 章必须拆成 10 步");
        for (i, child) in ordered.iter().enumerate() {
            let chapter = 3 + i as u32;
            assert_eq!(child.range, Some(ChapterRange::Single(chapter)));
            assert_eq!(child.kind, Some(RunKind::Prose));
            assert!(
                step_covers_chapter(child, chapter),
                "第 {chapter} 章的闸层认不到自己"
            );
            assert_eq!(child.words, Some((3000, 3000)), "字数要按章均摊");
        }
    }

    /// 规则 1 的正向：只读核对步引用的是还没写到的章 → 挪到该章的写步之后。
    #[test]
    fn check_step_moves_after_the_chapter_it_checks() {
        let steps = vec![
            step("narrative-writer", "写第003章正文"),
            step("story-architect", "核对第012章的细纲是否与设定一致"),
            step("narrative-writer", "写第012章正文"),
        ];
        let out = normalize_step_order(steps);
        let tasks: Vec<&str> = out.iter().map(|s| s.task.as_str()).collect();
        assert_eq!(tasks[0], "写第003章正文");
        assert_eq!(tasks[1], "写第012章正文");
        assert!(tasks[2].contains("核对第012章"), "{tasks:?}");
    }

    /// 规则 1 的反向：引用的章号不大于已写到的最大章号 → 原地不动。
    #[test]
    fn check_step_for_an_already_written_chapter_stays_put() {
        let steps = vec![
            step("narrative-writer", "写第003章正文"),
            step("story-architect", "核对第003章的正文与细纲是否一致"),
            step("narrative-writer", "写第012章正文"),
        ];
        let out = normalize_step_order(steps);
        let tasks: Vec<&str> = out.iter().map(|s| s.task.as_str()).collect();
        assert_eq!(tasks[0], "写第003章正文");
        assert!(tasks[1].contains("核对第003章"), "{tasks:?}");
        assert_eq!(tasks[2], "写第012章正文");
    }

    /// 规则 2：写步相对顺序不变；同批次里没有该章写步的只读步也不许乱挪。
    #[test]
    fn write_steps_keep_their_order_and_unanchored_checks_stay() {
        let steps = vec![
            step("narrative-writer", "写第002章正文"),
            step("story-architect", "梳理第040章的伏笔时间线"),
            step("narrative-writer", "写第005章正文"),
            step("story-architect", "核对第005章正文与细纲"),
        ];
        let out = normalize_step_order(steps);
        let tasks: Vec<&str> = out.iter().map(|s| s.task.as_str()).collect();
        assert_eq!(tasks.len(), 4);
        assert_eq!(tasks[0], "写第002章正文");
        assert!(tasks[1].contains("第040章"), "{tasks:?}");
        assert_eq!(tasks[2], "写第005章正文");
        assert!(tasks[3].contains("核对第005章"), "{tasks:?}");
    }

    /// 实测事故：用户点名「结合参考资料、追踪」，任务被改写成闸层模板后这句蒸发，
    /// 计划卡与派发任务脱节。现在素材段必须注入大纲/正文步骤，且只注一次。
    #[test]
    fn attach_sources_clause_only_hits_write_steps() {
        let mut plan = MasterPlan {
            source: "rules".to_string(),
            intent: None,
            steps: vec![
                PlanStep {
                    agent_id: "writer".to_string(),
                    task: "写第 1 章正文".to_string(),
                    ..Default::default()
                },
                PlanStep {
                    agent_id: "tracker".to_string(),
                    task: "检查伏笔".to_string(),
                    ..Default::default()
                },
            ],
            raw: String::new(),
        };
        let request = "结合参考资料和追踪写第一章正文";
        attach_sources_clause(&mut plan, &agents(), request);
        assert!(
            plan.steps[0].task.contains("【作者指定结合素材】"),
            "{:?}",
            plan.steps
        );
        assert!(
            !plan.steps[1].task.contains("【作者指定结合素材】"),
            "{:?}",
            plan.steps
        );
        // 再挂一次不许重复
        attach_sources_clause(&mut plan, &agents(), request);
        assert_eq!(
            plan.steps[0].task.matches("【作者指定结合素材】").count(),
            1
        );
        // 没点名素材的原话：什么都不加
        let mut plan2 = MasterPlan {
            source: "rules".to_string(),
            intent: None,
            steps: vec![PlanStep {
                agent_id: "writer".to_string(),
                task: "写第 2 章正文".to_string(),
                ..Default::default()
            }],
            raw: String::new(),
        };
        attach_sources_clause(&mut plan2, &agents(), "帮我写第二章正文");
        assert!(!plan2.steps[0].task.contains("【作者指定结合素材】"));
    }

    /// 批量细纲拆子区间重写任务时，素材段必须原样带进每一组。
    #[test]
    fn batch_outline_chunks_carry_sources_clause() {
        let clause = intent::sources_clause("结合参考资料写001到012的细纲").expect("有素材段");
        let base = intent::outline_batch_task(1, 12, None, None);
        let steps = vec![PlanStep {
            agent_id: "architect".to_string(),
            task: format!("{base}\n{clause}"),
            ..Default::default()
        }];
        let (expanded, _, _) = expand_batch_steps(steps, &agents(), None, None, None );
        assert!(
            expanded.len() >= 2,
            "12 章应拆成多组，实际 {} 组",
            expanded.len()
        );
        for st in &expanded {
            assert!(st.task.contains("【作者指定结合素材】"), "{}", st.task);
        }
    }

    /// 用户说「结合每个章节的细纲完成正文」而细纲已齐：不许再强制批量重生成细纲
    /// （多跑步骤 + 可能覆盖已有细纲）。缺一章就照常补。
    #[test]
    fn range_prose_with_complete_outlines_skips_architect() {
        let root = std::env::temp_dir().join(format!(
            "storydesk_outlines_ready_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("大纲")).unwrap();
        for ch in 1..=3 {
            std::fs::write(
                root.join("大纲").join(format!("细纲_第{ch:03}章.md")),
                "# 细纲\n冲突与钩子\n",
            )
            .unwrap();
        }
        let found = intent::classify("结合每个章节的细纲，完成第001至003章的正文");
        assert_eq!(
            found.chapter_range,
            Some((1, 3)),
            "{:?}",
            found.chapter_range
        );
        let mut plan = MasterPlan {
            source: "llm".to_string(),
            intent: None,
            steps: vec![
                PlanStep {
                    agent_id: "architect".to_string(),
                    task: intent::outline_batch_task(1, 3, None, None),
                    ..Default::default()
                },
                PlanStep {
                    agent_id: "writer".to_string(),
                    task: intent::writer_batch_task(1, 3, None, None),
                    ..Default::default()
                },
            ],
            raw: String::new(),
        };
        enforce_prose_plan(&mut plan, &root, &agents(), &found, 1, None);
        assert!(
            !plan.steps.iter().any(|s| s.agent_id == "architect"),
            "细纲齐备不应再补批量细纲步：{:?}",
            plan.steps
        );
        assert!(
            plan.intent
                .as_deref()
                .unwrap_or_default()
                .contains("细纲齐备"),
            "{:?}",
            plan.intent
        );

        // 缺一章就照常补细纲步
        std::fs::remove_file(root.join("大纲").join("细纲_第002章.md")).unwrap();
        let mut plan2 = MasterPlan {
            source: "llm".to_string(),
            intent: None,
            steps: vec![PlanStep {
                agent_id: "writer".to_string(),
                task: intent::writer_batch_task(1, 3, None, None),
                ..Default::default()
            }],
            raw: String::new(),
        };
        enforce_prose_plan(&mut plan2, &root, &agents(), &found, 1, None);
        assert!(
            plan2.steps.iter().any(|s| s.agent_id == "architect"),
            "缺细纲要补步：{:?}",
            plan2.steps
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 真实事故（截图 1-3）：用户说「完成 001 章的正文」，确认执行却按 005 章跑。
    /// 前端缓存的 chapter=5 不许压过用户原话里的 001。
    #[test]
    fn user_chapter_beats_the_cached_frontend_chapter() {
        assert_eq!(resolve_chapter("完成001章的正文", Some(5), Some(5), 3), 1);
        // request 为空（老前端/回放历史计划）时，退到计划步骤里写的章号
        assert_eq!(resolve_chapter("", Some(1), Some(5), 3), 1);
        // 计划里也没写章号，才用前端传来的
        assert_eq!(resolve_chapter("", None, Some(7), 3), 7);
        // 全都没有：正文目录的下一章
        assert_eq!(resolve_chapter("", None, None, 3), 3);
        // 「第 2 章」这类带空格/中文数字的写法也要认
        assert_eq!(resolve_chapter("接着写第 2 章", None, Some(9), 3), 2);
    }

    /// 心跳的线上格式必须和前端 `MasterProgressPayload` 对上：
    /// 字段名错一个字母，界面就永远收不到进度，又退回「看着像卡住」。
    #[test]
    fn heartbeat_payload_matches_the_frontend_contract() {
        let beat = BeatInfo {
            run_id: "run-1".to_string(),
            step_run_id: "run-1-step1".to_string(),
            index: 0,
            total: 2,
            agent_id: "writer".to_string(),
            agent_name: "主笔写手".to_string(),
            target_dir: "正文".to_string(),
            chapter: Some(3),
            done: 1,
            resume_from: Some(2),
            persisted: true,
        };
        let payload = beat.payload("running", 1234);
        assert_eq!(payload.status, "running");
        assert_eq!(payload.elapsed_ms, 1234);
        assert_eq!(payload.chapter, Some(3));
        let json = serde_json::to_string(&payload).unwrap();
        for key in [
            "\"runId\"",
            "\"agentId\"",
            "\"agentName\"",
            "\"targetDir\"",
            "\"status\"",
            "\"elapsedMs\"",
            "\"chunks\"",
            "\"chapter\"",
            // 断点续跑的可见性：已完成步数 / 续跑起点 / 是否已落盘。
            "\"done\"",
            "\"resumeFrom\"",
            "\"persisted\"",
        ] {
            assert!(json.contains(key), "心跳载荷缺字段 {key}：{json}");
        }
    }

    #[test]
    fn rules_plan_puts_outline_before_prose() {
        let steps = plan_locally(&agents(), "帮我写第一章正文", 1, None).steps;
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].agent_id, "architect");
        assert_eq!(steps[1].agent_id, "writer");
        assert!(steps[0].task.contains("第 1 章"));
    }

    #[test]
    fn rules_plan_reads_novel_request_as_prose() {
        // 阶段 11 的核心回归：这句话以前只命中「设定」，兜底派给 architect，
        // architect 为了交差写了份细纲盖掉旧的。现在必须判成写正文。
        let request = "把设定里的几个文件内容给我写了，要写一本古代的农民起事的小说";
        let local = plan_locally(&agents(), request, 1, None);
        assert!(local.intent.contains("写正文"), "{}", local.intent);
        assert_eq!(
            local.steps.last().map(|s| s.agent_id.as_str()),
            Some("writer"),
            "最后一步必须是写手"
        );
        // 派给写手的那条任务不能再是用户原话
        let task = &local.steps.last().expect("有步骤").task;
        assert!(!task.contains("给我写了"), "{task}");
        assert!(task.contains("正文"), "{task}");
    }

    #[test]
    fn chapter_from_task_text() {
        assert_eq!(chapter_in_task("生成第 1 章细纲：目标、冲突"), Some(1));
        assert_eq!(chapter_in_task("按第 3 章细纲写本章正文"), Some(3));
        assert_eq!(chapter_in_task("按第十二章细纲写正文"), Some(12));
        assert_eq!(chapter_in_task("检查伏笔与时间线"), None);
    }

    /// 阶段 11.5 的核心回归：旧细纲属于另一本书时必须作废重写。
    #[test]
    fn stale_outline_is_regenerated_for_the_new_genre() {
        let root = std::env::temp_dir().join("storydesk-master-stale");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("大纲")).unwrap();
        std::fs::create_dir_all(root.join("设定")).unwrap();
        std::fs::write(
            root.join("大纲/细纲_第001章.md"),
            "# 第001章 雨夜来客\n\n客栈的雨夜，来客敲开木门。\n",
        )
        .unwrap();
        std::fs::write(root.join("设定/题材定位.md"), "古代农民起事，群像。\n").unwrap();

        let request = "把设定里的几个文件内容给我写了，要写一本古代的农民起事的小说";
        let local = plan_locally(&agents(), request, 1, Some(&root));
        // 设定偏薄时计划卡写明「先补基础设定」：补设定 → 重生成细纲 → 写手。
        let architect_idx = local
            .steps
            .iter()
            .position(|s| s.agent_id == "architect" && s.event.is_none())
            .expect("旧细纲题材不符，必须有架构师重新生成");
        assert!(
            local.steps[..architect_idx]
                .iter()
                .all(|s| s.agent_id == "settings" || s.event.as_ref().is_some_and(|e| e.is_card_step())),
            "架构师之前只允许是补设定：{:?}",
            local.steps
        );
        let first = &local.steps[architect_idx].task;
        assert!(first.contains("重新生成"), "{first}");
        assert!(first.contains("作废"), "{first}");
        let last = local.steps.last().expect("有步骤");
        assert_eq!(last.agent_id, "writer");
        assert!(last.task.contains("古代的农民起事"), "{}", last.task);
        assert!(last.task.contains("不要沿用"), "{}", last.task);
        // 计划卡要能看出「题材切换 + 设定偏薄」
        assert!(local.intent.contains("整本从头写"), "{}", local.intent);
        assert!(local.intent.contains("设定偏薄"), "{}", local.intent);
    }

    /// 细纲与题材一致时不该重复生成，只派写手。
    #[test]
    fn matching_outline_is_kept_and_only_the_writer_runs() {
        let root = std::env::temp_dir().join("storydesk-master-keep");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("大纲")).unwrap();
        std::fs::create_dir_all(root.join("设定")).unwrap();
        std::fs::write(
            root.join("大纲/细纲_第001章.md"),
            "# 第001章 秋收之前\n古代的农民起事，村里的余粮被抢。\n字数目标：2400–2600 字。对白占四成。\n",
        )
        .unwrap();
        std::fs::write(
            root.join("设定/世界概览.md"),
            format!(
                "# 世界\n古平原，大河改道，官仓三层盘剥。{}\n",
                "漕运".repeat(120)
            ),
        )
        .unwrap();
        std::fs::write(
            root.join("设定/人物.md"),
            format!("# 人物\n李敢，佃户出身，会磨刀。{}\n", "阿禾".repeat(120)),
        )
        .unwrap();

        let local = plan_locally(&agents(), "帮我写第一章正文", 1, Some(&root));
        assert_eq!(local.steps.len(), 1, "{:?}", local.steps);
        assert_eq!(local.steps[0].agent_id, "writer");
        // 细纲里的字数与比例要求要进写手的任务
        assert!(
            local.steps[0].task.contains("2400-2600 字"),
            "{}",
            local.steps[0].task
        );
    }

    /// 事件层引导：书里还没有事件卡时，排章级细纲前先排一步「建事件卡」。
    #[test]
    fn event_step_precedes_the_first_chapter_outline() {
        let root = std::env::temp_dir().join("storydesk-master-event-bootstrap");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("大纲")).unwrap();
        std::fs::create_dir_all(root.join("设定")).unwrap();

        let local = plan_locally(&agents(), "写第 5 章细纲", 5, Some(&root));
        let event_idx = local
            .steps
            .iter()
            .position(|s| s.event.as_ref().is_some_and(|e| e.is_card_step()))
            .unwrap_or_else(|| panic!("应插一步事件卡：{:?}", local.steps));
        let outline_idx = local
            .steps
            .iter()
            .position(|s| s.agent_id == "architect" && s.task.contains("重新生成第 5 章细纲"))
            .unwrap_or_else(|| panic!("应有细纲步：{:?}", local.steps));
        assert!(
            event_idx < outline_idx,
            "事件卡必须排在细纲之前：{:?}",
            local.steps
        );
        let card = &local.steps[event_idx];
        assert!(card.task.contains("排事件卡"), "{}", card.task);
        assert_eq!(card.lock, Some(OutlineLock::OutlineDir));

        // 已经有事件卡就不再插：事件层只引导一次，之后由作者与模型维护。
        std::fs::create_dir_all(root.join("大纲/事件")).unwrap();
        std::fs::write(
            root.join("大纲/事件/事件_001_开局.md"),
            "# 事件 001 · 开局\n",
        )
        .unwrap();
        let again = plan_locally(&agents(), "写第 5 章细纲", 5, Some(&root));
        assert!(
            !again.steps.iter().any(|s| s.event.is_some()),
            "已有事件卡还插引导步：{:?}",
            again.steps
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 事件边界优先：同一事件覆盖的章归一步，子步带上事件归属。
    #[test]
    fn expand_splits_on_event_boundaries() {
        let root = std::env::temp_dir().join("storydesk-master-event-split");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("大纲/事件")).unwrap();
        std::fs::write(
            root.join("大纲/事件/事件_002_码头交接.md"),
            "# 事件 002 · 码头交接\n\n## 基本信息\n\n| 项 | 值 |\n| --- | --- |\n| 章号区间 | 2-6 |\n| 状态 | 进行 |\n\n## 章型分工\n\n| 章 | 章型 | 本章消耗 | 本章新增 |\n| --- | --- | --- | --- |\n| 2 | 起 | — | U-001 |\n",
        )
        .unwrap();
        let parent = PlanStep::outline_span("architect", 1, 8, Some("古代的农民起事"), None);
        let (out, covered, _) =
            expand_batch_steps(vec![parent], &agents(), None, None, Some(&root));
        let spans: Vec<(u32, u32, Option<String>)> = out
            .iter()
            .map(|s| match &s.range {
                Some(ChapterRange::Span(a, b)) => {
                    (*a, *b, s.event.as_ref().map(|e| e.id.clone()))
                }
                other => panic!("意外的范围：{other:?}"),
            })
            .collect();
        assert_eq!(
            spans,
            vec![(1, 1, None), (2, 6, Some("002".to_string())), (7, 8, None)],
            "{out:?}"
        );
        assert!(covered.contains(&6));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 模型给的计划里带一步「去沿用现有细纲」——细纲本来就能沿用时，这步要摘掉。
    #[test]
    fn reusable_outline_drops_the_architect_step() {
        let root = std::env::temp_dir().join("storydesk-master-drop");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("大纲")).unwrap();
        std::fs::create_dir_all(root.join("设定")).unwrap();
        std::fs::write(
            root.join("大纲/细纲_第001章.md"),
            "# 第001章 秋收之前\n古代的农民起事，村里的余粮被抢。字数目标：2400–2600 字。\n",
        )
        .unwrap();
        std::fs::write(
            root.join("设定/世界概览.md"),
            format!("# 世界\n{}\n", "漕运".repeat(150)),
        )
        .unwrap();
        std::fs::write(
            root.join("设定/人物.md"),
            format!("# 人物\n{}\n", "李敢".repeat(150)),
        )
        .unwrap();

        let mut plan = MasterPlan {
            source: "llm".to_string(),
            intent: Some("写正文 · 正文/ · 第 1 章".to_string()),
            steps: vec![
                PlanStep {
                    agent_id: "architect".to_string(),
                    task: "生成第 1 章细纲（已有细纲就沿用，不要覆盖）".to_string(),
                    ..Default::default()
                },
                PlanStep {
                    agent_id: "writer".to_string(),
                    task: "按第 1 章细纲写本章正文".to_string(),
                    ..Default::default()
                },
            ],
            raw: String::new(),
        };
        enforce_topic_and_settings(&mut plan, &root, &agents(), "帮我写第一章正文", 1);
        assert_eq!(plan.steps.len(), 1, "{:?}", plan.steps);
        assert_eq!(plan.steps[0].agent_id, "writer");
    }

    #[test]
    fn rules_plan_keeps_one_intent_per_round() {
        // 一轮只做一个意图：拆对标就只派对标，不再顺手塞一条伏笔步骤
        let local = plan_locally(&agents(), "拆一下对标的节奏", 3, None);
        assert_eq!(local.steps.len(), 1);
        assert_eq!(local.steps[0].agent_id, "benchmarker");
        assert!(!local.intent.contains("只读不改文件"), "{}", local.intent);

        // 伏笔单独说的时候仍然归 tracker
        let local = plan_locally(&agents(), "把伏笔账过一遍，哪些还没回收", 3, None);
        assert_eq!(local.steps[0].agent_id, "tracker");
    }

    #[test]
    fn rules_plan_sends_checks_to_the_tracker_as_read_only() {
        // 阶段 10 的核心回归：以前「检查伏笔与时间线」会落到 architect 手上，
        // 它为了交差直接 Write 覆盖大纲。现在必须一步、tracker、且带【只读模式】。
        let steps = plan_locally(&agents(), "检查伏笔与时间线", 1, None).steps;
        assert_eq!(steps.len(), 1, "检查类需求只该派一步");
        assert_eq!(steps[0].agent_id, "tracker");
        assert!(
            steps[0].task.starts_with("【只读模式】"),
            "{}",
            steps[0].task
        );
        assert!(steps[0].task.contains("不要修改任何文件"));
        assert!(
            !steps.iter().any(|s| s.agent_id == "architect"),
            "绝不能派建筑师去写"
        );
    }

    #[test]
    fn rules_plan_checks_settings_without_writing() {
        let steps = plan_locally(&agents(), "核对一下人物设定有没有自相矛盾", 2, None).steps;
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].agent_id, "settings");
        assert!(steps[0].task.starts_with("【只读模式】"));
    }

    /// 阶段 11.6 的核心回归：真实失败案例「把大纲写了，整本故事的大纲写好，
    /// 再把设定里这几个都写好」必须拆成「写全书大纲 → 补设定」两步，
    /// 不允许再降级成「第 1 章细纲 + 沿用现成的」。
    #[test]
    fn rules_plan_splits_whole_book_outline_then_settings() {
        let request = "把大纲写了，整本故事的大纲写好，再把设定里这几个都写好";
        let local = plan_locally(&agents(), request, 1, None);
        assert_eq!(local.steps.len(), 2, "{:?}", local.steps);
        assert_eq!(local.steps[0].agent_id, "architect");
        assert!(
            local.steps[0].task.contains("大纲/大纲.md"),
            "{}",
            local.steps[0].task
        );
        assert!(
            local.steps[0].task.contains("作废"),
            "{}",
            local.steps[0].task
        );
        assert!(
            !local.steps[0].task.contains("沿用"),
            "点名要大纲时禁止静默沿用：{}",
            local.steps[0].task
        );
        assert!(
            !local.steps[0].task.contains("第 1 章"),
            "{}",
            local.steps[0].task
        );
        assert_eq!(local.steps[1].agent_id, "settings");
        assert!(
            local.steps[1].task.contains("补全"),
            "{}",
            local.steps[1].task
        );
        assert!(
            local.steps[1].task.contains("大纲/"),
            "{}",
            local.steps[1].task
        );
        // 计划卡必须能看出两步的先后
        assert!(local.intent.contains("先"), "{}", local.intent);
        assert!(local.intent.contains("写整本大纲"), "{}", local.intent);
        assert!(local.intent.contains("补全设定"), "{}", local.intent);
    }

    /// 「生成第3章细纲」仍然是章级：只派 architect 写第 3 章细纲。
    #[test]
    fn rules_plan_keeps_chapter_outline_chapter_level() {
        let local = plan_locally(&agents(), "生成第3章细纲", 1, None);
        assert_eq!(local.steps.len(), 1, "{:?}", local.steps);
        assert_eq!(local.steps[0].agent_id, "architect");
        assert!(
            local.steps[0].task.contains("第 3 章"),
            "{}",
            local.steps[0].task
        );
        assert!(
            !local.steps[0].task.contains("大纲/大纲.md"),
            "{}",
            local.steps[0].task
        );
        assert!(!local.steps.iter().any(|s| s.agent_id == "settings"));
    }

    /// 模型给的降级计划（一步「生成第 1 章细纲（已有细纲就沿用）」）必须被
    /// `enforce_topic_and_settings` 修正成「写全书大纲 + 补设定」。
    #[test]
    fn llm_downgraded_outline_plan_is_fixed() {
        let mut plan = MasterPlan {
            source: "llm".to_string(),
            intent: None,
            steps: vec![PlanStep {
                agent_id: "architect".to_string(),
                task: "生成第 1 章细纲：目标、冲突、转折、结尾钩子（已有细纲就沿用，不要覆盖）"
                    .to_string(),
                ..Default::default()
            }],
            raw: String::new(),
        };
        let request = "把大纲写了，整本故事的大纲写好，再把设定里这几个都写好";
        let root = std::env::temp_dir().join("storydesk-master-llm-fix");
        enforce_topic_and_settings(&mut plan, &root, &agents(), request, 1);
        assert_eq!(plan.steps.len(), 2, "{:?}", plan.steps);
        assert_eq!(plan.steps[0].agent_id, "architect");
        assert!(
            plan.steps[0].task.contains("大纲/大纲.md"),
            "{}",
            plan.steps[0].task
        );
        assert!(
            !plan.steps[0].task.contains("沿用"),
            "{}",
            plan.steps[0].task
        );
        assert_eq!(plan.steps[1].agent_id, "settings");
        assert!(
            plan.steps[1].task.contains("补全"),
            "{}",
            plan.steps[1].task
        );
    }

    /// 真实事故：「补充每卷章数」被覆盖成「重写整本大纲、旧内容一律作废」。
    #[test]
    fn supplement_outline_request_patches_instead_of_rewriting() {
        let request = "大纲内没有写七个卷每个卷有多少章补充一下";
        let local = plan_locally(&agents(), request, 1, None);
        assert!(!local.steps.is_empty(), "{:?}", local.steps);
        assert!(
            local.steps[0].task.contains("【增量补大纲】"),
            "{}",
            local.steps[0].task
        );
        assert!(
            !local.steps[0]
                .task
                .contains("一律作废，其中的人物与情节不得保留"),
            "{}",
            local.steps[0].task
        );
        // 模型已经写对的补卷任务，规则不许再覆盖掉。
        let mut plan = MasterPlan {
            source: "llm".to_string(),
            intent: None,
            steps: vec![PlanStep {
                agent_id: "architect".to_string(),
                task: "读取 大纲/ 目录下现有的大纲文件（以 大纲/大纲.md 为主），\
                       在卷级大纲里为七个卷逐一补充「每卷有多少章」，并给出全书总章数。\
                       交付物：更新后的 大纲/大纲.md"
                    .to_string(),
                ..Default::default()
            }],
            raw: String::new(),
        };
        let root = std::env::temp_dir().join("storydesk-master-supplement");
        enforce_topic_and_settings(&mut plan, &root, &agents(), request, 1);
        assert!(
            plan.steps[0].task.contains("逐一补充"),
            "{}",
            plan.steps[0].task
        );
        assert!(
            !plan.steps[0].task.contains("重新生成整本故事"),
            "{}",
            plan.steps[0].task
        );
        // 保留模型任务的同时必须钉上执行铁律（不许加写细纲、不许整文件重写）。
        assert!(
            plan.steps[0].task.contains("【执行铁律】"),
            "{}",
            plan.steps[0].task
        );
        // 关键：保留的任务必须被文件锁认成全书级——事故里模型写对了大纲，
        // 锁却按章级细纲锁把 大纲/大纲.md 判越界、恢复回旧版。
        let lock = outline_lock_for(&plan.steps[0], 1);
        assert_eq!(lock, OutlineLock::Book, "{}", plan.steps[0].task);
        assert!(outline_file_allowed("大纲/大纲.md", lock));
        assert!(!outline_file_allowed("大纲/细纲_第001章.md", lock));
    }

    /// 全书级标记优先于章号正则：任务文本里带「第1—30章」也不能降级成章级锁。
    #[test]
    fn book_marker_wins_over_range_in_lock() {
        let task = "【增量补大纲】为七个卷补充章数，如第一卷 第1—30章。";
        assert_eq!(outline_lock_for(&text_step(task), 1), OutlineLock::Book);
        let batch = intent::outline_batch_task(6, 10, None, None);
        assert_eq!(
            outline_lock_for(&text_step(&batch), 6),
            OutlineLock::Chapters(6, 10)
        );
    }

    #[test]
    fn outline_dir_lock_allows_any_outline_file() {
        let lock = outline_lock_for(&text_step("【大纲目录级】生成 大纲/细纲索引.md"), 1);
        assert_eq!(lock, OutlineLock::OutlineDir);
        assert!(outline_file_allowed("大纲/细纲索引.md", lock));
        assert!(outline_file_allowed("大纲/细纲_第007章.md", lock));
        assert!(!outline_file_allowed("正文/第007章_x.md", lock));
    }

    /// 真实事故复现：模型把全书细纲拆成 4 步（索引 + 分段区间），
    /// 修正器把同一条单章规则任务逐步盖章，计划卡变成 4×「重新生成第 1 章细纲」。
    /// 大纲换算不出区间时，必须保留模型的分步，只钉目录级锁标记。
    #[test]
    fn multi_step_chapter_outline_plan_keeps_model_steps() {
        let root = std::env::temp_dir().join("storydesk-master-multistep");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("大纲")).unwrap();
        std::fs::write(
            root.join("大纲").join("大纲.md"),
            "# 隐藏条款\n卷一 入职\n卷二 深入",
        )
        .unwrap();
        let request = "根据大纲，设定，追踪的内容开始写七个卷对应章的每个章节的细纲";
        let mut plan = MasterPlan {
            source: "llm".to_string(),
            intent: None,
            raw: String::new(),
            steps: vec![
                PlanStep { agent_id: "architect".into(), task: "读取 大纲/、设定/、追踪/，生成 大纲/细纲索引.md，按卷列明章号区间与每章核心事件".into(), ..Default::default() },
                PlanStep { agent_id: "architect".into(), task: "依据 大纲/细纲索引.md 生成第 1 至第 5 章的章节级细纲".into(), ..Default::default() },
                PlanStep { agent_id: "architect".into(), task: "生成第 6 至第 10 章的章节级细纲".into(), ..Default::default() },
                PlanStep { agent_id: "architect".into(), task: "生成第 11 至第 15 章的章节级细纲".into(), ..Default::default() },
            ],
        };
        enforce_topic_and_settings(&mut plan, &root, &agents(), request, 1);
        let arch: Vec<&PlanStep> = plan
            .steps
            .iter()
            .filter(|s| ai::AgentRole::parse(&s.agent_id) == Some(ai::AgentRole::Architect))
            .collect();
        assert_eq!(
            arch.len(),
            4,
            "模型分步不许被盖章合并：{:?}",
            arch.iter().map(|s| &s.task).collect::<Vec<_>>()
        );
        for step in &arch {
            assert!(step.task.starts_with("【大纲目录级】"), "{}", step.task);
            let lock = outline_lock_for(step, 1);
            assert!(
                outline_file_allowed("大纲/细纲_第007章.md", lock),
                "目录级锁必须放行细纲"
            );
        }
        // 带章号区间的步骤会被批量拆步器归一成标准批量任务（模型任务换成规则任务），
        // 不带区间的索引步保持目录级锁。
        let (expanded, _, _) = expand_batch_steps(plan.steps, &agents(), None, None, None );
        let index_kept = expanded
            .iter()
            .any(|s| s.task.starts_with("【大纲目录级】"));
        assert!(index_kept, "索引步不该被拆，应原样保留");
        let normalized = expanded
            .iter()
            .filter(|s| !s.task.starts_with("【大纲目录级】"))
            .count();
        assert!(normalized >= 3, "区间步应被拆步器归一，实际 {normalized}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 真实事故：「第1—40(030)章」因括号注记不容忍丢了区间，
    /// 4 步计划被盖章成 4×「重新生成第 40 章细纲」。
    #[test]
    fn parenthetical_range_collapses_steps_to_one_batch() {
        let root = std::env::temp_dir().join("storydesk-master-parenrange");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("大纲")).unwrap();
        std::fs::write(
            root.join("大纲").join("大纲.md"),
            "# 隐藏条款\n卷一：第1—40章，共40章",
        )
        .unwrap();
        let request = "根据大纲，设定，追踪的内容开始写卷一第1—40(030)章章节的细纲";
        let mut plan = MasterPlan {
            source: "llm".to_string(),
            intent: None,
            raw: String::new(),
            steps: vec![
                PlanStep {
                    agent_id: "architect".into(),
                    task: "生成第 1 至第 5 章的章节级细纲".into(),
                    ..Default::default()
                },
                PlanStep {
                    agent_id: "architect".into(),
                    task: "生成第 6 至第 10 章的章节级细纲".into(),
                    ..Default::default()
                },
                PlanStep {
                    agent_id: "architect".into(),
                    task: "生成第 11 至第 15 章的章节级细纲".into(),
                    ..Default::default()
                },
                PlanStep {
                    agent_id: "architect".into(),
                    task: "生成第 16 至第 20 章的章节级细纲".into(),
                    ..Default::default()
                },
            ],
        };
        enforce_topic_and_settings(&mut plan, &root, &agents(), request, 1);
        let arch: Vec<&PlanStep> = plan
            .steps
            .iter()
            .filter(|s| ai::AgentRole::parse(&s.agent_id) == Some(ai::AgentRole::Architect))
            .collect();
        assert_eq!(
            arch.len(),
            1,
            "区间已知时应收编成一步批量：{:?}",
            arch.iter().map(|s| &s.task).collect::<Vec<_>>()
        );
        assert_eq!(
            intent::chapter_range_of(&arch[0].task),
            Some((1, 40)),
            "{}",
            arch[0].task
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 真实事故：「修改每个章节的规定，对白改为25%-35%」模型规划是对的，
    /// 但章级文件锁只放行细纲 001，第 002 章起全被当越界回滚，
    /// 而且模型爱逐个打开细纲整文件重写。
    #[test]
    fn rule_edit_request_gets_dir_lock_and_minimal_edit_law() {
        let root = std::env::temp_dir().join("storydesk-master-ruleedit");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("大纲")).unwrap();
        std::fs::create_dir_all(root.join("设定")).unwrap();
        std::fs::write(
            root.join("大纲").join("大纲.md"),
            "# 隐藏条款\n卷一：第1—40章",
        )
        .unwrap();
        std::fs::write(root.join("设定").join("写作规范.md"), "对白占比：20%。").unwrap();
        let request = "修改一下每个章节的规定，对白修改为25%至35%，生效的设定你来修改";
        let mut plan = MasterPlan {
            source: "llm".to_string(),
            intent: None,
            raw: String::new(),
            steps: vec![
                PlanStep {
                    agent_id: "architect".into(),
                    task: "先读 大纲/ 下现有的卷纲与各章细纲，找出里面写的【每章规定】，把每一章的对白占比规定统一改成 25%-35%，其余结构不变".into(),
                    ..Default::default()
                },
                PlanStep {
                    agent_id: "settings".into(),
                    task: "先读 设定/ 下已有的全部设定文件，把当前生效的对白占比同步改成 25%-35%".into(),
                    ..Default::default()
                },
            ],
        };
        enforce_topic_and_settings(&mut plan, &root, &agents(), request, 1);
        let arch = plan
            .steps
            .iter()
            .find(|s| ai::AgentRole::parse(&s.agent_id) == Some(ai::AgentRole::Architect))
            .expect("architect 步必须还在");
        assert!(arch.task.starts_with("【大纲目录级】"), "{}", arch.task);
        assert!(arch.task.contains("【格式修订铁律】"), "{}", arch.task);
        assert!(
            arch.task.contains("其余结构不变"),
            "模型原任务不许被盖章替换：{}",
            arch.task
        );
        let lock = outline_lock_for(arch, 1);
        assert!(
            outline_file_allowed("大纲/细纲_第002章.md", lock),
            "第 002 章起不许当越界"
        );
        assert!(!outline_file_allowed("正文/第002章_x.md", lock));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 真实事故：【增量补大纲】任务文本带着「第一卷：第1—40章」，
    /// 批量拆步器按章号命中，把一步补写拆成 8 步章级细纲、原任务被替换丢掉。
    #[test]
    fn book_marker_tasks_are_never_batch_expanded() {
        let task =
            "【增量补大纲】读取 大纲/大纲.md，为七个卷补充章数，例如第一卷：第1—40章，共40章。";
        let steps = vec![PlanStep {
            agent_id: "architect".to_string(),
            task: task.to_string(),
            ..Default::default()
        }];
        let (expanded, batch, notes) = expand_batch_steps(steps, &agents(), None, None, None );
        assert_eq!(
            expanded.len(),
            1,
            "全书级任务不许拆步，实际 {} 步",
            expanded.len()
        );
        assert!(
            expanded[0].task.contains("【增量补大纲】"),
            "{}",
            expanded[0].task
        );
        assert!(batch.is_empty(), "{batch:?}");
        assert!(notes.is_empty(), "{notes:?}");
        // 真批量细纲仍要照常拆。
        let batch_task = intent::outline_batch_task(1, 12, None, None);
        let (expanded, _, _) = expand_batch_steps(
            vec![PlanStep {
                agent_id: "architect".to_string(),
                task: batch_task,
                ..Default::default()
            }],
            &agents(),
            None,
            None, None, );
        assert_eq!(expanded.len(), 3, "1-12 章细纲仍应拆成 3 组");
    }

    /// 真实事故：「写卷一第012至040章正文」执行时，tracker 的只读核对步
    /// 被 kind_for 归成 Outline、拆成 6 块「批量生成细纲」，第 1/35 步就突然写细纲。
    /// 写作范围每次不固定：修复不许依赖任何具体章号，只认角色与只读标记。
    #[test]
    fn non_architect_steps_never_become_outline_batches() {
        // 只读 tracker 步 + 写手批量步：只读步原样保留，正文照常逐章拆。
        let steps = vec![
            PlanStep {
                agent_id: "tracker".to_string(),
                task: "【只读模式】连贯核对第 012 章到第 040 章的正文与细纲，只输出报告"
                    .to_string(),
                ..Default::default()
            },
            PlanStep {
                agent_id: "writer".to_string(),
                task: intent::writer_batch_task(12, 40, None, None),
                ..Default::default()
            },
        ];
        let (expanded, _, _) = expand_batch_steps(steps, &agents(), None, None, None );
        assert_eq!(
            expanded.len(),
            30,
            "1 只读 + 29 章正文，实际 {}",
            expanded.len()
        );
        assert!(
            expanded[0].task.contains("【只读模式】连贯核对"),
            "只读步任务被改写: {}",
            expanded[0].task
        );
        assert!(
            !expanded
                .iter()
                .any(|s| s.agent_id == "tracker" && s.task.contains("批量生成")),
            "tracker 仍收到细纲批量任务"
        );
        // 非只读的 tracker 区间步也不许被重写成细纲批量：一步做完。
        let steps2 = vec![PlanStep {
            agent_id: "tracker".to_string(),
            task: "核对第 012 章到第 040 章的伏笔回收，更新 追踪/伏笔.md".to_string(),
            ..Default::default()
        }];
        let (expanded2, batch2, _) = expand_batch_steps(steps2, &agents(), None, None, None );
        assert_eq!(expanded2.len(), 1, "非大纲角色不许被拆成细纲块");
        assert!(
            expanded2[0].task.contains("核对第 012 章"),
            "{}",
            expanded2[0].task
        );
        assert!(batch2.is_empty(), "{batch2:?}");
        // 大纲架构师的细纲批量仍照常拆块。
        let steps3 = vec![PlanStep {
            agent_id: "architect".to_string(),
            task: intent::outline_batch_task(12, 26, None, None),
            ..Default::default()
        }];
        let (expanded3, _, _) = expand_batch_steps(steps3, &agents(), None, None, None );
        assert_eq!(expanded3.len(), 3, "12-26 共 15 章应拆 3 块");
        assert!(expanded3.iter().all(|s| s.agent_id == "architect"));
    }

    /// 明确要沿用时不作废：「按现有细纲写第 4 章正文」不该触发细纲重写。
    #[test]
    fn explicit_reuse_request_does_not_regenerate() {
        let root = std::env::temp_dir().join("storydesk-master-reuse");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("大纲")).unwrap();
        std::fs::create_dir_all(root.join("设定")).unwrap();
        std::fs::write(
            root.join("大纲/细纲_第004章.md"),
            "# 第004章 秋收之前\n古代的农民起事，村里的余粮被抢。字数目标：2400–2600 字。\n",
        )
        .unwrap();
        std::fs::write(
            root.join("设定/世界概览.md"),
            format!("# 世界\n{}\n", "漕运".repeat(150)),
        )
        .unwrap();
        std::fs::write(
            root.join("设定/人物.md"),
            format!("# 人物\n{}\n", "李敢".repeat(150)),
        )
        .unwrap();

        let local = plan_locally(&agents(), "按现有细纲写第4章正文", 4, Some(&root));
        assert_eq!(local.steps.len(), 1, "{:?}", local.steps);
        assert_eq!(local.steps[0].agent_id, "writer");
    }

    #[test]
    fn rules_plan_always_returns_something() {
        // 看不懂的时候不许猜着写：一步、只读、让最近的角色先说清理解
        let local = plan_locally(&agents(), "随便看看", 2, None);
        assert_eq!(local.steps.len(), 1);
        assert!(local.intent.contains("只读不改文件"), "{}", local.intent);
        assert!(local.steps[0].task.starts_with("【只读模式】"));
        assert!(local.intent.contains("把握不大"), "{}", local.intent);
    }

    #[test]
    fn extract_json_ignores_code_fences_and_prose() {
        let raw = "好的，计划如下：\n```json\n{\"plan\":[{\"agent_id\":\"writer\",\"task\":\"写第一章\"}]}\n```\n完成";
        let json = extract_json(raw).expect("应能抠出 JSON");
        assert!(json.starts_with('{') && json.ends_with('}'));
        assert!(json.contains("writer"));
    }

    #[test]
    fn parse_plan_drops_unknown_agents() {
        let raw = r#"{"plan":[{"agent_id":"ghost","task":"x"},{"agent_id":"writer","task":"写"}]}"#;
        let steps = parse_plan(raw, &agents()).expect("至少剩一步");
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].agent_id, "writer");
    }

    #[test]
    fn parse_plan_caps_at_four_steps() {
        let raw = r#"{"plan":[
            {"agent_id":"writer","task":"1"},{"agent_id":"writer","task":"2"},
            {"agent_id":"writer","task":"3"},{"agent_id":"writer","task":"4"},
            {"agent_id":"writer","task":"5"}]}"#;
        assert_eq!(parse_plan(raw, &agents()).unwrap().len(), MAX_STEPS);
    }

    /// 修复回归（真实事故）：「根据参考资料，大纲，设定，追踪开始第一卷的001-045的细纲」
    /// 以前被拆成「整本大纲 + 补设定」、缩水成第 1 章、原话还被当题材注进任务。
    /// 现在必须是：一步 architect 批量生成 001-045 章细纲，无凭空多出的步骤，无原话注入。
    #[test]
    fn range_outline_request_batches_all_chapters() {
        let request = "根据参考资料，大纲，设定，追踪开始第一卷的001-045的细纲";
        let local = plan_locally(&agents(), request, 1, None);
        assert_eq!(local.steps.len(), 1, "{:?}", local.steps);
        assert_eq!(local.steps[0].agent_id, "architect");
        let task = &local.steps[0].task;
        assert!(task.contains("第 001"), "{task}");
        assert!(task.contains("第 045"), "{task}");
        assert!(task.contains("细纲_第001章.md"), "{task}");
        assert!(task.contains("共 45 份"), "{task}");
        assert!(!task.contains("根据参考资料"), "原话不得注入子任务：{task}");
        assert!(
            !local.steps.iter().any(|s| s.agent_id == "settings"),
            "裸名词「设定」不该凭空多出补设定步：{:?}",
            local.steps
        );
        assert!(local.intent.contains("001-045"), "{}", local.intent);
    }

    /// 模型把区间请求降级成「生成第 1 章细纲」时，闸层要改写成批量任务，
    /// 且不再按碎句凭空补一步设定。
    #[test]
    fn llm_single_chapter_plan_for_range_request_is_upgraded() {
        let mut plan = MasterPlan {
            source: "llm".to_string(),
            intent: None,
            steps: vec![PlanStep {
                agent_id: "architect".to_string(),
                task: "生成第 1 章细纲：目标、冲突、转折、结尾钩子（已有细纲就沿用，不要覆盖）"
                    .to_string(),
                ..Default::default()
            }],
            raw: String::new(),
        };
        let request = "根据参考资料，大纲，设定，追踪开始第一卷的001-045的细纲";
        enforce_topic_and_settings(&mut plan, Path::new("不存在"), &agents(), request, 1);
        assert_eq!(plan.steps.len(), 1, "{:?}", plan.steps);
        assert_eq!(plan.steps[0].agent_id, "architect");
        assert!(
            plan.steps[0].task.contains("第 045"),
            "{}",
            plan.steps[0].task
        );
        assert!(
            plan.steps[0].task.contains("一份都不许跳过"),
            "{}",
            plan.steps[0].task
        );
        assert!(
            !plan.steps.iter().any(|s| s.agent_id == "settings"),
            "{:?}",
            plan.steps
        );
    }

    /// 相对章数：书里已有 100 章 → 下一章 101。「接着上次写5章细纲」= 101-105。
    #[test]
    fn relative_count_request_resolves_against_next_chapter() {
        let local = plan_locally(&agents(), "接着上次写5章细纲", 101, None);
        assert_eq!(local.steps.len(), 1, "{:?}", local.steps);
        assert_eq!(local.steps[0].agent_id, "architect");
        let task = &local.steps[0].task;
        assert!(task.contains("第 101 章"), "{task}");
        assert!(task.contains("第 105 章"), "{task}");
        assert!(task.contains("共 5 份"), "{task}");
        assert!(
            local.intent.contains("相对章数：按第 101-105 章"),
            "{}",
            local.intent
        );
    }

    /// 无数字的模糊请求：必须一步只读澄清，不许猜区间、不许默认第 1 章。
    #[test]
    fn vague_range_request_is_clarified_not_executed() {
        let local = plan_locally(&agents(), "帮我写点接下来的细纲，大概几章吧", 7, None);
        assert_eq!(local.steps.len(), 1, "{:?}", local.steps);
        assert!(
            local.steps[0].task.starts_with("【只读模式 · 需要澄清】"),
            "{}",
            local.steps[0].task
        );
        assert!(local.intent.contains("需要澄清"), "{}", local.intent);
        assert!(
            !local.steps.iter().any(|s| s.agent_id == "architect"),
            "不许瞎猜区间开跑：{:?}",
            local.steps
        );
        // 模型自己猜出来的计划同样要被闸层拦下
        let mut plan = MasterPlan {
            source: "llm".to_string(),
            intent: None,
            steps: vec![PlanStep {
                agent_id: "architect".to_string(),
                task: "先生成第 1-3 章细纲".to_string(),
                ..Default::default()
            }],
            raw: String::new(),
        };
        enforce_topic_and_settings(
            &mut plan,
            Path::new("不存在"),
            &agents(),
            "帮我写点接下来的细纲，大概几章吧",
            1,
        );
        assert_eq!(plan.steps.len(), 1);
        assert!(
            plan.steps[0].task.contains("需要澄清"),
            "{}",
            plan.steps[0].task
        );
    }

    /// 「从第15章开始写10章」= 绝对区间：先批量补 15-24 章细纲，再逐章写正文。
    #[test]
    fn start_count_request_batches_prose_chapters() {
        let local = plan_locally(&agents(), "从第15章开始写10章", 1, None);
        let architect = local
            .steps
            .iter()
            .position(|s| s.agent_id == "architect")
            .expect("应先补区间细纲");
        assert!(
            local.steps[architect].task.contains("第 015 章"),
            "{}",
            local.steps[architect].task
        );
        assert!(
            local.steps[architect].task.contains("第 024 章"),
            "{}",
            local.steps[architect].task
        );
        let writer = local.steps.last().expect("应有写正文步骤");
        assert_eq!(writer.agent_id, "writer");
        assert!(writer.task.contains("第 015 章"), "{}", writer.task);
        assert!(writer.task.contains("第 024 章"), "{}", writer.task);
    }

    /// 用户规定的字数范围必须进细纲与写手任务（真实事故：要求每章 6000-8000 字，
    /// 第 3 章只写出 5956 字——任务里根本没有这个数）。
    #[test]
    fn word_count_request_reaches_outline_and_writer_tasks() {
        let request = "完成2-45章的正文，每章纯字数要写6000-8000字";
        let local = plan_locally(&agents(), request, 1, None);
        let outline = local
            .steps
            .iter()
            .find(|s| s.agent_id == "architect")
            .expect("应先补区间细纲");
        assert!(outline.task.contains("第 002 章"), "{}", outline.task);
        assert!(
            outline.task.contains("字数 6000 到 8000 字"),
            "{}",
            outline.task
        );
        let writer = local
            .steps
            .iter()
            .find(|s| s.agent_id == "writer")
            .expect("应有写正文步骤");
        assert!(writer.task.contains("每章纯正文"), "{}", writer.task);
        assert!(writer.task.contains("不少于 6000 字"), "{}", writer.task);
        assert!(writer.task.contains("不超过 8000 字"), "{}", writer.task);
    }

    /// 模型自己写的写手任务不带字数时，闸层要把用户的字数要求补进去。
    #[test]
    fn llm_writer_plan_gets_word_count_appended() {
        let mut plan = MasterPlan {
            source: "llm".to_string(),
            intent: None,
            steps: vec![PlanStep {
                agent_id: "writer".to_string(),
                task: "按第 3 章细纲写正文".to_string(),
                ..Default::default()
            }],
            raw: String::new(),
        };
        let request = "写第3章正文，每章纯字数要写6000-8000字";
        enforce_topic_and_settings(&mut plan, Path::new("不存在"), &agents(), request, 1);
        let writer = plan
            .steps
            .iter()
            .find(|s| s.agent_id == "writer")
            .expect("写手步骤应保留");
        assert!(writer.task.contains("不少于 6000 字"), "{}", writer.task);
    }

    /// 跳章根治：写手批量任务在派发前必须拆成「每章一步」，
    /// 每步都带「本次只写第 N 章」的硬限定与三位数文件名规则。
    #[test]
    fn prose_batch_expands_into_per_chapter_steps() {
        let steps = vec![PlanStep {
            agent_id: "writer".to_string(),
            task: intent::writer_batch_task(3, 6, Some("古代的农民起事"), Some((6000, 8000))),
            ..Default::default()
        }];
        let (expanded, batch, notes) =
            expand_batch_steps(steps, &agents(), Some("古代的农民起事"), Some((6000, 8000)), None );
        assert_eq!(expanded.len(), 4, "3-6 章必须拆成 4 步");
        assert!(batch.contains(&3) && batch.contains(&6));
        for (i, ch) in (3..=6).enumerate() {
            let task = &expanded[i].task;
            assert!(task.contains(&format!("本次只写第 {ch} 章")), "{task}");
            assert!(task.contains(&format!("第{ch:03}章_")), "{task}");
            assert!(task.contains("不少于 6000 字"), "{task}");
            assert!(task.contains("不要代写"), "{task}");
        }
        assert!(notes[0].contains("共 4 步"), "{:?}", notes);
    }

    /// 细纲批量按 5 章分组：一次生成 40+ 份细纲必然烂尾，后面的章没细纲，
    /// 写手步只能被门禁拦下或跳章。
    #[test]
    fn outline_batch_splits_into_groups_of_five() {
        let steps = vec![PlanStep {
            agent_id: "architect".to_string(),
            task: intent::outline_batch_task(1, 12, None, None),
            ..Default::default()
        }];
        let (expanded, batch, _) = expand_batch_steps(steps, &agents(), None, None, None );
        assert_eq!(expanded.len(), 3, "1-12 章应拆成 1-5 / 6-10 / 11-12 三组");
        assert!(
            expanded[0].task.contains("第 001 章到第 005 章"),
            "{}",
            expanded[0].task
        );
        assert!(
            expanded[1].task.contains("第 006 章到第 010 章"),
            "{}",
            expanded[1].task
        );
        assert!(
            expanded[2].task.contains("第 011 章到第 012 章"),
            "{}",
            expanded[2].task
        );
        assert_eq!(batch.len(), 12);
    }

    /// 单章任务不许被拆步逻辑碰到。
    #[test]
    fn single_chapter_steps_are_left_alone() {
        let steps = vec![PlanStep {
            agent_id: "writer".to_string(),
            task: intent::writer_task(3, None, Some((6000, 8000)), None),
            ..Default::default()
        }];
        let (expanded, batch, notes) =
            expand_batch_steps(steps, &agents(), None, Some((6000, 8000)), None );
        assert_eq!(expanded.len(), 1);
        assert!(batch.is_empty());
        assert!(notes.is_empty());
    }

    /// 确认执行路径拿不到用户原话，字数硬要求与题材要从写手任务文本里恢复。
    #[test]
    fn word_count_and_genre_are_recovered_from_writer_task_text() {
        let task = intent::writer_batch_task(3, 45, Some("北山游猎"), Some((6000, 8000)));
        // 成对抽取必须拿回 (6000, 8000)：单值容差会把「不超过 8000」估成 9000。
        assert_eq!(intent::clause_word_count_of(&task), Some((6000, 8000)));
        assert_eq!(
            intent::clause_word_count_of(&task).or_else(|| intent::word_count_of(&task)),
            Some((6000, 8000))
        );
        assert_eq!(genre_from_task(&task), Some("北山游猎".to_string()));
        let placeholder = "（题材：以 设定/题材定位.md 为准）";
        assert_eq!(genre_from_task(placeholder), None);
    }

    /// 错章根治之一：从正文文件名抽章号——章号验收的依据。
    #[test]
    fn resume_start_rejects_out_of_range_from_step() {
    // 越界必须报错：以前直接透传，主循环一次都不进，随后按「全部完成」写盘。
    assert!(resume_start(Some(3), 3, Some(1)).is_err());
    assert!(resume_start(Some(9), 3, Some(0)).is_err());
    // 边界内正常：显式 from_step 优先，其次第一个没完成的步。
    assert_eq!(resume_start(Some(2), 3, Some(0)).unwrap(), 2);
    assert_eq!(resume_start(None, 3, Some(1)).unwrap(), 1);
    // 全部完成 / 状态里没有步骤也要报错，而不是回落到 total。
    assert!(resume_start(None, 3, None).is_err());
    assert!(resume_start(Some(0), 0, None).is_err());
    let message = resume_start(Some(3), 3, Some(1)).unwrap_err().to_string();
    assert!(message.contains("超出范围"), "{message}");
}

#[test]
fn prose_file_chapter_parses_names() {
        assert_eq!(prose_file_chapter("正文/第004章_血路.md"), Some(4));
        assert_eq!(prose_file_chapter("正文\\第12章_马惊夜.md"), Some(12));
        assert_eq!(prose_file_chapter("正文/第7章.md"), Some(7));
        // 没有章号的文件抽不出，也不该误抽（大纲文件不算正文章）
        assert_eq!(prose_file_chapter("正文/随笔.md"), None);
        assert_eq!(prose_file_chapter("大纲/细纲_第003章.md"), Some(3));
    }

    /// 错章根治之二：越界/错章文件移入回收站，原位置不再有它。
    #[test]
    fn quarantine_moves_files_into_recycle_bin() {
        let root =
            std::env::temp_dir().join(format!("storydesk-quarantine-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let prose = root.join("正文");
        std::fs::create_dir_all(&prose).unwrap();
        std::fs::write(prose.join("第004章_血路.md"), "越界内容").unwrap();
        let moved = quarantine_files(
            &root,
            &[
                "正文/第004章_血路.md".to_string(),
                "正文/不存在的.md".to_string(),
            ],
        );
        assert_eq!(moved, vec!["正文/第004章_血路.md".to_string()]);
        assert!(!prose.join("第004章_血路.md").exists(), "原位置应收走");
        assert_eq!(
            std::fs::read_to_string(root.join(".越界回收站/正文/第004章_血路.md")).unwrap(),
            "越界内容",
            "回收站里要能找回原文"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 错章根治之三：模型覆盖了作者已有的旧章，必须恢复原文，不能只收进回收站。
    #[test]
    fn overwritten_existing_chapter_is_restored_not_quarantined() {
        let root =
            std::env::temp_dir().join(format!("storydesk-restore-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let prose = root.join("正文");
        std::fs::create_dir_all(&prose).unwrap();
        let old = "# 第001章 霜降裁驿\n\n这是作者原来的第一章。\n";
        std::fs::write(prose.join("第001章_霜降裁驿.md"), old).unwrap();

        let before = snapshot_book(&root);
        // 写手被派去写第 2 章，却把第 1 章覆盖成了新内容
        std::fs::write(prose.join("第001章_霜降裁驿.md"), "被覆盖的新内容").unwrap();
        let after = snapshot_book(&root);
        assert!(after.mtimes.contains_key("正文/第001章_霜降裁驿.md"));

        let restored = restore_files(
            &root,
            &["正文/第001章_霜降裁驿.md".to_string()],
            &before.contents,
        );
        assert_eq!(restored, vec!["正文/第001章_霜降裁驿.md".to_string()]);
        assert_eq!(
            std::fs::read_to_string(prose.join("第001章_霜降裁驿.md")).unwrap(),
            old,
            "作者旧稿必须原样恢复"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 错章根治之四：被误删的章节也要按快照恢复。
    #[test]
    fn deleted_chapter_is_restored_from_snapshot() {
        let root =
            std::env::temp_dir().join(format!("storydesk-undelete-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let prose = root.join("正文");
        std::fs::create_dir_all(&prose).unwrap();
        std::fs::write(prose.join("第001章_霜降裁驿.md"), "旧稿").unwrap();
        let before = snapshot_book(&root);
        std::fs::remove_file(prose.join("第001章_霜降裁驿.md")).unwrap();

        let restored = restore_files(
            &root,
            &["正文/第001章_霜降裁驿.md".to_string()],
            &before.contents,
        );
        assert_eq!(restored.len(), 1);
        assert_eq!(
            std::fs::read_to_string(prose.join("第001章_霜降裁驿.md")).unwrap(),
            "旧稿"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 细纲章号锁：单章只认本章，批量只认区间，全书大纲只认 大纲/大纲.md。
    #[test]
    fn outline_lock_rejects_other_chapters_and_book_outline() {
        let single = outline_lock_for(
            &text_step(&intent::outline_task(3, None, true, "测试", None)),
            3,
        );
        assert_eq!(single, OutlineLock::Chapters(3, 3));
        assert!(outline_file_allowed("大纲/细纲_第003章.md", single));
        assert!(!outline_file_allowed("大纲/细纲_第005章.md", single));
        assert!(!outline_file_allowed("大纲/大纲.md", single));

        let batch = outline_lock_for(
            &text_step(&intent::outline_batch_task(6, 10, None, None)),
            6,
        );
        assert_eq!(batch, OutlineLock::Chapters(6, 10));
        assert!(outline_file_allowed("大纲/细纲_第007章.md", batch));
        assert!(!outline_file_allowed("大纲/细纲_第011章.md", batch));
        assert!(!outline_file_allowed("大纲/大纲.md", batch));

        let book = outline_lock_for(&text_step(&intent::book_outline_task(None, true)), 1);
        assert_eq!(book, OutlineLock::Book);
        assert!(outline_file_allowed("大纲/大纲.md", book));
        assert!(!outline_file_allowed("大纲/细纲_第001章.md", book));
    }

    /// 用户点名 9000-9500 字时，派发阶段不能再把细纲里的 8000-9500 / 单场 2200-2600
    /// 追加到写手 prompt 里——两处字数打架时模型永远选低的。
    #[test]
    fn prose_constraints_drop_outline_word_count_when_user_gave_one() {
        let root =
            std::env::temp_dir().join(format!("storydesk-constraints-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("大纲")).unwrap();
        std::fs::write(
            root.join("大纲/细纲_第002章.md"),
            "# 第002章\n\n### 场景一\n- 字数建议：2200—2600字。\n- 对白比例：约35%。\n\n\
             ### 场景二\n- 字数建议：3400—4000字。\n\n\
             - **正文目标字数**：8000—9500字（不计标点与空白）。\n",
        )
        .unwrap();

        let user = prose_constraints(&root, 2, Some((9000, 9500))).expect("有比例约束");
        assert!(user.contains("对白"), "{user}");
        assert!(
            !user.contains("8000"),
            "用户字数优先，细纲总字数必须让位：{user}"
        );
        assert!(!user.contains("2200"), "单场建议更不能进 prompt：{user}");

        let outline = prose_constraints(&root, 2, None).expect("有细纲约束");
        assert!(
            outline.contains("8000-9500"),
            "没给用户字数时才用细纲总字数：{outline}"
        );
        assert!(
            !outline.contains("2200"),
            "任何时候都不该拿单场建议当全章目标：{outline}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 章号锁定写进写手任务模板（计划卡可见；编排层还会再兜底注入一次）。
    #[test]
    fn writer_task_carries_chapter_lock() {
        let task = intent::writer_task(1, None, Some((9000, 9500)), None);
        assert!(task.contains("章号锁定"), "{task}");
        assert!(task.contains("第001章_"), "{task}");
        assert!(task.contains("不要修改、续写或重写"), "{task}");
        // 设定任务也带目录锁定（实测设定管理员越界写正文的事故）
        for t in [
            intent::settings_fill_task(None),
            intent::settings_bootstrap_task(None),
        ] {
            assert!(t.contains("目录锁定"), "{t}");
            assert!(t.contains("绝不要写正文"), "{t}");
        }
    }

    /// 真实事故（《隐藏条款》331—350，2026-11-17 09:12 那条请求）：用户要「第 331 至 350 章的
    /// 细纲作为整本书的结尾」，现场却是「第 335 章是全书最后一章」、细纲只排到 331—335，
    /// 还凭空派了 20 个正文步。根因链：classify 被「正文目标字数 / 不写正文」带偏成 Prose →
    /// 计划落进写正文闸被改写 → 模型自带的 331—335 区间字段留在 arch 步上 →
    /// 收尾从句与拆步都按这个字段取终点。
    #[test]
    fn outline_ending_request_pins_ending_to_declared_last_chapter() {
        let request = "【本次只做一件事】章级细纲\n\
【产出】大纲/细纲_第331章.md 至 大纲/细纲_第350章.md（共 20 章，每章一个细纲文件）\n\
【范围】第 331—350 章（共 20 章；一步只写一章）\n\
【字数】6000-6500 字（写进各章细纲的「正文目标字数」）\n\
【收尾】按用户要求，这 20 章作为整本书的结尾处理，细纲需覆盖全书收束安排\n\
【依据】先读大纲、设定、追踪相关文件，再动笔\n\
【不要做】不写正文；不改动大纲、设定、追踪文件；不超出第 331—350 章范围\n\
【原话】根据大纲，设定，追踪的内容开始写第331至350章的细纲作为整本书的结尾，\
强调字数要求:6000-6500字";
        let root = std::env::temp_dir().join("storydesk-ending-331-350");
        let _ = std::fs::remove_dir_all(&root);

        // 模型自己把 20 章切成 4 块时，第一块 arch 步会带着 331—335 的区间字段。
        let mut plan = MasterPlan {
            source: "llm".into(),
            intent: None,
            raw: String::new(),
            steps: vec![
                PlanStep {
                    range: Some(ChapterRange::Span(331, 335)),
                    ..step("architect", "批量生成第 331 到 335 章的章级细纲")
                },
                PlanStep {
                    range: Some(ChapterRange::Span(331, 350)),
                    ..step(
                        "writer",
                        &intent::writer_batch_task(331, 350, None, Some((6000, 6500))),
                    )
                },
            ],
        };
        enforce_topic_and_settings(&mut plan, &root, &agents(), request, 331);
        attach_sources_clause(&mut plan, &agents(), request);
        let normalized: Vec<PlanStep> = std::mem::take(&mut plan.steps)
            .into_iter()
            .map(|s| normalize_step(s, &agents()))
            .collect();
        let (expanded, _, notes) = expand_batch_steps(normalized, &agents(), None, None, None);

        let arch: Vec<&PlanStep> = expanded
            .iter()
            .filter(|s| ai::AgentRole::parse(&s.agent_id) == Some(ai::AgentRole::Architect))
            .collect();
        let ranges: Vec<Option<ChapterRange>> = arch.iter().map(|s| s.range).collect();
        assert_eq!(
            ranges,
            vec![
                Some(ChapterRange::Span(331, 335)),
                Some(ChapterRange::Span(336, 340)),
                Some(ChapterRange::Span(341, 345)),
                Some(ChapterRange::Span(346, 350)),
            ],
            "20 章细纲必须按 5 章一块拆满 331—350：{notes:?}"
        );

        // 收尾只落在最后一块，章号取用户点名的 350，不是模型顺手留下的 335。
        let ending: Vec<&&PlanStep> = arch
            .iter()
            .filter(|s| s.task.contains(intent::ENDING_MARK))
            .collect();
        assert_eq!(ending.len(), 1, "收尾从句只许出现一次：{:?}", ending);
        let last = arch[3];
        assert!(
            last.task.contains("第 350 章是全书最后一章"),
            "{}",
            last.task
        );
        assert!(last.task.contains("大纲/细纲_第350章.md"), "{}", last.task);
        // 「作者指定结合素材」只许出现一次：事故现场里它被收尾从句卷成了两段。
        assert_eq!(
            last.task.matches("【作者指定结合素材】").count(),
            1,
            "{}",
            last.task
        );

        // 用户明写「不写正文」：计划里不许再有写手步（现场白派了 20 个逐章正文步）。
        assert!(
            !expanded
                .iter()
                .any(|s| ai::AgentRole::parse(&s.agent_id) == Some(ai::AgentRole::Writer)),
            "不许凭空派生正文步：{:?}",
            expanded.iter().map(|s| &s.task).collect::<Vec<_>>()
        );
    }

    /// 同一个请求，模型把 arch 步写成单章（区间 331—350 被降级成 331—335 的原文）：
    /// 大纲闸必须按用户声明的区间重写成批量任务，并作废模型留下的旧区间字段。
    #[test]
    fn llm_chunked_outline_step_is_rewritten_to_declared_range() {
        let request = "写第331至350章的细纲作为整本书的结尾，不写正文，6000-6500字";
        let root = std::env::temp_dir().join("storydesk-ending-rewrite");
        let _ = std::fs::remove_dir_all(&root);
        let mut plan = MasterPlan {
            source: "llm".into(),
            intent: None,
            raw: String::new(),
            steps: vec![PlanStep {
                range: Some(ChapterRange::Span(331, 335)),
                lock: Some(OutlineLock::Chapters(331, 335)),
                ..step("architect", "批量生成第 331 到 335 章的章级细纲")
            }],
        };
        enforce_topic_and_settings(&mut plan, &root, &agents(), request, 331);
        let arch = plan
            .steps
            .iter()
            .find(|s| ai::AgentRole::parse(&s.agent_id) == Some(ai::AgentRole::Architect))
            .expect("大纲步不许被删掉");
        let normalized = normalize_step(arch.clone(), &agents());
        assert!(
            normalized.task.contains("第 331 章到第 350 章"),
            "闸层必须按请求区间重写：{}",
            normalized.task
        );
        assert_eq!(
            normalized.range,
            Some(ChapterRange::Span(331, 350)),
            "旧区间字段必须让位给任务文本：{}",
            normalized.task
        );
        assert_eq!(
            normalized.lock,
            Some(OutlineLock::Chapters(331, 350)),
            "旧文件锁必须按新区间重算：{}",
            normalized.task
        );
    }

    /// 真实事故（同一本书，2026-11-17 10:04）：计划只剩 **1 步**，派发后整批 20 章细纲
    /// 被当成一步塞给大纲架构师。根因：这本书没有 `大纲/事件/`，`event_bootstrap_step`
    /// 先排了一步「排事件卡」，闸层随后把它的任务换成批量细纲却只清了区间与锁，
    /// 空 id 的卡步标记留在 `event` 上 → 拆步第一道守卫把它当卡步放过。
    #[test]
    /// 结构化 `purpose` 优先于任务文本：文本被改写、或反过来文本里带着卡步字眼，
    /// 都不该改变「这一步到底是什么」。
    fn step_purpose_beats_task_text() {
        let root = std::env::temp_dir().join("storydesk-purpose-wins");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("大纲")).unwrap();
        std::fs::create_dir_all(root.join("设定")).unwrap();

        // ① 真事件卡步：任务文本里塞进「批量生成第 331 章到第 335 章的章级细纲」也不拆。
        let card_task = intent::outline_batch_task(331, 335, None, None);
        let card = PlanStep {
            purpose: Some(crate::models::dto::StepPurpose::EventCard),
            ..step("architect", &card_task)
        };
        let (out, _, _) =
            expand_batch_steps(vec![card], &agents(), None, None, Some(&root));
        assert_eq!(out.len(), 1, "事件卡步不许拆：{out:?}");

        // ② 真批量细纲步：即使残留了卡步标记（event 空 id），也必须按区间拆开。
        let batch_task = intent::outline_batch_task(331, 350, None, Some((6000, 6500)));
        let batch = PlanStep {
            purpose: Some(crate::models::dto::StepPurpose::OutlineBatch),
            event: Some(EventRef::card_step()),
            ..step("architect", &batch_task)
        };
        let (out, _, _) =
            expand_batch_steps(vec![batch], &agents(), None, Some((6000, 6500)), Some(&root));
        assert_eq!(out.len(), 4, "批量细纲要拆 4 块：{out:?}");
        let last = out.last().unwrap();
        assert_eq!(last.range, Some(ChapterRange::Span(346, 350)));
    }

    #[test]
    fn event_card_step_with_rewritten_task_is_split() {
        let root = std::env::temp_dir().join("storydesk-card-step-rewrite");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("大纲")).unwrap();
        std::fs::create_dir_all(root.join("设定")).unwrap();

        let task = intent::outline_batch_task(331, 350, None, Some((6000, 6500)));
        let mut card = PlanStep {
            event: Some(EventRef::card_step()),
            lock: Some(OutlineLock::OutlineDir),
            ..step("architect", &task)
        };
        // 闸层换任务时连 event 一起清掉；这里故意保留标记，验证拆步自己也不上当。
        let (out, _, _) = expand_batch_steps(
            vec![card.clone()],
            &agents(),
            None,
            Some((6000, 6500)),
            Some(&root),
        );
        assert_eq!(out.len(), 4, "20 章细纲必须拆成 4 块：{out:?}");

        // 闸层真正换任务时：标记被清掉、子步不再带卡步标记。
        card.task = task.clone();
        card.event = None;
        card.lock = None;
        let (out, _, _) = expand_batch_steps(
            vec![card],
            &agents(),
            None,
            Some((6000, 6500)),
            Some(&root),
        );
        assert_eq!(out.len(), 4, "{out:?}");
        assert!(
            out.iter().all(|s| s.event.as_ref().map_or(true, |e| !e.is_card_step())),
            "子步不许继承卡步标记：{out:?}"
        );
        // 末块收在 346—350，区间来自任务文本而不是那个过期的目录锁。
        let last = out.last().unwrap();
        assert_eq!(last.range, Some(ChapterRange::Span(346, 350)), "{:?}", last.range);
        assert_eq!(
            last.lock,
            Some(OutlineLock::Chapters(346, 350)),
            "目录锁必须让位给章号锁：{:?}",
            last.lock
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 端到端：真实事故那次请求（这本书没有 `大纲/事件/`，会先排一步建事件卡）
    /// 必须得到 4 个细纲块、收尾钉在 350、且不派生正文步。
    #[test]
    fn ending_outline_request_without_event_cards_splits_into_chunks() {
        let request = "【本次只做一件事】章级细纲\n\
【产出】大纲/细纲_第331章.md 至 大纲/细纲_第350章.md（共 20 章，每章一个细纲文件）\n\
【范围】第 331—350 章（共 20 章；一步只写一章）\n\
【字数】6000-6500 字（写进各章细纲的「正文目标字数」）\n\
【收尾】按用户要求，这 20 章作为整本书的结尾处理，细纲需覆盖全书收束安排\n\
【不要做】不写正文；不改动大纲、设定、追踪文件；不超出第 331—350 章范围";
        let root = std::env::temp_dir().join("storydesk-ending-no-event-cards");
        let _ = std::fs::remove_dir_all(&root);
        for dir in ["大纲", "设定", "追踪"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }

        // 模型给的计划：一步建事件卡 + 一步批量细纲（都没有区间字段，靠任务文本）。
        let mut plan = MasterPlan {
            source: "llm".into(),
            intent: None,
            raw: String::new(),
            steps: vec![
                PlanStep {
                    event: Some(EventRef::card_step()),
                    lock: Some(OutlineLock::OutlineDir),
                    ..step("architect", &intent::event_task("都市悬疑", None, Some((331, 350))))
                },
                step("architect", "批量生成第 331 到 350 章的章级细纲"),
            ],
        };
        enforce_topic_and_settings(&mut plan, &root, &agents(), request, 331);
        attach_sources_clause(&mut plan, &agents(), request);
        assert!(
            !plan.steps.iter().any(|s| s.event.as_ref().is_some_and(|e| e.is_card_step())),
            "换过任务文本的步不许留着卡步标记：{:?}",
            plan.steps
        );
        let normalized: Vec<PlanStep> = std::mem::take(&mut plan.steps)
            .into_iter()
            .map(|s| normalize_step(s, &agents()))
            .collect();
        let (expanded, _, notes) =
            expand_batch_steps(normalized, &agents(), None, Some((6000, 6500)), Some(&root));

        let ranges: Vec<Option<ChapterRange>> = expanded
            .iter()
            .map(|s| s.range)
            .collect();
        assert_eq!(
            ranges,
            vec![
                Some(ChapterRange::Span(331, 335)),
                Some(ChapterRange::Span(336, 340)),
                Some(ChapterRange::Span(341, 345)),
                Some(ChapterRange::Span(346, 350)),
            ],
            "20 章必须拆成 4 块：{notes:?}"
        );
        let last = expanded.last().unwrap();
        assert!(last.task.contains("第 350 章是全书最后一章"), "{}", last.task);
        // 素材段只许一份，且必须在收尾从句之前（事故现场它被卷进从句、出现两遍）。
        assert_eq!(
            last.task.matches("【作者指定结合素材】").count(),
            1,
            "{}",
            last.task
        );
        let sources = last.task.find("【作者指定结合素材】").unwrap();
        let ending = last.task.find(intent::ENDING_MARK).unwrap();
        assert!(sources < ending, "素材段必须在收尾从句之前：{}", last.task);
        assert!(
            last.task.trim_end().ends_with("这一个文件。"),
            "收尾从句必须收在任务末尾：{}",
            last.task
        );
        assert!(
            !expanded
                .iter()
                .any(|s| ai::AgentRole::parse(&s.agent_id) == Some(ai::AgentRole::Writer)),
            "不写正文的请求不许派生写手步"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 写正文的收尾请求（对照组）：收尾从句钉在最后一个逐章子步上，前面的子步一个都不许带。
    #[test]
    fn prose_ending_request_pins_ending_to_last_chapter_step() {
        let request = "续写第331章至第350章正文，作为整本书的结尾，在第350章收束并写（全书完）";
        let root = std::env::temp_dir().join("storydesk-ending-prose");
        let _ = std::fs::remove_dir_all(&root);
        let mut plan = MasterPlan {
            source: "llm".into(),
            intent: None,
            raw: String::new(),
            steps: vec![PlanStep {
                range: Some(ChapterRange::Span(331, 350)),
                ..step(
                    "writer",
                    &intent::writer_batch_task(331, 350, None, Some((6000, 6500))),
                )
            }],
        };
        enforce_topic_and_settings(&mut plan, &root, &agents(), request, 331);
        attach_sources_clause(&mut plan, &agents(), request);
        let normalized: Vec<PlanStep> = std::mem::take(&mut plan.steps)
            .into_iter()
            .map(|s| normalize_step(s, &agents()))
            .collect();
        let (expanded, _, notes) = expand_batch_steps(normalized, &agents(), None, None, None);
        let prose: Vec<&PlanStep> = expanded
            .iter()
            .filter(|s| ai::AgentRole::parse(&s.agent_id) == Some(ai::AgentRole::Writer))
            .collect();
        assert_eq!(prose.len(), 20, "331—350 必须逐章派发：{notes:?}");
        for step in &prose[..19] {
            assert!(
                !step.task.contains(intent::ENDING_MARK),
                "只有最后一章带收尾：{}",
                step.task
            );
        }
        let last = prose[19];
        assert_eq!(last.range, Some(ChapterRange::Single(350)));
        assert!(
            last.task.contains("第 350 章是全书最后一章"),
            "{}",
            last.task
        );
    }
}
