import { create } from "zustand";

import { api } from "@/lib/ipc";
import type { EnvReport, TreeNode } from "@/types";

const LAST_WORKSPACE_KEY = "storydesk.lastWorkspace";

interface ProjectState {
  workspaceDir: string | null;
  bookRoot: string | null;
  bookName: string | null;
  activeBook: string | null;
  books: string[];
  tree: TreeNode | null;
  selectedPath: string | null;
  selectedRel: string | null;

  loading: boolean;
  error: string | null;

  env: EnvReport | null;
  envLoading: boolean;

  createProject: (workspaceDir: string, bookName: string) => Promise<boolean>;
  openWorkspace: (workspaceDir: string) => Promise<boolean>;
  restoreLastWorkspace: () => Promise<void>;
  refreshTree: () => Promise<void>;
  loadEnv: () => Promise<void>;
  select: (node: TreeNode) => void;
  closeProject: () => void;
  clearError: () => void;
}

function rememberWorkspace(dir: string | null) {
  try {
    if (dir) localStorage.setItem(LAST_WORKSPACE_KEY, dir);
    else localStorage.removeItem(LAST_WORKSPACE_KEY);
  } catch {
    /* localStorage 不可用时忽略 */
  }
}

function lastWorkspace(): string | null {
  try {
    return localStorage.getItem(LAST_WORKSPACE_KEY);
  } catch {
    return null;
  }
}

export const useProjectStore = create<ProjectState>()((set, get) => ({
  workspaceDir: null,
  bookRoot: null,
  bookName: null,
  activeBook: null,
  books: [],
  tree: null,
  selectedPath: null,
  selectedRel: null,

  loading: false,
  error: null,

  env: null,
  envLoading: false,

  createProject: async (workspaceDir, bookName) => {
    set({ loading: true, error: null });
    try {
      const info = await api.newProject(workspaceDir, bookName);
      const [tree, books] = await Promise.all([
        api.listTree(info.bookRoot),
        api.listBooks(info.workspaceDir).catch(() => [] as string[]),
      ]);
      rememberWorkspace(info.workspaceDir);
      set({
        workspaceDir: info.workspaceDir,
        bookRoot: info.bookRoot,
        bookName: info.bookName,
        activeBook: info.activeBook,
        books,
        tree,
        selectedPath: null,
        selectedRel: null,
        loading: false,
        error: null,
      });
      return true;
    } catch (err) {
      set({ loading: false, error: errorText(err) });
      return false;
    }
  },

  openWorkspace: async (workspaceDir) => {
    set({ loading: true, error: null });
    try {
      const info = await api.openProject(workspaceDir);
      const [tree, books] = await Promise.all([
        api.listTree(info.bookRoot),
        api.listBooks(info.workspaceDir).catch(() => [] as string[]),
      ]);
      rememberWorkspace(info.workspaceDir);
      set({
        workspaceDir: info.workspaceDir,
        bookRoot: info.bookRoot,
        bookName: info.bookName,
        activeBook: info.activeBook,
        books,
        tree,
        selectedPath: null,
        selectedRel: null,
        loading: false,
        error: null,
      });
      return true;
    } catch (err) {
      set({ loading: false, error: errorText(err) });
      return false;
    }
  },

  restoreLastWorkspace: async () => {
    const dir = lastWorkspace();
    if (!dir) return;
    const ok = await get().openWorkspace(dir);
    if (!ok) {
      // 上次的项目已不在（被移动/删除）：回到欢迎页，不覆盖用户的新选择
      set({ workspaceDir: null, bookRoot: null, bookName: null, tree: null });
      rememberWorkspace(null);
    }
  },

  refreshTree: async () => {
    const bookRoot = get().bookRoot;
    if (!bookRoot) return;
    try {
      const tree = await api.listTree(bookRoot);
      set({ tree, error: null });
    } catch (err) {
      set({ error: errorText(err) });
    }
  },

  loadEnv: async () => {
    set({ envLoading: true });
    try {
      const env = await api.envCheck();
      set({ env, envLoading: false });
    } catch (err) {
      set({ envLoading: false, error: errorText(err) });
    }
  },

  select: (node) => set({ selectedPath: node.path, selectedRel: node.rel }),

  closeProject: () => {
    rememberWorkspace(null);
    set({
      workspaceDir: null,
      bookRoot: null,
      bookName: null,
      activeBook: null,
      books: [],
      tree: null,
      selectedPath: null,
      selectedRel: null,
      error: null,
    });
  },

  clearError: () => set({ error: null }),
}));

function errorText(err: unknown): string {
  const e = err as { kind?: string; message?: string; tool?: string; path?: string; field?: string };
  if (typeof err === "string") return err;
  if (!e || typeof e !== "object") return String(err);
  switch (e.kind) {
    case "envMissing":
      return `缺少环境依赖：${e.tool ?? ""} —— ${e.message ?? ""}`;
    case "fs":
      return `文件操作失败：${e.path ?? ""} —— ${e.message ?? ""}`;
    case "invalidInput":
      return `参数不合法（${e.field ?? ""}）：${e.message ?? ""}`;
    case "pathEscape":
      return "路径越界：目标不在工作区内";
    default:
      return e.message ?? "未知错误";
  }
}

export { errorText };
