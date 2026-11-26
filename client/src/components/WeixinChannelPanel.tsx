import { SettingsDisclosure } from "./SettingsDisclosure";
import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import QRCode from "qrcode";
import { useAppDialog } from "./AppDialog";
import {
  weixinDisconnect, weixinQrPoll, weixinQrStart, weixinRevokeSession, weixinSetWorkspaces, weixinStatus,
  type WeixinStatus,
} from "@/lib/weixin-client";

interface Workspace { cwd: string; sessionCount?: number }

const STATUS: Record<string, string> = {
  wait: "等待扫码…", scaned: "已扫码，请在微信确认", confirmed: "绑定成功",
  need_verifycode: "请填写微信显示的配对码", verify_code_blocked: "配对码尝试受限，请重新获取二维码",
  expired: "二维码已过期，请刷新", binded_redirect: "该 Bot 已绑定，但本机没有可用凭据。请在微信解除绑定后重新扫码。",
  scaned_but_redirect: "正在连接微信服务…",
};

export function WeixinChannelPanel(_props: { onToast?: (message: string) => void }) {
  const [status, setStatus] = useState<WeixinStatus | null>(null);
  const [workspaces, setWorkspaces] = useState<Workspace[]>([]);
  const [selected, setSelected] = useState<string[]>([]);
  const [defaultWorkspace, setDefaultWorkspace] = useState("");
  const [statusError, setStatusError] = useState("");
  const [workspacesError, setWorkspacesError] = useState("");
  const [workspacesLoading, setWorkspacesLoading] = useState(true);
  const [qrUrl, setQrUrl] = useState("");
  const [qrImage, setQrImage] = useState("");
  const [qrStatus, setQrStatus] = useState("");
  const [verifyCode, setVerifyCode] = useState("");
  const [checkingCode, setCheckingCode] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  // Mutations advance this at start and settlement, invalidating status reads
  // launched before or during them, and any response after disconnect/unmount.
  const generation = useRef(0);
  const pollingGeneration = useRef<number | null>(null);
  const { requestConfirmation, dialog } = useAppDialog();

  useEffect(() => {
    let active = true;
    const current = generation.current;
    void weixinStatus().then((next) => {
      if (!active || current !== generation.current) return;
      setStatus(next);
      setSelected(next.allowedWorkspaces);
      setDefaultWorkspace(next.defaultWorkspace ?? next.allowedWorkspaces[0] ?? "");
      setStatusError("");
    }).catch((cause) => { if (active && current === generation.current) setStatusError(String(cause)); });
    void invoke<Workspace[]>("agent_list_workspaces")
      .then((spaces) => { if (active) { setWorkspaces(spaces); setWorkspacesError(""); setWorkspacesLoading(false); } })
      .catch((cause) => { if (active) { setWorkspacesError(String(cause)); setWorkspacesLoading(false); } });
    return () => { active = false; generation.current += 1; };
  }, []);

  useEffect(() => {
    if (!status?.connected) return;
    let active = true;
    let inFlight = false;
    const timer = window.setInterval(() => {
      if (inFlight) return;
      inFlight = true;
      const current = generation.current;
      void weixinStatus().then((next) => {
        if (active && current === generation.current) { setStatus(next); setStatusError(""); }
      }).catch((cause) => {
        if (active && current === generation.current) setStatusError(String(cause));
      }).finally(() => { inFlight = false; });
    }, 10_000);
    return () => { active = false; window.clearInterval(timer); };
  }, [status?.connected]);

  const refreshStatus = () => {
    const current = generation.current;
    void weixinStatus().then((next) => {
      if (current !== generation.current) return;
      if (!status) {
        setSelected(next.allowedWorkspaces);
        setDefaultWorkspace(next.defaultWorkspace ?? next.allowedWorkspaces[0] ?? "");
      }
      setStatus(next);
      setStatusError("");
    }).catch((cause) => { if (current === generation.current) setStatusError(String(cause)); });
  };

  const refreshWorkspaces = () => {
    setWorkspacesLoading(true);
    void invoke<Workspace[]>("agent_list_workspaces")
      .then((spaces) => { setWorkspaces(spaces); setWorkspacesError(""); setWorkspacesLoading(false); })
      .catch((cause) => { setWorkspacesError(String(cause)); setWorkspacesLoading(false); });
  };

  const toggleWorkspace = (cwd: string, checked: boolean) => {
    const next = checked ? [...selected, cwd] : selected.filter((item) => item !== cwd);
    setSelected(next);
    if (!next.includes(defaultWorkspace)) setDefaultWorkspace(next[0] ?? "");
  };

  useEffect(() => {
    if (!qrUrl) { setQrImage(""); return; }
    let active = true;
    void QRCode.toDataURL(qrUrl, { margin: 2, width: 232 })
      .then((image) => { if (active) setQrImage(image); })
      .catch(() => { if (active) setError("二维码绘制失败，请重新获取"); });
    return () => { active = false; };
  }, [qrUrl]);

  const start = async () => {
    const current = ++generation.current;
    pollingGeneration.current = null;
    setBusy(true);
    setError("");
    setQrUrl("");
    setQrStatus("");
    setVerifyCode("");
    setCheckingCode(false);
    try {
      const result = await weixinQrStart();
      if (current !== generation.current) return;
      setQrUrl(result.qrUrl);
      setQrStatus("wait");
      void poll(current);
    } catch (cause) { if (current === generation.current) setError(String(cause)); }
    finally { if (current === generation.current) setBusy(false); }
  };

  const poll = async (current: number, code?: string) => {
    if (pollingGeneration.current === current) return;
    pollingGeneration.current = current;
    if (code) setCheckingCode(true);
    try {
      while (current === generation.current) {
        try {
          const result = await weixinQrPoll(code);
          if (current !== generation.current) return;
          setError("");
          setQrStatus(result.status);
          if (result.connected) {
            const next = await weixinStatus();
            if (current !== generation.current) return;
            setStatus(next);
            setSelected(next.allowedWorkspaces);
            setDefaultWorkspace(next.defaultWorkspace ?? next.allowedWorkspaces[0] ?? "");
            setQrUrl("");

            return;
          }
          if (["expired", "verify_code_blocked", "binded_redirect"].includes(result.status)) return;
          if (result.status === "need_verifycode") return;
          code = undefined;
          if (result.status !== "wait") await new Promise((resolve) => setTimeout(resolve, 1000));
        } catch (cause) {
          if (current !== generation.current) return;
          setQrStatus("连接暂时中断，正在重试…");
          await new Promise((resolve) => setTimeout(resolve, 3000));
          if (current !== generation.current) return;
          setError(String(cause));
        }
      }
    } finally {
      if (pollingGeneration.current === current) pollingGeneration.current = null;
      if (current === generation.current) setCheckingCode(false);
    }
  };

  const submitCode = () => {
    if (!/^\d{1,32}$/.test(verifyCode)) { setError("请输入微信显示的数字配对码"); return; }
    setError("");
    void poll(generation.current, verifyCode);
  };

  const saveWorkspaces = async () => {
    const current = ++generation.current;
    setBusy(true); setError("");
    try {
      const next = await weixinSetWorkspaces(selected, defaultWorkspace || undefined);
      if (current !== generation.current) return;
      setStatus(next);
      setSelected(next.allowedWorkspaces);
      setDefaultWorkspace(next.defaultWorkspace ?? next.allowedWorkspaces[0] ?? "");
    } catch (cause) { if (current === generation.current) setError(String(cause)); }
    finally {
      if (current === generation.current) { generation.current += 1; setBusy(false); }
    }
  };

  const revokeSession = async (sessionId: string) => {
    const current = ++generation.current;
    setBusy(true); setError("");
    try {
      const next = await weixinRevokeSession(sessionId);
      if (current !== generation.current) return;
      setStatus(next);
    } catch (cause) { if (current === generation.current) setError(String(cause)); }
    finally {
      if (current === generation.current) { generation.current += 1; setBusy(false); }
    }
  };

  const disconnect = () => requestConfirmation({
    title: "解除微信绑定？",
    description: "本机将停止接收微信指令，并删除本机的绑定凭据。之后可重新扫码绑定。",
    confirmLabel: "解除绑定", danger: true,
    action: async () => {
      const current = ++generation.current;
      pollingGeneration.current = null;
      setBusy(false);
      try {
        await weixinDisconnect();
        if (current !== generation.current) return;
        setStatus({ connected: false, allowedWorkspaces: [], sharedSessions: [], pendingReplies: 0, online: false });
        setSelected([]); setQrUrl("");
        setDefaultWorkspace(""); setStatusError("");
      } finally {
        if (current === generation.current) generation.current += 1;
      }
    },
    onError: (cause) => setError(String(cause)),
  });

  return <div className="weixin-channel">

    {error && <div className="panel-inline-error" role="alert">{error}<button type="button" className="settings-btn" onClick={() => setError("")}>关闭</button></div>}
    {!status && <div className="weixin-channel__card" role="status">{statusError ? <>绑定状态读取失败：{statusError}<div className="weixin-channel__recovery"><button type="button" className="settings-btn" onClick={refreshStatus}>重试</button><button type="button" className="settings-btn settings-btn--danger weixin-channel__secondary" onClick={disconnect}>清除本机绑定</button></div></> : "正在读取绑定状态…"}</div>}
    {status && !status.connected && <section className="weixin-channel__card">
      <h3>绑定微信</h3>
      <p>用微信扫码并确认，二维码仅在本机显示。</p>
      {qrImage && <img className="weixin-channel__qr" src={qrImage} alt="微信绑定二维码" />}
      {qrUrl && <p aria-live="polite">{STATUS[qrStatus] ?? qrStatus}</p>}
      {qrStatus === "need_verifycode" && <div className="weixin-channel__code"><input className="form-control" aria-label="微信配对码" inputMode="numeric" value={verifyCode} onChange={(event) => setVerifyCode(event.target.value)} placeholder="微信显示的配对码" disabled={checkingCode} /><button type="button" className="settings-btn settings-btn--primary" onClick={submitCode} disabled={checkingCode}>{checkingCode ? "正在验证…" : "提交配对码"}</button></div>}
      <button type="button" className="settings-btn settings-btn--primary" onClick={() => void start()} disabled={busy}>{busy ? "正在获取…" : qrUrl ? "刷新二维码" : "获取绑定二维码"}</button>
    </section>}
    {status?.connected && <>
      <section className="weixin-channel__card"><div className="weixin-channel__card-header"><h3>{status.online && !statusError ? "已连接" : "已绑定，连接中"}</h3><button type="button" className="settings-btn weixin-channel__secondary" onClick={refreshStatus}>刷新状态</button></div>
        <p>连接状态：{statusError ? `无法确认 · ${statusError}` : status.online ? "正在接收微信消息" : "暂时无法接收消息，等待自动恢复"}{status.lastError && !statusError ? ` · ${status.lastError}` : ""}</p>
        {status.pendingReplies > 0 && <p role="status">{status.pendingReplies} 条文字回复待发送，连接恢复后自动重试。</p>}

        <p>当前会话：{status.activeSession ? `${status.activeSessionTitle ?? "任务"}` : "尚未选择"}</p>
        <p>右键任务并选择「在微信继续」，即可切换微信会话。</p>
      </section>
      <section className="weixin-channel__card"><h3>授权工作区</h3>
        <p>微信可访问授权目录中的任务，新任务默认在标记的目录创建。</p>
        {workspacesLoading && <p role="status">正在读取工作区…</p>}
        {workspacesError && <p role="alert">工作区列表读取失败：{workspacesError} <button type="button" className="settings-btn weixin-channel__secondary" onClick={refreshWorkspaces}>重试</button></p>}
        {!workspacesLoading && !workspacesError && workspaces.length === 0 && <p>暂无已有工作区。先在桌面创建任务。</p>}
        {!workspacesLoading && !workspacesError && selected.filter((cwd) => !workspaces.some((space) => space.cwd === cwd)).map((cwd) => <div className="weixin-channel__workspace" key={cwd}>
          <label className="weixin-channel__workspace-access"><input type="checkbox" checked onChange={() => toggleWorkspace(cwd, false)} /><span title={cwd}>{cwd}</span></label>
          <small>当前列表中不可用，可取消授权</small>
        </div>)}
        {!workspacesLoading && !workspacesError && workspaces.map((space) => <div className="weixin-channel__workspace" key={space.cwd}>
          <label className="weixin-channel__workspace-access"><input type="checkbox" checked={selected.includes(space.cwd)} onChange={(event) => toggleWorkspace(space.cwd, event.target.checked)} /><span title={space.cwd}>{space.cwd}</span></label>
          <small>{selected.includes(space.cwd) && `微信编号 ${selected.indexOf(space.cwd) + 1} · `}{space.sessionCount ?? 0} 个任务</small>
          <label className="weixin-channel__workspace-default"><input type="radio" name="weixin-default-workspace" checked={defaultWorkspace === space.cwd && selected.includes(space.cwd)} disabled={!selected.includes(space.cwd)} onChange={() => setDefaultWorkspace(space.cwd)} aria-label={`设为默认工作区 ${space.cwd}`} />默认</label>
        </div>)}
        <button type="button" className="settings-btn settings-btn--primary" onClick={() => void saveWorkspaces()} disabled={busy || workspacesLoading || !!workspacesError || (selected.join("\0") === status.allowedWorkspaces.join("\0") && defaultWorkspace === (status.defaultWorkspace ?? status.allowedWorkspaces[0] ?? ""))}>保存访问范围</button>
      </section>
      <SettingsDisclosure title="单独交接的任务" description={`${status.sharedSessions.length} 个任务 · 可单独撤销交接授权`}>
      <section className="weixin-channel__card"><h3>交接授权</h3>
        <p>撤销交接后，仍可通过已授权的工作区访问该任务。</p>
        {status.sharedSessions.length === 0 && <p>暂无单独交接的任务。</p>}
        {status.sharedSessions.map((session) => <div className="weixin-channel__shared" key={session.sessionId}>
          <div><strong>{session.title}</strong><small>#{session.sessionId.slice(-8)}{session.cwd && ` · ${session.cwd}`}</small>{status.allowedWorkspaces.includes(session.cwd) && <small>所属工作区已授权，撤销交接后仍可访问</small>}</div>
          <button type="button" className="settings-btn" onClick={() => void revokeSession(session.sessionId)} disabled={busy} aria-label={`撤销 ${session.title} 的单独交接授权`}>撤销交接</button>
        </div>)}
      </section>
      </SettingsDisclosure>
      <SettingsDisclosure title="连接详情" description="查看微信 Bot 标识和当前会话编号。">
        <p className="settings-hint">微信 Bot：{status.botId}</p>
        <p className="settings-hint">当前会话编号：{status.activeSession ?? "尚未选择"}</p>
      </SettingsDisclosure>
      <details className="weixin-channel__card weixin-channel__guide"><summary>微信指令与使用说明</summary>
        <div className="weixin-channel__commands"><span>任务 / 任务2</span><span>每页 5 条；也支持“任务 2”</span><span>切换 #编号</span><span>进入已有会话</span><span>工作区</span><span>查看新任务可用目录</span><span>新任务：内容</span><span>在默认工作区新建任务</span><span>新任务 2：内容</span><span>在指定工作区新建任务</span><span>状态 / 停止</span><span>查看或停止当前任务</span></div>
        <p>发送「/文件 相对路径」可取回当前工作区内不超过 20 MB 的文件。</p>
        <p>发送文字、语音转写、图片或文档可继续当前会话。任务沿用桌面权限，待确认的请求会带编号。</p>
      </details>
      <button type="button" className="settings-btn settings-btn--danger weixin-channel__disconnect" onClick={disconnect}>解除绑定</button>
    </>}
    {dialog}
  </div>;
}
