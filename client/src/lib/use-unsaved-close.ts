import { useRef } from "react";
import { useAppDialog } from "@/components/AppDialog";

/** One dismissal path for Escape, scrim, close and cancel in draft editors. */
export function useUnsavedClose({
  dirty,
  busy = false,
  onClose,
}: {
  dirty: boolean;
  busy?: boolean;
  onClose: () => void;
}) {
  const { requestConfirmation, dialog } = useAppDialog();
  const current = useRef({ dirty, busy, onClose });
  current.current = { dirty, busy, onClose };

  const requestClose = () => {
    if (current.current.busy) return;
    if (!current.current.dirty) {
      current.current.onClose();
      return;
    }
    requestConfirmation({
      title: "舍弃未保存的修改？",
      description: "当前输入尚未保存。你可以继续编辑，或舍弃修改后关闭。",
      cancelLabel: "继续编辑",
      confirmLabel: "舍弃修改",
      danger: true,
      action: () => {
        if (!current.current.busy) current.current.onClose();
      },
    });
  };

  return { requestClose, closeDialog: dialog };
}
