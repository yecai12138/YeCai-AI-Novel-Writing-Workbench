import { create } from "zustand";

import { api } from "@/lib/ipc";
import { formatError, TOOL_CHOICES } from "@/types";
import type { AgentConfig, AppSettings, ProviderConfig, ProviderKeyState, SettingsView } from "@/types";

interface SettingsState {
  view: SettingsView | null;
  loading: boolean;
  saving: boolean;
  error: string | null;
  loaded: boolean;

  load: () => Promise<void>;
  /** 某个提供商是否已存 Key（只看布尔，永远拿不到 key） */
  hasKey: (providerId: string) => boolean;
  keyState: (providerId: string) => ProviderKeyState | undefined;
  /** 当前激活的提供商 */
  activeProvider: () => ProviderConfig | null;

  setActiveProvider: (providerId: string | null) => Promise<void>;
  /** 新增或覆盖（按 id 匹配） */
  upsertProvider: (provider: ProviderConfig) => Promise<void>;
  updateProvider: (providerId: string, patch: Partial<ProviderConfig>) => Promise<void>;
  deleteProvider: (providerId: string) => Promise<void>;
  /** 追加模型 id（去重） */
  addModels: (providerId: string, models: string[]) => Promise<void>;
  removeModel: (providerId: string, model: string) => Promise<void>;
  /** 选定该提供商「使用中」的模型；null = 取消选择（回退目录第一个） */
  setActiveModel: (providerId: string, model: string | null) => Promise<void>;

  saveKey: (providerId: string, key: string) => Promise<void>;
  clearKey: (providerId: string) => Promise<void>;

  /** 当前生效的子智能体名单 */
  agents: () => AgentConfig[];
  /** 整份覆盖保存（增删改后调用） */
  saveAgents: (agents: AgentConfig[]) => Promise<void>;
  /** 恢复内置 6 个角色 */
  resetAgents: () => Promise<void>;

  /** 允许调用的工具（阶段 9）：全局白名单，空数组 = 一个都不许调 */
  allowedTools: () => string[];
  saveAllowedTools: (tools: string[]) => Promise<void>;
  /** 保存引擎偏好（claude / opencode） */
  saveEngine: (kind: "claude" | "opencode") => Promise<void>;

  reset: () => void;
}

const EMPTY: AppSettings = {
  version: 2,
  activeProviderId: null,
  providers: [],
  engine: { kind: "claude" },
  autosave: { enabled: true, delayMs: 1000 },
  agents: [],
  allowedTools: ["Read", "Write"],
};

/** 拿到当前设置（还没加载时给一份空壳，避免组件里到处判空）。 */
function currentSettings(view: SettingsView | null): AppSettings {
  return view?.settings ?? EMPTY;
}

