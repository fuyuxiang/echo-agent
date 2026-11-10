import { isRemovedModelId } from "@/lib/retired-models";
import { create } from "zustand";
import { projectsLoad, projectsSave, type PermissionMode } from "@/lib/agent-client";
import type { SessionSummary } from "@/lib/types";

/**
 * 本地「项目」实体存储。Rust 后端私有数据目录是权威副本，
 * localStorage 只作冷启动缓存与旧版数据迁移来源。每次变更都会原子写回后端。
 */

export interface RefItem {
  id: string;
  name: string;
  iconUrl?: string;
  scope?: string;
}

/** 项目下的真实对话（EchoAgent 会话）。 */
export interface ProjectConversation {
  sessionId: string;
  title: string;
  createdAt: string;
  /** Concrete local catalog model bound to this conversation. */
  modelId?: string;
  /** Mirrors the session sidecar flag so project links never open a hidden row. */
  archived?: boolean;
  /** An existing task moved into this project receives its project contract on its next turn. */
  pendingProjectContext?: boolean;
}

export type ProjectTab = "activity" | "task" | "asset";

interface ProjectDraftPermissionSelection {
  mode: PermissionMode;
}

export type PlanStatus = "pending" | "in_progress" | "paused" | "completed";

export interface PlanCard {
  id: string;
  title: string;
  status: PlanStatus;
  source?: string;
  /** Explicit model override. Missing means the project default is inherited until execution. */
  modelId?: string;
  /** Agent conversation created to execute this plan item. */
  sessionId?: string;
  sessionArchived?: boolean;
}

export interface TaskItem {
  id: string;
  title: string;
  scope: "personal" | "shared";
  source: string;
  status: PlanStatus;
  /** Explicit model override. Missing means the project default is inherited until execution. */
  modelId?: string;
  /** Agent conversation created to execute this task. */
  sessionId?: string;
  sessionArchived?: boolean;
}

export interface AssetItem {
  id: string;
  name: string;
  kind: "folder" | "file";
  ext?: string;
  sizeLabel?: string;
  updater?: string;
  updatedAt?: string;
  /** Canonical file copied into the project's private backend asset dir. */
  path?: string;
  sizeBytes?: number;
}

export interface ProjectMeta {
  id: string;
  name: string;
  cwd?: string;
  templateId?: string;
  instructions?: string;
  /** Preferred model for new project conversations, plans and tasks. */
  defaultModelId?: string;
  createdAt: string;
  // 详情
  connectors: RefItem[];
  experts: RefItem[];
  skills: RefItem[];
  /** Read-only migration source; new writes use tasks. */
  plans: PlanCard[];
  legacyPlans?: PlanCard[];
  schemaVersion?: 2;
  tasks: TaskItem[];
  assets: AssetItem[];
  /** Legacy local participant notes, preserved when saving existing projects. */
  members: string[];
  /** 项目下的真实对话（EchoAgent 会话列表），按创建时间倒序。 */
  conversations: ProjectConversation[];
}

export function projectOwnsSession(project: ProjectMeta, sessionId: string): boolean {
  return project.conversations.some(item => item.sessionId === sessionId)
    || project.tasks.some(item => item.sessionId === sessionId)
    || project.plans.some(item => item.sessionId === sessionId);
}

/** 计划看板列定义（对齐目标截图：待开始/进行中/暂停/完成）。 */
export const PLAN_COLUMNS: { status: PlanStatus; label: string }[] = [
  { status: "pending", label: "待开始" },
  { status: "in_progress", label: "进行中" },
  { status: "paused", label: "暂停" },
  { status: "completed", label: "已验收" },
];

const STORAGE_KEY = "echoagent.projects";
const DIRTY_KEY = "echoagent.projects.pending-backend-sync";
let persistChain: Promise<void> = Promise.resolve();
let persistRevision = 0;

/** Await the current native catalogue before granting newly imported assets. */
export function flushProjectPersistence(): Promise<void> { return persistChain; }

/** Deterministic, lossless migration: identical ids in the two old lists stay distinct. */
export function migrateProjectWorkItems(project: Pick<ProjectMeta, "tasks" | "plans">): TaskItem[] {
  const tasks = (Array.isArray(project.tasks) ? project.tasks : []).map((item) => ({ ...item, status: item.status ?? "pending" }));
  for (const plan of Array.isArray(project.plans) ? project.plans : []) {
    let id = `legacy-plan:${plan.id}`;
    while (tasks.some((task) => task.id === id)) id = `legacy-plan:${id}`;
    tasks.push({ ...plan, id, status: plan.status ?? "pending", scope: "personal", source: plan.source ?? "legacy-plan" });
  }
  return tasks;
}

