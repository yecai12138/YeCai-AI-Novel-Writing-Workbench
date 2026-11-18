/** Rust 侧 DTO 的镜像类型。字段名与 `src-tauri/src/models/dto.rs` 一一对应（serde camelCase）。 */

export type TreeNodeKind = "dir" | "file";

export interface TreeNode {
  name: string;
  /** 绝对路径：仅用于展示与回传 IPC，前端不据此直接读写文件 */
  path: string;
  kind: TreeNodeKind;
  /** 相对书根的路径，如 `正文/第001章_开篇.md` */
  rel: string;
  children: TreeNode[];
  size: number | null;
}

export interface ProjectInfo {
  workspaceDir: string;
  bookRoot: string;
  bookName: string;
  /** `.active-book` 内容（相对工作区根的路径） */
  activeBook: string;
  created: boolean;
}

export interface ToolStatus {
  tool: string;
  ok: boolean;
  version: string | null;
  path: string | null;
  hint: string | null;
  detail: string | null;
}

export interface EnvReport {
  node: ToolStatus;
  python: ToolStatus;
  bash: ToolStatus;
  engine: ToolStatus;
  allOk: boolean;
  engineOk: boolean;
  platform: string;
  checkedAt: string;
}

export interface DirEntryInfo {
  name: string;
  path: string;
  isEmpty: boolean;
}

export interface DirListing {
  current: string;
  parent: string | null;
  entries: DirEntryInfo[];
}

/** `read_file` 的返回。 */
export interface FileContent {
  path: string;
  rel: string;
  content: string;
  bytes: number;
  mtimeMs: number | null;
  sha1: string;
}

/** 一份历史快照（`.storydesk/snapshots/<sha1>/<时间戳>.md`）。 */
export interface SnapshotInfo {
  id: string;
  path: string;
  rel: string;
  /** `2026-11-05T09-52-13.123Z`（冒号换成短横，Windows 文件名不允许 `:`） */
  createdAt: string;
  bytes: number;
}

/** `write_file` / `restore_snapshot` 的返回。 */
export interface SaveOutcome {
  file: FileContent;
  snapshot: SnapshotInfo | null;
  savedAt: string;
}

/** `fs://changed` 事件载荷。 */
export interface FsChangedPayload {
  root: string;
  paths: string[];
  kind: "create" | "modify" | "remove" | "other" | "mixed";
}

// ---------------------------------------------------------------- 阶段 4：AI 引擎

/** 一条流水线。与 Rust `RunKind` 对应。 */
export type RunKind = "outline" | "prose" | "deslop";

/** 宿主引擎：claude CLI 或 opencode CLI（自定义 OpenAI 协议 API 走 opencode.json 映射）。 */
export type EngineKind = "claude" | "opencode";

/** 六个智能体角色，与 Rust `models::ai::AgentRole` 一一对应。 */
export type AgentRole =
  | "researcher"
  | "architect"
  | "benchmarker"
  | "writer"
  | "settings"
  | "tracker";

/** `ai_start` 的入参（对应 Rust `RunRequest`）。 */
export interface AiRunRequest {
  workspaceRoot: string;
  bookRoot: string;
  kind: RunKind;
  engine?: EngineKind;
  chapter?: number | null;
  note?: string | null;
  /** 由哪个智能体执行；有值时会在四行契约**之前**注入一段 system prompt */
  agentRole?: AgentRole | null;
  /** 智能体聚焦目录（相对书根，如 `设定`）；不传用该角色的默认目录 */
  targetDir?: string | null;
  /** 前端抓到的上下文文件（相对书根）；空数组时后端自己扫目录兜底 */
  contextFiles?: string[];
}

/** 旧名兼容：阶段 4~6 的调用点写的是 `RunRequest`。 */
export type RunRequest = AiRunRequest;

/** `agent_specs` 的载荷：卡片与 prompt 用同一份档案（Rust 侧权威）。 */
export interface AgentInfo {
  role: AgentRole;
  name: string;
  emoji: string;
  dir: string;
  duty: string;
  guard: string;
}

