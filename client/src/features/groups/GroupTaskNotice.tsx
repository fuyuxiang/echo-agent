import { activeRequest, requestLabels } from "./group-presentation";
import { autoBlockedGroupWorks, failedGroupWorks, terminalWork, type Group, type GroupRequest } from "./group-client";

export function GroupTaskNotice({ group, request, busy, onControl, onDetails }: {
  group: Group; request: GroupRequest; busy: boolean;
  onControl: (action: "retry" | "summarize", workId: string) => void;
  onDetails: () => void;
}) {
  const scoped = { ...group, latestRequestId: request.id };
  const works = group.works.filter(work => work.rootId === request.id);
  const failures = failedGroupWorks(scoped);
  const blocked = autoBlockedGroupWorks(scoped);
  const uncertain = works.find(work => work.status === "uncertain");
  const stopped = [...works].reverse().find(work => work.status === "cancelled");
  const work = uncertain ?? failures[0] ?? blocked[0] ?? stopped;
  if (!work && !["missing_result", "partial"].includes(request.state)) return null;
  // A task has one recovery surface. Historical failures with continuations
  // remain inspectable in Work, without becoming repeated chat alerts.
  if (activeRequest(request) && !blocked.length) return null;
  const resultOnly = work?.summary || work?.resultRepair || request.state === "missing_result";
  const title = uncertain ? "本次任务的执行情况需要检查"
    : blocked.length ? `${group.members.find(member => member.id === work?.memberId)?.name ?? "成员"} 有操作被自动判断拦截`
    : failures.length ? `${group.members.find(member => member.id === work?.memberId)?.name ?? "成员"} ${resultOnly ? "整理结果失败" : "执行失败"}`
    : request.state === "stopped" ? "本次任务已停止" : requestLabels[request.state];
  const detail = uncertain ? "执行曾被中断，请先核对已有文件与记录，再继续未完成部分。"
    : blocked.length ? "拦截已经结束，没有待批准请求。已有成果保留，可查看记录并按当前群权限检查后继续。"
    : work?.error || (resultOnly ? "执行记录已保留，可以整理已有结果，无需重新执行操作。" : "已有回复和文件保留，尚未完成的工作不会自动重做。");
  const action = resultOnly && !uncertain ? "summarize" : work ? "retry" : "summarize";
  const disabled = busy || group.archived || !uncertain && (group.paused || works.some(item => !terminalWork(item.status)));
  return <section className="group-result-notice" role={failures.length ? "alert" : "status"}>
    <strong>{title}</strong><p>{detail}</p>
    <div className="group-result-notice__actions">
      <button type="button" className="btn btn--primary" disabled={disabled} onClick={() => onControl(action, work?.id ?? request.workId)}>{action === "summarize" ? "整理已有结果" : "检查后继续"}</button>
      <button type="button" className="groups-text-button" onClick={onDetails}>查看工作详情</button>
    </div>
  </section>;
}
