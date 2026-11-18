import { useCallback, useEffect, useState } from "react";

import { Button } from "@/components/common/Button";
import { Dialog } from "@/components/common/Dialog";
import { TextInput } from "@/components/common/Field";
import { api } from "@/lib/ipc";
import { cn } from "@/lib/cn";
import type { DirListing } from "@/types";
import { errorText } from "@/store/projectStore";

interface FolderPickerProps {
  open: boolean;
  onClose: () => void;
  onPick: (path: string) => void;
  title?: string;
  /** 打开时的起始目录；空字符串表示从盘符开始 */
  initialPath?: string;
  /** 已选目录是否必须是空目录（新建项目时提示用，不强制） */
  preferEmpty?: boolean;
}

/**
 * 应用内目录选择器。刻意不使用 tauri-plugin-dialog：
 * 该插件会往 capabilities 里加权限，而本阶段的能力清单只保留 core:default + 自定义命令。
 * 目录列举同样走 Rust 命令（`list_dirs`），渲染进程依然没有任何 fs 权限。
 */
export function FolderPicker({
  open,
  onClose,
  onPick,
  title = "选择文件夹",
  initialPath = "",
  preferEmpty = false,
}: FolderPickerProps) {
  const [path, setPath] = useState(initialPath);
  const [listing, setListing] = useState<DirListing | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async (target: string) => {
    setLoading(true);
    setError(null);
    try {
      const result = await api.listDirs(target);
      setListing(result);
      setPath(result.current);
    } catch (err) {
      setError(errorText(err));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    if (open) void load(initialPath);
  }, [open, initialPath, load]);

  // 目录本身是否为空：用于提示（不阻断）
  const currentIsEmpty = listing?.entries.length === 0;

  return (
    <Dialog
      open={open}
      onClose={onClose}
      title={title}
      description="只列出目录；路径全部由 Rust 侧校验。"
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>
            取消
          </Button>
          <Button
            variant="primary"
            disabled={!listing || !listing.current}
            onClick={() => {
              if (listing?.current) {
                onPick(listing.current);
                onClose();
              }
            }}
          >
            选择此文件夹
          </Button>
        </>
      }
    >
      <div className="space-y-3">
        <div className="flex items-center gap-2">
          <Button
            size="sm"
            disabled={!listing?.parent}
            onClick={() => listing?.parent && void load(listing.parent)}
          >
            上一级
          </Button>
          <Button size="sm" variant="ghost" onClick={() => void load("")}>
            盘符
          </Button>
          <TextInput
            value={path}
            onChange={(e) => setPath(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") void load(path);
            }}
            placeholder="C:\Users\...\小说"
            className="h-8 font-mono text-xs"
          />
          <Button size="sm" onClick={() => void load(path)} disabled={loading}>
            转到
          </Button>
        </div>

        {error && (
          <div className="rounded border border-rose-200 bg-rose-50 px-2.5 py-2 text-xs text-rose-700">
            {error}
          </div>
        )}

        <div className="h-64 overflow-y-auto rounded-md border border-ink-200 bg-white">
          {loading && <div className="px-3 py-3 text-xs text-ink-400">读取中…</div>}
          {!loading && listing && listing.entries.length === 0 && (
            <div className="px-3 py-3 text-xs text-ink-400">没有子目录</div>
          )}
          {!loading &&
            listing?.entries.map((entry) => (
              <button
                key={entry.path}
                type="button"
                onClick={() => void load(entry.path)}
                className={cn(
                  "flex w-full items-center gap-2 border-b border-ink-50 px-3 py-2 text-left text-xs",
                  "hover:bg-ink-50",
                )}
              >
                <svg viewBox="0 0 16 16" className="h-3.5 w-3.5 shrink-0 text-ink-400" fill="none" stroke="currentColor" strokeWidth="1.3">
                  <path d="M1.8 4.2h3.4l1.2 1.4h7.8v6.6a.8.8 0 01-.8.8H2.6a.8.8 0 01-.8-.8V4.2z" strokeLinejoin="round" />
                </svg>
                <span className="min-w-0 flex-1 truncate text-ink-700">{entry.name}</span>
                {entry.isEmpty && (
                  <span className="shrink-0 text-[10px] text-emerald-600">空</span>
                )}
              </button>
            ))}
        </div>

        {preferEmpty && currentIsEmpty && listing?.current && (
          <p className="text-[11px] text-emerald-700">当前目录为空，适合作为新项目目录。</p>
        )}
      </div>
    </Dialog>
  );
}
