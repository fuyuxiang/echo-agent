import { useMemo, useState } from "react";
import type { ToolCallView } from "@/stores/session-store";
import type { Group, GroupWork } from "./group-client";
import { terminalWork, workLabels, groupWorkAutoBlocked } from "./group-client";
import { groupMessageView } from "./group-message-view";
import { groupRequests, requestLabels, resultOnlyWorkIds } from "./group-presentation";

export function GroupWorkList({ group, requestId, onSelect, busy, onControl, onOpenTool }: {
  requestId?: string; onSelect?: (id: string) => void;
  group: Group; busy: boolean; onControl: (action: "retry" | "stop" | "summarize", workId: string) => void;
  onOpenTool: (tool: ToolCallView) => void;
}) {
  const requests = groupRequests(group);
  const resultOnlyIds = useMemo(() => resultOnlyWorkIds(group), [group.works]);
  const [visibleRequests, setVisibleRequests] = useState(24);
  const [visibleWorks, setVisibleWorks] = useState<Record<string, number>>({});
  const { byRoot, toolsByWork } = useMemo(() => {
    const byRoot = new Map<string, GroupWork[]>();
    const workById = new Map(group.works.map(work => [work.id, work]));
    const toolsByWork = new Map<string, Map<string, ToolCallView>>();
    for (const work of group.works) {
      const list = byRoot.get(work.rootId) ?? [];
      list.push(work); byRoot.set(work.rootId, list);
    }
    for (const message of group.messages) {
      if (!message.workId || !message.toolCalls.length) continue;
      const tools = toolsByWork.get(message.workId) ?? new Map<string, ToolCallView>();
      for (const part of groupMessageView(message, workById.get(message.workId)).parts) {
        if (part.kind === "tool_call") tools.set(part.toolCall.toolCallId, part.toolCall);
      }
      toolsByWork.set(message.workId, tools);
    }
    return { byRoot, toolsByWork };
  }, [group.works, group.messages]);
  return <div className="group-work-list">
    {onSelect && requests.length > 1 && <label className="group-detail-selector">查看任务<select aria-label="查看任务" value={requestId ?? requests[requests.length - 1]?.id} onChange={event => onSelect(event.target.value)}>{[...requests].reverse().map(request => <option key={request.id} value={request.id}>{group.messages.find(message => message.id === request.id)?.text.slice(0, 80) || "协作任务"}</option>)}</select></label>}
    <details className="group-work-records"><summary>用量统计</summary><p className="groups-muted">{group.works.filter(work => !requestId || work.rootId === requestId).reduce((sum, work) => sum + work.tokens, 0).toLocaleString()} Token</p></details>
    {!requests.length && <p className="groups-muted">提出目标后，这里会显示成员的工作与进度。</p>}
    {[...requests].reverse().filter(request => !requestId || request.id === requestId).filter((request,index) => index < visibleRequests || ["working","waiting","summarizing","queued","needs_check"].includes(request.state)).map(request => <div className="group-work-request" key={request.id}>
      <header><strong>{group.messages.find(message => message.id === request.id)?.text.slice(0, 100) || "协作任务"}</strong><small>{requestLabels[request.state]}</small></header>
      {(byRoot.get(request.id)?.length ?? 0) > (visibleWorks[request.id] ?? 30) && <button className="groups-text-button" onClick={() => setVisibleWorks(values => ({...values,[request.id]:(values[request.id] ?? 30)+30}))}>加载更早工作</button>}
      {(byRoot.get(request.id) ?? []).filter((work,index,works) => index >= works.length - (visibleWorks[request.id] ?? 30) || !terminalWork(work.status)).map(work => {
        const resultOnly = resultOnlyIds.has(work.id);
        const blocked = work.status === "completed" && groupWorkAutoBlocked(group, work.id);
        const tools = toolsByWork.get(work.id) ?? new Map<string, ToolCallView>();
        return <section key={work.id}><div><strong>{group.members.find(member => member.id === work.memberId)?.name ?? "成员"}</strong><span className="group-status">{blocked ? "有操作被拦截" : workLabels[work.status] ?? work.status}</span></div>
          <p>{work.summary ? "整理本次结论" : work.resultRepair ? "只读核对并补充已有结果" : work.prompt.slice(0, 180)}</p>
          {work.error && <p className="groups-error">{work.error}</p>}
          <small>{work.discussion || work.summary || work.resultRepair ? "只读处理" : "协作执行"}{work.tokens ? ` · ${work.tokens.toLocaleString()} Token` : ""}</small>
          {blocked && !resultOnly && <button type="button" className="groups-text-button" disabled={busy || group.archived || group.works.some(item => !terminalWork(item.status))} onClick={() => onControl("retry", work.id)}>检查后继续</button>}
          {tools.size > 0 && <details className="group-work-records"><summary>执行记录（{tools.size}）</summary>{[...tools.values()].map(tool => <button type="button" className="group-work-tool" key={tool.toolCallId} onClick={() => onOpenTool(tool)}><span>{tool.title}</span><small>{tool.status === "completed" ? "已完成" : tool.status === "failed" ? "失败" : "处理中"}</small></button>)}</details>}
          {["failed", "cancelled", "uncertain"].includes(work.status)
            ? <button className="groups-text-button" disabled={busy || group.paused && work.status !== "uncertain" || group.archived} onClick={() => onControl(resultOnly && work.status !== "uncertain" ? "summarize" : "retry", work.id)}>{resultOnly ? work.status === "uncertain" ? "核对并整理结果" : "重新整理结果" : "检查后重试"}</button>
            : !terminalWork(work.status) && <button className="groups-text-button" disabled={busy || work.status === "stopping"} onClick={() => onControl("stop", work.id)}>停止此项工作</button>}
        </section>;
      })}
      {["missing_result", "failed", "partial", "stopped"].includes(request.state) && <button type="button" className="groups-text-button" disabled={busy || group.paused || group.archived} onClick={() => onControl("summarize", request.workId)}>根据已有记录整理结果</button>}
    </div>)}
    {!requestId && requests.length > visibleRequests && <button className="groups-text-button" onClick={() => setVisibleRequests(count=>count+24)}>加载更早任务</button>}
  </div>;
}
