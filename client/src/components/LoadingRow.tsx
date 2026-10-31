import { useEffect, useRef, useState } from "react";
import { formatProcessDuration } from "@/lib/execution-process";
import { pickLoadingTip } from "@/lib/loading-tips";

const REQUEST_DURATION_MS = 1_500;
const BACKGROUND_HINT_DELAY_MS = 8_000;
const TIP_DELAY_MS = 3_000;
const TIP_INTERVAL_MS = 12_000;

/**
 * Keep factual request status primary; secondary copy accompanies longer waits.
 * The caller unmounts this row as soon as output or execution begins.
 */
export function LoadingRow({ startedAt }: { startedAt?: number }) {
  const origin = useRef(Date.now());
  const [now, setNow] = useState(() => Date.now());
  const [tip, setTip] = useState<string | null>(null);

  useEffect(() => {
    let nextTipAt = Date.now() + TIP_DELAY_MS;
    let previousTip: string | undefined;
    const update = () => {
      if (document.visibilityState === "hidden") return;
      const timestamp = Date.now();
      setNow(timestamp);
      if (timestamp >= nextTipAt) {
        previousTip = pickLoadingTip(Math.random, previousTip);
        setTip(previousTip);
        // Resuming a background page changes at most one tip, without catch-up.
        nextTipAt = timestamp + TIP_INTERVAL_MS;
      }
    };
    const timer = window.setInterval(update, 500);
    document.addEventListener("visibilitychange", update);
    return () => {
      window.clearInterval(timer);
      document.removeEventListener("visibilitychange", update);
    };
  }, []);

  const elapsed = Math.max(0, now - (startedAt ?? origin.current));
  const phase = elapsed < REQUEST_DURATION_MS
    ? "正在请求模型"
    : "正在等待模型响应";

  return (
    <div className="msg__loading">
      <div className="msg__loading-status">
        <span className="msg__loading-main" role="status" aria-live="polite">
          {phase}
        </span>
        {elapsed >= BACKGROUND_HINT_DELAY_MS && (
          <span className="msg__loading-context">
            <span aria-hidden="true">·</span>
            <span>{formatProcessDuration(elapsed)}</span>
            <span className="msg__loading-background-hint">可切换会话，任务会继续运行</span>
          </span>
        )}
      </div>
      {elapsed >= 30_000
        ? <p className="msg__loading-tip" aria-live="off">尚未收到模型输出，可继续等待，或停止本次执行。</p>
        : tip && <p className="msg__loading-tip" aria-live="off">{tip}</p>}
    </div>
  );
}
