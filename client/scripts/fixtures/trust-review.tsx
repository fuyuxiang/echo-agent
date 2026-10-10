import { useMemo, useState } from "react";
import { FolderTrustDialog } from "../../src/components/FolderTrustDialog";
import type { PendingInteractions } from "../../src/lib/agent-client";

export type TrustReviewVariant = "short" | "long" | "loading" | "error";

/** Returned only by the UI-review bridge; never sends a native/service command. */
export function trustReviewPending(variant: TrustReviewVariant = "short"): PendingInteractions {
  const workspace = variant === "long"
    ? `/workspace/项目资料/${"长期维护的项目目录/".repeat(12)}configuration-and-generated-agent-results`
    : "/workspace/echo-agent";
  return {
    permissions: [],
    questions: [],
    planApprovals: [],
    folderTrustRequests: [{
      requestId: "ui-review-trust-1",
      sessionId: "ui-review-session",
      cwd: workspace,
      workspace,
      configKinds: variant === "long"
        ? Array.from({ length: 28 }, (_, index) => `项目配置 ${index + 1}：MCP 工具、插件和自动执行 hooks ${"long-configuration-name-".repeat(2)}`)
        : ["MCP 工具", "项目 hooks"],
    }],
  };
}

export const trustReviewError = "无法读取工作区配置，请检查本地连接后重试。".repeat(18);

/** Uses the production queue/dialog; the review page supplies the invoke mock. */
export default function TrustReview({ variant = "short" }: { variant?: TrustReviewVariant }) {
  const [resolved, setResolved] = useState(false);
  const request = useMemo(() => trustReviewPending(variant).folderTrustRequests[0], [variant]);
  return (
    <main style={{ padding: 32 }}>
      <h1>工作区配置</h1>
      <p>文件夹信任确认、队列与错误状态</p>
      {resolved ? <p role="status">信任请求已处理</p> : (
        <FolderTrustDialog request={request} onResolve={() => setResolved(true)} />
      )}
    </main>
  );
}