/** `ai://chunk` 的载荷：归一化后的三种片段。 */
export interface AiChunkPayload {
  runId: string;
  seq: number;
  kind: "text" | "tool_use" | "status";
  text?: string;
  /** tool_use 的工具名，如 `Read` */
  tool?: string;
  /** tool_use 的目标，如 `大纲/细纲_第001章.md` */
  target?: string;
}

/** `ai://done` 的载荷。 */
export interface RunDonePayload {
  runId: string;
  exit: number;
  /** done | blocked | error | cancelled */
  status: "done" | "blocked" | "error" | "cancelled" | string;
  durationMs: number;
  stderr?: string;
  detail?: string;
}

/** Hook 阻断类型（Serde 内部标签 `kind`）。 */
export type HookBlockKind =
  | { kind: "missingOutline"; chapter: number }
  | { kind: "outlineTooShort"; outline: string }
  | { kind: "toxicDebtPrev" }
  /** 细纲声称属于某事件，但 `大纲/事件/` 下没有这张卡 */
  | { kind: "eventMissing"; chapter: number; event: string }
  /** 事件已结算，不能再往里加章 */
  | { kind: "eventSettled"; chapter: number; event: string }
  /** 卡在，但「章型分工」里没有这一章 */
  | { kind: "eventUnplanned"; chapter: number; event: string }
  | { kind: "unknown" };

/** `hook://blocked` 的载荷。 */
export interface HookBlock {
  runId: string;
  block: HookBlockKind;
  reason: string;
  stderr: string;
  /** generate_outline / open_prev_chapter / none */
  action: string;
  chapter?: number;
}

/** 门禁脚本的一次输出。 */
export interface GateOutput {
  ok: boolean;
  code: number;
  stdout: string;
  stderr: string;
}

/** oh-story 脚本链探测结果。 */
export interface GateInfo {
  ok: boolean;
  python: string | null;
  storyctl: string | null;
  brief: string | null;
  detail: string;
}

export interface TaskPacket {
  path: string;
  task: string;
  raw: string;
}

// ---------------------------------------------------------------- 阶段 5：设置与提供商

/** API 协议：决定请求头与注入的环境变量名。 */
export type ProviderProtocol = "openai" | "anthropic" | "bedrock";

/** 一个第三方模型提供商（**不含 API Key**，Key 在系统凭据库里）。 */
export interface ProviderConfig {
  id: string;
  name: string;
  baseUrl: string;
  protocol: ProviderProtocol;
  models: string[];
  /** 目录里选中的「使用中」模型；为空时引擎回退 models[0] */
  activeModel?: string | null;
}

export interface EnginePref {
  kind: EngineKind;
}

export interface AutosavePref {
  enabled: boolean;
  delayMs: number;
}

export interface AppSettings {
  version: number;
  activeProviderId: string | null;
  providers: ProviderConfig[];
  /**
   * 被用户删掉的内置提供商 id。
   * 不记下来的话，下次启动「补齐内置项」会把它又加回来。
   */
  removedBuiltins?: string[];
  engine: EnginePref;
  autosave: AutosavePref;
  /** 可增删改的子智能体（阶段 7.5）：主 AI 按这份名单派活 */
  agents: AgentConfig[];
  /**
   * 允许子智能体调用的工具（阶段 9）。缺省 = Read / Write；
   * 角色自己的 `AgentConfig.tools` 非空时优先用角色的那份。
   */
  allowedTools?: string[];
  /**
   * 指定的 opencode 可执行文件路径（第 8 项）。非空且文件存在时优于自动定位：
   * 用来指向桌面版自带的 v2，或切回本地保留的 v1.19.5。
   */
  opencodePath?: string | null;
}

/** 当前 opencode 引擎的版本/来源，以及本地保留的 v1 回退件（第 8 项 C）。 */
export interface OpencodeEngineInfo {
  path: string | null;
  version: string | null;
  /** 「设置指定」/「PATH」/「本程序安装目录」/「未找到」 */
  source: string;
  fallbackPath: string | null;
  fallbackVersion: string | null;
  pinnedVersion: string;
  configuredPath: string | null;
}