function clearRemovedModelBinding<T extends { modelId?: string }>(item: T): T {
  return typeof item.modelId === "string" && isRemovedModelId(item.modelId) ? { ...item, modelId: undefined } : item;
}

/** 旧数据/外部数据补齐缺省详情字段，保证组件可直接读数组。 */
function normalize(x: unknown): ProjectMeta | null {
  if (!x || typeof x !== "object") return null;
  const o = x as Partial<ProjectMeta> & { id?: unknown; name?: unknown };
  if (typeof o.id !== "string" || typeof o.name !== "string") return null;
  return {
    id: o.id,
    name: o.name,
    cwd: o.cwd,
    templateId: o.templateId,
    instructions: o.instructions,
    defaultModelId: typeof o.defaultModelId === "string" && !isRemovedModelId(o.defaultModelId) ? o.defaultModelId : undefined,
    createdAt: o.createdAt ?? new Date().toISOString(),
    connectors: Array.isArray(o.connectors) ? o.connectors : [],
    experts: Array.isArray(o.experts) ? o.experts : [],
    skills: Array.isArray(o.skills) ? o.skills : [],
    plans: [],
    legacyPlans: o.legacyPlans ?? (Array.isArray(o.plans) && o.plans.length ? o.plans : undefined),
    schemaVersion: 2,
    tasks: migrateProjectWorkItems({ tasks: o.tasks ?? [], plans: o.plans ?? [] }).map(clearRemovedModelBinding),
    assets: Array.isArray(o.assets) ? o.assets : [],
    members: Array.isArray(o.members) ? o.members : [],
    conversations: Array.isArray(o.conversations) ? o.conversations.map(clearRemovedModelBinding) : [],
  };
}

function load(): ProjectMeta[] {
  if (typeof window === "undefined") return [];
  try {
    const raw = window.localStorage.getItem(STORAGE_KEY);
    if (!raw) return [];
    const arr = JSON.parse(raw);
    return Array.isArray(arr) ? arr.map(normalize).filter(Boolean) as ProjectMeta[] : [];
  } catch {
    return [];
  }
}

function saveLocal(list: ProjectMeta[]) {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(STORAGE_KEY, JSON.stringify(list));
  } catch {
    /* quota / 隐私模式 — 静默降级为仅内存 */
  }
}

function persist(list: ProjectMeta[]): Promise<void> {
  saveLocal(list);
  try { window.localStorage.setItem(DIRTY_KEY, "1"); } catch { /* cache unavailable */ }
  const revision = ++persistRevision;
  useProjectsStore.setState({ persisting: true });
  // Serialize snapshots so a slower old write can never overwrite a newer one.
  persistChain = persistChain
    .catch(() => {})
    .then(() => projectsSave(list));
  void persistChain.then(() => {
    if (revision !== persistRevision) return;
    try { window.localStorage.removeItem(DIRTY_KEY); } catch { /* cache unavailable */ }
    useProjectsStore.setState({ persisting: false, persistError: null });
  }).catch((error) => {
    if (revision !== persistRevision) return;
    useProjectsStore.setState({
      persisting: false,
      persistError: String(error).replace(/^Error:\s*/, ""),
    });
  });
  return persistChain;
}

const uid = (p: string) => `${p}_${Date.now()}_${Math.random().toString(36).slice(2, 8)}`;

