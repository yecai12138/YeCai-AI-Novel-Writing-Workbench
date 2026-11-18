use serde::{Deserialize, Serialize};

use crate::models::ai::AgentRole;
use crate::models::event::EventRef;

/// 单个外部工具的探测结果。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolStatus {
    /// node / python / bash / engine
    pub tool: String,
    /// 是否满足应用要求（版本达标、可用）
    pub ok: bool,
    /// 解析出的版本号（拿不到时为 None）
    pub version: Option<String>,
    /// 解析出的可执行文件绝对路径（拿到时才填）
    pub path: Option<String>,
    /// 缺失或不达标时的安装指引（原样展示给用户）
    pub hint: Option<String>,
    /// 探测细节：实际执行的命令、原始输出或失败原因
    pub detail: Option<String>,
}

impl ToolStatus {
    pub fn missing(tool: &str, hint: &str, detail: &str) -> Self {
        Self {
            tool: tool.to_string(),
            ok: false,
            version: None,
            path: None,
            hint: Some(hint.to_string()),
            detail: Some(detail.to_string()),
        }
    }
}

/// 一次环境自检的完整结果。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvReport {
    pub node: ToolStatus,
    pub python: ToolStatus,
    pub bash: ToolStatus,
    pub engine: ToolStatus,
    /// node + python + bash 全部达标（engine 单独标注，因为用户可能还没装宿主 CLI）
    pub all_ok: bool,
    /// 是否找到可用宿主 Agent CLI（claude / codex / opencode 任一）
    pub engine_ok: bool,
    pub platform: String,
    pub checked_at: String,
}

/// 目录树节点。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TreeNode {
    pub name: String,
    /// 绝对路径（前端不直接用它做文件操作，只用于展示与 IPC 回传）
    pub path: String,
    pub kind: TreeNodeKind,
    /// 相对书根的路径，形如 `正文/第001章_开篇.md`
    pub rel: String,
    pub children: Vec<TreeNode>,
    /// 文件字节数；目录为 None
    pub size: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TreeNodeKind {
    Dir,
    File,
}

/// 项目信息（新建 / 打开通用）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectInfo {
    /// 工作区根：`.active-book` 与 `.storydesk/` 所在目录
    pub workspace_dir: String,
    /// 书根目录绝对路径
    pub book_root: String,
    pub book_name: String,
    /// `.active-book` 的内容（相对工作区的路径，如 `长篇/我的小说`）
    pub active_book: String,
    /// true 表示本次新建，false 表示打开已有项目
    pub created: bool,
}

/// 目录选择器里的一个子目录。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirEntryInfo {
    pub name: String,
    pub path: String,
    /// 是否为空目录（新建项目时会提示“建议选择空文件夹”）
    pub is_empty: bool,
}

/// `list_dirs` 的返回：当前目录 / 上级目录 / 子目录列表。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirListing {
    pub current: String,
    pub parent: Option<String>,
    pub entries: Vec<DirEntryInfo>,
}

/// 一次 `read_file` 的结果。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileContent {
    /// 绝对路径
    pub path: String,
    /// 相对工作区根的路径，统一用 `/` 分隔
    pub rel: String,
    pub content: String,
    /// 磁盘上的字节数
    pub bytes: u64,
    /// 磁盘最后修改时间（Unix 毫秒）；拿不到时为 None
    pub mtime_ms: Option<u64>,
    /// 内容的 sha1（用于前端判断“磁盘版本是否真的不同”）
    pub sha1: String,
}

/// 一份历史快照。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotInfo {
    /// 快照文件名，形如 `2026-11-05T09-52-13.123Z.md`
    pub id: String,
    pub path: String,
    /// 对应文件的相对路径
    pub rel: String,
    /// 时间戳（快照文件名去掉 .md）；冒号已换成短横以兼容 Windows
    pub created_at: String,
    pub bytes: u64,
}

/// 一次 `write_file` / `restore_snapshot` 的结果。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveOutcome {
    pub file: FileContent,
    /// 覆盖前留的底；新文件为 None
    pub snapshot: Option<SnapshotInfo>,
    pub saved_at: String,
}

