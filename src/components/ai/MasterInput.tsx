import { useEffect, useRef, useState } from "react";

import { Button } from "@/components/common/Button";
import { Badge } from "@/components/common/Field";
import { api } from "@/lib/ipc";
import { useAiStore } from "@/store/aiStore";
import { useSettingsStore } from "@/store/settingsStore";
import { polishPrompt } from "@/lib/promptPolish";

/** 预置的常见指令：点一下填进输入框，不直接发送（用户可以再补一句）。 */
const SUGGESTIONS = ["帮我写第一章正文", "生成第一章细纲", "检查伏笔与时间线", "拆解对标作品的节奏"];

/** 拖拽调节的输入框高度持久化（px）。用户嫌 2 行太小，改成可拖 + 记住高度。 */
const HEIGHT_KEY = "master-input-height";
const HEIGHT_MIN = 56;
const HEIGHT_MAX = 480;

function savedHeight(): number | null {
  const raw = Number(localStorage.getItem(HEIGHT_KEY));
  return Number.isFinite(raw) && raw >= HEIGHT_MIN && raw <= HEIGHT_MAX ? raw : null;
}

/**
 * 底部输入区：一个输入框 + 一个「下发任务」。
 *
 * 阶段 8 起它只负责「让主 AI 拆计划」——真正的执行要作者在计划卡上点「确认执行」，
 * 所以这里调的是 `planMaster` 而不是执行。
 */
