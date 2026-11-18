import { useEffect, useRef, useState } from "react";

import { HookBlockCard } from "@/components/ai/HookBlockCard";
import { PostCheckCard } from "@/components/ai/PostCheckCard";
import { RunTimeline } from "@/components/ai/RunTimeline";
import { Button } from "@/components/common/Button";
import { Badge } from "@/components/common/Field";
import { cn } from "@/lib/cn";
import { api } from "@/lib/ipc";
import { useAiStore } from "@/store/aiStore";
import { useProjectStore } from "@/store/projectStore";
import type { MasterDispatchPayload, PlanStep } from "@/types";

/**
 * 主 AI 统筹面板的对话流。
 *
 * 这里只有一种「对话对象」：主 AI 总监。子智能体不直接跟用户说话，
 * 它们的工作以「派发卡片 + 流式日志」的形式挂在那条派发记录下面。
 */
export function MasterChat() {
  const events = useAiStore((s) => s.masterEvents);
  const busy = useAiStore((s) => s.masterBusy);
  const endRef = useRef<HTMLDivElement>(null);

  // 只在贴着底部时自动滚动，用户往上翻历史时不打断他
  const pinned = useRef(true);
  useEffect(() => {
    const box = endRef.current?.parentElement;
    if (!box) return;
    const onScroll = () => {
      pinned.current = box.scrollHeight - box.scrollTop - box.clientHeight < 40;
    };
    box.addEventListener("scroll", onScroll);
    return () => box.removeEventListener("scroll", onScroll);
  }, []);

  useEffect(() => {
    if (pinned.current) endRef.current?.scrollIntoView({ block: "end" });
  }, [events, busy]);

  if (events.length === 0) return <EmptyChat />;

  return (
    <div className="min-h-0 flex-1 space-y-3 overflow-y-auto px-3 py-3" data-testid="master-chat">
      {events.map((event, i) => {
        switch (event.kind) {
          case "user":
            return (
              <div key={`u-${i}`} className="flex justify-end" data-testid="master-user">
                <div className="max-w-[85%] rounded-2xl rounded-br-sm bg-accent px-3 py-2 text-xs leading-relaxed text-white shadow-sm">
                  {event.text}
                </div>
              </div>
            );
          case "think":
            return (
              <div key={`t-${i}`} className="flex items-start gap-2" data-testid="master-think">
                <span className="mt-0.5 text-sm leading-none">🧠</span>
                <div className="min-w-0 text-[11px] leading-relaxed text-ink-500">
                  <span className="font-medium text-ink-600">
                    {event.phase === "stream"
                      ? "主 AI 正在思考…"
                      : event.phase === "planning"
                        ? "主 AI 拆解任务"
                        : "主 AI"}
                  </span>
                  {/* 规划/派发阶段的说明可能是长文本，限高滚动，别把整个聊天流顶飞。 */}
                  <div
                    className={cn(
                      "mt-0.5 whitespace-pre-wrap break-words",
                      (event.phase === "stream" || event.text.length > 160) &&
                        "max-h-36 overflow-y-auto rounded bg-ink-50/60 px-1.5 py-1",
                    )}
                  >
                    {event.text}
                  </div>
                </div>
              </div>
            );
          case "plan":
            return (
              <PlanCard
                key={`p-${i}`}
                id={event.id}
                at={event.at}
                source={event.source}
                intent={event.intent ?? null}
                steps={event.steps}
                expanded={event.expanded ?? null}
              />
            );
          case "system":
            return (
              <div
                key={`sys-${i}`}
                className="rounded border border-ink-150 bg-ink-50/60 px-2 py-1 text-[10px] leading-relaxed text-ink-500"
                data-testid="master-system"
              >
                {event.text}
              </div>
            );
          case "dispatch":
            return <DispatchCard key={`d-${event.payload.stepRunId}`} payload={event.payload} />;
          case "done":
            return (
              <div
                key={`done-${i}`}
                className={cn(
                  "rounded-lg border px-3 py-2 text-[11px] leading-relaxed",
                  event.payload.status === "done"
                    ? "border-emerald-200 bg-emerald-50 text-emerald-800"
                    : "border-amber-200 bg-amber-50 text-amber-800",
                )}
                data-testid="master-done"
              >
                {event.payload.text}
                <span className="ml-1 text-[10px] opacity-70">
                  （已完成 {event.payload.stepsDone}/{event.payload.stepsTotal}
                  {event.payload.resumeFrom
                    ? `，可从第 ${event.payload.resumeFrom} 步续跑`
                    : ""}
                  ）
                </span>
                {event.payload.persisted ? (
                  <span className="ml-1 align-middle">
                    <Badge tone="ok">进度已落盘</Badge>
                  </span>
                ) : null}
              </div>
            );
          default:
            return null;
        }
      })}
      {busy && <BusyHint />}
      <div ref={endRef} />
    </div>
  );
}

