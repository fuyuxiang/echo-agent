import "../../src/styles/index.css";
import React, { useState } from "react";
import { createRoot } from "react-dom/client";
import { ToolCallDetailBody } from "../../src/components/ToolCallCard";
import { ExecutionProcess } from "../../src/components/ExecutionProcess";
import { ThemeProvider } from "../../src/components/ThemeProvider";
import type { ToolCallView } from "../../src/stores/session-store";

document.documentElement.dataset.theme = new URLSearchParams(location.search).get("theme") ?? "light";
localStorage.setItem("echoagent.theme", document.documentElement.dataset.theme);
const call: ToolCallView = {
  toolCallId: "document-review", title: "Read 决赛准备3-评委参考问题.xlsx", kind: "read_file", status: "completed",
  documentProgress: { source: "决赛准备3-评委参考问题.xlsx", revision: "a".repeat(64), readRecords: 1240, totalRecords: 4001, checkpointCount: 12, complete: false,
    scope: "今年选手", checkpointPath: "/tmp/document-findings.md" },
  content: [{ type: "text", text: "已读取今年选手工作表的部分内容，源单元格位置和阶段记录已保存。" }],
};
function Review() {
  const [detail, setDetail] = useState(false);
  const [opened, setOpened] = useState("");
  return <main style={{ maxWidth: 900, margin: "48px auto", padding: 20, color: "var(--echo-text-strong)" }}>
    <p>正在逐位分析选手代码，已完成的阶段记录会保留。</p>
    <ExecutionProcess parts={[{ kind: "tool_call", toolCall: call }]} active autoExpand onOpenTool={() => setDetail(!detail)} />
    {detail && <ToolCallDetailBody tc={call} onOpenPath={setOpened} />}
    {opened && <p role="note">阶段记录已打开：{opened}</p>}
  </main>;
}
createRoot(document.getElementById("root")!).render(<ThemeProvider><Review /></ThemeProvider>);
