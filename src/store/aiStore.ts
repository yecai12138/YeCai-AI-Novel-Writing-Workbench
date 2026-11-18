import { create } from "zustand";

import { api } from "@/lib/ipc";
import { useProjectStore } from "@/store/projectStore";
import { useSettingsStore } from "@/store/settingsStore";
import { formatError } from "@/types";
import type {
  AgentConfig,
  AgentRole,
  AiChunkPayload,
  ChatMessage,
  EngineInfo,
  EngineKind,
  EventList,
  GateInfo,
  HookBlock,
  MasterContextPayload,
  MasterDispatchPayload,
  MasterDonePayload,
  MasterProgressPayload,
  MasterPlanPayload,
  MasterRunResult,
  MasterThinkPayload,
  PlanStep,
  PostCheck,
  RunDonePayload,
  RunKind,
} from "@/types";

export interface RunChunk {
  seq: number;
  kind: "text" | "tool_use" | "status";
  text?: string;
  tool?: string;
  target?: string;
}

export type RunStatus = "running" | "done" | "blocked" | "error" | "cancelled";

export interface AiRun {
  id: string;
  kind: RunKind;
  engine: EngineKind;
  chapter: number | null;
  status: RunStatus;
  chunks: RunChunk[];
  hookBlock: HookBlock | null;
  prompt: string;
  taskPacket: string | null;
  note: string | null;
  startedAt: number;
  finishedAt: number | null;
  durationMs: number | null;
  exit: number | null;
  stderr: string | null;
  /** 这条是谁跑的：null = 手动流水线（不注入智能体段落） */
  agent: AgentRole | null;
}

/** `startRun` 的额外入参：智能体相关字段只在智能体面板里传。 */
export interface RunOverrides {
  chapter?: number | null;
  note?: string;
  agentRole?: AgentRole | null;
  targetDir?: string | null;
  contextFiles?: string[];
}

/** 主 AI 对话流里的一条事件（阶段 7.5，阶段 8 加了 system 与「待确认计划」）。 */
export type MasterEvent =
  | { kind: "user"; id: string; at: number; text: string }
  | { kind: "think"; id: string; at: number; phase: string; text: string }
  | {
      kind: "plan";
      id: string;
      at: number;
      source: string;
      intent?: string | null;
      steps: PlanStep[];
      /** 拆步之后实际执行的步数；有值说明这条计划事件被 `plan-final` 展开过 */
      expanded?: number | null;
    }
  | { kind: "dispatch"; id: string; at: number; payload: MasterDispatchPayload }
  | { kind: "done"; id: string; at: number; payload: MasterDonePayload }
  /** 系统提示：上下文传递、越界拦截、取消计划都走它 */
  | { kind: "system"; id: string; at: number; text: string };

/** 已经规划好、等作者点「确认执行」的计划。 */
export interface PendingPlan {
  runId: string;
  source: string;
  /** 「我理解成了什么」，计划卡上直接显示 */
  intent?: string | null;
  /** 这一轮的用户原话：确认执行时要一起带回去（章号/字数都从它取） */
  request?: string;
  /** 本轮规划完成的时间戳；只有「新鲜」的计划才走 5 秒自动确认（重开恢复的历史卡不自动跑） */
  plannedAt?: number;
  steps: PlanStep[];
}

interface AiState {
  runs: AiRun[];
  /** 正在跑的 run id（同一时刻只允许一条流水线，避免两侧抢着写盘） */
  activeRunId: string | null;
  engine: EngineKind;
  engines: EngineInfo[];
  gate: GateInfo | null;
  /** 事件层：`大纲/事件/` 的解析结果；null = 还没读过（不是「没有事件卡」） */
  events: EventList | null;
  /** runId → 写完正文后的自动体检结果（`ai://postcheck`） */
  postchecks: Record<string, PostCheck>;
  chapter: number | null;
  note: string;
  /** 当前被「唤醒」的智能体：null 表示没选，走手动流水线 */
  activeAgent: AgentRole | null;
  /** 右栏视图：智能体团队 / 对话 */
  panelTab: "team" | "chat";
  /** 主 AI 对话流（阶段 7.5）：用户消息 + 思考 + 计划 + 派发 + 终局 */
  masterEvents: MasterEvent[];
  /** 当前这一轮主从编排的 runId（没在跑时为 null） */
  masterRunId: string | null;
  /** 正在跑的那一步的 runId（「停止」按钮要发给它） */
  masterStepRunId: string | null;
  /** 当前正在给哪个子智能体派什么活（BusyHint 显示用），一轮结束清空 */
  masterDispatchNote: string | null;
  /** 当前那一步的任务全文（BusyHint 可展开），一轮结束清空 */
  masterDispatchTask: string | null;
  /** 当前那一步的实时心跳：已跑多久、吐了多少条、多久没动静（一轮结束清空） */
  masterProgress: MasterProgressPayload | null;
  masterBusy: boolean;
  /** 已经规划好、等作者点「确认执行」的计划（阶段 8 半自动模式） */
  masterPlan: PendingPlan | null;
  /** 等待区：忙时又下的指令按序排队，上一个任务彻底跑完自动触发下一个 */
  masterQueue: string[];
  /** 「修改需求」时回填给输入框的草稿 */
  masterDraft: string;
  /** 当前会话 id（就是落盘文件名 `.storydesk/chats/<id>.json`） */
  sessionId: string | null;
  /** 历史是否已尝试加载过（避免每次渲染都打一次 IPC） */
  chatLoaded: boolean;
  busy: boolean;
  error: string | null;
  /** 事件监听是否装好 */
  bound: boolean;

