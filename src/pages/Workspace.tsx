import { useEffect, useState } from "react";

import type { UnlistenFn } from "@tauri-apps/api/event";
import { History, Trash2 } from "lucide-react";

import { AiPanel } from "@/components/ai/AiPanel";
import { Button } from "@/components/common/Button";
import { ConfirmDialog } from "@/components/common/ConfirmDialog";
import { ConflictBar } from "@/components/editor/ConflictBar";
import { MarkdownEditor } from "@/components/editor/MarkdownEditor";
import { StatusBar } from "@/components/layout/StatusBar";
import { ThreePane } from "@/components/layout/ThreePane";
import { TopBar } from "@/components/layout/TopBar";
import { FileTree } from "@/components/tree/FileTree";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { api } from "@/lib/ipc";
import { useAiStore } from "@/store/aiStore";
import { useEditorStore } from "@/store/editorStore";
import { useProjectStore } from "@/store/projectStore";
import type { SnapshotInfo } from "@/types";

/** 主工作区：TopBar + 三栏（文件树 / CodeMirror 编辑器 / AI 面板）+ 状态栏。 */
export function Workspace() {
  useFsWatcher();
  useResetAiPanel();
  useGlobalSaveShortcut();

  return (
    <div className="flex h-full min-h-0 flex-col">
      <TopBar />
      <div className="min-h-0 flex-1">
        <ThreePane
          leftTitle="目录"
          rightTitle="AI 面板"
          left={<FileTree />}
          center={<EditorPane />}
          right={<AiPanel />}
        />
      </div>
    </div>
  );
}

/** 订阅 Rust 侧的 `fs://changed`：磁盘变了就同步编辑器 + 刷新目录树。 */
function useFsWatcher() {
  const workspaceDir = useProjectStore((s) => s.workspaceDir);

  useEffect(() => {
    if (!workspaceDir) return;
    let disposed = false;
    let unlisten: UnlistenFn | null = null;

    void (async () => {
      try {
        await api.watchProject(workspaceDir);
      } catch (err) {
        console.warn("[storydesk] 启动文件监听失败", err);
      }

      const fn = await api.onFsChanged(async (payload) => {
        if (disposed) return;
        await useEditorStore.getState().handleDiskChange(payload.paths);
        void useProjectStore.getState().refreshTree();
      });
      if (disposed) fn();
      else unlisten = fn;
    })();

    return () => {
      disposed = true;
      unlisten?.();
      void api.unwatchProject().catch(() => undefined);
    };
  }, [workspaceDir]);
}

/** 打开/切换项目时把右栏拉回「智能体团队」——用户第一眼该看到的是团队，不是上次的对话记录。 */
function useResetAiPanel() {
  const workspaceDir = useProjectStore((s) => s.workspaceDir);

  useEffect(() => {
    useAiStore.getState().setPanelTab("team");
  }, [workspaceDir]);
}

/** Ctrl/Cmd + S 全局兜底（焦点不在编辑器里时也能立刻保存）。 */
function useGlobalSaveShortcut() {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "s") {
        e.preventDefault();
        void useEditorStore.getState().saveNow();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);
}

function EditorPane() {
  const path = useEditorStore((s) => s.path);
  const rel = useEditorStore((s) => s.rel);
  const loading = useEditorStore((s) => s.loading);
  const dirty = useEditorStore((s) => s.dirty);

  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="flex h-10 shrink-0 items-center gap-2 border-b border-ink-200 bg-white px-3">
        <span className="min-w-0 flex-1 truncate text-xs text-ink-600" title={path ?? ""}>
          {rel || "未选择文件"}
          {dirty && <span className="ml-1.5 text-amber-500">●</span>}
        </span>
        <SnapshotsMenu />
        <Button
          size="sm"
          variant="secondary"
          disabled={!path}
          onClick={() => void useEditorStore.getState().saveNow()}
          title="Ctrl/Cmd + S"
        >
          保存
        </Button>
      </div>

      <ConflictBar />

      <div className="min-h-0 flex-1 overflow-hidden">
        {loading ? (
          <div className="flex h-full items-center justify-center text-xs text-ink-400">
            读取文件…
          </div>
        ) : path ? (
          <MarkdownEditor />
        ) : (
          <div className="flex h-full flex-col items-center justify-center gap-1.5 text-center">
            <p className="text-sm text-ink-500">左侧选一个文件开始写</p>
            <p className="text-[11px] text-ink-400">
              正文写到 <code className="font-mono">正文/第NNN章_章名.md</code>，停手 1 秒自动保存
            </p>
          </div>
        )}
      </div>

      <StatusBar />
    </div>
  );
}

