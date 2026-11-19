import { Select } from "@/components/Select";
import { ProjectConnectionNotice } from "./ProjectConnectionNotice";
import { captureTeamProjectOperation, projectCollaborationCapabilities, projectCollaborationError, projectConnectionIssue, teamProjectRequest, type OpenTeamExecution, type StartTeamWork, type TeamProject } from "@/lib/team-project-client";
import { loadTeamProjectSnapshot, type CheckedProjectSnapshot } from "@/lib/use-project-workspace";
import { recordSharedProjectDeletion, sharedProjectId, useSharedProjectCatalog } from "@/lib/project-sharing";
import { ResourceIcon, ResourceList, ResourceRow } from "./ResourceList";
import { modelChoiceLabel } from "@/lib/model-presentation";
/**
 * 项目面板 — 对齐 EchoAgent 项目列表页 + 新建项目弹窗。
 *
 *  - 我的项目: 搜索、视图切换和项目列表（点击进入详情）
 *  - 从模版创建: 业务模板卡片
 *  - CreateProjectDialog: 对齐目标截图（项目名称 + 指令[选择模板] + 连接器/专家/技能 +添加 + 取消/确定）
 *  - 内部 openId 切换 列表 / ProjectDetailView（无需改 App 路由）
 */
import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { AddIcon, MoreDotsIcon } from "@/foundation/components/Icon/icons";
import { ActionMenu } from "./ActionMenu";
import { Button } from "./Button";
import { TemplateCard } from "./TemplateCard";
import { CapabilitySearch, CapabilityEmptyState } from "./experts-panel/shared/CapabilityPage";
import { ManagementToolbar } from "./PageToolbar";
import { flushProjectPersistence, useProjectsStore, type ProjectMeta, type ProjectTab, type RefItem } from "@/stores/projects-store";
import { useOrgSessionStore } from "@/stores/org-session-store";
import { FolderOpen } from "lucide-react";
import {
  TEMPLATE_OPTIONS,
  getTemplate,
  ConfigRow,
  RefPickerDialog,
  useProjectPickerOptions,
  type ProjectPickerOptions,
} from "./project-picker";
import { ProjectDetailView } from "./ProjectDetailView";
import { filesystemPickDirectory, projectAssetsRemoveAll, type PermissionMode, type WorkspaceInfo } from "@/lib/agent-client";
import { useModalFocus } from "@/lib/use-modal-focus";
import { useUnsavedClose } from "@/lib/use-unsaved-close";
import { useAppDialog } from "./AppDialog";
import type { ModelOption } from "./ModelSelector";
import type { SlashCommandInvocation } from "@/lib/slash-commands";
import type { StartProjectConversation } from "@/lib/project-context";
import { displayPath } from "@/lib/display-path";
import { PermissionPicker } from "./PermissionPicker";
import { selectProjectPermissionMode, usePermissionModeStore } from "@/stores/permission-mode-store";

interface ProjectsPanelProps {
  onStartTeamWork?: StartTeamWork;
  onOpenTeamExecution?: OpenTeamExecution;
  cwd?: string;
  onSelectWorkspace?: (cwd: string) => void;
  onToast?: (msg: string) => void;
  onStartProject?: (project: ProjectMeta, expectedPermissionMode: PermissionMode) => void | Promise<void>;
  /** Internal project navigation supersedes pending task-opening requests. */
  onNavigate?: () => void;
  /** Start a new conversation within a project (creates a real EchoAgent session). */
  onStartProjectConversation?: StartProjectConversation;
  workspaces?: WorkspaceInfo[];
  onOpenSession?: (sessionId: string, cwd?: string) => void;
  onOpenProjectWorkItemSession?: (projectId: string, sessionId: string, cwd?: string) => void;
  onRenameSession?: (sessionId: string, title: string, cwd?: string) => Promise<void>;
  onArchiveSession?: (sessionId: string, archived: boolean, cwd?: string) => Promise<void>;
  onDeleteSession?: (sessionId: string, cwd?: string) => Promise<void>;
  onOpenAutomation?: () => void;
  models?: ModelOption[];
  defaultModelId?: string;
  onOpenModelSettings?: () => void;
  onClientSlashCommand?: (invocation: SlashCommandInvocation) => boolean | void | Promise<boolean | void>;
  onNavigateConnectors?: () => void;
  onOpenKnowledgeBase?: () => void;
  onOpenMeetingMinutes?: (modelId?: string) => void;
  onOpenOrganization?: () => void;
}

const FROM_TEMPLATES = TEMPLATE_OPTIONS.filter((t) => t.id !== "custom");
const PROJECT_ROLE_LABELS = { admin: "管理员", collaborator: "可编辑", viewer: "仅查看" };

