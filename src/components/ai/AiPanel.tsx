import { useEffect } from "react";

import { EventPanel } from "@/components/ai/EventPanel";
import { MasterChat } from "@/components/ai/MasterChat";
import { MasterInput } from "@/components/ai/MasterInput";
import { Button } from "@/components/common/Button";
import { Badge } from "@/components/common/Field";
import { useAiStore } from "@/store/aiStore";
import { useProjectStore } from "@/store/projectStore";
import { useSettingsStore } from "@/store/settingsStore";

/**
 * 右栏：主 AI 统筹面板。
 *
 * 阶段 7.5 起这里只有**一个对话对象**——主 AI 总监。用户说一句综合指令，
 * 后端先规划再按序派给子智能体；聊天流里依次出现「思考 → 计划 → 派发卡片 →
 * 子智能体流式输出 → 收尾」。原来那套「团队 / 对话」双 Tab 已废除。
 */
export function AiPanel() {
  const engines = useAiStore((s) => s.engines);
  const gate = useAiStore((s) => s.gate);
  const masterBusy = useAiStore((s) => s.masterBusy);
  const bindEvents = useAiStore((s) => s.bindEvents);
  const loadEngines = useAiStore((s) => s.loadEngines);
  const loadGate = useAiStore((s) => s.loadGate);
  const loadChat = useAiStore((s) => s.loadChat);
  const clearChat = useAiStore((s) => s.clearChat);
  const chatLoaded = useAiStore((s) => s.chatLoaded);
  const sessionId = useAiStore((s) => s.sessionId);

  const workspaceDir = useProjectStore((s) => s.workspaceDir);
  const agents = useSettingsStore((s) => s.view?.settings.agents ?? []);
  const settingsLoaded = useSettingsStore((s) => s.loaded);
  const loadSettings = useSettingsStore((s) => s.load);

  useEffect(() => {
    void bindEvents();
  }, [bindEvents]);

  // 设置只在打开设置弹窗时才加载过；主 AI 面板要靠 agents 显示下属数量，这里补一次
  useEffect(() => {
    if (!settingsLoaded) void loadSettings();
  }, [settingsLoaded, loadSettings]);

  useEffect(() => {
    if (!workspaceDir) return;
    void loadEngines();
    void loadGate();
  }, [workspaceDir, loadEngines, loadGate]);

  // 阶段 8：进工作台先把这段会话的历史读回来（默认最近一次）
  useEffect(() => {
    if (!workspaceDir || chatLoaded) return;
    void loadChat();
  }, [workspaceDir, chatLoaded, loadChat]);

  // 徽章跟设置里的引擎偏好走（设置 → AI 引擎选择 claude / opencode）。
  // 之前写死显示 engines[0]（即 Claude Code CLI），用户明明选了 opencode 顶部却
  // 一直挂着「Claude Code CLI 未检测到」，看着像配置没生效——实际后端走的就是
  // 选中的引擎。这里显示的必须与后端 build() 用的同一个偏好值。
  const engineKind = useSettingsStore((s) => s.view?.settings.engine.kind ?? "claude");
  const engineInfo = engines.find((e) => e.kind === engineKind) ?? engines[0];
  const engineLabel = engineInfo?.name ?? (engineKind === "opencode" ? "opencode CLI" : "Claude Code CLI");
  const engineDetail =
    engineInfo?.detail ??
    "引擎（claude / opencode 二选一，自定义模型 API 请选 opencode）：先在设置里确认它已安装并能跑起来";

  return (
    // select-text：全局 body 是 user-select: none（桌面应用观感），AI 面板里的消息、
    // 计划卡、日志都是纯文本，用户要用左键选中复制，必须在这里重新打开。
    <div className="flex h-full min-h-0 flex-col select-text" data-testid="ai-panel">
      <div className="shrink-0 space-y-1.5 border-b border-ink-100 bg-white px-3 py-2.5">
        <div className="flex items-center gap-2">
          <span aria-hidden className="text-sm leading-none">
            🧭
          </span>
          <span className="text-xs font-semibold text-ink-900">主 AI 总监</span>
          <Badge tone={masterBusy ? "warn" : "neutral"}>
            {masterBusy ? "派活中" : "待命"}
          </Badge>
          <span
            className="ml-auto flex min-w-0 items-center gap-1 text-xs text-ink-700"
            title={engineDetail}
            data-testid="ai-engine"
          >
            <span aria-hidden className="text-[11px] leading-none">
              ⚙️
            </span>
            <span className="truncate font-medium">{engineLabel}</span>
            {engineInfo && !engineInfo.available && <Badge tone="warn">未检测到</Badge>}
          </span>
        </div>

        <div className="flex flex-wrap items-center gap-1.5 text-[11px]">
          <Badge tone={gate?.ok ? "ok" : "warn"}>
            {gate?.ok ? "门禁脚本就绪" : gate ? "门禁脚本缺失" : "门禁检测中"}
          </Badge>
          <Badge tone={agents.length ? "neutral" : "warn"}>下属 {agents.length} 个</Badge>
          <span className="text-[10px] text-ink-400">在「设置 → 智能体角色」里增删改</span>
        </div>

        <div className="flex items-center gap-1.5">
          <Button
            size="sm"
            variant="ghost"
            className="h-6 px-1.5 text-[10px]"
            onClick={() => void loadChat()}
            data-testid="chat-history"
            title={sessionId ? `会话 ${sessionId}` : "还没有会话"}
          >
            历史对话
          </Button>
          <Button
            size="sm"
            variant="ghost"
            className="h-6 px-1.5 text-[10px]"
            onClick={() => void clearChat()}
            disabled={masterBusy}
            data-testid="chat-clear"
          >
            清空当前对话
          </Button>
          <span className="ml-auto font-mono text-[9px] text-ink-300">
            {sessionId ?? "—"}
          </span>
        </div>
      </div>

      <EventPanel />
      <MasterChat />
      <MasterInput />
    </div>
  );
}
