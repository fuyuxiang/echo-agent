import { PageHeading } from "./PageHeading";
/**
 * Settings 面板的各个真实分区。
 *
 * 这些是 SettingsPanel.tsx 里除了"模型"以外的运行时设置分区。
 * 每个分区对接 EchoAgent 已有的能力：
 *  - personalize: 主题（接 ThemeProvider）+ 字号
 *  - shortcuts: 当前版本真实生效的快捷键说明
 *  - memory: 记忆行为配置
 *  - help: 版本更新、文档与排查说明
 *  - security: 系统授权与工具规则
 *  - data: 备份恢复与本地数据目录
 *  - general: 窗口与后台运行偏好
 *  - agent-settings: 子代理、配置重载与诊断
 */
import { useCallback, useEffect, useRef, useState } from "react";
import { useSettingsDraft } from "@/lib/settings-drafts";
import { BrowserTaskSettings } from "./BrowserTaskSettings";
import { PromptPredictionSettings } from "./PromptPredictionSettings";
import {
  Sun,
  Moon,
  Type,
  Folder,
  Trash2,
  ExternalLink,
  RefreshCw,
  Mail,
  CheckCheck,
  Filter,
  Download,
  CheckCircle2,
  AlertTriangle,
  Loader2,
  AppWindow,
  Globe2,
  Monitor,
  BookOpen,
  LifeBuoy,
  ChevronDown,
} from "lucide-react";
import { SettingsDisclosure } from "./SettingsDisclosure";
import { useTheme } from "./ThemeProvider";
import { exportBackup, inspectBackup, restoreBackup, backupLastError } from "@/lib/backup-client";
import {
  internalReload,
  memoryConfigGet,
  memoryConfigSave,
  notificationClear,
  notificationList,
  notificationMarkAllRead,
  notificationMarkRead,
  permissionList,
  permissionSave,
  subagentsConfigGet,
  subagentsConfigSave,
  echoAgentDataDir,
  desktopPreferencesGet,
  desktopPreferencesSave,
  openEchoAgentDataDir,
  openExternalUrl,
  type MemoryConfig,
} from "@/lib/agent-client";
import type {
  NotificationEntry,
  NotificationKind,
  PermissionRule,
} from "@/lib/types";
import { APP_VERSION } from "@/lib/app-version";
import {
  automationCapabilities,
  automationRequestComputerPermissions,
  type AutomationCapabilities,
} from "@/lib/automation-client";
import { useUpdateStore } from "@/stores/update-store";
import { validateOtlpEndpoint } from "@/lib/otlp-exporter";
import { DEFAULT_FONT_SIZE, MAX_FONT_SIZE, MIN_FONT_SIZE, readFontSize, saveFontSize } from "@/lib/font-size";
import { useAppDialog } from "./AppDialog";
import { usePermissionStore } from "@/stores/permission-store";
import "@/styles/help-settings.css";
import "@/styles/advanced-settings.css";
import "@/styles/security-settings.css";

const ECHO_AGENT_DOCS_URL = "https://fuyuxiang.github.io/echo-agent/";
const ACP_SPEC_URL = "https://agentclientprotocol.com/";
const SHORTCUT_GROUPS: Array<{
  title: string;
  items: Array<{ key: string; action: string }>;
}> = [
  {
    title: "全局导航",
    items: [
      { key: "Ctrl/Cmd + N", action: "新建任务" },
      { key: "Ctrl/Cmd + K", action: "搜索会话" },
      { key: "Ctrl/Cmd + ,", action: "打开设置" },
      { key: "Ctrl/Cmd + B", action: "切换侧栏" },
    ],
  },
  {
    title: "对话与编辑",
    items: [
      { key: "Enter", action: "发送消息" },
      { key: "Shift + Enter", action: "换行" },
      { key: "Ctrl/Cmd + F", action: "查找当前会话" },
      { key: "Esc", action: "关闭当前弹窗" },
    ],
  },
  {
    title: "快捷输入",
    items: [
      { key: "/ ", action: "触发技能/命令补全" },
      { key: "@ ", action: "引用对话文件" },
    ],
  },
];

export function SettingsSection({
  navigation,
  title,
  desc,
  actions,
  children,
}: {
  title: string;
  desc?: string;
  actions?: React.ReactNode;
  navigation?: React.ReactNode;
  children: React.ReactNode;
}) {
  return (
    <div className={`settings-section${navigation ? " settings-subview" : ""}`}>
      {navigation}
      <header className="settings-section__header">
        <PageHeading title={title} description={desc} level={2}
          className="settings-section__heading" titleClassName="settings-section__title"
          descriptionClassName="settings-section__desc" />
        {actions && <div className="settings-section__actions">{actions}</div>}
      </header>
      <div className="settings-section__body">{children}</div>
    </div>
  );
}

export function SettingsGroup({
  title,
  desc,
  meta,
  children,
  className = "",
  settingId,
}: {
  title: string;
  desc?: string;
  meta?: React.ReactNode;
  children: React.ReactNode;
  className?: string;
  settingId?: string;
}) {
  return (
    <section className={`settings-group${className ? ` ${className}` : ""}`} data-setting-id={settingId}>
      <header className="settings-group__header">
        <div>
          <h3 className="settings-group__title">{title}</h3>
          {desc && <p className="settings-group__desc">{desc}</p>}
        </div>
        {meta && <div className="settings-group__meta">{meta}</div>}
      </header>
      <div className="settings-group__content">{children}</div>
    </section>
  );
}

// ---------- 个性化 ----------

function AppearanceSettingsGroup() {
  const { theme, preference, setTheme } = useTheme();
  const [fontSize, setFontSize] = useState<number>(readFontSize);

  useEffect(() => {
    saveFontSize(fontSize);
  }, [fontSize]);

  return (
    <SettingsGroup title="外观" settingId="appearance" meta={<span>立即生效</span>}>
      <div className="settings-row settings-row--comfortable">
        <div className="settings-row__label settings-row__label--stacked">
          <span className="settings-row__name">
            {theme === "dark" ? <Moon size={17} /> : <Sun size={17} />}
            外观模式
          </span>
          <span className="settings-row__description">选择浅色、深色，或跟随系统设置。</span>
        </div>
        <div className="settings-row__control theme-toggle" role="group" aria-label="界面主题">
          <button
            className={`theme-toggle__btn ${preference === "system" ? "theme-toggle__btn--active" : ""}`}
            onClick={() => setTheme("system")}
            aria-pressed={preference === "system"}
          >
            <Monitor size={15} /> 跟随系统
          </button>
          <button
            className={`theme-toggle__btn ${preference === "light" ? "theme-toggle__btn--active" : ""}`}
            onClick={() => setTheme("light")}
            aria-pressed={preference === "light"}
          >
            <Sun size={15} /> 浅色
          </button>
          <button
            className={`theme-toggle__btn ${preference === "dark" ? "theme-toggle__btn--active" : ""}`}
            onClick={() => setTheme("dark")}
            aria-pressed={preference === "dark"}
          >
            <Moon size={15} /> 深色
          </button>
        </div>
      </div>

      <div className="settings-row settings-row--comfortable" data-setting-id="font-size">
        <div className="settings-row__label settings-row__label--stacked">
          <span className="settings-row__name"><Type size={17} />字体大小</span>
          <span className="settings-row__description">调整对话和界面文字大小，当前正文 {fontSize}px。</span>
        </div>
        <div className="settings-row__control settings-font-control">
          <span className="settings-font-control__sample settings-font-control__sample--small">A</span>
          <input
            type="range"
            min={MIN_FONT_SIZE}
            max={MAX_FONT_SIZE}
            value={fontSize}
            onChange={(e) => setFontSize(Number(e.target.value))}
            aria-label="字体大小"
            aria-valuetext={`${fontSize} 像素`}
          />
          <span className="settings-font-control__sample settings-font-control__sample--large">A</span>
          <button className="settings-btn settings-reset" onClick={() => setFontSize(DEFAULT_FONT_SIZE)}>
            重置
          </button>
        </div>
      </div>
    </SettingsGroup>
  );
}