export function ProjectsPanel({
  onStartTeamWork,
  onOpenTeamExecution,
  cwd,
  onToast,
  onStartProject,
  onNavigate,
  onStartProjectConversation,
  workspaces,
  onOpenSession,
  onOpenProjectWorkItemSession,
  onRenameSession,
  onArchiveSession,
  onDeleteSession,
  onOpenAutomation,
  models = [],
  defaultModelId,
  onOpenModelSettings,
  onClientSlashCommand,
  onNavigateConnectors,
  onOpenKnowledgeBase,
  onOpenMeetingMinutes,
  onOpenOrganization,
}: ProjectsPanelProps) {
  const projects = useProjectsStore((s) => s.projects);
  const persisting = useProjectsStore((s) => s.persisting);
  const persistError = useProjectsStore((s) => s.persistError);
  const retryPersist = useProjectsStore((s) => s.retryPersist);
  const rename = useProjectsStore((s) => s.rename);
  const remove = useProjectsStore((s) => s.remove);
  const activeProjectId = useProjectsStore((s) => s.activeProjectId);
  const focusProjectDraft = useProjectsStore((s) => s.focusProjectDraft);
  const activeProjectTab = useProjectsStore((s) => s.activeProjectTab);
  const setActiveProjectId = useProjectsStore((s) => s.setActiveProjectId);
  const [query, setQuery] = useState("");
  const [scope, setScope] = useState("all");
  const [templateDisclosure, setTemplateDisclosure] = useState<boolean>();
  const sharedCatalog = useSharedProjectCatalog();
  const [create, setCreate] = useState<CreatePreset | null>(null);
  const [openId, setOpenId] = useState<string | null>(null);
  const [checkedSnapshot, setCheckedSnapshot] = useState<CheckedProjectSnapshot>();
  const [sharedAttempt, setSharedAttempt] = useState<{ id: string; boundary: number; focusDraft: boolean; opening: boolean; error?: string }>();
  const [initialTab, setInitialTab] = useState<ProjectTab>("overview");
  const [draftFocusRequest, setDraftFocusRequest] = useState(0);
  const createButtonRef = useRef<HTMLButtonElement>(null);
  const picker = useProjectPickerOptions(cwd);
  const { requestInput, requestConfirmation, dialog } = useAppDialog(`${sharedCatalog.boundary}:${openId ?? ""}`);
  const navigationRevision = useRef(0);
  const cancelSharedOpen = useCallback(() => {
    ++navigationRevision.current;
    setSharedAttempt(undefined);
  }, []);
  const navigateProject = useCallback((id: string | null, focusDraft = false, tab: ProjectTab = "overview", checked?: CheckedProjectSnapshot) => {
    cancelSharedOpen();
    onNavigate?.();
    setOpenId(id);
    setCheckedSnapshot(checked);
    setInitialTab(tab);
    setDraftFocusRequest(current => focusDraft ? current + 1 : 0);
  }, [cancelSharedOpen, onNavigate]);

  useEffect(() => () => { ++navigationRevision.current; }, []);

  const showCreateProject = (preset: CreatePreset) => {
    cancelSharedOpen();
    setCreate(preset);
  };

  // Auto-open a project when navigated from the sidebar.
  useEffect(() => {
    if (activeProjectId) {
      navigateProject(activeProjectId, focusProjectDraft, activeProjectTab);
      setActiveProjectId(null);
    }
  }, [activeProjectId, activeProjectTab, focusProjectDraft, setActiveProjectId, navigateProject]);

  const sharedById = new Map(sharedCatalog.projects.map(project => [project.id, project]));
  const localItems = projects.map(local => {
    const sharedId = sharedCatalog.enabled ? sharedProjectId(local, sharedCatalog.account) : undefined;
    const shared = sharedId ? sharedById.get(sharedId) : undefined;
    return { key: local.id, name: shared?.name ?? local.name, local, sharedId, shared };
  });
  const linkedIds = new Set(localItems.flatMap(item => item.sharedId ? [item.sharedId] : []));
  const items = [...localItems, ...sharedCatalog.projects.filter(project => !linkedIds.has(project.id)).map(shared => ({
    key: `shared:${shared.id}`, name: shared.name, local: undefined, sharedId: shared.id, shared,
  }))];
  const normalizedQuery = query.trim().toLowerCase();
  const filtered = items.filter(item => item.name.toLowerCase().includes(normalizedQuery)
    && (scope === "all" || (scope === "shared") === Boolean(item.sharedId)));
  const hasSharedItems = items.some(item => item.sharedId);
  const loadingSharedEmpty = scope === "shared" && !hasSharedItems && sharedCatalog.loading && !sharedCatalog.error;
  const catalogNotice = scope === "shared" && sharedCatalog.error ? <ProjectConnectionNotice
    error={sharedCatalog.error} loading={sharedCatalog.loading} partialResults={hasSharedItems}
    onRetry={() => void sharedCatalog.reload()} onOpenOrganization={onOpenOrganization} /> : undefined;

  const assertCatalogCurrent = () => {
    if (useOrgSessionStore.getState().boundaryRevision !== sharedCatalog.boundary) {
      throw new Error("组织身份或权限已变化，请重新打开项目后操作");
    }
  };

  const openSharedProject = async (id: string, focusDraft = false) => {
    const revision = ++navigationRevision.current;
    const attempt = { id, boundary: sharedCatalog.boundary, focusDraft, opening: true };
    setSharedAttempt(previous => previous?.id === id && previous.boundary === attempt.boundary
      ? { ...attempt, error: previous.error } : attempt);
    try {
      assertCatalogCurrent();
      const identity = captureTeamProjectOperation();
      const snapshot = await loadTeamProjectSnapshot(id, identity);
      if (revision !== navigationRevision.current) return;
      identity.assertCurrent();
      const store = useProjectsStore.getState();
      const existing = store.projects.find(project => sharedProjectId(project, sharedCatalog.account) === id);
      const local = existing ?? store.add({ name: snapshot.name, sharedProjects: { [sharedCatalog.account]: id } });
      const canEdit = snapshot.role === "admin" || snapshot.role === "collaborator";
      navigateProject(local.id, focusDraft && canEdit, "overview", { boundary: attempt.boundary, snapshot });
      if (focusDraft && !canEdit) onToast?.("当前账号只能查看此共享项目。");
    } catch (error) {
      if (revision !== navigationRevision.current || useOrgSessionStore.getState().boundaryRevision !== attempt.boundary) return;
      const message = projectCollaborationError(error) || "打开共享项目失败";
      if (projectConnectionIssue(message) === "deleted") {
        recordSharedProjectDeletion(attempt.boundary, sharedCatalog.account, id);
        cancelSharedOpen();
        onToast?.(message);
      } else setSharedAttempt({ ...attempt, opening: false, error: message });
    }
  };

  const openProject = projects.find((p) => p.id === openId) ?? null;
  if (openProject) {
    return (
      <>
        <ProjectDetailView
          key={openProject.id}
          project={openProject}
          checkedSnapshot={checkedSnapshot}
          onTeamPublished={() => { void sharedCatalog.reload(); }}
          onStartTeamWork={onStartTeamWork}
          onOpenTeamExecution={onOpenTeamExecution}
          initialTab={initialTab}
          draftFocusRequest={draftFocusRequest}
          workspaces={workspaces}
          onBack={() => navigateProject(null)}
          onToast={onToast}
          onStartConversation={onStartProjectConversation}
          onOpenSession={onOpenSession}
          onOpenProjectWorkItemSession={onOpenProjectWorkItemSession}
          onRenameSession={onRenameSession}
          onArchiveSession={onArchiveSession}
          onDeleteSession={onDeleteSession}
          picker={picker}
          onOpenAutomation={onOpenAutomation}
          models={models}
          defaultModelId={defaultModelId}
          onOpenModelSettings={onOpenModelSettings}
          onClientSlashCommand={onClientSlashCommand}
          onNavigateConnectors={onNavigateConnectors}
          onOpenKnowledgeBase={onOpenKnowledgeBase}
          onOpenMeetingMinutes={onOpenMeetingMinutes}
          onOpenOrganization={onOpenOrganization}
        />
        {dialog}
      </>
    );
  }

  const handleRename = (name: string, local?: ProjectMeta, sharedId?: string) => {
    const identity = sharedId ? captureTeamProjectOperation() : undefined;
    let confirmedName = name;
    requestInput({
      title: `重命名项目“${name}”`,
      fields: [{ name: "name", label: "项目名称", defaultValue: name, required: true, maxLength: 120 }],
      confirmLabel: "保存",
      validate: ({ name: next }) => {
        const trimmed = next.trim();
        if (trimmed === confirmedName) return "请输入与当前不同的项目名称。";
        return local && projects.some(project => project.id !== local.id && project.name.trim() === trimmed)
          ? "已有同名项目，请使用其他名称。"
          : null;
      },
      action: async ({ name: next }) => {
        const nextName = next.trim();
        if (sharedId && identity) {
          assertCatalogCurrent();
          try {
            const snapshot = await loadTeamProjectSnapshot(sharedId, identity);
            if (snapshot.role !== "admin") throw new Error("只有项目管理员可以重命名共享项目。");
            if (snapshot.name !== nextName) {
              if (snapshot.name !== confirmedName) {
                confirmedName = snapshot.name;
                void sharedCatalog.reload();
                throw new Error(`项目名称已更新为「${snapshot.name}」。如需改为你输入的名称，请再次点击「保存」。`);
              }
              await teamProjectRequest("update", sharedId, { name: nextName, goal: snapshot.goal, expectedRevision: snapshot.revision }, undefined, identity);
            }
            identity.assertCurrent();
          } catch (error) {
            if (projectConnectionIssue(projectCollaborationError(error)) === "deleted") {
              identity.assertCurrent();
              recordSharedProjectDeletion(sharedCatalog.boundary, sharedCatalog.account, sharedId);
            }
            throw error;
          }
          void sharedCatalog.reload();
        }
        if (local) rename(local.id, nextName);
      },
    });
  };

  const startLocalProject = (project: ProjectMeta, returnFocus: HTMLButtonElement | null) => {
    if (!onStartProject) return;
    requestConfirmation({
      title: `开始「${project.name}」的项目对话？`,
      description: <>
        <p>工作目录为{project.cwd ? `「${displayPath(project.cwd)}」` : "自动分配的独立任务目录"}。确认后立即发送项目开场消息。</p>
        <div>任务默认权限 <PermissionPicker inheritTaskDefault showLockedAsText /></div>
      </>,
      confirmLabel: "开始项目对话",
      returnFocus,
      action: () => {
        const permissionMode = selectProjectPermissionMode(usePermissionModeStore.getState());
        if (!permissionMode) throw new Error("请确认任务默认权限后重试。");
        return onStartProject(project, permissionMode);
      },
    });
  };
  const confirmLocalRemoval = (project: ProjectMeta, name: string, sharedRecord: boolean, returnFocus: HTMLButtonElement | null) => {
    requestConfirmation({
      title: sharedRecord ? `移除「${name}」的本机记录？` : `确定删除项目「${name}」？`,
      description: <>
        {sharedRecord && <p>移除会清理此设备上的项目记录，组织端的共享项目仍可访问。</p>}
        <p>此设备上的任务和项目设置会移除。{project.assets.length > 0 && `${project.assets.length} 项本机资料副本会一并移除。`}原始文件和历史会话会保留。</p>
      </>,
      confirmLabel: sharedRecord ? "移除本机记录" : "删除项目",
      danger: true,
      returnFocus,
      action: async () => {
        await projectAssetsRemoveAll(project.id);
        remove(project.id);
        await flushProjectPersistence();
        if (sharedRecord) onToast?.("已移除本机记录。组织端的共享项目仍可访问。");
        window.requestAnimationFrame(() => createButtonRef.current?.focus());
      },
    });
  };

  const confirmSharedDeletion = (shared: TeamProject, returnFocus: HTMLButtonElement | null) => {
    const { account, boundary } = sharedCatalog;
    const identity = captureTeamProjectOperation();
    let expectedRevision = shared.revision;
    requestConfirmation({
      title: `删除共享项目「${shared.name}」？`,
      description: <>
        <p>删除后，所有成员都会失去对此项目及其共享任务和资料的访问。此操作会记入组织审计。</p>
        <p>此设备上已有的项目记录和资料副本会保留，可在「全部项目」中继续查看。</p>
      </>,
      confirmLabel: "删除共享项目",
      danger: true,
      returnFocus,
      action: async () => {
        assertCatalogCurrent();
        const capabilities = await projectCollaborationCapabilities(identity);
        if (!capabilities.projectDeletion) throw new Error("当前组织服务版本尚不支持删除共享项目，请联系管理员升级后重试。");
        try {
          const result = await teamProjectRequest<{ id: string }>("delete", shared.id, { expectedRevision }, undefined, identity);
          if (result?.id !== shared.id) throw new Error("组织服务未确认项目删除，请重试。");
        } catch (error) {
          const message = projectCollaborationError(error);
          if (projectConnectionIssue(message) !== "deleted") {
            if (message !== "项目已更新，请刷新后重试") throw error;
            const updated = await loadTeamProjectSnapshot(shared.id, identity).catch(snapshotError => {
              if (projectConnectionIssue(projectCollaborationError(snapshotError)) !== "deleted") throw snapshotError;
              return undefined;
            });
            if (updated) {
              if (updated.name !== shared.name) {
                void sharedCatalog.reload();
                throw new Error("项目名称已变化，请关闭此窗口后重新确认。");
              }
              expectedRevision = updated.revision;
              throw new Error("项目刚刚有更新。再次确认可删除最新版本。");
            }
          }
          identity.assertCurrent();
        }

        const keptLocal = recordSharedProjectDeletion(boundary, account, shared.id);
        void sharedCatalog.reload();
        try {
          if (keptLocal) await flushProjectPersistence();
          onToast?.(keptLocal ? "共享项目已删除。本机记录可在「全部项目」中查看。" : "共享项目已删除。");
        } catch (error) {
          onToast?.(`共享项目已删除。本机关联保存失败，请在项目列表重试保存：${projectCollaborationError(error)}`);
        }
        window.requestAnimationFrame(() => { if (!returnFocus?.isConnected) createButtonRef.current?.focus(); });
      },
    });
  };

  let emptyTitle = normalizedQuery ? "没有匹配的项目" : "还没有项目";
  let emptyDescription = normalizedQuery ? "试试其他项目名称。" : "可以新建项目，也可以从下方模板开始。";
  if (scope === "shared") {
    emptyTitle = normalizedQuery ? "没有匹配的共享项目" : "还没有共享项目";
    emptyDescription = normalizedQuery ? "试试其他项目名称。" : "可以在项目中邀请成员，也可以等待成员邀请你加入。";
    if (!sharedCatalog.enabled) {
      emptyTitle = "登录组织后查看共享项目";
      emptyDescription = "本机项目可在「仅自己」中查看。";
    } else if (loadingSharedEmpty) {
      emptyTitle = "正在读取共享项目";
      emptyDescription = "";
    }
  } else if (!normalizedQuery && (scope === "private" || sharedCatalog.loading || sharedCatalog.error)) {
    emptyTitle = "还没有本机项目";
  }

  return (
    <section className="management-workspace project-management" aria-label="项目">
      <ManagementToolbar title="项目">
        <Button ref={createButtonRef} size="compact" className="management-toolbar__action" onClick={() => showCreateProject({})}>
          <AddIcon size="sm" />新建项目
        </Button>
      </ManagementToolbar>
      <div className="capability-page">
        <div className="capability-page__inner capability-page__content">
      {persistError && (
        <div className="project-persist-alert" role="alert">
          <span>项目已保存在本机缓存，但项目文件保存失败，请重试：{persistError}</span>
          <button type="button" onClick={() => void retryPersist().catch(() => {})} disabled={persisting}>
            {persisting ? "正在保存…" : "重试保存"}
          </button>
        </div>
      )}
      <section className="project-section">
          {filtered.length > 0 && catalogNotice}
          {scope === "shared" && hasSharedItems && !catalogNotice && sharedCatalog.loading
            && <div className="project-collaboration-status" role="status">正在更新共享项目…</div>}
          <ResourceList collectionKey={items.length ? "projects" : undefined} className="project-grid" aria-label="所有项目"
            toolbarStart={<div className="project-list-tools">
              <div className="project-list-tools__label">
                <h2 className="project-section__title">所有项目</h2>
                <span className="resource-collection__count">{filtered.length} 项</span>
              </div>
              <Select aria-label="项目范围" className="project-scope-select" value={scope} onChange={event => setScope(event.target.value)}><option value="all">全部项目</option><option value="private">仅自己</option><option value="shared">已共享</option></Select>
              <CapabilitySearch value={query} onChange={setQuery} label="搜索项目" />
            </div>}
            emptyState={!filtered.length ? catalogNotice ?? <CapabilityEmptyState
              icon={<ProjectGlyph />} title={emptyTitle} description={emptyDescription} onClear={query ? () => setQuery("") : undefined} /> : undefined}>
            {filtered.map(({ key, name, local, sharedId, shared }) => {
              const attempt = sharedId && sharedAttempt?.id === sharedId && sharedAttempt.boundary === sharedCatalog.boundary ? sharedAttempt : undefined;
              const enter = (focusDraft = false) => {
                if (local) navigateProject(local.id, focusDraft);
                else if (sharedId) void openSharedProject(sharedId, focusDraft);
              };
              let start: ((returnFocus: HTMLButtonElement | null) => void) | undefined;
              if (sharedId && onStartTeamWork && (shared?.role === "admin" || shared?.role === "collaborator")) {
                start = () => enter(true);
              } else if (!sharedId && local && onStartProject) {
                start = returnFocus => startLocalProject(local, returnFocus);
              }
              const sharedRecord = Boolean(sharedId || local?.sharedProjects && Object.keys(local.sharedProjects).length);
              const visibility = shared ? `已共享 · ${PROJECT_ROLE_LABELS[shared.role] ?? "成员"}`
                : sharedId ? "已共享 · 本机记录" : "仅自己";
              return <ProjectCard key={key} name={name} local={local} visibility={visibility}
                description={shared?.goal || local?.description || local?.instructions || "集中管理任务、资料与交付成果"}
                onEnter={() => enter()} onStart={start} opening={attempt?.opening}
                connectionNotice={attempt?.error ? <ProjectConnectionNotice error={attempt.error} openingProject
                  loading={attempt.opening} onRetry={() => enter(attempt.focusDraft)} onOpenOrganization={onOpenOrganization} /> : undefined}
                onRename={!sharedId || shared?.role === "admin" ? () => handleRename(name, local, sharedId) : undefined}
                onDelete={local ? returnFocus => confirmLocalRemoval(local, name, sharedRecord, returnFocus) : undefined}
                onDeleteShared={shared?.role === "admin" ? returnFocus => confirmSharedDeletion(shared, returnFocus) : undefined}
                deleteLabel={sharedRecord ? "移除本机记录" : "删除"} />;
            })}
          </ResourceList>
      </section>

      <details className="project-section project-templates" open={templateDisclosure ?? items.length === 0}>
        <summary onClick={event => {
          event.preventDefault();
          setTemplateDisclosure(!event.currentTarget.parentElement?.hasAttribute("open"));
        }}>
          <span className="project-section__title">从模板创建</span>
          <span className="resource-collection__count">{FROM_TEMPLATES.length} 个</span>
        </summary>
        <div className="template-grid">
          {FROM_TEMPLATES.map((t) => (
            <TemplateCard
              key={t.id}
              icon={<ProjectGlyph />}
              title={t.title}
              description={t.desc}
              onSelect={() => showCreateProject({ templateId: t.id })}
            />
          ))}
        </div>
      </details>
        </div>
      </div>

      {create && (
        <CreateProjectDialog
          preset={create}
          onCancel={() => setCreate(null)}
          onConfirm={(saved) => {
            setCreate(null);
            navigateProject(saved.id);
          }}
          cwd={cwd}
          picker={picker}
          models={models}
          defaultModelId={defaultModelId}
          onOpenModelSettings={onOpenModelSettings}
        />
      )}
      {dialog}
    </section>
  );
}

