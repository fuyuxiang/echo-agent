import { useEffect, useState } from "react";
import {
  Settings as SettingsIcon,
  SlidersHorizontal,
  Keyboard,
  Brain,
  Cpu,
  Palette,
  Database,
  BarChart3,
  Shield,
  HelpCircle,
  X,
  Cloud,
  Send,
  MessageCircle,
  type LucideIcon,
} from "lucide-react";
import {
  AgentSettingsPanel,
  DataSettingsPanel,
  GeneralSettingsPanel,
  HelpSettingsPanel,
  MemorySettingsPanel,
  NotificationCenterSettingsPanel,
  PersonalizeSettingsPanel,
  SecuritySettingsPanel,
  ShortcutsSettingsPanel,
} from "./SettingsSections";
import { UsageQuotaPanel } from "./UsageQuotaPanel";
import { ModelConnectionsPanel } from "./ModelConnectionsPanel";
import { NotifyChannelsPanel } from "./NotifyChannelsPanel";
import { WeixinChannelPanel } from "./WeixinChannelPanel";
import { CloudStoragePanel } from "./CloudStoragePanel";
import { ArchivedSessionsSettingsPanel } from "./ArchivedSessionsSettingsPanel";
import { ResourcesPanel } from "./ResourcesPanel";
import { useModalFocus } from "@/lib/use-modal-focus";
import { useAppDialog } from "./AppDialog";
import { useSessionsStore } from "@/stores/sessions-store";

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

const NOTIFICATION_VIEWS: Array<[SettingsSectionId, string]> = [
  ["agent-mail", "事件收件箱"], ["notify-channels", "通知渠道"],
];
const MEMORY_VIEWS: Array<[SettingsSectionId, string]> = [
  ["memory", "记忆行为"], ["personal-memory", "个人记忆"],
];
const DATA_VIEWS: Array<[SettingsSectionId, string]> = [
  ["data", "备份与目录"], ["archived", "已归档"],
];

function isActiveNavItem(active: SettingsSectionId, item: SettingsSectionId): boolean {
  if (item === "agent-mail") return active === "agent-mail" || active === "notify-channels";
  if (item === "memory") return active === "memory" || active === "personal-memory";
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
  {
    label: "消息与提醒",
    items: [
      { id: "agent-mail", label: "通知", icon: Send },
      { id: "weixin-channel", label: "消息通道", icon: MessageCircle },
    ],
  },
  {
    label: "智能体",
    items: [
      { id: "model", label: "模型", icon: Cpu },
      { id: "agent-settings", label: "智能体设置", icon: SlidersHorizontal },
      { id: "memory", label: "记忆", icon: Brain },
    ],
  },
  {
    label: "应用",
    items: [
      { id: "general", label: "系统设置", icon: SettingsIcon },
      { id: "personalize", label: "个性化", icon: Palette },
      { id: "shortcuts", label: "快捷键", icon: Keyboard },
    ],
  },
  {
    label: "数据与支持",
    items: [
      { id: "usage", label: "用量统计", icon: BarChart3 },
      { id: "cloud-storage", label: "云存储", icon: Cloud },
      { id: "data", label: "数据管理", icon: Database },
      { id: "security", label: "安全中心", icon: Shield },
      { id: "help", label: "帮助与更新", icon: HelpCircle },
    ],
  },
];

