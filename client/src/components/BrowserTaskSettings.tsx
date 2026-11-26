import { Select } from "@/components/Select";
import { useEffect, useMemo, useState } from "react";
import { useSessionsStore } from "@/stores/sessions-store";
import { useSessionStore } from "@/stores/session-store";
import { useSessionAutomation } from "@/lib/use-session-automation";
import { automationClearBrowserData, automationSetPrivateNetwork, automationStatus, automationPairBrowser, automationDisconnectBrowser, automationUseIsolatedBrowser, type AutomationStatus, type TaskBrowserSource } from "@/lib/automation-client";
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
  const taskWorking = working || Boolean(status?.task?.authorized && !status.paused);
  const extensionSource = status?.execution?.browserSource === "extension" || Boolean(status?.extensionConnected);
  const browserSource = status?.execution?.browserSource;
  const canUsePanel = status?.browserPanel?.web === true;
  const [pairing, setPairing] = useState<{ owner: typeof scope; endpoint: string; extensionPath: string; expiresAt: number; expired: boolean } | null>(null);
  const currentPairing = pairing?.owner === scope ? pairing : null;
  useEffect(() => {
    if (!pairing || pairing.expired) return;
    const timer = window.setTimeout(() => setPairing(current => current === pairing ? {...current, expired: true} : current), Math.max(0, pairing.expiresAt - Date.now()));
    return () => window.clearTimeout(timer);
  }, [pairing]);
  useEffect(() => {
    if (status?.extensionConnected) setPairing(current => current?.owner === scope ? null : current);
  }, [status?.extensionConnected, scope]);
  const pairBrowser = async () => {
    if (scope.pending || taskWorking || !automation.isActive()) return;
    scope.pending = true; setBusy(true); setFeedback(null);
    try {
      const value = await automationPairBrowser(selectedId);
      if (automation.isActive()) {
        setPairing({owner:scope,...value,expiresAt:Date.now() + value.expiresInSeconds * 1000,expired:false});
        await automation.refresh();
      }
    } catch (error) { if (automation.isActive()) setFeedback({error:true,text:friendlyError(error)}); }
    finally { scope.pending=false; setOperation(current => current.owner === scope ? {...current,busy:false} : current); }
  };
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
  const useTaskBrowser = (source?: TaskBrowserSource) => run(async () => {
    const next = await (source ? automationUseIsolatedBrowser(selectedId, source) : automationUseIsolatedBrowser(selectedId));
    if (automation.isActive()) setPairing(current => current?.owner === scope ? null : current);
    return next;
  }, source === "panel" ? "已切换为会话内置浏览器，可返回对话继续。"
    : source === "isolated" ? "已切换为任务独立浏览器，可返回对话继续。" : "已切换为任务浏览器，可返回对话继续。");
  const copyEndpoint = async () => {
    if (!currentPairing || currentPairing.expired) return;
    try {
      await navigator.clipboard.writeText(currentPairing.endpoint);
      if (automation.isActive()) setFeedback({error:false,text:"连接地址已复制。"});
    } catch {
      if (automation.isActive()) setFeedback({error:true,text:"复制未成功，请选中连接地址后手动复制。"});
    }
  };
  const label = options.find(item => item.sessionId === selectedId)?.title || "未命名任务";
  return <div className="browser-task-settings">
    <label className="settings-row settings-row--comfortable">
      <span className="settings-row__label"><span className="settings-row__name">管理哪个任务</span></span>
      <Select className="form-control settings-select" aria-label="管理浏览器数据的任务" value={selectedId} disabled={busy}
        onChange={event => { setSelected(event.target.value); setFeedback(null); }}>
        <option value="">请选择任务</option>
        {options.map(item => <option key={item.sessionId} value={item.sessionId}>{item.title || "未命名任务"}{item.sessionId === currentId && item.title !== "当前任务" ? "（当前任务）" : ""}</option>)}
      </Select>
    </label>
    {!selectedId ? <p className="settings-hint">{options.length ? "仅管理所选任务的浏览器数据。" : "暂无可管理的任务。"}</p>
      : automation.error ? <p className="settings-msg settings-msg--warn" role="alert">{automation.error}
        <button type="button" className="settings-btn" disabled={busy} onClick={automation.reconnect}>重新读取</button></p>
      : !status ? <p className="settings-hint" role="status">正在读取任务的浏览器设置…</p> : <>
        <h4 className="security-settings__browser-heading">浏览器连接</h4>
        <p className="settings-hint">内置浏览器可在侧栏接手，公网登录和浏览记录按账号共享。</p>
        <p className="settings-hint">独立浏览器为当前任务保留登录状态。</p>
        <p className="settings-hint">扩展连接已登录的 Chrome 或 Edge 标签，并沿用原浏览器的网络权限。</p>
        {taskWorking && <p className="settings-hint">请先停止任务再切换浏览器。</p>}
        <p className="settings-hint" role="status">{status.extensionConnected ? "已连接所选浏览器标签页"
          : extensionSource ? currentPairing && !currentPairing.expired ? "等待扩展连接" : "浏览器尚未连接，可重新连接或使用任务浏览器"
          : browserSource === "panel" ? "当前使用会话内置浏览器" : browserSource === "isolated" ? "当前使用任务独立浏览器" : "当前使用任务浏览器"}</p>
        <div className="browser-task-settings__actions">
        <button type="button" className="settings-btn" disabled={busy || taskWorking} onClick={() => void pairBrowser()}>{extensionSource ? "重新生成连接地址" : "连接已登录浏览器"}</button>
        {status.extensionConnected && <button type="button" className="settings-btn" disabled={busy || taskWorking}
          onClick={() => void run(async () => { await automationDisconnectBrowser(selectedId); return automationStatus(selectedId); }, "浏览器连接已断开。")}>断开浏览器连接</button>}
        {canUsePanel && browserSource !== "panel" && <button type="button" className="settings-btn" disabled={busy || taskWorking}
          onClick={() => void useTaskBrowser("panel")}>使用会话内置浏览器</button>}
        {status.isolatedBrowserAvailable === true && browserSource !== "isolated" && <button type="button" className="settings-btn"
          disabled={busy || taskWorking} onClick={() => void useTaskBrowser("isolated")}>使用任务独立浏览器</button>}
        {(extensionSource || currentPairing) && !canUsePanel && status.isolatedBrowserAvailable !== true
          && <button type="button" className="settings-btn" disabled={busy || taskWorking} onClick={() => void useTaskBrowser()}>使用任务浏览器</button>}
        </div>
        {currentPairing && !status.extensionConnected && <div className="settings-hint browser-task-settings__pairing">
          <p>在 Chrome 或 Edge 的扩展管理页开启开发者模式，选择“加载已解压的扩展程序”，使用下方目录：</p>
          <code>{currentPairing.extensionPath}</code>
          <p>在扩展中勾选允许访问的标签页，粘贴连接地址。</p>
          {currentPairing.expired ? <p role="status">连接地址已过期，请重新生成连接地址，或使用任务浏览器。</p> : <>
          <input className="form-control" readOnly aria-label="浏览器连接地址" value={currentPairing.endpoint} onFocus={event => event.currentTarget.select()} />
          <button type="button" className="settings-btn" disabled={busy} onClick={() => void copyEndpoint()}>复制连接地址</button>
          <p>连接地址五分钟内有效，仅用于当前任务。</p>
          </>}
        </div>}
        <h4 className="security-settings__browser-heading">访问范围</h4>
        <label className="settings-row settings-row--comfortable browser-task-settings__network">
          <span className="settings-row__label settings-row__label--stacked">
            <span className="settings-row__name">{extensionSource ? "允许导航到本机与内网页面" : "允许访问本机与内网"}</span>
            <span className="settings-row__description">{extensionSource ? "仅控制 Agent 主动打开的地址，页面自身的请求不受此开关限制。"
              : browserSource === "panel" ? "开启后新标签可访问本机与内网，关闭时会断开并关闭相关网页。也可在侧栏仅授权所需端口。"
              : "仅对所选任务生效，也可在侧栏只授权指定端口。"}</span>
          </span>
          <input type="checkbox" checked={status.allowPrivateNetwork} disabled={busy}
            onChange={event => void run(() => automationSetPrivateNetwork(selectedId, event.target.checked), "访问范围已更新。")}/>
        </label>
        <h4 className="security-settings__browser-heading">任务网页数据</h4>
        <p className="settings-hint">可在网页侧栏清除内置浏览器的公网登录和浏览记录。</p>
        {extensionSource && <p className="settings-hint">扩展的登录、缓存和下载文件在原浏览器中管理。</p>}
        <div className="settings-row settings-row--comfortable">
          <div className="settings-row__label settings-row__label--stacked">
            <span className="settings-row__name">下载文件与任务专用数据</span>
            <span className="settings-row__description">{taskWorking ? "任务运行中，请先停止任务再清除。" : status.browserHasData ? "关闭任务网页标签并清除下载文件和任务专用数据。" : "该任务暂无可清除的浏览器数据。"}</span>
          </div>
          <button type="button" className="settings-btn" disabled={busy || taskWorking || !status.browserHasData}
            onClick={() => requestConfirmation({ title: "清除该任务的浏览器数据？",
              description: `将关闭“${label}”的网页标签，并清除该任务的下载文件和专用浏览器数据。`,
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
