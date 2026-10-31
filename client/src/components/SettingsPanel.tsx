import { useCallback, useEffect, useRef, useState } from "react";
import {
  Settings as SettingsIcon,
  SlidersHorizontal,
  Brain,
  Cpu,
  Database,
  BarChart3,
  Shield,
  HelpCircle,
  X,
  Cloud,
  Bell,
  MessageCircle,
  type LucideIcon,
} from "lucide-react";
import {
  SettingsSection,
  AgentSettingsPanel,
  DataSettingsPanel,
  GeneralSettingsPanel,
  HelpSettingsPanel,
  NotificationCenterSettingsPanel,
  SecuritySettingsPanel,
  ShortcutsSettingsPanel,
} from "./SettingsSections";
import { UsageQuotaPanel } from "./UsageQuotaPanel";
import { ModelConnectionsPanel } from "./ModelConnectionsPanel";
import { NotifyChannelsPanel } from "./NotifyChannelsPanel";
import { WeixinChannelPanel } from "./WeixinChannelPanel";
import { CloudStoragePanel } from "./CloudStoragePanel";
import { ArchivedSessionsSettingsPanel } from "./ArchivedSessionsSettingsPanel";
import { PersonalMemoryPanel } from "./PersonalMemoryPanel";
import { useModalFocus } from "@/lib/use-modal-focus";
import { useAppDialog } from "./AppDialog";
import { useSessionsStore } from "@/stores/sessions-store";
import { SettingsDraftContext, type SettingsDraftState } from "@/lib/settings-drafts";
import { SettingsSearch } from "./SettingsSearch";

/**
 * EchoAgent-style Settings dialog.
 *
 * Full-screen overlay → centered `.settings-modal` with grouped navigation and
 * a right panel backed by real runtime, config,
 * notification, memory, security, data and appearance settings.
 *
 * The 模型 section delegates to a source-aware connection manager that keeps
 * organization-managed configuration separate from personal API connections.
 */

export type SettingsSectionId =
  | "agent-mail"
  | "notify-channels"
  | "weixin-channel"
  | "general"
  | "agent-settings"
  | "shortcuts"
  | "memory"
  | "personal-memory"
  | "model"
  | "personalize"
  | "archived"
  | "data"
  | "usage"
  | "cloud-storage"
  | "security"
  | "help";

interface NavItem {
  id: SettingsSectionId;
  label: string;
  icon: LucideIcon;
}

interface NavGroup {
  label: string;
  items: NavItem[];
}

const GENERAL_VIEWS: Array<[SettingsSectionId, string]> = [
  ["general", "常用偏好"], ["shortcuts", "快捷键"],
];
const NOTIFICATION_VIEWS: Array<[SettingsSectionId, string]> = [
  ["notify-channels", "通知设置"], ["agent-mail", "通知记录"],
];
const DATA_VIEWS: Array<[SettingsSectionId, string]> = [
  ["data", "备份与恢复"], ["archived", "已归档会话"],
];

// Keep callers of historical IDs working without maintaining duplicate views.
function normalizeSection(section: SettingsSectionId): SettingsSectionId {
  if (section === "personalize") return "general";
  if (section === "personal-memory") return "memory";
  return section;
}

function isActiveNavItem(active: SettingsSectionId, item: SettingsSectionId): boolean {
  if (item === "agent-mail" || item === "notify-channels") return active === "agent-mail" || active === "notify-channels";
  if (item === "general") return GENERAL_VIEWS.some(([id]) => id === active);
  if (item === "data") return active === "data" || active === "archived";
  return active === item;
}

function SettingsViewTabs({
  label,
  items,
  active,
  onSelect,
}: {
  label: string;
  items: Array<[SettingsSectionId, string]>;
  active: SettingsSectionId;
  onSelect: (section: SettingsSectionId) => void;
}) {
  return (
    <div className="settings-view-tabs__sticky">
      <div className="settings-view-tabs" role="group" aria-label={label}>
        {items.map(([id, title]) => (
          <button key={id} type="button" aria-pressed={active === id}
            className={`settings-view-tabs__item${active === id ? " settings-view-tabs__item--active" : ""}`}
            onClick={() => onSelect(id)}>{title}</button>
        ))}
      </div>
    </div>
  );
}

