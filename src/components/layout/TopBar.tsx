import { Button } from "@/components/common/Button";
import { useProjectStore } from "@/store/projectStore";
import { useUiStore } from "@/store/uiStore";

/** 顶部条：项目名 + 切换项目 + 环境自检/设置。 */
export function TopBar() {
  const bookName = useProjectStore((s) => s.bookName);
  const workspaceDir = useProjectStore((s) => s.workspaceDir);
  const refreshTree = useProjectStore((s) => s.refreshTree);
  const closeProject = useProjectStore((s) => s.closeProject);
  const setSettingsOpen = useUiStore((s) => s.setSettingsOpen);
  const setStyleStatsOpen = useUiStore((s) => s.setStyleStatsOpen);
  const setNewProjectOpen = useUiStore((s) => s.setNewProjectOpen);

  return (
    <header className="flex h-12 shrink-0 items-center gap-3 border-b border-ink-200 bg-white px-3">
      <div className="flex min-w-0 items-baseline gap-2">
        <span className="truncate text-sm font-semibold text-ink-900">
          {bookName ?? "未命名"}
        </span>
        <span
          className="hidden truncate text-[11px] text-ink-400 sm:inline"
          title={workspaceDir ?? ""}
        >
          {workspaceDir ?? ""}
        </span>
      </div>

      <div className="ml-auto flex items-center gap-1.5">
        <Button
          size="sm"
          variant="ghost"
          onClick={() => setStyleStatsOpen(true)}
          title="全书文风统计：只出计数，不做评分"
        >
          文风体检
        </Button>
        <Button size="sm" variant="ghost" onClick={() => void refreshTree()} title="重新读取磁盘">
          刷新
        </Button>
        <Button size="sm" variant="ghost" onClick={() => setNewProjectOpen(true)}>
          新建项目
        </Button>
        <Button size="sm" variant="ghost" onClick={closeProject} title="关闭当前项目">
          关闭
        </Button>
        <Button size="sm" onClick={() => setSettingsOpen(true)}>
          设置
        </Button>
      </div>
    </header>
  );
}
