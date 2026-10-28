import { BrowserPreview } from "../../src/components/BrowserPreview";
import { AboutDialog } from "../../src/components/AboutDialog";
import { FeedbackDialog } from "../../src/components/FeedbackDialog";
import { UpdateDialog } from "../../src/components/UpdateDialog";
import { useUpdateStore } from "../../src/stores/update-store";
import { DesignSystemReview } from "./design-system-review";
import { ToastReview } from "./toast-review";
import { ContentMenusReview } from "./content-menus-review";
import { SearchOverlay } from "../../src/components/SearchOverlay";
import "../../src/styles/index.css";
// Development-only visual fixture. Native calls use isolated in-memory data;
// this module is never imported by the production entry point.
import React, { useState } from "react";
import { createRoot } from "react-dom/client";
import { SidebarLayoutContext, useAppSidebarLayout } from "../../src/lib/sidebar-layout";
import { SidebarResizeHandle } from "../../src/components/SidebarResizeHandle";
import { ThemeProvider } from "../../src/components/ThemeProvider";
import { SettingsPanel, type SettingsSectionId } from "../../src/components/SettingsPanel";
import { PlaceholderPage } from "../../src/components/PlaceholderPage";
import { ProjectDetailView } from "../../src/components/ProjectDetailView";
import { WorkbenchIdentity } from "../../src/features/coding/shell/WorkbenchIdentity";
import { ProjectSwitcher } from "../../src/features/coding/shell/ProjectSwitcher";
import { TaskSwitcher } from "../../src/features/coding/shell/TaskSwitcher";
import { TheiaAgentComposer } from "../../src/features/coding/agent/TheiaAgentComposer";
import { OrganizationMemoryPanel } from "../../src/components/OrganizationMemoryPanel";
import { useSubagentStore } from "../../src/stores/subagent-store";
import { usePermissionStore } from "../../src/stores/permission-store";
import { ChatView } from "../../src/components/ChatView";
import { Sidebar } from "../../src/components/Sidebar";
import { AutomationEditPage } from "../../src/components/automation/AutomationEditPage";
import { AutomationPanel } from "../../src/components/AutomationPanel";
import type { Automation } from "../../src/lib/types";
import { buildDraft, draftFromAutomation } from "../../src/components/automation/schedule-utils";
import { useAutomationEditorStore } from "../../src/stores/automation-editor-store";
import { createAutomationTemplates } from "../../src/components/automation/template-config";
import { WorkspacePicker } from "../../src/components/WorkspacePicker";
import { GlobalTooltip } from "../../src/components/GlobalTooltip";
import { TasksPanel } from "../../src/components/TasksPanel";
import { MeetingMinutesPanel } from "../../src/components/MeetingMinutesPanel";
import { PermissionPicker } from "../../src/components/PermissionPicker";
import { HomePage } from "../../src/components/HomePage";
import { ProjectsPanel } from "../../src/components/ProjectsPanel";
import ModalReview from "./modal-review";
import ResultReviewFixture from "./result-review";
import DetailReview from "./detail-review";
import TrustReview, { trustReviewPending, trustReviewError } from "./trust-review";
import ExecutionReviewFixture, { executionReviewTeamSnapshot, executionReviewFinalMarkdown } from "./execution-review";
import { useSessionStore } from "../../src/stores/session-store";
import { useSessionsStore } from "../../src/stores/sessions-store";
import { useProjectsStore } from "../../src/stores/projects-store";
import { useGroupStore } from "../../src/features/groups/group-store";
import { useMessageQueueStore } from "../../src/stores/message-queue-store";

const query = new URLSearchParams(location.search);
const recoveryReview = { permissionResolutions: 0, modeChanges: 0 };
Object.assign(window, { recoveryReview });
if (query.get("surface") === "update") useUpdateStore.setState({ status: "error", error: "暂时无法检查更新，请保留现有工作并稍后重试。" });
if (query.get("surface") === "runtime") query.set("executionView", "runtime");
localStorage.setItem("echoagent.theme", query.get("theme") ?? "light");
let memory = { enabled: true, initialInjectionEnabled: true, saveOnEnd: true, watcherEnabled: true,
  autoFlushEnabled: true, dreamEnabled: true, retrievalMode: "local", revision: "1",
  retrievalSummary: "仅在本机进行全文检索；摘要和整理仍使用会话模型" };
let providers: unknown[] = query.get("settingsDesign") === "1" ? [{
  id: "review-dav", label: "团队文件", kind: "webdav", baseUrl: "https://review.invalid/dav", enabled: true,
}] : [];
let channels: import("../../src/lib/notify-channels").NotifyChannel[] = [];
let notificationTitlePreview = false;
let notificationChannelReadFailed = false;
let notificationDesktopReadFailed = false;
if (query.get("notificationState") === "configured") channels = [
  { id: "review-webhook", label: "团队任务动态", kind: "slack-webhook", endpoint: "https://hooks.example.test/private-review-token", enabled: true },
  { id: "review-email", label: "旧邮件配置", kind: "email", enabled: true },
];
let notificationEntries: import("../../src/lib/types").NotificationEntry[] = query.get("notificationState") === "records" ? [
  { id: 1, kind: "permission", title: "整理报告需要授权", body: "打开任务查看请求并决定是否允许", severity: "warn", read: false, sessionId: "review-task", requestId: "review-permission", at: "2026-10-18T10:00:00Z" },
  { id: 2, kind: "session_complete", title: "任务《本周工作报告整理》已完成", body: "打开任务查看结果", severity: "info", read: false, sessionId: "review-task", at: "2026-10-18T09:00:00Z" },
  { id: 3, kind: "mcp_status", title: "文档工具连接失败", body: JSON.stringify({ status: "disconnected", message: "review-technical-details" }), severity: "warn", read: true, at: "2026-10-18T08:00:00Z" },
  { id: 4, kind: "models_update", title: "模型目录已更新", severity: "info", read: true, at: "2026-10-18T07:00:00Z" },
] : [];

