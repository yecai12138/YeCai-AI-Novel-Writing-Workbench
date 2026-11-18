import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

import type {
  AgentConfig,
  AgentInfo,
  AiChunkPayload,
  AiRunRequest,
  AppSettings,
  ChatMessage,
  DirListing,
  EngineInfo,
  EnvReport,
  FileContent,
  FsChangedPayload,
  GateInfo,
  GateOutput,
  HookBlock,
  MasterDispatchPayload,
  MasterDonePayload,
  MasterContextPayload,
  MasterExecuteRequest,
  MasterPlan,
  MasterPlanPayload,
  MasterPlanRequest,
  MasterProgressPayload,
  MasterRunRequest,
  MasterRunResult,
  MasterThinkPayload,
  EventAudit,
  EventCard,
  EventList,
  PostCheck,
  ProjectInfo,
  ProviderConfig,
  RunDonePayload,
  RunRequest,
  SaveOutcome,
  SettingsView,
  SnapshotInfo,
  StartResult,
  StoryError,
  TaskPacket,
  TreeNode,
} from "@/types";

/**
 * 唯一的后端出口。
 *
 * 渲染进程没有任何 fs / shell / http 插件权限，所有磁盘操作都必须经过这里的命令，
 * 由 Rust 侧的 path_guard 做归属校验。
 */
export function isTauriRuntime(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

const NOT_TAURI: StoryError = {
  kind: "internal",
  message:
    "当前不是 Tauri 运行环境。请用 `npm run tauri dev` 启动桌面应用（单独的 `npm run dev` 只有网页外壳，没有 Rust 后端）。",
};

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (!isTauriRuntime()) throw NOT_TAURI;
  return (await invoke(cmd, args)) as T;
}

/** 提示词优化结果：真模型改写的产物 + 是谁改的（界面上要如实标出来）。 */
export interface PolishView {
  text: string;
  provider: string;
  model: string;
}