/// 文件监听事件 `fs://changed` 的载荷。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FsChangedPayload {
    /// 被监听的根目录
    pub root: String,
    /// 这一批变更涉及的绝对路径（已去重、已过滤）
    pub paths: Vec<String>,
    /// create / modify / remove / other / mixed
    pub kind: String,
}

// ---------------------------------------------------------------- 阶段 4：AI 引擎

/// 一次 AI 运行属于哪条流水线。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum RunKind {
    /// 生成细纲（story-architect 的 outline 任务）
    Outline,
    /// 写本章正文（narrative-writer，写盘前会被 hook 拦）
    Prose,
    /// 去 AI 味 / 润色（story-deslop）
    Deslop,
}

impl RunKind {
    /// 写正文与去味必须先过前置门禁（细纲存在性等）。
    pub fn needs_gate(self) -> bool {
        matches!(self, RunKind::Prose | RunKind::Deslop)
    }
}

/// 用哪个引擎跑。
///
/// 两种：宿主 `claude` CLI 与宿主 `opencode` CLI（自定义 OpenAI 协议 API 的唯一可行路径）。
/// 旧配置里写成 `"mock"` 的直接按 `claude` 处理，不因为读到一个过时的字段
/// 就把整个设置判死（反序列化见下方手写实现）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum EngineKind {
    /// 真实宿主 CLI：`claude --output-format stream-json -p <prompt>`
    Claude,
    /// 宿主 CLI：`opencode run --format json --auto <prompt>`
    /// 经 `opencode.json` 把「设置里的自定义提供商」映射给引擎（openai-compatible / anthropic）。
    Opencode,
}

impl<'de> serde::Deserialize<'de> for EngineKind {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        match raw.as_str() {
            "opencode" => Ok(EngineKind::Opencode),
            "claude" => Ok(EngineKind::Claude),
            _ => {
                eprintln!("[storydesk] 引擎配置写的是「{raw}」，不认识，按 claude 处理");
                Ok(EngineKind::Claude)
            }
        }
    }
}

impl EngineKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EngineKind::Claude => "claude",
            EngineKind::Opencode => "opencode",
        }
    }
}

/// 一次 `ai_start` 的入参。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunRequest {
    pub workspace_root: String,
    pub book_root: String,
    pub kind: RunKind,
    #[serde(default)]
    pub engine: Option<EngineKind>,
    /// 目标章号；生成细纲与写正文都要
    #[serde(default)]
    pub chapter: Option<u32>,
    /// 直接注入的 prompt 尾部（例如「润色第 3 段」）。为空时用任务包里的默认流程
    #[serde(default)]
    pub note: Option<String>,
    /// 由哪个智能体执行。有值时会在四行契约**之前**注入一段 system prompt。
    #[serde(default)]
    pub agent_role: Option<crate::models::ai::AgentRole>,
    /// 智能体聚焦的目录（相对书根，如 `设定`）。不传则用该角色的默认目录。
    #[serde(default)]
    pub target_dir: Option<String>,
    /// 前端抓到的上下文文件名（相对书根）；为空时后端自己扫目录兜底。
    #[serde(default)]
    pub context_files: Vec<String>,
}

/// 流式片段：`ai://chunk` 的载荷。三种 type 归一化后的形态。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiChunk {
    pub run_id: String,
    /// 递增序号，前端按它排序去重
    pub seq: u64,
    /// text | tool_use | status
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// tool_use 的工具名，如 `Read`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    /// tool_use 的目标（文件路径等），用于「📖 读取文件：xxx」
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
}

/// `ai://done` 的载荷。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunDone {
    pub run_id: String,
    pub exit: i32,
    /// done | blocked | error | cancelled
    pub status: String,
    pub duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Hook 阻断的类型（`hook://blocked` 载荷内嵌）。
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum HookBlockKind {
    /// 「第 N 章缺少细纲（大纲/细纲_第NNN章.md）」
    MissingOutline { chapter: u32 },
    /// 「的细纲（…）是空的」
    OutlineTooShort { outline: String },
    /// 「上一章有未清毒句式欠账」
    ToxicDebtPrev,
    /// 「细纲声称属于事件 NNN，但 大纲/事件/ 下没有这张卡」——旧版会拦，现在只提示
    EventMissing { chapter: u32, event: String },
    /// 「所属事件已结算，不能再往里加章」
    EventSettled { chapter: u32, event: String },
    /// 「事件卡的章型分工里没有这一章」
    EventUnplanned { chapter: u32, event: String },
    /// 命中 exit 2 但不是已知文案
    Unknown,
}

