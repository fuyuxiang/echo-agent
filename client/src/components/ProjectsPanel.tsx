/**
 * 项目面板 — 对齐 EchoAgent 项目列表页 + 新建项目弹窗。
 *
 *  - hero: 标题「项目」+ 副标题 + 「新建项目」+ 协作插画
 *  - 我的项目: 搜索 + 卡片网格（点击进入详情）
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
import { AddIcon, SearchIcon, MoreDotsIcon, ChevronDownIcon } from "@/foundation/components/Icon/icons";
import heroImg from "@/assets/landing-hero.png";
import { useProjectsStore, type ProjectMeta, type RefItem } from "@/stores/projects-store";
import {
  TEMPLATE_OPTIONS,
  getTemplate,
  ConfigRow,
  RefPickerDialog,
  useProjectPickerOptions,
  type ProjectPickerOptions,
} from "./project-picker";
import { ProjectDetailView } from "./ProjectDetailView";
import { projectAssetsRemoveAll } from "@/lib/agent-client";
import { useModalFocus } from "@/lib/use-modal-focus";
import { useUnsavedClose } from "@/lib/use-unsaved-close";
import { useAnchoredFloating } from "@/lib/use-anchored-floating";
import { useAppDialog } from "./AppDialog";
import type { ModelOption } from "./ModelSelector";
import type { SlashCommandInvocation } from "@/lib/slash-commands";

interface ProjectsPanelProps {
  cwd?: string;
  onSelectWorkspace?: (cwd: string) => void;
  onToast?: (msg: string) => void;
  onStartProject?: (project: ProjectMeta) => void;
  /** Start a new conversation within a project (creates a real EchoAgent session). */
  onStartProjectConversation?: (
    projectId: string,
    message: string,
    modelId?: string,
    attachments?: string[],
  ) => Promise<string | undefined>;
  onOpenSession?: (sessionId: string, cwd?: string) => void;
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
  cwd,
  onToast,
  onStartProject,
  onStartProjectConversation,
  onOpenSession,
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
  const setActiveProjectId = useProjectsStore((s) => s.setActiveProjectId);
  const [query, setQuery] = useState("");
  const [create, setCreate] = useState<CreatePreset | null>(null);
  const [openId, setOpenId] = useState<string | null>(null);
  const [deleteTarget, setDeleteTarget] = useState<ProjectMeta | null>(null);
  const [deleting, setDeleting] = useState(false);
  const createButtonRef = useRef<HTMLButtonElement>(null);
  const picker = useProjectPickerOptions(cwd);
  const { requestInput, dialog } = useAppDialog(openId);

  // Auto-open a project when navigated from the sidebar.
  useEffect(() => {
    if (activeProjectId) {
      setOpenId(activeProjectId);
      setActiveProjectId(null);
    }
  }, [activeProjectId, setActiveProjectId]);

  const filtered = projects.filter((p) =>
    p.name.toLowerCase().includes(query.toLowerCase()),
  );

  const openProject = projects.find((p) => p.id === openId) ?? null;
  if (openProject) {
    return (
      <>
        <ProjectDetailView
          project={openProject}
          onBack={() => setOpenId(null)}
          onToast={onToast}
          onStartConversation={onStartProjectConversation}
          onOpenSession={onOpenSession}
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
      action: ({ name }) => rename(p.id, name.trim()),
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
      onToast?.("已删除项目");
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
    <div className="project-page">
      {persistError && (
        <div className="project-persist-alert" role="alert">
          <span>项目已保存在本机缓存，但后端落盘失败：{persistError}</span>
          <button type="button" onClick={retryPersist} disabled={persisting}>
            {persisting ? "重试中…" : "重试"}
          </button>
        </div>
      )}
      <section className="project-hero">
        <div className="project-hero__text">
          <h1 className="project-hero__title">项目</h1>
          <p className="project-hero__subtitle">围绕目标组织上下文、对话与交付资产</p>
          <button ref={createButtonRef} type="button" className="project-hero__create" onClick={() => setCreate({})}>
            <AddIcon size="sm" />
            <span>新建项目</span>
          </button>
        </div>
        <img className="project-hero__art" src={heroImg} alt="项目工作台插画" draggable={false} />
      </section>

      <section className="project-section">
        <div className="project-section__head">
          <h3 className="project-section__title">我的项目</h3>
          <div className="project-search">
            <SearchIcon size="sm" className="project-search__icon" />
            <input
              type="text"
              className="project-search__input"
              placeholder="搜索项目"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
            />
          </div>
        </div>
        {filtered.length === 0 ? (
          <div className="project-grid-empty">
            {projects.length === 0
              ? "还没有项目，点击「新建项目」或从下方模板创建。"
              : "没有匹配的项目。"}
          </div>
        ) : (
          <div className="project-grid">
            {filtered.map((p) => (
              <ProjectCard
                key={p.id}
                project={p}
                onEnter={() => setOpenId(p.id)}
                onStart={() => onStartProject?.(p)}
                onRename={() => handleRename(p)}
                onDelete={() => setDeleteTarget(p)}
              />
            ))}
          </div>
        )}
      </section>

      <section className="project-section">
        <div className="project-section__head">
          <h3 className="project-section__title">从模板创建</h3>
        </div>
        <div className="project-grid">
          {FROM_TEMPLATES.map((t) => (
            <button
              key={t.id}
              type="button"
              className="project-card2"
              onClick={() => setCreate({ templateId: t.id })}
            >
              <span className="project-card2__glyph">
                <ProjectGlyph />
              </span>
              <span className="project-card2__body">
                <span className="project-card2__name">{t.title}</span>
                <span className="project-card2__desc">{t.desc}</span>
              </span>
            </button>
          ))}
        </div>
      </section>

      {create && (
        <CreateProjectDialog
          preset={create}
          onCancel={() => setCreate(null)}
          onConfirm={(saved) => {
            setCreate(null);
            setOpenId(saved.id);
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
    </div>
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
          {assetDetail}{assetDetail ? " " : ""}原始文件和历史会话不会被删除。
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
  project, onEnter, onStart, onRename, onDelete,
}: {
  project: ProjectMeta;
  onEnter: () => void;
  onStart: () => void;
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
    <div className="project-card2" ref={ref}>
      <button type="button" className="project-card2__main" onClick={onEnter}>
        <span className="project-card2__glyph"><ProjectGlyph /></span>
        <span className="project-card2__body">
          <span className="project-card2__name">{project.name}</span>
          <span className="project-card2__sub">{addedLabel(project.createdAt)}</span>
        </span>
      </button>
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
          <button type="button" role="menuitem" className="project-card2__menu-item" onClick={() => { closeMenu(); onStart(); }}>开始项目对话</button>
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
    </div>
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
  const [templateId, setTemplateId] = useState<string | undefined>(initial?.id);
  const [instructions, setInstructions] = useState(initial?.instructions ?? "");
  const [connectors, setConnectors] = useState<RefItem[]>(initial?.connectors ?? []);
  const [experts, setExperts] = useState<RefItem[]>(initial?.experts ?? []);
  const [skills, setSkills] = useState<RefItem[]>(initial?.skills ?? []);
  const [modelId, setModelId] = useState<string | undefined>(() => {
    if (defaultModelId && models.some((model) => model.id === defaultModelId)) {
      return defaultModelId;
    }
    return models[0]?.id;
  });
  const [pickerFor, setPickerFor] = useState<null | "connectors" | "experts" | "skills">(null);
  const [tplOpen, setTplOpen] = useState(false);
  const tplRef = useRef<HTMLDivElement>(null);
  const tplTriggerRef = useRef<HTMLButtonElement>(null);
  const tplMenuRef = useRef<HTMLDivElement>(null);
  const tplMenuId = `project-template-${useId().replace(/:/g, "")}`;
  const tplFloating = useAnchoredFloating(tplTriggerRef, tplMenuRef, tplOpen, {
    preferredPlacement: "bottom", align: "end", width: 208,
    estimatedHeight: 244, maxHeight: 320, offset: 4,
    zIndex: "var(--echo-layer-dialog-local)",
  });
  const closeTemplate = () => { setTplOpen(false); tplTriggerRef.current?.focus(); };
  useEffect(() => {
    if (!tplOpen) return;
    (tplMenuRef.current?.querySelector<HTMLButtonElement>('[aria-checked="true"]') ?? tplMenuRef.current?.querySelector<HTMLButtonElement>("button"))?.focus();
    const outside = (event: MouseEvent) => {
      if (!tplRef.current?.contains(event.target as Node) && !tplMenuRef.current?.contains(event.target as Node)) setTplOpen(false);
    };
    const escape = (event: KeyboardEvent) => {
      if (event.key === "Escape" && !event.isComposing && event.keyCode !== 229) {
        event.preventDefault();
        closeTemplate();
      }
    };
    document.addEventListener("mousedown", outside);
    document.addEventListener("keydown", escape);
    return () => {
      document.removeEventListener("mousedown", outside);
      document.removeEventListener("keydown", escape);
    };
  }, [tplOpen]);
  const initialDraft = useRef(JSON.stringify({ instructions, templateId, connectors, experts, skills }));
  const initialModelId = useRef(modelId);
  const { requestClose, closeDialog } = useUnsavedClose({
    dirty: Boolean(name) || modelId !== initialModelId.current
      || JSON.stringify({ instructions, templateId, connectors, experts, skills }) !== initialDraft.current,
    onClose: onCancel,
  });
  const cancel = requestClose;
  const dialogRef = useModalFocus<HTMLDivElement>(true, cancel);

  useEffect(() => {
    if (modelId && models.some((model) => model.id === modelId)) return;
    const next = defaultModelId && models.some((model) => model.id === defaultModelId)
      ? defaultModelId
      : models[0]?.id;
    initialModelId.current = next;
    setModelId(next);
  }, [defaultModelId, modelId, models]);

  const applyTemplate = (id: string) => {
    const t = getTemplate(id);
    setTemplateId(id);
    setInstructions(t?.instructions ?? "");
    setConnectors(t?.connectors ?? []);
    setExperts(t?.experts ?? []);
    setSkills(t?.skills ?? []);
    closeTemplate();
  };

  const currentTpl = getTemplate(templateId);

  const setPicked = (k: typeof pickerFor, items: RefItem[]) => {
    if (k === "connectors") setConnectors(items);
    else if (k === "experts") setExperts(items);
    else if (k === "skills") setSkills(items);
    setPickerFor(null);
  };

  const submit = () => {
    if (!name.trim()) return;
    const saved = add({
      name: name.trim(),
      cwd,
      templateId,
      instructions: instructions.trim() || undefined,
      defaultModelId: modelId,
      connectors, experts, skills,
    });
    onConfirm(saved);
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
              maxLength={15}
              onChange={(e) => setName(e.target.value)}
              placeholder="请输入项目名称"
              data-modal-initial-focus
            />
          </div>

          <div className="create-colleague-field">
            <label className="create-colleague-label" htmlFor="create-project-model">默认执行模型</label>
            {models.length > 0 ? (
              <select
                id="create-project-model"
                className="create-colleague-input proj-model-native-select"
                value={modelId ?? ""}
                onChange={(event) => setModelId(event.target.value || undefined)}
              >
                {models.map((model) => (
                  <option key={model.id} value={model.id}>{model.label || model.id}</option>
                ))}
              </select>
            ) : (
              <div className="proj-model-empty">
                <span>尚未配置可用模型，项目可以先创建，但执行前需要完成配置。</span>
                {onOpenModelSettings && (
                  <button type="button" onClick={onOpenModelSettings}>前往设置模型</button>
                )}
              </div>
            )}
            <span className="proj-model-field-hint">
              项目中新建的对话、计划和任务将默认使用此模型，之后可随时调整。
            </span>
          </div>

          <div className="create-colleague-field">
            <div className="proj-field-head">
              <label className="create-colleague-label" htmlFor="create-project-instructions">指令</label>
              <div className="proj-tpl-select" ref={tplRef}>
                <button type="button" ref={tplTriggerRef} className="proj-tpl-select__btn" aria-expanded={tplOpen} aria-haspopup="menu" aria-controls={tplMenuId} onKeyDown={(event) => { if (["ArrowDown", "ArrowUp"].includes(event.key) && !event.nativeEvent.isComposing) { event.preventDefault(); setTplOpen(true); } }} onClick={() => setTplOpen((v) => !v)}>
                  {currentTpl && currentTpl.id !== "custom" ? currentTpl.title : "选择模板"}
                  <ChevronDownIcon size="sm" />
                </button>
                {tplOpen && createPortal(
                  <div id={tplMenuId} ref={tplMenuRef} className="proj-tpl-select__menu" style={tplFloating.style} role="menu" aria-label="项目模板" onClick={(event) => event.stopPropagation()} onKeyDown={(event) => {
                    if (event.nativeEvent.isComposing || event.keyCode === 229) return;
                    if (event.key === "Escape") { event.preventDefault(); event.stopPropagation(); closeTemplate(); return; }
                    const buttons = Array.from(tplMenuRef.current?.querySelectorAll<HTMLButtonElement>("button") ?? []);
                    const index = buttons.indexOf(document.activeElement as HTMLButtonElement);
                    const next = event.key === "ArrowDown" ? (index + 1) % buttons.length : event.key === "ArrowUp" ? (index - 1 + buttons.length) % buttons.length : event.key === "Home" ? 0 : event.key === "End" ? buttons.length - 1 : -1;
                    if (next >= 0) { event.preventDefault(); buttons[next]?.focus(); }
                  }}>
                    {TEMPLATE_OPTIONS.map((t) => (
                      <button
                        key={t.id}
                        type="button"
                        role="menuitemradio"
                        aria-checked={t.id === (templateId ?? "custom")}
                        className={`proj-tpl-select__item${t.id === templateId ? " proj-tpl-select__item--on" : ""}`}
                        onClick={() => applyTemplate(t.id)}
                      >
                        {t.title}
                      </button>
                    ))}
                  </div>, document.body,
                )}
              </div>
            </div>
            <textarea
              id="create-project-instructions"
              className="create-colleague-textarea"
              value={instructions}
              onChange={(e) => setInstructions(e.target.value)}
              rows={5}
              placeholder="提供当前项目的背景信息和规范，让 EchoAgent 的回复更精准、更符合要求。比如：项目目标、团队习惯、风格偏好、输出约束等"
            />
          </div>

          <ConfigRow label="连接器" items={connectors} onAdd={() => setPickerFor("connectors")} onRemove={(id) => setConnectors((p) => p.filter((x) => x.id !== id))} />
          <ConfigRow label="专家" items={experts} onAdd={() => setPickerFor("experts")} onRemove={(id) => setExperts((p) => p.filter((x) => x.id !== id))} />
          <ConfigRow label="技能" items={skills} onAdd={() => setPickerFor("skills")} onRemove={(id) => setSkills((p) => p.filter((x) => x.id !== id))} />
        </div>

        <div className="create-colleague-footer create-project-footer">
          <span className="proj-version-note">切换模板将替换指令及连接器、专家和技能配置。</span>
          <button className="btn btn--ghost" onClick={cancel}>取消</button>
          <button className="btn btn--primary" onClick={submit} disabled={!name.trim()}>确定</button>
        </div>
      </div>

      {closeDialog}
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
