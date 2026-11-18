import { useState } from "react";
import type { TextareaHTMLAttributes } from "react";

import { Button } from "@/components/common/Button";
import { ConfirmDialog } from "@/components/common/ConfirmDialog";
import { Badge, TextInput } from "@/components/common/Field";
import { cn } from "@/lib/cn";
import { useSettingsStore } from "@/store/settingsStore";
import { TOOL_CHOICES } from "@/types";
import type { AgentConfig } from "@/types";

/** 允许绑定的目录，与 Rust `models::ai::KNOWN_DIRS` 一一对应。 */
const DIRS = ["设定", "大纲", "正文", "对标", "参考资料", "追踪"];

/** 没配过时的默认工具（与 Rust `models::ai::default_tools` 一致）。 */
const DEFAULT_TOOLS = ["Read", "Write"];

const BLANK: AgentConfig = {
  id: "",
  name: "",
  avatar: "🤖",
  systemPrompt: "",
  targetDir: "大纲",
  tools: [],
};

/**
 * 设置 → 智能体角色。
 *
 * 这里编的就是派活时真正进 prompt 的东西：`systemPrompt` 会原样出现在
 * `角色约束：…` 那一行，`targetDir` 决定它默认在哪个目录干活。
 */
export function AgentRolesTab() {
  const agents = useSettingsStore((s) => s.view?.settings.agents ?? []);
  const saving = useSettingsStore((s) => s.saving);
  const error = useSettingsStore((s) => s.error);
  const saveAgents = useSettingsStore((s) => s.saveAgents);
  const resetAgents = useSettingsStore((s) => s.resetAgents);

  const [draft, setDraft] = useState<AgentConfig | null>(null);
  const [isNew, setIsNew] = useState(false);
  const [pendingDelete, setPendingDelete] = useState<AgentConfig | null>(null);

  const startNew = () => {
    setDraft({ ...BLANK, id: `agent-${Math.random().toString(36).slice(2, 7)}` });
    setIsNew(true);
  };

  const startEdit = (agent: AgentConfig) => {
    setDraft({ ...agent, tools: [...agent.tools] });
    setIsNew(false);
  };

  const patch = (fields: Partial<AgentConfig>) =>
    setDraft((prev) => (prev ? { ...prev, ...fields } : prev));

  const commit = async () => {
    if (!draft) return;
    const next = isNew
      ? [...agents, draft]
      : agents.map((a) => (a.id === draft.id ? draft : a));
    await saveAgents(next);
    setDraft(null);
  };

  const remove = async (agent: AgentConfig) => {
    await saveAgents(agents.filter((a) => a.id !== agent.id));
    if (draft?.id === agent.id) setDraft(null);
  };

  return (
    <div className="space-y-3" data-testid="agent-roles">
      <ToolsPanel />

      <div className="flex items-center gap-2">
        <h4 className="text-xs font-medium text-ink-700">智能体角色（{agents.length}）</h4>
        <Badge tone="neutral">主 AI 会按这份名单派活</Badge>
        <div className="ml-auto flex gap-1.5">
          <Button size="sm" variant="ghost" onClick={() => void resetAgents()} disabled={saving}>
            恢复默认 6 个角色
          </Button>
          <Button size="sm" variant="secondary" onClick={startNew} data-testid="agent-new">
            新增角色
          </Button>
        </div>
      </div>

      {error && (
        <div className="rounded border border-rose-200 bg-rose-50 px-2 py-1.5 text-[11px] text-rose-700">
          {error}
        </div>
      )}

      <div className="divide-y divide-ink-100 rounded-md border border-ink-200">
        {agents.map((agent) => (
          <div key={agent.id} className="flex items-start gap-2 px-3 py-2" data-agent-id={agent.id}>
            <span className="mt-0.5 text-base leading-none">{agent.avatar}</span>
            <div className="min-w-0 flex-1">
              <div className="flex items-center gap-2">
                <span className="text-xs font-medium text-ink-800">{agent.name}</span>
                <span className="font-mono text-[10px] text-ink-400">{agent.id}</span>
                <Badge tone="neutral">{agent.targetDir}/</Badge>
                {draft?.id === agent.id && <Badge tone="warn">编辑中</Badge>}
              </div>
              <div className="mt-0.5 line-clamp-2 text-[11px] leading-relaxed text-ink-500">
                {agent.systemPrompt}
              </div>
            </div>
            <div className="flex shrink-0 gap-1">
              <Button size="sm" variant="ghost" onClick={() => startEdit(agent)} data-testid={`agent-edit-${agent.id}`}>
                编辑
              </Button>
              <Button
                size="sm"
                variant="ghost"
                className="text-rose-600 hover:bg-rose-50"
                onClick={() => setPendingDelete(agent)}
                data-testid={`agent-del-${agent.id}`}
              >
                删除
              </Button>
            </div>
          </div>
        ))}
        {agents.length === 0 && (
          <div className="px-3 py-4 text-center text-[11px] text-ink-400">
            还没有角色。点「恢复默认 6 个角色」或者自己新增一个。
          </div>
        )}
      </div>

      {draft && (
        <div className="space-y-2 rounded-md border border-ink-200 bg-ink-50/60 p-3">
          <div className="flex items-center gap-2">
            <span className="text-xs font-medium text-ink-700">{isNew ? "新增角色" : `编辑「${draft.name}」`}</span>
            <span className="font-mono text-[10px] text-ink-400">id: {draft.id}</span>
            <Button size="sm" variant="ghost" className="ml-auto" onClick={() => setDraft(null)}>
              取消
            </Button>
          </div>

          <div className="grid grid-cols-[64px_1fr_1fr] gap-2">
            <label className="block">
              <span className="text-[11px] text-ink-500">Emoji</span>
              <TextInput
                value={draft.avatar}
                onChange={(e) => patch({ avatar: e.target.value })}
                className="mt-0.5"
                data-testid="agent-avatar"
              />
            </label>
            <label className="block">
              <span className="text-[11px] text-ink-500">名称</span>
              <TextInput
                value={draft.name}
                onChange={(e) => patch({ name: e.target.value })}
                placeholder="例如：主笔写手"
                className="mt-0.5"
                data-testid="agent-name"
              />
            </label>
            <label className="block">
              <span className="text-[11px] text-ink-500">绑定目录</span>
              <select
                value={draft.targetDir}
                onChange={(e) => patch({ targetDir: e.target.value })}
                className="mt-0.5 w-full rounded border border-ink-300 bg-white px-2 py-1 text-xs text-ink-800"
                data-testid="agent-dir"
              >
                {DIRS.map((d) => (
                  <option key={d} value={d}>
                    {d}/
                  </option>
                ))}
              </select>
            </label>
          </div>

          <label className="block">
            <span className="text-[11px] text-ink-500">系统提示词（会作为「角色约束」原样进 prompt）</span>
            <TextareaRaw
              value={draft.systemPrompt}
              onChange={(e) => patch({ systemPrompt: e.target.value })}
              rows={5}
              placeholder="例如：每章先写一句「本章卖点」，再写场景；不用「仿佛/似乎」这类虚词。"
              className="mt-0.5 font-mono text-[11px]"
              data-testid="agent-prompt"
            />
          </label>

          <div className="flex items-center gap-2">
            <span className="text-[10px] text-ink-400">
              保存后立刻生效：下一次派活就用新的提示词。
            </span>
            <Button
              size="sm"
              variant="primary"
              className="ml-auto"
              onClick={() => void commit()}
              disabled={saving || !draft.name.trim()}
              data-testid="agent-save"
            >
              {saving ? "保存中…" : "保存"}
            </Button>
          </div>
        </div>
      )}

      <ConfirmDialog
        open={pendingDelete !== null}
        title="删除角色"
        danger
        confirmText="删除"
        message={`确定要删除角色【${pendingDelete?.name ?? ""}】吗？删除后主 AI 不会再给它派活。`}
        onConfirm={() => {
          const target = pendingDelete;
          setPendingDelete(null);
          if (target) void remove(target);
        }}
        onClose={() => setPendingDelete(null)}
      />
    </div>
  );
}

