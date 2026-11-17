import { Select } from "@/components/Select";
import { ProjectConnectionNotice } from "./ProjectConnectionNotice";
import { captureTeamProjectOperation, teamProjectRequest, type TeamProjectSnapshot, type OpenTeamExecution, type StartTeamWork } from "@/lib/team-project-client";
import { sharedProjectId, useSharedProjectCatalog } from "@/lib/project-sharing";
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
  useId,
  useLayoutEffect,
  useRef,
  useState,
  type CSSProperties,
} from "react";
import { createPortal } from "react-dom";
import { AddIcon, MoreDotsIcon } from "@/foundation/components/Icon/icons";
import { Button } from "./Button";
import { TemplateCard } from "./TemplateCard";
import { CapabilitySearch, CapabilityEmptyState } from "./experts-panel/shared/CapabilityPage";
import { ManagementToolbar } from "./PageToolbar";
import { flushProjectPersistence, useProjectsStore, type ProjectMeta, type ProjectTab, type RefItem } from "@/stores/projects-store";
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
const PROJECT_MENU_MARGIN = 8;
const PROJECT_MENU_OFFSET = 4;
const PROJECT_MENU_ESTIMATED_WIDTH = 140;
const PROJECT_MENU_ESTIMATED_HEIGHT = 150;
const PROJECT_MENU_Z_INDEX = "var(--echo-layer-popover)";