interface ProjectsState {
  projects: ProjectMeta[];
  persisting: boolean;
  persistError: string | null;
  retryPersist: () => void;
  /** Sidebar → ProjectsPanel communication: when set, the panel auto-opens this project. */
  activeProjectId: string | null;
  focusProjectDraft: boolean;
  activeProjectTab: ProjectTab;
  setActiveProjectId: (id: string | null, focusDraft?: boolean, tab?: ProjectTab) => void;
  /** One-shot composer selections, kept in memory across project navigation. */
  draftPermissions: Partial<Record<string, ProjectDraftPermissionSelection>>;
  setDraftPermission: (id: string, mode: PermissionMode) => void;
  consumeDraftPermission: (id: string, selection: ProjectDraftPermissionSelection) => void;
  add: (p: {
    name: string;
    cwd?: string;
    templateId?: string;
    instructions?: string;
    defaultModelId?: string;
    connectors?: RefItem[];
    experts?: RefItem[];
    skills?: RefItem[];
  }) => ProjectMeta;
  rename: (id: string, name: string) => void;
  remove: (id: string) => void;
  updateConfig: (
    id: string,
    patch: Partial<Pick<ProjectMeta, "instructions" | "defaultModelId" | "connectors" | "experts" | "skills">>,
  ) => void;
  addTask: (id: string, title: string, modelId?: string) => void;
  moveTask: (id: string, taskId: string, status: PlanStatus) => void;
  setTaskModel: (id: string, taskId: string, modelId: string) => void;
  linkTaskSession: (id: string, taskId: string, sessionId: string, modelId: string) => void;
  removeTask: (id: string, taskId: string) => void;
  addAsset: (id: string, a: Pick<AssetItem, "name" | "kind"> & Partial<AssetItem>) => void;
  addAssets: (id: string, assets: Array<Pick<AssetItem, "name" | "kind"> & Partial<AssetItem>>) => void;
  removeAsset: (id: string, assetId: string) => void;
  addConversation: (id: string, conv: ProjectConversation) => void;
  removeConversation: (id: string, sessionId: string) => void;
  /** Move a catalog task into one project without changing the runtime session. */
  moveSessionToProject: (id: string, session: SessionSummary) => Promise<void>;
  /** Link a copied session to its source project without cloning work items. */
  associateForkWithSourceProject: (sourceSessionId: string, fork: SessionSummary) => Promise<string | null>;
  /** Remove a session from one project without deleting the underlying history. */
  detachSessionFromProject: (id: string, sessionId: string) => Promise<void>;
  setProjectContextPending: (id: string, sessionId: string, pending: boolean) => void;
  updateConversationTitle: (id: string, sessionId: string, title: string) => void;
  /** Mirror archive state into every project reference to this session. */
  setSessionArchived: (sessionId: string, archived: boolean) => void;
  /** Repair duplicated project link flags from authoritative session rows. */
  reconcileSessionArchiveStates: (
    sessions: Array<Pick<SessionSummary, "sessionId" | "archived">>,
  ) => void;
  /** Remove deleted session links from conversations, plans and tasks. */
  removeSessionReferences: (sessionId: string) => void;
}

