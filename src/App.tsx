import { useEffect } from "react";

import { NewProjectDialog } from "@/components/project/NewProjectDialog";
import { SettingsDialog } from "@/components/settings/SettingsDialog";
import { Welcome } from "@/pages/Welcome";
import { Workspace } from "@/pages/Workspace";
import { useAiStore } from "@/store/aiStore";
import { useEditorStore } from "@/store/editorStore";
import { useProjectStore } from "@/store/projectStore";
import { useUiStore } from "@/store/uiStore";

export default function App() {
  const workspaceDir = useProjectStore((s) => s.workspaceDir);
  const restoreLastWorkspace = useProjectStore((s) => s.restoreLastWorkspace);
  const loadEnv = useProjectStore((s) => s.loadEnv);
  // 三个 store 的失败原因合并到同一条底部错误条：以前只有 projectStore 的会被显示，
  // editorStore（保存冲突/切换失败）只在状态栏挤一行，aiStore 的只在设置弹窗里能看到。
  const projectError = useProjectStore((s) => s.error);
  const editorError = useEditorStore((s) => s.error);
  const aiError = useAiStore((s) => s.error);
  const error = projectError ?? editorError ?? aiError;
  const clearError = () => {
    useProjectStore.getState().clearError();
    useEditorStore.getState().clearError();
    useAiStore.getState().clearError();
  };

  const settingsOpen = useUiStore((s) => s.settingsOpen);
  const newProjectOpen = useUiStore((s) => s.newProjectOpen);

  useEffect(() => {
    // 启动时恢复上次的项目；顺手跑一次环境自检，让设置页一打开就有数据
    void restoreLastWorkspace();
    void loadEnv();
  }, [restoreLastWorkspace, loadEnv]);

  return (
    <div className="flex h-full flex-col bg-ink-50 text-ink-900">
      <div className="min-h-0 flex-1">
        {workspaceDir ? <Workspace /> : <Welcome />}
      </div>

      {error && (
        <div className="border-t border-rose-200 bg-rose-50 px-4 py-2 text-sm text-rose-800">
          <div className="mx-auto flex max-w-4xl items-start gap-3">
            <span className="mt-0.5 shrink-0 font-semibold">出错</span>
            <pre className="min-w-0 flex-1 whitespace-pre-wrap break-words font-sans">{error}</pre>
            <button
              type="button"
              onClick={clearError}
              className="shrink-0 rounded border border-rose-300 px-2 py-0.5 text-xs hover:bg-rose-100"
            >
              知道了
            </button>
          </div>
        </div>
      )}

      <SettingsDialog open={settingsOpen} onOpenChange={(o) => useUiStore.getState().setSettingsOpen(o)} />
      <NewProjectDialog open={newProjectOpen} onOpenChange={(o) => useUiStore.getState().setNewProjectOpen(o)} />
    </div>
  );
}