/**
 * 「允许调用的工具」：全局白名单（阶段 9）。
 *
 * 勾一下就直接落盘——这里没有「保存」按钮，因为它不是表单型数据，
 * 用户勾完就希望下一次派活生效；落盘失败会由 store 把 error 顶到上面那条红字里。
 */
function ToolsPanel() {
  const allowed = useSettingsStore((s) => s.view?.settings.allowedTools ?? DEFAULT_TOOLS);
  const saving = useSettingsStore((s) => s.saving);
  const error = useSettingsStore((s) => s.error);
  const saveAllowedTools = useSettingsStore((s) => s.saveAllowedTools);

  const toggle = (tool: string) => {
    const next = allowed.includes(tool) ? allowed.filter((t) => t !== tool) : [...allowed, tool];
    void saveAllowedTools(next);
  };

  return (
    <div className="rounded-md border border-ink-200 bg-white p-3" data-testid="tool-whitelist">
      <div className="flex items-center gap-2">
        <h4 className="text-xs font-medium text-ink-700">允许调用的工具（{allowed.length} 个）</h4>
        {allowed.length === 0 && <Badge tone="warn">一个都没勾：子智能体只能输出文字</Badge>}
        {saving && <span className="text-[10px] text-ink-400">保存中…</span>}
      </div>

      <div className="mt-2 flex flex-wrap gap-x-4 gap-y-1.5">
        {TOOL_CHOICES.map((tool) => (
          <label
            key={tool}
            className="flex cursor-pointer items-center gap-1.5 text-[11px] text-ink-700"
          >
            <input
              type="checkbox"
              checked={allowed.includes(tool)}
              onChange={() => toggle(tool)}
              className="h-3.5 w-3.5 accent-accent"
              data-testid={`tool-${tool}`}
            />
            <span className="font-mono">{tool}</span>
          </label>
        ))}
      </div>

      <p className="mt-2 text-[10px] leading-relaxed text-ink-400">
        生效方式：勾的那些会写进子智能体 prompt 的「允许的工具：…」一行（就在【严格限制】那段后面），
        下一次派活立刻生效。角色自己填了 tools 的以角色为准。它只改提示词与目录守卫，
        CLI 侧的真实权限没被改动——真要硬限制，得配 <code className="font-mono">claude --allowedTools</code>。
      </p>

      {error && (
        <div className="mt-2 rounded border border-rose-200 bg-rose-50 px-2 py-1.5 text-[11px] text-rose-700">
          {error}
        </div>
      )}
    </div>
  );
}

/** 多行输入框（common 里只有单行 `TextInput`，这里补一个同样配色的）。 */function TextareaRaw({ className, ...rest }: TextareaHTMLAttributes<HTMLTextAreaElement>) {
  return (
    <textarea
      className={cn(
        "w-full rounded-md border border-ink-200 bg-white px-2.5 py-2 text-sm leading-relaxed text-ink-900",
        "placeholder:text-ink-300 focus:border-accent focus:outline-none focus:ring-2 focus:ring-accent/25",
        className,
      )}
      {...rest}
    />
  );
}
