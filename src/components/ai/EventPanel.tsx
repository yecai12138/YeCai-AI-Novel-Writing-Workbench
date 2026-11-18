import { useEffect, useState } from "react";

import { api } from "@/lib/ipc";
import { useAiStore } from "@/store/aiStore";
import { useProjectStore } from "@/store/projectStore";
import type { EventAudit, EventStatus, EventSummary } from "@/types";

const STATUS_LABEL: Record<EventStatus, string> = {
  planned: "已规划",
  active: "进行中",
  settled: "已结算",
};

const STATUS_DOT: Record<EventStatus, string> = {
  planned: "bg-ink-300",
  active: "bg-amber-500",
  settled: "bg-emerald-500",
};

function spanText(card: EventSummary): string {
  if (!card.span) return "章号未定";
  const [from, to] = card.span;
  return from === to ? `第 ${from} 章` : `第 ${from}—${to} 章`;
}

/**
 * 事件面板：把 `大纲/事件/*.md` 摊在右栏。
 *
 * 事件层是"章与章之间的那根线"，但线本身在文件系统里，写作时看不见。
 * 这里只做三件事：有没有卡、每张卡覆盖到哪、还剩几项没结。
 * 点开一整张卡不在这个面板里做——文件树里就是原文，不重复造一个只读视图。
 */
export function EventPanel() {
  const events = useAiStore((s) => s.events);
  const loadEvents = useAiStore((s) => s.loadEvents);
  const bookRoot = useProjectStore((s) => s.bookRoot);
  const [open, setOpen] = useState(false);
  const [audit, setAudit] = useState<EventAudit | null>(null);
  const [auditing, setAuditing] = useState(false);
  const [auditError, setAuditError] = useState<string | null>(null);

  const runAudit = async () => {
    if (!bookRoot || auditing) return;
    setAuditing(true);
    setAuditError(null);
    try {
      setAudit(await api.eventAudit(bookRoot));
    } catch (err) {
      // 以前这里 catch 完就 setAudit(null)：按钮点了没反应，看起来像「审一遍坏了」。
      setAudit(null);
      setAuditError(err instanceof Error ? err.message : String(err));
    } finally {
      setAuditing(false);
    }
  };

  useEffect(() => {
    if (bookRoot) void loadEvents();
  }, [bookRoot, loadEvents]);

  const cards = events?.cards ?? [];
  const errors = events?.errors ?? [];
  const openCount = cards.reduce((n, c) => n + c.openCount, 0);

  return (
    <div className="shrink-0 border-b border-ink-100 bg-white" data-testid="event-panel">
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        className="flex w-full items-center gap-1.5 px-3 py-1.5 text-left hover:bg-ink-50"
        data-testid="event-panel-toggle"
      >
        <span aria-hidden className="text-[11px] leading-none">
          🧵
        </span>
        <span className="text-[11px] font-medium text-ink-700">事件</span>
        <span className="text-[10px] text-ink-400">
          {events === null
            ? "读取中"
            : cards.length === 0
              ? "还没有事件卡"
              : `${cards.length} 张 · 未结 ${openCount} 项`}
        </span>
        <span className="ml-auto text-[10px] text-ink-300">{open ? "▾" : "▸"}</span>
      </button>

      {open && (
        <div className="max-h-56 overflow-auto px-3 pb-2" data-testid="event-panel-body">
          {cards.length === 0 && (
            <p className="text-[10px] leading-relaxed text-ink-400">
              还没有事件卡。写正文时如果这本书从未排过事件，主 AI 会先插一步"排事件卡"；
              也可以直接说「给这本书排事件卡」。没有事件卡时按老规矩一章一事，不会报错。
            </p>
          )}

          {cards.map((card) => (
            <div
              key={card.id}
              className="mt-1 rounded border border-ink-150 bg-ink-50/50 px-2 py-1"
              data-testid={`event-card-${card.id}`}
            >
              <div className="flex items-baseline gap-1.5">
                <span
                  className={`mt-[5px] h-1.5 w-1.5 shrink-0 rounded-full ${STATUS_DOT[card.status]}`}
                  title={STATUS_LABEL[card.status]}
                />
                <span className="shrink-0 font-mono text-[10px] text-ink-500">{card.id}</span>
                <span className="min-w-0 truncate text-[11px] text-ink-800">{card.title}</span>
                <span className="ml-auto shrink-0 text-[10px] text-ink-400">{spanText(card)}</span>
              </div>
              <div className="mt-0.5 flex items-center gap-1.5 pl-3">
                <span className="text-[10px] text-ink-400">
                  {card.roleCount > 0 ? `已排 ${card.roleCount} 章` : "章型分工还没填"}
                </span>
                <span className="text-[10px] text-ink-500">未结 {card.openCount} 项</span>
                <span
                  className="ml-auto truncate font-mono text-[9px] text-ink-300"
                  title={card.path}
                >
                  {card.path}
                </span>
              </div>
            </div>
          ))}

          <div className="mt-1.5 flex items-center gap-1.5">
            <button
              type="button"
              onClick={() => void runAudit()}
              disabled={auditing}
              className="rounded border border-ink-200 px-1.5 py-0.5 text-[10px] text-ink-600 hover:bg-ink-50 disabled:text-ink-300"
              data-testid="event-audit"
              title="只读检查：一章一事 / 账本从没动过 / 细纲和卡对不上（没有卡也能点，会告诉你是「没卡」而不是「没问题」）"
            >
              {auditing ? "审计中…" : "审一遍"}
            </button>
            {audit && <AuditSummary audit={audit} />}
            {auditError && (
              <span className="text-[10px] text-rose-600" data-testid="event-audit-error">
                审计失败：{auditError}
              </span>
            )}
          </div>

          {audit && audit.findings.length > 0 && (
            <div className="mt-1 rounded border border-amber-200 bg-amber-50 px-2 py-1" data-testid="event-audit-findings">
              {audit.findings.map((f, i) => (
                <p
                  key={`${f.code}-${f.chapter ?? "x"}-${i}`}
                  className={`text-[10px] leading-relaxed ${f.severity === "info" ? "text-ink-600" : "text-amber-900"}`}
                >
                  <span className="font-mono text-amber-700">
                    {f.event ? `事件 ${f.event}` : "—"}
                    {f.chapter != null ? ` · 第 ${f.chapter} 章` : ""}
                  </span>{" "}
                  {f.message}
                </p>
              ))}
            </div>
          )}

          {errors.length > 0 && (
            <div className="mt-1.5 rounded border border-rose-200 bg-rose-50 px-2 py-1">
              {errors.map((e) => (
                <p key={e.path} className="text-[10px] leading-relaxed text-rose-700">
                  <span className="font-mono">{e.path}</span>：{e.message}
                </p>
              ))}
            </div>
          )}
        </div>
      )}
    </div>
  );
}

