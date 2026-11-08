import { GitFork, ChevronDown, ChevronRight, ArrowUpRight } from "lucide-react";
import type { ContinuationTarget, SessionSummary } from "@/lib/types";

export function ConversationOrigin({ session, catalog, deleted, expanded, onToggle, onOpen }: {
  session: SessionSummary; catalog: SessionSummary[]; deleted: string[]; expanded: boolean;
  onToggle: () => void; onOpen?: (id: string, target?: ContinuationTarget) => void;
}) {
  const origin = session.continuation;
  if (!origin && session.sessionKind !== "fork") return null;
  const parentId = origin?.parentSessionId ?? session.parentSessionId;
  if (!parentId) return null;
  const parent = catalog.find(row => row.sessionId === parentId);
  const deletedParent = deleted.includes(parentId);
  const missing = session.parentMissing || deletedParent;
  const title = parent?.title || origin?.parentTitle || "原对话";
  return <div className="conversation-origin">
    <div className="conversation-origin__source"><GitFork size={16} aria-hidden="true" /><span>接续自</span>
      {onOpen && !missing ? <button type="button" className="conversation-origin__link" onClick={() => onOpen(parentId, origin ? {promptIndex:origin.promptIndex, promptId:origin.promptId ?? undefined} : undefined)}
        title={`返回“${title}”${origin ? `，定位到第 ${origin.inheritedTurnCount} 轮的接续位置` : ""}`}>{title}</button> : <span>{title}</span>}
      {origin && <span className="conversation-origin__meta">· 接续至第 {origin.inheritedTurnCount} 轮</span>}
      {missing && <span className="conversation-origin__meta">· {deletedParent ? "原对话已删除" : "原对话不可用"}</span>}
      {parent?.archived && <span className="conversation-origin__meta">· 已归档</span>}
    </div>
    {origin && <button type="button" className="conversation-origin__history" aria-expanded={expanded} aria-controls="conversation-inherited-history" onClick={onToggle}>
      {expanded ? <ChevronDown size={14} /> : <ChevronRight size={14} />}已继承 {origin.inheritedTurnCount} 轮上下文 · {expanded ? "收起" : "展开"}
    </button>}
    {origin && <p className="conversation-origin__workspace">聊天记录独立保存；文件保持当前状态，与原对话共用工作目录。</p>}
  </div>;
}

export function ContinuationLinks({ sessions, onOpen }: { sessions:SessionSummary[]; onOpen?:(id:string) => void }) {
  if (!sessions.length || !onOpen) return null;
  return <details className="conversation-continuations">
    <summary><GitFork size={14} aria-hidden="true" />从这里接续的对话 · {sessions.length}</summary>
    <div>{sessions.map(session => <button type="button" key={session.sessionId} onClick={() => onOpen(session.sessionId)}>
      <span>{session.title}</span><ArrowUpRight size={14} aria-hidden="true" />
    </button>)}</div>
  </details>;
}