/// `hook://blocked` 的载荷：UI 直接渲染成阻断卡片。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HookBlock {
    pub run_id: String,
    pub block: HookBlockKind,
    /// 一句话原因（卡片标题）
    pub reason: String,
    /// stderr 原文，原样展示
    pub stderr: String,
    /// 建议动作 id：generate_outline / open_prev_chapter / none
    pub action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chapter: Option<u32>,
}

/// 一次门禁脚本调用的结果。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GateOutput {
    pub ok: bool,
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// 门禁结论分级：不是所有「不 ok」都该停下整轮编排。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GateVerdict {
    /// 通过（或只是提示）
    Pass,
    /// 可修复：缺细纲 / 细纲太薄 / 事件卡与细纲对不上——插一步补上就能继续，不必中断
    Repairable,
    /// 必须停下：正文越界、错章、脚本本身跑不起来
    Stop,
}

/// 可修复门禁的修复动作（带上要修哪一章）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "action", content = "chapter")]
pub enum GateRemedy {
    /// 不需要修（Pass）
    None,
    /// 补这一章的章级细纲（插一个细纲架构师步）
    WriteOutline(u32),
    /// 事件卡与细纲声明对不上（插一步改事件卡，再重试正文）
    FixEventCard(u32),
}

/// oh-story 脚本链的探测结果（设置页展示）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GateInfo {
    pub ok: bool,
    pub python: Option<String>,
    pub storyctl: Option<String>,
    pub brief: Option<String>,
    pub detail: String,
}

/// 任务包产出。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskPacket {
    pub path: String,
    pub task: String,
    /// 脚本原始输出（排错用）
    pub raw: String,
}

/// 引擎可用性（前端引擎下拉框）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineInfo {
    pub kind: EngineKind,
    pub name: String,
    pub available: bool,
    pub detail: String,
}

// ---------------------------------------------------------------- 阶段 5：后置体检

/// 一条体检问题。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PostCheckIssue {
    /// wordcount | ai_pattern | structure | script | missing
    pub code: String,
    /// 一句话说明（原样展示给作者）
    pub text: String,
    /// 这条问题针对哪个事件（`001`）。一章挂着多张卡时，靠它分清是当前卡、第二张卡还是上游遗留；
    /// 与事件无关的问题（字数、编码、脚本）为 None。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event: Option<String>,
}

/// 写完正文之后的自动体检结果（`ai://postcheck` 的载荷）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PostCheck {
    pub run_id: String,
    pub chapter: u32,
    /// 是否通过（skipped 时恒为 true）
    pub ok: bool,
    /// 没做体检（缺脚本、没找到正文）时为 true
    pub skipped: bool,
    /// 卡片标题：通过 / 未通过的一句话结论
    pub reason: String,
    pub issues: Vec<PostCheckIssue>,
    /// 正文（中文字符）字数，拿不到时为 None
    pub chars: Option<usize>,
    /// 体检的正文文件（相对书根）
    pub file: Option<String>,
    /// storyctl 原话（可为空）
    pub stdout: String,
    pub stderr: String,
    pub checked_at: String,
}

impl PostCheck {
    pub fn skipped(run_id: &str, chapter: u32, reason: &str) -> Self {
        Self {
            run_id: run_id.to_string(),
            chapter,
            ok: true,
            skipped: true,
            reason: reason.to_string(),
            issues: Vec::new(),
            chars: None,
            file: None,
            stdout: String::new(),
            stderr: String::new(),
            checked_at: crate::services::snapshot::iso_now(),
        }
    }
}

// ---------------------------------------------------------------- 阶段 7.5：主从编排

