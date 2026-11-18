import { create } from "zustand";

import { api } from "@/lib/ipc";
import { errorText, useProjectStore } from "@/store/projectStore";
import type { SnapshotInfo } from "@/types";

/** 自动保存延迟：停手 1 秒后落盘。 */
export const AUTOSAVE_DELAY = 1000;

export type SaveStatus = "idle" | "saving" | "saved" | "unsaved" | "error";

export interface ConflictState {
  /** 磁盘上现在的内容 */
  diskContent: string;
  /** 磁盘上内容的 sha1，仅用于日志/调试 */
  diskSha1: string;
  /** 检测到冲突的时间（毫秒） */
  detectedAt: number;
}

interface EditorState {
  path: string | null;
  rel: string | null;

  /** 编辑器里的内容 */
  content: string;
  /** 上次与磁盘同步时磁盘上的内容（用来判断“磁盘是不是真的变了”） */
  diskContent: string;

  dirty: boolean;
  status: SaveStatus;
  savedAt: number | null;
  loading: boolean;
  error: string | null;

  conflict: ConflictState | null;

  snapshots: SnapshotInfo[];
  lastSnapshot: SnapshotInfo | null;

  /** 返回 false = 没切（上一个文件的未保存内容没能落盘），当前内容原样保留 */
  openFile: (path: string, rel: string) => Promise<boolean>;
  /** 返回 false = 没关（未保存内容没能落盘），当前内容原样保留 */
  closeFile: () => Promise<boolean>;
  /** 清掉切换/保存失败留下的提示（错误条上的「知道了」） */
  clearError: () => void;
  setContent: (next: string) => void;
  save: () => Promise<boolean>;
  /** Ctrl/Cmd + S：先取消等待中的自动保存，再立刻写盘 */
  saveNow: () => Promise<boolean>;
  reloadFromDisk: () => Promise<void>;
  /** 冲突时选「保留我的版本」：磁盘版本另存 .bak，然后用自己的内容覆盖 */
  keepMine: () => Promise<void>;
  dismissConflict: () => void;
  loadSnapshots: () => Promise<void>;
  restoreSnapshot: (id: string) => Promise<void>;
  /** 物理删除一版快照（不可恢复，界面上必须先二次确认） */
  deleteSnapshot: (id: string) => Promise<void>;
  handleDiskChange: (paths: string[]) => Promise<void>;
  reset: () => void;
}

let autosaveTimer: ReturnType<typeof setTimeout> | null = null;

function clearAutosave() {
  if (autosaveTimer !== null) {
    clearTimeout(autosaveTimer);
    autosaveTimer = null;
  }
}

function scheduleAutosave() {
  clearAutosave();
  autosaveTimer = setTimeout(() => {
    autosaveTimer = null;
    void useEditorStore.getState().save();
  }, AUTOSAVE_DELAY);
}

