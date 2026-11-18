import type { ReactNode } from "react";

import { useUiStore } from "@/store/uiStore";

import { ResizeHandle } from "./ResizeHandle";

interface ThreePaneProps {
  left: ReactNode;
  center: ReactNode;
  right: ReactNode;
  /** 左栏标题（折叠态竖排展示） */
  leftTitle: string;
  rightTitle: string;
}

/**
 * 三栏布局：左（文件树）/ 中（编辑器）/ 右（AI 面板）。
 * 宽度与折叠状态存在 uiStore，持久化到 localStorage；中栏最小宽度由 flex 保证（min-w-0 + flex-1）。
 */
export function ThreePane({ left, center, right, leftTitle, rightTitle }: ThreePaneProps) {
  const leftWidth = useUiStore((s) => s.leftWidth);
  const rightWidth = useUiStore((s) => s.rightWidth);
  const leftCollapsed = useUiStore((s) => s.leftCollapsed);
  const rightCollapsed = useUiStore((s) => s.rightCollapsed);
  const setLeftWidth = useUiStore((s) => s.setLeftWidth);
  const setRightWidth = useUiStore((s) => s.setRightWidth);
  const toggleLeft = useUiStore((s) => s.toggleLeft);
  const toggleRight = useUiStore((s) => s.toggleRight);

  return (
    <div className="flex h-full min-h-0 w-full overflow-hidden">
      {/* 左栏 */}
      {leftCollapsed ? (
        <RailButton label={leftTitle} onClick={toggleLeft} side="left" />
      ) : (
        <>
          <aside
            style={{ width: leftWidth }}
            className="flex min-h-0 shrink-0 flex-col border-r border-ink-200 bg-white"
          >
            <PaneHeader title={leftTitle} onCollapse={toggleLeft} direction="left" />
            <div className="min-h-0 flex-1 overflow-y-auto">{left}</div>
          </aside>
          <ResizeHandle
            onResize={(d) => setLeftWidth(useUiStore.getState().leftWidth + d)}
            onDoubleClick={() => setLeftWidth(264)}
          />
        </>
      )}

      {/* 中栏 */}
      <main className="flex min-w-0 flex-1 flex-col bg-ink-50">{center}</main>

      {/* 右栏 */}
      {rightCollapsed ? (
        <RailButton label={rightTitle} onClick={toggleRight} side="right" />
      ) : (
        <>
          <ResizeHandle
            onResize={(d) => setRightWidth(useUiStore.getState().rightWidth - d)}
            onDoubleClick={() => setRightWidth(340)}
          />
          <aside
            style={{ width: rightWidth }}
            className="flex min-h-0 shrink-0 flex-col border-l border-ink-200 bg-white"
          >
            <PaneHeader title={rightTitle} onCollapse={toggleRight} direction="right" />
            <div className="min-h-0 flex-1 overflow-y-auto">{right}</div>
          </aside>
        </>
      )}
    </div>
  );
}

function PaneHeader({
  title,
  onCollapse,
  direction,
}: {
  title: string;
  onCollapse: () => void;
  direction: "left" | "right";
}) {
  return (
    <div className="flex h-10 shrink-0 items-center justify-between gap-2 border-b border-ink-100 px-3">
      <span className="truncate text-xs font-semibold uppercase tracking-wide text-ink-500">
        {title}
      </span>
      <button
        type="button"
        onClick={onCollapse}
        title={direction === "left" ? "折叠左栏" : "折叠右栏"}
        className="rounded p-1 text-ink-400 hover:bg-ink-100 hover:text-ink-700"
      >
        <svg viewBox="0 0 16 16" className="h-3.5 w-3.5" fill="none" stroke="currentColor" strokeWidth="1.6">
          {direction === "left" ? (
            <path d="M9.5 4L5.5 8l4 4" strokeLinecap="round" strokeLinejoin="round" />
          ) : (
            <path d="M6.5 4l4 4-4 4" strokeLinecap="round" strokeLinejoin="round" />
          )}
        </svg>
      </button>
    </div>
  );
}

function RailButton({
  label,
  onClick,
  side,
}: {
  label: string;
  onClick: () => void;
  side: "left" | "right";
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      title={`展开${label}`}
      className={`flex w-9 shrink-0 items-center justify-center bg-white text-ink-500 hover:bg-ink-50 hover:text-ink-800 ${
        side === "left" ? "border-r border-ink-200" : "border-l border-ink-200"
      }`}
    >
      <span
        className="text-[11px] font-medium tracking-[0.2em]"
        style={{ writingMode: "vertical-rl" }}
      >
        {label}
      </span>
    </button>
  );
}
