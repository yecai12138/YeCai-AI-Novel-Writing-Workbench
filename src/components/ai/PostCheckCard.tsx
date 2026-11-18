import { AlertCircle, CheckCircle2, RefreshCw, Sparkles, X } from "lucide-react";

import { Button } from "@/components/ui/button";
import type { PostCheck } from "@/types";

interface PostCheckCardProps {
  post: PostCheck;
  busy?: boolean;
  onDeslop: () => void;
  onRetry: () => void;
  onDismiss: () => void;
}

/** 问题 code → 展示文案。 */
const CODE_LABEL: Record<string, string> = {
  wordcount: "字数不足",
  ai_pattern: "AI 味",
  missing: "没找到正文",
  structure: "正文文件不唯一",
  encoding: "文件编码坏了",
  script: "体检脚本报错",
  continuity: "事件没接上",
};

/**
 * 后置体检卡片：正文写完自动跑 `gate::postcheck_prose`，结果以 `ai://postcheck` 推来。
 * 通过时只是一条绿色确认，不打断阅读；不通过时给「去 AI 味 / 重新体检」两条修复路径。
 */
export function PostCheckCard({ post, busy, onDeslop, onRetry, onDismiss }: PostCheckCardProps) {
  if (post.skipped) {
    return (
      <div className="mx-3 mb-2 rounded-md border border-ink-200 bg-ink-50 px-2.5 py-1.5 text-[11px] leading-4 text-ink-500">
        后置体检跳过：{post.reason}
      </div>
    );
  }

  if (post.ok) {
    return (
      <div className="mx-3 mb-2 flex items-start gap-1.5 rounded-md border border-emerald-200 bg-emerald-50 px-2.5 py-1.5 text-[11px] leading-4 text-emerald-800">
        <CheckCircle2 className="mt-0.5 h-3.5 w-3.5 shrink-0" />
        <span className="flex-1">{post.reason}</span>
        <button
          type="button"
          onClick={onDismiss}
          title="收起"
          className="shrink-0 text-emerald-600 hover:text-emerald-900"
        >
          <X className="h-3 w-3" />
        </button>
      </div>
    );
  }

  const aiPattern = post.issues.some((i) => i.code === "ai_pattern");
  const wordcount = post.issues.some((i) => i.code === "wordcount");
  const continuity = post.issues.filter((i) => i.code === "continuity");

  return (
    <div
      role="alert"
      className="mx-3 mb-2 shrink-0 rounded-md border border-amber-300 bg-amber-50 px-2.5 py-2 text-[11px] leading-4 text-amber-900"
    >
      <div className="flex items-start gap-1.5">
        <AlertCircle className="mt-0.5 h-3.5 w-3.5 shrink-0" />
        <div className="min-w-0 flex-1">
          <div className="font-medium">体检未通过</div>
          <div className="mt-0.5 break-words">{post.reason}</div>
        </div>
        <button
          type="button"
          onClick={onDismiss}
          title="收起"
          className="shrink-0 text-amber-600 hover:text-amber-900"
        >
          <X className="h-3 w-3" />
        </button>
      </div>

      {continuity.length > 0 && (
        <p className="mt-1 text-amber-800">
          事件对不上 {continuity.length} 处：细纲承诺消耗的未结项没在正文里落地，或正文里冒出了卡上没有的编号。
          这不是文字问题，是事件没闭合——要么补写，要么改事件卡。
        </p>
      )}

      {post.issues.length > 1 && (
        <ul className="mt-1 space-y-0.5 pl-5">
          {post.issues.slice(1).map((issue) => (
            <li
              key={`${issue.code}-${issue.event ?? ""}-${issue.text}`}
              className="list-disc"
            >
              {CODE_LABEL[issue.code] ?? issue.code}
              {issue.event ? `（事件 ${issue.event}）` : ""}：{issue.text}
            </li>
          ))}
        </ul>
      )}

      <div className="mt-1.5 flex flex-wrap items-center gap-1.5">
        {aiPattern && (
          <Button size="sm" variant="secondary" className="h-6 px-2 text-[11px]" disabled={busy} onClick={onDeslop}>
            <Sparkles className="mr-1 h-3 w-3" /> 去 AI 味
          </Button>
        )}
        {wordcount && (
          <span className="text-amber-800">建议继续扩写，或把阈值调低（Rust 侧 PROSE_MIN_CHARS）。</span>
        )}
        <Button size="sm" variant="ghost" className="h-6 px-2 text-[11px]" disabled={busy} onClick={onRetry}>
          <RefreshCw className="mr-1 h-3 w-3" /> 重新体检
        </Button>
        {post.chars != null && (
          <span className="ml-auto shrink-0 text-amber-700">本次 {post.chars} 字</span>
        )}
      </div>

      {post.stdout && (
        <details className="mt-1.5">
          <summary className="cursor-pointer text-amber-700 hover:text-amber-900">
            体检输出（storyctl / 本地统计）
          </summary>
          <pre className="mt-1 max-h-28 overflow-auto whitespace-pre-wrap break-words rounded border border-amber-200 bg-white p-1.5 font-mono text-[10px] leading-4 text-ink-700">
{post.stdout}
          </pre>
        </details>
      )}
    </div>
  );
}