// ---------- 快捷键 ----------

export function ShortcutsSettingsPanel() {
  return (
    <SettingsSection
      title="快捷键"
      desc="以下是当前版本已实际生效的快捷键。"
    >
      <div className="shortcuts-groups">
        {SHORTCUT_GROUPS.map((group) => (
          <SettingsGroup title={group.title} key={group.title}>
            <ul className="shortcuts-list">
              {group.items.map((shortcut) => (
                <li key={shortcut.key} className="shortcuts-list__row">
                  <span className="shortcuts-list__action">{shortcut.action}</span>
                  <kbd className="shortcuts-list__key">{shortcut.key}</kbd>
                </li>
              ))}
            </ul>
          </SettingsGroup>
        ))}
      </div>
    </SettingsSection>
  );
}

// ---------- 记忆 ----------

const DEFAULT_MEMORY_CONFIG: MemoryConfig = {
  enabled: true,
  initialInjectionEnabled: true,
  saveOnEnd: true,
  watcherEnabled: true,
  autoFlushEnabled: true,
  dreamEnabled: true,
};

export function MemorySettingsPanel({ compact = false, onBusyChange, onConfigChange }: { compact?: boolean; onBusyChange?: (busy: boolean) => void; onConfigChange?: (config: MemoryConfig) => void }) {
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState<string | null>(null);
  const [config, setConfig] = useState<MemoryConfig>(DEFAULT_MEMORY_CONFIG);
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState(false);
  useSettingsDraft({ dirty: false, busy, label: "记忆设置" });
  const [reload, setReload] = useState(0);

  useEffect(() => { onBusyChange?.(busy); return () => onBusyChange?.(false); }, [busy, onBusyChange]);
  useEffect(() => { if (!loading && !loadError && !busy) onConfigChange?.(config); }, [config, loading, loadError, busy, onConfigChange]);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setLoadError(false);
    void memoryConfigGet()
      .then((value) => {
        if (!cancelled) setConfig(value);
      })
      .catch((error) => {
        if (!cancelled) { setLoadError(true); setMsg(`读取记忆配置失败：${String(error).replace(/^Error:\s*/, "")}`); }
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [reload]);

  const updateConfig = async (key: keyof MemoryConfig, value: boolean) => {
    if (loading || loadError || busy) return;
    const previous = config;
    setConfig({ ...config, [key]: value });
    setBusy(true);
    try {
      const saved = await memoryConfigSave({ [key]: value }, config.revision);
      setConfig({ ...previous, [key]: value, ...saved });
      setMsg("记忆配置已保存，立即用于后续提取和模型请求。");
    } catch (e) {
      setConfig(previous);
      setLoadError(true);
      setMsg(`失败：${String(e).replace(/^Error:\s*/, "")}`);
    } finally {
      setBusy(false);
    }
  };

  const legacyPolicyChanged = !config.initialInjectionEnabled || !config.saveOnEnd || !config.watcherEnabled || !config.autoFlushEnabled || !config.dreamEnabled;
  const restorePolicy = async () => {
    if (loading || loadError || busy) return;
    setBusy(true);
    try {
      const saved = await memoryConfigSave({ ...DEFAULT_MEMORY_CONFIG, enabled: config.enabled }, config.revision);
      setConfig(saved);
      setMsg("已恢复自动学习、整理和参考记忆。");
    } catch (e) { setLoadError(true); setMsg(`失败：${String(e).replace(/^Error:\s*/, "")}`); }
    finally { setBusy(false); }
  };

  const controls = <>
    {loadError && <div role="alert" className="settings-hint">配置未加载，暂时无法修改。<button className="settings-btn" onClick={() => setReload((n) => n + 1)}>重新加载</button></div>}
    <div className="settings-row settings-row--comfortable">
      <div className="settings-row__label settings-row__label--stacked"><span className="settings-row__name">使用记忆</span><span className="settings-row__description">{compact ? config.enabled ? "在后续任务中参考你的偏好和工作背景，减少重复说明。" : "已停止学习和使用记忆，已保存的内容仍可管理。" : "记住你的偏好和项目背景，在后续对话中参考。"}</span></div>
      <label className="sk-toggle"><input type="checkbox" role="switch" aria-label="使用记忆" checked={config.enabled} disabled={loading || loadError || busy} onChange={(event) => void updateConfig("enabled", event.target.checked)} /><span className="sk-toggle-track"><span className="sk-toggle-thumb" /></span></label>
    </div>
    {!loading && !loadError && legacyPolicyChanged && <p className="settings-hint" role="note">你在旧版中关闭了部分记忆策略，自动学习或参考记忆可能受限。<button className="settings-btn" disabled={busy} onClick={() => void restorePolicy()}>恢复推荐策略</button></p>}
    {!compact && <p className="settings-hint">关闭后立即停止后续提取和使用记忆，已有内容仍可查看和管理。自动整理会使用已配置的模型服务。</p>}
    {msg && <p className="settings-msg" role="status">{msg}</p>}
  </>;
  return compact ? <div className="personal-memory__switch" data-setting-id="memory-preference">{controls}</div> : <SettingsSection title="记忆" desc="管理 Echo 对你的了解。">{controls}</SettingsSection>;

}

// ---------- 帮助与更新 ----------

export function HelpSettingsPanel() {
  const [resourceError, setResourceError] = useState("");
  const updateStatus = useUpdateStore((state) => state.status);
  const update = useUpdateStore((state) => state.update);
  const checkedAt = useUpdateStore((state) => state.checkedAt);
  const updateError = useUpdateStore((state) => state.error);
  const updateErrorStage = useUpdateStore((state) => state.errorStage);
  const downloaded = useUpdateStore((state) => state.downloaded);
  const total = useUpdateStore((state) => state.total);
  const checkUpdate = useUpdateStore((state) => state.check);
  const installUpdate = useUpdateStore((state) => state.install);
  const updateBusy = updateStatus === "checking" || updateStatus === "downloading" || updateStatus === "installing";
  const updateProgress = total && total > 0 ? Math.min(100, Math.round((downloaded / total) * 100)) : undefined;
  const checkedDate = checkedAt ? new Date(checkedAt) : null;
  const checkedLabel = checkedDate && !Number.isNaN(checkedDate.getTime())
    ? `${checkedDate.toDateString() === new Date().toDateString() ? "今天 " : checkedDate.toLocaleDateString("zh-CN") + " "}${checkedDate.toLocaleTimeString("zh-CN", { hour: "2-digit", minute: "2-digit", hour12: false })}`
    : null;
  const statusLabel = {
    idle: "启动时自动检查更新",
    checking: "正在检查更新…",
    available: `发现新版本 v${update?.version}`,
    downloading: `正在下载 v${update?.version}${updateProgress === undefined ? "" : ` · ${updateProgress}%`}`,
    installing: "正在安装，完成后将重启",
    "up-to-date": "当前已是最新版本",
    error: updateErrorStage === "install" ? "安装更新失败" : updateErrorStage === "download" ? "下载更新失败" : "检查更新失败",
  }[updateStatus];

  const openHelpResource = (url: string) => {
    setResourceError("");
    void openExternalUrl(url).catch((error) => {
      setResourceError(`打开链接失败：${String(error).replace(/^Error:\s*/, "")}`);
    });
  };

  return (
    <div className="help-settings">
    <SettingsSection title="帮助与更新">
      <section className="help-settings__version" aria-label="版本与更新" data-setting-id="updates">
        <div className="help-settings__version-row">
          <img className="help-settings__logo" src="/app-icon.png" alt="" width={40} height={40} draggable={false} />
          <div className="help-settings__identity">
            <div className="help-settings__product"><h3>EchoAgent</h3><span>v{APP_VERSION}</span></div>
            <p className="help-settings__status" role={updateStatus === "error" ? "alert" : "status"}>
              {updateStatus === "up-to-date" && <CheckCircle2 size={14} aria-hidden="true" />}
              {updateStatus === "error" && <AlertTriangle size={14} aria-hidden="true" />}
              {statusLabel}
            </p>
            {checkedLabel && !updateBusy && updateStatus !== "error" && <p className="help-settings__checked">上次检查：<time dateTime={checkedAt} title={checkedDate!.toLocaleString("zh-CN")}>{checkedLabel}</time></p>}
          </div>
          {updateStatus === "available" ? (
            <button className="settings-btn settings-btn--primary" onClick={() => void installUpdate()}>
              <Download size={15} aria-hidden="true" /> 下载并安装
            </button>
          ) : (
            <button className="settings-btn" onClick={() => void checkUpdate(true)} disabled={updateBusy}>
              {updateBusy ? <Loader2 size={15} className="update-dialog__spinner" aria-hidden="true" /> : <RefreshCw size={15} aria-hidden="true" />}
              {updateStatus === "checking" ? "检查中" : updateStatus === "downloading" ? "下载中" : updateStatus === "installing" ? "安装中" : updateStatus === "error" ? "重新检查" : "检查更新"}
            </button>
          )}
        </div>
        {(updateStatus === "downloading" || updateStatus === "installing") && <div className="settings-update-progress help-settings__progress" role="progressbar" aria-label="更新进度" aria-valuemin={0} aria-valuemax={100} aria-valuenow={updateStatus === "downloading" ? updateProgress : undefined}>
          <span style={updateStatus === "downloading" && updateProgress !== undefined ? { width: `${updateProgress}%` } : undefined} />
        </div>}
        {updateStatus === "available" && update?.notes?.trim() && <details className="help-settings__details" key={`notes-${update.version}`}><summary>更新内容<ChevronDown size={14} aria-hidden="true" /></summary><p>{update.notes}</p></details>}
        {updateStatus === "error" && <div className="help-settings__error">
          <p>{updateErrorStage === "install" ? "重新检查更新后，可再次尝试安装。" : updateErrorStage === "download" ? "请检查网络连接后重新检查更新。" : "请确认已连接企业内网或 VPN，再重试。"}</p>
          {updateError && <details className="help-settings__details" key={`error-${updateErrorStage}-${updateError}`}><summary>查看详情<ChevronDown size={14} aria-hidden="true" /></summary><p>{updateError}</p></details>}
        </div>}
      </section>
      <div className="help-settings__resources" role="group" aria-label="使用帮助">
        <a className="help-settings__resource" aria-label="使用文档（在浏览器中打开）" href={ECHO_AGENT_DOCS_URL} onClick={(event) => { event.preventDefault(); openHelpResource(ECHO_AGENT_DOCS_URL); }}>
          <BookOpen size={19} aria-hidden="true" /><span><strong>使用文档</strong><span>功能说明与配置指南</span></span><ExternalLink size={16} aria-hidden="true" />
        </a>
        <details className="help-settings__troubleshooting" data-setting-id="help-faq">
          <summary className="help-settings__resource"><LifeBuoy size={19} aria-hidden="true" /><span><strong>问题排查</strong><span>模型连接、应用启动等问题</span></span><ChevronDown size={16} aria-hidden="true" /></summary>
          <div className="help-settings__troubleshooting-body">
            <ol className="settings-checklist">
              <li><span>1</span><div><strong>确认模型配置</strong><p>在“模型”中确认有可用模型；使用个人服务时，添加连接并填写所需凭据。</p></div></li>
              <li><span>2</span><div><strong>检查配置文件</strong><p>确认 <code>~/.echo-agent/config.toml</code> 可以正常读写。</p></div></li>
              <li><span>3</span><div><strong>重新加载或重启</strong><p>先在“高级设置”重新加载对应配置，仍无效时再重启应用。</p></div></li>
            </ol>
            <details className="help-settings__details help-settings__technical"><summary>技术资料<ChevronDown size={14} aria-hidden="true" /></summary>
              <a className="settings-inline-link" href={ACP_SPEC_URL} onClick={(event) => { event.preventDefault(); openHelpResource(ACP_SPEC_URL); }}>ACP 协议规范 <ExternalLink size={13} aria-hidden="true" /></a>
            </details>
          </div>
        </details>
      </div>
      {resourceError && <p className="settings-msg settings-msg--warn" role="alert">{resourceError}</p>}
    </SettingsSection>
    </div>
  );
}

// ---------- 安全中心 ----------

const PERMISSION_ACTION_LABELS: Record<string, string> = { allow: "允许", ask: "询问", deny: "拒绝" };
const PERMISSION_TOOL_LABELS: Record<string, string> = {
  bash: "执行命令", read: "读取文件", edit: "修改文件", grep: "搜索文件",
  mcp: "连接器工具", webfetch: "访问网页", any: "所有工具",
};

export function SecuritySettingsPanel() {
  const [rules, setRules] = useState<PermissionRule[]>([]);
  const [savedRules, setSavedRules] = useState("[]");
  const [loading, setLoading] = useState(true);
  const [rulesError, setRulesError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [draft, setDraft] = useState<PermissionRule>({ action: "ask", tool: "bash", pattern: "" });
  const [feedback, setFeedback] = useState("");
  const [automationSupport, setAutomationSupport] = useState<AutomationCapabilities | null>(null);
  const [automationLoading, setAutomationLoading] = useState(true);
  const [automationBusy, setAutomationBusy] = useState(false);
  const [automationError, setAutomationError] = useState<string | null>(null);
  const [automationFeedback, setAutomationFeedback] = useState("");
  const rulesDirty = JSON.stringify(rules) !== savedRules;
  const builderDirty = draft.action !== "ask" || draft.tool !== "bash" || Boolean(draft.pattern?.trim());
  useSettingsDraft({ dirty: rulesDirty || builderDirty, busy: saving, label: "权限规则" });

  const loadRules = useCallback(async () => {
    setLoading(true);
    try {
      const nextRules = await permissionList();
      setRules(nextRules);
      setSavedRules(JSON.stringify(nextRules));
      setRulesError(null);
    } catch (reason) {
      // Preserve the last known rules. Replacing them with [] would make the
      // next "save all" silently erase a valid backend configuration.
      setRulesError(String(reason).replace(/^Error:\s*/, ""));
    } finally {
      setLoading(false);
    }
  }, []);

  const loadAutomationSupport = useCallback(async () => {
    if (!("__TAURI_INTERNALS__" in window)) {
      setAutomationLoading(false);
      return;
    }
    setAutomationLoading(true);
    try {
      setAutomationSupport(await automationCapabilities());
      setAutomationError(null);
    } catch (error) {
      setAutomationError(String(error).replace(/^Error:\s*/, ""));
    } finally {
      setAutomationLoading(false);
    }
  }, []);

  useEffect(() => {
    void loadRules();
    void loadAutomationSupport();
  }, [loadAutomationSupport, loadRules]);

  useEffect(() => {
    if (!("__TAURI_INTERNALS__" in window)) return;
    const refreshPermissions = () => { void loadAutomationSupport(); };
    window.addEventListener("focus", refreshPermissions);
    return () => window.removeEventListener("focus", refreshPermissions);
  }, [loadAutomationSupport]);

  const requestComputerPermissions = async () => {
    setAutomationBusy(true);
    setAutomationFeedback("");
    try {
      await automationRequestComputerPermissions();
      await loadAutomationSupport();
      setAutomationFeedback("已发起系统授权，请完成授权后返回应用查看状态。");
    } catch (error) {
      setAutomationFeedback(`无法打开系统权限：${String(error).replace(/^Error:\s*/, "")}`);
    } finally {
      setAutomationBusy(false);
    }
  };

  const addRule = () => {
    if (!draft.tool.trim()) return;
    setFeedback("");
    setRules((items) => [...items, {
      action: draft.action,
      tool: draft.tool.trim().toLowerCase(),
      pattern: draft.pattern?.trim() || undefined,
    }]);
    setDraft({ action: "ask", tool: "bash", pattern: "" });
  };

  const saveRules = async () => {
    if (loading || rulesError || saving || builderDirty) return;
    setSaving(true);
    setFeedback("");
    try {
      await permissionSave(rules);
      setSavedRules(JSON.stringify(rules));
      setFeedback("规则已保存；重启 Agent 后对既有会话生效。");
    } catch (error) {
      setFeedback(`保存失败：${String(error).replace(/^Error:\s*/, "")}`);
    } finally {
      setSaving(false);
    }
  };



  const browser = automationSupport?.browser;
  const computer = automationSupport?.computer;
  const isMac = Boolean(computer && /mac|darwin/i.test(computer.platform));
  const desktopUnavailable = computer?.desktopAvailable === false;
  const missingPermissions = computer ? [!computer.screenCapture && "屏幕录制", !computer.inputControl && "辅助功能"].filter(Boolean).join("、") : "";
  const computerReady = Boolean(computer?.available && computer.screenCapture && computer.inputControl && !desktopUnavailable);
  const canRequestPermissions = Boolean(!automationLoading && !automationError && computer?.available && isMac && !desktopUnavailable && missingPermissions);
  const browserState = automationLoading ? "检查中…" : automationError ? "检查失败" : !browser ? "未检查" : browser.available ? "已就绪" : "未就绪";
  const computerState = automationLoading ? "检查中…" : automationError ? "检查失败" : !computer ? "未检查" : !computer.available ? "不可用" : desktopUnavailable ? "暂不可用" : canRequestPermissions ? "需要授权" : computerReady ? isMac ? "已授权" : "已就绪" : "暂不可用";
  const browserDescription = automationLoading ? "正在检查本机浏览器" : automationError ? "暂时无法确认浏览器状态" : !browser ? "请在桌面客户端查看操作权限" : browser.available ? browser.browserName || "兼容浏览器" : browser.reason || "未找到可用浏览器";
  const computerDescription = automationLoading ? "正在检查系统权限" : automationError ? "暂时无法确认系统权限" : !computer ? "请在桌面客户端查看操作权限" : !computer.available || desktopUnavailable ? computer.reason || "当前桌面环境暂不可用" : canRequestPermissions ? `缺少系统权限：${missingPermissions}。授权后返回应用会自动检查。` : computerReady ? isMac ? "屏幕录制、辅助功能已授权，可在系统设置中管理或撤销。" : "当前桌面会话已就绪，具体应用的访问权限会在使用时检查。" : computer.reason || "请检查当前桌面会话后重试";

  return (
    <div className="security-settings">
    <SettingsSection title="安全与权限" desc="查看系统授权，管理任务浏览器和高级权限规则。">
      <section className="security-settings__capabilities" data-setting-id="automation-permissions" aria-label="操作权限">
        <header className="security-settings__capability-header"><div><h3>操作权限</h3><p>在聊天中说明电脑任务即可开始，也可从输入框的 + 菜单选择操作能力。</p></div>
          <button type="button" className="settings-btn settings-btn--ghost" aria-label={automationError ? "重试检查权限" : "重新检查权限"} disabled={automationLoading || automationBusy} onClick={() => void loadAutomationSupport()}><RefreshCw size={14} />{automationLoading ? "检查中…" : automationError ? "重试" : "刷新"}</button>
        </header>
        <div className="security-settings__capability" role="group" aria-label="浏览器操作状态">
          <Globe2 size={20} aria-hidden="true" /><div><strong>浏览器操作</strong><p>{browserDescription}</p></div><span className={`security-settings__badge${!automationLoading && !automationError && browser?.available ? " security-settings__badge--ready" : ""}`}>{browserState}</span>
        </div>
        <div className="security-settings__capability" role="group" aria-label="电脑操作状态">
          <Monitor size={20} aria-hidden="true" /><div><strong>电脑操作</strong><p>{computerDescription}</p></div><span className={`security-settings__badge${!automationLoading && !automationError && computerReady ? " security-settings__badge--ready" : canRequestPermissions ? " security-settings__badge--attention" : ""}`}>{computerState}</span>
          {canRequestPermissions && <button type="button" className="settings-btn" disabled={automationBusy} onClick={() => void requestComputerPermissions()}>{automationBusy ? "打开中…" : "前往授权"}</button>}
        </div>
        {automationError && <p className="settings-msg settings-msg--warn" role="alert">检查失败：{automationError}</p>}
        {automationFeedback && <p className={automationFeedback.startsWith("无法") ? "settings-msg settings-msg--warn" : "security-settings__feedback"} role={automationFeedback.startsWith("无法") ? "alert" : "status"}>{automationFeedback}</p>}
        <p className="settings-hint">{isMac ? "支持的原生操作按应用定向执行；坐标操作仍使用前台。" : "支持的原生控件可直接操作；真实鼠标键盘输入仍使用前台。"}移动鼠标不会暂停任务；与目标应用发生输入冲突时会让出当前操作。</p>
        <p className="security-settings__privacy">使用时，相关网页或屏幕内容可能作为任务上下文发送给当前模型。</p>
      </section>
      <SettingsDisclosure title="任务浏览器" settingId="browser-data" description="管理所选任务的登录数据和访问范围">
        <BrowserTaskSettings />
      </SettingsDisclosure>
      <SettingsDisclosure title="高级权限规则" settingId="permission-rules" description={feedback.startsWith("保存失败") ? feedback : rulesError ? "读取失败，请展开重试；原有规则不会被覆盖。" : loading ? "正在读取…" : `${rules.length ? `${rules.length} 条自定义规则` : "未添加自定义规则"}${rulesDirty || builderDirty ? " · 有未保存的修改" : ""}`} defaultOpen={Boolean(rulesError) || feedback.startsWith("保存失败")}>
        <SettingsGroup
          title="权限规则"
          desc="拒绝优先于询问，询问优先于允许。保存后重启 Agent 生效。"
        >
          {rulesError && (
            <div className="settings-msg settings-msg--warn" role="alert">
              <span>权限规则读取失败：{rulesError}。已保留上次数据，修复前不会允许覆盖保存。</span>{" "}
              <button type="button" className="settings-btn" onClick={() => void loadRules()} disabled={loading}>
                {loading ? "重试中…" : "重试"}
              </button>
            </div>
          )}
          {rules.length > 0 ? (
            <ul className="rules-list" aria-label="权限规则列表">
              {rules.map((rule, index) => (
                <li key={`${rule.action}-${rule.tool}-${rule.pattern ?? ""}-${index}`} className={`rules-list__item rules-list__item--${rule.action}`}>
                  <span className="rules-list__action">{PERMISSION_ACTION_LABELS[rule.action] || rule.action}</span>
                  <span className="rules-list__tool">{PERMISSION_TOOL_LABELS[rule.tool] || rule.tool}</span>
                  <span className="rules-list__pattern">{rule.pattern || "所有调用"}</span>
                  <button
                    type="button"
                    className="settings-btn settings-btn--ghost settings-btn--danger settings-btn--compact settings-btn--icon-only rules-list__remove"
                    onClick={() => { setFeedback(""); setRules((items) => items.filter((_, itemIndex) => itemIndex !== index)); }}
                    aria-label={`删除规则 ${index + 1}`}
                    disabled={loading || saving || Boolean(rulesError)}
                  >
                    ×
                  </button>
                </li>
              ))}
            </ul>
          ) : !loading && !rulesError ? (
            <div className="settings-empty-inline">未添加自定义规则。需要为特定工具设置处理方式时，再添加规则。</div>
          ) : null}

          <div className="permission-rule-builder">
            <label className="permission-rule-builder__field">
              <span>处理方式</span>
              <select className="form-control" value={draft.action} onChange={(e) => { setFeedback(""); setDraft({ ...draft, action: e.target.value }); }} aria-label="规则动作" disabled={loading || saving || Boolean(rulesError)}>
                <option value="deny">拒绝</option>
                <option value="ask">询问</option>
                <option value="allow">允许</option>
              </select>
            </label>
            <label className="permission-rule-builder__field">
              <span>工具类型</span>
              <select className="form-control" value={draft.tool} onChange={(e) => { setFeedback(""); setDraft({ ...draft, tool: e.target.value }); }} aria-label="工具类型" disabled={loading || saving || Boolean(rulesError)}>
                {['bash', 'read', 'edit', 'grep', 'mcp', 'webfetch', 'any'].map((tool) => <option key={tool} value={tool}>{PERMISSION_TOOL_LABELS[tool]}（{tool}）</option>)}
              </select>
            </label>
            <label className="permission-rule-builder__field permission-rule-builder__field--pattern">
              <span>匹配范围（可选）</span>
              <input className="form-control" value={draft.pattern ?? ""} onChange={(e) => { setFeedback(""); setDraft({ ...draft, pattern: e.target.value }); }} aria-label="匹配范围（可选）" placeholder="例如 git *" disabled={loading || saving || Boolean(rulesError)} />
            </label>
          </div>
          <div className="settings-group__footer">
            <button className="settings-btn" type="button" onClick={addRule} disabled={loading || saving || Boolean(rulesError)}>添加规则</button>
            {builderDirty && <button className="settings-btn" type="button" onClick={() => setDraft({ action: "ask", tool: "bash", pattern: "" })} disabled={saving}>清空输入</button>}
            <button className="settings-btn settings-btn--primary" type="button" onClick={() => void saveRules()} disabled={saving || loading || Boolean(rulesError) || builderDirty || !rulesDirty}>
              {saving ? "保存中…" : "保存修改"}
            </button>
          </div>
          {rulesDirty && <p className="security-settings__unsaved" role="status">规则列表有未保存的修改。</p>}
          {builderDirty && <p className="settings-hint" role="note">当前输入尚未加入规则列表。请先“添加规则”，再“保存修改”。</p>}
        </SettingsGroup>
        {feedback && <p className="settings-hint" role={feedback.startsWith("保存失败") ? "alert" : "status"}>{feedback}</p>}
      </SettingsDisclosure>

    </SettingsSection>
    </div>
  );
}

// ---------- 数据管理 ----------

export function DataSettingsPanel() {
  const [agentHome, setAgentHome] = useState("");
  const [message, setMessage] = useState("");
  const [busy, setBusy] = useState(false);
  const { requestConfirmation, dialog } = useAppDialog();

  useEffect(() => {
    void backupLastError().then((error) => { if (error) setMessage(`上次恢复失败，已保留原有数据：${error}`); }).catch(() => {});
    void echoAgentDataDir()
      .then(setAgentHome)
      .catch((error) => setMessage(`读取数据目录失败：${String(error).replace(/^Error:\s*/, "")}`));
  }, []);

  return (
    <SettingsSection title="备份与恢复" desc="导出本机数据备份，或恢复已有备份。已归档任务可在上方单独管理。">
      <SettingsGroup title="数据备份" settingId="backups" desc="保存会话、项目资料、已保存的文档编辑稿和历史版本。不包含密钥、组织登录凭据及外部工作目录的文件。">
        <p className="settings-hint">备份包含聊天和资料正文，请保存在可信位置。建议等待任务结束后操作；恢复前会显示校验结果并再次确认。</p>
        <div className="settings-group__footer">
          <button type="button" className="settings-btn" disabled={busy} onClick={async () => {
            setBusy(true);
            try { const path = await exportBackup(); if (path) setMessage(`备份已保存：${path}`); }
            catch (error) { setMessage(`备份失败：${String(error)}`); }
            finally { setBusy(false); }
          }}>{busy ? "处理中…" : "导出备份"}</button>
          <button type="button" className="settings-btn" disabled={busy} onClick={async () => {
            setBusy(true);
            try {
              const preview = await inspectBackup();
              if (preview) requestConfirmation({
                title: "恢复备份并重启？",
                description: `已校验 ${preview.fileCount} 个文件，约 ${(preview.totalBytes / 1024 / 1024).toFixed(1)} MB，创建于 ${new Date(preview.createdAt).toLocaleString()}。包含 ${preview.uiKeys.length} 类界面数据。恢复将停止当前任务并替换同名记录，旧文件保留在数据目录的 restore-previous 文件夹中。备份中的项目目录关联也会恢复，请确认来源可信。定时任务和待发送队列将暂停，需检查后手动恢复。`,
                confirmLabel: "恢复并重启",
                action: async () => { setBusy(true); try { await restoreBackup(preview.token); } finally { setBusy(false); } },
                onError: (error) => setMessage(`恢复失败：${String(error)}`),
              });
            } catch (error) { setMessage(`备份校验失败：${String(error)}`); }
            finally { setBusy(false); }
          }}>从备份恢复</button>
        </div>
      </SettingsGroup>
      <SettingsDisclosure title="本地数据与备份范围" description="查看存储位置，以及备份包含的内容">
        <p className="settings-hint">备份包含会话、项目资料、代码任务记录、记忆、粘贴附件、草稿、队列和本机用量。恢复会替换同名记录，其他数据保留。历史外部文件仍依赖原工作目录。原件缺失时，可从所属任务的成果面板打开已恢复的编辑稿并另存为 Word 或 PDF。</p>
        <SettingsGroup title="本地数据目录" settingId="data-directory" desc="会话、项目、自动化、通知和配置都保存在此目录。">
          <div className="settings-row settings-row--comfortable settings-row--path">
            <div className="settings-row__label settings-row__label--stacked">
              <span className="settings-row__name"><Folder size={17} />存储位置</span>
              <span className="settings-row__description">此位置包含应用的重要本地数据</span>
            </div>
            <code className="settings-path-value">{agentHome || "正在读取…"}</code>
          </div>
          <div className="settings-group__footer">
            <button className="settings-btn settings-btn--primary" onClick={() => {
              void openEchoAgentDataDir().catch((error) => setMessage(`打开失败：${String(error).replace(/^Error:\s*/, "")}`));
            }}>
              <Folder size={15} /> 在系统中打开
            </button>
          </div>
        </SettingsGroup>
        <p className="settings-hint">请通过应用管理会话和资料，避免直接删除数据目录中的文件。</p>
      </SettingsDisclosure>
      {message && <p className="settings-msg" role="status">{message}</p>}
      {dialog}
    </SettingsSection>
  );
}

// ---------- 系统设置 ----------

export function GeneralSettingsPanel() {
  const [msg, setMsg] = useState<string | null>(null);
  const [closeToTray, setCloseToTray] = useState(true);
  const [loadingDesktopPreferences, setLoadingDesktopPreferences] = useState(true);
  const [desktopPreferencesError, setDesktopPreferencesError] = useState<string | null>(null);
  const [desktopPreferencesReload, setDesktopPreferencesReload] = useState(0);
  const [savingDesktopPreferences, setSavingDesktopPreferences] = useState(false);
  useSettingsDraft({ dirty: false, busy: savingDesktopPreferences, label: "后台运行设置" });

  useEffect(() => {
    let active = true;
    setLoadingDesktopPreferences(true);
    void desktopPreferencesGet()
      .then((preferences) => {
        if (active) {
          setCloseToTray(preferences.closeToTray);
          setDesktopPreferencesError(null);
        }
      })
      .catch((error) => {
        if (active) {
          setDesktopPreferencesError(String(error).replace(/^Error:\s*/, ""));
        }
      })
      .finally(() => {
        if (active) setLoadingDesktopPreferences(false);
      });
    return () => {
      active = false;
    };
  }, [desktopPreferencesReload]);

  const handleCloseToTrayChange = async (enabled: boolean) => {
    const previous = closeToTray;
    setCloseToTray(enabled);
    setSavingDesktopPreferences(true);
    setMsg(null);
    try {
      const saved = await desktopPreferencesSave(enabled);
      setCloseToTray(saved.closeToTray);
      setMsg(enabled ? "已开启后台运行。" : "已关闭后台运行，下次关闭窗口将退出 EchoAgent。");
    } catch (error) {
      setCloseToTray(previous);
      setMsg(`保存桌面偏好失败：${String(error).replace(/^Error:\s*/, "")}`);
    } finally {
      setSavingDesktopPreferences(false);
    }
  };



  return (
    <SettingsSection
      title="通用"
    >
      <AppearanceSettingsGroup />
      <SettingsGroup title="输入框" settingId="composer-predictions" meta={<span>立即生效</span>}>
        <PromptPredictionSettings />
      </SettingsGroup>
      <SettingsGroup title="后台运行" settingId="background">
        <div className="settings-row settings-row--comfortable">
          <div className="settings-row__label settings-row__label--stacked">
            <span className="settings-row__name"><AppWindow size={17} />关闭窗口后继续运行</span>
            <span className="settings-row__description">
              开启后，关闭窗口会保留在系统托盘，任务和自动化调度可继续运行。关闭此选项后，下次关闭窗口将退出应用。
            </span>
          </div>
          <label className="sk-toggle">
            <input
              type="checkbox"
              role="switch"
              aria-label="关闭窗口后继续运行"
              checked={closeToTray}
              disabled={loadingDesktopPreferences || savingDesktopPreferences || Boolean(desktopPreferencesError)}
              onChange={(event) => void handleCloseToTrayChange(event.target.checked)}
            />
            <span className="sk-toggle-track"><span className="sk-toggle-thumb" /></span>
          </label>
        </div>
        {desktopPreferencesError && (
          <div className="settings-msg settings-msg--warn" role="alert">
            桌面偏好读取失败：{desktopPreferencesError}。在读取成功前不会修改原有设置。
            <button type="button" className="settings-btn" onClick={() => setDesktopPreferencesReload((value) => value + 1)}>重试</button>
          </div>
        )}
        {!closeToTray && !loadingDesktopPreferences && (
          <div className="settings-info-callout settings-info-callout--warn" role="status">
            关闭主窗口将停止任务执行和自动化调度。如需继续运行，请最小化窗口。
          </div>
        )}
      </SettingsGroup>

      {msg && <p className="settings-msg">{msg}</p>}
    </SettingsSection>
  );
}

const RELOAD_OPTIONS = [
  { kind: "mcp_all", name: "MCP 连接器", description: "连接器或工具配置更改后未生效时，重新读取配置。" },
  { kind: "skills", name: "技能目录", description: "新添加或修改的技能未出现时，重新扫描技能目录。" },
  { kind: "models", name: "模型配置", description: "模型连接更改后未生效时，重新读取模型配置。" },
] as const;
type ReloadKind = typeof RELOAD_OPTIONS[number]["kind"];
type ReloadFeedback = { kind: "success" | "error"; text: string };

function RuntimeReloadSettingsGroup() {
  const [busy, setBusy] = useState<ReloadKind | null>(null);
  const busyRef = useRef(false);
  const [results, setResults] = useState<Partial<Record<ReloadKind, ReloadFeedback>>>({});
  useSettingsDraft({ dirty: false, busy: Boolean(busy), label: "配置重载" });
  const handleReload = async (kind: ReloadKind, name: string) => {
    if (busyRef.current) return;
    busyRef.current = true;
    setBusy(kind);
    setResults(current => ({ ...current, [kind]: undefined }));
    try {
      await internalReload(kind);
      setResults(current => ({ ...current, [kind]: { kind: "success", text: `${name}已重新加载。` } }));
    } catch (error) {
      setResults(current => ({ ...current, [kind]: { kind: "error", text: `重新加载失败：${String(error).replace(/^Error:\s*/, "")}` } }));
    } finally {
      busyRef.current = false;
      setBusy(null);
    }
  };
  return <SettingsDisclosure title="配置未生效？" settingId="reload" description="按需重新加载连接器、技能或模型配置">
    <p className="settings-hint">选择未生效的配置重新加载，完成后再重试相关操作。</p>
    <div className="advanced-settings__reloads">
      {RELOAD_OPTIONS.map(option => <section className="advanced-settings__reload" aria-label={option.name} key={option.kind}>
        <div className="advanced-settings__reload-row">
          <div><h3>{option.name}</h3><p>{option.description}</p></div>
          <button type="button" className="settings-btn" aria-label={`重新加载${option.name}`} aria-busy={busy === option.kind} disabled={Boolean(busy)} onClick={() => void handleReload(option.kind, option.name)}>
            {busy === option.kind ? <Loader2 size={15} className="advanced-settings__spin" /> : <RefreshCw size={15} />}{busy === option.kind ? "加载中…" : "重新加载"}
          </button>
        </div>
        {results[option.kind] && <p className={`settings-msg settings-msg--${results[option.kind]!.kind}`} role={results[option.kind]!.kind === "error" ? "alert" : "status"}>{results[option.kind]!.text}</p>}
      </section>)}
    </div>
  </SettingsDisclosure>;
}

function DeveloperSettingsGroup({ children, hasPendingChanges, hasError }: {
  children: React.ReactNode;
  hasPendingChanges: boolean;
  hasError: boolean;
}) {
  const [feedback, setFeedback] = useState("");
  const [savedEndpoint, setSavedEndpoint] = useState(() => {
    try { return localStorage.getItem("echoagent.otlp.endpoint") ?? ""; } catch { return ""; }
  });
  const [otlpEndpoint, setOtlpEndpoint] = useState(savedEndpoint);
  const endpointDirty = otlpEndpoint.trim() !== savedEndpoint;
  useSettingsDraft({ dirty: endpointDirty, label: "监控服务配置" });
  const saveOtlp = (value = otlpEndpoint) => {
    const endpoint = value.trim();
    try {
      if (endpoint) localStorage.setItem("echoagent.otlp.endpoint", validateOtlpEndpoint(endpoint));
      else localStorage.removeItem("echoagent.otlp.endpoint");
      setSavedEndpoint(endpoint);
      setOtlpEndpoint(endpoint);
    } catch (error) {
      setFeedback(`无法保存监控配置：${String(error).replace(/^Error:\s*/, "")}`);
      return;
    }
    setFeedback(endpoint ? "监控接入配置已保存，重启应用后生效。" : "监控已关闭，重启应用后生效。");
  };
  const summary = [
    hasPendingChanges || endpointDirty ? "有未保存的修改" : null,
    savedEndpoint ? "监控服务已启用 · 更改后需重启应用" : "执行参数与监控接入，通常无需修改",
  ].filter(Boolean).join(" · ");
  return <SettingsDisclosure title="开发者选项" settingId="developer-options" defaultOpen={hasError || feedback.startsWith("无法保存")} description={summary}>
    {children}
    <SettingsGroup title="接入监控服务" settingId="diagnostics" desc="将运行事件发送到你配置的监控服务，用于排查问题。">
      <div className="advanced-settings__monitor" role="group" aria-label="监控服务配置">
        <label className="settings-row__name" htmlFor="advanced-monitor-endpoint">监控服务地址（OTLP）</label>
        <input id="advanced-monitor-endpoint" className="settings-input" value={otlpEndpoint} onChange={event => { setOtlpEndpoint(event.target.value); setFeedback(""); }} placeholder="http://127.0.0.1:4318/v1/logs" spellCheck={false} autoComplete="off" aria-describedby="advanced-monitor-hint" aria-invalid={feedback.startsWith("无法保存") || undefined} />
        <p className="settings-hint" id="advanced-monitor-hint">留空表示关闭，修改后重启应用生效。</p>
        <div className="advanced-settings__actions">
          <button type="button" className="settings-btn" disabled={!endpointDirty} onClick={() => saveOtlp()}>保存</button>
          {endpointDirty && <button type="button" className="settings-btn" onClick={() => { setOtlpEndpoint(savedEndpoint); setFeedback(""); }}>撤销修改</button>}
          {savedEndpoint && <button type="button" className="settings-btn settings-btn--ghost" onClick={() => saveOtlp("")}>关闭监控</button>}
        </div>
        {feedback && <p className={`settings-msg settings-msg--${feedback.startsWith("无法保存") ? "error" : "success"}`} role={feedback.startsWith("无法保存") ? "alert" : "status"}>{feedback}</p>}
      </div>
    </SettingsGroup>
  </SettingsDisclosure>;
}

// ---------- 智能体设置 ----------

/** Runtime behavior settings. Capability inventory belongs to the primary 能力 page. */
export function AgentSettingsPanel() {
  const [subagentDepth, setSubagentDepth] = useState<number | null>(null);
  const [subagentDraft, setSubagentDraft] = useState<string>("");
  const [savingRuntime, setSavingRuntime] = useState(false);
  const [runtimeMsg, setRuntimeMsg] = useState<{ kind: "success" | "error" | "invalid"; text: string } | null>(null);
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState<string | null>(null);

  const depthDirty = subagentDepth !== null && subagentDraft !== String(subagentDepth);
  useSettingsDraft({ dirty: depthDirty, busy: savingRuntime, label: "子代理配置" });

  const reload = useCallback(async () => {
    setLoading(true);
    setLoadError(null);
    try {
      const config = await subagentsConfigGet();
      setSubagentDepth(config.maxDepth);
      setSubagentDraft(String(config.maxDepth));
    } catch (error) {
      setSubagentDepth(null);
      setSubagentDraft("");
      setLoadError(String(error).replace(/^Error:\s*/, ""));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    reload();
  }, [reload]);

  /** Save subagent max_depth. Clamped to ≥1 on the backend. */
  const saveSubagentDepth = useCallback(async () => {
    if (subagentDepth === null || loadError) return;
    const requestedDepth = Number(subagentDraft);
    if (!Number.isInteger(requestedDepth) || requestedDepth < 1 || requestedDepth > 10) {
      setRuntimeMsg({ kind: "invalid", text: "子代理最大层级须为 1 到 10 的整数" });
      return;
    }
    setSavingRuntime(true);
    setRuntimeMsg(null);
    try {
      const clamped = await subagentsConfigSave(requestedDepth);
      setSubagentDepth(clamped);
      setSubagentDraft(String(clamped));
      setRuntimeMsg({ kind: "success", text: `子代理深度已保存为 ${clamped}（重启 Agent 后生效）` });
    } catch (e) {
      setRuntimeMsg({ kind: "error", text: `保存失败：${String(e).replace(/^Error:\s*/, "")}` });
    } finally {
      setSavingRuntime(false);
    }
  }, [loadError, subagentDepth, subagentDraft]);

  return (
    <div className="advanced-settings">
    <SettingsSection title="高级设置" desc="按需排查配置问题，或调整开发者选项。日常使用通常无需修改。">
      <RuntimeReloadSettingsGroup />
      <DeveloperSettingsGroup hasPendingChanges={depthDirty} hasError={Boolean(loadError) || runtimeMsg?.kind === "error" || runtimeMsg?.kind === "invalid"}>
      {loadError && (
        <div className="settings-msg settings-msg--warn" role="alert">
          <span>子代理配置读取失败：{loadError}</span>{" "}
          <button type="button" className="settings-btn" onClick={() => void reload()} disabled={loading}>
            {loading ? "重试中…" : "重试"}
          </button>
        </div>
      )}

      {/* 运行时配置：子代理深度 */}
      <SettingsGroup title="子代理" settingId="subagents">
          {/* 子代理嵌套深度 */}
          <div className="agent-runtime-row">
            <div className="agent-runtime-row__label">
              <span className="agent-runtime-row__name">子代理最大层级</span>
              <span className="agent-runtime-row__hint">
                {subagentDepth === null
                  ? "子代理配置当前不可用，重试成功前不会保存任何默认值。"
                  : `控制任务继续分派的层级。1 表示仅顶层可分派；修改后重启 Agent 生效。`}
              </span>
            </div>
            <div className="agent-runtime-row__control">
              <input
                type="number"
                min={1}
                max={10}
                aria-label="子代理最大层级"
                className="settings-input settings-input--narrow"
                value={subagentDraft}
                onChange={(e) => { setSubagentDraft(e.target.value); setRuntimeMsg(null); }}
                aria-invalid={runtimeMsg?.kind === "invalid" || undefined}
                aria-describedby={runtimeMsg ? "subagent-depth-feedback" : undefined}
                disabled={savingRuntime || loading || subagentDepth === null || !!loadError}
              />
              <button
                className="settings-btn"
                onClick={saveSubagentDepth}
                disabled={
                  savingRuntime ||
                  loading ||
                  subagentDepth === null ||
                  !!loadError ||
                  subagentDraft === String(subagentDepth)
                }
              >
                {savingRuntime ? "保存中…" : "保存"}
              </button>
              {depthDirty && <button className="settings-btn" disabled={savingRuntime} onClick={() => { setSubagentDraft(String(subagentDepth)); setRuntimeMsg(null); }}>撤销修改</button>}
            </div>
          </div>

          {runtimeMsg && (
            <p id="subagent-depth-feedback" className={`settings-msg settings-msg--${runtimeMsg.kind === "success" ? "success" : "error"}`} role={runtimeMsg.kind === "success" ? "status" : "alert"}>{runtimeMsg.text}</p>
          )}
      </SettingsGroup>

      </DeveloperSettingsGroup>
    </SettingsSection>
    </div>
  );
}

// ---------- 通知中心 ----------

const KIND_FILTERS = [
  { key: "all", label: "全部" },
  { key: "unread", label: "未读" },
  { key: "action", label: "需要处理" },
  { key: "result", label: "任务结果" },
  { key: "error", label: "异常" },
  { key: "system", label: "系统事件" },
];

function matchesNotificationFilter(entry: NotificationEntry, filter: string): boolean {
  const actionable = entry.kind === "permission" || entry.kind === "folder_trust";
  const failed = entry.kind === "error" || entry.severity === "error" || entry.severity === "warn";
  switch (filter) {
    case "unread": return !entry.read;
    case "action": return actionable;
    case "result": return entry.kind === "session_complete";
    case "error": return !actionable && failed;
    case "system": return !actionable && entry.kind !== "session_complete" && !failed;
    default: return true;
  }
}

function isStructuredNotificationBody(body: string): boolean {
  try { const value: unknown = JSON.parse(body); return value !== null && typeof value === "object"; }
  catch { return false; }
}

/** Local inbox for actionable requests, failures, and task results. */
export function NotificationCenterSettingsPanel({
  onOpenSession,
  onOpenAutomation,
  onToast,
  onClose,
}: {
  onOpenSession?: (sessionId: string) => void | Promise<void>;
  onOpenAutomation?: (automationId: string) => void | Promise<void>;
  onToast?: (message: string) => void;
  onClose?: () => void;
}) {
  const [entries, setEntries] = useState<NotificationEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const [mutating, setMutating] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [sessionOpenError, setSessionOpenError] = useState<string | null>(null);
  const [filter, setFilter] = useState<string>("all");
  const { requestConfirmation, dialog } = useAppDialog();

  const reload = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      setEntries(await notificationList());
    } catch (loadError) {
      setError(String(loadError).replace(/^Error:\s*/, ""));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    reload();
  }, [reload]);

  const handleMarkRead = useCallback(
    async (id: number) => {
      setMutating(true);
      setError(null);
      try {
        await notificationMarkRead(id);
        await reload();
      } catch (markError) {
        setError(String(markError).replace(/^Error:\s*/, ""));
      } finally {
        setMutating(false);
      }
    },
    [reload],
  );

  const handleMarkAllRead = useCallback(async () => {
    setMutating(true);
    setError(null);
    try {
      await notificationMarkAllRead();
      await reload();
    } catch (markError) {
      setError(String(markError).replace(/^Error:\s*/, ""));
    } finally {
      setMutating(false);
    }
  }, [reload]);

  const handleClear = useCallback(() => {
    requestConfirmation({
      title: "清空所有通知？",
      description: "所有本机通知记录都将被删除，此操作无法撤销。",
      confirmLabel: "清空通知",
      danger: true,
      action: async () => {
        setMutating(true);
        setError(null);
        try {
          await notificationClear();
          await reload();
        } finally {
          setMutating(false);
        }
      },
      onError: (clearError) => setError(String(clearError).replace(/^Error:\s*/, "")),
    });
  }, [reload, requestConfirmation]);

  const openRelatedSession = async (sessionId: string, requestId?: string, notificationId?: number) => {
    if (!onOpenSession) return;
    setSessionOpenError(null);
    try {
      await onOpenSession(sessionId);
      if (requestId) {
        const permissionState = usePermissionStore.getState();
        const closed = permissionState.closedRequestIds.includes(requestId);
        const pending = permissionState.promote(requestId, sessionId);
        if (closed) {
          onToast?.("这项请求已处理");
        } else if (!pending) {
          onToast?.("正在加载待确认的操作");
        }
      }
      if (notificationId != null) void notificationMarkRead(notificationId).catch(() => {});
      onClose?.();
    } catch (openError) {
      setSessionOpenError(String(openError).replace(/^Error:\s*/, ""));
    }
  };

  const openRelatedAutomation = async (automationId: string, notificationId: number) => {
    if (!onOpenAutomation) return;
    setSessionOpenError(null);
    try {
      await onOpenAutomation(automationId);
      void notificationMarkRead(notificationId).catch(() => {});
      onClose?.();
    } catch (openError) {
      setSessionOpenError(String(openError).replace(/^Error:\s*/, ""));
    }
  };

  const filtered = entries.filter(
    (entry) => matchesNotificationFilter(entry, filter),
  );
  const unreadCount = entries.filter((e) => !e.read).length;

  return (
    <SettingsSection
      title="通知记录"
      desc="查看需要处理的请求、任务结果与异常。记录仅保存在本机。"
      actions={
        <>
          <button className="settings-btn" onClick={reload} disabled={loading || mutating}>
            <RefreshCw size={14} /> {loading ? "加载中…" : "刷新"}
          </button>
          <button
            className="settings-btn"
            onClick={handleMarkAllRead}
            disabled={entries.length === 0 || loading || mutating}
          >
            <CheckCheck size={14} /> 全部已读
          </button>
          <button
            className="settings-btn settings-btn--danger"
            onClick={handleClear}
            disabled={(!error && entries.length === 0) || loading || mutating}
          >
            <Trash2 size={14} /> 清空
          </button>
        </>
      }
    >
      {sessionOpenError && (
        <p className="settings-msg settings-msg--warn" role="alert">打开关联任务失败：{sessionOpenError}</p>
      )}
      {error && (
        <p className="settings-msg settings-msg--warn" role="alert">
          通知记录不可用：{error}。原文件未被覆盖；你可以修复文件后重试，或点击“清空”重建。
        </p>
      )}
      <SettingsGroup
        title="最近通知"
        meta={<span className="settings-group__count">{unreadCount} 条未读 · {entries.length} 条记录</span>}
      >
        <div className="notification-filters" aria-label="通知类型筛选">
          <Filter size={13} aria-hidden="true" />
          {KIND_FILTERS.map((f) => (
            <button
              key={f.key}
              className={`notification-filter ${
                filter === f.key ? "notification-filter--active" : ""
              }`}
              onClick={() => setFilter(f.key)}
              aria-pressed={filter === f.key}
            >
              {f.label}
            </button>
          ))}
        </div>
        <div className="notification-list">
          {filtered.length === 0 && !loading && (
            <div className="notification-empty">
              <Mail size={32} color="var(--echo-text-tertiary)" />
              <p>当前筛选条件下暂无通知。</p>
            </div>
          )}
          {filtered.map((entry) => (
            <div
              key={entry.id}
              className={`notification-row notification-row--${entry.severity} ${
                entry.read ? "notification-row--read" : ""
              }`}
            >
              <div
                className={`notification-row__dot notification-row__dot--${notificationDotTone(entry)}`}
              />
              <div className="notification-row__body">
                <div className="notification-row__head">
                  <span className="notification-row__kind">
                    {kindLabel(entry.kind as NotificationKind)}
                  </span>
                  <span className="notification-row__title">{entry.title}</span>
                  {!entry.read && <span className="notification-row__unread">未读</span>}
                </div>
                {entry.body && (isStructuredNotificationBody(entry.body) ? (
                  <details className="notification-row__details">
                    <summary>查看技术详情</summary>
                    <pre className="notification-row__body-text">{entry.body}</pre>
                  </details>
                ) : <p className="notification-row__description">{entry.body}</p>)}
                <div className="notification-row__meta">
                  <span>{formatTime(entry.at)}</span>
                  {entry.sessionId && (
                    onOpenSession ? (
                      <button type="button" className="notification-row__session notification-row__session--link"
                        onClick={() => void openRelatedSession(entry.sessionId!, entry.requestId, entry.id)} disabled={mutating}>
                        {entry.kind === "permission" ? "打开任务处理授权" : "打开相关任务"}
                      </button>
                    ) : <span className="notification-row__session">会话 #{entry.sessionId.slice(0, 8)}</span>
                  )}
                  {!entry.sessionId && entry.automationId && onOpenAutomation && (
                    <button type="button" className="notification-row__session notification-row__session--link"
                      onClick={() => void openRelatedAutomation(entry.automationId!, entry.id)} disabled={mutating}>
                      打开自动化记录
                    </button>
                  )}
                </div>
              </div>
              {!entry.read && (
                <button
                  className="notification-row__mark"
                  onClick={() => void handleMarkRead(entry.id)}
                  disabled={mutating}
                  title="标记已读"
                  aria-label={`将“${entry.title}”标记为已读`}
                >
                  <CheckCheck size={12} />
                </button>
              )}
            </div>
          ))}
          {loading && <div className="notification-empty">正在加载通知…</div>}
        </div>
      </SettingsGroup>
      {dialog}
    </SettingsSection>
  );
}

function kindLabel(kind: NotificationKind | string): string {
  const map: Record<string, string> = {
    permission: "权限请求",
    folder_trust: "文件夹信任",
    task_update: "任务更新",
    plan_mode: "计划模式",
    mcp_status: "工具连接",
    models_update: "模型更新",
    summary: "任务名称更新",
    session_complete: "任务结果",
    error: "错误",
    info: "信息",
  };
  return map[String(kind)] ?? String(kind);
}

function notificationDotTone(entry: NotificationEntry): string {
  switch (entry.severity) {
    case "error":
    case "warn":
      return entry.severity;
    case "info": {
      if (entry.kind !== "session_complete") return "info";
      // Stopped automations also use info. Their body preserves the stop reason if the title is truncated.
      const stopped = entry.title.endsWith("》已停止")
        || entry.body?.startsWith("本次运行已停止，后续计划不受影响");
      return stopped ? "info" : "success";
    }
    default:
      return "info";
  }
}

function formatTime(iso: string): string {
  try {
    const d = new Date(iso);
    return d.toLocaleString("zh-CN", {
      month: "2-digit",
      day: "2-digit",
      hour: "2-digit",
      minute: "2-digit",
      second: "2-digit",
    });
  } catch {
    return iso;
  }
}