/** 等正在进行的写盘落地（不发起新写盘）。最多等 5 秒，避免后端卡住时连切文件一起卡住。 */
async function waitWhileSaving(): Promise<void> {
  const deadline = Date.now() + 5000;
  while (useEditorStore.getState().status === "saving" && Date.now() < deadline) {
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
}

/**
 * 离开当前文件前，确认编辑器里的内容真的落盘了。
 *
 * 旧实现是 `await prev.save()` 不看返回值：save() 在「正在写 / 有未处理冲突 / 写盘抛错」
 * 三种情况下返回 false 且一个字节都不写，紧接着 openFile/closeFile 用 emptyState 把编辑器
 * 清掉——未保存的正文就这么没了。这里把三种情况都当成「不许走」，并把原因交给调用方提示用户。
 */
async function flushBeforeLeave(): Promise<{ ok: boolean; reason?: string }> {
  for (let round = 0; round < 3; round += 1) {
    await waitWhileSaving();
    const state = useEditorStore.getState();
    if (!state.path || !state.dirty) return { ok: true };
    if (state.conflict) {
      return {
        ok: false,
        reason:
          "这个文件有未处理的保存冲突（磁盘上的内容也被改过），请先选「重新加载」或「保留我的版本」",
      };
    }
    const saved = await state.save();
    const after = useEditorStore.getState();
    if (!after.dirty) return { ok: true };
    // 保存过程中用户又敲了字：再排一轮（最多 3 轮，避免和输入抢跑）
    if (!saved && after.status === "error") {
      return { ok: false, reason: after.error ?? "保存失败" };
    }
  }
  const last = useEditorStore.getState();
  return { ok: false, reason: last.error ?? "保存没有完成" };
}

/** Windows 路径比较：去掉 canonicalize 的 `\\?\` 前缀，忽略分隔符差异与大小写。 */
function samePath(a: string, b: string): boolean {
  const norm = (p: string) =>
    p
      .replace(/^\\\\\?\\UNC\\/i, "\\\\")
      .replace(/^\\\\\?\\/, "")
      .replace(/\\/g, "/")
      .toLowerCase();
  return norm(a) === norm(b);
}

const emptyState = {
  path: null,
  rel: null,
  content: "",
  diskContent: "",
  dirty: false,
  status: "idle" as SaveStatus,
  savedAt: null,
  loading: false,
  error: null,
  conflict: null,
  snapshots: [],
  lastSnapshot: null,
};

export const useEditorStore = create<EditorState>()((set, get) => ({
  ...emptyState,

  openFile: async (path, rel) => {
    const ws = useProjectStore.getState().workspaceDir;
    // 没项目时以前静默 return false，界面表现为「点了没反应」，这里补一条原因。
    if (!ws) {
      set({ error: "还没有打开项目：先从左上角打开书库，再点文件名" });
      return false;
    }

    const prev = get();
    // 同一个文件且有未保存改动：什么都不做。否则下面会从磁盘重读一遍，
    // 把编辑器里的 dirty 内容直接冲掉（点一下文件名就丢字，从来不提示）。
    // 干净的时候仍然允许重读，等价于「刷新」。
    if (prev.path === path && prev.dirty) return true;

    // 切换文件前必须确认手上的未保存内容真的落盘了；写不进去就留在原文件上，
    // 绝不带着 dirty 内容去执行下面的 set(...emptyState)。
    if (prev.dirty && prev.path) {
      const flushed = await flushBeforeLeave();
      if (!flushed.ok) {
        set({ error: `没有切换文件：${flushed.reason ?? "当前内容还没保存成功"}` });
        return false;
      }
    }

    clearAutosave();
    set({ loading: true, error: null, conflict: null });

    try {
      const file = await api.readFile(ws, path);
      set({
        path,
        rel: file.rel || rel,
        content: file.content,
        diskContent: file.content,
        dirty: false,
        status: "saved",
        savedAt: Date.now(),
        loading: false,
        error: null,
        conflict: null,
        snapshots: [],
        lastSnapshot: null,
      });
      void get().loadSnapshots();
      return true;
    } catch (err) {
      set({
        ...emptyState,
        path,
        rel,
        loading: false,
        status: "error",
        error: errorText(err),
      });
      return false;
    }
  },

  clearError: () => set({ error: null }),

  closeFile: async () => {
    const state = get();
    if (state.dirty && state.path) {
      const flushed = await flushBeforeLeave();
      if (!flushed.ok) {
        set({ error: `没有关闭：${flushed.reason ?? "当前内容还没保存成功"}` });
        return false;
      }
    }
    clearAutosave();
    set({ ...emptyState });
    return true;
  },

  setContent: (next) => {
    const { content, path } = get();
    if (!path || next === content) return;
    set({ content: next, dirty: next !== get().diskContent, status: "unsaved" });
    scheduleAutosave();
  },

  save: async () => {
    const { path, content, status, conflict } = get();
    const ws = useProjectStore.getState().workspaceDir;
    if (!path || !ws) return false;
    // 已经在写了：不并发，等这一轮结束由保存结果决定是否再排一次
    if (status === "saving") return false;
    // 冲突还没处理：不自动保存，等用户点「重新加载」或「保留我的版本」
    if (conflict) return false;

    set({ status: "saving", error: null });
    try {
      const outcome = await api.writeFile(ws, path, content);
      const stillDirty = get().content !== outcome.file.content;
      set({
        diskContent: outcome.file.content,
        rel: outcome.file.rel,
        dirty: stillDirty,
        status: stillDirty ? "unsaved" : "saved",
        savedAt: Date.now(),
        error: null,
        lastSnapshot: outcome.snapshot ?? get().lastSnapshot,
      });
      if (outcome.snapshot) {
        set((s) => ({ snapshots: [outcome.snapshot!, ...s.snapshots].slice(0, 20) }));
      }
      // 保存期间用户又敲字了：再排一次自动保存
      if (stillDirty) scheduleAutosave();
      // 文件大小变了，树里的字节数要跟着更新
      void useProjectStore.getState().refreshTree();
      return true;
    } catch (err) {
      set({ status: "error", error: errorText(err) });
      return false;
    }
  },

  saveNow: async () => {
    clearAutosave();
    return get().save();
  },

  reloadFromDisk: async () => {
    const { path } = get();
    const ws = useProjectStore.getState().workspaceDir;
    if (!path || !ws) return;
    clearAutosave();
    try {
      const file = await api.readFile(ws, path);
      set({
        content: file.content,
        diskContent: file.content,
        dirty: false,
        status: "saved",
        savedAt: Date.now(),
        conflict: null,
        error: null,
      });
    } catch (err) {
      set({ status: "error", error: errorText(err), conflict: null });
    }
  },

  keepMine: async () => {
    const { path, content, conflict } = get();
    const ws = useProjectStore.getState().workspaceDir;
    if (!path || !ws) return;

    try {
      // 磁盘版本另存一份 .bak，方便事后对照，绝不静默丢掉别人的改动
      if (conflict) {
        await api.writeFile(ws, `${path}.bak`, conflict.diskContent);
      }
      set({ conflict: null });
      const outcome = await api.writeFile(ws, path, content);
      set({
        diskContent: outcome.file.content,
        dirty: outcome.file.content !== get().content,
        status: outcome.file.content === get().content ? "saved" : "unsaved",
        savedAt: Date.now(),
        error: null,
      });
      void useProjectStore.getState().refreshTree();
    } catch (err) {
      set({ status: "error", error: errorText(err) });
    }
  },

  dismissConflict: () => set({ conflict: null }),

  loadSnapshots: async () => {
    const { path } = get();
    const ws = useProjectStore.getState().workspaceDir;
    if (!path || !ws) return;
    try {
      const snapshots = await api.listSnapshots(ws, path);
      set({ snapshots });
    } catch {
      // 快照列表只是辅助信息，读失败不打扰用户
    }
  },

  restoreSnapshot: async (id) => {
    const { path } = get();
    const ws = useProjectStore.getState().workspaceDir;
    if (!path || !ws) return;
    clearAutosave();
    try {
      const outcome = await api.restoreSnapshot(ws, path, id);
      set({
        content: outcome.file.content,
        diskContent: outcome.file.content,
        dirty: false,
        status: "saved",
        savedAt: Date.now(),
        conflict: null,
        error: null,
        lastSnapshot: outcome.snapshot ?? get().lastSnapshot,
      });
      await get().loadSnapshots();
      void useProjectStore.getState().refreshTree();
    } catch (err) {
      set({ status: "error", error: errorText(err) });
    }
  },

  deleteSnapshot: async (id) => {
    const { snapshots } = get();
    const ws = useProjectStore.getState().workspaceDir;
    const target = snapshots.find((s) => s.id === id);
    if (!target) return;
    if (!ws) return;
    try {
      // 后端要求绑定工作区根：快照路径必须在这个根内，否则拒绝删除。
      await api.deleteSnapshot(ws, target.path);
      // 本地直接摘掉，再跟后端对齐一次（并发下后端可能已经 prune 掉了）
      set({ snapshots: get().snapshots.filter((s) => s.id !== id) });
      await get().loadSnapshots();
    } catch (err) {
      set({ status: "error", error: errorText(err) });
    }
  },

  handleDiskChange: async (paths) => {
    const { path, dirty, status, diskContent } = get();
    const ws = useProjectStore.getState().workspaceDir;
    if (!path || !ws) return;
    if (!paths.some((p) => samePath(p, path))) return;

    // 自己正在写盘：等写完再看，避免把自家写入误判成外部修改
    if (status === "saving") return;

    let fresh;
    try {
      fresh = await api.readFile(ws, path);
    } catch {
      set({ error: "磁盘上的这个文件已经不见了（可能被外部删除或改名为目录）" });
      return;
    }

    const next = fresh.content;
    if (next === get().content) {
      // 内容一致：只是 mtime 变了（比如另存了一遍）
      set({ diskContent: next, dirty: false, status: "saved" });
      return;
    }

    if (!dirty) {
      // 编辑器没有未保存改动：静默跟随磁盘
      set({
        content: next,
        diskContent: next,
        dirty: false,
        status: "saved",
        savedAt: Date.now(),
        conflict: null,
        error: null,
      });
      return;
    }

    // 有未保存改动，且磁盘上的内容也变了 → 冲突
    if (next !== diskContent) {
      // 冲突未解决前不许自动保存，否则会把别人的改动直接盖掉
      clearAutosave();
      set({
        conflict: { diskContent: next, diskSha1: fresh.sha1, detectedAt: Date.now() },
      });
    }
  },

  reset: () => {
    clearAutosave();
    set({ ...emptyState });
  },
}));