let automationFlowTask: Automation = { ...buildDraft(undefined, "/review/EchoAgent"), id: "review-automation", name: "保留草稿与通知导航验收任务", prompt: "整理交付报告", status: "PAUSED", createdAt: "2026-10-17T00:00:00Z" };
let automationFlowRemoved = query.get("state") === "removed";
if (query.get("surface") === "automation-flow" && automationFlowRemoved) {
  useAutomationEditorStore.getState().begin(draftFromAutomation(automationFlowTask), automationFlowTask);
}
let agents: unknown[] = [];
const extensionState = query.get("extensionState");
const extensionCount = extensionState === "long" ? 48 : ["populated", "error"].includes(extensionState ?? "") ? 3 : 0;
const extensionNames = ["需求评审", "文档整理", "数据分析"];
const extensionName = (index: number) => `${extensionNames[index % extensionNames.length]} ${String(index + 1).padStart(2, "0")}`;
if (extensionCount) agents = Array.from({ length: extensionCount }, (_, index) => ({
  name: extensionName(index), description: "检查工作目标与交付结果，整理可以继续使用的建议。", scope: "user",
  path: `/review/agents/expert-${index}.md`, raw: "检查工作目标与交付结果。", modelTags: ["default"], avatar: index % 20 + 1,
}));
let extensionSkills = Array.from({ length: extensionCount }, (_, index) => ({
  name: `review-skill-${index}`, displayName: extensionName(index), description: "梳理需求、检查实现并生成可验证的交付报告。",
  scope: index % 2 ? "plugin" : "user", enabled: true, path: `/review/skills/skill-${index}/SKILL.md`,
}));
let extensionPlugins = Array.from({ length: extensionCount }, (_, index) => ({
  id: `user/review-${index}`, name: extensionName(index), description: "提供任务所需的技能、专家与工具。",
  root: `/review/plugins/plugin-${index}`, scope: "user", enabled: true, trusted: true,
  version: "1.0.0", skillCount: 2, agentCount: 1, mcpServerCount: 1,
  ...(query.get("pluginManagement") === "1" ? {management:{uninstallable:index===0,removable:index===1,packagePlugins:[extensionName(index)],removalPath:`/review/plugins/plugin-${index}`,pathPlugins:[extensionName(index)]}} : {}),
}));
let extensionConnectors = Array.from({ length: extensionCount }, (_, index) => ({
  name: `review-mcp-${index}`, displayName: extensionName(index), transport: "stdio", target: `review-tool-${index}`,
  enabled: true, source: "user", status: "ready", live: true, editable: true, setupValues: {}, args: [], env: {},
  tools: [{ name: "review", displayName: "检查交付结果", description: "检查需求和产物的一致性。", enabled: true }],
}));
let extensionConfig = JSON.stringify({ mcpServers: { "review-tool": { command: "review-tool" } } }, null, 2);
const extensionAttempts: Record<string, number> = {};
if (extensionState && extensionCount) useSessionStore.getState().setSession("review-extension-session");
let rules: unknown[] = [];
const orgUser = { id: "review-user", username: "review", displayName: "界面检查", role: "member", clearance: 1 };
const orgScopes = [
  { id: "mine", kind: "personal", name: "我的空间", canPublishDocuments: true },
  { id: "team", kind: "team", name: "产品研发团队", canPublishDocuments: true },
];
if (query.get("orgName")) orgScopes.push({ id: "org", kind: "org", name: query.get("orgName")!, canPublishDocuments: true });
const orgDocuments = [
  { id: "review-docx", title: "自动化分析报告功能介绍.docx", sourceType: "docx", status: "ready", byteSize: 453740, scopeId: "team", scopeKind: "team", scopeName: "产品研发团队", ownerId: "another-user", chunkCount: 12, tags: [], updatedAt: 1 },
  { id: "review-xlsx", title: "指标体系模型设计模板.xlsx", sourceType: "xlsx", status: "ready", byteSize: 379494, scopeId: "team", scopeKind: "team", scopeName: "产品研发团队", ownerId: "another-user", chunkCount: 1899, tags: [], updatedAt: 1 },
  { id: "review-pptx", title: "指标体系构建方法论.pptx", sourceType: "pptx", status: "ready", byteSize: 1572864, scopeId: "team", scopeKind: "team", scopeName: "产品研发团队", ownerId: "another-user", chunkCount: 2, tags: [], updatedAt: 1 },
];
if (["conversation", "conversation-layout", "workbench", "queue"].includes(query.get("surface") ?? "")) {
  const chat = useSessionStore.getState();
  chat.setSession("review-conversation");
  if (query.get("surface") === "queue") {
    useMessageQueueStore.setState({ queues: { "review-conversation": Array.from({ length: 20 }, (_, index) => ({
      id: `queue-review-${index}`, text: `待发送消息 ${index + 1}：核对中文文件与任务交付结果。`,
      attachments: index === 19 ? ["/review/中文附件.docx"] : [], status: "queued", createdAt: Date.now(),
    })) } });
  }
  for (const prompt of [
    "评审投影仪性能与内存泄漏，先看一下实现结构。",
    "这个服务端的启动方式是什么？",
    "手机扫码连接投影 IP 时，局域网发现怎么实现？",
    "检查蓝牙遥控器和手机端的控制逻辑。",
    "APK 打包后怎样验证 WebView 与原生服务的通信？",
    "梳理当前功能的主要风险和下一步计划。",
    "把重要结论整理成一份简短的评审记录。",
  ]) {
    chat.pushUser(prompt, [], "review-conversation");
    chat.pushAssistant("已检查相关实现，并记录了需要继续确认的细节。\n\n下一步可以针对这个问题继续深入。 ".repeat(3));
  }
  const messages = useSessionStore.getState().messages;
  chat.setSession("review-other");
  chat.setMessages(messages.map((message) => ({ ...message, id: `other-${message.id}` })));
  chat.setSession("review-conversation");
  useSessionsStore.setState({ independent: [
    { sessionId: "review-conversation", title: "请检查最新任务和运行状态", cwd: "", status: "working", pinned: true, updatedAt: new Date().toISOString() },
    { sessionId: "review-other", title: "等待授权的另外一个任务", cwd: "", status: "awaiting_permission", updatedAt: new Date().toISOString() },
    { sessionId: "review-done", title: "最近完成的历史任务", cwd: "", status: "completed", updatedAt: new Date().toISOString() },
    { sessionId: "review-news", title: "关注当天 AI 领域的重要动态", cwd: "", status: "completed", updatedAt: new Date(Date.now() - 53 * 60000).toISOString() },
    { sessionId: "review-reading", title: "趣味冷知识随机问答", cwd: "", status: "stopped", updatedAt: new Date(Date.now() - 12 * 3600000).toISOString() },
    { sessionId: "review-weather", title: "查询北京当日天气", cwd: "", status: "completed", updatedAt: new Date(Date.now() - 13 * 3600000).toISOString() },
    { sessionId: "review-choice", title: "随机挑选一个有趣的冷知识", cwd: "", status: "completed", updatedAt: new Date(Date.now() - 25 * 3600000).toISOString() },
    { sessionId: "review-model", title: "比较新模型的编程能力", cwd: "", status: "completed", updatedAt: new Date(Date.now() - 26 * 3600000).toISOString() },
    { sessionId: "review-plugin", title: "添加搜索插件并检查连接配置", cwd: "", status: "failed", updatedAt: new Date(Date.now() - 48 * 3600000).toISOString() },
    { sessionId: "review-writing", title: "部署 writing-dna skill 与验证", cwd: "", status: "completed", updatedAt: new Date(Date.now() - 72 * 3600000).toISOString() },
  ], tasksOpen: true, loading: false, currentSessionId: "review-conversation" });
  if (query.has("planReview")) {
    const plan = { entries: [{ content: "检查当前实现与交付结果", priority: "high" as const, status: "pending" as const }] };
    chat.setPlan(plan);
    chat.requestPlanApproval({ requestId: "review-plan", sessionId: "review-conversation", toolCallId: "review-plan-tool" });
    if (query.get("planReview") === "dirty") chat.setPlan(plan, { localRevision: true });
  }
  if (query.has("recovery")) {
    chat.startStreaming("review-conversation", "recovery-turn");
    chat.markComplete({ sessionId: "review-conversation", promptId: "recovery-turn", stopReason: "cancelled", cancellationCategory: "PermissionRejected" });
  }
}
if (query.get("surface") === "sidebar-scroll") {
  const variant = query.get("variant") ?? "both";
  const taskCount = ["projects", "short"].includes(variant) ? 2 : 54;
  const projectCount = ["tasks", "short"].includes(variant) ? 2 : 18;
  useGroupStore.setState({ groups: Array.from({length: Math.min(12, Number(query.get("groups")) || 0)}, (_, index) => ({
    id:`sidebar-group-${index}`, name:`群聊 ${index + 1}`, goal:"侧栏协作", cwd:"/review/EchoAgent", sourceSessionId:null, sourceTeamId:null,
    coordinatorId:"coordinator", members:[], messages:[], works:[], decisions:[], archived:false, pinned:false, paused:false,
    revision:1, updatedAt:12-index, maxCalls:12, tokenBudget:100000,
  })) });
  useSessionsStore.setState({
    independent: Array.from({ length: taskCount }, (_, index) => ({
      sessionId: `sidebar-task-${index}`,
      title: `任务 ${String(index + 1).padStart(2, "0")} · 检查目录与列表滚动`,
      cwd: "/review/EchoAgent",
      status: "completed" as const,
      updatedAt: new Date(Date.now() - index * 3600000).toISOString(),
    })),
    tasksOpen: true,
    projectsOpen: true,
    loading: false,
    currentSessionId: "sidebar-task-0",
  });
  useProjectsStore.setState({
    projects: Array.from({ length: projectCount }, (_, index) => ({
      id: `sidebar-project-${index}`,
      name: `项目 ${String(index + 1).padStart(2, "0")} · EchoAgent 工作区`,
      cwd: `/review/project-${index}`,
      createdAt: new Date().toISOString(),
      connectors: [], experts: [], skills: [], plans: [], tasks: [], assets: [], members: [], conversations: [],
    })),
  });
  if (query.get("sessionTimes") === "1") {
    const ago = (minutes: number) => new Date(Date.now() - minutes * 60_000).toISOString();
    useSessionsStore.setState({ independent: [
      { sessionId: "sidebar-task-0", title: "数字会话标识", cwd: "/review/EchoAgent", status: "completed", updatedAt: ago(5) },
      { sessionId: "sidebar-task-1", title: "核对很长的会话标题与运行状态，确认时间始终可见且操作按钮不会遮挡文字", cwd: "/review/EchoAgent", pinned: true, status: "awaiting_permission", updatedAt: ago(59) },
      { sessionId: "sidebar-task-2", title: "北京今天天气怎么样", cwd: "/review/EchoAgent", status: "working", updatedAt: ago(0) },
      { sessionId: "sidebar-task-3", title: "检查失败的资料整理任务", cwd: "/review/EchoAgent", status: "failed", updatedAt: ago(120) },
      { sessionId: "sidebar-task-4", title: "", cwd: "/review/EchoAgent", status: "paused" },
      { sessionId: "sidebar-task-5", title: "历史讨论与交付文件归档", cwd: "/review/EchoAgent", status: "completed", updatedAt: new Date(new Date().getFullYear() - 1, 11, 31).toISOString() },
      { sessionId: "sidebar-project-session", title: "项目中的方案讨论与细节确认", cwd: "/review/EchoAgent", status: "awaiting_answer", pinned: true, updatedAt: ago(59) },
    ] });
    const project = useProjectsStore.getState().projects[0];
    useProjectsStore.setState({ projects: [{ ...project, name: "产品设计", conversations: [{ sessionId: "sidebar-project-session", title: "项目中的方案讨论与细节确认", createdAt: ago(120) }] }] });
  }
}
if (query.get("surface") === "workbench") {
  const chat = useSessionStore.getState();
  const state = query.get("state") || "complete";
  useSessionsStore.setState({ tasksOpen: true });
  const longTitle = "梳理 EchoAgent Desktop 的任务交付、权限控制与工作区体验 · Review desktop workspace navigation and delivery";
  useSessionsStore.getState().upsert({ sessionId: "review-conversation", title: longTitle });
  if (state === "empty") chat.setMessages([]);
  if (["files", "collaboration"].includes(state)) chat.setMessages([...chat.messages, {
    id: "workbench-tool", role: "assistant", complete: true, parts: [{ kind: "tool_call", toolCall: {
      toolCallId: "review-write", title: "更新任务交付说明", kind: "edit", status: "completed",
      content: [{ type: "diff", diff: { path: "/review/EchoAgent/report.md", old: "旧内容", new: "# 交付记录\n已完成界面整改" } }],
    } }, { kind: "text", text: "已更新交付记录，并保留权限确认与执行控制。" }],
  }]);
  if (state === "plan") chat.setPlan({ entries: [
    { content: "审查真实业务入口", status: "completed", priority: "high" },
    { content: "验证工作台交互与交付", status: "in_progress", priority: "high" },
  ] });
  if (["running", "permission", "paused", "browser-paused", "computer-paused"].includes(state)) chat.startStreaming("review-conversation", "workbench-run");
  if (state === "permission") {
    const kind = query.get("permissionCase");
    useSessionsStore.getState().upsert({ sessionId: "review-conversation", status: "awaiting_permission" });
    usePermissionStore.getState().request({ requestId: "workbench-permission", sessionId: "review-conversation", toolCallId: "review-write",
      toolKind: kind === "history" ? "other" : kind === "command" ? "execute" : "edit",
      title: kind === "history" ? "conversation_history" : kind === "command" ? "是否允许运行以下命令？" : "是否允许写入工作目录中的交付记录？",
      rawInput: kind === "history" ? { sessionId: "review-conversation", limit: 10 }
        : kind === "command" ? { command: `python3 scripts/check-delivery.py --workspace /review/EchoAgent/${"很长的目录/".repeat(12)}`, cwd: "/review/EchoAgent" }
        : { path: "/review/EchoAgent/交付记录.md", content: "本轮交付记录" },
      options: [
        { optionId: "allow", kind: "allow", title: "允许一次" }, { optionId: "deny", kind: "deny", title: "拒绝" },
        query.get("permissionOptions") === "short"
          ? { optionId: "reject-always", kind: "deny_always", title: "Always reject" }
          : { optionId: "allow-path", kind: "allow_always", title: `Always allow: /review/EchoAgent/${query.get("permissionOptions") === "long" ? "需要逐段检查的目录/".repeat(12) : ""}交付记录.md` },
        { optionId: "enable-always-approve", kind: "allow", title: "Yes, and don't ask again for anything (always-approve mode)" },
      ],
    });
  }
  if (["paused", "browser-paused", "computer-paused"].includes(state)) { chat.requestControl("review-conversation", "pause"); chat.confirmControl("review-conversation", "pause"); }
  if (state === "browser-paused" || state === "computer-paused") chat.setAgentMode(state === "browser-paused" ? "browser_use" : "computer_use", "review-conversation");
  if (state === "failed") chat.setError("隔离验收：本轮任务失败，可以重试。");
  if (state === "collaboration") useSubagentStore.getState().applyEvent({ sessionId: "review-conversation", subagentId: "review-child", childSessionId: "review-child", phase: "spawned", description: "核对交付文档", status: "running" });
}
const workbenchCalls: unknown[] = [];
Object.assign(window, { workbenchCalls });
const callbacks = new Map();
const planReviewOutcomes: unknown[] = [];
let trustResolved = false;
let modelImportAttempts = 0;
Object.assign(window, {
  planReviewOutcomes,
  __TAURI_INTERNALS__: {
    transformCallback: (callback: unknown) => { const id = callbacks.size + 1; callbacks.set(id, callback); return id; },
    unregisterCallback: (id: number) => callbacks.delete(id),
    metadata: { currentWindow: { label: "main" }, currentWebview: { label: "main" } },
    invoke: async (command: string, args: any = {}) => {
      if (query.get("surface") === "workbench") workbenchCalls.push({command, args});
      if (command === "agent_resolve_permission") recoveryReview.permissionResolutions++;
      if (command === "permission_mode_set") recoveryReview.modeChanges++;
      if (extensionState === "error" && ["agents_list", "skills_list", "plugins_list", "mcp_list"].includes(command)) {
        extensionAttempts[command] = (extensionAttempts[command] ?? 0) + 1;
        if (extensionAttempts[command] === 1) throw new Error("隔离验收：列表暂不可用");
      }
      switch (command) {
        case "agent_session_workspace_status": return {cwd:args.cwd, loaded:true, status: query.get("workspace") === "missing" && !(window as any).workspaceReady ? "missing" : "ready", message: query.get("workspace") === "missing" ? "原目录已被移动" : null};
        case "filesystem_pick_directory": return query.get("pickedFolder") ?? null;
        case "export_text_file": return "/review/exported-session.md";
        case "agent_set_session_pinned": return args.pinned;
        case "agent_set_session_archived": return args.archived;
        case "session_fork": return "review-forked-session";
        case "rewind_points": return [{promptIndex: 0, promptPreview: "评审投影仪性能与内存泄漏"}];
        case "rewind_execute": return { targetPromptIndex: args.targetPromptIndex };

        case "agent_resolve_plan_approval": planReviewOutcomes.push(args); return true;
        case "team_snapshot": {
          if (query.get("teamState") === "error") throw new Error("隔离验收：团队快照读取失败");
          return executionReviewTeamSnapshot();
        }
        case "read_text_file": return executionReviewFinalMarkdown;
        case "path_stat": return { path: args.path, absolute: args.path, exists: true, kind: "file" };
        case "list_dir": return [];
        case "agent_list_pending_interactions": {
          const state = query.get("state") ?? "long";
          if (query.get("surface") !== "trust" || trustResolved) return { permissions: [], questions: [], planApprovals: [], folderTrustRequests: [] };
          if (state === "loading") return new Promise(() => {});
          if (state === "error") throw new Error(trustReviewError);
          return trustReviewPending(state as "short" | "long");
        }
        case "folder_trust_respond": trustResolved = true; return true;
        case "providers_list": if (query.has("settingsDesign")) return {
          providers: [
            { id: "review-org", label: "组织提供", providerKind: "custom", source: "organization", managed: true, baseUrl: "https://api.example.test/v1", credentialConfigured: true, syncedAt: "2026-10-18T10:00:00Z" },
            { id: "review-personal", label: "我的模型连接", providerKind: "custom", source: "personal", baseUrl: "https://api.example.test/v1", credentialConfigured: true },
          ],
          models: [
            { modelId: "review-org/model", remoteModelId: "组织模型", name: "组织模型", providerId: "review-org" },
            { modelId: "review-personal/model", remoteModelId: "个人模型", name: "个人模型", providerId: "review-personal" },
          ],
        }; return { providers: query.has("modelState") ? [{
          id: "review-model-provider", providerKind: "custom", label: "隔离模型连接",
          source: "personal", baseUrl: "https://review.invalid/v1", apiBackend: "chat_completions",
          authScheme: "bearer", credentialConfigured: true,
        }] : [], models: [] };
        case "providers_fetch_models_for_provider": {
          modelImportAttempts += 1;
          if (query.get("modelState") === "loading") return new Promise(() => {});
          if (query.get("modelState") === "load-error" && modelImportAttempts === 1) throw new Error("隔离验证：模型目录暂不可用");
          if (query.get("modelState") === "empty") return [];
          return Array.from({ length: 36 }, (_, index) => ({ id: `review-model-${index + 1}`, ownedBy: "隔离供应商 · 无真实模型请求" }));
        }
        case "providers_save_connection": throw new Error("隔离验证：保存失败，未写入原生配置");
        case "agents_defaults_get": return { defaultModel: "", defaultPermission: "ask", rememberToolApprovals: true };
        case "agent_auth_status": return { ready: false, providers: [], runtimeReady: false, synchronized: true, runtimeModels: [], reason: "尚未配置可用模型" };
        case "desktop_preferences_get": return { closeToTray: true, showNotificationTaskTitle: notificationTitlePreview };
        case "desktop_notification_preview_save": notificationTitlePreview = args.showTaskTitle; return { closeToTray: true, showNotificationTaskTitle: notificationTitlePreview };
        case "notification_list": return notificationEntries;
        case "notification_mark_read": notificationEntries = notificationEntries.map(entry => entry.id === args.id ? { ...entry, read: true } : entry); return;
        case "notification_mark_all_read": notificationEntries = notificationEntries.map(entry => ({ ...entry, read: true })); return;
        case "notification_clear": notificationEntries = []; return;
        case "subagents_config_get": return { maxDepth: 2 };
        case "automations_snapshot": return { automations: query.get("surface") === "automation-flow" && !automationFlowRemoved ? [automationFlowTask] : [], records: [] };
        case "automations_save": {
          if (query.get("surface") !== "automation-flow") throw new Error("This fixture does not save automations");
          if (args.automation.id && automationFlowRemoved) throw new Error("原自动化任务已被移除");
          automationFlowTask = { ...args.automation, id: args.automation.id || "review-new-automation", createdAt: "2026-10-17T00:00:00Z" };
          automationFlowRemoved = false;
          return automationFlowTask;
        }
        case "memory_records_get": return { enabled: memory.enabled, pendingCount: 1,
          extraction: { attemptedAt: 1792320000, succeededAt: 1792320000, outcome: "saved", detail: "", recordsWritten: 2 },
          retrieval: { at: 1792320300, mode: "Hybrid", count: 1, sources: ["全局偏好 · 回答方式"], durationMs: 80 },
          records: [
            { id: "review-user", scope: "global", kind: "user", key: "回答方式", content: "先给结论，再说明依据。报告使用中文，提供清晰的下一步。", status: "active", pinned: true, origin: "manual", confidence: 1, sources: [], evidence: "用户明确要求", createdAt: 1792320000, updatedAt: 1792320000, validUntil: null, revision: 1 },
            { id: "review-task", scope: "workspace", kind: "task", key: "客户汇报初稿", content: "本周完成客户汇报初稿，重点说明实施进展与待协调事项。", status: "active", pinned: false, origin: "automatic", confidence: .85, sources: ["review-source"], evidence: "这周先把客户汇报初稿整理出来", createdAt: 1792320000, updatedAt: 1792320000, validUntil: 1893455999, revision: 1 },
          ] };
        case "memory_record_save": return { ...args.input, id: args.input.id || "review-new", status: "active", sources: [], origin: "manual", createdAt: 1792320000, updatedAt: 1792320000, revision: 1 };
        case "memory_record_retire": return null;
        case "memory_config_get": return memory;
        case "memory_list": return [
          { scope: "session", path: "2026-09-06-interval-01a074eb.md", content: "<think>The user is asking me to write a memory summary. Looking at this session, I was tasked with researching today's AI news focused on AI coding and embodied intelligence.", size: 180, revision: "r1", modifiedAt: "2026-09-06T10:00:00Z", readOnly: true },
          { scope: "session", path: "2026-09-06-interval-01a074e6.md", content: "<think>The user is asking me to write a memory summary as described in the system prompt. Let me re-read the system prompt instructions.", size: 146, revision: "r2", modifiedAt: "2026-09-06T11:00:00Z", readOnly: true },
        ];
        case "agent_resolve_permission":
          if (query.get("permissionFailure") === "1") throw new Error("连接暂不可用，请重试");
          return true;
        case "permission_mode_set": return { sessionId: args.sessionId, permissionMode: args.mode, configuredPermissionMode: args.mode,
          autoModeAvailable: true, alwaysApproveAvailable: true, locked: false, runtimeSyncState: "synced", runtimeAppliedMode: args.mode,
          agentRunning: true, runtimeSynced: true, remainingPending: 1 };
        case "permission_mode_get": return { sessionId: args.sessionId, permissionMode: "ask", configuredPermissionMode: "ask", autoModeAvailable: true, alwaysApproveAvailable: true, locked: false, runtimeSyncState: args.sessionId ? "synced" : "offline", runtimeAppliedMode: args.sessionId ? "ask" : undefined };
        case "memory_config_save": memory = { ...memory, ...args.memory }; return memory;
        case "permission_list": return rules;
        case "permission_save": rules = args.rules; return;
        case "automation_capabilities": return { browser: { available: true, browserName: "Chrome" }, computer: { available: true, platform: "macos", screenCapture: true, inputControl: true } };
        case "automation_status": return {
          sessionId: args.sessionId,
          mode: query.get("state") === "browser-paused" ? "browser_use" : query.get("state") === "computer-paused" ? "computer_use" : "default",
          paused: ["browser-paused", "computer-paused"].includes(query.get("state") || ""),
          task: { phase: "paused", reason: null, scope: "隔离界面验收", authorized: true, generation: 1 },
          allowPrivateNetwork: false, browserRunning: false, browserHasData: true,
          browser: { available: true, browserName: "Chrome" },
          computer: { available: true, platform: "macos", screenCapture: true, inputControl: true },
        };
        case "automation_pending_approvals": return [];
        case "storage_providers_list": return providers;
        case "storage_provider_upsert": providers = [args.config]; return;
        case "storage_list": return args.id === "review-dav" ? [{ path: "/readme.md", name: "readme.md", isDir: false, size: 256 }] : [];
        case "storage_read_text": return "隔离 UI 验证内容，不访问真实云存储。";
        case "storage_provider_test": return;
        case "notify_desktop_status":
          if (query.get("notificationState") === "desktop-error" && !notificationDesktopReadFailed) { notificationDesktopReadFailed = true; throw new Error("隔离验证：桌面提醒状态暂时不可读"); }
          return { enabled: channels.filter(c => c.kind === "desktop").length === 0 || channels.some(c => c.kind === "desktop" && c.enabled), source: channels.some(c => c.kind === "desktop") ? "configured" : "default", permission: "unknown" };
        case "notify_desktop_set_enabled": channels = [...channels.filter(c => c.kind !== "desktop"), { id: "desktop-review", label: "桌面提醒", kind: "desktop", enabled: args.enabled }]; return;
        case "notify_desktop_test": return { id: "desktop", ok: true };
        case "notify_channels_list":
          if (query.get("notificationState") === "channel-error" && !notificationChannelReadFailed) { notificationChannelReadFailed = true; throw new Error("隔离验证：外部渠道暂时不可读"); }
          return channels.map(channel => ({ ...channel, endpoint: channel.endpoint ? `${new URL(channel.endpoint).origin}/…` : undefined }));
        case "notify_channel_upsert": {
          const previous = channels.find(channel => channel.id === args.channel.id);
          channels = [...channels.filter(channel => channel.id !== args.channel.id), { ...args.channel, endpoint: args.channel.endpoint ?? previous?.endpoint }]; return;
        }
        case "notify_channel_set_enabled": channels = channels.map(channel => channel.id === args.id ? { ...channel, enabled: args.enabled } : channel); return;
        case "notify_channel_remove": channels = channels.filter(channel => channel.id !== args.id); return;
        case "notify_channel_test": return { ok: true };
        case "weixin_status": return query.get("surface") === "weixin-offline"
          ? { connected: true, botId: "review-bot", activeSession: "review-session-12345678", allowedWorkspaces: ["/review/EchoAgent"], sharedSessions: [{ sessionId: "review-session-12345678", title: "准备发布说明", cwd: "/review/EchoAgent" }], pendingReplies: 3, online: false, lastError: "微信连接超时" }
          : query.get("surface") === "weixin-connected"
          ? { connected: true, botId: "review-bot", activeSession: "review-session-12345678", allowedWorkspaces: ["/review/EchoAgent"], sharedSessions: [{ sessionId: "review-session-12345678", title: "准备发布说明", cwd: "/review/EchoAgent" }], pendingReplies: 0, online: true }
          : { connected: false, allowedWorkspaces: [], sharedSessions: [], pendingReplies: 0, online: false };
        case "weixin_revoke_session": return { connected: true, botId: "review-bot", allowedWorkspaces: ["/review/EchoAgent"], sharedSessions: [], pendingReplies: 0, online: true };
        case "agent_list_workspaces": return [{ cwd: "/review/EchoAgent", sessionCount: 3 }, { cwd: "/review/设计系统", sessionCount: 2 }];
        case "weixin_qr_start": return { qrUrl: "https://weixin.qq.com/x/review-qr" };
        case "weixin_qr_poll": return { status: "expired", connected: false };
        case "agents_list": return agents;
        case "skills_list": return extensionSkills;
        case "skills_toggle": extensionSkills = extensionSkills.map(skill => skill.name === args.name ? { ...skill, enabled: args.enabled } : skill); return;
        case "mcp_list": return extensionConnectors;
        case "mcp_toggle": extensionConnectors = extensionConnectors.map(server => server.name === args.name ? { ...server, enabled: args.enabled } : server); return { warnings: [] };
        case "mcp_delete": extensionConnectors = extensionConnectors.filter(server => server.name !== args.name); return { warnings: [] };
        case "mcp_toggle_tool": extensionConnectors = extensionConnectors.map(server => server.name === args.serverName ? { ...server, tools: server.tools.map(tool => tool.name === args.toolName ? { ...tool, enabled: args.enabled } : tool) } : server); return;
        case "mcp_config_path": return "/review/mcp.json";
        case "mcp_config_read": return { filePath: "/review/mcp.json", content: extensionConfig };
        case "mcp_config_save": {
          extensionConfig = args.content;
          return { appliedLive: Boolean(args.sessionId), serverCount: Object.keys(JSON.parse(extensionConfig).mcpServers ?? {}).length, warnings: [] };
        }
        case "agents_template": return JSON.stringify(args);
        case "agents_save": {
          const entry = { ...JSON.parse(args.raw), name: args.name, path: `/review/${args.name}.md`, scope: "user", raw: args.raw };
          agents = [...agents, entry]; return entry;
        }
        case "plugins_list": return { plugins: extensionPlugins };
        case "plugins_action": {
          if (["uninstall", "remove"].includes(args.action.type)) {
            if (query.get("pluginRemoveFailure") === "1" && !extensionAttempts.removePlugin) { extensionAttempts.removePlugin = 1; return {status:"internal_error",message:"隔离验收：插件目录暂时不可写，安装记录保留"}; }
            extensionPlugins = extensionPlugins.filter(plugin => args.action.type === "uninstall" ? plugin.id !== args.action.pluginId : plugin.root !== args.action.path);
          } else if (["enable", "disable"].includes(args.action.type)) extensionPlugins = extensionPlugins.map(plugin => plugin.id === args.action.pluginId ? {...plugin,enabled:args.action.type==="enable"}:plugin);
          return {status:"success"};
        }
        case "tasks_list":
          if (query.get("surface") === "workbench" && !["running", "permission", "collaboration", "paused", "browser-paused", "computer-paused"].includes(query.get("state") || "complete")) return [];
          return [{ id: "review-task", source: "task", description: "执行代码验证和生成报告", status: "running", sessionId: args.sessionId }];
        case "org_session": return { loggedIn: query.get("surface") !== "organization-login", organizationMemoryEnabled: true, serverUrl: "https://10.132.19.82:8787", user: orgUser, bootstrap: { apiVersion: 1, user: orgUser, scopes: orgScopes, policy: { allowPersonalCloud: true, allowSkillSubmission: true }, serverTime: 1 } };
        case "org_list_scopes": return orgScopes;
        case "org_list_documents": {
          const items = orgDocuments.filter((document) => !args.scopeId || document.scopeId === args.scopeId);
          return { items, total: items.length, page: 1, size: 20 };
        }
        case "org_list_memories": case "org_list_skills": case "org_skill_submissions_mine": case "org_memory_promotions_mine": return [];
        case "org_document_submissions_mine_page": return { items: [], total: 0, page: 1, size: 20 };
        case "org_fetch_document": return { docId: args.docId, text: "Sheet: 模型设计\n列(2): 指标 | 口径\n第1行: 指标=活跃用户, 口径=当日登录人数", chunks: [{ seq: 0, text: "Sheet: 模型设计\n列(2): 指标 | 口径\n第1行: 指标=活跃用户, 口径=当日登录人数" }] };
        case "plugin:event|listen": return callbacks.size;
        case "plugin:event|unlisten": return;
        case "meeting_list": return [];
        case "meeting_capture_status": return null;
        case "meeting_capture_support": return { systemAudio: true, detail: "首次使用需允许系统音频录制" };
        case "meeting_check_connection": return;
        default: return [];
      }
    },
  },
  __TAURI_EVENT_PLUGIN_INTERNALS__: { unregisterListener: () => {} },
});