/** 运维面板：一个目录/一项资源的占用。 */
export interface OpsDirView {
  path: string;
  files: number;
  bytes: number;
  human: string;
}

export interface OpsDbView {
  path: string;
  bytes: number;
  human: string;
  walBytes: number;
  exists: boolean;
}

export interface OpsReposView {
  path: string;
  dirs: number;
  bytes: number;
  human: string;
}

export interface OpsOverview {
  logs: OpsDirView;
  engineLog: OpsDirView;
  database: OpsDbView;
  repos: OpsReposView;
  dbWarnBytes: number;
  logRotateAt: number;
}

export interface OpsAction {
  removed: number;
  freedBytes: number;
  freedHuman: string;
  note: string;
}

/** 设置界面「允许调用的工具」可勾的名字，与 Rust `models::ai::TOOL_CHOICES` 一一对应。 */
export const TOOL_CHOICES = [
  "Read",
  "Write",
  "Edit",
  "MultiEdit",
  "Bash",
  "Glob",
  "Grep",
  "WebFetch",
  "WebSearch",
] as const;

export type ToolName = (typeof TOOL_CHOICES)[number];

/** 某个提供商是否已存 Key（只有布尔，永远不含 key）。 */
export interface ProviderKeyState {
  providerId: string;
  hasKey: boolean;
  /** 是否支持「用环境变量替代填 Key」（目前只有 bedrock） */
  supportsEnv: boolean;
  /** 对应的环境变量名，用于提示文案 */
  keyEnv: string;
}

export interface SettingsView {
  settings: AppSettings;
  providerKeys: ProviderKeyState[];
  /** settings.json 的绝对路径（用来核对「文件里没有明文」） */
  path: string;
  secretBackend: string;
}

// ---------------------------------------------------------------- 阶段 5：后置体检

export interface PostCheckIssue {
  /** wordcount | ai_pattern | structure | script | missing | encoding | continuity */
  code: string;
  text: string;
  /** 这条问题针对哪个事件（`001`）：一章挂多张卡时靠它分清是哪张卡的账；与事件无关的问题没有 */
  event?: string | null;
}

/** 写完正文后的自动体检结果（`ai://postcheck`）。 */
export interface PostCheck {
  runId: string;
  chapter: number;
  ok: boolean;
  skipped: boolean;
  reason: string;
  issues: PostCheckIssue[];
  chars: number | null;
  file: string | null;
  stdout: string;
  stderr: string;
  checkedAt: string;
}

export interface EngineInfo {
  kind: EngineKind;
  name: string;
  available: boolean;
  detail: string;
}

/** `ai_start` 的返回。 */
export interface StartResult {
  runId: string;
  engine: EngineKind;
  chapter: number | null;
  /** 实际送进 CLI 的 prompt（不含任何密钥） */
  prompt: string;
  taskPacket: string | null;
  gate: GateOutput | null;
  /** 前置门禁就拦下时带回来，此时引擎未启动 */
  blocked: HookBlock | null;
  note: string | null;
  /** 本条由哪个智能体执行（没有智能体时 null） */
  agent: string | null;
}

// ---------------------------------------------------------------- 阶段 7.5：主从智能体

/** 可自定义的子智能体（对应 Rust `models::ai::AgentConfig`）。 */
export interface AgentConfig {
  id: string;
  name: string;
  /** Emoji 头像 */
  avatar: string;
  /** 定制的系统提示词：会原样进 `角色约束：…` */
  systemPrompt: string;
  /** 绑定的目录（相对书根），限定在 设定/大纲/正文/对标/参考资料/追踪 */
  targetDir: string;
  /** 预留字段，暂未参与执行 */
  tools: string[];
}

export type AgentRoleName =
  | "researcher"
  | "architect"
  | "benchmarker"
  | "writer"
  | "settings"
  | "tracker";

export type PlanStepKind = "outline" | "prose" | "deslop";

/** 章号范围：全书 / 单章 / 区间（对应后端 ChapterRange 的 serde tag=type, content=value）。 */
export type ChapterRange =
  | { type: "book" }
  | { type: "single"; value: number }
  | { type: "span"; value: [number, number] };

