import { useEffect, useState } from "react";

import { Button } from "@/components/common/Button";
import { Dialog } from "@/components/common/Dialog";
import { Field, TextInput } from "@/components/common/Field";

interface PromptDialogProps {
  open: boolean;
  title: string;
  label: string;
  /** 打开时的默认值（重命名用） */
  defaultValue?: string;
  placeholder?: string;
  hint?: string;
  confirmText?: string;
  /** 返回错误字符串则停留在对话框里显示 */
  onSubmit: (value: string) => Promise<string | null> | string | null;
  onClose: () => void;
}

/** 单行输入对话框：新建章节标题 / 新建文件 / 重命名都复用它。 */
export function PromptDialog({
  open,
  title,
  label,
  defaultValue = "",
  placeholder,
  hint,
  confirmText = "确定",
  onSubmit,
  onClose,
}: PromptDialogProps) {
  const [value, setValue] = useState(defaultValue);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    if (open) {
      setValue(defaultValue);
      setError(null);
      setBusy(false);
    }
  }, [open, defaultValue]);

  const submit = async () => {
    const trimmed = value.trim();
    if (!trimmed) {
      setError("不能为空");
      return;
    }
    setBusy(true);
    const message = await onSubmit(trimmed);
    setBusy(false);
    if (message) {
      setError(message);
      return;
    }
    onClose();
  };

  return (
    <Dialog
      open={open}
      onClose={onClose}
      title={title}
      footer={
        <>
          <Button size="sm" variant="ghost" onClick={onClose} disabled={busy}>
            取消
          </Button>
          <Button size="sm" variant="primary" onClick={() => void submit()} disabled={busy}>
            {busy ? "处理中…" : confirmText}
          </Button>
        </>
      }
    >
      <Field label={label} hint={hint} error={error}>
        <TextInput
          autoFocus
          value={value}
          placeholder={placeholder}
          onChange={(e) => {
            setValue(e.target.value);
            setError(null);
          }}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              e.preventDefault();
              void submit();
            }
          }}
        />
      </Field>
    </Dialog>
  );
}
