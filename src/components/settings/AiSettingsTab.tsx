import { AlertTriangle, KeyRound, Loader2, Plus, Trash2, X } from "lucide-react";
import { useEffect, useMemo, useState } from "react";

import { Badge } from "@/components/common/Field";
import { ConfirmDialog } from "@/components/common/ConfirmDialog";
import { ModelPickerDialog } from "@/components/settings/ModelPickerDialog";
import { OpsSection } from "@/components/settings/OpsSection";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { api, isTauriRuntime } from "@/lib/ipc";
import { cn } from "@/lib/cn";
import { useSettingsStore } from "@/store/settingsStore";
import type { OpencodeEngineInfo, ProviderConfig, ProviderProtocol } from "@/types";
import { formatError } from "@/types";

/**
 * opencode 会话库体量告警。
 *
 * 数据目录隔离之后，会话库归本程序独有——它不会自己瘦身，涨到一定体量就该提示清理，
 * 否则 1.7 GB 的库会把每次开会话都拖慢。
 */
/**
 * opencode 引擎版本与来源（第 8 项 C）。
 *
 * 两版 CLI 的参数不兼容（v2 要 `--standalone`、`--log-level` 只认小写），所以界面
 * 必须能直接回答「现在跑的是哪一版、从哪来的」；健康自检失败时用户还得能一键切回。
 */
function OpenCodeEngineSection() {
  const [info, setInfo] = useState<OpencodeEngineInfo | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const settings = useSettingsStore((s) => s.view?.settings ?? null);
  const saving = useSettingsStore((s) => s.saving);

  useEffect(() => {
    let alive = true;
    void api
      .opencodeEngineInfo()
      .then((view) => {
        if (alive) setInfo(view);
      })
      .catch((err) => {
        if (alive) setError(formatError(err));
      });
    return () => {
      alive = false;
    };
  }, []);

  const switchTo = (path: string | null) => {
    if (!settings) return;
    setBusy(true);
    setError(null);
    void api
      .saveSettings({ ...settings, opencodePath: path })
      .then(() => api.opencodeEngineInfo())
      .then((view) => setInfo(view))
      .catch((err) => setError(formatError(err)))
      .finally(() => setBusy(false));
  };

  const usingFallback =
    !!info?.fallbackPath && info.path === info.fallbackPath;

  return (
    <div className="space-y-2 rounded border border-ink-200 bg-ink-50 px-2.5 py-2">
      <div className="flex items-center gap-2">
        <span className="text-[11px] font-medium text-ink-700">opencode 引擎</span>
        {info?.version ? (
          <Badge tone="ok">v{info.version}</Badge>
        ) : (
          <Badge tone="warn">未检测到</Badge>
        )}
        {info && <span className="text-[11px] text-ink-500">来源：{info.source}</span>}
        {info?.configuredPath && !info.path?.startsWith(info.configuredPath) && (
          <Badge tone="warn">指定的路径不存在</Badge>
        )}
      </div>
      <div className="break-all text-[11px] text-ink-500">
        {info?.path ?? "没有找到 opencode 可执行文件"}
      </div>
      <div className="text-[11px] text-ink-500">
        本程序下载的版本钉在 v{info?.pinnedVersion ?? "?"}；v2 会带 --standalone 与私有数据目录，
        v1 不带（两版参数不兼容，切换后由引擎自动适配）。
      </div>
      <div className="flex flex-wrap items-center gap-2">
        {info?.fallbackPath && !usingFallback && (
          <Button
            size="sm"
            variant="outline"
            disabled={busy || saving}
            onClick={() => switchTo(info.fallbackPath)}
          >
            切回 v1.19.5
          </Button>
        )}
        {(usingFallback || info?.configuredPath) && (
          <Button
            size="sm"
            variant="outline"
            disabled={busy || saving}
            onClick={() => switchTo(null)}
          >
            改回自动定位
          </Button>
        )}
        {info?.fallbackPath && (
          <span className="text-[11px] text-ink-400">
            回退件：{info.fallbackVersion ?? "v1"} · {info.fallbackPath}
          </span>
        )}
      </div>
      {error && <div className="text-[11px] text-rose-600">{error}</div>}
    </div>
  );
}