  /** 清掉失败提示（错误条上的「知道了」）；不动正在跑的流水线状态 */
  clearError: () => void;
  bindEvents: () => Promise<void>;
  applyChunk: (payload: AiChunkPayload) => void;
  applyDone: (payload: RunDonePayload) => void;
  applyBlocked: (payload: HookBlock) => void;
  applyPostCheck: (payload: PostCheck) => void;
  applyMasterThink: (payload: MasterThinkPayload) => void;
  applyMasterPlan: (payload: MasterPlanPayload) => void;
  applyMasterDispatch: (payload: MasterDispatchPayload) => void;
  applyMasterProgress: (payload: MasterProgressPayload) => void;
  applyMasterDone: (payload: MasterDonePayload) => void;
  /** 阶段 8：上下文传递 / 越界拦截的系统提示 */
  applyMasterContext: (payload: MasterContextPayload) => void;
  /** 对主 AI 说一句话：先只要计划（不花钱执行），作者确认后才跑 */
  planMaster: (text: string) => Promise<void>;
  /** 队列泵：没在忙、没有待确认计划且有排队指令时，自动下发下一条 */
  pumpMasterQueue: () => void;
  /** 从等待区撤掉某条指令 */
  removeQueued: (index: number) => void;
  /** 「✅ 确认执行」：带着已确认的计划调 `master_execute`，不再重新规划；
   *  传 `steps` 就是「编辑过的计划」——会换一个新 runId，避免和历史卡片撞 id。 */
  confirmMaster: (steps?: PlanStep[]) => Promise<void>;
  /** 「🚫 取消计划」/「✏️ 修改需求」：丢掉这份计划，把原话回填到输入框 */
  editMasterRequest: () => void;
  setDraft: (text: string) => void;
  /** 读取上一次会话（应用重启后恢复聊天流） */
  loadChat: () => Promise<void>;
  /** 清空当前对话（磁盘上写回 `[]`） */
  clearChat: () => Promise<void>;
  clearMaster: () => void;
  setChapter: (chapter: number | null) => void;
  setNote: (note: string) => void;
  setActiveAgent: (role: AgentRole | null) => void;
  /** 退出当前智能体并回到「智能体团队」——对话页头部的 × 用它。 */
  exitAgent: () => void;
  setPanelTab: (tab: "team" | "chat") => void;
  loadEngines: () => Promise<void>;
  /** 切换引擎偏好（claude / opencode），写进设置持久化。 */
  setEngine: (kind: EngineKind) => Promise<void>;
  loadGate: () => Promise<void>;
  /** 读一次 `大纲/事件/`。写完细纲/正文后调它刷新（事件卡由 AI 产出，随时会变）。 */
  loadEvents: () => Promise<void>;
  startRun: (kind: RunKind, overrides?: RunOverrides) => Promise<void>;
  /**
   * 断点续跑：接着上次停下的步派发。fromStep 是 0 基下标（第一步 = 0），
   * 省略时由后端取第一个没完成的步。返回 false = 没起（已有流水线在跑 / 起点非法）。
   */
  resumeMaster: (runId: string, fromStep?: number | null) => Promise<boolean>;
  cancelActive: () => Promise<void>;
  clearRuns: () => void;
  dismissBlock: (runId: string) => void;
  dismissPostCheck: (runId: string) => void;
  reset: () => void;
}

const KIND_LABEL: Record<RunKind, string> = {
  outline: "生成细纲",
  prose: "写本章正文",
  deslop: "去 AI 味",
};

export function runKindLabel(kind: RunKind): string {
  return KIND_LABEL[kind];
}

/** 事件可能比 `ai_start` 的返回值早到（前置门禁在命令内部就发事件），所以按 id 建占位 run。 */
function makeRun(id: string, kind: RunKind, chapter: number | null, engine: EngineKind): AiRun {
  return {
    id,
    kind,
    chapter,
    engine,
    status: "running",
    chunks: [],
    hookBlock: null,
    prompt: "",
    taskPacket: null,
    note: null,
    startedAt: Date.now(),
    finishedAt: null,
    durationMs: null,
    exit: null,
    stderr: null,
    agent: null,
  };
}