/// 这一步覆盖的章节范围。
///
/// 与 `intent::chapter_range_of(task)` 解出来的区间是同一件事，只是不再需要
/// 把「第 5 至第 8 章」这种说法再从中文里抠一遍。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "camelCase")]
pub enum ChapterRange {
    /// 全书级（大纲/大纲.md）
    Book,
    /// 单章
    Single(u32),
    /// 闭区间 `from..=to`
    Span(u32, u32),
}

impl ChapterRange {
    pub fn bounds(self) -> (u32, u32) {
        match self {
            Self::Book => (0, 0),
            Self::Single(c) => (c, c),
            Self::Span(from, to) => (from, to),
        }
    }

    /// 这一章是否落在范围内（`Book` 不认任何单章）。
    pub fn covers(self, chapter: u32) -> bool {
        match self {
            Self::Book => false,
            Self::Single(c) => c == chapter,
            Self::Span(from, to) => chapter >= from && chapter <= to,
        }
    }
}

/// 细纲步骤允许写哪些文件（原 `commands::master::OutlineLock`，R4 提到 dto 供 PlanStep 携带）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "camelCase")]
pub enum OutlineLock {
    /// 全书大纲：只允许写 大纲/大纲.md。
    Book,
    /// 章级细纲：只允许该区间内的 细纲_第NNN章.md。
    Chapters(u32, u32),
    /// 大纲目录级：放行 大纲/ 下的文件，靠任务文本与拆步器约束。
    OutlineDir,
}

/// 一步的用途（结构化语义，替代「在任务文本里找关键词」）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StepPurpose {
    /// 建事件卡（`大纲/事件/`）
    EventCard,
    /// 批量章级细纲（`大纲/细纲_第NNN章.md` × N）
    OutlineBatch,
    /// 批量章级正文（`正文/第NNN章_*.md` × N）
    ProseBatch,
}

/// 计划里的一步：派给谁、要它交付什么。
///
/// R4：除 `agent_id`/`task` 外的字段全部可选。旧数据（`.storydesk/chats/*.json`
/// 与历史 `masterEvents`）里没有这些键，`#[serde(default)]` 让它们反序列化成 `None`，
/// 闸层再回退到文本解析——所以老会话仍能打开、仍能重跑。
/// 新数据由 [`crate::services::plan_step`] 的构造器写入，同时渲染出与原来逐字节相同的 `task`。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanStep {
    /// 子智能体 id（对应 `AgentConfig.id`）
    #[serde(alias = "agentId", alias = "agent", alias = "agent_id")]
    pub agent_id: String,
    /// 这一步要交付的内容（中文）
    #[serde(alias = "action", alias = "description")]
    pub task: String,
    /// 角色（writer / architect / tracker …）；`None` 时按 `agent_id` 现查档案。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<AgentRole>,
    /// 流水线归属（正文 / 细纲 / 去味）；`None` 时按角色目录猜。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<RunKind>,
    /// 这一步覆盖哪几章。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<ChapterRange>,
    /// 细纲步的文件锁。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lock: Option<OutlineLock>,
    /// 用户指定的纯正文字数区间。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub words: Option<(u32, u32)>,
    /// 只读步（核对/盘点/报告）。
    ///
    /// R4 规格里的五个字段没有对应项，而「`is_read_only_task` 改为优先读字段」
    /// 需要一个字段才能成立，所以补这一个——同样是 `Option` + `default`，
    /// 旧数据反序列化成 `None` 后仍走 `ai::is_read_only_task` 的文本判定。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_only: Option<bool>,
    /// 结构化语义：这一步是不是「全书收尾」步。
    ///
    /// `None` = 旧计划（没有这个字段），调用方回退到 `task` 里找 `【全书收尾】` 标记。
    /// 有字段之后，「收尾」不再靠 task 文本里有没有那句从句来判断——文本会被改写、会被搬运，
    /// 而字段跟着步骤走。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ending: Option<bool>,
    /// 结构化语义：这一步的用途（事件卡 / 批量细纲 / 批量正文）。
    ///
    /// 与 `ending` 同理：`None` 回退到文本判定。拆步器原来只认「任务里有没有『排事件卡』」，
    /// 一旦任务文本被别的流程改写（例如细纲批量任务），事件卡步就会被当成批量细纲步
    /// 而不拆、或者反过来被拆错。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose: Option<StepPurpose>,
    /// 这一步在为哪个叙事事件干活（`大纲/事件/事件_NNN_*.md`）。
    ///
    /// 有了它，拆步器才能把「同一事件的多章」当成一个整体排程，而不是一章一步；
    /// 没有它（老计划、老会话）就还走原来的一章一步，行为不变。
    /// 事件卡步本身不是新 `RunKind`：它复用 `kind = outline` + `lock = outlineDir`，
    /// 靠这个字段区分「写第 N 章细纲」与「写某事件的章型分工」。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event: Option<EventRef>,
}