/** `2026-11-05T09-52-13.123Z` → `2026-11-05 09-52-13.123`（保留毫秒，同一秒内的多份也能区分） */
function snapTime(createdAt: string): string {
  return createdAt.replace("T", " ").replace("Z", "");
}

/**
 * 快照面板（Popover）：列出最近 20 份，可「恢复」或物理删除。
 * 删除不可恢复，必须先过二次确认。
 */
function SnapshotsMenu() {
  const path = useEditorStore((s) => s.path);
  const snapshots = useEditorStore((s) => s.snapshots);
  const restore = useEditorStore((s) => s.restoreSnapshot);
  const removeSnapshot = useEditorStore((s) => s.deleteSnapshot);
  const loadSnapshots = useEditorStore((s) => s.loadSnapshots);
  const [open, setOpen] = useState(false);
  const [pending, setPending] = useState<SnapshotInfo | null>(null);

  if (!path) return null;

  return (
    <>
      <Popover
        open={open}
        onOpenChange={(next: boolean) => {
          setOpen(next);
          if (next) void loadSnapshots();
        }}
      >
        <PopoverTrigger asChild>
          <Button size="sm" variant="ghost" title="查看历史快照">
            <History className="h-3.5 w-3.5" /> 快照{snapshots.length > 0 ? ` ${snapshots.length}` : ""}
          </Button>
        </PopoverTrigger>
        <PopoverContent className="w-80">
          <div className="border-b border-ink-100 px-3 py-2 text-[11px] text-ink-500">
            每次覆盖保存前的旧版本（最多留 20 份）
          </div>
          <div className="max-h-72 overflow-y-auto py-1">
            {snapshots.length === 0 ? (
              <div className="px-3 py-3 text-[11px] text-ink-400">
                还没有快照。保存一次后就会出现。
              </div>
            ) : (
              snapshots.map((snap) => (
                <div
                  key={snap.id}
                  className="flex items-center gap-1.5 px-2 py-1.5 text-[11px] hover:bg-ink-50"
                >
                  <span className="min-w-0 flex-1 truncate font-mono text-ink-600" title={snap.path}>
                    {snapTime(snap.createdAt)}
                  </span>
                  <span className="shrink-0 text-[10px] text-ink-400">{snap.bytes}B</span>
                  <button
                    type="button"
                    onClick={() => {
                      void restore(snap.id);
                      setOpen(false);
                    }}
                    className="shrink-0 rounded px-1.5 py-0.5 text-accent hover:bg-accent-soft"
                  >
                    恢复
                  </button>
                  <button
                    type="button"
                    aria-label="删除此版本快照"
                    title="删除此版本快照"
                    onClick={() => setPending(snap)}
                    className="shrink-0 rounded p-1 text-ink-400 hover:bg-rose-50 hover:text-rose-600"
                  >
                    <Trash2 className="h-3 w-3" />
                  </button>
                </div>
              ))
            )}
          </div>
        </PopoverContent>
      </Popover>

      <ConfirmDialog
        open={pending !== null}
        title="删除快照"
        danger
        confirmText="删除"
        message={
          <>
            确定删除此版本快照吗？不可恢复。
            {pending && (
              <div className="mt-1 font-mono text-[11px] text-ink-500">{snapTime(pending.createdAt)}</div>
            )}
          </>
        }
        onConfirm={async () => {
          const target = pending;
          setPending(null);
          if (target) await removeSnapshot(target.id);
        }}
        onClose={() => setPending(null)}
      />
    </>
  );
}
