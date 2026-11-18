import { useEffect, useLayoutEffect, useRef, useState } from "react";

import { cn } from "@/lib/cn";

export interface MenuItem {
  id: string;
  label: string;
  /** 右侧灰色说明（如快捷键、目标目录名） */
  hint?: string;
  danger?: boolean;
  disabled?: boolean;
  onSelect: () => void | Promise<void>;
}

export type MenuEntry = MenuItem | "separator";

interface ContextMenuProps {
  x: number;
  y: number;
  items: MenuEntry[];
  onClose: () => void;
}

/**
 * 右键菜单：绝对定位 + 视口内收边。
 *
 * 用 mousedown 捕获阶段关闭，避免和菜单项自身的 click 打架。
 */
export function ContextMenu({ x, y, items, onClose }: ContextMenuProps) {
  const ref = useRef<HTMLDivElement | null>(null);
  const [pos, setPos] = useState({ x, y });

  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const rect = el.getBoundingClientRect();
    const margin = 8;
    setPos({
      x: Math.min(x, window.innerWidth - rect.width - margin),
      y: Math.min(y, window.innerHeight - rect.height - margin),
    });
  }, [x, y]);

  useEffect(() => {
    const onDown = (e: MouseEvent) => {
      if (!ref.current?.contains(e.target as Node)) onClose();
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("mousedown", onDown, true);
    window.addEventListener("keydown", onKey, true);
    window.addEventListener("blur", onClose);
    return () => {
      window.removeEventListener("mousedown", onDown, true);
      window.removeEventListener("keydown", onKey, true);
      window.removeEventListener("blur", onClose);
    };
  }, [onClose]);

  return (
    <div
      ref={ref}
      role="menu"
      style={{ left: pos.x, top: pos.y }}
      className="fixed z-[60] min-w-[168px] overflow-hidden rounded-lg border border-ink-200 bg-white py-1 shadow-xl animate-fade-in"
    >
      {items.map((item, index) =>
        item === "separator" ? (
          <div key={`sep-${index}`} className="my-1 h-px bg-ink-100" />
        ) : (
          <button
            key={item.id}
            type="button"
            role="menuitem"
            disabled={item.disabled}
            onClick={() => {
              onClose();
              void item.onSelect();
            }}
            className={cn(
              "flex w-full items-center gap-2 px-3 py-1.5 text-left text-[13px] disabled:cursor-not-allowed disabled:text-ink-300",
              item.danger
                ? "text-rose-600 hover:bg-rose-50"
                : "text-ink-700 hover:bg-ink-100",
            )}
          >
            <span className="flex-1 truncate">{item.label}</span>
            {item.hint && (
              <span className="shrink-0 text-[10px] text-ink-400">{item.hint}</span>
            )}
          </button>
        ),
      )}
    </div>
  );
}
