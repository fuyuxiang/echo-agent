import React from "react";
import { createRoot } from "react-dom/client";
import "../../src/styles/index.css";
import { TeamProjectsPanel } from "../../src/components/TeamProjectsPanel";
import { ThemeProvider } from "../../src/components/ThemeProvider";
import { useOrgSessionStore } from "../../src/stores/org-session-store";
import type { TeamProjectSnapshot } from "../../src/lib/team-project-client";

const theme = new URLSearchParams(location.search).get("theme") ?? "light";
localStorage.setItem("echoagent.theme", theme);
document.documentElement.dataset.theme = theme;
const project: TeamProjectSnapshot = {
  id: "019c0000-0000-7000-8000-000000000001", name: "客户接口建设", goal: "完成需求核对、接口设计与联调交付。保留关键决策依据，帮助后续成员接手。", scopeId: "project-scope", ownerId: "a", revision: 8, updatedAt: Date.parse("2026-11-16T10:00:00+08:00"), role: "admin",
  members: [{ userId: "a", username: "a", displayName: "张敏", role: "admin", status: "active" }, { userId: "c", username: "c", displayName: "李新", role: "collaborator", status: "active" }],
  tasks: [{ id: "task", title: "接口联调与异常处理验证", description: "接口方案已完成，按当前需求规范验证超时、重试与异常返回。", acceptance: "输出联调报告并完成项目负责人复核", status: "review", assigneeId: "b", assigneeName: "王工", revision: 3, updatedAt: Date.now() }],
  decisions: [{ id: "decision", content: "采用 REST 接口，统一返回业务错误码。", rationale: "保持与现有客户系统兼容，避免重复改造。", status: "active", sourceTaskId: "task", sourceDocumentId: null, supersedesId: null, revision: 2 }],
  runs: [{ id: "run", taskId: "task", actorId: "c", sessionId: "local-execution", status: "completed", modelId: "org-default", expertName: "接口验证专家", expertVersion: null, summary: "## 联调结论\n\n已验证超时、重试与错误码处理。\n\n- 成果：接口联调报告.md\n- 待验收：客户测试环境的限流参数\n\n请在资料与成果中查看已发布文件。", updatedAt: Date.now() }],
  assets: [{ id: "file", familyId: "family", title: "接口规范与联调说明.docx", version: 2, status: "parsing", ftsStatus: "pending", byteSize: 24000, currentDocumentId: "previous-file", updatedAt: Date.now() }], events: [],
};
Object.assign(window, { __TAURI_INTERNALS__: { metadata: { currentWindow: { label: "main" }, currentWebview: { label: "main" } }, invoke: async (command: string, args: any) => {
  if (command === "agents_list" || command === "group_team_templates") return [];
  if (command === "org_project_request") {
    if (args.action === "ask") return { answer: "接口方案已完成，联调工作待验收。当前采用 REST 接口以兼容现有系统。[cit-1]", verification: "supported", insufficient: false, projectRevision: 8, updatedAt: project.updatedAt, gaps: ["接口规范 v2 正在处理，当前引用上一有效版本。"], citations: [{ id: "cit-1", title: "接口联调工作项", quote: "接口方案已完成，联调待验收。", resourceId: "task", kind: "project_task", openUrl: "" }] };
    return structuredClone(project);
  }
  if (command === "org_fetch_document") return { text: "接口规范原文", chunks: [] };
  throw new Error(`Unimplemented isolated fixture command: ${command}`);
} } });
useOrgSessionStore.setState({ hydrated: true, boundaryRevision: 1, session: { loggedIn: true, accountGeneration: 1, serverUrl: "https://isolated.example", user: { id: "c", username: "c", displayName: "李新", role: "member", clearance: 0 } } });
createRoot(document.getElementById("root")!).render(<ThemeProvider><TeamProjectsPanel selectedId={project.id} onSelect={() => {}} models={[{ id: "org-default", label: "组织默认模型" }]} defaultModelId="org-default" onOpenExecution={async () => {}} /></ThemeProvider>);