// ============================================================
// Project Card
// ============================================================

function ProjectCard({
  name,
  description,
  visibility,
  local,
  onEnter,
  onStart,
  onRename,
  onDelete,
  onDeleteShared,
  deleteLabel,
  opening = false,
  connectionNotice,
}: {
  name: string;
  description: string;
  visibility: string;
  local?: ProjectMeta;
  onEnter: () => void;
  onStart?: (returnFocus: HTMLButtonElement | null) => void;
  onRename?: () => void;
  onDelete?: (returnFocus: HTMLButtonElement | null) => void;
  onDeleteShared?: (returnFocus: HTMLButtonElement | null) => void;
  deleteLabel: string;
  opening?: boolean;
  connectionNotice?: ReactNode;
}) {
  return (
    <ResourceRow className={connectionNotice ? "project-card2 project-card2--connection-notice" : "project-card2"}>
      <button type="button" className="project-card2__main resource-row__main" disabled={opening} onClick={onEnter}>
        <ResourceIcon><ProjectGlyph /></ResourceIcon>
        <span className="project-card2__body">
          <span className="project-card2__name resource-row__title">{name}</span>
          <span className="resource-row__description resource-clamp">{description}</span>
          <span className="resource-row__meta">
            <span>{visibility}</span>
            {local && <><span>{local.tasks.length} 个任务</span><span>{local.assets.length} 份资料</span></>}
          </span>
          {local && <span className="project-card2__sub resource-row__meta">{addedLabel(local.createdAt)}</span>}
        </span>
      </button>
      <div className="resource-row__actions">
        <button type="button" className="capability-button" onClick={onEnter} disabled={opening} aria-busy={opening}
          aria-label={`${opening ? "正在连接" : "打开项目"} ${name}`}>{opening ? "正在连接…" : "进入项目"}</button>
        {(onStart || onRename || onDelete || onDeleteShared) && <ActionMenu label={`${name} 更多操作`} menuLabel={`${name} 操作菜单`}
          trigger={<MoreDotsIcon size="sm" />} triggerClassName="resource-more" busy={opening}
          menuClassName="topbar-actions__menu resource-menu">
          {({ close, triggerRef }) => <>
            <button type="button" role="menuitem" className="topbar-actions__item" onClick={() => { close(true); onEnter(); }}>进入项目</button>
            {onStart && <button type="button" role="menuitem" className="topbar-actions__item" onClick={() => {
              close(true);
              onStart(triggerRef.current);
            }}>开始项目对话</button>}
            {onRename && <button type="button" role="menuitem" className="topbar-actions__item" onClick={() => { close(true); onRename(); }}>重命名</button>}
            {(onDelete || onDeleteShared) && <div className="project-card2__menu-sep" role="separator" />}
            {onDelete && <button type="button" role="menuitem" className="topbar-actions__item resource-action--danger"
              onClick={() => { close(true); onDelete(triggerRef.current); }}>{deleteLabel}</button>}
            {onDeleteShared && <button type="button" role="menuitem" className="topbar-actions__item resource-action--danger"
              onClick={() => { close(true); onDeleteShared(triggerRef.current); }}>删除共享项目</button>}
          </>}
        </ActionMenu>}
      </div>
      {connectionNotice && <div className="project-card2__feedback">{connectionNotice}</div>}
    </ResourceRow>
  );
}

