import { useEffect, useState } from "react";

import { getVersion } from "@tauri-apps/api/app";

import { Button } from "@/components/common/Button";
import { Dialog } from "@/components/common/Dialog";
import { Badge } from "@/components/common/Field";
import { AgentRolesTab } from "@/components/settings/AgentRolesTab";
import { AiSettingsTab } from "@/components/settings/AiSettingsTab";
import { cn } from "@/lib/cn";
import { api, isTauriRuntime } from "@/lib/ipc";
import { useAiStore } from "@/store/aiStore";
import { useProjectStore } from "@/store/projectStore";
import type { ToolStatus } from "@/types";

interface SettingsDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
}

type Tab = "env" | "ai" | "agents" | "about";

export function SettingsDialog({ open, onOpenChange }: SettingsDialogProps) {
  const [tab, setTab] = useState<Tab>("env");
  const env = useProjectStore((s) => s.env);
  const envLoading = useProjectStore((s) => s.envLoading);
  const loadEnv = useProjectStore((s) => s.loadEnv);

  useEffect(() => {
    if (open && tab === "env" && !env && !envLoading) void loadEnv();
  }, [open, tab, env, envLoading, loadEnv]);

  return (
    <Dialog
      open={open}
      onClose={() => onOpenChange(false)}
      title="设置"
      width="lg"
      footer={
        <>
          {tab === "env" && (
            <Button variant="secondary" onClick={() => void loadEnv()} disabled={envLoading}>
              {envLoading ? "检测中…" : "重新检测"}
            </Button>
          )}
          <Button variant="primary" onClick={() => onOpenChange(false)}>
            关闭
          </Button>
        </>
      }
    >
      <div className="flex gap-5">
        <nav className="w-32 shrink-0 space-y-0.5">
          <TabButton active={tab === "env"} onClick={() => setTab("env")}>
            环境自检
          </TabButton>
          <TabButton active={tab === "ai"} onClick={() => setTab("ai")}>
            AI 引擎
          </TabButton>
          <TabButton active={tab === "agents"} onClick={() => setTab("agents")}>
            智能体角色
          </TabButton>
          <TabButton active={tab === "about"} onClick={() => setTab("about")}>
            关于
          </TabButton>
        </nav>

        <div className="min-w-0 flex-1">
          {tab === "env" && <EnvTab />}
          {tab === "ai" && <AiTab />}
          {tab === "agents" && <AgentsTab />}
          {tab === "about" && <AboutTab />}
        </div>
      </div>
    </Dialog>
  );
}

function TabButton({
  active,
  onClick,
  children,
}: {
  active: boolean;
  onClick: () => void;
  children: React.ReactNode;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={cn(
        "w-full rounded-md px-2.5 py-1.5 text-left text-xs font-medium",
        active ? "bg-accent-soft text-accent-strong" : "text-ink-600 hover:bg-ink-100",
      )}
    >
      {children}
    </button>
  );
}