export const useAiStore = create<AiState>((set, get) => ({
  runs: [],
  activeRunId: null,
  engine: "claude",
  engines: [],
  gate: null,
  events: null,
  postchecks: {},
  chapter: null,
  note: "",
  activeAgent: null,
  panelTab: "team",
  masterEvents: [],
  masterRunId: null,
  masterStepRunId: null,
  masterDispatchNote: null,
  masterDispatchTask: null,
  masterProgress: null,
  masterBusy: false,
  masterPlan: null,
  masterQueue: [],
  masterDraft: "",
  sessionId: null,
  chatLoaded: false,
  busy: false,
  error: null,
  bound: false,

  clearError: () => set({ error: null }),

  bindEvents: async () => {
    if (get().bound) return;
    set({ bound: true });

    await api.onAiChunk((payload: AiChunkPayload) => get().applyChunk(payload));
    await api.onAiDone((payload: RunDonePayload) => get().applyDone(payload));
    await api.onHookBlocked((payload: HookBlock) => get().applyBlocked(payload));
    await api.onPostCheck((payload: PostCheck) => get().applyPostCheck(payload));
    await api.onMasterThink((payload) => get().applyMasterThink(payload));
    await api.onMasterPlan((payload) => get().applyMasterPlan(payload));
    await api.onMasterDispatch((payload) => get().applyMasterDispatch(payload));
    await api.onMasterProgress((payload) => get().applyMasterProgress(payload));
    await api.onMasterDone((payload) => get().applyMasterDone(payload));
    await api.onMasterContext((payload) => get().applyMasterContext(payload));
  },

  applyChunk: (payload: AiChunkPayload) =>
    set((state) => {
      // 主 AI 规划阶段的流式输出（runId === masterRunId；子步骤的 id 是 `<masterRunId>-stepN`）：
      // 实时并入聊天流里的同一条思考行，让「主 AI 正在思考…」后面跟的是模型此刻真实的
      // 思考/输出内容（思考块、环境说明、计划正文），而不是一句固定文案。不进 runs 列表
      // ——规划流没有「执行详情」卡，进 runs 只会留一个永远没人渲染的占位。
      if (state.masterRunId && payload.runId === state.masterRunId) {
        const text = (payload.text ?? "").trim();
        if (!text) return state;
        const liveId = `${payload.runId}-live`;
        const events = [...state.masterEvents];
        const last = events.length - 1;
        if (last >= 0 && events[last].kind === "think" && events[last].id === liveId) {
          // 增量追加到正在流式更新的那一行（只在它确实还是最后一条时合并，
          // 否则中间插过 plan/dispatch 的话宁可另起一行，也不回写历史）。
          events[last] = {
            ...events[last],
            at: Date.now(),
            text: `${events[last].text}${text}`,
          };
        } else {
          events.push({
            kind: "think" as const,
            id: liveId,
            at: Date.now(),
            phase: "stream",
            text,
          });
        }
        return { masterEvents: events };
      }

      const idx = state.runs.findIndex((r) => r.id === payload.runId);
      const runs = idx < 0 ? [placeholder(payload.runId), ...state.runs] : [...state.runs];
      const target = idx < 0 ? 0 : idx;
      const run = runs[target];
      if (run.chunks.some((c) => c.seq === payload.seq)) return state;
      runs[target] = {
        ...run,
        chunks: [
          ...run.chunks,
          { seq: payload.seq, kind: payload.kind, text: payload.text, tool: payload.tool, target: payload.target },
        ].sort((a, b) => a.seq - b.seq),
      };
      return { runs };
    }),

  applyDone: (payload: RunDonePayload) => {
    set((state) => {
      const idx = state.runs.findIndex((r) => r.id === payload.runId);
      const runs = idx < 0 ? [placeholder(payload.runId), ...state.runs] : [...state.runs];
      const target = idx < 0 ? 0 : idx;
      const status: RunStatus =
        payload.status === "done"
          ? "done"
          : payload.status === "blocked"
            ? "blocked"
            : payload.status === "cancelled"
              ? "cancelled"
              : "error";
      runs[target] = {
        ...runs[target],
        status,
        exit: payload.exit,
        durationMs: payload.durationMs,
        stderr: payload.stderr ?? runs[target].stderr,
        note: payload.detail ?? runs[target].note,
        finishedAt: Date.now(),
      };
      return {
        runs,
        activeRunId: state.activeRunId === payload.runId ? null : state.activeRunId,
      };
    });
  },

  applyBlocked: (payload: HookBlock) => {
    set((state) => {
      const idx = state.runs.findIndex((r) => r.id === payload.runId);
      const runs = idx < 0 ? [placeholder(payload.runId), ...state.runs] : [...state.runs];
      const target = idx < 0 ? 0 : idx;
      runs[target] = {
        ...runs[target],
        status: "blocked",
        hookBlock: payload,
        stderr: payload.stderr,
        chapter: payload.chapter ?? runs[target].chapter,
      };
      return { runs };
    });
  },

  applyPostCheck: (payload: PostCheck) => {
    set((state) => ({
      postchecks: { ...state.postchecks, [payload.runId]: payload },
    }));
  },

  applyMasterThink: (payload) =>
    set((state) => {
      const text = payload.text.trim();
      if (!text) return state;
      const dup = state.masterEvents.some(
        (e) => e.kind === "think" && e.id === payload.runId && e.phase === payload.phase && e.text === text,
      );
      if (dup) return state;
      return {
        masterRunId: payload.runId,
        masterEvents: [
          ...state.masterEvents,
          { kind: "think" as const, id: payload.runId, at: Date.now(), phase: payload.phase, text },
        ],
      };
    }),

  applyMasterPlan: (payload) =>
    set((state) => {
      // `plan-final`：拆步 + 排序之后的终版计划。计划卡第一次挂出来时发的是拆步前的
      // 那一版（用户要先看「我理解成了什么」再点确认），这里把同一张卡的步骤列表换成
      // 真实执行的那份，并把步数标出来——否则卡上写着 2 步、实际跑了 30 步。
      if (payload.kind === "plan-final") {
        const idx = state.masterEvents.findIndex(
          (e) => e.kind === "plan" && e.id === payload.runId,
        );
        if (idx < 0) {
          // 没有对应的初见卡（历史被清过 / 事件乱序）：按普通计划补一张，别丢信息。
          const event: MasterEvent = {
            kind: "plan",
            id: payload.runId,
            at: Date.now(),
            source: payload.source,
            intent: payload.intent ?? null,
            steps: payload.steps,
            expanded: payload.steps.length,
          };
          persistMasterEvent(event);
          return { masterRunId: payload.runId, masterEvents: [...state.masterEvents, event] };
        }
        const prev = state.masterEvents[idx];
        if (prev.kind !== "plan") return state;
        const updated: MasterEvent = {
          ...prev,
          at: prev.at,
          source: payload.source,
          intent: payload.intent ?? prev.intent ?? null,
          steps: payload.steps,
          expanded: payload.steps.length,
        };
        persistMasterEvent(updated);
        const masterEvents = [...state.masterEvents];
        masterEvents[idx] = updated;
        return { masterRunId: payload.runId, masterEvents };
      }
      if (state.masterEvents.some((e) => e.kind === "plan" && e.id === payload.runId)) return state;
      const event: MasterEvent = {
        kind: "plan",
        id: payload.runId,
        at: Date.now(),
        source: payload.source,
        intent: payload.intent ?? null,
        steps: payload.steps,
      };
      persistMasterEvent(event);
      return { masterRunId: payload.runId, masterEvents: [...state.masterEvents, event] };
    }),

  applyMasterDispatch: (payload) =>
    set((state) => {
      const dup = state.masterEvents.some(
        (e) => e.kind === "dispatch" && e.payload.stepRunId === payload.stepRunId,
      );
      if (dup) return state;
      const event: MasterEvent = { kind: "dispatch", id: payload.runId, at: Date.now(), payload };
      persistMasterEvent(event);
      // BusyHint：标题行短句一眼可读；任务全文单独存，太长才存（BusyHint 里可展开）。
      // 阶段 11.8：图 1 事故——派活提示只有一行被截断的小字，作者看不出 AI 在干什么。
      return {
        masterRunId: payload.runId,
        masterStepRunId: payload.stepRunId,
        masterDispatchNote: `${payload.agentName}（${payload.agentId}）· 第 ${payload.index + 1}/${payload.total} 步`,
        // 任务全文一律留下：作者要看的正是「主 AI 到底让子智能体做什么」，
        // 之前只在超过 90 字时才存，短任务反而看不到具体内容（图 1 事故）。
        masterDispatchTask: payload.task,
        masterProgress: null,
        masterEvents: [...state.masterEvents, event],
      };
    }),

  applyMasterProgress: (payload) =>
    set((state) => {
      // 心跳只更新 BusyHint，不进聊天流：两秒一条会把对话刷爆。
      // 规划阶段（index 0 / planner）没有 dispatch 卡片，也要把 note 立起来，
      // 否则模型端不出字时界面还是一片空白，作者只能猜是不是卡死。
      if (payload.agentId === "planner") {
        return {
          masterProgress: payload,
          masterDispatchNote:
            payload.status === "planning"
              ? "主 AI 正在拆解任务（规划中）"
              : state.masterDispatchNote,
          masterDispatchTask: state.masterDispatchTask ?? "（主 AI 规划中，还没有派发子任务）",
        };
      }
      return { masterProgress: payload };
    }),

  applyMasterDone: (payload) =>
    set((state) => {
      const id = payload.runId;
      const dup = state.masterEvents.some((e) => e.kind === "done" && e.id === id);
      const event: MasterEvent = { kind: "done", id, at: Date.now(), payload };
      if (!dup) persistMasterEvent(event);
      return {
        masterRunId: null,
        masterStepRunId: null,
        masterDispatchNote: null,
        masterDispatchTask: null,
        masterProgress: null,
        masterEvents: dup ? state.masterEvents : [...state.masterEvents, event],
      };
    }),

  applyMasterContext: (payload) =>
    set((state) => {
      const event: MasterEvent = {
        kind: "system",
        id: `${payload.runId}-ctx${payload.index}`,
        at: Date.now(),
        text: payload.text,
      };
      persistMasterEvent(event);
      return { masterEvents: [...state.masterEvents, event] };
    }),

  planMaster: async (text) => {
    const project = useProjectStore.getState();
    const trimmed = text.trim();
    if (!trimmed) return;
    if (!project.workspaceDir || !project.bookRoot) {
      set({ error: "先打开一个项目" });
      return;
    }
    // 忙、或已有待确认计划：不打断、也不报错——进等待区，
    // 上一个任务彻底跑完（或计划被取消）后自动下发下一条。
    if (get().masterBusy || get().masterPlan !== null) {
      set((state) => ({
        masterQueue: [...state.masterQueue, trimmed],
        masterEvents: [
          ...state.masterEvents,
          {
            kind: "system",
            id: `sys-${Date.now().toString(36)}-q`,
            at: Date.now(),
            text: `已收到指令 · 进入等待区（第 ${state.masterQueue.length + 1} 位）：「${trimmed.slice(0, 40)}${trimmed.length > 40 ? "…" : ""}」· 上一个任务结束后自动执行`,
          } as MasterEvent,
        ],
      }));
      return;
    }
    const agents: AgentConfig[] = useSettingsStore.getState().agents();
    if (agents.length === 0) {
      set({ error: "还没有智能体角色：设置 → 智能体角色 → 恢复默认 6 个角色" });
      return;
    }
    const bookRoot = project.bookRoot;
    const sessionId = get().sessionId ?? (await ensureSession(bookRoot));
    if (get().sessionId !== sessionId) set({ sessionId });
    set({ masterBusy: true, error: null, masterDraft: "" });

    // 先落一条用户消息：哪怕规划失败，作者的原话也不会丢
    const userEvent: MasterEvent = {
      kind: "user",
      id: `u-${Date.now().toString(36)}`,
      at: Date.now(),
      text: trimmed,
    };
    set((state) => ({ masterEvents: [...state.masterEvents, userEvent] }));
    void persistEvent(bookRoot, sessionId, userEvent);

    const startedAt = Date.now();
    try {
      const plan = await api.masterPlan({
        workspaceRoot: project.workspaceDir,
        bookRoot,
        request: trimmed,
        engine: get().engine,
        agents,
      });
      // 正常情况 `master://plan` 已经把计划卡推进来了；监听没装上时用返回值补一张。
      const pushed = get()
        .masterEvents.filter((e) => e.kind === "plan" && e.at >= startedAt)
        .pop();
      const planEvent: MasterEvent =
        pushed ??
        ({
          kind: "plan",
          id: `plan-${Date.now().toString(36)}`,
          at: Date.now(),
          source: plan.source,
          intent: plan.intent ?? null,
          steps: plan.steps,
        } as MasterEvent);
      if (!pushed) {
        set((state) => ({ masterEvents: [...state.masterEvents, planEvent] }));
        void persistEvent(bookRoot, sessionId, planEvent);
      }
      const steps = planEvent.kind === "plan" ? planEvent.steps : plan.steps;
      set({
        masterPlan: {
          runId: planEvent.id,
          source: plan.source,
          intent: plan.intent ?? null,
          request: trimmed,
          plannedAt: Date.now(),
          steps,
        },
      });
    } catch (err) {
      set({ error: formatError(err) });
    } finally {
      set({ masterBusy: false });
      // 规划失败时（桌上没有计划卡）让等待区的指令接着走；
      // 成功时 pump 看到 masterPlan 还挂着会自己按住，等执行结束再放下一条。
      get().pumpMasterQueue();
    }
  },

  pumpMasterQueue: () => {
    const { masterBusy, masterPlan, masterQueue } = get();
    if (masterBusy || masterPlan !== null || masterQueue.length === 0) return;
    const [next, ...rest] = masterQueue;
    set({ masterQueue: rest });
    void get().planMaster(next);
  },

  removeQueued: (index) =>
    set((state) => ({ masterQueue: state.masterQueue.filter((_, i) => i !== index) })),

  confirmMaster: async (steps) => {
    const pending = get().masterPlan;
    const project = useProjectStore.getState();
    if (!pending) return;
    if (!project.workspaceDir || !project.bookRoot) {
      set({ error: "先打开一个项目" });
      return;
    }
    if (get().masterBusy) {
      set({ error: "还在派活，等这一步跑完" });
      return;
    }
    const agents: AgentConfig[] = useSettingsStore.getState().agents();
    const bookRoot = project.bookRoot;
    const sessionId = get().sessionId ?? (await ensureSession(bookRoot));

    // 阶段 9：编辑过的计划带新 runId（`<原>-e<时间戳>`），
    // 这样历史里的旧计划卡与新一轮的派发/收尾不会互相覆盖。
    const edited = Boolean(steps && steps.length > 0);
    const planSteps = edited ? (steps as PlanStep[]) : pending.steps;
    const runId = edited ? `${pending.runId}-e${Date.now().toString(36)}` : pending.runId;

    const startEvent: MasterEvent = {
      kind: "system",
      id: `sys-${Date.now().toString(36)}`,
      at: Date.now(),
      text: edited
        ? `已确认执行（改过的 ${planSteps.length} 步），开始派发。`
        : `已确认执行，开始派发 ${planSteps.length} 步。`,
    };
    set((state) => ({
      masterBusy: true,
      masterPlan: null,
      error: null,
      masterEvents: [...state.masterEvents, startEvent],
    }));
    void persistEvent(bookRoot, sessionId, startEvent);

    try {
      const result: MasterRunResult = await api.masterExecute({
        workspaceRoot: project.workspaceDir,
        bookRoot,
        steps: planSteps,
        engine: get().engine,
        agents,
        // 用户原话必须带回去：后端靠它恢复章号、字数与题材。以前这里没带 request，
        // 只带 store 里上一轮遗留的 chapter，于是计划卡写着 001 章、派发按 005 章跑。
        request: pending.request ?? "",
        // 也不再把 store 里的 chapter 当权威：章号以用户原话与计划步骤里写的为准，
        // 都没有时后端自己取「正文的下一章」。这里传 null，避免旧值覆盖用户的话。
        chapter: null,
        source: pending.source,
        runId,
      });
      // 事件正常情况已经到齐；这里补齐「监听没装上 / 事件丢了」的情况
      set((state) => ({ masterEvents: reconcile(state.masterEvents, result, agents) }));
      const done = get()
        .masterEvents.filter((e) => e.kind === "done")
        .pop();
      if (done) void persistEvent(bookRoot, sessionId, done);
      // 派发卡也要落盘（重开软件后卡片还在）
      for (const event of get().masterEvents) {
        if (event.kind === "dispatch" && event.at >= startEvent.at) {
          void persistEvent(bookRoot, sessionId, event);
        }
      }
    } catch (err) {
      set({ error: formatError(err) });
    } finally {
      set({ masterBusy: false, masterStepRunId: null });
      // 子智能体是直接写盘的，刷一下文件树让落盘结果立刻可见
      void useProjectStore.getState().refreshTree();
      void get().loadGate();
      // 这一轮可能新增了事件卡、也可能刚往账本里写了结算行：面板要跟着变
      void get().loadEvents();
      // 上一个任务彻底跑完：等待区的下一条自动下发
      get().pumpMasterQueue();
    }
  },

  resumeMaster: async (runId, fromStep) => {
    const project = useProjectStore.getState();
    if (!project.workspaceDir || !project.bookRoot) {
      set({ error: "先打开一个项目" });
      return false;
    }
    // 与 startRun 同一个口径的原子互斥：检查与占坑之间不能有 await。
    if (get().masterBusy) {
      set({ error: "上一条流水线还在跑，先等它结束或取消" });
      return false;
    }
    const agents: AgentConfig[] = useSettingsStore.getState().agents();
    const bookRoot = project.bookRoot;
    // 占坑：必须在第一个 await 之前落地。
    set({ masterBusy: true, error: null });
    try {
      const sessionId = get().sessionId ?? (await ensureSession(bookRoot));
      const result: MasterRunResult = await api.masterResume({
        workspaceRoot: project.workspaceDir,
        bookRoot,
        runId,
        fromStep: fromStep ?? null,
        engine: get().engine,
      });
      // 事件正常已经到齐；这里补齐「监听没装上 / 事件丢了」的情况。
      set((state) => ({ masterEvents: reconcile(state.masterEvents, result, agents) }));
      const done = get()
        .masterEvents.filter((e) => e.kind === "done")
        .pop();
      if (done) void persistEvent(bookRoot, sessionId, done);
      return true;
    } catch (err) {
      // 起点越界之类的问题在这里落地成一句人话，UI 走既有 error 通道展示。
      set({ error: formatError(err) });
      return false;
    } finally {
      set({ masterBusy: false, masterStepRunId: null });
      void useProjectStore.getState().refreshTree();
      void get().loadGate();
      void get().loadEvents();
      get().pumpMasterQueue();
    }
  },

  editMasterRequest: () => {
    const lastUser = [...get().masterEvents].reverse().find((e) => e.kind === "user");
    const draft = lastUser && lastUser.kind === "user" ? lastUser.text : "";
    const pending = get().masterPlan;
    set((state) => ({
      masterPlan: null,
      masterDraft: draft,
      // 阶段 9：「取消计划」把那张还没执行的计划卡从对话流里摘掉（它还在磁盘上，
      // 下一轮 `loadChat` 会按「取消过又没执行」的规则把它过滤掉）
      masterEvents: [
        ...state.masterEvents.filter(
          (e) => !(e.kind === "plan" && pending !== null && e.id === pending.runId),
        ),
        {
          kind: "system" as const,
          id: `sys-${Date.now().toString(36)}`,
          at: Date.now(),
          text: "已取消这份计划，改完需求再下发。",
        },
      ],
    }));
    // 计划被撤 = 这一轮任务到此为止：等待区的下一条接着走
    get().pumpMasterQueue();
  },

  setDraft: (text) => set({ masterDraft: text }),

  loadChat: async () => {
    const project = useProjectStore.getState();
    if (!project.bookRoot) return;
    const bookRoot = project.bookRoot;
    let sessionId: string | null = null;
    try {
      sessionId = localStorage.getItem(SESSION_KEY(bookRoot));
      if (!sessionId) sessionId = await api.latestChatSession(bookRoot);
    } catch {
      sessionId = null;
    }
    if (!sessionId) sessionId = newSessionId();
    localStorage.setItem(SESSION_KEY(bookRoot), sessionId);

    let history: ChatMessage[] = [];
    try {
      history = await api.loadChatHistory(bookRoot, sessionId);
    } catch {
      history = [];
    }
    const events = history
      .map(toEvent)
      .filter((event): event is MasterEvent => event !== null)
      // 阶段 9：取消过的计划（后面跟了「已取消这份计划」、又没有派发/收尾）不回放，
      // 否则重开软件会看到一张按钮已经失效的孤儿卡。
      .filter((event, idx, all) => {
        if (event.kind !== "plan") return true;
        const later = all.slice(idx + 1);
        const started = later.some((e) => e.kind === "dispatch" || e.kind === "done");
        const cancelled = later.some((e) => e.kind === "system" && e.text.includes("已取消这份计划"));
        return !(cancelled && !started);
      });
    // 关软件前只拆了计划、还没点「确认执行」——重开之后这张卡得继续能点，
    // 否则用户看到一份计划却没法执行（只能重发一遍需求再烧一次 Token）。
    const lastPlan = [...events].reverse().find((e) => e.kind === "plan");
    const lastDone = [...events].reverse().find((e) => e.kind === "done");
    const stillPending =
      lastPlan && lastPlan.kind === "plan" && (!lastDone || lastPlan.at > lastDone.at)
        ? {
            runId: lastPlan.id,
            source: lastPlan.source,
            intent: lastPlan.intent ?? null,
            steps: lastPlan.steps,
          }
        : null;
    set({
      sessionId,
      chatLoaded: true,
      masterPlan: stillPending,
      masterEvents: events,
      masterRunId: null,
      masterStepRunId: null,
    });
  },

  clearChat: async () => {
    const project = useProjectStore.getState();
    const sessionId = get().sessionId;
    set({
      masterEvents: [],
      masterPlan: null,
      masterQueue: [],
      masterRunId: null,
      masterStepRunId: null,
      masterBusy: false,
      error: null,
    });
    if (!project.bookRoot || !sessionId) return;
    try {
      await api.clearChatHistory(project.bookRoot, sessionId);
    } catch (err) {
      set({ error: formatError(err) });
    }
  },

  clearMaster: () =>
    set({
      masterEvents: [],
      masterPlan: null,
      masterQueue: [],
      masterRunId: null,
      masterStepRunId: null,
      masterDispatchNote: null,
      masterDispatchTask: null,
      masterProgress: null,
      masterBusy: false,
    }),

  setChapter: (chapter) => set({ chapter }),
  setNote: (note) => set({ note }),
  setActiveAgent: (role) => set({ activeAgent: role, panelTab: "chat" }),
  exitAgent: () => set({ activeAgent: null, panelTab: "team" }),
  setPanelTab: (tab) => set({ panelTab: tab }),

  loadEngines: async () => {
    try {
      const engines = await api.aiEngines();
      // 引擎跟设置里的偏好走（设置 → AI 引擎可切换 claude / opencode）。
      // claude 不可用时后端会自动兜底到 opencode，这里只管把偏好传过去。
      const pref =
        useSettingsStore.getState().view?.settings.engine.kind === "opencode"
          ? "opencode"
          : "claude";
      set({ engines, engine: pref });
    } catch (err) {
      set({ error: formatError(err) });
    }
  },

  /** 切换引擎偏好（写进设置持久化），并立即生效到下一次任务。 */
  setEngine: async (kind) => {
    try {
      await useSettingsStore.getState().saveEngine(kind);
      set({ engine: kind });
    } catch (err) {
      set({ error: formatError(err) });
    }
  },

  loadEvents: async () => {
    const bookRoot = useProjectStore.getState().bookRoot;
    if (!bookRoot) {
      set({ events: null });
      return;
    }
    try {
      set({ events: await api.eventList(bookRoot) });
    } catch (err) {
      // 事件层是辅助信息，读失败不该挡住写作：清空并留一行错误
      set({ events: { cards: [], errors: [] }, error: formatError(err) });
    }
  },

  loadGate: async () => {
    const workspaceDir = useProjectStore.getState().workspaceDir;
    if (!workspaceDir) return;
    try {
      const gate = await api.gateProbe(workspaceDir);
      set({ gate });
    } catch (err) {
      set({ gate: { ok: false, python: null, storyctl: null, brief: null, detail: formatError(err) } });
    }
  },

  startRun: async (kind, overrides) => {
    const project = useProjectStore.getState();
    if (!project.workspaceDir || !project.bookRoot) {
      set({ error: "先打开一个项目" });
      return;
    }
    // 互斥必须「检查 + 占坑」在同一段同步代码里完成：zustand 的 set 是同步的，
    // 只要中间没有 await，连点两次时第二次一定能看到 busy。
    // 以前入口只查 activeRunId，而它要到 aiRun 返回之后（920 行）才写，
    // 于是两次点击都能通过检查、起两条流水线写同一章。
    if (get().activeRunId || get().busy) {
      set({ error: "上一条流水线还在跑，先等它结束或取消" });
      return;
    }

    const state = get();
    const chapter = overrides?.chapter ?? state.chapter;
    const note = overrides?.note ?? state.note;
    const agentRole = overrides?.agentRole ?? null;
    const targetDir = overrides?.targetDir ?? null;
    // 占坑：这一行必须在第一个 await 之前落地，它就是互斥锁本身。
    set({ busy: true, error: null });

    try {
      const result = await api.aiRun({
        workspaceRoot: project.workspaceDir,
        bookRoot: project.bookRoot,
        kind,
        engine: state.engine,
        chapter: chapter ?? null,
        note: note.trim() ? note.trim() : null,
        agentRole,
        targetDir,
        contextFiles: overrides?.contextFiles ?? [],
      });

      // 事件可能已经先到（前置门禁在命令内部发），用 upsert 把元信息补上，chunk 不丢。
      set((prev) => {
        const idx = prev.runs.findIndex((r) => r.id === result.runId);
        const run =
          idx >= 0
            ? {
                ...prev.runs[idx],
                kind,
                engine: result.engine,
                chapter: result.chapter,
                prompt: result.prompt,
                taskPacket: result.taskPacket,
                note: result.note,
                agent: agentRole,
              }
            : {
                ...makeRun(result.runId, kind, result.chapter, result.engine),
                prompt: result.prompt,
                taskPacket: result.taskPacket,
                note: result.note,
                agent: agentRole,
                status: result.blocked ? ("blocked" as RunStatus) : ("running" as RunStatus),
                hookBlock: result.blocked,
              };
        const runs = idx >= 0 ? [...prev.runs] : [run, ...prev.runs];
        if (idx >= 0) runs[idx] = run;
        return {
          runs,
          busy: false,
          activeRunId: result.blocked ? null : result.runId,
          chapter: result.chapter ?? prev.chapter,
        };
      });
    } catch (err) {
      set({ busy: false, error: formatError(err) });
    } finally {
      // 兜底释放占坑：正常路径与 catch 都已经把 busy 置回 false，这里只覆盖
      // 「异常绕过了上面两处」（例如 set 回调里抛错）的极端情况，
      // 避免 busy 永久为 true、界面再也起不了流水线。
      if (get().busy) set({ busy: false });
    }

    // 跑过一次就顺手刷新门禁状态：装好 storyctl.py 或改了 config.json 之后，
    // 面板上的「门禁脚本」徽标会自己更新，不必重开面板。
    void get().loadGate();
  },

  cancelActive: async () => {
    const id = get().activeRunId;
    if (!id) return;
    try {
      await api.aiCancel(id);
      set((state) => ({
        runs: state.runs.map((r) =>
          r.id === id ? { ...r, status: "cancelled" as RunStatus, finishedAt: Date.now() } : r,
        ),
        activeRunId: null,
      }));
    } catch (err) {
      set({ error: formatError(err) });
    }
  },

  clearRuns: () =>
    set({ runs: [], activeRunId: null, error: null, note: "", postchecks: {}, masterEvents: [], masterRunId: null }),

  dismissBlock: (runId) =>
    set((state) => ({
      runs: state.runs.map((r) =>
        r.id === runId && r.status === "blocked" ? { ...r, hookBlock: null } : r,
      ),
    })),

  dismissPostCheck: (runId) =>
    set((state) => {
      const next = { ...state.postchecks };
      delete next[runId];
      return { postchecks: next };
    }),

  reset: () =>
    set({
      runs: [],
      activeRunId: null,
      gate: null,
      postchecks: {},
      chapter: null,
      note: "",
      activeAgent: null,
      masterEvents: [],
      masterRunId: null,
      masterStepRunId: null,
      masterBusy: false,
      error: null,
      busy: false,
    }),
}));