function OpenCodeStorageNotice() {
  const [warning, setWarning] = useState<string | null>(null);

  useEffect(() => {
    let alive = true;
    void api
      .opencodeStorage()
      .then((view) => {
        if (alive) setWarning(view.warning ?? null);
      })
      .catch(() => {
        // 读不到就不显示：这是提示，不该在设置页上抛错。
      });
    return () => {
      alive = false;
    };
  }, []);

  if (!warning) return null;
  return (
    <div className="flex items-start gap-2 rounded border border-amber-200 bg-amber-50 px-2 py-1.5 text-[11px] text-amber-800">
      <AlertTriangle className="mt-0.5 size-3.5 shrink-0" />
      <span>{warning}</span>
    </div>
  );
}

/** 内置四家（与 Rust `builtin_providers()` 的 id 一一对应）。 */
const PRESETS = [
  { id: "openai", label: "OpenAI", baseUrl: "https://api.openai.com/v1" },
  { id: "anthropic", label: "Anthropic", baseUrl: "https://api.anthropic.com/v1" },
  { id: "moonshot", label: "Moonshot", baseUrl: "https://api.moonshot.cn/v1" },
  { id: "bedrock", label: "Amazon Bedrock", baseUrl: "" },
] as const;

const PROTOCOL_OPTIONS: { value: ProviderProtocol; label: string }[] = [
  { value: "openai", label: "openai（OpenAI 兼容，Bearer）" },
  { value: "anthropic", label: "anthropic（x-api-key）" },
  { value: "bedrock", label: "bedrock（AWS）" },
];

/**
 * AI 引擎设置页。两个 Tab：
 * 1. 第三方模型提供商：从内置四家里选一个，填 Key；
 * 2. 自定义模型 API：自己的 baseUrl + 协议 + Key，并能真实拉 `/models` 列表来勾选。
 *
 * 安全约定：Key 只经 `set_provider_key` 进系统凭据库，界面只能拿到 `hasKey`。
 */
export function AiSettingsTab() {
  const loaded = useSettingsStore((s) => s.loaded);
  const loading = useSettingsStore((s) => s.loading);
  const load = useSettingsStore((s) => s.load);
  const error = useSettingsStore((s) => s.error);

  useEffect(() => {
    if (!loaded) void load();
  }, [loaded, load]);

  if (!isTauriRuntime()) {
    return (
      <div className="rounded border border-amber-200 bg-amber-50 px-3 py-2 text-xs text-amber-800">
        当前是浏览器预览环境，没有 Rust 后端。请用 <code className="font-mono">npm run tauri dev</code>{" "}
        启动桌面应用后再配置提供商。
      </div>
    );
  }

  return (
    <div className="space-y-3">
      {error && (
        <div className="rounded border border-rose-200 bg-rose-50 px-2 py-1.5 text-[11px] text-rose-700">
          {error}
        </div>
      )}

      <OpsSection />

      <OpenCodeEngineSection />

      <OpenCodeStorageNotice />

      <Tabs defaultValue="preset">
        <TabsList>
          <TabsTrigger value="preset">第三方模型提供商</TabsTrigger>
          <TabsTrigger value="custom">自定义模型 API</TabsTrigger>
        </TabsList>

        <TabsContent value="preset">
          {loading && !loaded ? (
            <div className="py-6 text-center text-xs text-ink-400">正在读取设置…</div>
          ) : (
            <PresetPanel />
          )}
        </TabsContent>

        <TabsContent value="custom">
          <CustomPanel />
        </TabsContent>
      </Tabs>
    </div>
  );
}

// ---------------------------------------------------------------- Tab 1

