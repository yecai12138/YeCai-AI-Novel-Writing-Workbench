import { useCallback, useEffect, useState } from "react";

import { Button } from "@/components/common/Button";
import { Dialog } from "@/components/common/Dialog";
import { api } from "@/lib/ipc";
import { useProjectStore } from "@/store/projectStore";
import { formatError, type StyleStats } from "@/types";

interface StyleStatsDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
}

/**
 * 全书文风体检。
 *
 * 这个面板**只显示计数**，不给评分、不给「好/坏」结论：同一句式一章出现三五次时
 * 单章读起来都正常，只有把全书次数摊开才看得见。判断留给作者与模型。
 * 数据源是 Rust 侧 stylestat（只读正文目录，不写任何文件）。
 */
export function StyleStatsDialog({ open, onOpenChange }: StyleStatsDialogProps) {
  const workspaceDir = useProjectStore((s) => s.workspaceDir);
  const bookRoot = useProjectStore((s) => s.bookRoot);
  const bookName = useProjectStore((s) => s.bookName);

  const [stats, setStats] = useState<StyleStats | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [checked, setChecked] = useState(false);

  const load = useCallback(async () => {
    if (!workspaceDir || !bookRoot) {
      setError("还没有打开项目");
      return;
    }
    setLoading(true);
    setError(null);
    try {
      const data = await api.styleStats(workspaceDir, bookRoot);
      setStats(data);
      setChecked(true);
    } catch (e) {
      setError(formatError(e));
    } finally {
      setLoading(false);
    }
  }, [workspaceDir, bookRoot]);

  useEffect(() => {
    if (open) {
      setStats(null);
      setChecked(false);
      void load();
    }
  }, [open, load]);

  const patterns = stats
    ? [...stats.patterns].sort((a, b) => b.total - a.total)
    : [];
  const maxTotal = patterns.reduce((m, p) => Math.max(m, p.total), 0);

  return (
    <Dialog
      open={open}
      onClose={() => onOpenChange(false)}
      title="文风体检"
      description="只出计数，不做评分：同一句式一章出现三五次时单章读起来都正常，把全书次数摊开才看得见。"
      width="lg"
      footer={
        <>
          <Button variant="secondary" onClick={() => void load()} disabled={loading}>
            {loading ? "统计中…" : "重新统计"}
          </Button>
          <Button variant="primary" onClick={() => onOpenChange(false)}>
            关闭
          </Button>
        </>
      }
    >
      {error && (
        <p className="rounded-md border border-rose-200 bg-rose-50 px-3 py-2 text-sm text-rose-700">
          {error}
        </p>
      )}

      {!error && loading && (
        <p className="text-sm text-ink-500">正在统计全书正文…（只读，不改文件）</p>
      )}

      {!error && !loading && checked && !stats && (
        <div className="space-y-2 text-sm text-ink-600">
          <p className="font-medium text-ink-800">样本不足</p>
          <p>
            当前正文少于 5 章，频率还没有意义，因此不出统计。写完 5 章后
            这里会自动出数；写作时也不会把这份数字塞进提示词。
          </p>
        </div>
      )}

      {!error && !loading && stats && (
        <div className="space-y-5 text-sm">
          <div className="grid grid-cols-3 gap-3">
            <Stat label="已统计章数" value={String(stats.chapters)} />
            <Stat label="全书字数" value={stats.totalChars.toLocaleString()} />
            <Stat label="章均字数" value={stats.avgChars.toLocaleString()} />
          </div>

          <section className="space-y-2">
            <h3 className="text-xs font-semibold text-ink-700">句式 tic（全书次数 / 每章）</h3>
            <ul className="space-y-1.5">
              {patterns.map((p) => (
                <li key={p.name} className="space-y-1">
                  <div className="flex items-baseline justify-between gap-3">
                    <span className="text-ink-800">{p.name}</span>
                    <span className="shrink-0 tabular-nums text-ink-500">
                      {p.total} 次 · {p.perChapter.toFixed(2)}/章
                    </span>
                  </div>
                  <div className="h-1.5 overflow-hidden rounded-full bg-ink-100">
                    <div
                      className="h-full rounded-full bg-accent/70"
                      style={{ width: maxTotal > 0 ? p.total / maxTotal * 100 + "%" : "0%" }}
                    />
                  </div>
                </li>
              ))}
            </ul>
          </section>

          {stats.topPhrases.length > 0 && (
            <section className="space-y-2">
              <h3 className="text-xs font-semibold text-ink-700">最近 20 章高频短语</h3>
              <div className="flex flex-wrap gap-1.5">
                {stats.topPhrases.map((p) => (
                  <span
                    key={p.text}
                    className="rounded-full border border-ink-200 bg-ink-50 px-2 py-0.5 text-[12px] text-ink-700"
                    title={"全书出现 " + p.count + " 次"}
                  >
                    {p.text} <span className="text-ink-400">{p.count}</span>
                  </span>
                ))}
              </div>
            </section>
          )}

          <section className="space-y-2">
            <h3 className="text-xs font-semibold text-ink-700">跨章逐字重复句</h3>
            {stats.repeatedSentences.length === 0 ? (
              <p className="text-ink-500">没有跨两章以上完全重复的长句。</p>
            ) : (
              <ul className="space-y-1.5">
                {stats.repeatedSentences.map((s) => (
                  <li key={s.text} className="rounded-md border border-ink-100 bg-ink-50/60 px-2.5 py-1.5">
                    <p className="text-ink-800">{s.text}</p>
                    <p className="mt-0.5 text-[11px] text-ink-500">
                      出现 {s.count} 次，横跨 {s.chapters} 章
                    </p>
                  </li>
                ))}
              </ul>
            )}
          </section>

          <section className="grid grid-cols-2 gap-3">
            <Stat
              label="章末短结尾占比"
              value={Math.round(stats.ending.shortRatio * 100) + "%"}
              hint={"章末行中位数 " + stats.ending.medianChars + " 字"}
            />
            <Stat
              label="章标题前缀"
              value={
                stats.titles.withoutPrefix === 0
                  ? "统一带「第N章」"
                  : stats.titles.withPrefix === 0
                    ? "统一不带前缀"
                    : "混用：" + stats.titles.withPrefix + " / " + stats.titles.withoutPrefix
              }
              hint="混用会把机制痕迹留在产物里"
            />
          </section>

          <p className="border-t border-ink-100 pt-3 text-[11px] leading-relaxed text-ink-400">
            口径说明：句式 tic 用正则在全文里数，是近似值；高频短语取最近 20 章的 3 字窗口，
            已滤掉角色名与虚词组合。这些数字只在同一本书内纵向比较有意义
            {bookName ? "（当前书：" + bookName + "）" : ""}。
          </p>
        </div>
      )}
    </Dialog>
  );
}

function Stat({ label, value, hint }: { label: string; value: string; hint?: string }) {
  return (
    <div className="rounded-lg border border-ink-100 bg-white px-3 py-2">
      <p className="text-[11px] text-ink-500">{label}</p>
      <p className="mt-0.5 text-base font-semibold tabular-nums text-ink-900">{value}</p>
      {hint && <p className="mt-0.5 text-[11px] text-ink-400">{hint}</p>}
    </div>
  );
}