function EnvTab() {
  const env = useProjectStore((s) => s.env);
  const envLoading = useProjectStore((s) => s.envLoading);
  const loadEnv = useProjectStore((s) => s.loadEnv);
  const [installing, setInstalling] = useState<string | null>(null);
  const [installNote, setInstallNote] = useState<string | null>(null);

  const extract = (e: unknown): string => {
    if (e instanceof Error) return e.message;
    if (typeof e === "string") return e;
    if (e && typeof e === "object") {
      const obj = e as Record<string, unknown>;
      const msg = obj.error ?? obj.message;
      if (typeof msg === "string") return msg;
    }
    return String(e);
  };

  /** 下载宿主 Agent CLI（opencode，GitHub Releases 走代理/镜像兜底）。 */
  const installEngine = async () => {
    setInstalling("opencode");
    setInstallNote("准备下载…");
    try {
      const unlisten = await api.onOpencodeProgress((p) => {
        const mb = (n: number) => `${(n / 1024 / 1024).toFixed(1)}MB`;
        const total = p.total ? ` / ${mb(p.total)}` : "";
        setInstallNote(`${p.note ?? "下载中"}：${mb(p.downloaded)}${total}`);
      });
      const path = await api.engineInstallOpencode();
      unlisten();
      setInstallNote(`已安装：${path}`);
      await loadEnv();
    } catch (err) {
      setInstallNote(`安装失败：${extract(err)}`);
    } finally {
      setInstalling(null);
    }
  };

  /** 下载运行时依赖（node/python/gitbash，国内镜像），进度实时显示。 */
  const installRuntime = async (tool: "node" | "python" | "gitbash") => {
    setInstalling(tool);
    setInstallNote("准备下载…");
    try {
      const unlisten = await api.onSetupProgress((p) => {
        if (p.tool !== tool) return;
        const mb = (n: number) => `${(n / 1024 / 1024).toFixed(1)}MB`;
        const total = p.total ? ` / ${mb(p.total)}` : "";
        setInstallNote(`${p.note ?? "下载中"}：${mb(p.downloaded)}${total}`);
      });
      const path = await api.runtimeInstall(tool);
      unlisten();
      setInstallNote(`已安装：${path}`);
      await loadEnv();
    } catch (err) {
      setInstallNote(`安装失败：${extract(err)}`);
    } finally {
      setInstalling(null);
    }
  };

  if (!isTauriRuntime()) {
    return (
      <div className="rounded border border-amber-200 bg-amber-50 px-3 py-2 text-xs text-amber-800">
        当前是浏览器预览环境，没有 Rust 后端。请用 <code className="font-mono">npm run tauri dev</code>{" "}
        启动桌面应用后再自检。
      </div>
    );
  }

  if (!env) {
    return (
      <div className="py-6 text-center text-xs text-ink-400">
        {envLoading ? "正在检测 node / python / bash / 宿主 CLI…" : "尚未检测"}
      </div>
    );
  }

  const rows: ToolStatus[] = [env.node, env.python, env.bash, env.engine];

  return (
    <div className="space-y-3">
      <div
        className={cn(
          "flex items-center justify-between gap-3 rounded-md border px-3 py-2",
          env.allOk
            ? "border-emerald-200 bg-emerald-50"
            : "border-amber-200 bg-amber-50",
        )}
      >
        <div className="text-xs">
          <div className="font-medium text-ink-800">
            {env.allOk ? "写作必需依赖齐备" : "缺少写作必需依赖"}
          </div>
          <div className="mt-0.5 text-[11px] text-ink-500">
            必需：Node.js 18+ / Python 3 / bash（Git Bash）。宿主 CLI 只影响 AI 生成，不影响建项目与编辑。
          </div>
        </div>
        <Badge tone={env.allOk ? "ok" : "warn"}>
          {env.allOk ? "OK" : "待处理"}
        </Badge>
      </div>

      <div className="divide-y divide-ink-100 rounded-md border border-ink-200">
        {rows.map((row) => (
          <ToolRow key={row.tool} row={row} />
        ))}
      </div>

      {/* 缺失依赖一键下载（国内镜像，免安装版，装到应用目录不污染系统） */}
      <div className="flex flex-wrap items-center gap-2">
        {(
          [
            ["node", "下载 Node.js", env.node.ok],
            ["python", "下载 Python", env.python.ok],
            ["gitbash", "下载 Git Bash", env.bash.ok],
          ] as const
        ).map(([tool, label, ok]) => (
          <Button
            key={tool}
            size="sm"
            variant={ok ? "secondary" : "primary"}
            onClick={() => void installRuntime(tool)}
            disabled={installing !== null}
          >
            {installing === tool ? "下载中…" : ok ? `${label}（备用）` : label}
          </Button>
        ))}
        {/* 宿主 CLI 与三个运行时依赖并排：opencode 是 AI 生成的前提，缺失时置顶为主按钮 */}
        <Button
          size="sm"
          variant={env.engine.ok ? "secondary" : "primary"}
          onClick={() => void installEngine()}
          disabled={installing !== null}
        >
          {installing === "opencode" ? "下载中…" : env.engine.ok ? "下载 opencode CLI（备用）" : "下载 opencode CLI（宿主 Agent CLI）"}
        </Button>
      </div>
      {installNote && (
        <div className="rounded border border-ink-200 bg-ink-50/60 px-2 py-1.5 text-[11px] text-ink-600">
          {installNote}
        </div>
      )}
      <p className="text-[10px] leading-relaxed text-ink-400">
        Node/Python/Git Bash 从国内镜像（npmmirror / 华为云）下载免安装版；opencode CLI 从 GitHub
        下载（自动识别系统代理/常见代理端口，失败再走镜像）。都装到
        <span className="font-mono"> %LOCALAPPDATA%\storydesk\</span>
        ，不写系统 PATH、不污染系统；应用会自动识别已安装的版本。系统里已装过的优先用系统的。
      </p>

      <div className="text-[11px] text-ink-400">
        {env.platform} · 检测时间 {formatTs(env.checkedAt)}
      </div>
    </div>
  );
}