export function SettingsPanel({
  open,
  onClose,
  onModelsChanged,
  sessionId,
  cwd,
  initialSection = "model",
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
  onRestoreSession?: (sessionId: string, archived: boolean, cwd?: string) => Promise<void>;
  onDeleteSession?: (sessionId: string, cwd?: string) => Promise<void>;
  onOpenSession?: (sessionId: string, cwd?: string) => void | Promise<void>;
  onOpenAutomation?: (automationId: string) => void | Promise<void>;
  onToast?: (message: string) => void;
}) {
  const [active, setActive] = useState<SettingsSectionId>(initialSection);
  const [cloudDirty, setCloudDirty] = useState(false);
  const { requestConfirmation, dialog } = useAppDialog();
  const confirmLeave = (action: () => void) => {
    if (active !== "cloud-storage" || !cloudDirty) {
      action();
      return;
    }
    requestConfirmation({
      title: "舍弃云端文件未保存的修改？",
      description: "当前云端文本尚未保存。继续将丢失这些修改。",
      confirmLabel: "舍弃修改",
      danger: true,
      action: async () => action(),
    });
  };
  const requestClose = () => confirmLeave(onClose);
  const modalRef = useModalFocus<HTMLDivElement>(open, requestClose);
  const archivedCount = useSessionsStore((state) => state.independent
    .filter((session) => session.archived && !session.hidden).length);

  // Select the caller-requested section every time the dialog opens.
  useEffect(() => {
    if (open) setActive(initialSection);
  }, [initialSection, open]);

  if (!open) return null;

  return (
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
                          onClick={() => confirmLeave(() => setActive(item.id))}
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
          <button
            className="settings-modal__close"
            onClick={requestClose}
            aria-label="关闭设置"
            title="关闭 (Esc)"
            data-modal-initial-focus
          >
            <X size={16} strokeWidth={1.75} />
          </button>
          <div className="settings-modal__panel">
            {active === "model" ? (
              <ModelConnectionsPanel onModelsChanged={onModelsChanged} />
            ) : active === "personalize" ? (
              <PersonalizeSettingsPanel />
            ) : active === "shortcuts" ? (
              <ShortcutsSettingsPanel />
            ) : active === "memory" ? (
              <div className="settings-subview">
                <SettingsViewTabs label="记忆视图" items={MEMORY_VIEWS} active={active} onSelect={setActive} />
                <MemorySettingsPanel />
              </div>
            ) : active === "personal-memory" ? (
              <div className="settings-subview settings-personal-memory">
                <SettingsViewTabs label="记忆视图" items={MEMORY_VIEWS} active={active} onSelect={setActive} />
                <ResourcesPanel cwd={cwd} sessionId={sessionId} onToast={onToast} />
              </div>
            ) : active === "help" ? (
              <HelpSettingsPanel />
            ) : active === "security" ? (
              <SecuritySettingsPanel />
            ) : active === "data" ? (
              <div className="settings-subview">
                <SettingsViewTabs label="数据视图" items={DATA_VIEWS} active={active} onSelect={setActive} />
                <DataSettingsPanel />
              </div>
            ) : active === "archived" ? (
              <div className="settings-subview">
                <SettingsViewTabs label="数据视图" items={DATA_VIEWS} active={active} onSelect={setActive} />
                <ArchivedSessionsSettingsPanel
                  onRestoreSession={onRestoreSession}
                  onDeleteSession={onDeleteSession}
                  onOpenSession={onOpenSession}
                  onClose={onClose}
                  onToast={onToast}
                />
              </div>
            ) : active === "usage" ? (
              <div className="settings-section">
                <header className="settings-section__header">
                  <div className="settings-section__heading">
                    <h2 className="settings-section__title">用量统计</h2>
                    <p className="settings-section__desc">查看 Token 消耗、成本估算和配额策略。</p>
                  </div>
                </header>
                <div className="settings-section__body">
                  <UsageQuotaPanel />
                </div>
              </div>
            ) : active === "notify-channels" ? (
              <div className="settings-section settings-subview">
                <SettingsViewTabs label="通知视图" items={NOTIFICATION_VIEWS} active={active} onSelect={setActive} />
                <header className="settings-section__header">
                  <div className="settings-section__heading">
                    <h2 className="settings-section__title">通知渠道</h2>
                    <p className="settings-section__desc">配置任务完成、异常和自动化事件要推送到哪里。</p>
                  </div>
                </header>
                <div className="settings-section__body">
                  <NotifyChannelsPanel onToast={onToast} />
                </div>
              </div>
            ) : active === "weixin-channel" ? (
              <div className="settings-section">
                <header className="settings-section__header">
                  <div className="settings-section__heading">
                    <h2 className="settings-section__title">消息通道</h2>
                    <p className="settings-section__desc">连接消息应用，在外出时继续桌面任务并管理访问范围。</p>
                  </div>
                </header>
                <div className="settings-section__body"><WeixinChannelPanel onToast={onToast} /></div>
              </div>
            ) : active === "cloud-storage" ? (
              <div className="settings-section">
                <header className="settings-section__header">
                  <div className="settings-section__heading">
                    <h2 className="settings-section__title">云存储</h2>
                    <p className="settings-section__desc">管理 WebDAV 连接，并手动浏览、上传和下载云端文件；不会自动同步。</p>
                  </div>
                </header>
                <div className="settings-section__body">
                  <CloudStoragePanel onToast={onToast} onUnsavedChange={setCloudDirty} />
                </div>
              </div>
            ) : active === "general" ? (
              <GeneralSettingsPanel />
            ) : active === "agent-settings" ? (
              <AgentSettingsPanel />
            ) : active === "agent-mail" ? (
              <div className="settings-subview">
                <SettingsViewTabs label="通知视图" items={NOTIFICATION_VIEWS} active={active} onSelect={setActive} />
                <NotificationCenterSettingsPanel onOpenSession={onOpenSession} onOpenAutomation={onOpenAutomation} onToast={onToast} onClose={onClose} />
              </div>
            ) : null}
          </div>
        </div>
      </div>
      {dialog}
    </div>
  );
}
