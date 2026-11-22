import { retrySessionInteractions, useSessionInteractionRecovery } from "@/lib/session-interactions";

export function SessionInteractionNotice({ sessionId }: { sessionId: string }) {
  const phase = useSessionInteractionRecovery(sessionId);
  if (!phase) return null;

  return <div className="yield-banner" role={phase === "failed" ? "alert" : "status"}>
    <span>{phase === "checking" ? "正在检查待处理事项…" : "暂时无法获取待处理事项"}</span>
    {phase === "failed" && <button type="button" className="btn btn--ghost" onClick={() => retrySessionInteractions(sessionId)}>
      重试检查
    </button>}
  </div>;
}