/** 心跳里「静默」多久就提示一次。注意：CLI 在**一次工具调用完成前不会发送任何事件**，
 *  模型生成 9000 字正文、跑长命令时必然静默 2 分钟以上，所以这条只解释、不断言卡死。 */
const STUCK_MS = 120_000;

function formatElapsed(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000));
  const m = Math.floor(total / 60);
  const s = total % 60;
  return m > 0 ? `${m} 分 ${String(s).padStart(2, "0")} 秒` : `${s} 秒`;
}

const STATUS_TEXT: Record<string, string> = {
  planning: "规划中",
  running: "运行中",
  done: "已完成",
  blocked: "被拦下",
  failed: "失败",
  cancelled: "已取消",
};

function BusyHint() {
  // 正在派活：一张「当前步骤」小卡片——第几步、派给谁、跑了多久、还有没有动静。
  // 事故背景：写手步静默跑了 10 分 22 秒，界面只有一行被截断的小字，
  // 作者以为卡死就把窗口关了。现在改成时钟 + 心跳 + 卡住提示 + 任务全文常驻。
  const note = useAiStore((s) => s.masterDispatchNote);
  const task = useAiStore((s) => s.masterDispatchTask);
  const progress = useAiStore((s) => s.masterProgress);
  const [expanded, setExpanded] = useState(true);
  const [copied, setCopied] = useState(false);
  const receivedAt = useRef(Date.now());
  const [now, setNow] = useState(() => Date.now());

  useEffect(() => {
    receivedAt.current = Date.now();
    setNow(Date.now());
  }, [progress]);

  // 心跳两秒一条，本地再按秒走表：模型那边再慢，这边秒数也一直在动。
  useEffect(() => {
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, []);

  const sinceBeat = Math.max(0, now - receivedAt.current);
  const elapsed = progress ? progress.elapsedMs + sinceBeat : null;
  const idle = progress?.idleMs ?? null;
  const silentFor = progress ? (idle === null ? null : idle + sinceBeat) : null;
  const finished =
    progress !== null && !["running", "planning"].includes(progress.status);
  const stuck =
    !finished &&
    progress !== null &&
    (silentFor === null
      ? (elapsed ?? 0) > STUCK_MS
      : silentFor > STUCK_MS);

  const copyTask = async () => {
    if (!task) return;
    try {
      await navigator.clipboard.writeText(task);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1500);
    } catch {
      setCopied(false);
    }
  };

  return (
    <div
      className={cn(
        "rounded-lg border px-2.5 py-2 text-[11px]",
        stuck
          ? "border-amber-300 bg-amber-50/70"
          : "border-accent/30 bg-accent-soft/40",
      )}
      data-testid="master-busy"
    >
      <div className="flex items-start gap-2">
        <span
          className={cn(
            "mt-1 inline-block h-2 w-2 shrink-0 rounded-full",
            stuck ? "bg-amber-500" : "animate-pulse bg-accent",
          )}
        />
        <div className="min-w-0 flex-1 text-ink-600">
          <div className="flex flex-wrap items-center gap-x-2 gap-y-1">
            <span className="font-medium text-ink-800">{note ?? "主 AI 正在派活…"}</span>
            {progress && (
              <>
                <span className="text-ink-500" data-testid="master-busy-status">
                  {STATUS_TEXT[progress.status] ?? progress.status}
                </span>
                {elapsed !== null && (
                  <span className="font-mono text-ink-700" data-testid="master-busy-elapsed">
                    已跑 {formatElapsed(elapsed)}
                  </span>
                )}
              </>
            )}
          </div>

          {progress && (
            <div className="mt-1 flex flex-wrap items-center gap-x-2 gap-y-0.5 text-[10px] text-ink-500">
              {progress.chapter ? (
                <span data-testid="master-busy-chapter">第 {progress.chapter} 章</span>
              ) : null}
              {progress.targetDir ? <span>写入 {progress.targetDir}/</span> : null}
              <span data-testid="master-busy-chunks">已收到 {progress.chunks} 条输出</span>
              {silentFor !== null && !finished ? (
                <span data-testid="master-busy-idle">
                  上次输出 {formatElapsed(silentFor)} 前
                </span>
              ) : null}
              {silentFor === null && !finished ? <span>还在等第一句输出</span> : null}
              {progress.produced && progress.produced.length > 0 ? (
                <span>产出 {progress.produced.join("、")}</span>
              ) : null}
            </div>
          )}

          {progress && progress.status !== "planning" && progress.total > 0 && (
            <div
              className="mt-1 flex flex-wrap items-center gap-x-2 gap-y-1 text-[10px]"
              data-testid="master-busy-resume"
            >
              <span className="font-mono text-ink-700">
                已完成 {progress.done ?? 0}/{progress.total}
              </span>
              {progress.resumeFrom ? (
                <span className="text-ink-600">
                  可从第 {progress.resumeFrom} 步续跑
                </span>
              ) : (
                <span className="text-ink-600">这一轮已经到齐</span>
              )}
              {progress.persisted ? (
                <Badge tone="ok">进度已落盘</Badge>
              ) : (
                <Badge tone="warn">进度还没落盘</Badge>
              )}
              <span className="text-ink-400">
                {progress.persisted
                  ? "关掉窗口也不会丢，回来从上面那一步接着跑"
                  : "这一步结束后写入 .storydesk/runs/"}
              </span>
            </div>
          )}

          {stuck && (
            <div
              className="mt-1 rounded border border-amber-200 bg-amber-100/60 px-1.5 py-1 text-[10px] text-amber-800"
              data-testid="master-busy-stuck"
            >
              已 2 分钟没有新输出。这通常不是卡死：模型正在生成或写入长内容
              （一次工具调用完成之前，CLI 不会发出任何事件），上面「已跑」时间仍在增长就说明进程活着。
              写 9000 字级别的正文静默三五分钟很常见，建议继续等；
              若十几分钟仍无动静，可点右栏「停止」重来——每一步的结果都会写进下面的流水，不会悄悄失败。
            </div>
          )}

          {task && (
            <>
              <div
                className={cn(
                  "mt-1 whitespace-pre-wrap break-words rounded bg-white/60 px-1.5 py-1 text-ink-700",
                  !expanded && "max-h-8 overflow-hidden",
                  expanded && "max-h-60 overflow-y-auto",
                )}
                data-testid="master-busy-task"
              >
                {task}
              </div>
              <div className="mt-0.5 flex items-center gap-2">
                <button
                  type="button"
                  onClick={() => setExpanded((v) => !v)}
                  className="text-[10px] text-accent-strong hover:underline"
                  data-testid="master-busy-expand"
                >
                  {expanded ? "收起任务全文" : "展开任务全文"}
                </button>
                <button
                  type="button"
                  onClick={() => void copyTask()}
                  className="text-[10px] text-accent-strong hover:underline"
                  data-testid="master-busy-copy"
                >
                  {copied ? "已复制" : "复制任务全文"}
                </button>
              </div>
            </>
          )}
          <div className="mt-1 text-[10px] text-ink-400">
            子智能体的实时输出显示在下面那张派发卡片里；这张小卡每 2 秒刷新一次进度。
          </div>
        </div>
      </div>
    </div>
  );
}