function ToolRow({ row }: { row: ToolStatus }) {
  const label: Record<string, string> = {
    node: "Node.js",
    python: "Python",
    bash: "bash（Git Bash）",
    engine: "宿主 Agent CLI",
  };

  return (
    <div className="px-3 py-2">
      <div className="flex items-center gap-2">
        <span className="w-28 shrink-0 text-xs font-medium text-ink-800">
          {label[row.tool] ?? row.tool}
        </span>
        <Badge tone={row.ok ? "ok" : "error"}>{row.ok ? "可用" : "缺失"}</Badge>
        {row.version && (
          <span className="truncate font-mono text-[11px] text-ink-500">{row.version}</span>
        )}
        <span className="ml-auto shrink-0 text-[10px] text-ink-300">
          {row.tool === "engine" ? "AI 生成" : "必需"}
        </span>
      </div>

      {row.path && (
        <div className="mt-1 break-all font-mono text-[10px] text-ink-400">{row.path}</div>
      )}

      {row.hint && (
        <div
          className={cn(
            "mt-1.5 rounded px-2 py-1.5 text-[11px] leading-relaxed",
            row.ok ? "bg-ink-50 text-ink-600" : "bg-amber-50 text-amber-800",
          )}
        >
          {row.hint}
        </div>
      )}

      {row.detail && (
        <details className="mt-1">
          <summary className="cursor-pointer text-[10px] text-ink-400 hover:text-ink-600">
            探测细节
          </summary>
          <pre className="mt-1 max-h-28 overflow-auto whitespace-pre-wrap break-all rounded bg-ink-950/90 px-2 py-1.5 font-mono text-[10px] text-ink-100">
            {row.detail}
          </pre>
        </details>
      )}
    </div>
  );
}

function AboutTab() {
  // 版本号只有一个来源：src-tauri/tauri.conf.json 里的 version（打包时写进 exe，
  // 运行时由 Tauri 的 getVersion() 从应用元数据读回来）。这里以前写死 v1.0.0，
  // 装的是 1.0.7 也照样显示 1.0.0，每升一版都得记得来手改一次——现在自动跟随。
  const [version, setVersion] = useState<string | null>(null);
  useEffect(() => {
    let alive = true;
    void getVersion()
      .then((v) => {
        if (alive) setVersion(v);
      })
      .catch(() => {
        if (alive) setVersion(null);
      });
    return () => {
      alive = false;
    };
  }, []);
  return (
    <div className="space-y-3 text-xs leading-relaxed text-ink-600">
      <div>
        <div className="text-sm font-semibold text-ink-900">野菜·AI小说工作台</div>
        <div className="mt-0.5 font-mono text-[11px] text-ink-400" data-testid="about-version">
          {version ? `v${version}` : "v—"}
        </div>
        <div className="mt-0.5 text-[11px] text-ink-500">
          作者：<span className="font-medium text-ink-700">野菜</span>（GitHub：
          <span className="font-mono">yecai12138</span>）
        </div>
      </div>
      <ul className="space-y-1.5">
        <li>· 写作流程来自 <span className="font-mono">zenstory-ai/oh-story-claudecode</span>（skill 包，需装在宿主 Agent 里）。</li>
        <li>· AI 生成由 Rust 侧 spawn 宿主 CLI 子进程完成，stdout 按行解析后以 <span className="font-mono">ai://chunk</span> 推给界面。</li>
        <li>· 渲染进程没有任何 fs/shell/http 权限，所有磁盘操作走 invoke → Rust 命令 → path guard。</li>
        <li>· 每个提供商的 API Key 存在系统凭据库（account= provider id），只以子进程环境变量注入；<span className="font-mono">settings.json</span> 里没有明文。</li>
        <li>· 写完正文会自动跑后置体检，结果以 <span className="font-mono">ai://postcheck</span> 推给界面。</li>
        <li>· 应用私有数据写在项目根的 <span className="font-mono">.storydesk/</span>，不碰 oh-story 的 <span className="font-mono">.story/</span>。</li>
      </ul>
    </div>
  );
}