/** 细纲步允许写哪些文件（对应后端 OutlineLock）。 */
export type StepOutlineLock =
  | { type: "book" }
  | { type: "chapters"; value: [number, number] }
  | { type: "outlineDir" };

/** 主 AI 给出的一个步骤。
 *
 * R4 起步骤除了中文任务，还可以带结构化字段（角色 / 流水线 / 章号区间 / 文件锁 / 字数 / 只读）。
 * 这些字段一律可选：旧计划（磁盘上的 masterEvents、.storydesk/chats/*.json）里没有它们，
 * 后端在字段缺省时回退到任务文本解析。前端只透传，不参与判定。
 */
export interface PlanStep {
  agentId: string;
  task: string;
  /** researcher / architect / benchmarker / writer / settings / tracker */
  role?: AgentRoleName | null;
  /** outline / prose / deslop */
  kind?: PlanStepKind | null;
  range?: ChapterRange | null;
  lock?: StepOutlineLock | null;
  /** 目标字数区间 [下限, 上限] */
  words?: [number, number] | null;
  readOnly?: boolean | null;
  /** 事件卡步：`kind = outline` + `lock = outlineDir` 时靠它区分写章细纲还是写事件章型分工 */
  event?: EventRef | null;
}

/** ---------- 事件层（S1）：`大纲/事件/*.md` 的镜像类型 ---------- */

/** 事件状态：已规划 / 正在展开 / 已结算（后端 EventStatus，serde camelCase）。 */
export type EventStatus = "planned" | "active" | "settled";

/** 章型：起 / 推 / 转 / 结（后端 EventRole）。 */
export type EventRole = "setup" | "develop" | "turn" | "payoff";

/** 章型的中文标签，界面上和细纲里的写法一致。 */
export const EVENT_ROLE_LABEL: Record<EventRole, string> = {
  setup: "起",
  develop: "推",
  turn: "转",
  payoff: "结",
};

/** 计划步指向的事件：这一步在为哪个事件干活。 */
export interface EventRef {
  id: string;
  role?: EventRole | null;
}

/** 未结项：已经写出来、但还没办完的事。 */
export interface EventOpenItem {
  code: string;
  text: string;
  planted?: number | null;
  due?: number | null;
  anchors: string[];
}

/** 已结算账本的一行。 */
export interface EventSettledItem {
  code: string;
  atChapter?: number | null;
  note: string;
}

/** 事件内的章型分工：这一章消耗哪些未结项、新增哪些。 */
export interface EventChapterRole {
  chapter: number;
  role: EventRole;
  consumes: string[];
  plants: string[];
}

/** 一张完整的事件卡。 */
export interface EventCard {
  id: string;
  title: string;
  path: string;
  volume?: number | null;
  span?: [number, number] | null;
  prev?: string | null;
  status: EventStatus;
  settle: string[];
  openItems: EventOpenItem[];
  ledger: EventSettledItem[];
  roles: EventChapterRole[];
  warnings: string[];
}

/** 列表里的事件摘要。 */
export interface EventSummary {
  id: string;
  title: string;
  path: string;
  status: EventStatus;
  span?: [number, number] | null;
  openCount: number;
  roleCount: number;
}

/** 单张卡解析失败：其余卡照常返回，这里只是错误清单。 */
export interface EventParseError {
  path: string;
  message: string;
}

export interface EventList {
  cards: EventSummary[];
  errors: EventParseError[];
}

/** 审计发现：卡是不是空的（一章一事 / 账本没动过 / 细纲和卡对不上）。 */
export interface AuditFinding {
  /** single_chapter / no_ledger_progress / outline_absent / outline_silent / declared_elsewhere / missing_card / unplanned_in_card */
  code: string;
  /** warn 是要处理的问题；info 是提示（本书没有事件层时，细纲里的「事件 NNN」不构成错误） */
  severity: string;
  event?: string | null;
  chapter?: number | null;
  message: string;
}