/** 事件早于 `ai_start` 返回时的占位 run：kind/engine 随后由 startRun 补齐。 */
function placeholder(runId: string): AiRun {
  return makeRun(runId, "prose", null, "claude");
}

/** 每本书记一个会话 id（`localStorage`），重开应用就接着上次那段聊。 */
const SESSION_KEY = (bookRoot: string) => `storydesk.chatSession:${bookRoot}`;

function newSessionId(): string {
  return `s-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 6)}`;
}

async function ensureSession(bookRoot: string): Promise<string> {
  const saved = localStorage.getItem(SESSION_KEY(bookRoot));
  if (saved) return saved;
  const id = newSessionId();
  localStorage.setItem(SESSION_KEY(bookRoot), id);
  return id;
}

/**
 * 一条事件 → 落盘消息。
 * `think` 不落盘：它是过程噪音，重启后没必要重放；用户真正要留的是
 * 「我说了什么 / 它拆了什么 / 派给了谁 / 最后怎么样」。
 */
function toMessage(event: MasterEvent): ChatMessage | null {
  // 派发卡用 stepRunId 当消息 id（一步一张卡，重试也不会写两条）；
  // 计划用 `plan-` 前缀，避免和同一轮 runId 的收尾消息撞 id 互相覆盖。
  const id =
    event.kind === "dispatch"
      ? event.payload.stepRunId
      : event.kind === "plan"
        ? `plan-${event.id}`
        : event.id;
  const base = { id, timestamp: event.at };
  switch (event.kind) {
    case "user":
      return { ...base, role: "user", content: event.text };
    case "plan":
      return {
        ...base,
        role: "master",
        content: `拆解出 ${event.steps.length} 步计划`,
        plan: event.steps,
        planSource: event.source,
        planIntent: event.intent ?? null,
        // 同一张计划卡会被 `plan-final` 覆盖成拆步后的版本（id 同为 `plan-<runId>`，
        // 落盘按 id 覆盖），所以展开步数必须一起存，否则重开软件又只剩拆前的小步数。
        planExpanded: event.expanded ?? null,
      };
    case "dispatch":
      return {
        ...base,
        role: "agent",
        content: `已将任务派发给 ${event.payload.agentName}：${event.payload.task}`,
        payload: event.payload as unknown as Record<string, unknown>,
      };
    case "done":
      return {
        ...base,
        role: "master",
        content: event.payload.text,
        payload: event.payload as unknown as Record<string, unknown>,
      };
    case "system":
      return { ...base, role: "system", content: event.text };
    default:
      return null;
  }
}

