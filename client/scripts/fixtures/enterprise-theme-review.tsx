// Isolated acceptance data around production components. No native installs,
// real credentials, file writes, model requests or publishing are performed.
import React, { lazy, Suspense, useEffect, useMemo, useRef, useState } from "react";
import { createRoot } from "react-dom/client";
import "../../src/styles/index.css";
import { ThemeProvider, initializeTheme, useTheme } from "../../src/components/ThemeProvider";
import { ImportSkillModal } from "../../src/components/experts-panel/skills/ImportSkillModal";
import { McpConfigEditor } from "../../src/components/experts-panel/connectors/McpConfigEditor";
import { RefPickerDialog } from "../../src/components/project-picker";
import { KnowledgePicker } from "../../src/components/KnowledgePicker";
import { AgentSettingsPanel } from "../../src/components/SettingsSections";
import { ProjectDetailView } from "../../src/components/ProjectDetailView";
import type { TheiaIdeFrameHandle } from "../../src/features/coding/TheiaIdeFrame";
import { useKnowledgeStore } from "../../src/stores/knowledge-store";
import { useOrgSessionStore } from "../../src/stores/org-session-store";
import { useProjectsStore, type ProjectMeta } from "../../src/stores/projects-store";
import type { SkillPackageInspection, SkillCapabilityState } from "../../src/lib/types";

const query = new URLSearchParams(location.search);
const TheiaTaskReview = lazy(() => import("../../src/features/coding/TheiaTaskReview").then(module => ({ default: module.TheiaTaskReview })));
const TheiaIdeFrame = lazy(() => import("../../src/features/coding/TheiaIdeFrame").then(module => ({ default: module.TheiaIdeFrame })));
const surface = query.get("surface") ?? "skill-import";
if (!query.has("restore")) localStorage.setItem("echoagent.theme", query.get("theme") ?? "light");
initializeTheme();
const calls: { command: string; args: Record<string, unknown> }[] = [];
const callbacks = new Map<number, (value: unknown) => void>();
let callbackId = 0;
let config = '{\n  "mcpServers": { "reports": { "command": "review-tool" } }\n}';
const references = [{ id: "reports", name: "工作报告与资料检查" }, { id: "research", name: "跨团队知识整理与长名称引用示例" }];
const project: ProjectMeta = {
  id: "theme-project", name: "企业 Agent 工作空间", cwd: "/isolated-review/workspace",
  createdAt: "2026-11-04T00:00:00Z", instructions: "保留信息来源，先核对关键数据，再整理可交付的报告。",
  defaultModelId: "review/available", connectors: [references[0]], experts: [references[0]], skills: [references[0]],
  plans: [], tasks: [
    { id: "todo", title: "核对本周工作资料与来源", scope: "personal", source: "手动创建", status: "pending" },
    { id: "working", title: "整理跨团队项目进展与待确认事项", scope: "shared", source: "团队协作", status: "in_progress" },
    { id: "done", title: "检查已完成任务的交付文档", scope: "personal", source: "手动创建", status: "completed" },
  ], assets: [{ id: "guide", name: "项目资料与交付规范.md", kind: "file", ext: "md", sizeLabel: "24 KB", path: "/isolated-review/guide.md" }],
  members: ["业务分析师"], conversations: [{ sessionId: "project-review", title: "核对项目交付与下一步安排", createdAt: "2026-11-04T00:00:00Z" }],
};
if (query.get("state") === "unavailable") project.defaultModelId = "review/removed";
useProjectsStore.setState({ projects: [project] });
useKnowledgeStore.setState({ sourceCount: 2, defaultSources: ["personal"] });
useOrgSessionStore.setState({ hydrated: true, session: null });

function inspection(state: SkillCapabilityState, index: number): SkillPackageInspection {
  return {
    sourcePath: `/isolated-review/package-${index}.zip`, name: index ? "团队资料连接" : "报告生成与来源检查",
    description: "检查本地资料、确认依赖与授权状态，再生成可复核的工作报告。", version: "1.0.0",
    fileCount: 4, totalBytes: 18000, riskLevel: "medium", alreadyInstalled: false, sourceHash: `review-hash-${index}`,
    findings: [{ code: "review-command", level: "medium", message: "包含本地执行入口，请先检查脚本及依赖。", path: "scripts/report.py" }],
    warnings: ["此页仅检查技能包，不会安装或执行内容。"],
    capability: { declared: true, state, ready: false, capabilities: ["生成文档", "读取资料"], checks: [
      { kind: state === "missing_dependencies" ? "command" : "connector", key: index ? "team-documents" : "python", status: state === "missing_dependencies" ? "missing" : "configuration_required", message: index ? "需要连接账号并确认授权范围" : "尚未找到要求的本地依赖" },
    ] },
  };
}

