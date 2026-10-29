// Isolated visual/interaction review using the production composer and menus.
import "../../src/styles/index.css";
import React, { useState } from "react";
import { createRoot } from "react-dom/client";
import { ThemeProvider } from "../../src/components/ThemeProvider";
import { TheiaAgentComposer } from "../../src/features/coding/agent/TheiaAgentComposer";
import { TaskSwitcher } from "../../src/features/coding/shell/TaskSwitcher";
import { ProjectChecksEditor } from "../../src/features/coding/panels/ProjectChecksEditor";
import { ExternalToolRecovery } from "../../src/features/coding/panels/ExternalToolRecovery";
import type { CodingTask } from "../../src/features/coding/lib/types";
import { usePermissionModeStore } from "../../src/stores/permission-mode-store";
import type { PermissionMode } from "../../src/lib/agent-client";
import { saveFontSize } from "../../src/lib/font-size";

const query = new URLSearchParams(location.search);
localStorage.setItem("echoagent.theme", query.get("theme") ?? "light");
saveFontSize(Number(query.get("font")) || 13);
const initialMode: PermissionMode = query.get("mode") === "always" ? "always-approve" : "ask";
usePermissionModeStore.setState({ homeMode: initialMode });
let mode = initialMode;
let pendingRecovery = true;
Object.assign(window, { __TAURI_INTERNALS__: {
  invoke: async (command: string, args: { sessionId?: string; mode?: PermissionMode; config?: unknown } = {}) => {
    if (command === "coding_verification_config_get") return {checks:[{id:"lint",label:"语法检查",kind:"lint",command:"node --check src/index.js",cwd:".",timeoutSecs:300,env:{NODE_ENV:"test"},autoRun:false}]};
    if (command === "coding_verification_config_save") return args.config;
    if (command === "coding_external_tools_pending") return pendingRecovery ? [{operationId:"operation",runId:"run",toolName:"外部文件工具",localInflight:false}] : [];
    if (command === "coding_external_tool_recover") {pendingRecovery=false;return;}
    if (command === "permission_mode_set") mode = args.mode ?? "ask";
    if (command === "permission_mode_get" || command === "permission_mode_set") return {
      sessionId: args.sessionId, permissionMode: mode, configuredPermissionMode: mode,
      autoModeAvailable: true, alwaysApproveAvailable: true, locked: false, runtimeSyncState: "synced",
    };
    throw new Error(`Unexpected isolated command: ${command}`);
  },
} });
const models = [
  { id: "review/MiniMax-M3.1-Flash-20261019", label: "MiniMax-M3.1-Flash-企业研发长名称模型", providerId: "review" },
  { id: "review/another-model", label: "另一个模型", providerId: "review" },
];
const state = query.get("state");
const task: CodingTask | null = ["followup", "streaming", "verifying"].includes(state ?? "") ? {
  schemaVersion: 2, id: "task", name: "检查输入体验", requirement: "实现输入卡片", phase: state === "verifying" ? "verifying" : "implementing",
  sessionId: "review-session", acceptanceCriteria: [], taskNodes: [], planIssues: [], globalConstraints: [], createdAt: "", updatedAt: "",
} : null;

function Review() {
  const [modelId, setModelId] = useState(models[0].id);
  const [busy, setBusy] = useState(false);
  const [queued, setQueued] = useState<string[]>([]);
  const [sent, setSent] = useState<string[]>([]);
  const [context, setContext] = useState(state === "context" ? ["src/一个名称很长的目录/开发任务输入卡片组件.tsx", "src/styles/coding-workbench.css"] : []);
  const [toast, setToast] = useState("");
  const send = async (text: string) => {
    setBusy(true);
    await new Promise((resolve) => setTimeout(resolve, 180));
    setSent((previous) => [...previous, text]);
    setBusy(false);
  };
  if (state === "settings") return <div className="coding-workbench coding-workbench--theia" style={{"--echo-agent-width":`${Number(query.get("width"))||410}px`} as React.CSSProperties}>
    <main className="echo-theia-workspace" />
    <aside className="echo-theia-agent">
      <div className="coding-workspace-detail" style={{flex:1}}><ProjectChecksEditor root="/isolated/review" onSaved={()=>{}}/></div>
      <ExternalToolRecovery root="/isolated/review" onRecovered={()=>setToast("工程已恢复")}/>
      <p role="status">{toast}</p>
    </aside>
  </div>;
  return <div className="coding-workbench coding-workbench--theia" style={{ "--echo-agent-width": `${Number(query.get("width")) || 410}px` } as React.CSSProperties}>
    <header className="coding-workbench__topbar" style={{ padding: "0 16px" }}>代码开发 · EchoAgent</header>
    <main className="echo-theia-workspace" style={{ display: "grid", placeItems: "center", color: "var(--echo-text-weak)" }}>打开文件开始编辑</main>
    <div className="echo-theia-agent__splitter" />
    <aside className="echo-theia-agent">
      <div className="echo-theia-agent__heading"><TaskSwitcher tasks={[]} activeId={task?.id} activeName={task?.name}
        onSelect={() => {}} onNew={() => {}} onRename={() => {}} onDelete={() => {}} /></div>
      <div style={{ flex: 1, minHeight: 0, overflow: "auto", padding: 16 }}>
        <output aria-label="排队记录">{queued.length} 条待发送</output>
        <output aria-label="提交记录">{sent.length} 次提交{sent.length > 0 && `：${sent[sent.length - 1]}`}</output>
        {toast && <p role="status">{toast}</p>}
      </div>
      <TheiaAgentComposer workspaceRoot="/isolated/review" task={task} sessionId={task?.sessionId ?? null}
        models={state === "unconfigured" ? [] : models} modelId={state === "unconfigured" ? undefined : modelId}
        contextPaths={context} apiReady={state !== "unconfigured"} starting={!task && busy} sending={false} streaming={state === "streaming"}
        startError={state === "error" ? "启动失败，请保留草稿后重试。" : null}
        onModelChange={setModelId} onStart={send} onSend={async (text) => { await send(text); return true; }}
        onEnqueue={(text) => {setQueued(previous => [...previous, text]); return true;}}
        onCaptureContext={(kind) => setToast(`捕获上下文：${kind}`)}
        onRemoveContext={(path) => setContext((previous) => previous.filter((item) => item !== path))}
        onDraftContextPaths={setContext} onOpenSettings={() => setToast("已打开模型设置")} onToast={setToast} />
    </aside>
  </div>;
}
createRoot(document.getElementById("root")!).render(<ThemeProvider><Review /></ThemeProvider>);