/** 落盘消息 → 事件（重启后重建聊天流）。 */
function toEvent(message: ChatMessage): MasterEvent | null {
  const at = message.timestamp || Date.now();
  // 落盘时计划消息带了 `plan-` 前缀（避免和同轮 runId 的收尾消息撞 id），还原时去掉
  const id = message.id.startsWith("plan-") ? message.id.slice("plan-".length) : message.id;
  switch (message.role) {
    case "user":
      return { kind: "user", id, at, text: message.content };
    case "master":
      if (message.plan && message.plan.length > 0) {
        return {
          kind: "plan",
          id,
          at,
          source: message.planSource ?? "llm",
          intent: message.planIntent ?? null,
          steps: message.plan,
          expanded: message.planExpanded ?? null,
        };
      }
      if (message.payload && "stepsTotal" in message.payload) {
        return { kind: "done", id, at, payload: message.payload as unknown as MasterDonePayload };
      }
      return { kind: "system", id, at, text: message.content };
    case "agent":
      if (message.payload) {
        return {
          kind: "dispatch",
          id,
          at,
          payload: message.payload as unknown as MasterDispatchPayload,
        };
      }
      return { kind: "system", id, at, text: message.content };
    case "system":
      return { kind: "system", id, at, text: message.content };
    default:
      return null;
  }
}