function PresetPanel() {
  const settings = useSettingsStore((s) => s.view?.settings ?? null);
  const secretBackend = useSettingsStore((s) => s.view?.secretBackend ?? "");
  const setActiveProvider = useSettingsStore((s) => s.setActiveProvider);
  const saveKey = useSettingsStore((s) => s.saveKey);
  const clearKey = useSettingsStore((s) => s.clearKey);
  const keyState = useSettingsStore((s) => s.keyState);
  const saving = useSettingsStore((s) => s.saving);

  const [key, setKey] = useState("");

  const activeId = settings?.activeProviderId ?? "anthropic";
  const providers = settings?.providers ?? [];
  const active = providers.find((p) => p.id === activeId) ?? null;
  const state = active ? keyState(active.id) : undefined;
  const presetIds = PRESETS.map((p) => p.id);
  const activeIsPreset = active ? presetIds.includes(active.id as (typeof presetIds)[number]) : false;

  return (
    <div className="space-y-3">
      <div className="space-y-1.5">
        <Label htmlFor="provider-preset">模型提供商</Label>
        <Select
          value={activeIsPreset ? activeId : undefined}
          onValueChange={(value) => void setActiveProvider(value)}
        >
          <SelectTrigger id="provider-preset">
            <SelectValue placeholder="选择一个提供商" />
          </SelectTrigger>
          <SelectContent>
            {PRESETS.map((preset) => {
              const provider = providers.find((p) => p.id === preset.id);
              return (
                <SelectItem key={preset.id} value={preset.id}>
                  {preset.label}
                  {provider ? "" : "（未创建）"}
                </SelectItem>
              );
            })}
          </SelectContent>
        </Select>
        <p className="text-[11px] leading-relaxed text-ink-400">
          选中的提供商在启动引擎时决定注入的环境变量名；没填 Key 时不会注入任何变量，沿用 CLI 自身登录态。
          {!activeIsPreset && " 当前激活的是自定义提供商，可在「自定义模型 API」里查看。"}
        </p>
      </div>

      {active && (
        <>
          <section className="space-y-1.5">
            <div className="flex items-center gap-2">
              <Label htmlFor="provider-key">API 密钥</Label>
              <Badge tone={state?.hasKey ? "ok" : "neutral"}>
                {state?.hasKey ? "已保存" : "未保存"}
              </Badge>
              {secretBackend && (
                <span className="font-mono text-[10px] text-ink-400">后端：{secretBackend}</span>
              )}
            </div>
            <div className="flex items-center gap-1.5">
              <Input
                id="provider-key"
                type="password"
                autoComplete="off"
                spellCheck={false}
                value={key}
                onChange={(e) => setKey(e.target.value)}
                placeholder={active.protocol === "bedrock" ? "AWS Bearer Token" : "sk-…"}
                className="h-8 text-xs"
              />
              <Button
                size="sm"
                disabled={saving || key.trim().length === 0}
                onClick={() => void saveKey(active.id, key).then(() => setKey(""))}
              >
                <KeyRound className="h-3.5 w-3.5" /> 保存
              </Button>
              {state?.hasKey && (
                <Button size="sm" variant="ghost" disabled={saving} onClick={() => void clearKey(active.id)}>
                  清除
                </Button>
              )}
            </div>
            <p className="text-[11px] leading-relaxed text-ink-400">
              Key 存进系统凭据库（account = {active.id}），运行时只作为子进程环境变量注入；界面与日志里都不会出现明文。
            </p>
          </section>

          {state?.supportsEnv && !state.hasKey && (
            <div className="flex items-start gap-1.5 rounded-md border border-amber-200 bg-amber-50 px-2.5 py-2 text-[11px] leading-relaxed text-amber-800">
              <AlertTriangle className="mt-0.5 h-3.5 w-3.5 shrink-0" />
              <span>
                未检测到环境变量。请填写 API 密钥，或设置环境变量{" "}
                <code className="font-mono">{state.keyEnv}</code>。
              </span>
            </div>
          )}

          <ModelCatalog provider={active} />
        </>
      )}

      <ProviderTable />
    </div>
  );
}

