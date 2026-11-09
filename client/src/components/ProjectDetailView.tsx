import { findModelChoiceLabel, findModelDisplayName } from "@/lib/model-presentation";
import { PageToolbar } from "./PageToolbar";
import { useSessionsStore } from "@/stores/sessions-store";
import { useKnowledgeStore } from "@/stores/knowledge-store";
/**
 * 项目详情页 — 对齐目标截图（图3-6）。
 *
 *  布局: 顶部面包屑(📁 项目 / 名) + 右上「邀请」 + tab 栏(动态/计划/任务/资产)
 *        + 右侧「项目配置」栏(指令/连接器/专家/技能/自动化) + 底部项目级 Composer。
 *  项目元数据由 Rust 数据文件持久；计划/任务可编辑，资产是已复制
 *  到项目私有目录的真实文件，专家/技能/MCP 选项来自当前运行时。
 */
import { useCallback, useEffect, useRef, useState } from "react";
import {
  useProjectsStore,
  type ProjectMeta,
  type RefItem,
} from "@/stores/projects-store";
import {
  ConfigRow,
  RefPickerDialog,
  type ProjectPickerOptions,
} from "./project-picker";
import { ActivityTab, TaskTab, AssetsTab } from "./project-tabs";
import { Composer } from "./Composer";
import { ModelSelector, type ModelOption } from "./ModelSelector";
import { FolderIcon } from "@/foundation/components/Icon/icons";
import { useModalFocus } from "@/lib/use-modal-focus";
import { useAppDialog } from "./AppDialog";
import type { AgentEntry } from "@/lib/types";
import type { SlashCommandInvocation } from "@/lib/slash-commands";
import { useWorkspaceMentions } from "@/lib/use-workspace-mentions";
import { projectAssetsUse, filesystemAttachmentStats, type WorkspaceInfo } from "@/lib/agent-client";
import { friendlyError, isTaskWorkspaceError } from "@/lib/error-format";
import { draftAttachments } from "@/lib/draft-attachments";
import { draftRevision, touchDraft } from "@/lib/draft-lifecycle";
import { displayPath } from "@/lib/display-path";
import { hasRelativeWorkspaceMention } from "@/lib/at-commands";
import type { StartProjectConversation } from "@/lib/project-context";
import { selectProjectPermissionMode, usePermissionModeStore } from "@/stores/permission-mode-store";
import { flushProjectPersistence } from "@/stores/projects-store";
import "@/styles/project-context.css";

type TabKey = "activity" | "task" | "asset";
type DrawerKey = "instruction" | "model" | "connectors" | "experts" | "skills" | "automation";

const TABS: { key: TabKey; label: string }[] = [
  { key: "activity", label: "动态" },
  { key: "task", label: "工作项" },
  { key: "asset", label: "项目资料" },
];

const CONFIG_CARDS: { key: DrawerKey; title: string; desc: string }[] = [
  { key: "instruction", title: "指令", desc: "设定项目背景与规范，让 AI 与你高效协作" },
  { key: "model", title: "模型", desc: "选择项目对话和任务默认使用的模型" },
  { key: "connectors", title: "连接器", desc: "连接外部服务，扩展 AI 能力" },
  { key: "experts", title: "专家", desc: "配置项目专家，为成员提供更专业的服务" },
  { key: "skills", title: "技能", desc: "配置项目技能，让 AI 精准执行任务" },
  { key: "automation", title: "定时任务", desc: "让 AI 按计划自动执行任务" },
];

