import { useState } from "react";

import { Button } from "@/components/common/Button";
import { FolderPicker } from "@/components/project/FolderPicker";
import { isTauriRuntime } from "@/lib/ipc";
import { useProjectStore } from "@/store/projectStore";
import { useUiStore } from "@/store/uiStore";

/** 未打开项目时的欢迎页：新建 / 打开，以及一步到位的环境自检入口。 */
export function Welcome() {
  const openWorkspace = useProjectStore((s) => s.openWorkspace);
  const loading = useProjectStore((s) => s.loading);
  const env = useProjectStore((s) => s.env);
  const setNewProjectOpen = useUiStore((s) => s.setNewProjectOpen);
  const setSettingsOpen = useUiStore((s) => s.setSettingsOpen);

  const [pickerOpen, setPickerOpen] = useState(false);

  return (
    <div className="flex h-full items-center justify-center bg-gradient-to-b from-white to-ink-100 px-6">
      <div className="w-full max-w-2xl">
        <div className="mb-6 flex items-center gap-3">
          <div className="flex h-11 w-11 items-center justify-center rounded-xl bg-accent text-lg font-bold text-white shadow-sm">
            刃
          </div>
          <div>
            <h1 className="text-xl font-semibold tracking-tight text-ink-900">
              野菜·AI小说工作台
            </h1>
            <p className="mt-0.5 text-xs text-ink-500">
              桌面外壳，写作流程跑在 oh-story-claudecode 的 skill 上
            </p>
          </div>
        </div>

        <div className="rounded-xl border border-ink-200 bg-white p-5 shadow-sm">
          <h2 className="text-sm font-semibold text-ink-800">开始</h2>
          <p className="mt-1 text-xs leading-relaxed text-ink-500">
            新建项目会在你选的目录下创建{" "}
            <code className="rounded bg-ink-100 px-1 font-mono text-[11px]">长篇/&lt;书名&gt;/</code>{" "}
            与 <code className="rounded bg-ink-100 px-1 font-mono text-[11px]">设定/ 大纲/ 正文/ 追踪/</code>
            ，并写入 <code className="rounded bg-ink-100 px-1 font-mono text-[11px]">.active-book</code>。
          </p>

          <div className="mt-4 flex flex-wrap gap-2">
            <Button variant="primary" onClick={() => setNewProjectOpen(true)}>
              新建项目
            </Button>
            <Button onClick={() => setPickerOpen(true)} disabled={loading}>
              {loading ? "打开中…" : "打开项目"}
            </Button>
            <Button variant="ghost" onClick={() => setSettingsOpen(true)}>
              环境自检
            </Button>
          </div>

          {!isTauriRuntime() && (
            <div className="mt-4 rounded border border-amber-200 bg-amber-50 px-3 py-2 text-[11px] leading-relaxed text-amber-800">
              当前是浏览器预览环境：没有 Rust 后端，所有 invoke 都会失败。
              请用 <code className="font-mono">npm run tauri dev</code> 启动桌面应用。
            </div>
          )}

          {env && (
            <div className="mt-4 flex flex-wrap items-center gap-x-4 gap-y-1 border-t border-ink-100 pt-3 text-[11px] text-ink-500">
              <span>
                Node {env.node.ok ? "✓" : "✗"} {env.node.version ?? ""}
              </span>
              <span>Python {env.python.ok ? "✓" : "✗"}</span>
              <span>bash {env.bash.ok ? "✓" : "✗"}</span>
              <span>
                宿主 CLI {env.engine.ok ? `✓ ${env.engine.version ?? ""}` : "✗ 未安装"}
              </span>
              <button
                type="button"
                className="ml-auto text-accent hover:underline"
                onClick={() => setSettingsOpen(true)}
              >
                查看详情
              </button>
            </div>
          )}
        </div>

        <p className="mt-4 text-center text-[11px] text-ink-400">
          阶段 2：三栏骨架 · 新建项目 · 环境自检｜编辑器与 AI 面板为占位
        </p>
      </div>

      <FolderPicker
        open={pickerOpen}
        onClose={() => setPickerOpen(false)}
        initialPath=""
        title="打开项目（选择工作区根目录）"
        onPick={(p) => void openWorkspace(p)}
      />
    </div>
  );
}