function EmptyChat() {
  const planMaster = useAiStore((s) => s.planMaster);
  const examples = [
    "帮我写第一章正文",
    "生成第一章细纲",
    "检查伏笔与时间线",
    "拆解对标作品的节奏",
  ];

  return (
    <div className="flex min-h-0 flex-1 flex-col items-center justify-center gap-3 px-6 text-center">
      <div className="text-2xl">🧭</div>
      <div className="text-xs font-medium text-ink-700">对主 AI 总监说一句话</div>
      <div className="text-[11px] leading-relaxed text-ink-500">
        它会先拆成计划，再把每一步派给下面的专业智能体去做；
        你能看到谁在干活、写到哪一步、哪一步被拦下。
      </div>
      <div className="mt-1 flex flex-wrap justify-center gap-1.5">
        {examples.map((text) => (
          <button
            key={text}
            type="button"
            onClick={() => void planMaster(text)}
            className="rounded-full border border-ink-200 bg-white px-2.5 py-1 text-[11px] text-ink-600 hover:border-accent hover:text-accent-strong"
          >
            {text}
          </button>
        ))}
      </div>
    </div>
  );
}

/**
 * 计划卡（阶段 8 的核心，阶段 9 加了编辑/取消）：计划出来**不立刻执行**，先让作者看一眼。
 *
 * 「✅ 确认执行」→ `master_execute`（带这份 steps，不再规划）；
 * 「✏️ 编辑计划」→ 就地改每步的任务文本、删掉不要的步骤，保存后带改过的 steps 执行
 * （会换一个新 runId，历史里的旧卡不会被覆盖）；
 * 「🚫 取消计划」→ 丢掉这份计划，把原话回填到输入框（复用 `editMasterRequest`）。
 */
