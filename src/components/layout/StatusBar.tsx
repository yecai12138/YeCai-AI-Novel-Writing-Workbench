import { countWords, formatCount } from "@/lib/words";
import { useEditorStore } from "@/store/editorStore";
import { cn } from "@/lib/cn";

const STATUS_TEXT: Record<string, string> = {
  idle: "未打开文件",
  saving: "保存中…",
  saved: "已保存",
  unsaved: "未保存的更改",
  error: "保存失败",
};

const STATUS_TONE: Record<string, string> = {
  idle: "text-ink-400",
  saving: "text-accent",
  saved: "text-emerald-600",
  unsaved: "text-amber-600",
  error: "text-rose-600",
};

/** 底部状态栏：文件路径 / 实时字数 / 保存状态 / 快照。 */
export function StatusBar() {
  const rel = useEditorStore((s) => s.rel);
  const path = useEditorStore((s) => s.path);
  const content = useEditorStore((s) => s.content);
  const status = useEditorStore((s) => s.status);
  const savedAt = useEditorStore((s) => s.savedAt);
  const snapshots = useEditorStore((s) => s.snapshots);
  const error = useEditorStore((s) => s.error);

  const words = countWords(content);

  return (
    <div className="flex h-7 shrink-0 items-center gap-3 border-t border-ink-200 bg-white px-3 text-[11px] text-ink-500">
      <span className="min-w-0 flex-1 truncate font-mono" title={path ?? ""}>
        {rel ?? "—"}
      </span>

      {error && (
        <span className="max-w-[46%] truncate text-rose-600" title={error}>
          {error}
        </span>
      )}

      <span className="shrink-0 tabular-nums" title="汉字逐字计数，西文按词计数">
        字数 <span className="font-medium text-ink-700">{formatCount(words.total)}</span>
        <span className="text-ink-300">
          {" "}
          (中文 {formatCount(words.cjk)} / 西文 {formatCount(words.words)} / 字符{" "}
          {formatCount(words.chars)})
        </span>
      </span>

      <span className="shrink-0 text-ink-300">·</span>

      <span className="shrink-0" title="每次覆盖保存前都会把旧内容存进 .storydesk/snapshots/">
        快照 {snapshots.length}
      </span>

      <span className="shrink-0 text-ink-300">·</span>

      <span className={cn("shrink-0 font-medium", STATUS_TONE[status])}>
        {STATUS_TEXT[status]}
        {status === "saved" && savedAt && (
          <span className="ml-1 font-normal text-ink-400">
            {new Date(savedAt).toLocaleTimeString("zh-CN", { hour12: false })}
          </span>
        )}
      </span>
    </div>
  );
}
