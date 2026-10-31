import type { CodingTurnOptions } from "@/lib/agent-client";
import { lazy, Suspense, type ReactNode } from "react";
import { AgentToolIcon } from "@/foundation/components/Icon/icons";
import type { ProjectMeta } from "@/stores/projects-store";
import { openExternalUrl, openLocalPath } from "@/lib/agent-client";
import type { WorkspaceInfo } from "@/lib/agent-client";
import type { ModelOption } from "./ModelSelector";
import type { SlashCommandInvocation } from "@/lib/slash-commands";
import { CapabilitySection } from "./CapabilitySection";

const ProjectsPanel = lazy(() =>
  import("./ProjectsPanel").then((module) => ({ default: module.ProjectsPanel })),
);
const ExpertsPanel = lazy(() =>
  import("./experts-panel").then((module) => ({ default: module.ExpertsPanel })),
);
const AutomationPanel = lazy(() =>
  import("./AutomationPanel").then((module) => ({ default: module.AutomationPanel })),
);
const PluginsPanel = lazy(() =>
  import("./PluginsPanel").then((module) => ({ default: module.PluginsPanel })),
);
const KnowledgeBasePanel = lazy(() =>
  import("./KnowledgeBasePanel").then((module) => ({ default: module.KnowledgeBasePanel })),
);
const MeetingMinutesPanel = lazy(() =>
  import("./MeetingMinutesPanel").then((module) => ({ default: module.MeetingMinutesPanel })),
);
const UsageQuotaPanel = lazy(() =>
  import("./UsageQuotaPanel").then((module) => ({ default: module.UsageQuotaPanel })),
);
const NotifyChannelsPanel = lazy(() =>
  import("./NotifyChannelsPanel").then((module) => ({ default: module.NotifyChannelsPanel })),
);
const CloudStoragePanel = lazy(() =>
  import("./CloudStoragePanel").then((module) => ({ default: module.CloudStoragePanel })),
);
const OrganizationMemoryPanel = lazy(() =>
  import("./OrganizationMemoryPanel").then((module) => ({ default: module.OrganizationMemoryPanel })),
);
const CodingWorkbench = lazy(() =>
  import("@/features/coding/CodingWorkbench").then((module) => ({
    default: module.CodingWorkbench,
  })),
);

function DeferredPanel({ children }: { children: ReactNode }) {
  return (
    <Suspense
      fallback={(
        <div className="placeholder-page" role="status" aria-live="polite">
          <p className="placeholder-page__desc">正在加载…</p>
        </div>
      )}
    >
      {children}
    </Suspense>
  );
}

interface PlaceholderPageProps {
  onOpenAutomationSettings?: (section: "general" | "notifications") => void;
  label: string;
  /** Navigate to another sidebar view (e.g. 自动化 → 管理连接器 → 专家·技能·连接器). */
  onNavigate?: (label: string) => void;
  /** Open a session produced by an automation run record. */
  onOpenSession?: (sessionId: string, cwd?: string) => void;
  /** Navigate to the home page (used after expert summon). */
  onGoHome?: () => void;
  /** Open the custom expert editor after navigating from the conversation rail. */
  createExpertRequested?: boolean;
  onCreateExpertRequestHandled?: () => void;
  /** Start a new task with organization knowledge selected once. */
  onStartOrganizationConversation?: () => void;
  /** Surface transient feedback (errors, success toasts). */
  onToast?: (message: string) => void;
  /** Current cwd (for memory workspace scope, projects panel). */
  cwd?: string;
  /** Switch the active workspace (projects panel). */
  onSelectWorkspace?: (cwd: string) => void;
  /** Known working directories for the Coding Workspace picker. */
  workspaces?: WorkspaceInfo[];
  /** Recently opened coding projects. */
  codingWorkspaces?: WorkspaceInfo[];
  /** Currently active coding project (forwarded to CodingWorkbench). */
  activeCodingWorkspaceCwd?: string;
  /** Close the current project or remove a path from recent projects. */
  onCloseCodingWorkspace?: (cwd: string) => void;
  /** Coding Workspace Agent/runtime integration. */
  codingApiReady?: boolean;
  codingModels?: ModelOption[];
  codingModelId?: string;
  onOpenModelSettings?: () => void;
  onExitCodingWorkspace?: () => void;
  onRegisterCodingLeaveGuard?: (guard: (() => Promise<boolean>) | null) => void;
  /** Start an Agent session for a coding task. Returns the new session id. */
  onStartCodingRun?: (
    root: string,
    requirement: string,
    modelId: string | undefined,
    contextPaths: string[],
    onSessionReady: (sessionId: string) => Promise<void>,
    promptTextOverride?: string,
    coding?: CodingTurnOptions,
  ) => Promise<string | undefined>;
  onActivateCodingSession?: (sessionId: string, cwd: string) => Promise<void>;
  onChangeCodingModel?: (modelId: string) => void | Promise<void>;
  onSendCodingMessage?: (
    text: string,
    promptTextOverride?: string,
    coding?: CodingTurnOptions,
  ) => boolean | void | Promise<boolean | void>;
  onCorrectCodingRun?: (text: string) => boolean | Promise<boolean>;
  onCancelCodingRun?: () => boolean | void | Promise<boolean | void>;
  /** Current session id (for plugin actions that need a session). */
  sessionId?: string;
  /** 项目页：进入项目（新建会话并注入说明）。 */
  onStartProject?: (project: ProjectMeta) => void;
  /** 项目页：在项目中新建对话（创建真实 EchoAgent 会话）。 */
  onStartProjectConversation?: (
    projectId: string,
    message: string,
    modelId?: string,
    attachments?: string[],
  ) => Promise<string | undefined>;
  projectModels?: ModelOption[];
  projectDefaultModelId?: string;
  meetingModelId?: string;
  meetingModels?: ModelOption[];
  onOpenMeetingMinutes?: (modelId?: string) => void;
  onClientSlashCommand?: (invocation: SlashCommandInvocation) => boolean | void | Promise<boolean | void>;
  onRenameSession?: (sessionId: string, title: string, cwd?: string) => Promise<void>;
  onArchiveSession?: (sessionId: string, archived: boolean, cwd?: string) => Promise<void>;
  onDeleteSession?: (sessionId: string, cwd?: string) => Promise<void>;
  /** Native automation lifecycle refresh token. */
  automationRefreshSignal?: number;
  notificationAutomationId?: string;
  notificationAutomationSequence?: number;
}