function ConversationLayoutFixture() {
  const sessionId = useSessionStore((state) => state.sessionId);
  const entry = useSessionsStore(state => state.independent.find(item => item.sessionId === sessionId));
  const layout = useAppSidebarLayout();
  return <div className={"app" + (query.get("platform") === "mac" ? " app--macos" : "")}>
    <SidebarLayoutContext.Provider value={layout.context}>
    <div ref={layout.bodyRef} style={layout.style} className={"app__body" + (layout.collapsed ? " app__body--collapsed" : "")}>
      <Sidebar activeNav="新建任务" onNewSession={() => {}} onNavigate={() => {}} onOpenSettings={() => {}} onOpenSearch={() => {}}
        onSelect={(id) => { useSessionStore.getState().setSession(id); useSessionsStore.setState({ currentSessionId: id }); }} />
      <SidebarResizeHandle layout={layout} />
      <main className="app__main">
        <ChatView onSend={(text, attachments) => {
          (window as any).workbenchCalls.push({command:"review_send",args:{sessionId,text,attachments}});
          if (sessionId) useSessionStore.getState().resumeSession(sessionId);
          return true;
        }} onSendNow={async (text, attachments) => {
          (window as any).workbenchCalls.push({command:"review_send_now",args:{sessionId,text,attachments}});
          return query.get("rejectSend") !== "1";
        }} onCancel={action => {
          if (!sessionId) return false;
          useSessionStore.getState().requestControl(sessionId, action ?? "pause");
          useSessionStore.getState().confirmControl(sessionId, action ?? "pause");
          return true;
        }}
          navigation={{ sidebarCollapsed: layout.collapsed, onToggleSidebar: () => layout.setCollapsed(!layout.collapsed), onNewTask: () => { (window as any).workbenchNewTask = true; } }}
          onNewTask={() => { (window as any).workbenchNewTask = true; }}
          onReloadWorkspace={async () => { (window as any).workspaceReloaded = true; }}
          modelId={query.has("composerModel") ? "review/MiniMax-M3.1-Flash" : undefined}
          models={query.has("composerModel") ? [{id:"review/MiniMax-M3.1-Flash",label:"MiniMax-M3.1-Flash"}] : undefined}
          onModelChange={query.has("composerModel") ? () => {} : undefined}
          cwd={query.get("surface") === "workbench" ? "/Users/review/项目/EchoAgent/一个很长的任务工作目录/desktop-client" : undefined}
          title={entry?.title ?? "聊天布局回归"}
          onRename={async title => { if (sessionId) useSessionsStore.getState().upsert({ sessionId, title }); }}
        />
      </main>
    </div></SidebarLayoutContext.Provider>
    <GlobalTooltip />
  </div>;
}

