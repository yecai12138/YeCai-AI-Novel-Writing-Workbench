import { Button } from "@/components/common/Button";
import { useEditorStore } from "@/store/editorStore";

/**
 * 冲突提示栏：编辑器有未保存改动，同时磁盘上的文件被别人改了。
 *
 * 两个选项都不丢数据：
 * - 重新加载 = 放弃本地改动，跟随磁盘；
 * - 保留我的版本 = 磁盘内容另存 `<文件>.bak`，再把自己的内容写回去。
 */
export function ConflictBar() {
  const conflict = useEditorStore((s) => s.conflict);
  const reload = useEditorStore((s) => s.reloadFromDisk);
  const keepMine = useEditorStore((s) => s.keepMine);

  if (!conflict) return null;

  const detected = new Date(conflict.detectedAt).toLocaleTimeString("zh-CN", {
    hour12: false,
  });

  return (
    <div
      role="alert"
      className="flex shrink-0 flex-wrap items-center gap-2 border-b border-amber-200 bg-amber-50 px-3 py-2 text-xs text-amber-900"
    >
      <svg viewBox="0 0 16 16" className="h-3.5 w-3.5 shrink-0" fill="none" stroke="currentColor" strokeWidth="1.5">
        <path d="M8 1.8l6.2 11.4H1.8L8 1.8z" strokeLinejoin="round" />
        <path d="M8 6v3.4M8 11.2v.9" strokeLinecap="round" />
      </svg>
      <span className="font-medium">磁盘版本已更新</span>
      <span className="text-amber-700">
        检测于 {detected}，你本地还有未保存的改动 →
      </span>
      <span className="ml-auto flex items-center gap-1.5">
        <Button size="sm" variant="secondary" onClick={() => void reload()}>
          重新加载
        </Button>
        <Button size="sm" variant="primary" onClick={() => void keepMine()}>
          保留我的版本（另存为 .bak）
        </Button>
      </span>
    </div>
  );
}