// ============================================================
// Create Project Dialog (对齐目标截图)
// ============================================================

interface CreatePreset { templateId?: string }

function CreateProjectDialog({
  preset,
  onCancel,
  onConfirm,
  cwd,
  picker,
  models,
  defaultModelId,
  onOpenModelSettings,
}: {
  preset: CreatePreset;
  onCancel: () => void;
  onConfirm: (saved: ProjectMeta) => void;
  cwd?: string;
  picker: { options: ProjectPickerOptions; loading: boolean; error: string | null };
  models: ModelOption[];
  defaultModelId?: string;
  onOpenModelSettings?: () => void;
}) {
  const add = useProjectsStore((s) => s.add);
  const initial = preset.templateId ? getTemplate(preset.templateId) : undefined;
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [directory, setDirectory] = useState(cwd);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const pendingProject = useRef<string>();
  const { requestConfirmation, dialog: templateDialog } = useAppDialog("create-project-template");
  const [templateId, setTemplateId] = useState<string | undefined>(initial?.id);
  const [instructions, setInstructions] = useState(initial?.instructions ?? "");
  const [connectors, setConnectors] = useState<RefItem[]>(initial?.connectors ?? []);
  const [experts, setExperts] = useState<RefItem[]>(initial?.experts ?? []);
  const [skills, setSkills] = useState<RefItem[]>(initial?.skills ?? []);
  const [modelId, setModelId] = useState<string>();
  const [pickerFor, setPickerFor] = useState<null | "connectors" | "experts" | "skills">(null);
  const initialDraft = useRef(JSON.stringify({ instructions, templateId, connectors, experts, skills }));
  const initialModelId = useRef(modelId);
  const { requestClose, closeDialog } = useUnsavedClose({
    dirty: Boolean(name || description) || directory !== cwd || modelId !== initialModelId.current
      || JSON.stringify({ instructions, templateId, connectors, experts, skills }) !== initialDraft.current,
    busy,
    onClose: onCancel,
  });
  const cancel = requestClose;
  const dialogRef = useModalFocus<HTMLDivElement>(true, cancel);

  const applyTemplate = (id: string) => {
    const t = getTemplate(id);
    const apply = () => { setTemplateId(id); setInstructions(t?.instructions ?? ""); setConnectors(t?.connectors ?? []); setExperts(t?.experts ?? []); setSkills(t?.skills ?? []); };
    const previous = getTemplate(templateId);
    if (instructions !== (previous?.instructions ?? "") || connectors.length || experts.length || skills.length) requestConfirmation({ title: "替换现有项目配置？", description: "模板将替换当前指令及能力选择，项目名称、说明和目录保留。", confirmLabel: "使用模板替换", action: apply });
    else apply();
  };

  const setPicked = (k: typeof pickerFor, items: RefItem[]) => {
    if (k === "connectors") setConnectors(items);
    else if (k === "experts") setExperts(items);
    else if (k === "skills") setSkills(items);
    setPickerFor(null);
  };

  const submit = async () => {
    if (!name.trim() || busy) return;
    setBusy(true); setError("");
    try {
      const config = { name: name.trim(), description: description.trim(), cwd: directory, templateId, instructions: instructions.trim() || undefined, defaultModelId: modelId, connectors, experts, skills };
      if (pendingProject.current) useProjectsStore.getState().updateConfig(pendingProject.current, config);
      else pendingProject.current = add(config).id;
      await flushProjectPersistence();
      const saved = useProjectsStore.getState().projects.find(p => p.id === pendingProject.current);
      if (!saved) throw new Error("项目已被移除，请重新创建");
      onConfirm(saved);
    } catch (e) { setError(`保存项目失败：${String(e).replace(/^Error:\s*/, "")}。输入已保留，可重试保存。`); }
    finally { setBusy(false); }
  };

  return (
    <div className="modal-overlay create-colleague-overlay" onClick={cancel}>
      <div
        ref={dialogRef}
        className="create-colleague-dialog create-project-dialog"
        onClick={(e) => e.stopPropagation()}
        role="dialog"
        aria-modal="true"
        aria-label="新建项目"
        tabIndex={-1}
      >
        <div className="create-colleague-header">
          <h3>新建项目</h3>
          <button className="create-colleague-close" onClick={cancel} aria-label="关闭">×</button>
        </div>

        <div className="create-colleague-body">
          <div className="create-colleague-field">
            <label className="create-colleague-label" htmlFor="create-project-name">项目名称</label>
            <input
              id="create-project-name"
              type="text"
              className="create-colleague-input"
              value={name}
              maxLength={120}
              onChange={(e) => setName(e.target.value)}
              placeholder="请输入项目名称"
              data-modal-initial-focus
            />
          </div>
          <div className="create-colleague-field"><label className="create-colleague-label" htmlFor="create-project-description">项目说明 <span className="proj-optional">（可选）</span></label><textarea id="create-project-description" className="create-colleague-textarea" rows={2} style={{ minHeight: 72 }} value={description} maxLength={12000} onChange={e => setDescription(e.target.value)} placeholder="这个项目要解决什么问题、交付什么结果？" /></div>
          <div className="create-colleague-field"><span className="create-colleague-label">工作目录 <span className="proj-optional">（可选）</span></span><div className="project-settings-choice"><span>{directory ? displayPath(directory) : "为任务分配独立目录"}</span><button type="button" className="pd-btn" disabled={busy} onClick={async () => { setBusy(true); try { const selected = await filesystemPickDirectory(); if (selected) setDirectory(selected); } catch (e) { setError(String(e)); } finally { setBusy(false); } }}><FolderOpen size={14} />选择文件夹</button>{directory && <button type="button" className="pd-btn pd-btn--ghost" disabled={busy} onClick={() => setDirectory(undefined)}>清除</button>}</div></div>
          <details open={!!initial?.id}><summary>项目指令与模板 <span className="proj-optional">· 可稍后设置</span></summary>
          <div className="create-colleague-field">
            <div className="proj-field-head">
              <label className="create-colleague-label" htmlFor="create-project-instructions">指令</label>
              <Select className="create-colleague-input" aria-label="选择项目指令模板" value={templateId ?? "custom"} onChange={e => applyTemplate(e.target.value)}>{TEMPLATE_OPTIONS.map(t => <option value={t.id} key={t.id}>{t.title}</option>)}</Select>
            </div>
            <textarea id="create-project-instructions" className="create-colleague-textarea" value={instructions} onChange={e => setInstructions(e.target.value)} rows={4} placeholder="长期适用的背景、工作规范和输出要求" />
          </div></details>
          <details><summary>执行偏好 <span className="proj-optional">· 模型、专家、技能、连接器</span></summary>
          <div className="create-colleague-field">
            <label className="create-colleague-label" htmlFor="create-project-model">默认执行模型</label>
            {models.length > 0 ? (
              <Select
                id="create-project-model"
                className="create-colleague-input proj-model-native-select"
                value={modelId ?? ""}
                onChange={(event) => setModelId(event.target.value || undefined)}
              >
                <option value="">跟随应用默认{defaultModelId ? `（${models.find(m => m.id === defaultModelId)?.label ?? "默认模型"}）` : ""}</option>{models.map((model) => (
                  <option key={model.id} value={model.id}>{modelChoiceLabel(model, models)}</option>
                ))}
              </Select>
            ) : (
              <div className="proj-model-empty">
                <span>尚未配置可用模型，项目可以先创建，但执行前需要完成配置。</span>
                {onOpenModelSettings && (
                  <button type="button" onClick={onOpenModelSettings}>前往设置模型</button>
                )}
              </div>
            )}
            <span className="proj-model-field-hint">
              新任务使用此偏好，之后可随时调整。
            </span>
          </div>

          <ConfigRow label="连接器" items={connectors} onAdd={() => setPickerFor("connectors")} onRemove={(id) => setConnectors((p) => p.filter((x) => x.id !== id))} />
          <ConfigRow label="专家" items={experts} onAdd={() => setPickerFor("experts")} onRemove={(id) => setExperts((p) => p.filter((x) => x.id !== id))} />
          <ConfigRow label="技能" items={skills} onAdd={() => setPickerFor("skills")} onRemove={(id) => setSkills((p) => p.filter((x) => x.id !== id))} />
          </details>
          {error && <p role="alert" className="project-inline-error">{error}</p>}
        </div>

        <div className="create-colleague-footer create-project-footer">
          <span className="proj-version-note">创建后可添加资料、邀请成员</span>
          <button className="btn btn--ghost" onClick={cancel} disabled={busy}>取消</button>
          <button className="btn btn--primary" onClick={() => void submit()} disabled={!name.trim() || busy}>{busy ? "保存中…" : pendingProject.current ? "重试保存" : "创建项目"}</button>
        </div>
      </div>

      {closeDialog}
      {templateDialog}
      {pickerFor && (
        <RefPickerDialog
          title={pickerFor === "connectors" ? "连接器" : pickerFor === "experts" ? "专家" : "技能"}
          options={picker.options[pickerFor]}
          emptyHint={picker.loading ? "正在读取运行时能力…" : picker.error ? `读取失败：${picker.error}` : `当前没有已启用的${pickerFor === "connectors" ? "连接器" : pickerFor === "experts" ? "Agent" : "Skill"}`}
          selected={pickerFor === "connectors" ? connectors : pickerFor === "experts" ? experts : skills}
          onCancel={() => setPickerFor(null)}
          onConfirm={(items) => setPicked(pickerFor, items)}
        />
      )}
    </div>
  );
}

// ============================================================
// Glyph + helpers
// ============================================================

function ProjectGlyph() {
  return (
    <svg viewBox="0 0 24 24" width="20" height="20" fill="none" aria-hidden="true">
      <circle cx="6" cy="7" r="2.2" stroke="currentColor" strokeWidth="1.6" />
      <circle cx="18" cy="7" r="2.2" stroke="currentColor" strokeWidth="1.6" />
      <circle cx="12" cy="17.5" r="2.2" stroke="currentColor" strokeWidth="1.6" />
      <path d="M7.7 8.4 10.5 15.6M16.3 8.4 13.5 15.6M8 7h8" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" />
    </svg>
  );
}

function addedLabel(iso: string): string {
  const t = new Date(iso).getTime();
  if (Number.isNaN(t)) return "";
  const days = Math.floor((Date.now() - t) / 86_400_000);
  if (days <= 0) return "添加于 今天";
  return `添加于 ${days} 天前`;
}
