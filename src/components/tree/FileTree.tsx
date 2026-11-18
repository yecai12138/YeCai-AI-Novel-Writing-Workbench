import { useEffect, useRef, useState } from "react";

import { ConfirmDialog } from "@/components/common/ConfirmDialog";
import { ContextMenu, type MenuEntry } from "@/components/common/ContextMenu";
import { PromptDialog } from "@/components/common/PromptDialog";
import { api } from "@/lib/ipc";
import { cn } from "@/lib/cn";
import { useEditorStore } from "@/store/editorStore";
import { errorText, useProjectStore } from "@/store/projectStore";
import type { TreeNode } from "@/types";

type PromptKind = "chapter" | "note" | "rename";

interface PromptState {
  kind: PromptKind;
  node: TreeNode;
}

interface MenuState {
  x: number;
  y: number;
  node: TreeNode;
}

/** 左侧文件树：点击打开、右键菜单（新建章节/新建文件/重命名/删除）。 */
export function FileTree() {
  const tree = useProjectStore((s) => s.tree);
  const loading = useProjectStore((s) => s.loading);
  const selectedPath = useProjectStore((s) => s.selectedPath);
  const select = useProjectStore((s) => s.select);

  const [expanded, setExpanded] = useState<Record<string, boolean>>({});
  const [menu, setMenu] = useState<MenuState | null>(null);
  const [prompt, setPrompt] = useState<PromptState | null>(null);
  const [pendingDelete, setPendingDelete] = useState<TreeNode | null>(null);
  const [toast, setToast] = useState<string | null>(null);
  // 对标导入兜底：付费书抓不到时，用户自己导出 txt/md，右键 对标/ 目录导进来。
  const importTarget = useRef<TreeNode | null>(null);
  const importInputRef = useRef<HTMLInputElement>(null);

  // 默认展开根与第一层，但**保留用户手工折叠过的节点**（保存会自动刷新树，不能每次把它弹开）
  useEffect(() => {
    if (!tree) {
      setExpanded({});
      return;
    }
    setExpanded((prev) => {
      const next = { ...prev };
      if (!(tree.path in next)) next[tree.path] = true;
      for (const child of tree.children) {
        if (child.kind === "dir" && !(child.path in next)) next[child.path] = true;
      }
      return next;
    });
  }, [tree]);

  useEffect(() => {
    if (!toast) return;
    const timer = setTimeout(() => setToast(null), 4000);
    return () => clearTimeout(timer);
  }, [toast]);

  const toggle = (path: string) => setExpanded((prev) => ({ ...prev, [path]: !prev[path] }));

  // openFile 返回 false = 上一个文件没保存成功，这次根本没切换：必须当场说清楚，
  // 否则用户看到的是「点了没反应」（内容其实还在，只是没动）。
  const openFile = (node: TreeNode) => {
    select(node);
    void useEditorStore
      .getState()
      .openFile(node.path, node.rel)
      .then((ok) => {
        if (!ok) setToast(useEditorStore.getState().error ?? "没能打开这个文件");
      });
  };

  const workspaceDir = useProjectStore((s) => s.workspaceDir);
  const bookRoot = useProjectStore((s) => s.bookRoot);
  const refreshTree = useProjectStore((s) => s.refreshTree);

  const menuItems = (node: TreeNode): MenuEntry[] => {
    const isDir = node.kind === "dir";
    const isRoot = node.rel === "";
    const items: MenuEntry[] = [];

    if (!isDir) {
      items.push({ id: "open", label: "打开", onSelect: () => openFile(node) });
    }

    // 「新建章节」只对正文目录（或书根，自动落到 正文/）出现
    if (isDir && (isRoot || node.rel === "正文")) {
      items.push({
        id: "chapter",
        label: "新建章节",
        hint: "正文/第NNN章_…",
        onSelect: () => setPrompt({ kind: "chapter", node }),
      });
    }

    if (isDir) {
      items.push({
        id: "note",
        label: "新建 Markdown 文件",
        hint: node.rel || "书根",
        onSelect: () => setPrompt({ kind: "note", node }),
      });
    }

    // 「导入对标文本」只在 对标/（及其子目录）出现：付费书抓不到时的兜底通道
    if (isDir && (node.rel === "对标" || node.rel.startsWith("对标/"))) {
      items.push({
        id: "import-bench",
        label: "导入对标文本…",
        hint: "付费书导出 txt/md",
        onSelect: () => {
          importTarget.current = node;
          importInputRef.current?.click();
        },
      });
    }

    if (!isRoot) {
      if (items.length > 0) items.push("separator");
      items.push({
        id: "rename",
        label: "重命名…",
        onSelect: () => setPrompt({ kind: "rename", node }),
      });
      items.push({
        id: "delete",
        label: "删除…",
        danger: true,
        onSelect: () => setPendingDelete(node),
      });
    }

    return items;
  };

  const submitChapter = async (title: string): Promise<string | null> => {
    if (!workspaceDir || !bookRoot) return "还没有打开项目";
    try {
      const file = await api.createChapter(workspaceDir, bookRoot, null, title);
      await refreshTree();
      const opened = await useEditorStore.getState().openFile(file.path, file.rel);
      setExpanded((prev) => ({ ...prev, [dirPathOf(file.rel)]: true }));
      // 章节已经建好了，只是没能打开：留在对话框里说明，别让用户以为没建成又点一次。
      if (!opened) {
        return `章节已建好（${file.rel}），但没能打开：${
          useEditorStore.getState().error ?? "当前文件没保存成功"
        }`;
      }
      return null;
    } catch (err) {
      return errorText(err);
    }
  };

  const submitNote = async (name: string): Promise<string | null> => {
    if (!workspaceDir) return "还没有打开项目";
    const target = prompt?.node;
    if (!target) return "没有目标目录";
    const dir = target.kind === "dir" ? target.path : parentOf(target.path);
    try {
      const file = await api.createNote(workspaceDir, dir, name);
      await refreshTree();
      if (target.kind === "dir") setExpanded((prev) => ({ ...prev, [target.path]: true }));
      const opened = await useEditorStore.getState().openFile(file.path, file.rel);
      if (!opened) {
        return `文件已建好（${file.rel}），但没能打开：${
          useEditorStore.getState().error ?? "当前文件没保存成功"
        }`;
      }
      return null;
    } catch (err) {
      return errorText(err);
    }
  };

  const submitRename = async (name: string): Promise<string | null> => {
    if (!workspaceDir) return "还没有打开项目";
    const target = prompt?.node;
    if (!target) return "没有目标";
    try {
      const file = await api.renameEntry(workspaceDir, target.path, name);
      await refreshTree();
      // 改的正是当前打开的文件：编辑器跟着换路径，别断连
      const editor = useEditorStore.getState();
      if (editor.path === target.path) {
        const opened = await editor.openFile(file.path, file.rel);
        if (!opened) {
          // 改名已经落盘，只是编辑器还停在旧路径：明确告知，别让用户以为改名失败又改一遍。
          setToast(
            `已重命名为 ${file.rel}，但没能重新打开：${
              useEditorStore.getState().error ?? "当前文件没保存成功"
            }`,
          );
          return null;
        }
        select({ ...target, path: file.path, rel: file.rel, name: file.rel.split("/").pop() ?? name });
      }
      return null;
    } catch (err) {
      return errorText(err);
    }
  };

  const confirmDelete = async () => {
    if (!workspaceDir || !pendingDelete) return;
    const target = pendingDelete;
    try {
      await api.deleteEntry(workspaceDir, target.path);
      if (useEditorStore.getState().path === target.path) {
        const closed = await useEditorStore.getState().closeFile();
        if (!closed) {
          // 文件已进回收站，但编辑器里还有没保存的字：不能静默关掉，也不能假装关掉了。
          setToast(
            `已删除 ${target.rel}，但编辑器里的未保存内容没能关掉：${
              useEditorStore.getState().error ?? "当前内容没保存成功"
            }`,
          );
        }
      }
      await refreshTree();
      if (useProjectStore.getState().selectedPath === target.path) {
        useProjectStore.setState({ selectedPath: null, selectedRel: null });
      }
    } catch (err) {
      setToast(errorText(err));
    }
  };

  // 把用户选中的本地 txt/md 读成文本，写进 对标/ 目标目录（浏览器文件输入，不需要额外权限）。
  const runBenchmarkImport = async (files: FileList | null) => {
    const target = importTarget.current;
    importTarget.current = null;
    if (!target || !files || files.length === 0) return;
    if (!workspaceDir) {
      setToast("还没有打开项目");
      return;
    }
    let ok = 0;
    let lastErr = "";
    for (const file of Array.from(files)) {
      try {
        const text = await file.text();
        const base = file.name.replace(/\.(txt|md|markdown)$/i, "") || "导入文本";
        // 去掉 Windows 文件名非法字符（中文保留，只替换分隔符与保留符号）。
        const safe = base.replace(/[\\/:*?"<>|]/g, "_");
        await api.writeFile(workspaceDir, `${target.path}\\${safe}.md`, text);
        ok += 1;
      } catch (err) {
        lastErr = errorText(err);
      }
    }
    await refreshTree();
    setExpanded((prev) => ({ ...prev, [target.path]: true }));
    setToast(
      ok > 0
        ? `已导入 ${ok} 个文本到 ${target.rel}/，对 benchmarker 说「拆对标」即可拆解`
        : `导入失败：${lastErr || "未知错误"}`,
    );
  };

  if (loading && !tree) {
    return <div className="px-3 py-4 text-xs text-ink-400">读取目录中…</div>;
  }

  if (!tree) {
    return <div className="px-3 py-4 text-xs text-ink-400">还没有项目</div>;
  }

  return (
    <div className="relative py-1 text-[13px]">
      <TreeNodeRow
        node={tree}
        depth={0}
        expanded={expanded}
        toggle={toggle}
        selectedPath={selectedPath}
        onOpen={openFile}
        onContextMenu={(node, e) => {
          e.preventDefault();
          setMenu({ x: e.clientX, y: e.clientY, node });
        }}
        isRoot
      />

      <input
        ref={importInputRef}
        type="file"
        accept=".txt,.md,.markdown"
        multiple
        className="hidden"
        onChange={(e) => {
          void runBenchmarkImport(e.target.files);
          e.target.value = "";
        }}
      />

      {toast && (
        <div className="mx-2 mt-2 rounded border border-rose-200 bg-rose-50 px-2 py-1.5 text-[11px] leading-relaxed text-rose-700">
          {toast}
        </div>
      )}

      {menu && (
        <ContextMenu
          x={menu.x}
          y={menu.y}
          items={menuItems(menu.node)}
          onClose={() => setMenu(null)}
        />
      )}

      <PromptDialog
        open={prompt?.kind === "chapter"}
        title="新建章节"
        label="章节标题"
        placeholder="例如：雨夜来客"
        hint="章号自动递增，文件名形如 第002章_雨夜来客.md"
        confirmText="创建并打开"
        onSubmit={submitChapter}
        onClose={() => setPrompt(null)}
      />

      <PromptDialog
        open={prompt?.kind === "note"}
        title="新建 Markdown 文件"
        label="文件名"
        placeholder="例如：世界观"
        hint={prompt ? `创建在 ${prompt.node.rel || "书根"}/` : undefined}
        confirmText="创建并打开"
        onSubmit={submitNote}
        onClose={() => setPrompt(null)}
      />

      <PromptDialog
        open={prompt?.kind === "rename"}
        title="重命名"
        label="新名称"
        defaultValue={prompt?.node.name ?? ""}
        hint={prompt ? (prompt.node.kind === "file" ? "包含扩展名，如 第001章_开篇.md" : "目录名") : undefined}
        confirmText="重命名"
        onSubmit={submitRename}
        onClose={() => setPrompt(null)}
      />

      <ConfirmDialog
        open={!!pendingDelete}
        title="删除这一项？"
        danger
        confirmText="删除"
        message={
          <>
            <p>
              将删除 <span className="font-mono text-[12px] text-ink-900">{pendingDelete?.rel}</span>
            </p>
            <p className="mt-2 text-xs text-ink-500">
              文件不会真被抹掉：会移进 <span className="font-mono">.storydesk/trash/&lt;时间戳&gt;/</span>，
              需要时可以从磁盘捡回来。
            </p>
          </>
        }
        onConfirm={confirmDelete}
        onClose={() => setPendingDelete(null)}
      />
    </div>
  );
}

interface RowProps {
  node: TreeNode;
  depth: number;
  expanded: Record<string, boolean>;
  toggle: (path: string) => void;
  selectedPath: string | null;
  onOpen: (node: TreeNode) => void;
  onContextMenu: (node: TreeNode, e: React.MouseEvent) => void;
  isRoot?: boolean;
}

function TreeNodeRow({
  node,
  depth,
  expanded,
  toggle,
  selectedPath,
  onOpen,
  onContextMenu,
  isRoot = false,
}: RowProps) {
  const isDir = node.kind === "dir";
  const open = !!expanded[node.path];
  const selected = selectedPath === node.path;

  return (
    <div>
      <button
        type="button"
        onClick={() => {
          if (isDir) toggle(node.path);
          else onOpen(node);
        }}
        onContextMenu={(e) => onContextMenu(node, e)}
        title={node.rel || node.name}
        className={cn(
          "group flex w-full items-center gap-1.5 rounded-sm py-[3px] pr-2 text-left",
          "hover:bg-ink-100/70",
          selected && "bg-accent-soft text-accent-strong hover:bg-accent-soft",
        )}
        style={{ paddingLeft: 6 + depth * 12 }}
      >
        <span className="flex h-3.5 w-3.5 shrink-0 items-center justify-center">
          {isDir ? (
            <svg
              viewBox="0 0 16 16"
              className={cn("h-3 w-3 text-ink-400 transition-transform", open && "rotate-90")}
              fill="currentColor"
            >
              <path d="M6 3.5l5 4.5-5 4.5z" />
            </svg>
          ) : (
            <span className="h-1 w-1 rounded-full bg-ink-300" />
          )}
        </span>

        <span className="shrink-0 text-ink-400">
          {isDir ? (
            <svg viewBox="0 0 16 16" className="h-3.5 w-3.5" fill="none" stroke="currentColor" strokeWidth="1.3">
              <path d="M1.8 4.2h3.4l1.2 1.4h7.8v6.6a.8.8 0 01-.8.8H2.6a.8.8 0 01-.8-.8V4.2z" strokeLinejoin="round" />
            </svg>
          ) : (
            <svg viewBox="0 0 16 16" className="h-3.5 w-3.5" fill="none" stroke="currentColor" strokeWidth="1.3">
              <path d="M3.5 2.2h5.3l3.7 3.6v8a.6.6 0 01-.6.6H3.5a.6.6 0 01-.6-.6V2.8a.6.6 0 01.6-.6z" strokeLinejoin="round" />
              <path d="M8.7 2.4v3.6h3.6" />
            </svg>
          )}
        </span>

        <span
          className={cn(
            "min-w-0 flex-1 truncate",
            isRoot ? "font-semibold text-ink-800" : "text-ink-700",
            !isDir && selected && "font-medium",
          )}
        >
          {node.name}
        </span>

        {!isDir && node.size !== null && (
          <span className="shrink-0 text-[10px] tabular-nums text-ink-300 group-hover:text-ink-400">
            {formatSize(node.size)}
          </span>
        )}
      </button>

      {isDir && open && node.children.length > 0 && (
        <div>
          {node.children.map((child) => (
            <TreeNodeRow
              key={child.path}
              node={child}
              depth={depth + 1}
              expanded={expanded}
              toggle={toggle}
              selectedPath={selectedPath}
              onOpen={onOpen}
              onContextMenu={onContextMenu}
            />
          ))}
        </div>
      )}

      {isDir && open && node.children.length === 0 && (
        <div
          className="py-[2px] text-[11px] italic text-ink-300"
          style={{ paddingLeft: 6 + (depth + 1) * 12 + 22 }}
        >
          空目录（右键新建）
        </div>
      )}
    </div>
  );
}

function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes}B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)}K`;
  return `${(bytes / 1024 / 1024).toFixed(1)}M`;
}

/** `正文/第001章_x.md` → `正文`（用于展开对应目录）。 */
function dirPathOf(rel: string): string {
  const parts = rel.split("/");
  if (parts.length <= 1) return rel;
  const dirRel = parts.slice(0, -1).join("/");
  const tree = useProjectStore.getState().tree;
  const found = tree ? findNode(tree, dirRel) : null;
  return found ? found.path : dirRel;
}

function parentOf(path: string): string {
  const normalized = path.replace(/\\/g, "/");
  return normalized.slice(0, normalized.lastIndexOf("/")).replace(/\//g, "\\");
}

function findNode(node: TreeNode, rel: string): TreeNode | null {
  if (node.rel === rel) return node;
  for (const child of node.children) {
    const hit = findNode(child, rel);
    if (hit) return hit;
  }
  return null;
}