export interface EventAudit {
  cards: number;
  /** 扫过的细纲份数：全书 大纲/细纲_第NNN章.md，不按章号截断 */
  outlinesScanned: number;
  /** 有「所属事件」声明的细纲数 */
  declaredChapters: number;
  findings: AuditFinding[];
  /** 「事件 → 章节」清单（方案五的只读索引要的第一样东西） */
  map?: EventChapterMap[];
  /** 欠账列表：还没结算的未结项（第二样东西） */
  debts?: EventDebt[];
  /** 卡内「已结算账本」里的每一行 */
  settledFromCard?: SettlementEntry[];
  /** `追踪/` 台账里认得的结算行 */
  settledFromTracking?: SettlementEntry[];
  /** 两本账对不上的地方：只列差异，不合并 */
  settlementDiff?: SettlementDiff[];
  /** 正文里出现的 `[[事件:xxx]]`：只记录，不参与归属 */
  bodyMentions?: BodyMention[];
}

/** 一条结算记录（卡内账本或 `追踪/` 台账的一行）。 */
export interface SettlementEntry {
  /** 卡内账本里的行记事件号；`追踪/` 台账里的行不知道归属，为空 */
  event: string;
  code: string;
  chapter?: number | null;
  note: string;
}

/** 两本账的一处差异。kind: card_only / tracking_only / chapter_mismatch */
export interface SettlementDiff {
  code: string;
  kind: string;
  message: string;
}

/** 正文里的一处 `[[事件:xxx]]` 引用。 */
export interface BodyMention {
  chapter: number;
  event: string;
}

/** 一行「事件 → 章节」：卡认的章与细纲声明的章分开列，对不上就是契约漂移。 */
export interface EventChapterMap {
  event: string;
  title: string;
  /** planned / active / settled */
  status: string;
  /** 卡自己认的章：章号区间展开 ∪ 章型分工 */
  chapters: number[];
  /** 细纲里写着「所属事件」或 `[[事件:NNN]]` 归它的章 */
  declaredChapters: number[];
  openItems: number;
  settledItems: number;
}

/** 一条欠账（未结算的未结项）。 */
export interface EventDebt {
  event: string;
  code: string;
  text: string;
  /** 卡上写的最迟结算章 */
  due?: number | null;
  /** 全书正文写到第几章（没有正文时 null） */
  writtenTo?: number | null;
  /** 正文已经写到最迟章之后，这一项还没结 */
  overdue: boolean;
  anchors: string[];
}

export interface MasterPlan {
  /** llm = 模型给的 JSON，rules = 本地关键词兜底 */
  source: string;
  /** 「我理解成了什么」：写正文 · 正文/ · 第 1 章（计划卡上直接显示，确认前就能看见） */
  intent?: string | null;
  steps: PlanStep[];
  raw: string;
}

export interface MasterPlanRequest {
  workspaceRoot: string;
  bookRoot: string;
  request: string;
  engine?: EngineKind;
  agents: AgentConfig[];
}

export interface MasterRunRequest extends MasterPlanRequest {
  chapter?: number | null;
  /** 阶段 8：作者确认过的计划；给了就不再规划 */
  steps?: PlanStep[];
  source?: string;
  runId?: string;
}

/** 阶段 8：`master_execute` 的入参——带着已确认的计划执行，不再调模型规划。 */
export interface MasterExecuteRequest {
  workspaceRoot: string;
  bookRoot: string;
  steps: PlanStep[];
  engine?: EngineKind;
  agents: AgentConfig[];
  /** 用户这一轮的原话：后端要拿它恢复章号、字数与题材（不带就会退回「下一章」）。 */
  request?: string;
  chapter?: number | null;
  runId?: string;
  source?: string;
}

export interface MasterStepResult {
  index: number;
  agentId: string;
  agentName: string;
  runId: string;
  /** done | blocked | failed | cancelled */
  status: string;
  exit: number;
  durationMs: number;
  stderr?: string | null;
}

export interface MasterRunResult {
  runId: string;
  plan: MasterPlan;
  steps: MasterStepResult[];
  status: string;
  summary: string;
}

