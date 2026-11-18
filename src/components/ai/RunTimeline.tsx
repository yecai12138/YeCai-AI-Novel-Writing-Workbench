import { useEffect, useRef, useState, type ReactNode } from "react";

import { Markdown } from "@/components/ai/Markdown";
import { cn } from "@/lib/cn";
import type { RunChunk } from "@/store/aiStore";

/** 把工具名翻译成人看得懂的动作 + 图标。 */
function toolLine(chunk: RunChunk): { icon: string; label: string; detail: string | null } {
  const tool = (chunk.tool ?? "").toLowerCase();
  const target = chunk.target ?? null;

  if (tool === "read" || tool === "readfile" || tool === "read_file") {
    return { icon: "📖", label: "读取文件", detail: target };
  }
  if (tool === "write" || tool === "edit" || tool === "multiedit" || tool === "str_replace") {
    return { icon: "✍️", label: "写入文件", detail: target };
  }
  if (tool === "glob" || tool === "grep" || tool === "search") {
    return { icon: "🔍", label: "检索", detail: target };
  }
  if (tool === "bash" || tool === "shell" || tool === "command") {
    return { icon: "⌨️", label: "执行命令", detail: target };
  }
  if (tool === "webfetch" || tool === "websearch" || tool === "fetch") {
    return { icon: "🌐", label: "联网", detail: target };
  }
  if (tool === "todowrite" || tool === "task") {
    return { icon: "🧭", label: "规划", detail: target };
  }
  return { icon: "🔧", label: chunk.tool ?? "工具调用", detail: target };
}

/**
 * 三种视图：
 * - `text`：只留 AI 说的话（默认对话区，日志不打扰视线）
 * - `trace`：只留状态行与工具调用（塞进「查看执行详情」折叠框）
 * - `all`：全部铺开（旧行为）
 */
export type TimelineMode = "all" | "text" | "trace";

interface RunTimelineProps {
  chunks: RunChunk[];
  running?: boolean;
  mode?: TimelineMode;
  empty?: string;
  className?: string;
}

/**
 * 流式区：text 直接铺开，status 细灰行，tool_use 折叠成一行。
 * tool_use 绝不打印原始 JSON —— 用户要看的是「在读哪个文件」，不是参数结构。
 */
export function RunTimeline({
  chunks,
  running = false,
  mode = "all",
  empty,
  className,
}: RunTimelineProps) {
  const endRef = useRef<HTMLDivElement>(null);
  const pinned = useRef(true);
  const boxRef = useRef<HTMLDivElement>(null);

  const visible =
    mode === "all"
      ? chunks
      : chunks.filter((c) => (mode === "text" ? c.kind === "text" : c.kind !== "text"));

  useEffect(() => {
    const box = boxRef.current;
    if (!box) return;
    const onScroll = () => {
      pinned.current = box.scrollHeight - box.scrollTop - box.clientHeight < 48;
    };
    box.addEventListener("scroll", onScroll);
    return () => box.removeEventListener("scroll", onScroll);
  }, []);

  useEffect(() => {
    if (mode === "trace") return;
    if (pinned.current) endRef.current?.scrollIntoView({ block: "end" });
  }, [visible.length, mode]);

  if (visible.length === 0) {
    return (
      <div
        className={cn(
          "flex items-center justify-center px-6 text-center text-xs leading-relaxed text-ink-400",
          mode === "trace" ? "py-2" : "h-full",
          className,
        )}
      >
        {empty ?? (running ? "等待引擎吐出第一段…" : "选一个动作开始：先生成细纲，再写正文。")}
      </div>
    );
  }

  // 连续的 text 块合并成一段再走 Markdown 渲染：流式吐出的表格/标题经常被
  // 拆成多个 chunk，不合并的话每个碎块都各自成段，表格就散成一行行竖线文本。
  const elements: ReactNode[] = [];
  let textBuf: string[] = [];
  let mdKey = 0;
  const flushText = () => {
    if (textBuf.length === 0) return;
    elements.push(<Markdown key={`md-${mdKey++}`} source={textBuf.join("")} className="text-sm" />);
    textBuf = [];
  };
  for (const chunk of visible) {
    if (chunk.kind === "text") {
      textBuf.push(chunk.text ?? "");
      continue;
    }
    flushText();
    if (chunk.kind === "status") {
      elements.push(
        <div
          key={chunk.seq}
          className="flex items-start gap-1.5 text-[11px] leading-5 text-ink-400"
        >
          <span className="mt-[1px]">·</span>
          <span className="min-w-0 break-words">{chunk.text}</span>
        </div>,
      );
    } else if (chunk.kind === "tool_use") {
      elements.push(<ToolRow key={chunk.seq} chunk={chunk} />);
    }
  }
  flushText();

  return (
    <div
      ref={boxRef}
      className={cn(
        "space-y-2 overflow-y-auto px-3 py-3 text-sm",
        mode === "trace" ? "max-h-56 space-y-1.5 px-2 py-1 text-xs" : "h-full",
        className,
      )}
    >
      {elements}
      {running && mode !== "trace" && (
        <span className="inline-block h-4 w-[2px] animate-pulse bg-accent align-middle" />
      )}
      <div ref={endRef} />
    </div>
  );
}

/**
 * 单条工具调用：默认单行截断，点击展开看完整目标（路径/命令全文），
 * 悬停也有 title 提示。trace 折叠框和全量视图共用这一行。
 */
function ToolRow({ chunk }: { chunk: RunChunk }) {
  const [open, setOpen] = useState(false);
  const { icon, label, detail } = toolLine(chunk);
  return (
    <div
      onClick={
        detail
          ? () => {
              // 这一行点击展开/收起，但里面的路径、命令全文是要能左键选中复制的：
              // 有选区就当成拖选，不切换展开状态（以前这里 select-none，选不动）。
              if (!window.getSelection()?.isCollapsed) return;
              setOpen((v) => !v);
            }
          : undefined
      }
      className={cn(
        "flex items-start gap-2 rounded-md border border-ink-100 bg-ink-50/70 px-2 py-1 text-[12px] text-ink-600",
        detail && "cursor-pointer hover:border-ink-200",
      )}
      title={detail ? (open ? "点击收起" : "点击展开完整内容") : undefined}
    >
      <span className="mt-[1px] shrink-0">{icon}</span>
      <span className="shrink-0 font-medium text-ink-700">{label}</span>
      {detail && (
        <>
          <span className="shrink-0 text-ink-300">：</span>
          <span
            className={cn(
              "min-w-0 flex-1 font-mono text-[11px] text-ink-500",
              open ? "break-all whitespace-pre-wrap" : "truncate",
            )}
          >
            {detail}
          </span>
          <span className="mt-[1px] shrink-0 text-[9px] leading-4 text-ink-300">
            {open ? "▾" : "▸"}
          </span>
        </>
      )}
    </div>
  );
}
