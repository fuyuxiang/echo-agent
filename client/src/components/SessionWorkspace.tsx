import { FolderOpen } from "lucide-react";
import { displayPath } from "@/lib/display-path";
import type { useSessionWorkspace } from "@/lib/use-session-workspace";

type Workspace = ReturnType<typeof useSessionWorkspace>;

export function SessionWorkspaceWarning({ workspace, onNewTask }: { workspace: Workspace; onNewTask?: () => void }) {
  if (!workspace.value || workspace.value.status === "ready") return null;
  return <div className="session-workspace-warning" role="alert">
    <strong>{workspace.value.status === "missing" ? "本任务的工作目录已找不到" : "本任务的工作目录暂不可用"}</strong>
    <span>请恢复原目录或重新授权后继续。使用其他目录请新建任务，历史记录会保留。</span>
    <span className="session-workspace-warning__path" title={workspace.cwd}>{displayPath(workspace.cwd ?? "")}</span>
    {workspace.value.message && <details><summary>查看原因</summary><p>{workspace.value.message}</p></details>}
    <div className="session-workspace-warning__actions">
      <button type="button" disabled={workspace.busy} onClick={() => void workspace.retry()}>{workspace.busy ? "正在检查…" : "重试连接"}</button>
      <button type="button" disabled={workspace.busy} onClick={() => void workspace.authorize()}>重新授权原目录</button>
      {onNewTask && <button type="button" disabled={workspace.busy} onClick={onNewTask}>新建任务</button>}
    </div>
  </div>;
}

export function SessionWorkspaceDirectory({ workspace, onOpen }: {
  workspace: Workspace;
  onOpen: () => void;
}) {
  return <section className="session-workspace-directory" aria-label="本任务工作目录">
    <div className="session-workspace-directory__heading">
      <strong>工作目录</strong>
      <button type="button" disabled={!workspace.cwd || !workspace.available || workspace.busy} onClick={onOpen}>
        <FolderOpen size={14} aria-hidden="true" />打开文件夹
      </button>
    </div>
    <span className="session-workspace-directory__path" title={workspace.cwd}>{displayPath(workspace.cwd ?? "尚未创建")}</span>
    <small>{workspace.checking ? "正在检查目录…" : workspace.available ? "本会话固定使用此目录；切换目录请新建任务" : "目录暂不可用，可恢复原目录或重新授权"}</small>
    {!workspace.checking && !workspace.available && <div className="session-workspace-warning__actions">
      <button type="button" disabled={workspace.busy} onClick={() => void workspace.retry()}>{workspace.busy ? "正在检查…" : "恢复连接"}</button>
      <button type="button" disabled={workspace.busy} onClick={() => void workspace.authorize()}>重新授权</button>
    </div>}
  </section>;
}