interface ProjectMenuPosition {
  left: number;
  top: number;
  width: number;
  maxHeight: number;
  placement: "top" | "bottom";
}

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
  const sharedCatalog = useSharedProjectCatalog();
  const [create, setCreate] = useState<CreatePreset | null>(null);
  const [openId, setOpenId] = useState<string | null>(null);
  const [initialTab, setInitialTab] = useState<ProjectTab>("overview");
  const [draftFocusRequest, setDraftFocusRequest] = useState(0);
  const [deleteTarget, setDeleteTarget] = useState<ProjectMeta | null>(null);
  const [deleting, setDeleting] = useState(false);
  const createButtonRef = useRef<HTMLButtonElement>(null);
  const picker = useProjectPickerOptions(cwd);
  const { requestInput, requestConfirmation, dialog } = useAppDialog(openId);
  const navigateProject = useCallback((id: string | null, focusDraft = false, tab: ProjectTab = "overview") => {
    onNavigate?.();
    setOpenId(id);
    setInitialTab(tab);
    setDraftFocusRequest(current => focusDraft ? current + 1 : 0);
  }, [onNavigate]);

  // Auto-open a project when navigated from the sidebar.
  useEffect(() => {
    if (activeProjectId) {
      navigateProject(activeProjectId, focusProjectDraft, activeProjectTab);
      setActiveProjectId(null);
    }
  }, [activeProjectId, activeProjectTab, focusProjectDraft, setActiveProjectId, navigateProject]);

  const linkedIds = new Set(projects.map(project => sharedProjectId(project, sharedCatalog.account)).filter(Boolean));
  const remoteProjects = sharedCatalog.projects.filter(project => !linkedIds.has(project.id));
  const filteredRemote = remoteProjects.filter(project => scope !== "private" && project.name.toLowerCase().includes(query.toLowerCase()));
  const filtered = projects.filter(project => project.name.toLowerCase().includes(query.toLowerCase()) && (scope === "all" || (scope === "shared") === !!sharedProjectId(project, sharedCatalog.account)));
  const totalCount = projects.length + remoteProjects.length;
  const filteredCount = filtered.length + filteredRemote.length;

  const openSharedProject = async (remoteId: string) => {
    try {
      const identity = captureTeamProjectOperation();
      const remote = sharedCatalog.projects.find(project => project.id === remoteId);
      if (!remote) return;
      identity.assertCurrent();
      const store = useProjectsStore.getState();
      let local = store.projects.find(project => sharedProjectId(project, sharedCatalog.account) === remoteId);
      if (!local) local = store.add({ name: remote.name });
      await store.linkSharedProject(local.id, sharedCatalog.account, remoteId);
      identity.assertCurrent();
      navigateProject(local.id, false, "overview");
    } catch (error) { onToast?.(`打开项目失败：${String(error)}`); }
  };

  const openProject = projects.find((p) => p.id === openId) ?? null;
  if (openProject) {
    return (
      <>
        <ProjectDetailView
          key={openProject.id}
          project={openProject}
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

  const handleRename = (p: ProjectMeta) => {
    requestInput({
      title: `重命名项目“${p.name}”`,
      fields: [{ name: "name", label: "项目名称", defaultValue: p.name, required: true, maxLength: 120 }],
      confirmLabel: "保存",
      validate: ({ name }) => {
        const trimmed = name.trim();
        if (trimmed === p.name) return "请输入与当前不同的项目名称。";
        return projects.some((project) => project.id !== p.id && project.name.trim() === trimmed)
          ? "已有同名项目，请使用其他名称。"
          : null;
      },
      action: async ({ name }) => {
        const remoteId = sharedProjectId(p, sharedCatalog.account);
        if (remoteId) {
          const identity = captureTeamProjectOperation();
          const snapshot = await teamProjectRequest<TeamProjectSnapshot>("snapshot", remoteId, undefined, undefined, identity);
          await teamProjectRequest("update", remoteId, { name: name.trim(), goal: snapshot.goal, expectedRevision: snapshot.revision }, undefined, identity);
          identity.assertCurrent();
        }
        rename(p.id, name.trim());
        void sharedCatalog.reload();
      },
    });
  };
  const handleDelete = async () => {
    const target = deleteTarget;
    if (!target || deleting) return;
    setDeleting(true);
    try {
      await projectAssetsRemoveAll(target.id);
      remove(target.id);
      setDeleteTarget(null);

      // The original menu trigger disappears with the deleted card. Move
      // focus to the primary project action instead of leaving it on <body>.
      window.requestAnimationFrame(() => createButtonRef.current?.focus());
    } catch (error) {
      onToast?.(`删除失败：${String(error).replace(/^Error:\s*/, "")}`);
    } finally {
      setDeleting(false);
    }
  };

  return (
    <section className="management-workspace project-management" aria-label="项目">
      <ManagementToolbar title="项目">
        <Button ref={createButtonRef} size="compact" className="management-toolbar__action" onClick={() => setCreate({})}>
          <AddIcon size="sm" />新建项目
        </Button>
      </ManagementToolbar>
      <div className="capability-page">
        <div className="capability-page__inner capability-page__content">
      {persistError && (
        <div className="project-persist-alert" role="alert">
          <span>项目已保存在本机缓存，但项目文件保存失败，请重试：{persistError}</span>
          <button type="button" onClick={retryPersist} disabled={persisting}>
            {persisting ? "重试中…" : "重试"}
          </button>
        </div>
      )}
      {scope !== "private" && (sharedCatalog.error
        ? <ProjectConnectionNotice error={sharedCatalog.error} loading={sharedCatalog.loading} partialResults={scope === "all"} onRetry={() => void sharedCatalog.reload()} onOpenOrganization={onOpenOrganization} />
        : sharedCatalog.loading && filteredCount > 0 && <div className="project-collaboration-status" role="status">正在读取共享项目…</div>)}
      <section className="project-section">
          <ResourceList collectionKey={totalCount ? "projects" : undefined} className="project-grid" aria-label="所有项目"
            toolbarStart={<div className="project-list-tools">
              <div className="project-list-tools__label">
                <h2 className="project-section__title">所有项目</h2>
                <span className="resource-collection__count">{filteredCount} 项</span>
              </div>
              <Select aria-label="项目范围" className="project-scope-select" value={scope} onChange={event => setScope(event.target.value)}><option value="all">全部项目</option><option value="private">仅自己</option><option value="shared">已共享</option></Select>
              <CapabilitySearch value={query} onChange={setQuery} label="搜索项目" />
            </div>}
            emptyState={filteredCount === 0 && scope !== "private" && (sharedCatalog.loading || sharedCatalog.error)
              ? <CapabilityEmptyState icon={<ProjectGlyph />} title={sharedCatalog.loading ? "正在读取共享项目" : "共享项目暂时不可用"} description={scope === "all" ? "本机项目可正常使用；共享项目恢复连接后会在这里显示。" : "本机项目仍可在“全部项目”或“仅自己”中查看。"} onClear={query ? () => setQuery("") : undefined} />
              : filteredCount === 0 ? <CapabilityEmptyState icon={<ProjectGlyph />}
              title={totalCount === 0 ? "还没有项目" : "没有匹配的项目"}
              description={totalCount === 0 ? "可以新建项目，也可以从下方模板开始。" : "试试其他项目名称。"}
              onClear={query ? () => setQuery("") : undefined} /> : undefined}>
            {filtered.map((p) => (
              <ProjectCard
                key={p.id}
                project={p}
                shared={!!sharedProjectId(p, sharedCatalog.account)}
                onEnter={() => navigateProject(p.id)}
                onStart={onStartProject ? (returnFocus) => requestConfirmation({
                  title: `开始「${p.name}」的项目对话？`,
                  description: <>
                    <p>工作目录为{p.cwd ? `「${displayPath(p.cwd)}」` : "自动分配的独立任务目录"}。确认后立即发送项目开场消息。</p>
                    <div>任务默认权限 <PermissionPicker inheritTaskDefault showLockedAsText /></div>
                  </>,
                  confirmLabel: "开始项目对话",
                  returnFocus,
                  action: () => {
                    const permissionMode = selectProjectPermissionMode(usePermissionModeStore.getState());
                    if (!permissionMode) throw new Error("请确认任务默认权限后重试。");
                    return onStartProject(p, permissionMode);
                  },
                }) : undefined}
                onRename={() => handleRename(p)}
                onDelete={() => setDeleteTarget(p)}
              />
            ))}
            {filteredRemote.map(project => <ResourceRow className="project-remote-row" key={`shared:${project.id}`}><button type="button" className="project-card2__main resource-row__main" onClick={() => void openSharedProject(project.id)}><ResourceIcon><ProjectGlyph /></ResourceIcon><span className="project-card2__body"><span className="project-card2__name resource-row__title">{project.name}</span><span className="resource-row__description resource-clamp">{project.goal || "集中管理共享工作与成果"}</span><span className="resource-row__meta">已共享 · {project.role === "admin" ? "管理员" : project.role === "viewer" ? "仅查看" : "可编辑"}</span></span></button><div className="resource-row__actions"><button type="button" className="capability-button" onClick={() => void openSharedProject(project.id)} aria-label={`打开项目 ${project.name}`}>进入项目</button></div></ResourceRow>)}
          </ResourceList>
      </section>

      <section className="project-section">
        <div className="project-section__head">
          <h2 className="project-section__title">从模板创建</h2>
        </div>
        <div className="template-grid">
          {FROM_TEMPLATES.map((t) => (
            <TemplateCard
              key={t.id}
              icon={<ProjectGlyph />}
              title={t.title}
              description={t.desc}
              onSelect={() => setCreate({ templateId: t.id })}
            />
          ))}
        </div>
      </section>
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
      {deleteTarget && (
        <ProjectDeleteDialog
          project={deleteTarget}
          busy={deleting}
          onCancel={() => {
            if (!deleting) setDeleteTarget(null);
          }}
          onConfirm={() => void handleDelete()}
        />
      )}
      {dialog}
    </section>
  );
}

function ProjectDeleteDialog({
  project,
  busy,
  onCancel,
  onConfirm,
}: {
  project: ProjectMeta;
  busy: boolean;
  onCancel: () => void;
  onConfirm: () => void;
}) {
  const titleId = useId();
  const descriptionId = useId();
  const dialogRef = useModalFocus<HTMLDivElement>(true, () => {
    if (!busy) onCancel();
  });
  const assetDetail = project.assets.length > 0
    ? `项目中的 ${project.assets.length} 项资产副本也会删除。`
    : "";

  return (
    <div
      className="modal-overlay"
      onClick={(event) => {
        if (event.target === event.currentTarget && !busy) onCancel();
      }}
    >
      <div
        ref={dialogRef}
        className="atm-confirm-dialog"
        role="alertdialog"
        aria-modal="true"
        aria-labelledby={titleId}
        aria-describedby={descriptionId}
        tabIndex={-1}
      >
        <h3 id={titleId} className="atm-confirm-title">确定删除项目「{project.name}」？</h3>
        <p id={descriptionId} className="atm-confirm-content">
          {assetDetail}{assetDetail ? " " : ""}原始文件和历史会话不会被删除。{project.sharedProjects && Object.keys(project.sharedProjects).length > 0 && " 已共享的项目和成员权限会保留，可从项目列表继续访问。"}
        </p>
        <div className="atm-confirm-actions">
          <button
            type="button"
            className="atm-btn atm-btn--secondary"
            onClick={onCancel}
            disabled={busy}
            data-modal-initial-focus
          >
            取消
          </button>
          <button
            type="button"
            className="atm-btn atm-btn--danger"
            onClick={onConfirm}
            disabled={busy}
          >
            {busy ? "删除中…" : "删除项目"}
          </button>
        </div>
      </div>
    </div>
  );
}

// ============================================================
// Project Card
// ============================================================

function ProjectCard({
  project,
  shared = false, onEnter, onStart, onRename, onDelete,
}: {
  project: ProjectMeta;
  shared?: boolean;
  onEnter: () => void;
  onStart?: (returnFocus: HTMLButtonElement | null) => void;
  onRename: () => void;
  onDelete: () => void;
}) {
  const [menuOpen, setMenuOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const [menuPosition, setMenuPosition] = useState<ProjectMenuPosition | null>(null);

  const closeMenu = useCallback((restoreFocus = false) => {
    setMenuOpen(false);
    setMenuPosition(null);
    if (restoreFocus) {
      window.requestAnimationFrame(() => triggerRef.current?.focus());
    }
  }, []);

  const updateMenuPosition = useCallback(() => {
    const trigger = triggerRef.current;
    if (!trigger) return;

    const rect = trigger.getBoundingClientRect();
    const viewportWidth = window.innerWidth;
    const viewportHeight = window.innerHeight;
    if (
      rect.bottom < 0 || rect.top > viewportHeight ||
      rect.right < 0 || rect.left > viewportWidth
    ) {
      closeMenu();
      return;
    }

    const measuredWidth = menuRef.current?.offsetWidth || PROJECT_MENU_ESTIMATED_WIDTH;
    // scrollHeight retains the full content height after maxHeight constrains
    // the visible box, so repositioning can expand the menu again later.
    const measuredHeight = menuRef.current?.scrollHeight
      || menuRef.current?.offsetHeight
      || PROJECT_MENU_ESTIMATED_HEIGHT;
    const menuWidth = Math.min(measuredWidth, Math.max(0, viewportWidth - PROJECT_MENU_MARGIN * 2));
    const spaceBelow = viewportHeight - rect.bottom - PROJECT_MENU_OFFSET - PROJECT_MENU_MARGIN;
    const spaceAbove = rect.top - PROJECT_MENU_OFFSET - PROJECT_MENU_MARGIN;
    const placement = spaceBelow >= measuredHeight || spaceBelow >= spaceAbove ? "bottom" : "top";
    const availableHeight = Math.max(0, placement === "bottom" ? spaceBelow : spaceAbove);
    const renderedHeight = Math.min(measuredHeight, availableHeight);
    const maxLeft = Math.max(PROJECT_MENU_MARGIN, viewportWidth - menuWidth - PROJECT_MENU_MARGIN);
    const left = Math.min(Math.max(PROJECT_MENU_MARGIN, rect.right - menuWidth), maxLeft);
    const desiredTop = placement === "bottom"
      ? rect.bottom + PROJECT_MENU_OFFSET
      : rect.top - PROJECT_MENU_OFFSET - renderedHeight;
    const maxTop = Math.max(PROJECT_MENU_MARGIN, viewportHeight - renderedHeight - PROJECT_MENU_MARGIN);
    const top = Math.min(Math.max(PROJECT_MENU_MARGIN, desiredTop), maxTop);

    setMenuPosition({ left, top, width: menuWidth, maxHeight: availableHeight, placement });
  }, [closeMenu]);

  useLayoutEffect(() => {
    if (!menuOpen) return;
    updateMenuPosition();
    const frame = window.requestAnimationFrame(updateMenuPosition);
    window.addEventListener("resize", updateMenuPosition);
    window.addEventListener("scroll", updateMenuPosition, true);
    return () => {
      window.cancelAnimationFrame(frame);
      window.removeEventListener("resize", updateMenuPosition);
      window.removeEventListener("scroll", updateMenuPosition, true);
    };
  }, [menuOpen, updateMenuPosition]);

  useEffect(() => {
    if (!menuOpen) return;
    const h = (e: MouseEvent) => {
      const target = e.target as Node;
      if (ref.current?.contains(target) || menuRef.current?.contains(target)) return;
      closeMenu();
    };
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") closeMenu(true);
    };
    document.addEventListener("mousedown", h);
    document.addEventListener("keydown", onKeyDown);
    return () => {
      document.removeEventListener("mousedown", h);
      document.removeEventListener("keydown", onKeyDown);
    };
  }, [menuOpen, closeMenu]);

  const menuStyle: CSSProperties = menuPosition
    ? {
        position: "fixed",
        left: menuPosition.left,
        top: menuPosition.top,
        width: menuPosition.width,
        minWidth: menuPosition.width,
        zIndex: PROJECT_MENU_Z_INDEX,
        maxHeight: menuPosition.maxHeight,
        overflowY: "auto",
      }
    : { position: "fixed", visibility: "hidden" };

  return (
    <ResourceRow className="project-card2" ref={ref}>
      <button type="button" className="project-card2__main resource-row__main" onClick={onEnter}>
        <ResourceIcon><ProjectGlyph /></ResourceIcon>
        <span className="project-card2__body">
          <span className="project-card2__name resource-row__title">{project.name}</span>
          <span className="resource-row__description resource-clamp">{project.description || project.instructions || "集中管理任务、资料与交付成果"}</span>
          <span className="resource-row__meta"><span>{shared ? "已共享" : "仅自己"}</span><span>{project.tasks.length} 个任务</span><span>{project.assets.length} 份资料</span></span>
          <span className="project-card2__sub resource-row__meta">{addedLabel(project.createdAt)}</span>
        </span>
      </button>
      <div className="resource-row__actions">
        <button type="button" className="capability-button" onClick={onEnter} aria-label={`打开项目 ${project.name}`}>进入项目</button>
        <div className="project-card2__more-wrap">
          <button
            ref={triggerRef}
            type="button"
            className="project-card2__more"
            aria-label="更多操作"
            aria-haspopup="menu"
            aria-expanded={menuOpen}
            onClick={(e) => {
              e.stopPropagation();
              if (menuOpen) closeMenu();
              else setMenuOpen(true);
            }}
          >
            <MoreDotsIcon size="sm" />
          </button>
        </div>
      </div>
      {menuOpen && typeof document !== "undefined" && createPortal(
        <div
          ref={menuRef}
          className="project-card2__menu"
          style={menuStyle}
          role="menu"
          aria-label={`${project.name} 操作菜单`}
          data-placement={menuPosition?.placement}
          onClick={(e) => e.stopPropagation()}
        >
          <button type="button" role="menuitem" className="project-card2__menu-item" onClick={() => { closeMenu(); onEnter(); }}>进入项目</button>
          {onStart && <button type="button" role="menuitem" className="project-card2__menu-item" onClick={() => {
            closeMenu();
            onStart(triggerRef.current);
          }}>开始项目对话</button>}
          <button type="button" role="menuitem" className="project-card2__menu-item" onClick={() => { closeMenu(); onRename(); }}>重命名</button>
          <div className="project-card2__menu-sep" role="separator" />
          <button
            type="button"
            role="menuitem"
            className="project-card2__menu-item project-card2__menu-item--danger"
            onClick={() => {
              // Preserve a connected invoker for the modal focus hook before
              // the portal menu item is removed.
              triggerRef.current?.focus();
              closeMenu();
              onDelete();
            }}
          >
            删除
          </button>
        </div>,
        document.body,
      )}
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