function HomeReviewFixture() {
  const layout = useAppSidebarLayout();
  const [cwd, setCwd] = useState(query.has("automaticWorkspace") ? "" : "/review/EchoAgent");
  return <div className="app"><SidebarLayoutContext.Provider value={layout.context}>
    <div ref={layout.bodyRef} style={layout.style} className={"app__body" + (layout.collapsed ? " app__body--collapsed" : "") + (layout.resizing ? " app__body--sidebar-resizing" : "")}>
      <Sidebar activeNav="新建任务" onNewSession={() => {}} onSelect={() => {}} onNavigate={() => {}} onOpenSettings={() => {}} onOpenSearch={() => {}} onToggleCollapse={() => layout.setCollapsed(true)} />
      <SidebarResizeHandle layout={layout} />
      <main className="app__main">
        {layout.collapsed && <div className="main-page-controls"><button className="main-topbar__btn" aria-label="展开侧边栏" onClick={() => layout.setCollapsed(false)}>展开侧边栏</button></div>}
        <HomePage onSend={() => {}} streaming={false} apiReady={query.get("state") !== "unconfigured"} creatingSession={query.get("state") === "loading"} sendError={query.get("state") === "error" ? "会话创建失败，请重试" : null} onOpenSettings={() => {}} onPlaceholder={() => {}} models={[{ id: "review/model", label: "评审模型", providerId: "review", providerKind: "custom" }]} modelId="review/model" onModelChange={() => {}} cwd={cwd} automaticWorkspace={!cwd} defaultWorkspaceCwd="/review/EchoAgent" workspaces={[{cwd:"/review/EchoAgent",sessionCount:1},{cwd:"/review/Other",sessionCount:1}]} onSelectWorkspace={setCwd} />
      </main>
    </div>
  </SidebarLayoutContext.Provider></div>;
}