export const api = {
  envCheck: () => call<EnvReport>("env_check"),

  /**
   * 提示词优化：把口语指令交给设置里激活的提供商（走它的 API Key）改写成任务规格。
   * 没 Key / 模型报错 / 超时都会抛错，调用方回落到本地规则并显示原因。
   */
  polishPrompt: (draft: string) => call<PolishView>("polish_prompt", { draft }),

  newProject: (workspaceDir: string, bookName: string) =>
    call<ProjectInfo>("new_project", { workspaceDir, bookName }),

  openProject: (workspaceDir: string) =>
    call<ProjectInfo>("open_project", { workspaceDir }),

  listTree: (bookRoot: string) => call<TreeNode>("list_tree", { bookRoot }),

  /** 事件层：`大纲/事件/*.md` 的解析结果（S1）。目录不存在时给空列表，不报错。 */
  eventList: (bookRoot: string) => call<EventList>("event_list", { bookRoot }),

  eventGet: (bookRoot: string, eventId: string) =>
    call<EventCard>("event_get", { bookRoot, eventId }),

  /** 事件层反向审计：只读，给清单不改文件。 */
  eventAudit: (bookRoot: string) => call<EventAudit>("event_audit", { bookRoot }),

  listBooks: (workspaceDir: string) =>
    call<string[]>("list_books", { workspaceDir }),

  readActiveBook: (workspaceDir: string) =>
    call<string | null>("read_active_book", { workspaceDir }),

  setActiveBook: (workspaceDir: string, activeBook: string) =>
    call<ProjectInfo>("set_active_book", { workspaceDir, activeBook }),

  /** 目录选择器：`path` 传空字符串时列举盘符。 */
  listDirs: (path: string) => call<DirListing>("list_dirs", { path }),

  /** 路径守卫自测：正常返回规范化后的路径，越界抛 `pathEscape`。 */
  validatePath: (workspaceRoot: string, target: string) =>
    call<string>("validate_path_cmd", { workspaceRoot, target }),

  // ---------------------------------------------------------------- 文件读写

  readFile: (workspaceRoot: string, path: string) =>
    call<FileContent>("read_file", { workspaceRoot, path }),

  writeFile: (workspaceRoot: string, path: string, content: string) =>
    call<SaveOutcome>("write_file", { workspaceRoot, path, content }),

  createChapter: (
    workspaceRoot: string,
    bookRoot: string,
    chapterNo: number | null,
    title: string,
  ) =>
    call<FileContent>("create_chapter", {
      workspaceRoot,
      bookRoot,
      chapterNo,
      title,
    }),

  createNote: (workspaceRoot: string, dirPath: string, name: string) =>
    call<FileContent>("create_note", { workspaceRoot, dirPath, name }),

  renameEntry: (workspaceRoot: string, path: string, newName: string) =>
    call<FileContent>("rename_entry", { workspaceRoot, path, newName }),

  moveEntry: (workspaceRoot: string, path: string, destDir: string) =>
    call<FileContent>("move_entry", { workspaceRoot, path, destDir }),

  deleteEntry: (workspaceRoot: string, path: string) =>
    call<string>("delete_entry", { workspaceRoot, path }),

  // ------------------------------------------------------------------- 快照

  listSnapshots: (workspaceRoot: string, path: string) =>
    call<SnapshotInfo[]>("list_snapshots", { workspaceRoot, path }),

  restoreSnapshot: (workspaceRoot: string, path: string, snapshotId: string) =>
    call<SaveOutcome>("restore_snapshot", { workspaceRoot, path, snapshotId }),

  /** 物理删除一版快照（后端绑定 workspaceRoot 校验它确实在 .storydesk/snapshots/ 下）。 */
  deleteSnapshot: (workspaceRoot: string, snapshotPath: string) =>
    call<void>("delete_snapshot", { workspaceRoot, snapshotPath }),

  // ------------------------------------------------------------------- 监听

  watchProject: (workspaceRoot: string) =>
    call<string>("watch_project", { workspaceRoot }),

  unwatchProject: () => call<void>("unwatch_project"),

  onFsChanged: async (
    handler: (payload: FsChangedPayload) => void,
  ): Promise<UnlistenFn> => {
    if (!isTauriRuntime()) return () => {};
    return listen<FsChangedPayload>("fs://changed", (event) => handler(event.payload));
  },

  // --------------------------------------------------------------- AI 引擎

  aiStart: (req: RunRequest) => call<StartResult>("ai_start", { req }),

  /** 阶段 7 起的正式名字（带智能体字段），与 `aiStart` 调同一个命令。 */
  aiRun: (req: AiRunRequest) => call<StartResult>("ai_start", { req }),

  /** 六个智能体的权威档案：名称、emoji、负责目录、职责、强约束。 */
  agentSpecs: () => call<AgentInfo[]>("agent_specs"),

  // ------------------------------------------------------- 主从智能体（阶段 7.5）

  /** 只规划不执行：给「半自动」模式用。 */
  masterPlan: (req: MasterPlanRequest) => call<MasterPlan>("master_plan", { req }),

  /** 规划 + 逐步派发 + 回传终局。 */
  masterRun: (req: MasterRunRequest) => call<MasterRunResult>("master_run", { req }),

  /** 阶段 8：执行作者确认过的计划（不再规划，省一次 Token）。 */
  masterExecute: (req: MasterExecuteRequest) =>
    call<MasterRunResult>("master_execute", { req }),

  /**
   * 断点续跑：读 .storydesk/runs/<runId>.json，从 fromStep 接着派发，已完成的步不重跑。
   * fromStep 是 0 基下标（第一步 = 0），省略时后端自己取第一个没完成的步；
   * 越界由后端报错，不会把状态文件写成「全部完成」。
   */
  masterResume: (req: {
    workspaceRoot: string;
    bookRoot: string;
    runId: string;
    fromStep?: number | null;
    engine?: MasterRunRequest["engine"];
  }) =>
    call<MasterRunResult>("master_resume", {
      workspaceRoot: req.workspaceRoot,
      bookRoot: req.bookRoot,
      runId: req.runId,
      fromStep: req.fromStep ?? null,
      engine: req.engine ?? null,
    }),

  // ------------------------------------------------------- 会话落盘（阶段 8）

  saveChatMessage: (bookRoot: string, sessionId: string, message: ChatMessage) =>
    call<void>("save_chat_message", { bookRoot, sessionId, message }),

  loadChatHistory: (bookRoot: string, sessionId: string) =>
    call<ChatMessage[]>("load_chat_history", { bookRoot, sessionId }),

  clearChatHistory: (bookRoot: string, sessionId: string) =>
    call<void>("clear_chat_history", { bookRoot, sessionId }),

  latestChatSession: (bookRoot: string) =>
    call<string | null>("latest_chat_session", { bookRoot }),

  onMasterContext: async (
    handler: (payload: MasterContextPayload) => void,
  ): Promise<UnlistenFn> => {
    if (!isTauriRuntime()) return () => {};
    return listen<MasterContextPayload>("master://context", (event) => handler(event.payload));
  },

  /** settings.json 里当前生效的子智能体名单。 */
  masterAgents: () => call<AgentConfig[]>("master_agents"),

  /** 内置六个角色的默认配置（不写盘，设置页「恢复默认」用）。 */
  masterDefaultAgents: () => call<AgentConfig[]>("master_default_agents"),

  onMasterThink: async (handler: (payload: MasterThinkPayload) => void): Promise<UnlistenFn> => {
    if (!isTauriRuntime()) return () => {};
    return listen<MasterThinkPayload>("master://think", (event) => handler(event.payload));
  },

  onMasterPlan: async (handler: (payload: MasterPlanPayload) => void): Promise<UnlistenFn> => {
    if (!isTauriRuntime()) return () => {};
    return listen<MasterPlanPayload>("master://plan", (event) => handler(event.payload));
  },

  onMasterDispatch: async (
    handler: (payload: MasterDispatchPayload) => void,
  ): Promise<UnlistenFn> => {
    if (!isTauriRuntime()) return () => {};
    return listen<MasterDispatchPayload>("master://dispatch", (event) => handler(event.payload));
  },

  onMasterDone: async (handler: (payload: MasterDonePayload) => void): Promise<UnlistenFn> => {
    if (!isTauriRuntime()) return () => {};
    return listen<MasterDonePayload>("master://done", (event) => handler(event.payload));
  },

  /** 某一步的实时心跳：已跑多久、吐了多少条输出、多久没动静。 */
  onMasterProgress: async (
    handler: (payload: MasterProgressPayload) => void,
  ): Promise<UnlistenFn> => {
    if (!isTauriRuntime()) return () => {};
    return listen<MasterProgressPayload>("master://progress", (event) => handler(event.payload));
  },

  aiCancel: (runId: string) => call<boolean>("ai_cancel", { runId }),

  aiRunning: () => call<string[]>("ai_running"),

  aiEngines: () => call<EngineInfo[]>("ai_engines"),

  /** 自动下载并安装 opencode CLI（进度走 opencode://progress 事件）。 */
  engineInstallOpencode: () => call<string>("engine_install_opencode"),

  /**
   * 本程序自己的 opencode 数据目录用量（设置页告警）。
   * 隔离之后会话库不再和桌面版共用；这里只报自己的体积。
   */
  /**
   * 当前 opencode 引擎的版本与来源 + 本地保留的 v1 回退件（第 8 项）。
   * 设置页据此显示「现在跑哪个版本」，并给出「切回 v1.19.5」。
   */
  /** 运维面板：日志 / 会话库 / repos 的占用。 */
  opsOverview: () => call<import("@/types").OpsOverview>("ops_overview"),
  /** 清空本程序日志目录（目录保留）。 */
  opsCleanLogs: () => call<import("@/types").OpsAction>("ops_clean_logs"),
  /** 会话库整理：checkpoint → VACUUM → integrity_check。 */
  opsVacuumDatabase: () => call<import("@/types").OpsAction>("ops_vacuum_database"),
  /** 删掉超过 keepDays 天没动过的 repos 子目录。 */
  opsCleanRepos: (keepDays: number) =>
    call<import("@/types").OpsAction>("ops_clean_repos", { keepDays }),

  opencodeEngineInfo: () => call<import("@/types").OpencodeEngineInfo>("opencode_engine_info"),

  opencodeStorage: () =>
    call<{
      root: string;
      dbBytes: number;
      logBytes: number;
      warning: string | null;
    }>("opencode_storage"),

  /** 自动下载运行时依赖：node / python / gitbash（国内镜像，进度走 setup://progress）。 */
  runtimeInstall: (tool: "node" | "python" | "gitbash") => call<string>("runtime_install", { tool }),

  /** 运行时依赖下载进度事件（tool 区分哪个工具）。 */
  onSetupProgress: async (handler: (payload: { tool: string; downloaded: number; total?: number; note?: string }) => void): Promise<UnlistenFn> => {
    if (!isTauriRuntime()) return () => {};
    return listen<{ tool: string; downloaded: number; total?: number; note?: string }>("setup://progress", (event) => handler(event.payload));
  },

  /** opencode 下载进度事件（downloaded/total 字节 + 阶段说明）。 */
  onOpencodeProgress: async (handler: (payload: { downloaded: number; total?: number; note?: string }) => void): Promise<UnlistenFn> => {
    if (!isTauriRuntime()) return () => {};
    return listen<{ downloaded: number; total?: number; note?: string }>("opencode://progress", (event) => handler(event.payload));
  },

  onAiChunk: async (handler: (payload: AiChunkPayload) => void): Promise<UnlistenFn> => {
    if (!isTauriRuntime()) return () => {};
    return listen<AiChunkPayload>("ai://chunk", (event) => handler(event.payload));
  },

  onAiDone: async (handler: (payload: RunDonePayload) => void): Promise<UnlistenFn> => {
    if (!isTauriRuntime()) return () => {};
    return listen<RunDonePayload>("ai://done", (event) => handler(event.payload));
  },

  onHookBlocked: async (handler: (payload: HookBlock) => void): Promise<UnlistenFn> => {
    if (!isTauriRuntime()) return () => {};
    return listen<HookBlock>("hook://blocked", (event) => handler(event.payload));
  },

  /** 写完正文后的自动体检结果。 */
  onPostCheck: async (handler: (payload: PostCheck) => void): Promise<UnlistenFn> => {
    if (!isTauriRuntime()) return () => {};
    return listen<PostCheck>("ai://postcheck", (event) => handler(event.payload));
  },

  // ----------------------------------------------------------------- 门禁

  gateProbe: (workspaceRoot: string) => call<GateInfo>("gate_probe", { workspaceRoot }),

  gateChapterCheck: (workspaceRoot: string, bookRoot: string, chapter: number) =>
    call<GateOutput>("gate_chapter_check", { workspaceRoot, bookRoot, chapter }),

  /** 手动重跑后置体检（卡片上的「重新体检」）。 */
  gatePostcheck: (workspaceRoot: string, bookRoot: string, chapter: number) =>
    call<PostCheck>("gate_postcheck", { workspaceRoot, bookRoot, chapter }),

  gateBuildPacket: (
    workspaceRoot: string,
    bookRoot: string,
    task: string,
    volume?: number | null,
    chapters?: string | null,
  ) =>
    call<TaskPacket>("gate_build_packet", {
      workspaceRoot,
      bookRoot,
      task,
      volume: volume ?? null,
      chapters: chapters ?? null,
    }),

  // ----------------------------------------------------------------- 设置与提供商

  getSettings: () => call<SettingsView>("get_settings"),

  saveSettings: (settings: AppSettings) => call<SettingsView>("save_settings", { settings }),

  /** 写某个提供商的 Key（空串 = 删除）。返回值只有 `hasKey`，不会回传明文。 */
  setProviderKey: (providerId: string, key: string) =>
    call<SettingsView>("set_provider_key", { providerId, key }),

  checkProviderKey: (providerId: string) => call<boolean>("check_provider_key", { providerId }),

  deleteProviderKey: (providerId: string) =>
    call<SettingsView>("delete_provider_key", { providerId }),

  /** 删除提供商：配置 + 当前标记 + 凭据库里的 Key 一起清（Rust 侧 `delete_provider`）。 */
  deleteProvider: (providerId: string) => call<void>("delete_provider", { providerId }),

  /** 真实请求 `{baseUrl}/models` 拉可用模型列表。 */
  fetchAvailableModels: (baseUrl: string, protocol: string, apiKey?: string | null) =>
    call<string[]>("fetch_available_models", {
      baseUrl,
      protocol,
      apiKey: apiKey ?? null,
    }),

  validateProvider: (provider: ProviderConfig) =>
    call<string | null>("validate_provider", { provider }),
};