/** `master://think` */
export interface MasterThinkPayload {
  runId: string;
  /** planning | thinking */
  phase: string;
  text: string;
}

/** `master://plan` */
export interface MasterPlanPayload {
  runId: string;
  source: string;
  /** 「我理解成了什么」，计划卡上直接显示 */
  intent?: string | null;
  steps: PlanStep[];
  /** `plan-final` = 拆步 + 排序后的终版计划；缺省是拆步前那一版 */
  kind?: string | null;
}

/** `master://dispatch`：聊天流里的「已派发」卡片 */
export interface MasterDispatchPayload {
  runId: string;
  index: number;
  total: number;
  agentId: string;
  agentName: string;
  avatar: string;
  targetDir: string;
  task: string;
  stepRunId: string;
}

/** `master://progress`：某一步的实时心跳（已跑多久 / 吐了多少条 / 多久没动静）。 */
export interface MasterProgressPayload {
  runId: string;
  index: number;
  total: number;
  agentId: string;
  agentName: string;
  targetDir: string;
  /** 这一步负责的章号；补设定/追踪这类与章无关的步骤没有 */
  chapter?: number | null;
  /** planning | running | done | blocked | failed | cancelled */
  status: string;
  elapsedMs: number;
  chunks: number;
  idleMs?: number | null;
  produced?: string[];
  /** 到目前为止已完成的步数（含跳过的），取自落盘的运行状态 */
  done?: number;
  /** 从第几步续跑（1 基）；全部完成时没有这个字段 */
  resumeFrom?: number | null;
  /** 进度是否已经写进 `.storydesk/runs/<runId>.json` */
  persisted?: boolean;
}

/** `master://done` */
export interface MasterDonePayload {
  runId: string;
  status: string;
  /** done | blocked | failed */
  text: string;
  stepsDone: number;
  stepsTotal: number;
  /** 第一个没跑完的步（1 基）；全跑完为 null——界面据此说「可从第 K 步续跑」 */
  resumeFrom?: number | null;
  /** 这一轮的进度已经落盘（关掉窗口也能续） */
  persisted?: boolean;
}

/** `master://context`：把上一步的产出交给下一步（或越界拦截提示）。 */
export interface MasterContextPayload {
  runId: string;
  index: number;
  total: number;
  fromIndex: number;
  files: string[];
  text: string;
}

/** 阶段 8：落盘的一条会话消息（`.storydesk/chats/<session>.json`）。 */
export interface ChatMessage {
  id: string;
  /** user | master | system | agent */
  role: string;
  content: string;
  plan?: PlanStep[];
  planSource?: string;
  /** 阶段 11：计划卡上的「我理解成了什么」，跟消息一起落盘 */
  planIntent?: string | null;
  /** 拆步后实际执行的步数（`plan-final` 事件回写；重开对话时恢复「已展开为 N 步」那行） */
  planExpanded?: number | null;
  payload?: Record<string, unknown> | null;
  timestamp: number;
}

/** Rust `StoryError` 的序列化形状：按 `kind` 分支展示，不用 message 做逻辑判断。 */
export type StoryError =
  | { kind: "envMissing"; tool: string; message: string; hint: string }
  | { kind: "fs"; message: string; path: string }
  | { kind: "pathEscape"; target: string; root: string }
  | { kind: "invalidInput"; field: string; message: string }
  | { kind: "internal"; message: string };

export function formatError(err: unknown): string {
  const e = err as Partial<StoryError> | string | undefined;
  if (!e) return "未知错误";
  if (typeof e === "string") return e;
  switch (e.kind) {
    case "envMissing":
      return `缺少环境：${e.tool} —— ${e.message}`;
    case "fs":
      return `文件操作失败：${e.path}\n${e.message}`;
    case "pathEscape":
      return `路径越界：${e.target} 不在工作区 ${e.root} 内`;
    case "invalidInput":
      return `参数不合法（${e.field}）：${e.message}`;
    case "internal":
      return `内部错误：${e.message}`;
    default:
      return (e as { message?: string }).message ?? "未知错误";
  }
}
