/**
 * 设置 → 运维：日志 / 会话库 / repos 三张卡片。
 *
 * 每张卡片都是「先看数、再动手」：左边报当前占用，右边按钮点了先弹二次确认、
 * 确认框里写清「预计释放多少」，确认后才调后端。所有动作后端都会写一行 engine.log。
 */

import { useCallback, useEffect, useState } from "react";

import { ConfirmDialog } from "@/components/common/ConfirmDialog";
import { Badge } from "@/components/common/Field";
import { Button } from "@/components/ui/button";
import { api } from "@/lib/ipc";
import type { OpsAction, OpsOverview } from "@/types";
import { formatError } from "@/types";

function human(bytes: number): string {
  const MB = 1024 * 1024;
  const GB = 1024 * MB;
  if (bytes >= GB) return `${(bytes / GB).toFixed(2)} GB`;
  if (bytes >= MB) return `${(bytes / MB).toFixed(1)} MB`;
  if (bytes >= 1024) return `${Math.round(bytes / 1024)} KB`;
  return `${bytes} B`;
}

type Pending = null | "logs" | "database" | "repos";

export function OpsSection() {
  const [overview, setOverview] = useState<OpsOverview | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [pending, setPending] = useState<Pending>(null);
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<string | null>(null);
  const [keepDays, setKeepDays] = useState(30);

  const refresh = useCallback(() => {
    void api
      .opsOverview()
      .then((view) => {
        setOverview(view);
        setError(null);
      })
      .catch((err) => setError(formatError(err)));
  }, []);

  useEffect(() => {
    refresh();
  }, [refresh]);

  const run = (fn: () => Promise<OpsAction>) => {
    setBusy(true);
    setResult(null);
    void fn()
      .then((out) => {
        setResult(`${out.note}（释放 ${out.freedHuman}）`);
        refresh();
      })
      .catch((err) => setError(formatError(err)))
      .finally(() => {
        setBusy(false);
        setPending(null);
      });
  };

  if (error && !overview) {
    return <div className="text-[11px] text-rose-600">运维信息读取失败：{error}</div>;
  }
  if (!overview) {
    return <div className="text-[11px] text-ink-400">正在读取日志与库的占用…</div>;
  }

  const dbOver =
    overview.dbWarnBytes > 0 && overview.database.bytes > overview.dbWarnBytes;

  return (
    <div className="space-y-2">
      <div className="flex items-center gap-2">
        <span className="text-[11px] font-medium text-ink-700">运维</span>
        <span className="text-[11px] text-ink-400">
          清理动作都会记一行到 engine.log，事后可追溯
        </span>
      </div>

      {/* 日志 */}
      <div className="rounded border border-ink-200 bg-white px-2.5 py-2">
        <div className="flex items-center justify-between gap-2">
          <div className="min-w-0">
            <div className="text-[11px] font-medium text-ink-700">
              日志 <Badge tone="neutral">{overview.logs.files} 个文件</Badge>{" "}
              <span className="text-ink-500">{overview.logs.human}</span>
            </div>
            <div className="mt-0.5 break-all text-[11px] text-ink-400">
              {overview.logs.path}
            </div>
            <div className="mt-0.5 text-[11px] text-ink-400">
              engine.log 超过 {human(overview.logRotateAt)} 自动轮转、保留 3 份；
              opencode 自己的日志 {overview.engineLog.human}（由引擎侧轮转）
            </div>
          </div>
          <Button
            size="sm"
            variant="outline"
            disabled={busy || overview.logs.files === 0}
            onClick={() => setPending("logs")}
          >
            清理日志
          </Button>
        </div>
      </div>

      {/* 会话库 */}
      <div className="rounded border border-ink-200 bg-white px-2.5 py-2">
        <div className="flex items-center justify-between gap-2">
          <div className="min-w-0">
            <div className="text-[11px] font-medium text-ink-700">
              会话库{" "}
              {dbOver ? (
                <Badge tone="warn">{overview.database.human}</Badge>
              ) : (
                <Badge tone="neutral">{overview.database.human}</Badge>
              )}
              {overview.database.walBytes > 0 && (
                <span className="ml-1 text-ink-400">
                  WAL {human(overview.database.walBytes)}
                </span>
              )}
            </div>
            <div className="mt-0.5 break-all text-[11px] text-ink-400">
              {overview.database.path}
            </div>
            <div className="mt-0.5 text-[11px] text-ink-400">
              整理走 checkpoint → VACUUM → integrity_check；不自动备份，需要备份请先手动复制
            </div>
          </div>
          <Button
            size="sm"
            variant="outline"
            disabled={busy || !overview.database.exists}
            onClick={() => setPending("database")}
          >
            整理
          </Button>
        </div>
      </div>

      {/* repos */}
      <div className="rounded border border-ink-200 bg-white px-2.5 py-2">
        <div className="flex items-center justify-between gap-2">
          <div className="min-w-0">
            <div className="text-[11px] font-medium text-ink-700">
              repos{" "}
              <Badge tone="neutral">
                {overview.repos.dirs} 个仓库 · {overview.repos.human}
              </Badge>
            </div>
            <div className="mt-0.5 break-all text-[11px] text-ink-400">
              {overview.repos.path}
            </div>
            <div className="mt-0.5 flex items-center gap-1.5 text-[11px] text-ink-400">
              <span>只删超过</span>
              <select
                className="rounded border border-ink-200 bg-white px-1 py-0.5 text-[11px]"
                value={keepDays}
                onChange={(e) => setKeepDays(Number(e.target.value))}
              >
                {[7, 30, 90].map((d) => (
                  <option key={d} value={d}>
                    {d} 天
                  </option>
                ))}
              </select>
              <span>没动过的仓库（按目录内最新文件时间算）</span>
            </div>
          </div>
          <Button
            size="sm"
            variant="outline"
            disabled={busy || overview.repos.dirs === 0}
            onClick={() => setPending("repos")}
          >
            清理过期仓库
          </Button>
        </div>
      </div>

      {result && <div className="text-[11px] text-emerald-600">{result}</div>}
      {error && <div className="text-[11px] text-rose-600">{error}</div>}

      <ConfirmDialog
        open={pending === "logs"}
        title="清理日志"
        message={
          <>
            将删除 <b>{overview.logs.files}</b> 个日志文件，预计释放{" "}
            <b>{overview.logs.human}</b>。
            <br />
            目录会保留，下一轮日志照旧写回同一个位置。清理后如果需要排查旧问题，日志就没了。
          </>
        }
        confirmText="清理"
        danger
        onConfirm={() => run(() => api.opsCleanLogs())}
        onClose={() => setPending(null)}
      />

      <ConfirmDialog
        open={pending === "database"}
        title="整理会话库"
        message={
          <>
            将对 <b>{overview.database.human}</b> 的会话库执行 VACUUM（WAL 会先收回去），
            完成后跑一次 integrity_check 并把结果回报。
            <br />
            不自动备份、不删数据；但如果此刻有引擎正在跑，可能因为库被占用而失败。
          </>
        }
        confirmText="开始整理"
        onConfirm={() => run(() => api.opsVacuumDatabase())}
        onClose={() => setPending(null)}
      />

      <ConfirmDialog
        open={pending === "repos"}
        title="清理过期仓库"
        message={
          <>
            将删除 repos 下超过 <b>{keepDays}</b> 天没动过的子目录（当前共{" "}
            <b>{overview.repos.dirs}</b> 个仓库、{overview.repos.human}）。
            <br />
            这些目录只影响历史会话的代码索引，不影响正文与大纲。
          </>
        }
        confirmText="清理"
        danger
        onConfirm={() => run(() => api.opsCleanRepos(keepDays))}
        onClose={() => setPending(null)}
      />
    </div>
  );
}
