import { Info } from "lucide-react";
import { Button } from "./Button";

export function ProjectConnectionNotice({ error, loading = false, partialResults = false, onRetry, onOpenOrganization }: {
  error: string; loading?: boolean; partialResults?: boolean; onRetry: () => void; onOpenOrganization?: () => void;
}) {
  const expired = /\b401\b|unauthorized|token.*expired|登录.*(失效|过期)|请.*登录/i.test(error);
  const connection = /error sending request|fetch|network|connect|timed?\s*out|timeout|dns|certificate|tls|ssl/i.test(error);
  const message = expired ? "组织登录状态已过期"
    : connection ? "暂时无法连接组织服务"
      : /[\u4e00-\u9fff]/.test(error) && !/https?:\/\//.test(error) ? error
        : "暂时无法更新共享项目";
  return <div className="project-connection-notice" role="status">
    <Info size={16} aria-hidden="true" />
    <div className="project-connection-notice__copy">
      <p>{message}</p>
      <span>{partialResults && "当前仅显示本机项目。"}{expired ? "请前往组织页面重新登录。" : "共享内容暂时无法更新，可稍后重试。"}个人项目可正常使用。</span>
      {message !== error && <details><summary>技术详情</summary><pre>{error}</pre></details>}
    </div>
    {expired && onOpenOrganization
      ? <Button size="compact" onClick={onOpenOrganization}>前往组织</Button>
      : <Button size="compact" onClick={onRetry} disabled={loading}>{loading ? "重试中…" : "重试"}</Button>}
  </div>;
}
