import { useMemo, useState } from "react";
import { useSessionsStore } from "@/stores/sessions-store";
import { useSessionStore } from "@/stores/session-store";
import { useSessionAutomation } from "@/lib/use-session-automation";
import { automationClearBrowserData, automationSetPrivateNetwork, type AutomationStatus } from "@/lib/automation-client";
import { friendlyError } from "@/lib/error-format";
import { useAppDialog } from "./AppDialog";
import { AutomationApprovalCard } from "./AutomationControls";

/** Browser maintenance belongs in settings, scoped to an explicitly selected
 * task. Opening settings never changes access or clears a browser profile. */
export function BrowserTaskSettings() {
  const sessions = useSessionsStore(state => state.independent);
  const currentId = useSessionStore(state => state.sessionId);
  const [selected, setSelected] = useState(currentId ?? "");
  const options = currentId && !sessions.some(item => item.sessionId === currentId)
    ? [{ sessionId: currentId, title: "当前任务" }, ...sessions] : sessions;
  const selectedId = options.some(item => item.sessionId === selected) ? selected : "";
  const scope = useMemo(() => ({ pending: false }), [selectedId]);
  const automation = useSessionAutomation(selectedId || null);
  const working = useSessionStore(state => {
    const transcript = state.transcripts[selectedId];
    return Boolean(transcript?.streamingMessageId || transcript?.pendingSendNowPromptId
      || (state.sessionId === selectedId && (state.streaming || state.sendNowPending))
      || (!transcript && sessions.some(item => item.sessionId === selectedId && ["working", "pausing", "stopping", "awaiting_permission", "awaiting_answer"].includes(item.status ?? ""))));
  });
  const [operation, setOperation] = useState({ owner: scope, busy: false });
  const busy = operation.owner === scope && operation.busy;
  const setBusy = (value: boolean) => setOperation({ owner: scope, busy: value });
  const [result, setResult] = useState<{ owner: typeof scope; feedback: { error: boolean; text: string } | null }>({ owner: scope, feedback: null });
  const feedback = result.owner === scope ? result.feedback : null;
  const setFeedback = (value: typeof feedback) => setResult({ owner: scope, feedback: value });
  const { requestConfirmation, dialog } = useAppDialog(selectedId);
  const status = automation.status;
  const run = async (operation: () => Promise<AutomationStatus>, message: string) => {
    if (!selectedId || !automation.isActive() || scope.pending) return;
    scope.pending = true;
    setBusy(true);
    setFeedback(null);
    try {
      const next = await operation();
      if (!automation.isActive()) return;
      automation.setStatus(next);
      setFeedback({ error: false, text: message });
    } catch (error) {
      if (automation.isActive()) setFeedback({ error: true, text: friendlyError(error) });
    } finally {
      scope.pending = false;
      setOperation(current => current.owner === scope ? { owner: scope, busy: false } : current);
    }
  };
  const taskWorking = working;
  const label = options.find(item => item.sessionId === selectedId)?.title || "未命名任务";
  return <div className="browser-task-settings">
    <label className="settings-row settings-row--comfortable">
      <span className="settings-row__label"><span className="settings-row__name">选择任务</span></span>
      <select className="form-control settings-select" aria-label="管理浏览器数据的任务" value={selectedId} disabled={busy}
        onChange={event => { setSelected(event.target.value); setFeedback(null); }}>
        <option value="">请选择任务</option>
        {options.map(item => <option key={item.sessionId} value={item.sessionId}>{item.title || "未命名任务"}</option>)}
      </select>
    </label>
    {!selectedId ? <p className="settings-hint">选择需要管理的任务，其他任务的浏览器数据不受影响。</p>
      : automation.error ? <p className="settings-msg settings-msg--warn" role="alert">{automation.error}
        <button type="button" className="settings-btn" disabled={busy} onClick={automation.reconnect}>重新读取</button></p>
      : !status ? <p className="settings-hint" role="status">正在读取任务的浏览器设置…</p> : <>
        <label className="settings-row settings-row--comfortable">
          <span className="settings-row__label settings-row__label--stacked">
            <span className="settings-row__name">允许访问本机与内网</span>
            <span className="settings-row__description">仅对所选任务生效，可访问本地服务及内网站点。</span>
          </span>
          <input type="checkbox" checked={status.allowPrivateNetwork} disabled={busy}
            onChange={event => void run(() => automationSetPrivateNetwork(selectedId, event.target.checked), "访问范围已更新。")}/>
        </label>
        <div className="settings-row settings-row--comfortable">
          <div className="settings-row__label settings-row__label--stacked">
            <span className="settings-row__name">浏览器数据</span>
            <span className="settings-row__description">{taskWorking ? "任务运行中，请先停止任务再清除。" : status.browserHasData ? "包含该任务的登录状态、缓存和下载文件。" : "该任务暂无浏览器数据。"}</span>
          </div>
          <button type="button" className="settings-btn" disabled={busy || taskWorking || !status.browserHasData}
            onClick={() => requestConfirmation({ title: "清除该任务的浏览器数据？",
              description: `将停止“${label}”的执行并关闭浏览器，清除登录状态、缓存和下载文件。该任务的对话和其他任务的数据会保留。`,
              confirmLabel: "清除数据", danger: true,
              action: () => run(() => automationClearBrowserData(selectedId), "该任务的浏览器数据已清除。"),
            })}>清除浏览器数据</button>
        </div>
      </>}
    <AutomationApprovalCard automation={{ ...automation, approvals: automation.approvals.filter(request => request.tool === "automation_set_private_network") }} />
    {feedback && <p className={feedback.error ? "settings-msg settings-msg--warn" : "settings-hint"} role={feedback.error ? "alert" : "status"}>{feedback.text}</p>}
    {dialog}
  </div>;
}