export function ProjectDetailView({
  project,
  draftFocusRequest,
  workspaces = [],
  onBack,
  onToast,
  onStartConversation,
  onOpenSession,
  onRenameSession,
  onArchiveSession,
  onDeleteSession,
  picker,
  onOpenAutomation,
  models = [],
  defaultModelId,
  onOpenModelSettings,
  onClientSlashCommand,
  onNavigateConnectors,
  onOpenKnowledgeBase,
  onOpenMeetingMinutes,
  onOpenOrganization,
}: {
  project: ProjectMeta;
  draftFocusRequest?: number;
  workspaces?: WorkspaceInfo[];
  onBack: () => void;
  onToast?: (msg: string) => void;
  /** Start a new conversation within this project (creates a real EchoAgent session). */
  onStartConversation?: StartProjectConversation;
  onOpenSession?: (sessionId: string, cwd?: string) => void;
  onRenameSession?: (sessionId: string, title: string, cwd?: string) => Promise<void>;
  onArchiveSession?: (sessionId: string, archived: boolean, cwd?: string) => Promise<void>;
  onDeleteSession?: (sessionId: string, cwd?: string) => Promise<void>;
  picker: { options: ProjectPickerOptions; loading: boolean; error: string | null };
  onOpenAutomation?: () => void;
  models?: ModelOption[];
  /** Application default, used only by projects that do not yet have a persisted choice. */
  defaultModelId?: string;
  onOpenModelSettings?: () => void;
  onClientSlashCommand?: (invocation: SlashCommandInvocation) => boolean | void | Promise<boolean | void>;
  onNavigateConnectors?: () => void;
  onOpenKnowledgeBase?: () => void;
  onOpenMeetingMinutes?: (modelId?: string) => void;
  onOpenOrganization?: () => void;
}) {
  // 读最新（交互后 store 更新，父传入的快照可能过期）。
  const live = useProjectsStore((s) => s.projects.find((p) => p.id === project.id)) ?? project;
  const updateConfig = useProjectsStore((s) => s.updateConfig);
  const addMember = useProjectsStore((s) => s.addMember);

  const selectedKnowledge = useKnowledgeStore(state => state.defaultSources);
  const draftKey = `project:${project.id}`;
  const workspaceDraftKey = `project-workspace:${project.id}`;
  const assetDraftKey = `project-assets:${project.id}`;
  const projectDraft = useSessionsStore((state) => state.drafts[draftKey] ?? "");
  const workspaceOverride = useSessionsStore((state) => state.drafts[workspaceDraftKey]);
  const taskCwd = workspaceOverride || live.cwd;
  const selectedAssetDraft = useSessionsStore(state => state.drafts[assetDraftKey] ?? "[]");
  let selectedAssetIds: string[] = [];
  try { const parsed: unknown = JSON.parse(selectedAssetDraft); if (Array.isArray(parsed)) selectedAssetIds = parsed.filter((id): id is string => typeof id === "string"); } catch { /* An invalid optional selection never changes the text draft. */ }
  const selectedAssets = live.assets.filter(asset => selectedAssetIds.includes(asset.id) && asset.kind === "file" && asset.path);
  const selectAssets = (ids: string[]) => {
    touchDraft(assetDraftKey);
    useSessionsStore.getState().setDraft(assetDraftKey, ids.length ? JSON.stringify(ids) : "");
  };
  const selectAsset = (id: string) => selectAssets([...new Set([...selectedAssetIds, id])]);
  const prepareProjectAttachments = async (attachments: string[] = []) => {
    if (selectedAssetIds.some(id => !selectedAssets.some(asset => asset.id === id))) throw new Error("部分已选项目资料已删除或没有文件，请移除失效选择后重试");
    await flushProjectPersistence();
    const granted = selectedAssets.length ? await projectAssetsUse(live.id, selectedAssets.map(asset => asset.path!)) : [];
    const all = [...new Set([...attachments, ...granted])];
    if (all.length > 20) throw new Error("本次附件与项目资料合计不能超过20个文件");
    if (granted.length) {
      const stats = await filesystemAttachmentStats(all);
      if (stats.rejected.length) throw new Error(stats.rejected.map(file => file.reason).join("；"));
      if (stats.files.length !== all.length || stats.files.reduce((bytes, file) => bytes + file.sizeBytes, 0) > 64 * 1024 * 1024) throw new Error("本次附件与项目资料总大小不能超过64MB");
    }
    return all;
  };
  const [tab, setTab] = useState<TabKey>("activity");
  const [drawer, setDrawer] = useState<DrawerKey | null>(null);
  const [membersOpen, setMembersOpen] = useState(false);
  const [pickerFor, setPickerFor] = useState<null | "connectors" | "experts" | "skills">(null);
  // Composer 失败原因。Composer 内部不持有"失败 alert",必须由调用方展示,
  // 否则用户看到"按了发送但什么都没发生"。
  const [sendError, setSendError] = useState<string | null>(null);
  const [workspaceError, setWorkspaceError] = useState<{ cwd?: string; message: string }>();
  const [composerExpert, setComposerExpert] = useState<AgentEntry | null>(null);
  const { requestInput, requestConfirmation, dialog } = useAppDialog(live.id);
  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; };
  }, []);
  const projectPermissionMode = usePermissionModeStore(selectProjectPermissionMode);
  const mentionCandidates = useWorkspaceMentions(taskCwd);
  const configuredProjectModelAvailable = !live.defaultModelId
    || models.some((model) => model.id === live.defaultModelId);
  const inheritedModelId = defaultModelId && models.some((model) => model.id === defaultModelId)
    ? defaultModelId
    : models[0]?.id;
  const projectModelId = live.defaultModelId
    ? configuredProjectModelAvailable ? live.defaultModelId : undefined
    : inheritedModelId;
  const setProjectModel = (modelId: string) => {
    updateConfig(live.id, { defaultModelId: modelId });
  };

  const addComposerExpert = (agent: AgentEntry) => {
    const item: RefItem = {
      id: agent.path || agent.name,
      name: agent.name,
      scope: agent.scope,
    };
    if (!live.experts.some((expert) => expert.id === item.id || expert.name === item.name)) {
      updateConfig(live.id, { experts: [...live.experts, item] });
    }
    setComposerExpert(agent);
  };

  const setPicked = (k: typeof pickerFor, items: RefItem[]) => {
    if (!k) return;
    if (k === "connectors") updateConfig(live.id, { connectors: items });
    else if (k === "experts") updateConfig(live.id, { experts: items });
    else updateConfig(live.id, { skills: items });
    setPickerFor(null);
  };

  const invite = () => {
    requestInput({
      title: "添加参与者备注",
      description: "可以填写姓名、职责或角色。该备注仅保存在本机项目中。",
      fields: [{ name: "name", label: "姓名或角色", required: true, maxLength: 100 }],
      confirmLabel: "添加",
      action: ({ name }) => {
        const trimmed = name.trim();
        addMember(live.id, trimmed);

        setMembersOpen(false);
      },
    });
  };

  const selectWorkspace = (cwd: string, trigger?: HTMLButtonElement | null) => {
    if (!mounted.current) return;
    const next = cwd || live.cwd;
    const apply = () => {
      if (next !== taskCwd) touchDraft(workspaceDraftKey);
      useSessionsStore.getState().setDraft(workspaceDraftKey, next === live.cwd ? "" : cwd);
      setWorkspaceError(undefined);
    };
    if (next === taskCwd || !hasRelativeWorkspaceMention(useSessionsStore.getState().drafts[draftKey] ?? "")) {
      apply();
      return;
    }
    requestConfirmation({
      title: "更改草稿的工作目录？",
      description: <>从「{taskCwd ? displayPath(taskCwd) : "默认任务目录"}」切换至「{next ? displayPath(next) : "默认任务目录"}」。草稿中的相对 @ 引用会按新目录解释，请在发送前核对这些引用。</>,
      confirmLabel: "切换目录",
      returnFocus: trigger,
      action: apply,
    });
  };

  const runProjectConversation = async (
    text: string,
    modelId: string,
    attachments: string[],
    fromDraft: boolean,
  ): Promise<string | undefined> => {
    if (!onStartConversation) {
      onToast?.("当前项目无法启动会话");
      return;
    }
    if (projectPermissionMode === undefined) return;
    setSendError(null);
    setWorkspaceError(undefined);
    const submittedRevision = draftRevision(draftKey);
    const workspaceRevision = draftRevision(workspaceDraftKey);
    const assetRevision = draftRevision(assetDraftKey);
    try {
      const sessionId = await onStartConversation(live.id, text, modelId, {
        cwd: taskCwd ?? null,
        expectedPermissionMode: projectPermissionMode,
        prepareAttachments: () => prepareProjectAttachments(attachments),
        rememberModel: fromDraft,
      });
      if (!sessionId) {
        setSendError(fromDraft ? "消息尚未发送，请查看提示后重试。" : "工作项尚未启动，请查看提示后重试。");
      } else {
        const drafts = useSessionsStore.getState();
        const draftUnchanged = draftRevision(draftKey) === submittedRevision;
        const hasSeparateDraft = !fromDraft && Boolean(drafts.drafts[draftKey]?.trim() || draftAttachments(draftKey).length);
        if (!hasSeparateDraft && draftUnchanged) {
          if (draftRevision(workspaceDraftKey) === workspaceRevision) drafts.setDraft(workspaceDraftKey, "");
          if (draftRevision(assetDraftKey) === assetRevision) drafts.setDraft(assetDraftKey, "");
        }
      }
      return sessionId;
    } catch (error) {
      const failureLabel = fromDraft ? "发送失败" : "启动工作项失败";
      const workspaceFailure = isTaskWorkspaceError(error);
      const detail = workspaceFailure || error instanceof Error ? String(error).replace(/^Error:\s*/, "") : friendlyError(error);
      if (!mounted.current) {
        onToast?.(`项目「${live.name}」${failureLabel}：${detail}`);
        return undefined;
      }
      if (workspaceFailure) {
        const recovery = taskCwd ? "工作目录不可用，请重新选择。" : "默认任务目录无法分配，请选择一个现有文件夹后重试。";
        setWorkspaceError({ cwd: taskCwd, message: `${recovery} ${detail}` });
      } else {
        setSendError(`${failureLabel}：${detail}`);
      }
      return undefined;
    }
  };

  const hasInstructions = typeof live.instructions === "string" && live.instructions.trim().length > 0;
  const hasCapabilities = Boolean(live.experts.length || live.skills.length || live.connectors.length);
  const projectHeaderAria = hasCapabilities
    ? `项目「${live.name}」已配置 ${live.experts.length} 个专家、${live.skills.length} 个技能、${live.connectors.length} 个连接器${hasInstructions ? "，已配置项目指令" : ""}`
    : hasInstructions
      ? `项目「${live.name}」已配置项目指令`
      : `项目「${live.name}」尚未配置指令、专家、技能或连接器`;

  return (
    <div className="pd-page">
      <PageToolbar className="pd-topbar">
        <div className="pd-crumb">
          <FolderIcon size="sm" />
          <button className="pd-crumb__link" onClick={onBack}>项目</button>
          <span className="pd-crumb__sep">/</span>
          <span className="pd-crumb__name">{live.name}</span>
        </div>
        <div className="pd-topbar__right">
          <button className="pd-invite" onClick={() => setMembersOpen((v) => !v)}>参与者</button>
          {membersOpen && (
            <div className="pd-members-pop">
              <div className="pd-members-pop__head">项目参与者（本机备注）</div>
              {live.members.length === 0 ? (
                <div className="pd-members-pop__empty">暂无成员</div>
              ) : (
                live.members.map((m) => (
                  <div className="pd-members-pop__item" key={m}>{m}</div>
                ))
              )}
              <button className="pd-members-pop__add" onClick={invite}>+ 添加参与者</button>
            </div>
          )}
        </div>
      </PageToolbar>

      <div className="pd-body">
        <div className="pd-main">
          <div className="pd-tabs-row">
            <nav className="pd-tabs">
              {TABS.map((t) => (
                <button
                  key={t.key}
                  className={`pd-tab-btn${tab === t.key ? " pd-tab-btn--on" : ""}`}
                  onClick={() => setTab(t.key)}
                >
                  {t.label}
                </button>
              ))}
            </nav>
          </div>

          <div className="pd-tab-content">
            {tab === "activity" && (
              <ActivityTab
                projectId={live.id}
                onOpenSession={onOpenSession}
                onRenameSession={onRenameSession}
                onArchiveSession={onArchiveSession}
                onDeleteSession={onDeleteSession}
                onToast={onToast}
                models={models}
              />
            )}
            {tab === "task" && (
              <TaskTab
                projectId={live.id}
                models={models}
                defaultModelId={projectModelId}
                onRun={onStartConversation ? (message, modelId) => runProjectConversation(message, modelId, [], false) : undefined}
                runDisabled={projectPermissionMode === undefined}
                onOpenSession={onOpenSession ? (sessionId) => onOpenSession(sessionId, live.cwd) : undefined}
                onRestoreSession={onArchiveSession
                  ? (sessionId) => onArchiveSession(sessionId, false, live.cwd)
                  : undefined}
                onToast={onToast}
              />
            )}
            {tab === "asset" && <AssetsTab projectId={live.id} onToast={onToast} onUseAsset={asset => { selectAsset(asset.id); }} selectedAssetIds={selectedAssetIds} />}
          </div>

          <div className="pd-composer-shell">
            <section className="project-task-context" aria-label="本次任务上下文">
              <div><strong>本次任务上下文</strong><span>发送消息或将工作项交给 Agent 时，会使用所选工作目录、项目资料和项目默认偏好。</span></div>
              <div className="project-task-context__chips">{selectedAssets.map(asset => <button type="button" className="project-context-chip" key={asset.id} onClick={() => selectAssets(selectedAssetIds.filter(id => id !== asset.id))} aria-label={`移除本次资料 ${asset.name}`}>{asset.name}<span aria-hidden="true"> ×</span></button>)}</div>
              {selectedAssetIds.length !== selectedAssets.length && <p role="alert">部分资料已失效。<button type="button" onClick={() => selectAssets(selectedAssets.map(asset => asset.id))}>移除失效选择</button></p>}
              {!selectedAssets.length && <p>本次未选用项目资料。<button type="button" onClick={() => setTab("asset")}>从项目资料中选择</button></p>}
              {selectedAssets.length > 0 && <button type="button" onClick={() => selectAssets([])}>清除本次资料选择</button>}
            </section>
            <div
              className="pd-composer-header"
              role="status"
              aria-label={projectHeaderAria}
              title={projectHeaderAria}
            >
              <span className="pd-composer-header__icon" aria-hidden="true">
                <FolderIcon size="sm" />
              </span>
              <span className="pd-composer-header__name">{live.name}</span>
              <span className="pd-composer-header__sep" aria-hidden="true">·</span>
              <span className="pd-composer-header__label">项目默认偏好：</span>
              {hasInstructions && (
                <span className="pd-composer-header__count pd-composer-header__count--active pd-composer-header__instruction">项目指令</span>
              )}
              {!hasCapabilities && !hasInstructions && (
                <span className="pd-composer-header__count">尚未配置任务参考信息</span>
              )}
              {hasCapabilities && (
                <>
                  <span className={`pd-composer-header__count${live.experts.length > 0 ? " pd-composer-header__count--active" : ""}`}>
                    {live.experts.length} 专家
                  </span>
                  <span className="pd-composer-header__sep" aria-hidden="true">·</span>
                  <span className={`pd-composer-header__count${live.skills.length > 0 ? " pd-composer-header__count--active" : ""}`}>
                    {live.skills.length} 技能
                  </span>
                  <span className="pd-composer-header__sep" aria-hidden="true">·</span>
                  <span className={`pd-composer-header__count${live.connectors.length > 0 ? " pd-composer-header__count--active" : ""}`}>
                    {live.connectors.length} 连接器
                  </span>
                </>
              )}
            </div>
            <Composer
              draftKey={draftKey}
              draft={projectDraft}
              focusRequest={draftFocusRequest}
              onDraftChange={(value) => useSessionsStore.getState().setDraft(draftKey, value)}
              streaming={false}
              onSend={async (text, attachments) => Boolean(await runProjectConversation(text, projectModelId!, attachments ?? [], true))}
              sendDisabled={projectPermissionMode === undefined}
              onCancel={() => { /* 项目页 composer 不流式,onCancel 为 Composer prop 必传的占位。 */ }}
              placeholder="描述项目任务，可添加文件作为参考（Shift+Enter 换行）"
              apiReady={!!projectModelId && models.length > 0}
              setupHint={
                models.length === 0
                  ? "请先在「设置 → 模型」配置模型"
                  : !projectModelId && live.defaultModelId
                    ? `原项目模型「${findModelDisplayName(models, live.defaultModelId)}」已不可用，请重新选择`
                    : undefined
              }
              onOpenSettings={onOpenModelSettings}
              onToast={onToast}
              modelId={projectModelId}
              modelLoading={false}
              models={models}
              onModelChange={setProjectModel}
              cwd={taskCwd}
              workspaces={workspaces}
              onSelectWorkspace={selectWorkspace}
              projectWorkspaceDefault={{ cwd: live.cwd }}
              workspaceError={workspaceError?.cwd === taskCwd ? workspaceError?.message : undefined}
              automaticWorkspace={!taskCwd}
              showMeta
              compact
              inheritTaskPermission
              onSelectExpert={addComposerExpert}
              activeExpertName={composerExpert?.name}
              onDismissExpert={() => setComposerExpert(null)}
              onNavigateConnectors={onNavigateConnectors}
              onOpenKnowledgeBase={onOpenKnowledgeBase}
              onOpenMeetingMinutes={() => onOpenMeetingMinutes?.(projectModelId)}
              onOpenOrganization={onOpenOrganization}
              onClientSlashCommand={onClientSlashCommand}
              filePaths={mentionCandidates.filePaths}
              workspaceSymbols={mentionCandidates.workspaceSymbols}
            />
            {(sendError || (!projectModelId && live.defaultModelId)) && (
              <div className="pd-composer-warning" role="alert">
                <span>
                  {sendError
                    ?? `原项目模型「${findModelDisplayName(models, live.defaultModelId)}」已不可用，请重新选择。`}
                </span>
                {models.length === 0 && onOpenModelSettings && (
                  <button type="button" onClick={onOpenModelSettings}>
                    前往设置模型
                  </button>
                )}
              </div>
            )}
          </div>
        </div>

        <aside className="pd-side project-context-sidebar">
          <section aria-label="本次能力与知识范围" className="project-context-summary">
            <h3 className="pd-side__title">本次能力与知识范围</h3>
            <p>知识来源：{selectedKnowledge.length ? selectedKnowledge.map(source => source === "personal" ? "个人知识" : "组织授权知识").join("、") : "未选用"}</p>
            <p className="project-context-summary__hint">在输入框左下角调整知识来源；发送前会检查可用性和授权。</p>
            {live.experts.length > 0 && <div><strong>偏好专家</strong>{live.experts.map(expert => <p key={expert.id}>{expert.name}<span className="project-context-scope">{expert.scope === "project" ? "当前项目" : expert.scope === "user" ? "用户专家" : "发送前校验来源"}</span></p>)}</div>}
          </section>
          <details className="project-default-preferences">
            <summary>项目默认偏好<span>指令、模型与能力</span></summary>
            <p>这些偏好会提供给新任务。资料仍需为每次任务单独选择。</p>
            {CONFIG_CARDS.map(c => <button type="button" key={c.key} className="project-default-preference" aria-label={`配置项目默认${c.title}`} onClick={() => setDrawer(c.key)} title={c.desc}><span>{c.title}</span><span>{c.key === "model" ? findModelChoiceLabel(models, projectModelId, "待选择") : c.key === "instruction" ? live.instructions?.trim() ? "已配置" : "未配置" : c.key === "experts" ? `${live.experts.length} 个` : c.key === "skills" ? `${live.skills.length} 个` : c.key === "connectors" ? `${live.connectors.length} 个` : "管理"}</span><span aria-hidden="true">›</span></button>)}
          </details>
        </aside>
      </div>

      {drawer && (
        <ConfigDrawer
          drawer={drawer}
          project={live}
          onClose={() => setDrawer(null)}
          onOpenPicker={(k) => setPickerFor(k)}
          onOpenAutomation={onOpenAutomation}
          models={models}
          modelId={projectModelId}
          unavailableModelId={configuredProjectModelAvailable ? undefined : live.defaultModelId}
          onModelChange={setProjectModel}
          onOpenModelSettings={onOpenModelSettings}
        />
      )}

      {pickerFor && (
        <RefPickerDialog
          title={pickerFor === "connectors" ? "连接器" : pickerFor === "experts" ? "专家" : "技能"}
          options={picker.options[pickerFor]}
          emptyHint={picker.loading ? "正在读取运行时能力…" : picker.error ? `读取失败：${picker.error}` : "当前没有已启用的可选项"}
          selected={pickerFor === "connectors" ? live.connectors : pickerFor === "experts" ? live.experts : live.skills}
          onCancel={() => setPickerFor(null)}
          onConfirm={(items) => setPicked(pickerFor, items)}
        />
      )}
      {dialog}
    </div>
  );
}

