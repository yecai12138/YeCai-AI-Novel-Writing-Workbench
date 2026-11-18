import { useCallback, useEffect, useRef, useState } from "react";

import { cn } from "@/lib/cn";

interface ResizeHandleProps {
  /** 归一化后的拖拽增量（px，正数向右） */
  onResize: (delta: number) => void;
  onDoubleClick?: () => void;
  className?: string;
}

/**
 * 竖向分隔条：pointer 事件 + 全局捕获，拖动时不选中文本、不触发编辑器选区。
 */
export function ResizeHandle({ onResize, onDoubleClick, className }: ResizeHandleProps) {
  const [dragging, setDragging] = useState(false);
  const lastX = useRef(0);

  const onPointerDown = useCallback((e: React.PointerEvent<HTMLDivElement>) => {
    e.preventDefault();
    lastX.current = e.clientX;
    setDragging(true);
  }, []);

  useEffect(() => {
    if (!dragging) return;

    const onMove = (e: PointerEvent) => {
      const delta = e.clientX - lastX.current;
      if (delta === 0) return;
      lastX.current = e.clientX;
      onResize(delta);
    };
    const stop = () => setDragging(false);

    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", stop);
    window.addEventListener("pointercancel", stop);
    document.body.style.cursor = "col-resize";

    return () => {
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", stop);
      window.removeEventListener("pointercancel", stop);
      document.body.style.cursor = "";
    };
  }, [dragging, onResize]);

  return (
    <div
      role="separator"
      aria-orientation="vertical"
      onPointerDown={onPointerDown}
      onDoubleClick={onDoubleClick}
      className={cn(
        "group relative z-10 w-px shrink-0 cursor-col-resize bg-ink-200",
        "before:absolute before:inset-y-0 before:-left-1 before:w-2 before:content-['']",
        dragging && "bg-accent",
        className,
      )}
    >
      <div
        className={cn(
          "absolute inset-y-0 -left-[3px] w-[7px] rounded-full transition-colors",
          dragging ? "bg-accent/30" : "group-hover:bg-ink-300/60",
        )}
      />
    </div>
  );
}