/** 审计汇总：按严重度分开数，并把扫描范围写出来。
 *
 * 以前只按「有没有卡」分支（`audit.cards === 0` → 「审不出问题」），于是同一屏上能同时
 * 出现「这本书还没有事件卡，审不出问题」和 5 条「细纲声明的事件没有卡」——两句话互相打脸，
 * 人也看不出审计到底看了多少份细纲（实际是全书，不是当前章）。
 */
function AuditSummary({ audit }: { audit: EventAudit }) {
  const scope = `已扫 ${audit.outlinesScanned} 份细纲 · ${audit.declaredChapters} 章有声明 · ${audit.cards} 张卡`;
  const warn = audit.findings.filter((f) => f.severity !== "info").length;
  const info = audit.findings.length - warn;
  if (audit.findings.length === 0) {
    return <span className="text-[10px] text-ink-500">没查出问题（{scope}）</span>;
  }
  const parts: string[] = [];
  if (warn > 0) parts.push(`${warn} 处要处理`);
  if (info > 0) parts.push(`${info} 条提示`);
  return (
    <span className="text-[10px] text-ink-500" data-testid="event-audit-summary">
      {parts.join("、")}（{scope}）
      {audit.cards === 0 ? "——没有事件卡：细纲里的「事件 NNN」只当提示，按一章一事写正文" : ""}
    </span>
  );
}