/** 计划卡挂出后几秒自动执行（用户点编辑/取消可阻止）。 */
const AUTO_RUN_SECONDS = 5;
/** 拆步后超过这个步数就折起来，只露前几步（否则一张卡能把聊天流整屏占满）。 */
const COLLAPSE_OVER = 8;
const COLLAPSE_SHOWN = 4;

function PlanCard({
  id,
  source,
  intent,
  steps,
  expanded,
}: {
  id: string;
  at: number;
  source: string;
  intent?: string | null;
  steps: PlanStep[];
  /** 拆步后实际执行的步数（`plan-final` 回写）；有值就说明这张卡被展开过 */
  expanded?: number | null;
}) {
  const pending = useAiStore((s) => s.masterPlan);
  const busy = useAiStore((s) => s.masterBusy);
  const confirmMaster = useAiStore((s) => s.confirmMaster);
  const editMasterRequest = useAiStore((s) => s.editMasterRequest);
  const waiting = pending !== null && pending.runId === id;
  // 同一轮编排的收尾事件带的就是这份计划的 runId；编辑过的计划是 `<id>-e…`
  const executed = useAiStore((s) =>
    s.masterEvents.some((e) => e.kind === "done" && (e.id === id || e.id.startsWith(`${id}-e`))),
  );

  // 编辑态：只在「这张卡还在等确认」时有意义，取消/确认后组件自然回到只读
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState<PlanStep[]>(steps);
  // 拆步后可能是几十步：默认只露前几步，点「已展开为 N 步」那行再全放出来。
  const [showAll, setShowAll] = useState(false);
  const expandedCount = expanded ?? 0;
  const collapsed = expandedCount > COLLAPSE_OVER && !showAll;
  const shownSteps = collapsed ? steps.slice(0, COLLAPSE_SHOWN) : steps;

  // 计划被别的路径清掉（取消、重开、历史重放）时，退出编辑态，避免留一份孤儿草稿
  useEffect(() => {
    if (!waiting) {
      setEditing(false);
      setDraft(steps);
    }
  }, [waiting, steps]);

  // 5 秒自动确认：计划卡挂出来就倒数，用户不动手就替他执行；
  // 点「编辑计划」/「取消」、或进入编辑态都会停表。
  // 只有本轮新鲜拆出的计划（带 plannedAt）才倒数——重开软件恢复的历史卡绝不自动跑。
  const [countdown, setCountdown] = useState<number | null>(null);
  useEffect(() => {
    if (!waiting || editing || busy || !pending?.plannedAt) {
      setCountdown(null);
      return;
    }
    let left = AUTO_RUN_SECONDS;
    setCountdown(left);
    const timer = window.setInterval(() => {
      left -= 1;
      if (left <= 0) {
        window.clearInterval(timer);
        setCountdown(null);
        void confirmMaster();
      } else {
        setCountdown(left);
      }
    }, 1000);
    return () => window.clearInterval(timer);
  }, [waiting, editing, busy, pending?.plannedAt, confirmMaster]);

  // 改任务文本 = 这一手里所有「由旧文本推出来的东西」全部作废。
  // 原计划是「第 3 章」，用户改成「第 5 章」：若旧的 range: single(3) 还留着，
  // 后端闸层会拿第 3 章的区间去卡第 5 章的稿子，判越界并回滚进回收站。
  // 所以这里只清结构化字段（role/kind/range/lock/words/readOnly），任务文本与 agentId 保留，
  // 后端 normalize_step 会按新文本重新解析；文本没变时一个字段都不动。
  const patchStep = (idx: number, task: string) =>
    setDraft((prev) =>
      prev.map((s, i) => {
        if (i !== idx || s.task === task) return s;
        return {
          ...s,
          task,
          role: undefined,
          kind: undefined,
          range: undefined,
          lock: undefined,
          words: undefined,
          readOnly: undefined,
        };
      }),
    );
  const dropStep = (idx: number) => setDraft((prev) => prev.filter((_, i) => i !== idx));
  const canSave = draft.length > 0 && draft.every((s) => s.task.trim().length > 0);

  return (
    <div
      className="rounded-lg border border-ink-200 bg-ink-50/70 px-3 py-2"
      data-testid="master-plan"
      data-plan-id={id}
      data-plan-pending={waiting ? "1" : "0"}
      data-plan-editing={editing ? "1" : "0"}
    >
      <div className="flex items-center gap-2">
        <span className="text-[11px] font-medium text-ink-700">拆解出的计划</span>
        <Badge tone={source === "llm" ? "ok" : "neutral"}>
          {source === "llm" ? "主 AI 规划" : source === "rules" ? "本地规则兜底" : source}
        </Badge>
        {editing && <Badge tone="warn">编辑中</Badge>}
        <span className="ml-auto text-[10px] text-ink-400">
          {(editing ? draft : steps).length} 步
        </span>
      </div>

      {/* 阶段 11：把我「理解成了什么」摊在用户面前——他点确认之前就该看到这句话，
          否则一旦理解错了，代价是一份被覆盖的大纲。 */}
      {intent && (
        <div
          className="mt-1 rounded border border-ink-150 bg-white/60 px-2 py-1 text-[10px] leading-relaxed text-ink-600"
          data-testid="plan-intent"
        >
          我理解为：<span className="font-medium text-ink-800">{intent}</span>
        </div>
      )}

      {editing ? (
        <div className="mt-1.5 space-y-1.5" data-testid="plan-editor">
          {draft.map((step, idx) => (
            <div key={`${step.agentId}-${idx}`} className="flex items-start gap-1.5">
              <span className="mt-1 w-3 shrink-0 text-[10px] text-ink-400">{idx + 1}.</span>
              <span className="mt-1 w-20 shrink-0 truncate font-mono text-[10px] text-ink-400">
                {step.agentId}
              </span>
              <textarea
                value={step.task}
                rows={2}
                onChange={(e) => patchStep(idx, e.target.value)}
                className="min-w-0 flex-1 rounded border border-ink-200 bg-white px-1.5 py-1 text-[11px] leading-relaxed text-ink-800 focus:border-accent focus:outline-none"
                data-testid={`plan-step-${idx}`}
              />
              <button
                type="button"
                onClick={() => dropStep(idx)}
                disabled={draft.length <= 1}
                title={draft.length <= 1 ? "至少要留一步" : "删掉这一步"}
                className="mt-1 shrink-0 rounded px-1 text-[11px] text-rose-600 hover:bg-rose-50 disabled:text-ink-300 disabled:hover:bg-transparent"
                data-testid={`plan-step-del-${idx}`}
              >
                ✕
              </button>
            </div>
          ))}

          <div className="flex items-center gap-1.5">
            <Button
              size="sm"
              variant="primary"
              disabled={busy || !canSave}
              onClick={() => void confirmMaster(draft)}
              data-testid="plan-save"
            >
              ✅ 保存并执行
            </Button>
            <Button
              size="sm"
              variant="ghost"
              disabled={busy}
              onClick={() => {
                setDraft(steps);
                setEditing(false);
              }}
              data-testid="plan-collapse"
            >
              收起
            </Button>
            <span className="ml-auto text-[10px] text-ink-400">
              {canSave ? "改完点「保存并执行」" : "每一步都得有任务描述"}
            </span>
          </div>
        </div>
      ) : (
        <>
          {/* 拆步后的真实步数：卡上原先只显示拆前的 2 步，实际却跑了 30 步。
              这行同时是「展开/收起」的开关，别只是句说明。 */}
          {expandedCount > 0 && (
            <button
              type="button"
              onClick={() => setShowAll((v) => !v)}
              className="mt-1 block text-left text-[10px] text-ink-500 hover:text-ink-700"
              data-testid="plan-expanded"
            >
              已展开为 {expandedCount} 步（{showAll ? "收起" : "可展开查看"}）
            </button>
          )}
          <ol className="mt-1 space-y-0.5">
            {shownSteps.map((step, idx) => (
              <li key={`${step.agentId}-${idx}`} className="text-[11px] leading-relaxed text-ink-600">
                {idx + 1}. <span className="font-mono text-[10px] text-ink-400">{step.agentId}</span>{" "}
                {step.event && (
                  <span
                    className="mr-1 rounded bg-violet-100 px-1 text-[10px] text-violet-700"
                    title={
                      step.event.id
                        ? "这一步要写的章属于该事件，写正文时会注入事件进度"
                        : "事件卡步：产出 大纲/事件/*.md，不写任何章"
                    }
                  >
                    {step.event.id ? `事件 ${step.event.id}` : "事件卡"}
                  </span>
                )}
                {step.task}
              </li>
            ))}
            {collapsed && (
              <li className="text-[10px] text-ink-400">
                … 其余 {steps.length - COLLAPSE_SHOWN} 步已收起（实际执行 {steps.length} 步）
              </li>
            )}
          </ol>

          {waiting ? (
            <div className="mt-2 flex items-center gap-1.5">
              <Button
                size="sm"
                variant="primary"
                disabled={busy}
                onClick={() => void confirmMaster()}
                data-testid="plan-confirm"
              >
                {countdown !== null ? `✅ 确认执行（${countdown}）` : "✅ 确认执行"}
              </Button>
              <Button
                size="sm"
                variant="ghost"
                disabled={busy}
                onClick={() => {
                  setDraft(steps);
                  setEditing(true);
                }}
                data-testid="plan-edit"
              >
                ✏️ 编辑计划
              </Button>
              <Button
                size="sm"
                variant="ghost"
                className="text-rose-600 hover:bg-rose-50"
                disabled={busy}
                onClick={editMasterRequest}
                data-testid="plan-cancel"
              >
                🚫 取消计划
              </Button>
              <span className="ml-auto text-[10px] text-ink-400" data-testid="plan-countdown-hint">
                {countdown !== null
                  ? `${countdown} 秒后自动执行 · 点「编辑/取消」可阻止`
                  : "不确认就什么都不动"}
              </span>
            </div>
          ) : (
            <div className="mt-2 text-[10px] text-ink-400">
              {executed ? "这份计划已执行（或已取消）" : "已确认，等待派发…"}
            </div>
          )}
        </>
      )}
    </div>
  );
}