const NAV_GROUPS: NavGroup[] = [
  { label: "偏好", items: [
    { id: "general", label: "通用", icon: SettingsIcon },
    { id: "notify-channels", label: "通知", icon: Bell },
  ] },
  { label: "智能体", items: [
    { id: "model", label: "模型", icon: Cpu },
    { id: "memory", label: "记忆", icon: Brain },
    { id: "security", label: "安全与权限", icon: Shield },
    { id: "agent-settings", label: "高级设置", icon: SlidersHorizontal },
  ] },
  { label: "连接与数据", items: [
    { id: "weixin-channel", label: "微信连接", icon: MessageCircle },
    { id: "cloud-storage", label: "云存储", icon: Cloud },
    { id: "usage", label: "模型用量", icon: BarChart3 },
    { id: "data", label: "数据管理", icon: Database },
  ] },
  { label: "关于与支持", items: [
    { id: "help", label: "帮助与更新", icon: HelpCircle },
  ] },
];

function settingsSectionTitle(section: SettingsSectionId): string {
  const subview = [...GENERAL_VIEWS, ...NOTIFICATION_VIEWS, ...DATA_VIEWS]
    .find(([id]) => id === section);
  return subview?.[1] ?? NAV_GROUPS.flatMap(group => group.items)
    .find(item => item.id === section)?.label ?? "设置";
}

