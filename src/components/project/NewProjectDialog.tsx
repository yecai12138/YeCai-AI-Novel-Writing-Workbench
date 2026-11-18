import { useEffect, useState } from "react";

import { Button } from "@/components/common/Button";
import { Dialog } from "@/components/common/Dialog";
import { Field, TextInput } from "@/components/common/Field";
import { FolderPicker } from "@/components/project/FolderPicker";
import { useProjectStore } from "@/store/projectStore";

const WILL_CREATE = [
  "设定/（世界观 / 角色 / 势力 / 关系.md / 题材定位.md / 全书梗概.md / 文风.md）",
  "大纲/（大纲.md，后续细纲_第NNN章.md）",
  "正文/（第NNN章_章名.md）",
  "追踪/（逐章记录 / 角色状态 / 时间线 / 上下文.md / 伏笔.md）",
  "对标/ 参考资料/",
  ".story/排纲笔记/（oh-story 内部区，应用只建空目录）",
  ".storydesk/（应用私有：snapshots / runs / logs / prompts）",
  ".active-book（内容为相对路径：长篇/<书名>）",
];

interface NewProjectDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
}

export function NewProjectDialog({ open, onOpenChange }: NewProjectDialogProps) {
  const createProject = useProjectStore((s) => s.createProject);
  const loading = useProjectStore((s) => s.loading);
  const lastWorkspace = useProjectStore((s) => s.workspaceDir);

  const [dir, setDir] = useState(lastWorkspace ?? "");
  const [name, setName] = useState("");
  const [pickerOpen, setPickerOpen] = useState(false);
  const [localError, setLocalError] = useState<string | null>(null);

  useEffect(() => {
    if (open) {
      setDir(lastWorkspace ?? "");
      setName("");
      setLocalError(null);
    }
  }, [open, lastWorkspace]);

  const previewBookRoot = dir && name ? `${dir.replace(/[\\/]+$/, "")}\\长篇\\${name.trim()}` : null;

  const submit = async () => {
    setLocalError(null);
    if (!dir.trim()) {
      setLocalError("请先选择或填写项目目录");
      return;
    }
    if (!name.trim()) {
      setLocalError("请填写书名");
      return;
    }
    const ok = await createProject(dir.trim(), name.trim());
    if (ok) onOpenChange(false);
  };

  return (
    <>
      <Dialog
        open={open}
        onClose={() => onOpenChange(false)}
        title="新建项目"
        description="选一个目录作为工作区根，应用会在它下面创建 oh-story 兼容的长篇目录结构。"
        footer={
          <>
            <Button variant="ghost" onClick={() => onOpenChange(false)}>
              取消
            </Button>
            <Button variant="primary" onClick={() => void submit()} disabled={loading}>
              {loading ? "创建中…" : "创建项目"}
            </Button>
          </>
        }
      >
        <div className="space-y-4">
          <Field label="项目目录" hint="工作区根，可填不存在的路径（会自动创建）">
            <div className="flex gap-2">
              <TextInput
                value={dir}
                onChange={(e) => setDir(e.target.value)}
                placeholder="D:\桌面\大制作\小说\我的作品集"
                className="font-mono text-xs"
              />
              <Button size="sm" className="h-9" onClick={() => setPickerOpen(true)}>
                浏览
              </Button>
            </div>
          </Field>

          <Field label="书名" hint={'不能含 \\ / : * ? " < > |'}>
            <TextInput
              value={name}
              onChange={(e) => setName(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") void submit();
              }}
              placeholder="例如：山海不夜"
            />
          </Field>

          {previewBookRoot && (
            <div className="rounded-md border border-ink-200 bg-ink-50 px-3 py-2">
              <div className="text-[11px] font-medium text-ink-500">书根将创建在</div>
              <div className="mt-0.5 break-all font-mono text-[11px] text-ink-800">
                {previewBookRoot}
              </div>
            </div>
          )}

          <div className="rounded-md border border-ink-200 bg-white px-3 py-2">
            <div className="mb-1 text-[11px] font-medium text-ink-500">将创建</div>
            <ul className="space-y-0.5">
              {WILL_CREATE.map((item) => (
                <li key={item} className="flex gap-1.5 text-[11px] leading-relaxed text-ink-600">
                  <span className="mt-[3px] h-1 w-1 shrink-0 rounded-full bg-ink-300" />
                  <span className="break-all">{item}</span>
                </li>
              ))}
            </ul>
          </div>

          {localError && (
            <div className="rounded border border-rose-200 bg-rose-50 px-2.5 py-2 text-xs text-rose-700">
              {localError}
            </div>
          )}
        </div>
      </Dialog>

      <FolderPicker
        open={pickerOpen}
        onClose={() => setPickerOpen(false)}
        onPick={(p) => setDir(p)}
        initialPath={dir}
        preferEmpty
        title="选择项目目录"
      />
    </>
  );
}
