import { Check, Search } from "lucide-react";
import { useMemo, useState } from "react";

import { Dialog } from "@/components/common/Dialog";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { cn } from "@/lib/cn";

interface ModelPickerDialogProps {
  open: boolean;
  onClose: () => void;
  /** 接口返回的全部模型 id */
  models: string[];
  /** 已经在这个提供商里的模型（默认勾选且不可取消） */
  already: string[];
  /** 弹窗标题右侧的来源说明，例如 `api.openai.com/v1/models` */
  source?: string;
  onConfirm: (selected: string[]) => void;
}

/**
 * 模型选择弹窗：带搜索框 + 多选。
 * 已存在的模型显示为「已在目录」，避免重复添加。
 */
export function ModelPickerDialog({
  open,
  onClose,
  models,
  already,
  source,
  onConfirm,
}: ModelPickerDialogProps) {
  const [keyword, setKeyword] = useState("");
  const [picked, setPicked] = useState<string[]>([]);

  const filtered = useMemo(() => {
    const kw = keyword.trim().toLowerCase();
    if (!kw) return models;
    return models.filter((m) => m.toLowerCase().includes(kw));
  }, [keyword, models]);

  const close = () => {
    setKeyword("");
    setPicked([]);
    onClose();
  };

  const toggle = (model: string) => {
    if (already.includes(model)) return;
    setPicked((prev) => (prev.includes(model) ? prev.filter((m) => m !== model) : [...prev, model]));
  };

  return (
    <Dialog
      open={open}
      onClose={close}
      title="选择要添加的模型"
      description={source ? `来源：${source}（共 ${models.length} 个）` : `共 ${models.length} 个模型`}
      width="md"
      footer={
        <>
          <span className="mr-auto text-[11px] text-ink-400">已选 {picked.length} 个</span>
          <Button variant="ghost" onClick={close}>
            取消
          </Button>
          <Button
            disabled={picked.length === 0}
            onClick={() => {
              onConfirm(picked);
              close();
            }}
          >
            确认添加
          </Button>
        </>
      }
    >
      <div className="space-y-2">
        <div className="relative">
          <Search className="pointer-events-none absolute left-2.5 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-ink-400" />
          <Input
            autoFocus
            value={keyword}
            onChange={(e) => setKeyword(e.target.value)}
            placeholder="搜索模型 id，例如 gpt-4o / claude / llama"
            className="h-8 pl-8 text-xs"
          />
        </div>

        <div className="max-h-64 overflow-y-auto rounded-md border border-ink-200">
          {filtered.length === 0 && (
            <div className="px-3 py-6 text-center text-xs text-ink-400">没有匹配的模型</div>
          )}
          {filtered.map((model) => {
            const exists = already.includes(model);
            const checked = picked.includes(model);
            return (
              <button
                key={model}
                type="button"
                onClick={() => toggle(model)}
                className={cn(
                  "flex w-full items-center gap-2 border-b border-ink-100 px-2.5 py-1.5 text-left text-xs last:border-b-0",
                  exists ? "cursor-default bg-ink-50 text-ink-400" : "hover:bg-accent-soft",
                  checked && "bg-accent-soft text-ink-900",
                )}
              >
                <span
                  className={cn(
                    "flex h-3.5 w-3.5 shrink-0 items-center justify-center rounded border",
                    checked || exists ? "border-accent bg-accent text-white" : "border-ink-300",
                  )}
                >
                  {(checked || exists) && <Check className="h-2.5 w-2.5" />}
                </span>
                <span className="min-w-0 flex-1 break-all font-mono">{model}</span>
                {exists && <span className="shrink-0 text-[10px]">已在目录</span>}
              </button>
            );
          })}
        </div>
      </div>
    </Dialog>
  );
}