Object.assign(window, {
  enterpriseThemeReview: { calls },
  __TAURI_EVENT_PLUGIN_INTERNALS__: { unregisterListener: () => {} },
  __TAURI_INTERNALS__: {
    metadata: { currentWindow: { label: "theme-review" }, currentWebview: { label: "theme-review" } },
    transformCallback: (callback: (value: unknown) => void) => { callbacks.set(++callbackId, callback); return callbackId; },
    unregisterCallback: (id: number) => callbacks.delete(id),
    invoke: async (command: string, args: Record<string, unknown> = {}) => {
      calls.push({ command, args });
      switch (command) {
        case "plugin:event|listen": return args.handler;
        case "plugin:event|unlisten": return;
        case "filesystem_pick_files": return ["/isolated-review/reports.zip"];
        case "filesystem_pick_directory": return "/isolated-review/workspace";
        case "skills_inspect_packages": return ["missing_dependencies", "configuration_required"].map((state, index) => ({ label: index ? "团队资料连接" : "报告生成", inspection: inspection(state as SkillCapabilityState, index) }));
        case "skills_install_package": throw new Error("验收禁止安装真实技能");
        case "mcp_config_path": return "/isolated-review/mcp.json";
        case "mcp_config_read": return { filePath: "/isolated-review/mcp.json", content: config };
        case "mcp_config_save": config = String(args.content); return { warnings: [] };
        case "subagents_config_get": return { maxDepth: 2 };
        case "subagents_config_save": if (query.get("state") === "error") throw new Error("隔离验证：配置暂不可写"); return args.maxDepth;
        case "coding_changeset_diff": return { original: "export const status = 'pending';\n", modified: "export const status = 'completed';\n// 保留语法颜色与可读的差异标记\nexport const retries: number = 3;\n", binary: false, modifiedHash: "review-sha256" };
        case "coding_theia_start": return { url: query.get("ideUrl"), embedToken: query.get("embedToken") };
        case "coding_document_editor_attach": return args.root;
        case "coding_document_editor_detach": case "coding_inline_cancel": return;
        case "projects_save": return;
        case "agent_models": return [{ id: "review/available", name: "团队工作模型" }];
        case "agent_status": return { ready: true, connected: true };
        case "agents_list": case "skills_list": case "mcp_list": case "tasks_list": return [];
        case "storage_providers_list": return [];
        case "agent_set_knowledge_sources": return;
        case "internal_reload": return;
        default: return null;
      }
    },
  },
});

function Review() {
  const theme = useTheme();
  const ide = useRef<TheiaIdeFrameHandle>(null);
  const ideRoot = query.get("root") ?? "/isolated-review";
  const openFile = useMemo(() => ({ path: `${ideRoot}/status.ts`, id: 1 }), [ideRoot]);
  const [referenceOpen, setReferenceOpen] = useState(true);
  const [editorOpen, setEditorOpen] = useState(true);
  useEffect(() => {
    Object.assign(window, { enterpriseThemeReview: { calls, setTheme: theme.setTheme, setEditorOpen,
      editorModelCount: async () => (await import("monaco-editor")).editor.getModels().length,
      ideContext: () => ide.current?.getContext(),
      ideDiff: () => ide.current?.openDiff(`${ideRoot}/status.ts`, "export const status = 'pending';\n", "主题差异验收"),
    } });
  }, [theme.setTheme]);
  if (surface === "skill-import") return <ImportSkillModal onClose={() => {}} />;
  if (surface === "references") return referenceOpen ? <RefPickerDialog title={query.get("kind") ?? "专家"} options={references} selected={[references[0]]} onCancel={() => setReferenceOpen(false)} onConfirm={() => setReferenceOpen(false)} /> : null;
  if (surface === "project") return <ProjectDetailView project={query.get("state") === "unavailable" ? { ...project, defaultModelId: "review/removed" } : project} picker={{ options: { experts: references, skills: references, connectors: references }, loading: false, error: null }} models={[{ id: "review/available", label: "团队工作模型", providerId: "review" }]} onBack={() => {}} />;
  if (surface === "monaco") return editorOpen ? <TheiaTaskReview root="/isolated-review" taskId="theme-task" path="status.ts" onClose={() => setEditorOpen(false)} onOpenFile={() => {}} onReviewed={async () => {}} /> : null;
  if (surface === "real-ide") return <main className="app"><TheiaIdeFrame ref={ide} root={ideRoot} completionEnabled={false} onBeforeMutation={async () => null} onAfterMutation={async () => {}} openFileRequest={openFile} /></main>;
  return <main className="app"><section className="panel-section" style={{ padding: 24, height: "100%", overflow: "auto", background: "var(--echo-surface-canvas)" }}>
    {surface === "knowledge" ? <KnowledgePicker /> : surface === "connector" ? <McpConfigEditor onBack={() => {}} /> : <AgentSettingsPanel />}
  </section></main>;
}
createRoot(document.getElementById("root")!).render(<ThemeProvider><Suspense fallback={<div role="status">正在准备验收工作区</div>}><Review /></Suspense></ThemeProvider>);