/** EchoAgent 功能面板（项目/组织/专家能力/自动化/知识库/插件）。 */
export function PlaceholderPage({
  label,
  onNavigate,
  onOpenSession,
  onGoHome,
  createExpertRequested,
  onCreateExpertRequestHandled,
  onStartOrganizationConversation,
  onToast,
  cwd,
  onSelectWorkspace,
  workspaces,
  codingWorkspaces,
  activeCodingWorkspaceCwd,
  onCloseCodingWorkspace,
  codingApiReady,
  codingModels,
  codingModelId,
  onOpenModelSettings,
  onExitCodingWorkspace,
  onRegisterCodingLeaveGuard,
  onStartCodingRun,
  onActivateCodingSession,
  onChangeCodingModel,
  onSendCodingMessage,
  onCancelCodingRun,
  onCorrectCodingRun,
  sessionId,
  onStartProject,
  onStartProjectConversation,
  projectModels,
  projectDefaultModelId,
  meetingModelId,
  meetingModels,
  onOpenMeetingMinutes,
  onClientSlashCommand,
  onRenameSession,
  onArchiveSession,
  onDeleteSession,
  automationRefreshSignal,
  notificationAutomationId,
  notificationAutomationSequence,
  onOpenAutomationSettings,
}: PlaceholderPageProps) {
  if (label === "项目") {
    return (
      <DeferredPanel>
        <ProjectsPanel
          cwd={cwd}
          onSelectWorkspace={onSelectWorkspace}
          onToast={onToast}
          onStartProject={onStartProject}
          onStartProjectConversation={onStartProjectConversation}
          models={projectModels}
          defaultModelId={projectDefaultModelId}
          onOpenModelSettings={onOpenModelSettings}
          onClientSlashCommand={onClientSlashCommand}
          onNavigateConnectors={() => onNavigate?.("专家·技能·连接器")}
          onOpenKnowledgeBase={() => onNavigate?.("知识库")}
          onOpenMeetingMinutes={onOpenMeetingMinutes ?? (() => onNavigate?.("录音转写"))}
          onOpenOrganization={() => onNavigate?.("组织")}
          onOpenSession={onOpenSession}
          onRenameSession={onRenameSession}
          onArchiveSession={onArchiveSession}
          onDeleteSession={onDeleteSession}
          onOpenAutomation={() => onNavigate?.("自动化")}
        />
      </DeferredPanel>
    );
  }

  if (label === "组织") {
    return <DeferredPanel><OrganizationMemoryPanel onToast={onToast} cwd={cwd} onStartConversation={onStartOrganizationConversation} /></DeferredPanel>;
  }

  if (["专家·技能·连接器", "技能", "连接器"].includes(label)) {
    const initialTab = label === "技能" ? "skills" : label === "连接器" ? "connectors" : "experts";
    return (
      <DeferredPanel>
        <CapabilitySection active={label} onNavigate={onNavigate}><ExpertsPanel cwd={cwd} onGoHome={onGoHome} onToast={onToast} initialTab={initialTab} hideNavigation createExpertRequested={createExpertRequested} onCreateExpertRequestHandled={onCreateExpertRequestHandled} /></CapabilitySection>
      </DeferredPanel>
    );
  }

  if (label === "自动化") {
    return (
      <DeferredPanel>
        <AutomationPanel
          onOpenSettings={onOpenAutomationSettings}
          onToast={onToast}
          onNavigate={onNavigate}
          onOpenSession={onOpenSession}
          cwd={cwd}
          refreshSignal={automationRefreshSignal}
          notificationAutomationId={notificationAutomationId}
          notificationAutomationSequence={notificationAutomationSequence}
        />
      </DeferredPanel>
    );
  }

  if (label === "代码开发") {
    return (
      <DeferredPanel>
        <CodingWorkbench
          cwd={cwd}
          workspaces={workspaces}
          onSelectWorkspace={onSelectWorkspace}
          onToast={onToast}
          codingWorkspaces={codingWorkspaces ?? []}
          activeCodingWorkspaceCwd={activeCodingWorkspaceCwd ?? ""}
          onCloseCodingWorkspace={onCloseCodingWorkspace}
          apiReady={codingApiReady}
          models={codingModels}
          defaultModelId={codingModelId}
          onOpenSettings={onOpenModelSettings}
          onExit={onExitCodingWorkspace}
          onRegisterLeaveGuard={onRegisterCodingLeaveGuard}
          sessionId={sessionId}
          onStartRun={onStartCodingRun}
          onActivateSession={onActivateCodingSession}
          onChangeModel={onChangeCodingModel}
          onSendMessage={onSendCodingMessage}
          onCancelRun={onCancelCodingRun}
          onCorrectRun={onCorrectCodingRun}
        />
      </DeferredPanel>
    );
  }

  if (label === "插件") {
    return (
      <DeferredPanel>
        <CapabilitySection active={label} onNavigate={onNavigate}>
          <div className="capabilities-content capability-tab">
            <PluginsPanel sessionId={sessionId} onToast={onToast} />
          </div>
        </CapabilitySection>
      </DeferredPanel>
    );
  }

  // 知识库(可插拔源,对齐 EchoAgent knowledge-base-panel)。
  if (label === "知识库") {
    return (
      <DeferredPanel>
        <div className="placeholder-page placeholder-page--panel knowledge-page">
          <KnowledgeBasePanel
            onOpen={(id, url) => {
              const target = url ?? id;
              const open = /^https?:\/\//i.test(target)
                ? openExternalUrl(target)
                : openLocalPath(target, cwd);
              void open.catch((error) => onToast?.(`打开知识条目失败：${String(error).replace(/^Error:\s*/, "")}`));
            }}
            onToast={onToast}
          />
        </div>
      </DeferredPanel>
    );
  }

  if (label === "录音转写") {
    return (
      <DeferredPanel>
        <MeetingMinutesPanel
          modelId={meetingModelId}
          models={meetingModels ?? []}
          onToast={onToast}
          onOpenModelSettings={onOpenModelSettings}
        />
      </DeferredPanel>
    );
  }

  // 用量配额(对齐 EchoAgent credit-usage)。
  if (label === "用量统计") {
    return (
      <DeferredPanel>
        <div className="placeholder-page placeholder-page--panel">
          <UsageQuotaPanel />
        </div>
      </DeferredPanel>
    );
  }

  // 通知渠道(对齐 EchoAgent IM 渠道)。
  if (label === "通知渠道") {
    return (
      <DeferredPanel>
        <div className="placeholder-page placeholder-page--panel">
          <NotifyChannelsPanel onToast={onToast} />
        </div>
      </DeferredPanel>
    );
  }

  // 云存储(对齐 EchoAgent 腾讯 Drive)。
  if (label === "云存储") {
    return (
      <DeferredPanel>
        <div className="placeholder-page placeholder-page--panel">
          <CloudStoragePanel onToast={onToast} />
        </div>
      </DeferredPanel>
    );
  }

  // 未注册的路由应该只会由损坏的持久化状态或新旧版本不兼容触发。
  return (
    <div className="placeholder-page">
      <AgentToolIcon size="xl" color="var(--echo-text-tertiary)" />
      <h2 className="placeholder-page__title">无法打开「{label}」</h2>
      <p className="placeholder-page__desc">当前版本未注册该功能路由，请返回首页重试。</p>
      {onGoHome && <button type="button" className="btn btn--primary" onClick={onGoHome}>返回首页</button>}
    </div>
  );
}