/** 一条派发记录：谁、干什么、以及这一步的实时输出。 */
function DispatchCard({ payload }: { payload: MasterDispatchPayload }) {
  // 这一张卡就是这一步的现场：心跳按 agentId + index 对上号，显示跑了多久、
  // 吐了多少条输出、落没落盘。派发那一刻看不出区别，跑起来才见真章。
  const progress = useAiStore((s) => s.masterProgress);
  const beat =
    progress && progress.agentId === payload.agentId && progress.index === payload.index
      ? progress
      : null;
  return (
    <div className="rounded-lg border border-accent/30 bg-accent-soft/40 p-2.5" data-testid="master-dispatch">
      <div className="flex items-start gap-2">
        <span className="mt-0.5 text-base leading-none">📤</span>
        <div className="min-w-0 flex-1">
          <div className="text-[11px] leading-relaxed text-ink-700">
            已将任务派发给{" "}
            <span className="font-medium text-ink-900">
              {payload.avatar} {payload.agentName}
            </span>
            <span className="ml-1 font-mono text-[10px] text-ink-400">
              {payload.agentId} · {payload.targetDir}/
            </span>
            <Badge tone="neutral">
              第 {payload.index + 1}/{payload.total} 步
            </Badge>
            {beat && (
              <span className="ml-1 text-[10px] text-ink-500" data-testid="master-dispatch-beat">
                {STATUS_TEXT[beat.status] ?? beat.status} · {formatElapsed(beat.elapsedMs)} ·{" "}
                {beat.chunks} 条输出
                {beat.chapter ? ` · 第 ${beat.chapter} 章` : ""}
              </span>
            )}
          </div>
          <div className="mt-1 rounded bg-white/70 px-2 py-1 text-[11px] leading-relaxed text-ink-800">
            {payload.task}
          </div>
          <StepLog runId={payload.stepRunId} />
        </div>
      </div>
    </div>
  );
}