export const useProjectsStore = create<ProjectsState>((set, get) => {
  const patch = (id: string, fn: (p: ProjectMeta) => ProjectMeta) => {
    const next = get().projects.map((p) => (p.id === id ? fn(p) : p));
    set({ projects: next });
    persist(next);
  };
  const commitMembership = async (next: ProjectMeta[]): Promise<void> => {
    const previous = get().projects;
    set({ projects: next });
    try {
      await persist(next);
    } catch (error) {
      // A later edit may already contain this move. Only roll back the exact
      // snapshot we submitted; otherwise keep the dirty cache for retry.
      if (get().projects === next) {
        set({ projects: previous });
        saveLocal(previous);
      }
      throw error;
    }
  };
  return {
    projects: load(),
    persisting: false,
    persistError: null,
    retryPersist: () => persist(get().projects),
    activeProjectId: null,
    focusProjectDraft: false,
    activeProjectTab: "activity",
    setActiveProjectId: (id, focusDraft = false, tab = "activity") =>
      set({ activeProjectId: id, focusProjectDraft: focusDraft, activeProjectTab: tab }),
    draftPermissions: {},
    setDraftPermission: (id, mode) =>
      set(state => ({ draftPermissions: { ...state.draftPermissions, [id]: { mode } } })),
    consumeDraftPermission: (id, selection) => set(state => {
      if (state.draftPermissions[id] !== selection) return state;
      const draftPermissions = { ...state.draftPermissions };
      delete draftPermissions[id];
      return { draftPermissions };
    }),
    add: (p) => {
      const item: ProjectMeta = {
        id: uid("proj"),
        name: p.name,
        cwd: p.cwd || undefined,
        templateId: p.templateId || undefined,
        instructions: p.instructions || undefined,
        defaultModelId: p.defaultModelId || undefined,
        createdAt: new Date().toISOString(),
        connectors: p.connectors ?? [],
        experts: p.experts ?? [],
        skills: p.skills ?? [],
        plans: [],
        tasks: [],
        assets: [],
        members: [],
        conversations: [],
      };
      const next = [item, ...get().projects];
      set({ projects: next });
      persist(next);
      return item;
    },
    rename: (id, name) => patch(id, (p) => ({ ...p, name })),
    remove: (id) => {
      const next = get().projects.filter((p) => p.id !== id);
      const draftPermissions = { ...get().draftPermissions };
      delete draftPermissions[id];
      set({ projects: next, draftPermissions });
      persist(next);
    },
    updateConfig: (id, cfg) => patch(id, (p) => ({ ...p, ...cfg })),
    addTask: (id, title, modelId) =>
      patch(id, (p) => ({
        ...p,
        tasks: [
          ...p.tasks,
          { id: uid("task"), title, scope: "personal", source: "manual", status: "pending", modelId },
        ],
      })),
    moveTask: (id, taskId, status) =>
      patch(id, (p) => ({
        ...p,
        tasks: p.tasks.map((task) => task.id === taskId ? { ...task, status } : task),
      })),
    setTaskModel: (id, taskId, modelId) =>
      patch(id, (p) => ({
        ...p,
        tasks: p.tasks.map((task) => task.id === taskId ? { ...task, modelId } : task),
      })),
    linkTaskSession: (id, taskId, sessionId, modelId) =>
      patch(id, (p) => ({
        ...p,
        tasks: p.tasks.map((task) => task.id === taskId ? { ...task, sessionId, modelId } : task),
      })),
    removeTask: (id, taskId) =>
      patch(id, (p) => ({ ...p, tasks: p.tasks.filter((t) => t.id !== taskId) })),
    addAsset: (id, a) =>
      patch(id, (p) => ({
        ...p,
        assets: [
          ...p.assets,
          {
            id: uid("asset"),
            name: a.name,
            kind: a.kind,
            ext: a.ext,
            sizeLabel: a.sizeLabel,
            updater: a.updater ?? "-",
            updatedAt: a.updatedAt ?? new Date().toISOString(),
            path: a.path,
            sizeBytes: a.sizeBytes,
          },
        ],
      })),
    addAssets: (id, assets) => {
      if (assets.length === 0) return;
      const now = new Date().toISOString();
      patch(id, (p) => ({
        ...p,
        assets: [
          ...p.assets,
          ...assets.map((asset) => ({
            id: uid("asset"),
            name: asset.name,
            kind: asset.kind,
            ext: asset.ext,
            sizeLabel: asset.sizeLabel,
            updater: asset.updater ?? "-",
            updatedAt: asset.updatedAt ?? now,
            path: asset.path,
            sizeBytes: asset.sizeBytes,
          })),
        ],
      }));
    },
    removeAsset: (id, assetId) =>
      patch(id, (p) => ({ ...p, assets: p.assets.filter((a) => a.id !== assetId) })),
    addConversation: (id, conv) =>
      patch(id, (p) => ({
        ...p,
        conversations: [conv, ...p.conversations],
      })),
    removeConversation: (id, sessionId) =>
      patch(id, (p) => ({
        ...p,
        conversations: p.conversations.filter((c) => c.sessionId !== sessionId),
      })),
    moveSessionToProject: async (id, session) => {
      if (!session.sessionId || session.hidden || session.sessionKind === "subagent") {
        throw new Error("该会话不能移入项目");
      }
      const projects = get().projects;
      const target = projects.find((project) => project.id === id);
      if (!target) throw new Error("目标项目不存在");
      if (projects.some(project => projectOwnsSession(project, session.sessionId))) {
        throw new Error("该会话已归属项目，请先移出原项目");
      }
      const now = new Date().toISOString();
      const next = projects.map((project) => project.id === id ? {
        ...project,
        conversations: [{
          sessionId: session.sessionId,
          title: session.title || "未命名会话",
          createdAt: now,
          modelId: session.currentModelId,
          archived: !!session.archived,
          pendingProjectContext: true,
        }, ...project.conversations],
      } : project);
      await commitMembership(next);
    },
    associateForkWithSourceProject: async (sourceSessionId, fork) => {
      if (!fork.sessionId || fork.sessionId === sourceSessionId || fork.hidden || fork.sessionKind === "subagent") {
        throw new Error("分叉会话标识无效");
      }
      const projects = get().projects;
      const sources = projects.filter(project => projectOwnsSession(project, sourceSessionId));
      if (sources.length === 0) return null;
      if (sources.length !== 1) throw new Error("源会话的项目归属冲突，请先检查项目关联");
      if (projects.some(project => projectOwnsSession(project, fork.sessionId))) {
        throw new Error("分叉会话已归属项目");
      }
      const source = sources[0];
      const conversation = source.conversations.find((item) => item.sessionId === sourceSessionId);
      const next = projects.map((project) => project.id === source.id ? {
        ...project,
        conversations: [{
          sessionId: fork.sessionId,
          title: fork.title || "未命名会话",
          createdAt: new Date().toISOString(),
          modelId: fork.currentModelId,
          archived: !!fork.archived,
          // Work-item-only legacy references have no context provenance;
          // their new conversation receives the current contract once.
          pendingProjectContext: conversation ? !!conversation.pendingProjectContext : true,
        }, ...project.conversations],
      } : project);
      await commitMembership(next);
      return source.id;
    },
    detachSessionFromProject: async (id, sessionId) => {
      const projects = get().projects;
      const project = projects.find((item) => item.id === id);
      if (!project || !projectOwnsSession(project, sessionId)) throw new Error("会话已不在该项目中");
      const next = projects.map((project) => {
        if (project.id !== id) return project;
        const plans = project.plans.map((plan) => {
          if (plan.sessionId !== sessionId) return plan;
          const nextPlan: PlanCard = { ...plan };
          delete nextPlan.sessionId;
          delete nextPlan.sessionArchived;
          return nextPlan;
        });
        const tasks = project.tasks.map((task) => {
          if (task.sessionId !== sessionId) return task;
          const nextTask: TaskItem = { ...task };
          delete nextTask.sessionId;
          delete nextTask.sessionArchived;
          return nextTask;
        });
        return {
          ...project,
          conversations: project.conversations.filter(
            (conversation) => conversation.sessionId !== sessionId,
          ),
          plans,
          tasks,
        };
      });
      await commitMembership(next);
    },
    setProjectContextPending: (id, sessionId, pending) => {
      const project = get().projects.find((item) => item.id === id);
      if (!project?.conversations.some((item) => item.sessionId === sessionId && !!item.pendingProjectContext !== pending)) return;
      patch(id, (item) => ({
        ...item,
        conversations: item.conversations.map((conversation) => conversation.sessionId === sessionId
          ? { ...conversation, pendingProjectContext: pending }
          : conversation),
      }));
    },
    updateConversationTitle: (id, sessionId, title) =>
      patch(id, (p) => ({
        ...p,
        conversations: p.conversations.map((c) =>
          c.sessionId === sessionId ? { ...c, title } : c,
        ),
      })),
    setSessionArchived: (sessionId, archived) => {
      let changed = false;
      const next = get().projects.map((project) => {
        const needsUpdate = project.conversations.some(
          (item) => item.sessionId === sessionId && !!item.archived !== archived,
        ) || project.plans.some(
          (item) => item.sessionId === sessionId && !!item.sessionArchived !== archived,
        ) || project.tasks.some(
          (item) => item.sessionId === sessionId && !!item.sessionArchived !== archived,
        );
        if (!needsUpdate) return project;
        changed = true;
        return {
          ...project,
          conversations: project.conversations.map((conversation) =>
            conversation.sessionId === sessionId
              ? { ...conversation, archived }
              : conversation,
          ),
          plans: project.plans.map((plan) =>
            plan.sessionId === sessionId
              ? { ...plan, sessionArchived: archived }
              : plan,
          ),
          tasks: project.tasks.map((task) =>
            task.sessionId === sessionId
              ? { ...task, sessionArchived: archived }
              : task,
          ),
        };
      });
      if (!changed) return;
      set({ projects: next });
      persist(next);
    },
    reconcileSessionArchiveStates: (sessions) => {
      const archivedById = new Map(
        sessions
          .filter((session) => session.archived !== undefined)
          .map((session) => [session.sessionId, !!session.archived]),
      );
      if (archivedById.size === 0) return;
      let changed = false;
      const next = get().projects.map((project) => {
        let projectChanged = false;
        const conversations = project.conversations.map((conversation) => {
          const archived = archivedById.get(conversation.sessionId);
          if (archived === undefined || !!conversation.archived === archived) return conversation;
          changed = true;
          projectChanged = true;
          return { ...conversation, archived };
        });
        const plans = project.plans.map((plan) => {
          if (!plan.sessionId) return plan;
          const archived = archivedById.get(plan.sessionId);
          if (archived === undefined || !!plan.sessionArchived === archived) return plan;
          changed = true;
          projectChanged = true;
          return { ...plan, sessionArchived: archived };
        });
        const tasks = project.tasks.map((task) => {
          if (!task.sessionId) return task;
          const archived = archivedById.get(task.sessionId);
          if (archived === undefined || !!task.sessionArchived === archived) return task;
          changed = true;
          projectChanged = true;
          return { ...task, sessionArchived: archived };
        });
        return projectChanged ? { ...project, conversations, plans, tasks } : project;
      });
      if (!changed) return;
      set({ projects: next });
      persist(next);
    },
    removeSessionReferences: (sessionId) => {
      let changed = false;
      const next = get().projects.map((project) => {
        const hasConversation = project.conversations.some((item) => item.sessionId === sessionId);
        const hasPlan = project.plans.some((item) => item.sessionId === sessionId);
        const hasTask = project.tasks.some((item) => item.sessionId === sessionId);
        if (!hasConversation && !hasPlan && !hasTask) return project;
        changed = true;
        const conversations = project.conversations.filter((item) => item.sessionId !== sessionId);
        const plans = project.plans.map((plan) => {
          if (plan.sessionId !== sessionId) return plan;
          const nextPlan: PlanCard = { ...plan };
          delete nextPlan.sessionId;
          delete nextPlan.sessionArchived;
          return nextPlan;
        });
        const tasks = project.tasks.map((task) => {
          if (task.sessionId !== sessionId) return task;
          const nextTask: TaskItem = { ...task };
          delete nextTask.sessionId;
          delete nextTask.sessionArchived;
          return nextTask;
        });
        return { ...project, conversations, plans, tasks };
      });
      if (!changed) return;
      set({ projects: next });
      persist(next);
    },
  };
});