function ProviderTable() {
  const settings = useSettingsStore((s) => s.view?.settings ?? null);
  const keyState = useSettingsStore((s) => s.keyState);
  const setActiveProvider = useSettingsStore((s) => s.setActiveProvider);
  const deleteProvider = useSettingsStore((s) => s.deleteProvider);
  const saving = useSettingsStore((s) => s.saving);

  const [pending, setPending] = useState<ProviderConfig | null>(null);

  const providers = settings?.providers ?? [];
  if (providers.length === 0) return null;

  return (
    <section className="space-y-1.5">
      <div className="flex items-center gap-2">
        <h4 className="text-xs font-medium text-ink-700">全部提供商</h4>
        <span className="text-[10px] text-ink-400">{providers.length} 个</span>
      </div>

      <div className="overflow-hidden rounded-md border border-ink-200">
        <table className="w-full border-collapse text-left">
          <thead className="bg-ink-50 text-[10px] uppercase tracking-wide text-ink-500">
            <tr>
              <th className="px-3 py-1.5 font-medium">显示名称</th>
              <th className="px-2 py-1.5 font-medium">Provider ID</th>
              <th className="px-2 py-1.5 font-medium">Key</th>
              <th className="px-2 py-1.5 font-medium">协议</th>
              <th className="px-2 py-1.5 font-medium">模型</th>
              <th className="px-2 py-1.5 text-right font-medium">操作</th>
            </tr>
          </thead>
          <tbody className="divide-y divide-ink-100">
            {providers.map((p) => {
              const state = keyState(p.id);
              const active = settings?.activeProviderId === p.id;
              return (
                <tr key={p.id} className={cn("text-xs", active && "bg-accent-soft/50")}>
                  <td className="max-w-[9rem] truncate px-3 py-1.5 font-medium text-ink-800" title={p.name}>
                    {p.name}
                  </td>
                  <td className="max-w-[8rem] truncate px-2 py-1.5 font-mono text-[10px] text-ink-500" title={p.id}>
                    {p.id}
                  </td>
                  <td className="px-2 py-1.5">
                    <Badge tone={state?.hasKey ? "ok" : "neutral"}>{state?.hasKey ? "有 Key" : "无 Key"}</Badge>
                  </td>
                  <td className="px-2 py-1.5 font-mono text-[10px] text-ink-500">{p.protocol}</td>
                  <td
                    className="max-w-[10rem] truncate px-2 py-1.5 font-mono text-[10px] text-ink-500"
                    title={`使用中模型：${p.activeModel ?? p.models[0] ?? "无"}（目录共 ${p.models.length} 个，可在下方模型目录里点选）`}
                  >
                    {p.models.length > 0 ? (p.activeModel ?? p.models[0]) : "—"}
                    <span className="ml-1 font-sans text-ink-400">({p.models.length})</span>
                  </td>
                  <td className="px-2 py-1.5">
                    <div className="flex items-center justify-end gap-1">
                      {active ? (
                        <span className="inline-flex items-center rounded-full border border-accent/40 bg-accent-soft px-2 py-0.5 text-[10px] font-medium text-accent-strong">
                          当前
                        </span>
                      ) : (
                        <Button
                          size="sm"
                          variant="ghost"
                          className="h-6 px-1.5 text-[11px]"
                          disabled={saving}
                          onClick={() => void setActiveProvider(p.id)}
                        >
                          设为当前
                        </Button>
                      )}
                      <Button
                        size="sm"
                        variant="ghost"
                        className="h-6 px-1.5 text-[11px] text-rose-600 hover:bg-rose-50 hover:text-rose-700"
                        disabled={saving}
                        onClick={() => setPending(p)}
                        title="删除这个提供商"
                      >
                        <Trash2 className="h-3 w-3" /> 删除
                      </Button>
                    </div>
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
      </div>

      <ConfirmDialog
        open={pending !== null}
        title="删除提供商"
        danger
        confirmText="删除"
        message={
          <>
            确定要删除提供商【{pending?.name}】吗？其对应的 API Key 也将被永久清除。
          </>
        }
        onConfirm={async () => {
          const target = pending;
          setPending(null);
          if (target) await deleteProvider(target.id);
        }}
        onClose={() => setPending(null)}
      />
    </section>
  );
}

function ModelCatalog({ provider }: { provider: ProviderConfig }) {
  const removeModel = useSettingsStore((s) => s.removeModel);
  const setActiveModel = useSettingsStore((s) => s.setActiveModel);
  const saving = useSettingsStore((s) => s.saving);
  // 使用中：用户选的那个优先；没选回退目录第一个（与引擎 model_id 口径一致）。
  const active = provider.activeModel ?? provider.models[0] ?? null;

  return (
    <section className="space-y-1.5">
      <div className="flex items-center gap-2">
        <h4 className="text-xs font-medium text-ink-700">模型目录</h4>
        <span className="text-[10px] text-ink-400">{provider.models.length} 个</span>
        {active && (
          <span className="ml-auto max-w-[16rem] truncate font-mono text-[10px] text-accent-strong" title={active}>
            使用中：{active}
          </span>
        )}
      </div>
      <div className="rounded-md border border-ink-200 px-2.5 py-2">
        {provider.models.length === 0 ? (
          <div className="text-[11px] text-ink-400">
            还没添加模型。去「自定义模型 API」用「获取可用模型」拉一次，或手动输入模型 ID。
          </div>
        ) : (
          <>
            <div className="flex flex-wrap gap-1">
              {provider.models.map((model) => {
                const isActive = model === active;
                return (
                  <button
                    key={model}
                    type="button"
                    disabled={saving}
                    title={isActive ? "再点一次取消选择（回退目录第一个）" : "点击把这个模型设为「使用中」"}
                    onClick={() => void setActiveModel(provider.id, isActive ? null : model)}
                    className={cn(
                      "inline-flex items-center gap-1 rounded border px-1.5 py-0.5 font-mono text-[10px] transition-colors",
                      isActive
                        ? "border-accent/60 bg-accent-soft text-accent-strong"
                        : "border-ink-200 bg-ink-50 text-ink-600 hover:border-accent/40 hover:text-accent-strong",
                    )}
                  >
                    {isActive && <span className="font-sans font-medium">使用中</span>}
                    {model}
                    <span
                      role="button"
                      tabIndex={-1}
                      aria-label={`移除模型 ${model}`}
                      title="从目录里移除"
                      onClick={(e) => {
                        e.stopPropagation();
                        void removeModel(provider.id, model);
                      }}
                      className="rounded p-0.5 text-ink-400 hover:bg-rose-50 hover:text-rose-600"
                    >
                      <X className="h-2.5 w-2.5" />
                    </span>
                  </button>
                );
              })}
            </div>
            <p className="mt-1.5 text-[10px] leading-relaxed text-ink-400">
              点目录里的模型即设为该提供商「使用中」的模型——引擎实际调用的就是它；再点一次取消选择，回退到目录第一个。
            </p>
          </>
        )}
      </div>
    </section>
  );
}

// ---------------------------------------------------------------- Tab 2

function CustomPanel() {
  const providers = useSettingsStore((s) => s.view?.settings.providers ?? []);
  const upsertProvider = useSettingsStore((s) => s.upsertProvider);
  const saveKey = useSettingsStore((s) => s.saveKey);
  const saving = useSettingsStore((s) => s.saving);

  const [form, setForm] = useState({
    id: "",
    name: "",
    baseUrl: "",
    protocol: "openai" as ProviderProtocol,
    key: "",
  });
  const [manualModel, setManualModel] = useState("");
  const [pendingModels, setPendingModels] = useState<string[]>([]);
  const [fetching, setFetching] = useState(false);
  const [fetchError, setFetchError] = useState<string | null>(null);
  const [pickerOpen, setPickerOpen] = useState(false);
  const [fetched, setFetched] = useState<string[]>([]);
  const [formError, setFormError] = useState<string | null>(null);
  const [okHint, setOkHint] = useState<string | null>(null);

  const idValid = useMemo(() => /^[a-z0-9][a-z0-9_-]*$/.test(form.id.trim()), [form.id]);

  const patch = (next: Partial<typeof form>) => {
    setForm((prev) => ({ ...prev, ...next }));
    setFormError(null);
    setOkHint(null);
  };

  const addManual = () => {
    const id = manualModel.trim();
    if (!id) return;
    if (pendingModels.includes(id)) return;
    setPendingModels((prev) => [...prev, id]);
    setManualModel("");
  };

  const fetchModels = async () => {
    setFetchError(null);
    setFetching(true);
    try {
      const list = await api.fetchAvailableModels(
        form.baseUrl.trim(),
        form.protocol,
        form.key.trim() ? form.key.trim() : null,
      );
      setFetched(list);
      setPickerOpen(true);
    } catch (err) {
      setFetchError(formatError(err));
    } finally {
      setFetching(false);
    }
  };

  const create = async () => {
    const id = form.id.trim();
    const name = form.name.trim() || id;
    const baseUrl = form.baseUrl.trim();

    if (!idValid) {
      setFormError("Provider ID 只能用小写字母、数字、- 和 _，且以字母或数字开头");
      return;
    }
    if (form.protocol !== "bedrock" && !baseUrl) {
      setFormError("API 地址不能为空");
      return;
    }

    const provider: ProviderConfig = {
      id,
      name,
      baseUrl,
      protocol: form.protocol,
      models: pendingModels,
    };

    await upsertProvider(provider);
    if (form.key.trim()) await saveKey(id, form.key.trim());

    setOkHint(`已创建提供商 ${name}（${id}），可在上面的「第三方模型提供商」里设为当前。`);
    setForm({ id: "", name: "", baseUrl: "", protocol: "openai", key: "" });
    setPendingModels([]);
  };

  const knownIds = providers.map((p) => p.id);

  return (
    <div className="space-y-3">
      <div className="grid grid-cols-2 gap-2">
        <div className="space-y-1">
          <Label htmlFor="custom-id">Provider ID（小写字母）</Label>
          <Input
            id="custom-id"
            value={form.id}
            onChange={(e) => patch({ id: e.target.value.toLowerCase() })}
            placeholder="my-gateway"
            className={cn("h-8 font-mono text-xs", form.id && !idValid && "border-rose-300")}
          />
          {knownIds.includes(form.id.trim()) && (
            <div className="text-[10px] text-amber-700">这个 id 已存在，创建会覆盖它的地址与模型目录。</div>
          )}
        </div>
        <div className="space-y-1">
          <Label htmlFor="custom-name">显示名称</Label>
          <Input
            id="custom-name"
            value={form.name}
            onChange={(e) => patch({ name: e.target.value })}
            placeholder="我的中转网关"
            className="h-8 text-xs"
          />
        </div>
      </div>

      <div className="space-y-1">
        <Label htmlFor="custom-base">API 地址</Label>
        <Input
          id="custom-base"
          value={form.baseUrl}
          onChange={(e) => patch({ baseUrl: e.target.value })}
          placeholder="https://api.example.com/v1"
          className="h-8 font-mono text-xs"
        />
      </div>

      <div className="space-y-1">
        <Label htmlFor="custom-protocol">API 协议</Label>
        <Select
          value={form.protocol}
          onValueChange={(value) => patch({ protocol: value as ProviderProtocol })}
        >
          <SelectTrigger id="custom-protocol" className="h-8 text-xs">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {PROTOCOL_OPTIONS.map((option) => (
              <SelectItem key={option.value} value={option.value}>
                {option.label}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </div>

      <div className="space-y-1">
        <Label htmlFor="custom-key">API 密钥</Label>
        <Input
          id="custom-key"
          type="password"
          autoComplete="off"
          spellCheck={false}
          value={form.key}
          onChange={(e) => patch({ key: e.target.value })}
          placeholder="sk-…（只进系统凭据库，不写进 settings.json）"
          className="h-8 text-xs"
        />
      </div>

      <section className="space-y-1.5 rounded-md border border-ink-200 px-2.5 py-2">
        <div className="flex items-center gap-2">
          <h4 className="text-xs font-medium text-ink-700">模型目录</h4>
          <Button
            size="sm"
            variant="link"
            className="ml-auto h-6 px-1 text-[11px]"
            disabled={fetching || (form.protocol !== "bedrock" && !form.baseUrl.trim())}
            onClick={() => void fetchModels()}
          >
            {fetching ? (
              <>
                <Loader2 className="h-3 w-3 animate-spin" /> 获取中...
              </>
            ) : (
              "获取可用模型"
            )}
          </Button>
        </div>

        {fetchError && (
          <div className="rounded border border-rose-200 bg-rose-50 px-2 py-1 text-[11px] text-rose-700">
            {fetchError}
          </div>
        )}

        <div className="flex items-center gap-1.5">
          <Input
            value={manualModel}
            onChange={(e) => setManualModel(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                e.preventDefault();
                addManual();
              }
            }}
            placeholder="手动输入模型 ID，回车添加"
            className="h-8 font-mono text-xs"
          />
          <Button size="sm" variant="secondary" disabled={!manualModel.trim()} onClick={addManual}>
            <Plus className="h-3.5 w-3.5" /> 添加
          </Button>
        </div>

        <div className="flex flex-wrap gap-1">
          {pendingModels.length === 0 ? (
            <span className="text-[11px] text-ink-400">还没有选中任何模型</span>
          ) : (
            pendingModels.map((model) => (
              <span
                key={model}
                className="inline-flex items-center gap-1 rounded border border-ink-200 bg-ink-50 px-1.5 py-0.5 font-mono text-[10px] text-ink-600"
              >
                {model}
                <button
                  type="button"
                  aria-label={`移除模型 ${model}`}
                  onClick={() => setPendingModels((prev) => prev.filter((m) => m !== model))}
                  className="rounded p-0.5 text-ink-400 hover:bg-rose-50 hover:text-rose-600"
                  title="移除"
                >
                  <X className="h-2.5 w-2.5" />
                </button>
              </span>
            ))
          )}
        </div>
      </section>

      {formError && (
        <div className="rounded border border-rose-200 bg-rose-50 px-2 py-1 text-[11px] text-rose-700">
          {formError}
        </div>
      )}
      {okHint && (
        <div className="rounded border border-emerald-200 bg-emerald-50 px-2 py-1 text-[11px] text-emerald-700">
          {okHint}
        </div>
      )}

      <div className="flex items-center gap-2">
        <Button disabled={saving || !idValid} onClick={() => void create()}>
          创建提供商
        </Button>
        <span className="text-[11px] text-ink-400">
          Key 与提供商分开存：创建后 Key 只留在系统凭据库，`settings.json` 里只有 id / 地址 / 协议 / 模型目录。
        </span>
      </div>

      <ModelPickerDialog
        open={pickerOpen}
        onClose={() => setPickerOpen(false)}
        models={fetched}
        already={pendingModels}
        source={form.baseUrl.trim() ? `${form.baseUrl.trim().replace(/\/+$/, "")}/models` : undefined}
        onConfirm={(selected) => setPendingModels((prev) => [...prev, ...selected])}
      />
    </div>
  );
}