export const useSettingsStore = create<SettingsState>((set, get) => ({
  view: null,
  loading: false,
  saving: false,
  error: null,
  loaded: false,

  load: async () => {
    set({ loading: true, error: null });
    try {
      const view = await api.getSettings();
      set({ view, loading: false, loaded: true });
    } catch (err) {
      set({ loading: false, loaded: true, error: formatError(err) });
    }
  },

  hasKey: (providerId) => get().view?.providerKeys.find((k) => k.providerId === providerId)?.hasKey ?? false,

  keyState: (providerId) => get().view?.providerKeys.find((k) => k.providerId === providerId),

  activeProvider: () => {
    const settings = currentSettings(get().view);
    if (!settings.activeProviderId) return null;
    return settings.providers.find((p) => p.id === settings.activeProviderId) ?? null;
  },

  setActiveProvider: async (providerId) => {
    const settings = currentSettings(get().view);
    await persist(set, get, { ...settings, activeProviderId: providerId });
  },

  upsertProvider: async (provider) => {
    const settings = currentSettings(get().view);
    const exists = settings.providers.some((p) => p.id === provider.id);
    const providers = exists
      ? settings.providers.map((p) => (p.id === provider.id ? provider : p))
      : [...settings.providers, provider];
    const activeProviderId = settings.activeProviderId ?? provider.id;
    await persist(set, get, { ...settings, providers, activeProviderId });
  },

  updateProvider: async (providerId, patch) => {
    const settings = currentSettings(get().view);
    const providers = settings.providers.map((p) => (p.id === providerId ? { ...p, ...patch } : p));
    await persist(set, get, { ...settings, providers });
  },

  deleteProvider: async (providerId) => {
    // 交给 Rust 一次做完：删配置 + 清当前标记 + 删凭据库里的 Key，
    // 前端只负责拿最新视图回来（本地拼一份容易和后端 normalize 的结果不一致）。
    set({ saving: true, error: null });
    try {
      await api.deleteProvider(providerId);
      const view = await api.getSettings();
      set({ view, saving: false, loaded: true });
    } catch (err) {
      set({ saving: false, error: formatError(err) });
      void get().load();
    }
  },

  addModels: async (providerId, models) => {
    const settings = currentSettings(get().view);
    const providers = settings.providers.map((p) => {
      if (p.id !== providerId) return p;
      const merged = [...p.models];
      for (const m of models) {
        const id = m.trim();
        if (id && !merged.includes(id)) merged.push(id);
      }
      return { ...p, models: merged };
    });
    await persist(set, get, { ...settings, providers });
  },

  removeModel: async (providerId, model) => {
    const settings = currentSettings(get().view);
    const providers = settings.providers.map((p) => {
      if (p.id !== providerId) return p;
      const next = { ...p, models: p.models.filter((m) => m !== model) };
      // 删掉的正好是「使用中」那个：一并清空选择，让引擎回退到目录第一个
      if (p.activeModel === model) next.activeModel = null;
      return next;
    });
    await persist(set, get, { ...settings, providers });
  },

  setActiveModel: async (providerId, model) => {
    const settings = currentSettings(get().view);
    const providers = settings.providers.map((p) =>
      p.id === providerId ? { ...p, activeModel: model } : p,
    );
    await persist(set, get, { ...settings, providers });
  },

  saveKey: async (providerId, key) => {
    if (!key.trim()) {
      set({ error: "请先粘贴 API Key" });
      return;
    }
    set({ saving: true, error: null });
    try {
      const view = await api.setProviderKey(providerId, key.trim());
      set({ view, saving: false });
    } catch (err) {
      set({ saving: false, error: formatError(err) });
    }
  },

  clearKey: async (providerId) => {
    set({ saving: true, error: null });
    try {
      const view = await api.setProviderKey(providerId, "");
      set({ view, saving: false });
    } catch (err) {
      set({ saving: false, error: formatError(err) });
    }
  },

  agents: () => currentSettings(get().view).agents,

  saveAgents: async (agents) => {
    const settings = currentSettings(get().view);
    await persist(set, get, { ...settings, agents });
  },

  resetAgents: async () => {
    try {
      const defaults = await api.masterDefaultAgents();
      await get().saveAgents(defaults);
    } catch (err) {
      set({ error: formatError(err) });
    }
  },

  allowedTools: () => currentSettings(get().view).allowedTools ?? ["Read", "Write"],

  saveAllowedTools: async (tools) => {
    const settings = currentSettings(get().view);
    await persist(set, get, {
      ...settings,
      allowedTools: TOOL_CHOICES.filter((tool) => tools.includes(tool)),
    });
  },

  /** 保存引擎偏好（claude / opencode），持久化到 settings.json。 */
  saveEngine: async (kind) => {
    const settings = currentSettings(get().view);
    await persist(set, get, {
      ...settings,
      engine: { kind },
    });
  },

  reset: () => set({ view: null, loading: false, saving: false, error: null, loaded: false }),
}));
/** 统一的落盘动作：写 Rust → 用返回的视图刷新本地（后端会 normalize 脏数据）。 */
async function persist(
  set: (partial: Partial<SettingsState>) => void,
  get: () => SettingsState,
  settings: AppSettings,
): Promise<void> {
  set({ saving: true, error: null });
  try {
    const view = await api.saveSettings(settings);
    set({ view, saving: false, loaded: true });
  } catch (err) {
    set({ saving: false, error: formatError(err) });
    // 失败时重新拉一次，避免界面停在「以为存上了」的乐观状态。
    void get().load();
  }
}