function CodingSidebarReview({ children }: { children: React.ReactNode }) {
  const layout = useAppSidebarLayout();
  return <div className="app app--macos app--coding"><SidebarLayoutContext.Provider value={layout.context}>
    <div ref={layout.bodyRef} style={layout.style} className={"app__body" + (layout.collapsed ? " app__body--collapsed" : "")}>
      <Sidebar activeNav="代码开发" onNewSession={() => {}} onSelect={() => {}} onNavigate={() => {}} onOpenSettings={() => {}} onOpenSearch={() => {}} />
      <SidebarResizeHandle layout={layout} />
      <main className="app__main">{children}</main>
    </div>
  </SidebarLayoutContext.Provider></div>;
}

function ManagementReviewFixture({ surface }: { surface: "projects" | "organization-shell" | "automation-templates" }) {
  const layout = useAppSidebarLayout();
  return <div className="app"><SidebarLayoutContext.Provider value={layout.context}>
    <div ref={layout.bodyRef} style={layout.style} className={"app__body" + (layout.collapsed ? " app__body--collapsed" : "") + (layout.resizing ? " app__body--sidebar-resizing" : "")}>
      <Sidebar activeNav={surface === "projects" ? "项目" : surface === "organization-shell" ? "组织" : "自动化"} onNewSession={() => {}} onSelect={() => {}} onNavigate={() => {}} onOpenSettings={() => {}} onOpenSearch={() => {}} onToggleCollapse={() => layout.setCollapsed(true)} />
      <SidebarResizeHandle layout={layout} />
      <main className="app__main">
        {layout.collapsed && <div className="main-page-controls"><button className="main-topbar__btn" aria-label="展开侧边栏" onClick={() => layout.setCollapsed(false)}>展开侧边栏</button></div>}
        {surface === "projects" ? <ProjectsPanel /> : surface === "organization-shell" ? <OrganizationMemoryPanel onStartConversation={() => {}} /> : <AutomationPanel cwd="/review/EchoAgent" />}
      </main>
    </div>
  </SidebarLayoutContext.Provider></div>;
}