// ============================================================
// 配置抽屉
// ============================================================

function ConfigDrawer({
  drawer,
  project,
  onClose,
  onOpenPicker,
  onOpenAutomation,
  models,
  modelId,
  unavailableModelId,
  onModelChange,
  onOpenModelSettings,
}: {
  drawer: DrawerKey;
  project: ProjectMeta;
  onClose: () => void;
  onOpenPicker: (k: "connectors" | "experts" | "skills") => void;
  onOpenAutomation?: () => void;
  models: ModelOption[];
  modelId?: string;
  unavailableModelId?: string;
  onModelChange: (modelId: string) => void;
  onOpenModelSettings?: () => void;
}) {
  const updateConfig = useProjectsStore((s) => s.updateConfig);
  const card = CONFIG_CARDS.find((c) => c.key === drawer)!;
  const [instructionDraft, setInstructionDraft] = useState(project.instructions ?? "");
  useEffect(() => {
    setInstructionDraft(project.instructions ?? "");
  }, [project.id, project.instructions]);
  const closeDrawer = useCallback(() => {
    if (drawer === "instruction" && instructionDraft !== (project.instructions ?? "")) {
      updateConfig(project.id, { instructions: instructionDraft });
    }
    onClose();
  }, [drawer, instructionDraft, onClose, project.id, project.instructions, updateConfig]);
  const dialogRef = useModalFocus<HTMLDivElement>(true, closeDrawer);

  return (
    <div className="modal-overlay" onClick={closeDrawer}>
      <div
        ref={dialogRef}
        className="create-colleague-dialog proj-drawer"
        onClick={(e) => e.stopPropagation()}
        role="dialog"
        aria-modal="true"
        aria-label={`项目配置：${card.title}`}
        tabIndex={-1}
      >
        <div className="create-colleague-header">
          <h3>{card.title}</h3>
          <button
            className="create-colleague-close"
            onClick={closeDrawer}
            aria-label="关闭"
            data-modal-initial-focus={drawer === "instruction" ? undefined : ""}
          >×</button>
        </div>
        <div className="create-colleague-body">
          {drawer === "instruction" && (
            <textarea
              className="create-colleague-textarea"
              rows={8}
              value={instructionDraft}
              maxLength={128_000}
              onChange={(e) => setInstructionDraft(e.target.value)}
              placeholder="填写项目目标、背景、工作规范和交付要求…"
              data-modal-initial-focus
            />
          )}
          {drawer === "model" && (
            <div className="proj-model-setting">
              <label className="create-colleague-label">项目默认模型</label>
              <p className="proj-model-setting__hint">
                新对话、新计划和新任务默认使用此模型；执行前仍可为单条计划或任务单独选择。
              </p>
              <ModelSelector
                ariaLabel="选择项目默认模型"
                onBeforeManageModels={closeDrawer}
                modelId={modelId}
                models={models}
                onModelChange={onModelChange}
                menuPlacement="bottom"
                menuAlign="start"
                menuWidth="anchor"
                menuZIndex="var(--echo-layer-dialog-popover)"
              />
              {unavailableModelId && (
                <p className="proj-model-setting__warning" role="alert">
                  原模型“{findModelDisplayName(models, unavailableModelId)}”已被删除或停用，请重新选择。
                </p>
              )}
              {models.length === 0 && (
                <button
                  type="button"
                  className="btn btn--ghost"
                  onClick={() => {
                    closeDrawer();
                    onOpenModelSettings?.();
                  }}
                >
                  前往设置模型
                </button>
              )}
            </div>
          )}
          {drawer === "connectors" && (
            <ConfigRow label="连接器" items={project.connectors} onAdd={() => onOpenPicker("connectors")} onRemove={(id) => updateConfig(project.id, { connectors: project.connectors.filter((x) => x.id !== id) })} />
          )}
          {drawer === "experts" && (
            <ConfigRow label="专家" items={project.experts} onAdd={() => onOpenPicker("experts")} onRemove={(id) => updateConfig(project.id, { experts: project.experts.filter((x) => x.id !== id) })} />
          )}
          {drawer === "skills" && (
            <ConfigRow label="技能" items={project.skills} onAdd={() => onOpenPicker("skills")} onRemove={(id) => updateConfig(project.id, { skills: project.skills.filter((x) => x.id !== id) })} />
          )}
          {drawer === "automation" && (
            <div className="proj-drawer-empty">
              <p>自动化在统一调度中心配置，可选择本项目目录作为运行工作区。</p>
              <button className="btn btn--ghost" onClick={() => { closeDrawer(); onOpenAutomation?.(); }}>打开自动化中心</button>
            </div>
          )}
        </div>
        <div className="create-colleague-footer">
          <button className="btn btn--primary" onClick={closeDrawer}>完成</button>
        </div>
      </div>
    </div>
  );
}

// ============================================================
// 项目 composer 改用首页 `<Composer>`（src/components/Composer.tsx），
// 原薄壳函数 ProjectComposer 已删除：附件 / 拖拽 / `/` / `@` / 语音 /
// 输入历史 / 知识来源 等能力统一由 Composer 接管；项目级契约（cwd +
// 上下文注入摘要）通过 header chip 与 Composer 的 props 表达。
// ============================================================