/**
 * Reconcile renderer cache with the canonical Rust store once Tauri is ready.
 * A missing backend file triggers a one-time migration from localStorage.
 */
export async function hydrateProjectsFromBackend(): Promise<void> {
  const cached = useProjectsStore.getState().projects;
  const revision = persistRevision;
  let hasPendingSync = false;
  try { hasPendingSync = window.localStorage.getItem(DIRTY_KEY) === "1"; } catch { /* cache unavailable */ }
  if (hasPendingSync) {
    // Use the normal write queue: a startup retry must never finish after and
    // overwrite a more recent user edit or clear its dirty-cache marker.
    await persist(cached);
    return;
  }
  const backend = await projectsLoad<ProjectMeta>();
  const latest = useProjectsStore.getState().projects;
  const normalized = backend.map(normalize).filter(Boolean) as ProjectMeta[];
  if (revision !== persistRevision || latest !== cached) {
    // Startup is asynchronous and the cached project UI is already usable.
    // Rebase only edits made since the read began, retaining backend-only rows
    // and fields while honoring new/deleted projects and explicit local edits.
    const baseById = new Map(cached.map((project) => [project.id, project]));
    const localById = new Map(latest.map((project) => [project.id, project]));
    const backendById = new Map(normalized.map((project) => [project.id, project]));
    const merged: ProjectMeta[] = [];
    for (const local of latest) {
      const base = baseById.get(local.id);
      const stored = backendById.get(local.id);
      if (!base) { merged.push(local); continue; }
      if (!stored) {
        if (JSON.stringify(base) !== JSON.stringify(local) || backend.length === 0) merged.push(local);
        continue;
      }
      const result = { ...stored } as Record<string, unknown>;
      const initial = base as unknown as Record<string, unknown>;
      const updated = local as unknown as Record<string, unknown>;
      for (const key of new Set([...Object.keys(initial), ...Object.keys(updated)])) {
        if (JSON.stringify(initial[key]) === JSON.stringify(updated[key])) continue;
        if (key in updated) result[key] = updated[key];
        else delete result[key];
      }
      merged.push(result as unknown as ProjectMeta);
    }
    for (const stored of normalized) {
      if (!localById.has(stored.id) && !baseById.has(stored.id)) merged.push(stored);
    }
    useProjectsStore.setState({ projects: merged });
    await persist(merged);
    return;
  }
  if (backend.length === 0 && cached.length > 0) {
    await persist(cached);
    return;
  }
  useProjectsStore.setState({ projects: normalized });
  saveLocal(normalized);
}