function formatTs(seconds: string): string {
  const n = Number(seconds);
  if (!Number.isFinite(n) || n <= 0) return "未知";
  return new Date(n * 1000).toLocaleTimeString();
}

/** AI 引擎页：引擎选择（claude / opencode）+ 下载 opencode + oh-story 脚本链 + 提供商配置（`AiSettingsTab`）。 */
function AiTab() {
  const engines = useAiStore((s) => s.engines);
  const engine = useAiStore((s) => s.engine);
  const setEngine = useAiStore((s) => s.setEngine);
  const gate = useAiStore((s) => s.gate);
  const error = useAiStore((s) => s.error);
  const loadEngines = useAiStore((s) => s.loadEngines);
  const loadGate = useAiStore((s) => s.loadGate);
  const workspaceDir = useProjectStore((s) => s.workspaceDir);
  const [installing, setInstalling] = useState(false);
  const [installNote, setInstallNote] = useState<string | null>(null);

  useEffect(() => {
    void loadEngines();
    if (workspaceDir) void loadGate();
  }, [loadEngines, loadGate, workspaceDir]);

  if (!isTauriRuntime()) {
    return (
      <div className="rounded border border-amber-200 bg-amber-50 px-3 py-2 text-xs text-amber-800">
        当前是浏览器预览环境，没有 Rust 后端，引擎与门禁状态不可用。
      </div>
    );
  }

  const installOpencode = async () => {
    setInstalling(true);
    setInstallNote("准备下载…");
    try {
      const unlisten = await api.onOpencodeProgress((p) => {
        const mb = (n: number) => `${(n / 1024 / 1024).toFixed(1)}MB`;
        const total = p.total ? ` / ${mb(p.total)}` : "";
        setInstallNote(`${p.note ?? "下载中"}：${mb(p.downloaded)}${total}`);
      });
      const path = await api.engineInstallOpencode();
      unlisten();
      setInstallNote(`已安装：${path}`);
      await loadEngines();
    } catch (err) {
      // Rust 侧的错误是 { error?: string } 或 { message?: string }，也可能直接是字符串；
      // 逐层取值，别把对象拼成 "[object Object]"。
      const extract = (e: unknown): string => {
        if (e instanceof Error) return e.message;
        if (typeof e === "string") return e;
        if (e && typeof e === "object") {
          const obj = e as Record<string, unknown>;
          const msg = obj.error ?? obj.message;
          if (typeof msg === "string") return msg;
          try {
            return JSON.stringify(e);
          } catch {
            return String(e);
          }
        }
        return String(e);
      };
      setInstallNote(`安装失败：${extract(err)}`);
    } finally {
      setInstalling(false);
    }
  };

  return (
    <div className="space-y-3">
      {error && (
        <div className="rounded border border-rose-200 bg-rose-50 px-2 py-1.5 text-[11px] text-rose-700">
          {error}
        </div>
      )}

      <section className="space-y-1.5">
        <h4 className="text-xs font-medium text-ink-700">引擎</h4>
        <div className="divide-y divide-ink-100 rounded-md border border-ink-200">
          {engines.map((e) => (
            <div key={e.kind} className="px-3 py-2">
              <div className="flex items-center gap-2">
                <label className="flex cursor-pointer items-center gap-2">
                  <input
                    type="radio"
                    name="engine-kind"
                    checked={engine === e.kind}
                    onChange={() => void setEngine(e.kind)}
                    disabled={!e.available}
                    className="h-3 w-3 accent-emerald-600"
                  />
                  <span className="w-32 shrink-0 text-xs font-medium text-ink-800">{e.name}</span>
                </label>
                <Badge tone={e.available ? "ok" : "warn"}>{e.available ? "可用" : "不可用"}</Badge>
                {e.kind === "opencode" && !e.available && (
                  <Button
                    size="sm"
                    variant="secondary"
                    className="ml-auto"
                    onClick={() => void installOpencode()}
                    disabled={installing}
                  >
                    {installing ? "下载中…" : "下载 opencode"}
                  </Button>
                )}
              </div>
              <div className="mt-1 break-all text-[11px] leading-relaxed text-ink-500">{e.detail}</div>
            </div>
          ))}
          {engines.length === 0 && (
            <div className="px-3 py-3 text-[11px] text-ink-400">正在探测…</div>
          )}
        </div>
        {installNote && (
          <div className="rounded border border-ink-200 bg-ink-50/60 px-2 py-1.5 text-[11px] text-ink-600">
            {installNote}
          </div>
        )}
        <p className="text-[10px] leading-relaxed text-ink-400">
          自定义模型 API（OpenAI 协议，如 DeepSeek）请选 opencode 引擎：baseURL 会写进书根的
          <span className="font-mono"> opencode.json </span>
          （Key 仍只经环境变量注入），联网 webfetch / websearch 默认可用。
          选 claude 但没装 Claude Code 时，只要 opencode 可用就自动改走 opencode。
        </p>
      </section>

      <AiSettingsTab />

      <section className="space-y-1.5">
        <div className="flex items-center gap-2">
          <h4 className="text-xs font-medium text-ink-700">oh-story 脚本链</h4>
          <Badge tone={gate?.ok ? "ok" : "warn"}>{gate?.ok ? "就绪" : "缺失"}</Badge>
          <Button
            size="sm"
            variant="ghost"
            className="ml-auto"
            onClick={() => void loadGate()}
            disabled={!workspaceDir}
          >
            重新探测
          </Button>
        </div>
        <div className="rounded-md border border-ink-200 px-3 py-2 text-[11px] leading-relaxed">
          <Row label="python" value={gate?.python ?? "—"} />
          <Row label="storyctl.py" value={gate?.storyctl ?? "—"} />
          <Row label="build_architect_brief.py" value={gate?.brief ?? "—"} />
          <div className="mt-1.5 border-t border-ink-100 pt-1.5 text-ink-500">{gate?.detail ?? "尚未探测"}</div>
        </div>
      </section>
    </div>
  );
}

function Row({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex gap-2 py-0.5">
      <span className="w-44 shrink-0 font-mono text-[10px] text-ink-400">{label}</span>
      <span className="min-w-0 break-all font-mono text-[10px] text-ink-600">{value}</span>
    </div>
  );
}

/** 智能体角色页：可增删改的「下属名单」，主 AI 按它派活。 */
function AgentsTab() {
  return (
    <div className="space-y-3">
      <div className="rounded-md border border-ink-200 bg-ink-50/60 px-3 py-2 text-[11px] leading-relaxed text-ink-600">
        右侧面板里你只对着「主 AI 总监」说话，它会先把任务拆成计划，再按顺序派给下面这些角色。
        每个角色的「系统提示词」会原样进 prompt 的 <code className="font-mono">角色约束：</code> 那一行，
        「绑定目录」决定它默认在哪个目录干活（同目录可以放多个角色，例如两个写手）。
        改完保存立刻生效，不必重启。
      </div>
      <AgentRolesTab />
    </div>
  );
}