/// 主 AI 拆解出来的执行计划。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MasterPlan {
    /// `llm` = 模型给的；`rules` = 模型没给出合法 JSON，用本地规则兜底
    pub source: String,
    /// 「我理解成了什么」：`写正文 · 正文/ · 第 1 章 · 题材「古代的农民起事」`。
    /// 计划卡上直接显示——用户点确认之前就知道 AI 到底理解成了什么。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intent: Option<String>,
    pub steps: Vec<PlanStep>,
    /// 模型原始输出（排错与「它到底说了什么」用）
    pub raw: String,
}

/// `master_plan` 的入参。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MasterPlanRequest {
    pub workspace_root: String,
    pub book_root: String,
    pub request: String,
    #[serde(default)]
    pub engine: Option<EngineKind>,
    /// 前端手上的智能体配置；为空时读 settings
    #[serde(default)]
    pub agents: Vec<crate::models::ai::AgentConfig>,
}

/// `master_run` 的入参。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MasterRunRequest {
    pub workspace_root: String,
    pub book_root: String,
    pub request: String,
    #[serde(default)]
    pub engine: Option<EngineKind>,
    #[serde(default)]
    pub agents: Vec<crate::models::ai::AgentConfig>,
    /// 写正文时的章号；不传就取下一章
    #[serde(default)]
    pub chapter: Option<u32>,
    /// 阶段 8：前端确认过的计划。给了就不再调模型规划（省一次 Token）。
    #[serde(default)]
    pub steps: Option<Vec<PlanStep>>,
    /// 阶段 8：计划来源标记（llm / rules / confirmed）。
    #[serde(default)]
    pub source: Option<String>,
    /// 阶段 8：沿用规划那一轮的 run id，把「计划卡」和「派发卡」串在同一段会话里。
    #[serde(default)]
    pub run_id: Option<String>,
    /// 断点续跑：从这个下标（0 基）往后跑，之前的步按运行状态文件跳过。
    /// `None` = 全新一轮（状态文件不存在时也按全新处理）。
    #[serde(default)]
    pub resume_from: Option<usize>,
}

/// 单步执行结果。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MasterStepResult {
    pub index: usize,
    pub agent_id: String,
    pub agent_name: String,
    /// 这一步自己的 run id（`ai://chunk` / `ai://done` 都带它）
    pub run_id: String,
    /// done | blocked | failed | cancelled
    pub status: String,
    pub exit: i32,
    pub duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr: Option<String>,
    /// 这一步总共跑了几次（引擎偶发错误重试后 > 1）
    #[serde(default)]
    pub attempts: u32,
    /// 一句话失败原因（引擎 stderr 摘要）；成功时为 None
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// 这一轮停下后能不能从这里续跑（失败/拦下的非批量步 = true）
    #[serde(default)]
    pub resumable: bool,
}

/// `master_run` 的返回：计划 + 每一步的结果。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MasterRunResult {
    pub run_id: String,
    pub plan: MasterPlan,
    pub steps: Vec<MasterStepResult>,
    /// done | blocked | failed
    pub status: String,
    /// 一句话收尾（给聊天流最后一条）
    pub summary: String,
}

/// `master://think` 的载荷：主 AI 正在思考。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MasterThink {
    pub run_id: String,
    /// thinking | planning | done
    pub phase: String,
    pub text: String,
}

