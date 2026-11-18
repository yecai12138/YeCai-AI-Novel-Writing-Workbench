import { Button } from "@/components/common/Button";
import type { HookBlock } from "@/types";

const TITLES: Record<string, string> = {
  missingOutline: "缺少前置文件",
  outlineTooShort: "细纲是空的",
  toxicDebtPrev: "上一章还有未清毒句式欠账",
  eventMissing: "细纲挂的事件卡不存在",
  eventSettled: "这个事件已经结算了",
  eventUnplanned: "这一章没进事件卡的章型分工",
  unknown: "被拦住了（原因未识别）",
};

function titleOf(block: HookBlock): string {
  return TITLES[block.block.kind] ?? "被 hook 拦住";
}

interface HookBlockCardProps {
  block: HookBlock;
  busy: boolean;
  onGenerateOutline: (chapter: number) => void;
  onOpenPrevChapter?: (chapter: number) => void;
  /** 事件类拦截的出口：把这句任务原样交给主 AI（排事件卡 / 补章型分工）。 */
  onDispatch?: (prompt: string) => void;
  onDismiss: () => void;
}

/**
 * 阻断卡片：红色主因 + stderr 原文 + 下一步按钮。
 *
 * 这里的文案对应 `.claude/hooks/guard-outline-before-prose.sh` 的 exit 2 分支，
 * 用户在应用里看到的措辞必须和终端里看到的完全一致，否则没法互相对照。
 */
export function HookBlockCard({
  block,
  busy,
  onGenerateOutline,
  onOpenPrevChapter,
  onDispatch,
  onDismiss,
}: HookBlockCardProps) {
  const chapter = block.chapter ?? (block.block.kind === "missingOutline" ? block.block.chapter : null);
  const eventId =
    block.block.kind === "eventMissing" ||
    block.block.kind === "eventSettled" ||
    block.block.kind === "eventUnplanned"
      ? block.block.event
      : null;

  return (
    <div role="alert" className="m-3 rounded-lg border border-rose-200 bg-rose-50/80 p-3">
      <div className="flex items-start gap-2">
        <span className="mt-[2px] text-base leading-none">⛔</span>
        <div className="min-w-0 flex-1">
          <div className="flex items-baseline justify-between gap-2">
            <h4 className="text-sm font-semibold text-rose-800">{titleOf(block)}</h4>
            <button
              type="button"
              onClick={onDismiss}
              className="shrink-0 text-[11px] text-rose-500 hover:text-rose-700"
            >
              收起
            </button>
          </div>

          <p className="mt-1 text-xs leading-relaxed text-rose-900">{block.reason}</p>

          {block.block.kind === "missingOutline" && (
            <p className="mt-1 font-mono text-[11px] text-rose-700">
              缺：大纲/细纲_第{String(block.block.chapter).padStart(3, "0")}章.md
            </p>
          )}
          {block.block.kind === "outlineTooShort" && (
            <p className="mt-1 font-mono text-[11px] text-rose-700">文件：{block.block.outline}</p>
          )}
          {eventId && (
            <p className="mt-1 font-mono text-[11px] text-rose-700">
              事件：{eventId} · 卡：大纲/事件/事件_{eventId}_*.md
            </p>
          )}

          <details className="mt-2">
            <summary className="cursor-pointer text-[11px] text-rose-600 hover:text-rose-800">
              hook 原文（stderr / 脚本输出）
            </summary>
            <pre className="mt-1 max-h-40 overflow-auto whitespace-pre-wrap break-words rounded border border-rose-200 bg-white/70 p-2 font-mono text-[11px] leading-5 text-ink-700">
{block.stderr.trim()}
            </pre>
          </details>

          <div className="mt-2.5 flex flex-wrap items-center gap-1.5">
            {block.action === "generate_outline" && chapter != null && (
              <Button
                size="sm"
                variant="primary"
                disabled={busy}
                onClick={() => onGenerateOutline(chapter)}
              >
                一键生成第{chapter}章细纲
              </Button>
            )}
            {block.block.kind === "eventMissing" && onDispatch && (
              <Button
                size="sm"
                variant="primary"
                disabled={busy}
                onClick={() => onDispatch(`给这本书排事件卡：按 大纲/事件/ 的格式建卡，覆盖现有的章号区间`)}
              >
                让架构师排事件卡
              </Button>
            )}
            {block.block.kind === "eventUnplanned" && onDispatch && chapter != null && (
              <Button
                size="sm"
                variant="primary"
                disabled={busy}
                onClick={() =>
                  onDispatch(
                    `把第 ${chapter} 章补进事件 ${eventId} 的「章型分工」，并相应更新「未结项」`,
                  )
                }
              >
                补进事件卡
              </Button>
            )}
            {block.block.kind === "eventSettled" && onDispatch && (
              <Button
                size="sm"
                onClick={() =>
                  onDispatch(`看一下事件 ${eventId} 的账本：哪些项是真结算了，哪些是误结`)}
              >
                查这张卡的账本
              </Button>
            )}
            {block.action === "open_prev_chapter" && chapter != null && onOpenPrevChapter && (
              <Button size="sm" onClick={() => onOpenPrevChapter(Math.max(1, chapter - 1))}>
                打开上一章
              </Button>
            )}
            <Button
              size="sm"
              variant="ghost"
              onClick={() => {
                void navigator.clipboard?.writeText(block.stderr);
              }}
            >
              复制原因
            </Button>
          </div>
        </div>
      </div>
    </div>
  );
}