export function SettingsPanel({
  open,
  onClose,
  onModelsChanged,
  sessionId,
  cwd,
  initialSection = "general",
  initialModelId,
  onRestoreSession,
  onDeleteSession,
  onOpenSession,
  onOpenAutomation,
  onToast,
}: {
  open: boolean;
  onClose: () => void;
  /** Called after a provider is saved/deleted so the app can refresh its
   *  model picker without a restart. */
  onModelsChanged?: () => void | Promise<void>;
  /** Current live session, required by memory flush/consolidation actions. */
  sessionId?: string;
  /** Current workspace for viewing and editing workspace memory. */
  cwd?: string;
  /** Section selected whenever the dialog is opened. */
  initialSection?: SettingsSectionId;
  initialModelId?: string;
  onRestoreSession?: (sessionId: string, archived: boolean, cwd?: string) => Promise<void>;
  onDeleteSession?: (sessionId: string, cwd?: string) => Promise<void>;
  onOpenSession?: (sessionId: string, cwd?: string) => void | Promise<void>;
  onOpenAutomation?: (automationId: string) => void | Promise<void>;
  onToast?: (message: string) => void;
}) {
  const [active, setActive] = useState<SettingsSectionId>(() => normalizeSection(initialSection));
  const [cloudDirty, setCloudDirty] = useState(false);
  const [memoryDirty, setMemoryDirty] = useState(false);
  const [memoryBusy, setMemoryBusy] = useState(false);
  const drafts = useRef(new Map<string, SettingsDraftState>());
  const [draftBusy, setDraftBusy] = useState(false);
  const reportDraft = useCallback((id: string, state: SettingsDraftState | null) => {
    if (state) drafts.current.set(id, state);
    else drafts.current.delete(id);
    setDraftBusy([...drafts.current.values()].some(draft => draft.busy));
  }, []);
  const [leaveFeedback, setLeaveFeedback] = useState("");
  useEffect(() => {
    if (!draftBusy && !memoryBusy) setLeaveFeedback("");
  }, [draftBusy, memoryBusy]);
  const [searchTarget, setSearchTarget] = useState<{ target?: string } | null>(null);
  const panelRef = useRef<HTMLDivElement>(null);
  const { requestConfirmation, dialog } = useAppDialog();
  const confirmLeave = (action: () => void) => {
    const isMemory = active === "memory";
    const isCloudDraft = active === "cloud-storage" && cloudDirty;
    const isBusy = () => (isMemory && memoryBusy) || [...drafts.current.values()].some(draft => draft.busy);
    if (isBusy()) { setLeaveFeedback("正在处理，请等待完成后再关闭或切换页面。"); return; }
    setLeaveFeedback("");
    const dirty = [...drafts.current.values()].filter(draft => draft.dirty);
    if (!isCloudDraft && !(isMemory && memoryDirty) && !dirty.length) {
      action();
      return;
    }
    // Search options unmount when the confirmation takes focus. Return to
    // their stable input on cancel so keyboard users can continue searching.
    const origin = document.activeElement;
    const returnFocus = origin instanceof HTMLElement
      ? origin.closest(".settings-search")?.querySelector<HTMLInputElement>("input")
      : undefined;
    requestConfirmation({
      title: isMemory ? "舍弃未保存的记忆？" : isCloudDraft ? "舍弃云端文件未保存的修改？" : "舍弃未保存的修改？",
      description: isMemory ? "记忆编辑或导入尚未保存。继续将丢失这些修改。" : isCloudDraft ? "当前云端文本尚未保存。继续将丢失这些修改。" : `${[...new Set(dirty.map(draft => draft.label ?? "编辑内容"))].join("、")}尚未保存。继续将丢失这些修改。`,
      confirmLabel: "舍弃修改",
      danger: true,
      returnFocus,
      action: () => { if (!isBusy()) action(); },
    });
  };
  const selectSection = (section: SettingsSectionId) => {
    const next = normalizeSection(section);
    if (next !== active) confirmLeave(() => { setActive(next); setSearchTarget(null); });
  };
  const requestClose = () => confirmLeave(onClose);
  const modalRef = useModalFocus<HTMLDivElement>(open, requestClose);
  const archivedCount = useSessionsStore((state) => state.independent
    .filter((session) => session.archived && !session.hidden).length);

  const selectRequested = useRef(selectSection);
  selectRequested.current = selectSection;
  const wasOpen = useRef(false);
  // A new direct link while Settings is open follows the same draft guard.
  useEffect(() => {
    if (open) {
      if (wasOpen.current) selectRequested.current(initialSection);
      else { setActive(normalizeSection(initialSection)); setSearchTarget(null); setLeaveFeedback(""); }
    }
    wasOpen.current = open;
  }, [initialSection, open]);

  useEffect(() => {
    if (!searchTarget || !open) return;
    const panel = panelRef.current;
    const target = searchTarget.target ? panel?.querySelector<HTMLElement>(`[data-setting-id="${searchTarget.target}"]`) : panel?.querySelector<HTMLElement>(".settings-section__header, .model-connections__header, .personal-memory__header") ?? panel;
    if (!target) return;
    // Opening summaries through their normal interaction preserves controlled
    // disclosure state and mounts any lazily rendered secondary content.
    const ancestors: HTMLDetailsElement[] = [];
    for (let node: HTMLElement | null = target; node && node !== panel; node = node.parentElement) {
      if (node instanceof HTMLDetailsElement && !node.open) ancestors.unshift(node);
    }
    ancestors.forEach(details => details.querySelector<HTMLElement>(":scope > summary")?.click());
    target.querySelector<HTMLElement>(':scope > button[aria-expanded="false"]')?.click();
    const frame = requestAnimationFrame(() => {
      target.scrollIntoView?.({ block: "center" });
      const control = target.querySelector<HTMLElement>("summary, button:not(:disabled), input:not(:disabled), select:not(:disabled), textarea:not(:disabled), h2");
      const focusTarget = control ?? target;
      if (!focusTarget.matches("button,input,select,textarea,summary")) focusTarget.tabIndex = -1;
      focusTarget.focus({ preventScroll: true });
      target.classList.add("settings-search-highlight");
    });
    const timer = window.setTimeout(() => target.classList.remove("settings-search-highlight"), 3000);
    return () => { cancelAnimationFrame(frame); clearTimeout(timer); target.classList.remove("settings-search-highlight"); };
  }, [searchTarget, active, open]);

  if (!open) return null;

  return (
    <SettingsDraftContext.Provider value={reportDraft}>
    <div
      className="settings-modal-overlay"
      ref={modalRef}
      role="dialog"
      aria-modal="true"
      aria-label="设置"
      onClick={(e) => {
        // 仅当点击遮罩本身(而非弹窗内容)时关闭,与 EchoAgent 一致。
        if (e.target === e.currentTarget) requestClose();
      }}
    >
      <div className="settings-modal">
        <nav className="settings-modal__nav">
          <div className="settings-navigation">
            {NAV_GROUPS.map((group) => (
              <section className="settings-navigation__group" key={group.label}>
                <h2 className="settings-navigation__group-label">{group.label}</h2>
                <ul className="settings-navigation__list">
                  {group.items.map((item) => {
                    const Icon = item.icon;
                    return (
                      <li key={item.id}>
                        <button
                          className={
                            "settings-navigation__item" +
                            (isActiveNavItem(active, item.id) ? " settings-navigation__item--active" : "")
                          }
                          onClick={() => selectSection(item.id)}
                          aria-current={isActiveNavItem(active, item.id) ? "page" : undefined}
                        >
                          <span className="settings-navigation__icon">
                            <Icon size={17} strokeWidth={1.75} />
                          </span>
                          <span className="settings-navigation__label">{item.label}</span>
                          {item.id === "data" && archivedCount > 0 && (
                            <span className="settings-navigation__badge" aria-label={`${archivedCount} 个已归档会话`}>
                              {archivedCount > 99 ? "99+" : archivedCount}
                            </span>
                          )}
                        </button>
                      </li>
                    );
                  })}
                </ul>
              </section>
            ))}
          </div>
        </nav>

        <div className="settings-modal__content">
          <div className="settings-modal__toolbar">
            <div className="settings-modal__location" aria-label="当前设置页">
              <span>设置</span><span aria-hidden="true">/</span>
              <strong>{settingsSectionTitle(active)}</strong>
            </div>
            <SettingsSearch onSelect={(entry, onSelected) => {
              const navigate = () => { setActive(normalizeSection(entry.section)); setSearchTarget({ target: entry.target }); onSelected(); };
              if (normalizeSection(entry.section) === active) navigate();
              else confirmLeave(navigate);
            }} />
            <button
              type="button"
              className="settings-modal__close"
              onClick={requestClose}
              aria-label="关闭设置"
              title="关闭 (Esc)"
              data-modal-initial-focus
            >
              <X size={16} strokeWidth={1.75} />
            </button>
          </div>
          {leaveFeedback && <p className="settings-navigation-feedback" role="status">{leaveFeedback}</p>}
          <div className="settings-modal__panel" ref={panelRef} tabIndex={-1}>
            {active === "model" ? (
              <ModelConnectionsPanel onModelsChanged={onModelsChanged} initialModelId={initialModelId} />
            ) : active === "shortcuts" ? (
              <div className="settings-subview">
                <SettingsViewTabs label="通用视图" items={GENERAL_VIEWS} active={active} onSelect={selectSection} />
                <ShortcutsSettingsPanel />
              </div>
            ) : active === "memory" ? (
              <div className="settings-subview settings-personal-memory">
                <PersonalMemoryPanel cwd={cwd} sessionId={sessionId} onToast={onToast} onOpenSession={onOpenSession} onUnsavedChange={setMemoryDirty} onBusyChange={setMemoryBusy} />
              </div>
            ) : active === "help" ? (
              <HelpSettingsPanel />
            ) : active === "security" ? (
              <SecuritySettingsPanel />
            ) : active === "data" ? (
              <div className="settings-subview">
                <SettingsViewTabs label="数据视图" items={DATA_VIEWS} active={active} onSelect={selectSection} />
                <DataSettingsPanel />
              </div>
            ) : active === "archived" ? (
              <div className="settings-subview">
                <SettingsViewTabs label="数据视图" items={DATA_VIEWS} active={active} onSelect={selectSection} />
                <ArchivedSessionsSettingsPanel
                  onRestoreSession={onRestoreSession}
                  onDeleteSession={onDeleteSession}
                  onOpenSession={onOpenSession}
                  onClose={onClose}
                  onToast={onToast}
                />
              </div>
            ) : active === "usage" ? (
              <SettingsSection title="模型用量" desc="查看本机记录的模型请求和用量。费用仅供估算，以服务商账单为准。"><UsageQuotaPanel /></SettingsSection>
            ) : active === "notify-channels" ? (
              <SettingsSection title="通知设置" navigation={<SettingsViewTabs label="通知视图" items={NOTIFICATION_VIEWS} active={active} onSelect={selectSection} />}><NotifyChannelsPanel onToast={onToast} sessionId={sessionId} onOpenWeixin={() => selectSection("weixin-channel")} /></SettingsSection>
            ) : active === "weixin-channel" ? (
              <SettingsSection title="微信连接" desc="在微信继续已授权的桌面任务。使用期间请保持电脑开机、联网并运行 EchoAgent。"><WeixinChannelPanel onToast={onToast} /></SettingsSection>
            ) : active === "cloud-storage" ? (
              <SettingsSection title="云存储" desc="连接 WebDAV 存储，按需浏览和传输文件。此功能不会自动同步本地资料。"><CloudStoragePanel onToast={onToast} onUnsavedChange={setCloudDirty} /></SettingsSection>
            ) : active === "general" ? (
              <div className="settings-subview">
                <SettingsViewTabs label="通用视图" items={GENERAL_VIEWS} active={active} onSelect={selectSection} />
                <GeneralSettingsPanel />
              </div>
            ) : active === "agent-settings" ? (
              <AgentSettingsPanel />
            ) : active === "agent-mail" ? (
              <div className="settings-subview">
                <SettingsViewTabs label="通知视图" items={NOTIFICATION_VIEWS} active={active} onSelect={selectSection} />
                <NotificationCenterSettingsPanel onOpenSession={onOpenSession} onOpenAutomation={onOpenAutomation} onToast={onToast} onClose={onClose} />
              </div>
            ) : null}
          </div>
        </div>
      </div>
      {dialog}
    </div>
    </SettingsDraftContext.Provider>
  );
}