/** 落盘是「顺手记一下」，失败只留日志，绝不打断对话流。 */
async function persistEvent(
  bookRoot: string,
  sessionId: string,
  event: MasterEvent,
): Promise<void> {
  const message = toMessage(event);
  if (!message) return;
  try {
    await api.saveChatMessage(bookRoot, sessionId, message);
  } catch (err) {
    console.warn("[storydesk] 会话落盘失败", formatError(err));
  }
}

/**
 * 事件一到就落盘（计划、派发、收尾都走这里）。
 *
 * 放在事件处理器里而不是「命令返回后统一补写」，是因为一轮编排可能跑几十秒，
 * 中途关掉软件也不该丢掉已经发生的派发卡片。
 */
function persistMasterEvent(event: MasterEvent): void {
  const bookRoot = useProjectStore.getState().bookRoot;
  const sessionId = useAiStore.getState().sessionId;
  if (!bookRoot || !sessionId) return;
  void persistEvent(bookRoot, sessionId, event);
}

/** 把 `master_run` 的返回值补进对话流（只补缺的事件，不覆盖已有的流式内容）。 */
function reconcile(events: MasterEvent[], result: MasterRunResult, agents: AgentConfig[]): MasterEvent[] {
  const next = [...events];
  const ranId = result.runId;

  if (!next.some((e) => e.kind === "plan" && e.id === ranId)) {
    next.push({
      kind: "plan",
      id: ranId,
      at: Date.now(),
      source: result.plan.source,
      intent: result.plan.intent ?? null,
      steps: result.plan.steps,
    });
  }

  for (const step of result.steps) {
    if (next.some((e) => e.kind === "dispatch" && e.payload.stepRunId === step.runId)) continue;
    const config = agents.find((a) => a.id === step.agentId);
    next.push({
      kind: "dispatch",
      id: ranId,
      at: Date.now(),
      payload: {
        runId: ranId,
        index: step.index,
        total: result.steps.length,
        agentId: step.agentId,
        agentName: step.agentName,
        avatar: config?.avatar ?? "🤖",
        targetDir: config?.targetDir ?? "",
        task: result.plan.steps[step.index]?.task ?? "",
        stepRunId: step.runId,
      },
    });
  }

  if (!next.some((e) => e.kind === "done" && e.id === ranId)) {
    next.push({
      kind: "done",
      id: ranId,
      at: Date.now(),
      payload: {
        runId: ranId,
        status: result.status,
        text: result.summary,
        stepsDone: result.steps.filter((s) => s.status === "done").length,
        stepsTotal: result.steps.length,
        // 第一个没跑完的步（1 基）。前端只做展示，续跑点在 Rust 侧由
        // `.storydesk/runs/<runId>.json` 决定，两边都是从同一份结果算出来的。
        resumeFrom:
          result.steps.findIndex((s) => s.status !== "done" && s.status !== "skipped") >= 0
            ? result.steps.findIndex((s) => s.status !== "done" && s.status !== "skipped") + 1
            : null,
        persisted: result.steps.length > 0,
      },
    });
  }

  return next;
}
