import { Info } from "lucide-react";
import { projectConnectionIssue } from "@/lib/team-project-client";
import { Button } from "./Button";

export function ProjectConnectionNotice({ error, loading = false, openingProject = false, partialResults = false, localRecord = false, onRetry, onOpenOrganization }: {
  error: string; loading?: boolean; openingProject?: boolean; partialResults?: boolean; localRecord?: boolean; onRetry?: () => void; onOpenOrganization?: () => void;
}) {
  const issue = projectConnectionIssue(error);
  const message = issue === "sign-in" ? "组织登录状态已过期"
    : issue === "access" ? openingProject ? "当前无法访问此共享项目" : "当前无法访问共享项目"
    : issue === "connection" ? "暂时无法连接组织服务"
      : /[\u4e00-\u9fff]/.test(error) && !/https?:\/\//.test(error) ? error
        : openingProject ? "暂时无法打开共享项目" : "暂时无法更新共享项目";
  const help = issue === "sign-in" ? "请前往组织页面重新登录。"
    : issue === "deleted" ? localRecord ? "你可以继续查看此设备上的任务和资料。" : "该项目的共享任务和资料已停止提供访问。"
    : issue === "access" ? `${localRecord ? "此设备上的任务和资料仍可查看。" : ""}可联系项目管理员确认访问权限。`
    : localRecord ? "此设备上的任务和资料仍可查看。可稍后重试连接。"
    : openingProject ? "可稍后重试打开此项目。" : "共享内容暂时无法更新，可稍后重试。";
  return <div className="project-connection-notice" role="status">
    <Info size={16} aria-hidden="true" />
    <div className="project-connection-notice__copy">
      <p>{message}</p>
      <span>{partialResults && "当前共享列表可能不完整。"}{help}</span>
      {message !== error && <details><summary>技术详情</summary><pre>{error}</pre></details>}
    </div>
    {issue === "deleted" ? null : issue === "sign-in" && onOpenOrganization
      ? <Button size="compact" onClick={onOpenOrganization}>前往组织</Button>
      : onRetry && <Button size="compact" onClick={onRetry} disabled={loading}>{loading ? "重试中…" : "重试"}</Button>}
  </div>;
}