export function MasterInput() {
  const busy = useAiStore((s) => s.masterBusy);
  const stepRunId = useAiStore((s) => s.masterStepRunId);
  const text = useAiStore((s) => s.masterDraft);
  const setDraft = useAiStore((s) => s.setDraft);
  const planMaster = useAiStore((s) => s.planMaster);
  const clearChat = useAiStore((s) => s.clearChat);
  const cancelActive = useAiStore((s) => s.cancelActive);
  const error = useAiStore((s) => s.error);
  const pending = useAiStore((s) => s.masterPlan);
  const queue = useAiStore((s) => s.masterQueue);
  const removeQueued = useAiStore((s) => s.removeQueued);
  const agents = useSettingsStore((s) => s.view?.settings.agents ?? []);
  const taRef = useRef<HTMLTextAreaElement>(null);
  // 「优化提示词」的撤销栈只留一层：优化前那一版就够用户后悔一次。
  const [polishBefore, setPolishBefore] = useState<string | null>(null);
  const [polishNotes, setPolishNotes] = useState<string[]>([]);

  const [polishing, setPolishing] = useState(false);

  /**
   * 优化提示词：先走真模型（设置里激活的提供商 + API Key），
   * 模型不可用（没 Key / 报错 / 超时）时才回落到本地规则，并把原因写在提示里——
   * 用户要的是「真的把我的话改好」，模板拼出来的规格不算优化。
   */
  const polish = async () => {
    const original = text;
    if (!original.trim() || polishing) return;
    setPolishing(true);
    setPolishNotes([]);
    try {
      const res = await api.polishPrompt(original);
      setPolishBefore(original);
      setDraft(res.text);
      setPolishNotes([`已由 ${res.provider} · ${res.model} 改写；觉得不对点「撤销优化」还原原话`]);
    } catch (e) {
      const local = polishPrompt(original);
      setPolishBefore(original);
      setDraft(local.text);
      setPolishNotes([
        `模型优化没成功：${e instanceof Error ? e.message : String(e)}——下面这份是本地规则兜的，配好 设置 → 模型提供商 的 API Key 后重试才是真改写`,
        ...local.notes,
      ]);
    } finally {
      setPolishing(false);
    }
  };

  const undoPolish = () => {
    if (polishBefore === null) return;
    setDraft(polishBefore);
    setPolishBefore(null);
    setPolishNotes([]);
  };

  // 草稿被清空（消息真的下发出去 / 点了清空对话）就等于那版草稿不存在了，
  // 提示与撤销栈一起收掉；用户自己改字不算，改完还能点「撤销优化」。
  useEffect(() => {
    if (!text.trim() && polishBefore !== null) {
      setPolishBefore(null);
      setPolishNotes([]);
    }
  }, [text, polishBefore]);

  // 恢复上次拖出的高度（浏览器 resize 只改内联 style，需要自己记）
  useEffect(() => {
    const ta = taRef.current;
    const height = savedHeight();
    if (ta && height) ta.style.height = `${height}px`;
  }, []);

  // 拖拽结束（mouseup 时 resize 已完成）把高度存下来；自动增高（输入多行）不存。
  const persistHeight = () => {
    const ta = taRef.current;
    if (!ta) return;
    const height = ta.offsetHeight;
    if (height >= HEIGHT_MIN && height <= HEIGHT_MAX) {
      localStorage.setItem(HEIGHT_KEY, String(height));
    }
  };

  /**
   * 「已由 X 改写」这条提示描述的是输入框里那一版草稿。草稿一发出，它就该消失——
   * 以前发给主 AI 之后提示还挂在输入框上方（撤销栈也还在），看着像消息没发出去。
   */
  const dropPolish = () => {
    setPolishBefore(null);
    setPolishNotes([]);
  };

  const send = () => {
    if (!text.trim()) return;
    // 忙 / 有待确认计划不再拦下：planMaster 自己会把指令放进等待区排队
    const queued = useAiStore.getState().masterBusy || useAiStore.getState().masterPlan !== null;
    void planMaster(text);
    // 进等待区时草稿还留在输入框里，不会触发下面那条「草稿空了就收掉」的效果：
    // 这里显式收掉，保证「发出去了」和「提示消失」同时发生。
    if (queued) dropPolish();
  };

  return (
    <div className="shrink-0 border-t border-ink-200 bg-white px-3 py-2.5" data-testid="master-input">
      {error && (
        <div className="mb-1.5 rounded border border-rose-200 bg-rose-50 px-2 py-1 text-[11px] text-rose-700">
          {error}
        </div>
      )}

      {/* 优化后的自查提示：补了什么、哪一条没识别到。不写这些，用户就看不出
          「优化」到底改了哪里，也没法判断该不该发。 */}
      {polishNotes.length > 0 && (
        <ul
          className="mb-1.5 space-y-0.5 rounded border border-sky-200 bg-sky-50/60 px-2 py-1"
          data-testid="master-polish-notes"
        >
          {polishNotes.map((n) => (
            <li key={n} className="text-[10px] leading-relaxed text-sky-800">
              · {n}
            </li>
          ))}
        </ul>
      )}

      <div className="flex items-center gap-1.5 overflow-x-auto pb-1.5">
        {SUGGESTIONS.map((s) => (
          <button
            key={s}
            type="button"
            onClick={() => setDraft(s)}
            className="shrink-0 rounded-full border border-ink-200 bg-ink-50 px-2 py-0.5 text-[10px] text-ink-600 hover:border-accent hover:text-accent-strong"
            data-testid="master-suggestion"
          >
            {s}
          </button>
        ))}
        <Badge tone={agents.length ? "neutral" : "warn"}>
          下属 {agents.length} 个
        </Badge>
      </div>

      {/* 等待区：忙时下发的指令排在这里，上一个任务跑完自动按顺序执行 */}
      {queue.length > 0 && (
        <div
          className="mb-1.5 rounded-md border border-amber-200 bg-amber-50/60 px-2 py-1.5"
          data-testid="master-queue"
        >
          <div className="text-[10px] font-medium text-amber-700">
            等待区 · {queue.length} 条（上一个任务结束后自动依次下发）
          </div>
          <ol className="mt-1 space-y-0.5">
            {queue.map((item, i) => (
              <li key={`${i}-${item.slice(0, 12)}`} className="flex items-center gap-1.5 text-[10px] text-ink-600">
                <span className="shrink-0 font-mono text-amber-600">{i + 1}.</span>
                <span className="min-w-0 flex-1 truncate" title={item}>{item}</span>
                <button
                  type="button"
                  onClick={() => removeQueued(i)}
                  title="从等待区撤掉这条"
                  className="shrink-0 rounded p-0.5 text-ink-400 hover:bg-rose-50 hover:text-rose-600"
                  data-testid="master-queue-remove"
                >
                  ✕
                </button>
              </li>
            ))}
          </ol>
        </div>
      )}

      <textarea
        ref={taRef}
        value={text}
        onChange={(e) => setDraft(e.target.value)}
        onKeyDown={(e) => {
          // Ctrl/Cmd + Enter 下发；单独 Enter 换行（指令常常是多行的）
          if ((e.metaKey || e.ctrlKey) && e.key === "Enter") {
            e.preventDefault();
            send();
          }
        }}
        onMouseUp={persistHeight}
        onTouchEnd={persistHeight}
        rows={3}
        placeholder="告诉主 AI 你要什么：例如「帮我写第一章正文，主角先出场再打一架」（右下角可拖动调节高度）"
        className="w-full resize-y rounded-md border border-ink-200 bg-white px-2.5 py-2 text-xs leading-relaxed text-ink-900 placeholder:text-ink-300 focus:border-accent focus:outline-none focus:ring-2 focus:ring-accent/25"
        style={{ minHeight: HEIGHT_MIN, maxHeight: HEIGHT_MAX }}
        data-testid="master-note"
      />

      <div className="mt-1.5 flex items-center gap-1.5">
        <span className="text-[10px] text-ink-400">
          {busy
            ? "主 AI 正忙：现在下发会进等待区排队"
            : pending
              ? "计划已就绪：5 秒后自动执行，也可在卡片上点「确认执行」"
              : "Ctrl/⌘ + Enter 下发 · 主 AI 先拆计划，你确认后才动笔"}
        </span>
        <div className="ml-auto flex shrink-0 items-center gap-1.5">
          <Button
            size="sm"
            variant="ghost"
            onClick={() => void polish()}
            disabled={!text.trim() || polishing}
            data-testid="master-polish"
            title="交给当前模型把你的话改写成可执行规格：交付物 / 章号 / 字数 / 收尾 / 不要做什么（不改你的意思）"
          >
            {polishing ? "优化中…" : "优化提示词"}
          </Button>
          {polishBefore !== null && (
            <Button size="sm" variant="ghost" onClick={undoPolish} data-testid="master-polish-undo">
              撤销优化
            </Button>
          )}
          <Button size="sm" variant="ghost" onClick={() => void clearChat()} disabled={busy}>
            清空对话
          </Button>
          {/* 执行中「停止」与「发送」并存：以前 busy 时发送按钮被顶掉，
              用户想往等待区追加指令都点不了，只能干等。 */}
          {busy && stepRunId && (
            <Button
              size="sm"
              variant="danger"
              onClick={() => {
                // 取消当前这一步：整轮编排会在这一步失败后停下
                void api.aiCancel(stepRunId).catch(() => void cancelActive());
              }}
              data-testid="master-stop"
            >
              停止
            </Button>
          )}
          <Button
            size="sm"
            variant="primary"
            onClick={send}
            disabled={!text.trim()}
            data-testid="master-send"
          >
            {busy || pending ? "加入等待区" : "下发任务"}
          </Button>
        </div>
      </div>
    </div>
  );
}