function Fixture() {
  const surface = query.get("surface") ?? "memory";
  const aliases: Record<string, string> = { "技能管理": "技能", "连接器管理": "连接器", "插件管理": "插件" };
  const [label, setLabel] = useState(aliases[query.get("label") ?? ""] ?? query.get("label") ?? "专家·技能·连接器");
  const [expertPageOpen, setExpertPageOpen] = useState(false);
  const [recoverySent, setRecoverySent] = useState("");
  const [cwd, setCwd] = useState("/review/EchoAgent");
  const [automationNotificationSequence, setAutomationNotificationSequence] = useState(0);
  const [automationDraft, setAutomationDraft] = useState(() => buildDraft(
    createAutomationTemplates()[0], "/Users/review/Documents/EchoAgent",
  ));
  if (surface === "browser-toolbar") return <div className="tool-side-panel" style={{width:"min(100%, 520px)",height:"100%"}}><BrowserPreview url="" /></div>;
  if (surface === "about") return <AboutDialog open onClose={() => {}} />;
  if (surface === "feedback") return <FeedbackDialog open sessionId="review" messageId="review" rating="up" onClose={() => {}} />;
  if (surface === "update") return <UpdateDialog open onClose={() => {}} />;
  if (surface === "controls") return <DesignSystemReview />;
  if (surface === "search") return <SearchOverlay open onClose={() => {}} onSelect={() => {}} />;
  if (surface === "automation-flow") return <div style={{ display: "flex", flexDirection: "column", height: "100%", minHeight: 0 }}>
    <div style={{ padding: 8 }}><button type="button" onClick={() => setAutomationNotificationSequence(sequence => sequence + 1)}>模拟任务通知</button></div>
    <AutomationPanel cwd="/review/EchoAgent" notificationAutomationId={automationNotificationSequence ? "review-notified-task" : undefined} notificationAutomationSequence={automationNotificationSequence} />
  </div>;
  if (surface === "collapsed-project" || surface === "collapsed-capabilities") {
    const project = {
      id: "review-project", name: "SKILL安装", createdAt: new Date().toISOString(),
      connectors: [], experts: [], skills: [], plans: [], tasks: [], assets: [], members: [], conversations: [],
    };
    return <div className={`app${query.get("platform") === "mac" ? " app--macos" : ""}`}>
      <div className="app__body app__body--collapsed">
        <main className="app__main">
          <header className="main-page-controls" data-tauri-drag-region>
            <button className="main-topbar__btn" aria-label="展开侧边栏">☰</button>
            <button className="main-topbar__btn" aria-label="新建任务">＋</button>
          </header>
          {surface === "collapsed-project"
            ? <ProjectDetailView project={project} onBack={() => {}} picker={{ options: { connectors: [], experts: [], skills: [] }, loading: false, error: null }} />
            : <PlaceholderPage label="专家·技能·连接器" />}
        </main>
      </div>
    </div>;
  }
  if (surface === "automation-edit") return <div className="automation-panel echo-agent-automation"><AutomationEditPage
    mode="create" draft={automationDraft} setDraft={setAutomationDraft}
    saving={false} workspaces={[{ cwd: "/Users/review/Documents/EchoAgent", sessionCount: 9 }]}
    models={[]} skills={[]} experts={[]} connectors={[]} records={[]}
    onSave={() => {}} onClose={() => {}} onArchiveRecord={() => {}}
    onRetryRecord={() => {}} onDeleteRecord={() => {}}
  /></div>;
  if (surface === "workspace-picker") return <div style={{ height: "100%", padding: "130px 120px", background: "var(--echo-bg-secondary)" }}>
    <WorkspacePicker
      cwd={String.raw`\\?\A:\E\Taurus\Documents\EchoAgent`}
      workspaces={[
        { cwd: String.raw`\\?\A:\E\Taurus\Documents\EchoAgent`, sessionCount: 22 },
        { cwd: String.raw`\\?\A:\E\Taurus\EchoAgentWorkspace\日常`, sessionCount: 34 },
        { cwd: String.raw`\\?\A:\E\Taurus\EchoAgent\snake`, sessionCount: 5 },
      ]}
      onSelectWorkspace={() => {}}
      menuPlacement="bottom"
    />
    <GlobalTooltip />
  </div>;
  if (surface === "sidebar-scroll") return <div style={{ display: "flex", height: "100%" }}>
    <Sidebar
      activeNav="新建任务" onNewSession={() => {}} onSelect={() => {}} onNavigate={() => {}}
      onOpenSettings={() => {}} onOpenSearch={() => {}} onToggleCollapse={() => {}}
      onPlaceholder={() => {}}
    />
    <main style={{ flex: 1, background: "var(--echo-bg-secondary)" }} />
    <GlobalTooltip />
  </div>;
  if (surface === "conversation-layout" || surface === "workbench") return <ConversationLayoutFixture />;
  if (surface.startsWith("modal-")) return <ModalReview variant={surface === "modal-short" ? "short" : surface === "modal-error" ? "error" : "long"} />;
  if (surface === "results") return <ResultReviewFixture />;
  if (surface === "details") return <DetailReview />;
  if (surface === "trust") return <TrustReview variant={(query.get("state") ?? "long") as "short" | "long" | "loading" | "error"} />;
  if (surface === "execution" || surface === "runtime") return <ExecutionReviewFixture />;
  if (surface === "home") return <HomeReviewFixture />;
  if (surface === "toast") return <ToastReview />;
  if (surface === "content-menus") return <ContentMenusReview />;
  if (surface === "page") return <PlaceholderPage label={label} onNavigate={setLabel} cwd="/review/EchoAgent" />;
  if (surface === "capabilities") {
    const content = <PlaceholderPage label={label} onNavigate={setLabel} sessionId={extensionCount ? "review-extension-session" : undefined} />;
    if (query.get("extensionShell") !== "1") return content;
    return <div className="app"><div className="app__body">
      <Sidebar activeNav={label} onNewSession={() => {}} onNavigate={setLabel} onOpenSettings={() => {}} onOpenSearch={() => {}} onSelect={() => {}} />
      <main className="app__main">{content}</main>
    </div></div>;
  }
  if (surface === "expert-entry") return <div className="app"><div className="app__body">
    <Sidebar activeNav={expertPageOpen ? label : "新建任务"} onNewSession={() => {}} onSelect={() => {}}
      onNavigate={next => { setLabel(next); setExpertPageOpen(true); }} onOpenSettings={() => {}} onOpenSearch={() => {}} />
    <main className="app__main">{expertPageOpen ? (
      <div style={{ height: "100%" }}>
        <button type="button" onClick={() => setExpertPageOpen(false)}>返回对话</button>
        <PlaceholderPage label={label} onNavigate={setLabel} />
      </div>
    ) : <div style={{ padding: 48, fontSize: 32, fontWeight: 700 }}>今天想完成什么？</div>}</main>
  </div></div>;
  if (surface === "conversation" || surface === "queue") return <div className="app" style={{ display: "flex", flexDirection: "row", height: "100%" }}>
    <aside style={{ width: "min(250px, 22vw)", flex: "none", borderRight: "1px solid var(--echo-border-default)", background: "var(--echo-bg-secondary)", padding: "24px 16px", boxSizing: "border-box" }}>EchoAgent</aside>
    <main className="app__main"><ChatView onSend={text => {
      setRecoverySent(text);
      if (query.has("recovery")) useSessionStore.getState().startStreaming("review-conversation", "recovery-followup");
      return true;
    }} onCancel={() => {}} title="评审投影仪性能与内存泄漏" /></main>
    {recoverySent && query.has("recovery") && <p role="note" style={{position:"absolute",top:0,left:0,maxWidth:"100%",background:"var(--echo-bg-secondary)",fontSize:12,padding:8}}>{recoverySent}</p>}
  </div>;
  if (surface === "projects" || surface === "organization-shell" || surface === "automation-templates") return <ManagementReviewFixture surface={surface} />;
  if (surface === "organization" || surface === "organization-login") return <div style={{ height: "100%", overflow: "auto" }}><OrganizationMemoryPanel onStartConversation={() => {}} /></div>;
  if (surface === "meeting") return <MeetingMinutesPanel modelId="review/MiniMax-M3" models={[{ id: "review/MiniMax-M3", label: "MiniMax M3", providerId: "review", providerKind: "custom", source: "personal" }]} />;
  if (surface === "permission-picker") return <div style={{ position: "relative", height: "100%", background: "var(--echo-bg-secondary)" }}>
    <div style={{ position: "absolute", left: "46%", top: "63%" }}><PermissionPicker /></div>
  </div>;
  if (surface === "coding") {
    const coding = <div className="app--macos" style={{ height: "100%" }}><div className="coding-workbench coding-workbench--theia">
    <header className="coding-workbench__topbar"><WorkbenchIdentity onExit={() => {}}>
      <ProjectSwitcher activeCwd={cwd} projects={[{ cwd: "/review/EchoAgent" }, { cwd: "/review/一个名称很长但仍然可以完整查看路径的项目" }]} onSelect={setCwd} onRemove={() => {}} onOpenFolder={() => {}} />
    </WorkbenchIdentity><div /></header>
    <main className="echo-theia-workspace" />
    <div className="echo-theia-agent__splitter" role="separator" tabIndex={0} aria-label="调整 Agent 面板宽度" aria-orientation="vertical" />
    <aside className="echo-theia-agent"><div className="echo-theia-agent__heading">
      <TaskSwitcher
        tasks={[
          { id: "task-1", name: "修复代码开发页面菜单遮挡问题", phase: "delivered", updatedAt: "2026-09-29T10:00:00Z" },
          { id: "task-2", name: "完成任务报告并检查回归测试", phase: "stopped", updatedAt: "2026-09-29T09:00:00Z" },
          { id: "task-3", name: "继续进行中任务的开发", phase: "implementing", updatedAt: "2026-09-29T08:00:00Z" },
        ]}
        activeId={null}
        onSelect={() => {}}
        onNew={() => {}}
        onRename={() => {}}
        onDelete={() => {}}
      />
    </div><TheiaAgentComposer workspaceRoot={cwd} task={null} sessionId={null}
      models={[{ id:"review/model", label:"评审模型", providerId:"review", providerKind:"custom" }]}
      modelId="review/model" contextPaths={[]} apiReady starting={false} sending={false} streaming={false}
      suggestedPrompt="检查代码工作台的主操作按钮"
      onModelChange={() => {}} onStart={() => {}} onSend={() => {}} onRemoveContext={() => {}} onDraftContextPaths={() => {}} />
    </aside>
    </div></div>;
    return query.get("codingShell") === "1" ? <CodingSidebarReview>{coding}</CodingSidebarReview> : coding;
  }
  return <SettingsPanel open initialSection={(surface === "weixin-connected" || surface === "weixin-offline" || surface === "weixin-unconnected" ? "weixin-channel" : surface) as SettingsSectionId} cwd={surface === "personal-memory" ? "/Users/fuyuxiang/Documents/EchoAgent" : undefined} onClose={() => {}} />;
}
createRoot(document.getElementById("root")!).render(<ThemeProvider><Fixture /></ThemeProvider>);