/** 单个子智能体的输出：正文流 + 折叠的执行详情 + 阻断/体检卡片。 */
function StepLog({ runId }: { runId: string }) {
  const run = useAiStore((s) => s.runs.find((r) => r.id === runId));
  const post = useAiStore((s) => s.postchecks[runId]);
  const dismissBlock = useAiStore((s) => s.dismissBlock);
  const dismissPostCheck = useAiStore((s) => s.dismissPostCheck);
  const applyPostCheck = useAiStore((s) => s.applyPostCheck);
  const planMaster = useAiStore((s) => s.planMaster);
  const workspaceDir = useProjectStore((s) => s.workspaceDir);
  const bookRoot = useProjectStore((s) => s.bookRoot);

  if (!run) {
    return <div className="mt-1.5 text-[10px] text-ink-400">正在启动智能体…</div>;
  }

  const chapter = run.chapter ?? post?.chapter ?? null;

  return (
    <div className="mt-1.5 space-y-1.5">
      <RunTimeline
        chunks={run.chunks}
        running={run.status === "running"}
        mode="text"
        empty="已启动，等待第一行输出…"
        className="max-h-52"
      />

      {run.chunks.length > 0 && (
        <details className="rounded border border-ink-150 bg-white/60">
          <summary className="cursor-pointer px-2 py-1 text-[10px] text-ink-400 hover:text-ink-600">
            执行详情（读写了哪些文件）
          </summary>
          <RunTimeline chunks={run.chunks} mode="trace" className="max-h-40" />
        </details>
      )}

      {run.hookBlock && (
        <HookBlockCard
          block={run.hookBlock}
          busy={false}
          onGenerateOutline={(n) => void planMaster(`生成第 ${n} 章细纲`)}
          onDispatch={(prompt) => void planMaster(prompt)}
          onDismiss={() => dismissBlock(runId)}
        />
      )}

      {post && (
        <PostCheckCard
          post={post}
          onDeslop={() => void planMaster(`把第 ${post.chapter} 章正文去 AI 味`)}
          onRetry={() => {
            if (!workspaceDir || !bookRoot || chapter === null) return;
            void api
              .gatePostcheck(workspaceDir, bookRoot, chapter)
              .then((next) => applyPostCheck(next));
          }}
          onDismiss={() => dismissPostCheck(runId)}
        />
      )}

      {run.status === "done" && (
        <div className="text-[10px] text-emerald-700">
          ✅ 这一步完成{run.durationMs ? `（${(run.durationMs / 1000).toFixed(1)}s）` : ""}
        </div>
      )}
      {run.status === "error" && (
        <div className="whitespace-pre-wrap break-all rounded bg-rose-50 px-2 py-1 text-[10px] text-rose-700">
          执行失败：{run.stderr ?? "未知错误"}
        </div>
      )}
    </div>
  );
}
