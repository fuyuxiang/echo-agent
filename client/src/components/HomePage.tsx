import { useEffect, useRef, useState } from "react";
import { Composer } from "./Composer";
import type { ModelOption } from "./ModelSelector";
import type { WorkspaceInfo } from "@/lib/agent-client";
import type { AgentEntry } from "@/lib/types";
import { useSessionsStore, HOME_DRAFT_KEY } from "@/stores/sessions-store";
import { usePendingExpertStore } from "@/stores/pending-expert-store";
import type { SlashCommandInvocation } from "@/lib/slash-commands";
import type { AutomationMode } from "@/lib/automation-client";

/** EchoAgent 首页：单一任务入口。 */
export function HomePage({
  onSend,
  streaming,
  apiReady,
  modelLoading = false,
  setupHint,
  creatingSession,
  sendError,
  onOpenSettings,
  onPlaceholder,
  modelId,
  models,
  onModelChange,
  cwd,
  workspaces,
  onSelectWorkspace,
  automaticWorkspace,
  defaultWorkspaceCwd,
  onSelectExpert,
  onNavigateConnectors,
  onOpenKnowledgeBase,
  onOpenMeetingMinutes,
  onOpenOrganization,
  commandRefreshKey,
  onClientSlashCommand,
  taskMode,
  onTaskModeChange,
}: {
  onSend: (text: string, attachments?: string[]) => boolean | void | Promise<boolean | void>;
  streaming: boolean;
  apiReady: boolean;
  modelLoading?: boolean;
  setupHint?: string;
  creatingSession?: boolean;
  sendError?: string | null;
  onOpenSettings: () => void;
  onPlaceholder: (label: string) => void;
  modelId?: string;
  models?: ModelOption[];
  onModelChange?: (id: string) => void;
  cwd?: string;
  workspaces?: WorkspaceInfo[];
  onSelectWorkspace?: (cwd: string) => void;
  automaticWorkspace?: boolean;
  defaultWorkspaceCwd?: string;
  onSelectExpert?: (agent: AgentEntry) => void;
  onNavigateConnectors?: () => void;
  onOpenKnowledgeBase?: () => void;
  onOpenMeetingMinutes?: () => void;
  onOpenOrganization?: () => void;
  commandRefreshKey?: number;
  onClientSlashCommand?: (
    invocation: SlashCommandInvocation,
  ) => boolean | void | Promise<boolean | void>;
  taskMode?: AutomationMode;
  onTaskModeChange?: (mode: AutomationMode) => void;
}) {
  // 受控填充 Composer 的内容 + nonce（召唤专家后写入 quick prompt）。
  const [externalText, setExternalText] = useState("");
  const [externalTextNonce, setExternalTextNonce] = useState(0);
  // 首页草稿(哨兵 key):用户离开首页再回来,未发送的字还在。
  const homeDraft = useSessionsStore((s) => s.drafts[HOME_DRAFT_KEY] ?? "");
  const setDraft = useSessionsStore((s) => s.setDraft);

  // Pending expert (set after "召唤" in the detail modal).
  const pendingExpert = usePendingExpertStore((s) => s.expert);
  const pendingHandledRef = useRef<string | null>(null);

  // When a pending expert arrives with a quickPrompt, pre-fill the composer.
  useEffect(() => {
    if (!pendingExpert) {
      // 已被 dismiss 或未召唤:重置处理记录,使再次召唤同一专家能重新预填。
      pendingHandledRef.current = null;
      return;
    }
    // Only auto-fill once per expert (avoid re-filling on store churn).
    if (pendingHandledRef.current === pendingExpert.expertId) return;
    pendingHandledRef.current = pendingExpert.expertId;
    const currentDraft = useSessionsStore.getState().drafts[HOME_DRAFT_KEY] ?? "";
    if (pendingExpert.quickPrompt && currentDraft.length === 0) {
      fillComposer(pendingExpert.quickPrompt);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [pendingExpert]);

  /** Dismiss the active expert: clear pending store + drop pre-fill guard. */
  const dismissPendingExpert = () => {
    usePendingExpertStore.getState().clear();
    pendingHandledRef.current = null;
  };

  /** 写入 Composer 并聚焦。 */
  const fillComposer = (text: string) => {
    setExternalText(text);
    setExternalTextNonce((n) => n + 1);
  };

  return (
    <div className="home">
      <div className="home__inner">
        <header className="home__header">
          <h1 className="home__title">今天想完成什么？</h1>
          <p className="home__description">描述任务，或添加文件、知识与工具。</p>
        </header>

        <section className="home__composer-area">
          <Composer
            streaming={streaming}
            disabled={creatingSession}
            onSend={onSend}
            onCancel={() => {}}
            apiReady={apiReady}
            modelLoading={modelLoading}
            setupHint={
              modelLoading
                ? "正在同步可用模型…"
                : setupHint ?? (models?.length
                  ? "当前模型暂不可用，请在「设置 → 模型」检查连接。"
                  : "暂无可用模型，请在「设置 → 模型」检查连接。")
            }
            onOpenSettings={modelLoading ? undefined : onOpenSettings}
            onPlaceholder={onPlaceholder}
            externalText={externalText}
            externalTextNonce={externalTextNonce}
            modelId={modelId}
            models={models}
            onModelChange={onModelChange}
            cwd={cwd}
            workspaces={workspaces}
            onSelectWorkspace={onSelectWorkspace}
            automaticWorkspace={automaticWorkspace}
            defaultWorkspaceCwd={defaultWorkspaceCwd}
            showMeta
            draft={homeDraft}
            draftKey={HOME_DRAFT_KEY}
            onDraftChange={(t) => setDraft(HOME_DRAFT_KEY, t)}
            onSelectExpert={onSelectExpert}
            onNavigateConnectors={onNavigateConnectors}
            automationMode={taskMode}
            automationModeDisabled={Boolean(creatingSession) || streaming}
            showAutomationModeBadge
            onAutomationModeChange={onTaskModeChange}
            onOpenKnowledgeBase={onOpenKnowledgeBase}
            onOpenMeetingMinutes={onOpenMeetingMinutes}
            onOpenOrganization={onOpenOrganization}
            commandRefreshKey={commandRefreshKey}
            onClientSlashCommand={onClientSlashCommand}
            activeExpertName={pendingExpert?.name}
            activeExpertAvatar={pendingExpert?.avatarLocal}
            onDismissExpert={dismissPendingExpert}
          />
          {creatingSession && (
            <div className="home__send-status" role="status" aria-live="polite">
              正在创建任务…
            </div>
          )}
          {!creatingSession && sendError && (
            <div className="home__send-error" role="alert">
              任务创建失败，请重试：{sendError}
            </div>
          )}
        </section>
      </div>
    </div>
  );
}
