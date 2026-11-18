import type { ReactNode } from "react";

import { Button } from "@/components/common/Button";
import { Dialog } from "@/components/common/Dialog";

interface ConfirmDialogProps {
  open: boolean;
  title: string;
  message: ReactNode;
  confirmText?: string;
  danger?: boolean;
  onConfirm: () => void | Promise<void>;
  onClose: () => void;
}

/** 二次确认对话框（删除等不可逆操作的最后一关）。 */
export function ConfirmDialog({
  open,
  title,
  message,
  confirmText = "确定",
  danger = false,
  onConfirm,
  onClose,
}: ConfirmDialogProps) {
  return (
    <Dialog
      open={open}
      onClose={onClose}
      title={title}
      footer={
        <>
          <Button size="sm" variant="ghost" onClick={onClose}>
            取消
          </Button>
          <Button
            size="sm"
            variant={danger ? "danger" : "primary"}
            onClick={() => {
              void Promise.resolve(onConfirm()).then(() => onClose());
            }}
          >
            {confirmText}
          </Button>
        </>
      }
    >
      <div className="text-sm leading-relaxed text-ink-700">{message}</div>
    </Dialog>
  );
}
