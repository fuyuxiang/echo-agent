/**
 * 消息反馈完整评分弹窗 —— 对齐 EchoAgent `echo-chat-ui/message-feedback`
 * 的 rating bar + 反馈弹窗(thumbs 选向 → 1–5 星评分 + 文字备注)。
 *
 * 由 MessageItem 的 👍/👎 触发:点击后弹出此弹窗,用户可选 1–5 星 + 写备注,
 * 提交写入 feedback-store;关闭则保留已选方向(rating 仍记录)。
 */
import { useEffect, useState } from "react";
import { useFeedbackStore, type FeedbackRating } from "@/stores/feedback-store";
import { XCloseIcon } from "@/foundation/components/Icon/icons";
import { useModalFocus } from "@/lib/use-modal-focus";

interface FeedbackDialogProps {
  open: boolean;
  sessionId: string;
  messageId: string;
  /** 初始方向(由触发的 👍/👎 决定)。 */
  rating: FeedbackRating;
  onClose: () => void;
  onToast?: (msg: string) => void;
}

const STAR_LABELS: Record<number, string> = {
  1: "很差",
  2: "较差",
  3: "一般",
  4: "较好",
  5: "很好",
};

export function FeedbackDialog({
  open,
  sessionId,
  messageId,
  rating,
  onClose,
  onToast,
}: FeedbackDialogProps) {
  const [stars, setStars] = useState(0);
  const [hover, setHover] = useState(0);
  const [note, setNote] = useState("");

  // Keep the local draft across dismiss/reopen; it belongs to this message.
  useEffect(() => {
    setStars(0);
    setHover(0);
    setNote("");
  }, [sessionId, messageId]);
  useEffect(() => {
    if (!open) setHover(0);
  }, [open]);
  const dialogRef = useModalFocus<HTMLDivElement>(open, onClose);

  const submit = () => {
    useFeedbackStore
      .getState()
      .setDetailed(sessionId, messageId, {
        rating,
        stars: stars > 0 ? stars : undefined,
        note: note.trim() || undefined,
      });
    onToast?.("已提交反馈");
    onClose();
  };

  if (!open) return null;

  const shown = hover || stars;

  return (
    <div
      ref={dialogRef}
      className="feedback-dialog__overlay"
      role="dialog"
      aria-modal="true"
      aria-label="消息反馈"
      tabIndex={-1}
      onClick={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div className="feedback-dialog">
        <div className="feedback-dialog__head">
          <span>{rating === "up" ? "👍 你对这条回复满意" : "👎 这条回复有待改进"}</span>
          <button
            type="button"
            className="feedback-dialog__close"
            onClick={onClose}
            aria-label="关闭"
            data-modal-initial-focus
          >
            <XCloseIcon size="sm" />
          </button>
        </div>
        <div className="feedback-dialog__body">
          <div className="feedback-dialog__stars">
            {[1, 2, 3, 4, 5].map((n) => (
              <button
                key={n}
                type="button"
                className={
                  "feedback-dialog__star" +
                  (shown >= n ? " feedback-dialog__star--active" : "")
                }
                onClick={() => setStars(n)}
                onMouseEnter={() => setHover(n)}
                onMouseLeave={() => setHover(0)}
                aria-label={`${n} 星`}
              >
                ★
              </button>
            ))}
            <span className="feedback-dialog__star-label">
              {shown > 0 ? STAR_LABELS[shown] : "点击评分(可选)"}
            </span>
          </div>
          <textarea
            className="feedback-dialog__note"
            placeholder="补充说明(可选)"
            value={note}
            onChange={(e) => setNote(e.target.value)}
            rows={3}
          />
        </div>
        <div className="feedback-dialog__foot">
          <button
            type="button"
            className="feedback-dialog__btn"
            onClick={onClose}
          >
            取消
          </button>
          <button
            type="button"
            className="feedback-dialog__btn feedback-dialog__btn--primary"
            onClick={submit}
          >
            提交
          </button>
        </div>
      </div>
    </div>
  );
}