/// `master://dispatch` 的载荷：把第 index 步派给了某个子智能体。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MasterDispatch {
    pub run_id: String,
    pub index: usize,
    pub total: usize,
    pub agent_id: String,
    pub agent_name: String,
    pub avatar: String,
    pub target_dir: String,
    pub task: String,
    /// 这一步的独立 run id，子智能体的流式日志按它归组
    pub step_run_id: String,
}

/// `master://progress` 的载荷：一步跑到哪了。
///
/// 派发卡只在派发那一刻说一句话，之后就整段静默；作者没法判断「在写 9000 字」
/// 和「进程卡死」的区别。这条心跳每隔几秒补一次已运行时长、已收到的输出条数、
/// 以及子智能体上一次吐字距现在多久，界面据此显示时钟和「可能卡住」提示。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MasterProgress {
    pub run_id: String,
    pub index: usize,
    pub total: usize,
    pub agent_id: String,
    pub agent_name: String,
    pub target_dir: String,
    /// 这一步负责的章号；与章无关的步骤（补设定/追踪）为 None
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chapter: Option<u32>,
    /// running | done | blocked | failed | cancelled
    pub status: String,
    /// 这一步开始到现在过了多少毫秒
    pub elapsed_ms: u64,
    /// 子智能体已经吐出的输出条数（0 表示它还没开口）
    pub chunks: u64,
    /// 距离上一次输出过了多少毫秒（None 表示还没有任何输出）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idle_ms: Option<u64>,
    /// 这一步已落盘的文件（跑完才有）
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub produced: Vec<String>,
    /// 到目前为止已经完成的步数（含跳过的），取自落盘的运行状态。
    #[serde(default)]
    pub done: usize,
    /// 从第几步续跑（1 基）。全部完成时为 None——那时没什么可续的。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_from: Option<usize>,
    /// 进度是否已经写进 `.storydesk/runs/<run_id>.json`（关掉窗口也不丢）。
    #[serde(default)]
    pub persisted: bool,
}

/// `master://plan` 的载荷：计划出来了（前端把步骤渲染成看板）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MasterPlanEvent {
    pub run_id: String,
    pub source: String,
    /// 阶段 11：「我理解成了什么」（计划卡上直接显示，确认前就能看见）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intent: Option<String>,
    pub steps: Vec<PlanStep>,
    /// 事件子类型：缺省是拆步之前那一版；`plan-final` 是拆步 + 排序之后的终版
    /// （前端据此替换计划卡的步骤列表，旧前端读到未知值时走原来的去重分支，安全忽略）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
}

/// `master://done` 的载荷：整轮编排结束。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MasterDone {
    pub run_id: String,
    /// done | blocked | failed
    pub status: String,
    pub text: String,
    pub steps_done: usize,
    pub steps_total: usize,
}

/// 阶段 8：`master_execute` 的入参——执行「用户已经确认过」的计划，不再调模型规划。
///
/// `steps` 由前端把 `master_plan` 返回的那份原样带回来，所以这一轮不会再烧一次 Token。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MasterExecuteRequest {
    pub workspace_root: String,
    pub book_root: String,
    pub steps: Vec<PlanStep>,
    pub engine: Option<EngineKind>,
    pub agents: Vec<crate::models::ai::AgentConfig>,
    /// 用户这一轮的原话。确认执行时必须带回来：章号与字数都要从这句话里取，
    /// 不能靠 `chapter`（那是前端 store 里的遗留值，实测把 001 章跑成了 005 章）。
    #[serde(default)]
    pub request: Option<String>,
    #[serde(default)]
    pub chapter: Option<u32>,
    /// 规划那一轮给的 run_id（前端用来把「计划卡」和「执行日志」串成一段）
    #[serde(default)]
    pub run_id: Option<String>,
    /// 计划来源：llm / rules / confirmed
    #[serde(default)]
    pub source: Option<String>,
}

/// `master://context` 的载荷：把上一步刚产出的文件交给下一步当上下文。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MasterContext {
    pub run_id: String,
    pub index: usize,
    pub total: usize,
    /// 这些产出是哪一步写出来的（0-based）
    pub from_index: usize,
    pub files: Vec<String>,
    pub text: String,
}
