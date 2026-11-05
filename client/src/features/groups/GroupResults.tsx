import { useEffect, useMemo, useState } from "react";
import { FileText } from "lucide-react";
import { collectSessionArtifacts } from "@/lib/session-artifacts";
import { openLocalPath } from "@/lib/markdown-host";
import { groupMessageView } from "./group-message-view";
import { publicGroupMessages, requestLabels, requestTitle } from "./group-presentation";
import type { Group, GroupRequest } from "./group-client";

export function GroupResults({ group, requests, requestId, onSelect, onConclusion, onError }: {
  group: Group; requests: GroupRequest[]; requestId: string | null;
  onSelect: (id: string) => void; onConclusion: (messageId: string) => void;
  onError: (error: string) => void;
}) {
  const result = requests.find(request => request.id === requestId) ?? requests[requests.length - 1];
  const [openError, setOpenError] = useState<string | null>(null);
  useEffect(() => setOpenError(null), [result?.id]);
  const open = (path: string, directory = false) => {
    setOpenError(null);
    void openLocalPath(path, { cwd: group.cwd, revealFile: false, type: directory ? "directory" : undefined,
      onToast: error => { setOpenError(error); onError(error); } });
  };
  const message = publicGroupMessages(group).find(message => message.id === result?.finalMessageId);
  const artifacts = useMemo(() => {
    const roots = new Set<string>();
    let cursor: string | undefined | null = result?.id;
    while (cursor && !roots.has(cursor)) {
      roots.add(cursor);
      cursor = group.messages.find(message => message.id === cursor)?.sourceRequestId;
    }
    const works = new Map(group.works.map(work => [work.id, work]));
    const views = group.messages.map(message => {
      const view = groupMessageView(message, works.get(message.workId ?? ""));
      // Providers may reuse a tool id in a different member/session. Keep its
      // provenance scoped to the work, without mutating cached message views.
      return { ...view, parts: view.parts.map(part => part.kind === "tool_call"
        ? { ...part, toolCall: { ...part.toolCall, toolCallId: `${message.workId ?? message.id}:${part.toolCall.toolCallId}` } } : part) };
    });
    const tools = new Set(views.filter((_, index) => roots.has(works.get(group.messages[index].workId ?? "")?.rootId ?? ""))
      .flatMap(view => view.parts.flatMap(part => part.kind === "tool_call" ? [part.toolCall.toolCallId] : [])));
    // Collect over the full history first: later deletions and moves must also
    // invalidate an output originally produced by an earlier related task.
    return collectSessionArtifacts(views, group.cwd).filter(file => tools.has(file.toolCallId));
  }, [group.messages, group.works, group.cwd, result?.id]);
  return <div className="group-results">
    {requests.length > 1 && <label className="group-detail-selector">成果所属任务<select value={result?.id ?? ""} onChange={event => onSelect(event.target.value)}>{[...requests].reverse().map(request => <option key={request.id} value={request.id}>{requestTitle(group, request.id)}</option>)}</select></label>}
    {!result ? <p className="groups-muted">提出任务后，结论和生成的文件会归集在这里。</p> : <>
      <p className="groups-muted">{requestLabels[result.state]}</p>
      <button type="button" className="groups-text-button" onClick={() => open(group.cwd, true)}>打开工作区</button>
      {openError && <p className="groups-error" role="alert">{openError}</p>}
      {message ? <section className="group-result-summary"><strong>本次结论</strong><p>{message.text.slice(0, 360)}{message.text.length > 360 ? "…" : ""}</p><button type="button" className="groups-text-button" onClick={() => onConclusion(message.id)}>在聊天中查看完整结论</button></section> : <p className="groups-muted">暂未形成最终结论。已有文件仍可查看。</p>}
      <div className="group-artifacts" aria-label="任务交付物">{!artifacts.length && <p className="groups-muted">暂无通过执行记录确认生成的文件。对话中的引用链接仍可直接打开。</p>}{artifacts.map(file => <button type="button" key={file.id} onClick={() => open(file.path)}><span className="group-artifact-name"><FileText size={15} /><strong>{file.path.split(/[\\/]/).pop()}</strong></span><small>{file.path}</small></button>)}</div>
    </>}
  </div>;
}
