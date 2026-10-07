/**
 * Settings 面板的各个真实分区。
 *
 * 这些是 SettingsPanel.tsx 里除了"模型"以外的运行时设置分区。
 * 每个分区对接 EchoAgent 已有的能力：
 *  - personalize: 主题（接 ThemeProvider）+ 字号
 *  - shortcuts: 当前版本真实生效的快捷键说明
 *  - memory: 记忆行为配置
 *  - help: 版本更新、文档与排查说明
 *  - security: 系统授权、工具规则与诊断
 *  - data: 备份恢复与本地数据目录
 *  - general: 窗口偏好与运行时热重载
 *  - agent-settings: 子代理与 Web 搜索运行时配置
 */
import { useCallback, useEffect, useState } from "react";
import {
  Sun,
  Moon,
  Type,
  Folder,
  Trash2,
  ExternalLink,
  RefreshCw,
  Shield,
  Database,
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
} from "lucide-react";
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
  desktopNotificationPreviewSave,
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

function SectionShell({
  title,
  desc,
  actions,
  children,
}: {
  title: string;
  desc?: string;
  actions?: React.ReactNode;
  children: React.ReactNode;
}) {
  return (
    <div className="settings-section">
      <header className="settings-section__header">
        <div className="settings-section__heading">
          <h2 className="settings-section__title">{title}</h2>
          {desc && <p className="settings-section__desc">{desc}</p>}
        </div>
        {actions && <div className="settings-section__actions">{actions}</div>}
      </header>
      <div className="settings-section__body">{children}</div>
    </div>
  );
}

function SettingsGroup({
  title,
  desc,
  meta,
  children,
  className = "",
}: {
  title: string;
  desc?: string;
  meta?: React.ReactNode;
  children: React.ReactNode;
  className?: string;
}) {
  return (
    <section className={`settings-group${className ? ` ${className}` : ""}`}>
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

export function PersonalizeSettingsPanel() {
  const { theme, preference, setTheme } = useTheme();
  const [fontSize, setFontSize] = useState<number>(readFontSize);

  useEffect(() => {
    saveFontSize(fontSize);
  }, [fontSize]);

  return (
    <SectionShell
      title="个性化"
      desc="调整外观和阅读字号。主题与主要界面文字会立即更新。"
    >
      <SettingsGroup title="界面外观" desc="选择适合当前环境的显示主题和阅读字号。">
        <div className="settings-row settings-row--comfortable">
          <div className="settings-row__label settings-row__label--stacked">
            <span className="settings-row__name">
              {theme === "dark" ? <Moon size={17} /> : <Sun size={17} />}
              主题
            </span>
            <span className="settings-row__description">可固定主题，也可跟随系统外观自动切换</span>
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

        <div className="settings-row settings-row--comfortable">
          <div className="settings-row__label settings-row__label--stacked">
            <span className="settings-row__name"><Type size={17} />字号</span>
            <span className="settings-row__description">当前正文大小为 {fontSize}px</span>
          </div>
          <div className="settings-row__control settings-font-control">
            <span className="settings-font-control__sample settings-font-control__sample--small">A</span>
            <input
              type="range"
              min={MIN_FONT_SIZE}
              max={MAX_FONT_SIZE}
              value={fontSize}
              onChange={(e) => setFontSize(Number(e.target.value))}
              aria-label="界面字号"
            />
            <span className="settings-font-control__sample settings-font-control__sample--large">A</span>
            <button className="settings-reset" onClick={() => setFontSize(DEFAULT_FONT_SIZE)}>
              重置
            </button>
          </div>
        </div>
      </SettingsGroup>
    </SectionShell>
  );
}

// ---------- 快捷键 ----------

export function ShortcutsSettingsPanel() {
  return (
    <SectionShell
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
    </SectionShell>
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

export function MemorySettingsPanel() {
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState<string | null>(null);
  const [config, setConfig] = useState<MemoryConfig>(DEFAULT_MEMORY_CONFIG);
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState(false);
  const [reload, setReload] = useState(0);

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
      setMsg("记忆配置已保存，重启 Agent 后对新会话生效。");
    } catch (e) {
      setConfig(previous);
      setLoadError(true);
      setMsg(`失败：${String(e).replace(/^Error:\s*/, "")}`);
    } finally {
      setBusy(false);
    }
  };

  const toggles: Array<{
    key: "enabled" | "initialInjectionEnabled" | "saveOnEnd" | "watcherEnabled" | "autoFlushEnabled" | "dreamEnabled";
    name: string;
    description: string;
  }> = [
    { key: "enabled", name: "启用记忆", description: "为新会话启用记忆检索、写入和整理能力" },
    { key: "initialInjectionEnabled", name: "会话开始时检索", description: "首轮对话自动注入相关长期记忆" },
    { key: "saveOnEnd", name: "会话结束时保存", description: "有可复用信息时生成可检索的会话摘要" },
    { key: "watcherEnabled", name: "监听外部修改", description: "在应用外修改记忆文件后自动同步检索索引" },
    { key: "autoFlushEnabled", name: "自动提取", description: "空闲或上下文压缩前提取信息并写入会话摘要" },
    { key: "dreamEnabled", name: "自动整理", description: "定期将会话摘要合并为结构化长期记忆" },
  ];

  return (
    <SectionShell
      title="记忆"
      desc="跨会话记忆保存在本机，可随时审阅。会话摘要是自动提取的中间资料，不是完整聊天记录。"
    >
      {loadError && <div role="alert" className="settings-hint">配置未加载，暂时无法修改。<button className="btn-secondary" onClick={() => setReload((n) => n + 1)}>重新加载</button></div>}
      <SettingsGroup title="记忆能力" desc="提取摘要和整理内容会使用当前会话的模型服务。">
        {toggles.map((toggle) => (
          <div className="settings-row settings-row--comfortable" key={toggle.key}>
            <div className="settings-row__label settings-row__label--stacked">
              <span className="settings-row__name">{toggle.name}</span>
              <span className="settings-row__description">{toggle.description}</span>
            </div>
            <label className="sk-toggle">
              <input
                type="checkbox"
                aria-label={toggle.name}
                checked={config[toggle.key]}
                disabled={loading || loadError || busy || (toggle.key !== "enabled" && !config.enabled)}
                onChange={(event) => void updateConfig(toggle.key, event.target.checked)}
              />
              <span className="sk-toggle-track"><span className="sk-toggle-thumb" /></span>
            </label>
          </div>
        ))}
      </SettingsGroup>
      <SettingsGroup title="存储位置" desc="工作区记忆按项目身份存放在此根目录的独立子目录中。">
        <div className="settings-row settings-row--comfortable">
          <div className="settings-row__label settings-row__label--stacked">
            <span className="settings-row__name"><Database size={17} />本地记忆目录</span>
            <span className="settings-row__description">可在“设置 → 记忆 → 个人记忆”中查看、编辑和审阅</span>
          </div>
          <code className="settings-path-chip">~/.echo-agent/memory/</code>
        </div>
      </SettingsGroup>
      {msg && <p className="settings-msg">{msg}</p>}
    </SectionShell>
  );
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

  const openHelpResource = (url: string) => {
    setResourceError("");
    void openExternalUrl(url).catch((error) => {
      setResourceError(`打开链接失败：${String(error).replace(/^Error:\s*/, "")}`);
    });
  };

  return (
    <SectionShell title="帮助与更新" desc="检查版本更新，查阅使用文档和常见问题排查步骤。">
      <SettingsGroup
        title="版本升级"
        desc="启动时会自动检查 EchoAgent 内网上的签名发布版。"
        meta={<span>v{APP_VERSION}</span>}
      >
        <div className="settings-action-row">
          <div className="settings-action-row__content">
            <strong>
              {updateStatus === "checking" && "正在检查更新…"}
              {updateStatus === "available" && `发现新版本 v${update?.version}`}
              {updateStatus === "downloading" && `正在下载 v${update?.version}`}
              {updateStatus === "installing" && "正在安装，完成后将重启"}
              {updateStatus === "up-to-date" && "当前已是最新版本"}
              {updateStatus === "error" && (updateErrorStage === "install" ? "安装更新失败" : updateErrorStage === "download" ? "下载更新失败" : "检查更新失败")}
              {updateStatus === "idle" && "检查 EchoAgent 更新"}
            </strong>
            <span>
              {updateStatus === "available"
                ? (update?.notes?.trim() || "新版本已准备好，安装前会校验发布签名。")
                : updateStatus === "error"
                  ? updateError
                  : checkedAt
                    ? `上次检查：${new Date(checkedAt).toLocaleString("zh-CN")}`
                    : "更新服务仅在内网或 VPN 环境可访问，断网不影响应用启动。"}
            </span>
            {(updateStatus === "downloading" || updateStatus === "installing") && (
              <div className="settings-update-progress" role="progressbar" aria-valuemin={0} aria-valuemax={100} aria-valuenow={updateProgress}>
                <span style={updateProgress === undefined ? undefined : { width: `${updateProgress}%` }} />
              </div>
            )}
          </div>
          {updateStatus === "available" ? (
            <button className="settings-btn settings-btn--primary" onClick={() => void installUpdate()}>
              <Download size={15} /> 下载并安装
            </button>
          ) : (
            <button className="settings-btn" onClick={() => void checkUpdate(true)} disabled={updateBusy}>
              {updateStatus === "checking" ? <Loader2 size={15} className="update-dialog__spinner" />
                : updateStatus === "up-to-date" ? <CheckCircle2 size={15} />
                  : updateStatus === "error" ? <AlertTriangle size={15} />
                    : <RefreshCw size={15} />}
              {updateStatus === "checking" ? "检查中" : "检查更新"}
            </button>
          )}
        </div>
      </SettingsGroup>
      <SettingsGroup title="帮助资源">
        <div className="help-grid">
          <a className="help-card" href={ECHO_AGENT_DOCS_URL} onClick={(event) => { event.preventDefault(); openHelpResource(ECHO_AGENT_DOCS_URL); }}>
            <ExternalLink size={18} />
            <strong>EchoAgent 文档</strong>
            <span>查看功能说明、配置方法与最佳实践</span>
          </a>
          <a className="help-card" href={ACP_SPEC_URL} onClick={(event) => { event.preventDefault(); openHelpResource(ACP_SPEC_URL); }}>
            <ExternalLink size={18} />
            <strong>ACP 协议规范</strong>
            <span>了解智能体客户端协议和运行机制</span>
          </a>
          <div className="help-card help-card--static">
            <Database size={18} />
            <strong>内置运行时</strong>
            <span>当前使用 EchoAgent Runtime，无需额外安装</span>
          </div>
        </div>
      </SettingsGroup>
      {resourceError && <p className="settings-msg settings-msg--warn" role="alert">{resourceError}</p>}
      <SettingsGroup title="快速排查" desc="遇到模型不可用或智能体无法启动时，建议按顺序检查。">
        <ol className="settings-checklist">
          <li><span>1</span><div><strong>确认模型配置</strong><p>在“模型”中确认有可用模型；使用个人服务时，添加连接并填写所需凭据。</p></div></li>
          <li><span>2</span><div><strong>检查配置文件</strong><p>确认 <code>~/.echo-agent/config.toml</code> 可以正常读写。</p></div></li>
          <li><span>3</span><div><strong>重新加载或重启</strong><p>先在“系统设置”尝试热重载，仍无效时再重启应用。</p></div></li>
        </ol>
      </SettingsGroup>
    </SectionShell>
  );
}

// ---------- 安全中心 ----------

export function SecuritySettingsPanel() {
  const [rules, setRules] = useState<PermissionRule[]>([]);
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
  const [otlpEndpoint, setOtlpEndpoint] = useState(() => {
    try { return localStorage.getItem("echoagent.otlp.endpoint") ?? ""; } catch { return ""; }
  });

  const loadRules = useCallback(async () => {
    setLoading(true);
    try {
      const nextRules = await permissionList();
      setRules(nextRules);
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
      setAutomationFeedback("已打开系统权限设置；授权后回到 EchoAgent 即可自动刷新。");
    } catch (error) {
      setAutomationFeedback(`无法打开系统权限：${String(error).replace(/^Error:\s*/, "")}`);
    } finally {
      setAutomationBusy(false);
    }
  };

  const addRule = () => {
    if (!draft.tool.trim()) return;
    setRules((items) => [...items, {
      action: draft.action,
      tool: draft.tool.trim().toLowerCase(),
      pattern: draft.pattern?.trim() || undefined,
    }]);
    setDraft({ action: "ask", tool: "bash", pattern: "" });
  };

  const saveRules = async () => {
    if (loading || rulesError) return;
    setSaving(true);
    setFeedback("");
    try {
      await permissionSave(rules);
      setFeedback("规则已保存；重启 Agent 后对既有会话生效。");
    } catch (error) {
      setFeedback(`保存失败：${String(error).replace(/^Error:\s*/, "")}`);
    } finally {
      setSaving(false);
    }
  };

  const saveOtlp = () => {
    const endpoint = otlpEndpoint.trim();
    if (endpoint) {
      try {
        localStorage.setItem("echoagent.otlp.endpoint", validateOtlpEndpoint(endpoint));
      } catch (error) {
        setFeedback(`OTLP 地址无效：${String(error).replace(/^Error:\s*/, "")}`);
        return;
      }
    } else {
      localStorage.removeItem("echoagent.otlp.endpoint");
    }
    setFeedback(endpoint ? "OTLP 已启用，重启应用后生效。" : "OTLP 已关闭，重启应用后生效。");
  };

  return (
    <SectionShell
      title="安全中心"
      desc="管理网页与电脑操作权限、工具授权规则和本地遥测。"
    >
      <SettingsGroup
        title="网页与电脑操作"
        desc="这些能力默认关闭，只会在你从输入框的 + 菜单为当前任务开启后使用。"
        meta={<span className="settings-status-badge">{automationLoading ? "检查中…" : automationError ? "检查失败" : "按任务开启"}</span>}
      >
        <div className="settings-row settings-row--comfortable">
          <div className="settings-row__label settings-row__label--stacked">
            <span className="settings-row__name"><Globe2 size={17} />操作网页</span>
            <span className="settings-row__description">
              {automationSupport?.browser.available
                ? `${automationSupport.browser.browserName || "兼容浏览器"}已就绪，每个任务使用独立浏览数据`
                : automationSupport?.browser.reason || "正在检查本机浏览器…"}
            </span>
          </div>
          <span className={`settings-status-badge${automationSupport?.browser.available ? " settings-status-badge--ready" : ""}`}>
            {automationSupport ? (automationSupport.browser.available ? "可用" : "不可用") : "未检查"}
          </span>
        </div>
        <div className="settings-row settings-row--comfortable">
          <div className="settings-row__label settings-row__label--stacked">
            <span className="settings-row__name"><Monitor size={17} />操作电脑</span>
            <span className="settings-row__description">
              {automationSupport?.computer.available
                ? automationSupport.computer.screenCapture && automationSupport.computer.inputControl
                  ? "屏幕录制和辅助功能权限已就绪"
                  : automationSupport.computer.reason || "需要屏幕录制和辅助功能权限"
                : automationSupport?.computer.reason || "正在检查系统支持…"}
            </span>
          </div>
          {automationSupport?.computer.available
            && (!automationSupport.computer.screenCapture || !automationSupport.computer.inputControl) ? (
              <button
                type="button"
                className="settings-btn"
                disabled={automationBusy}
                onClick={() => void requestComputerPermissions()}
              >
                {automationBusy ? "打开中…" : "授予系统权限"}
              </button>
            ) : (
              <span className={`settings-status-badge${automationSupport?.computer.available ? " settings-status-badge--ready" : ""}`}>
                {automationSupport ? (automationSupport.computer.available ? "可用" : "不可用") : "未检查"}
              </span>
            )}
        </div>
        {automationError && <p className="settings-msg settings-msg--warn" role="alert">{automationError}</p>}
        {automationFeedback && <p className="settings-hint" role="status">{automationFeedback}</p>}
        <div className="settings-info-callout">
          启用后，相关网页或屏幕内容会作为任务上下文发送给当前模型。浏览器数据按任务隔离，可在任务状态条的“更多”中清除。
        </div>
        <div className="settings-group__footer">
          <button type="button" className="settings-btn" onClick={() => void loadAutomationSupport()} disabled={automationLoading}>
            {automationLoading ? "检查中…" : "重新检查"}
          </button>
        </div>
      </SettingsGroup>
      <SettingsGroup
        title="工具权限规则"
        desc="规则按 deny、ask、allow 的优先级匹配；保存后重启 Agent 生效。"
        meta={<span className="settings-status-badge">{loading ? "加载中…" : rulesError ? "加载失败" : `${rules.length} 条规则`}</span>}
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
          <ul className="rules-list">
            {rules.map((rule, index) => (
              <li key={`${rule.action}-${rule.tool}-${rule.pattern ?? ""}-${index}`} className={`rules-list__item rules-list__item--${rule.action}`}>
                <span className="rules-list__action">{rule.action}</span>
                <span className="rules-list__tool">{rule.tool}</span>
                <span className="rules-list__pattern">{rule.pattern || "所有调用"}</span>
                <button
                  type="button"
                  className="rules-list__remove"
                  onClick={() => setRules((items) => items.filter((_, itemIndex) => itemIndex !== index))}
                  aria-label={`删除规则 ${index + 1}`}
                  disabled={loading || Boolean(rulesError)}
                >
                  ×
                </button>
              </li>
            ))}
          </ul>
        ) : !loading && !rulesError ? (
          <div className="settings-empty-inline">尚未添加自定义权限规则</div>
        ) : null}

        <div className="permission-rule-builder">
          <label className="permission-rule-builder__field">
            <span>处理方式</span>
            <select className="form-control" value={draft.action} onChange={(e) => setDraft({ ...draft, action: e.target.value })} aria-label="规则动作" disabled={loading || Boolean(rulesError)}>
              <option value="deny">拒绝</option>
              <option value="ask">询问</option>
              <option value="allow">允许</option>
            </select>
          </label>
          <label className="permission-rule-builder__field">
            <span>工具类型</span>
            <select className="form-control" value={draft.tool} onChange={(e) => setDraft({ ...draft, tool: e.target.value })} aria-label="工具类型" disabled={loading || Boolean(rulesError)}>
              {['bash', 'read', 'edit', 'grep', 'mcp', 'webfetch', 'any'].map((tool) => <option key={tool} value={tool}>{tool}</option>)}
            </select>
          </label>
          <label className="permission-rule-builder__field permission-rule-builder__field--pattern">
            <span>匹配模式（可选）</span>
            <input className="form-control" value={draft.pattern ?? ""} onChange={(e) => setDraft({ ...draft, pattern: e.target.value })} placeholder="例如 git *" disabled={loading || Boolean(rulesError)} />
          </label>
        </div>
        <div className="settings-group__footer">
          <button className="settings-btn" type="button" onClick={addRule} disabled={loading || Boolean(rulesError)}>添加到列表</button>
          <button className="settings-btn settings-btn--primary" type="button" onClick={() => void saveRules()} disabled={saving || loading || Boolean(rulesError)}>
            {saving ? "保存中…" : "保存全部规则"}
          </button>
        </div>
      </SettingsGroup>
      {feedback && <p className="settings-hint" role="status">{feedback}</p>}
      <SettingsGroup title="遥测与诊断" desc="仅上报运行事件名、级别和技术属性，不包含对话正文。">
        <div className="settings-row settings-row--comfortable">
          <div className="settings-row__label settings-row__label--stacked">
            <span className="settings-row__name"><Shield size={17} />OTLP 遥测端点</span>
            <span className="settings-row__description">留空表示关闭，修改后重启应用生效</span>
          </div>
          <div className="settings-row__control settings-row__control--wide">
            <input
              className="settings-input"
              value={otlpEndpoint}
              onChange={(event) => setOtlpEndpoint(event.target.value)}
              placeholder="http://127.0.0.1:4318/v1/logs"
            />
            <button type="button" className="settings-btn" onClick={saveOtlp}>保存</button>
          </div>
        </div>
      </SettingsGroup>
    </SectionShell>
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
    <SectionShell title="数据管理" desc="查看 EchoAgent 在本机保存的数据位置和内容范围。">
      <SettingsGroup title="备份与恢复" desc="备份会话、项目资料、代码任务记录、记忆、粘贴附件、草稿、队列和本机用量。不包含模型密钥、组织凭据或外部工作目录的文件。">
        <p className="settings-hint">建议在任务结束后备份。备份包含聊天和资料正文，请保存在可信位置。恢复会替换备份中同名的数据，其他数据保留；历史外部文件仍依赖原工作目录。</p>
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
          }}>选择备份恢复</button>
        </div>
      </SettingsGroup>
      <SettingsGroup title="本地数据目录" desc="会话、项目、自动化、通知和配置都保存在此目录。">
        <div className="settings-row settings-row--comfortable settings-row--path">
          <div className="settings-row__label settings-row__label--stacked">
            <span className="settings-row__name"><Folder size={17} />EchoAgent 数据目录</span>
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
      {message && <p className="settings-msg" role="status">{message}</p>}
      {dialog}
      <SettingsGroup title="数据安全" desc="建议通过应用内入口管理数据，避免直接删除目录中的文件。">
        <div className="settings-info-callout">
          删除会话请在侧栏对单个会话操作；直接修改或清理目录可能导致项目、通知或配置无法恢复。
        </div>
      </SettingsGroup>
    </SectionShell>
  );
}

// ---------- 系统设置 ----------

export function GeneralSettingsPanel() {
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState<string | null>(null);
  const [closeToTray, setCloseToTray] = useState(true);
  const [loadingDesktopPreferences, setLoadingDesktopPreferences] = useState(true);
  const [desktopPreferencesError, setDesktopPreferencesError] = useState<string | null>(null);
  const [desktopPreferencesReload, setDesktopPreferencesReload] = useState(0);
  const [savingDesktopPreferences, setSavingDesktopPreferences] = useState(false);

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

  const handleReload = async (kind: "mcp_all" | "skills" | "models") => {
    setBusy(true);
    try {
      await internalReload(kind);
      setMsg(`已触发 ${kind} 热重载`);
    } catch (e) {
      setMsg(`失败：${String(e).replace(/^Error:\s*/, "")}`);
    } finally {
      setBusy(false);
    }
  };

  return (
    <SectionShell
      title="系统设置"
      desc="管理窗口、后台运行与 EchoAgent 配置重载。"
    >
      <SettingsGroup title="窗口与后台运行" desc="控制关闭主窗口时是否保留 Runtime 与后台任务。">
        <div className="settings-row settings-row--comfortable">
          <div className="settings-row__label settings-row__label--stacked">
            <span className="settings-row__name"><AppWindow size={17} />关闭窗口时继续后台运行</span>
            <span className="settings-row__description">
              关闭后可从系统托盘重新打开；从托盘选择“退出 EchoAgent”才会完全退出。
            </span>
          </div>
          <label className="sk-toggle">
            <input
              type="checkbox"
              aria-label="关闭窗口时继续后台运行"
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
            关闭主窗口将停止当前 Runtime 和自动化调度。仍可使用最小化保留窗口与任务。
          </div>
        )}
      </SettingsGroup>
      <SettingsGroup title="运行时热重载" desc="只刷新对应配置，不关闭当前窗口或中断其他页面。">
        <div className="settings-action-row">
          <div className="settings-action-row__content"><strong>MCP 连接器</strong><span>重新读取所有 MCP 服务和工具配置。</span></div>
          <button className="settings-btn" onClick={() => handleReload("mcp_all")} disabled={busy}><RefreshCw size={15} />重新加载</button>
        </div>
        <div className="settings-action-row">
          <div className="settings-action-row__content"><strong>技能目录</strong><span>重新扫描本地、项目和组织下发的技能。</span></div>
          <button className="settings-btn" onClick={() => handleReload("skills")} disabled={busy}><RefreshCw size={15} />重新加载</button>
        </div>
        <div className="settings-action-row">
          <div className="settings-action-row__content"><strong>模型配置</strong><span>重新读取厂商、凭据和模型目录。</span></div>
          <button className="settings-btn" onClick={() => handleReload("models")} disabled={busy}><RefreshCw size={15} />重新加载</button>
        </div>
      </SettingsGroup>
      {msg && <p className="settings-msg">{msg}</p>}
    </SectionShell>
  );
}

// ---------- 智能体设置 ----------

/** Runtime behavior settings. Capability inventory belongs to the primary 能力 page. */
export function AgentSettingsPanel() {
  const [subagentDepth, setSubagentDepth] = useState<number | null>(null);
  const [subagentDraft, setSubagentDraft] = useState<string>("");
  const [savingRuntime, setSavingRuntime] = useState(false);
  const [runtimeMsg, setRuntimeMsg] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState<string | null>(null);

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
      setRuntimeMsg("子代理嵌套深度须为 1 到 10 的整数");
      return;
    }
    setSavingRuntime(true);
    setRuntimeMsg(null);
    try {
      const clamped = await subagentsConfigSave(requestedDepth);
      setSubagentDepth(clamped);
      setSubagentDraft(String(clamped));
      setRuntimeMsg(`子代理深度已保存为 ${clamped}（重启 agent 后生效）`);
    } catch (e) {
      setRuntimeMsg(`保存失败：${String(e).replace(/^Error:\s*/, "")}`);
    } finally {
      setSavingRuntime(false);
    }
  }, [loadError, subagentDepth, subagentDraft]);

  return (
    <SectionShell
      title="智能体设置"
      desc="调整子代理的运行方式。技能、连接器与插件请在左侧「扩展」中管理。"
      actions={(
        <button className="settings-btn" onClick={reload} disabled={loading}>
          <RefreshCw size={15} /> {loading ? "加载中…" : "刷新状态"}
        </button>
      )}
    >
      {loadError && (
        <div className="settings-msg settings-msg--warn" role="alert">
          <span>子代理配置读取失败：{loadError}</span>{" "}
          <button type="button" className="settings-btn" onClick={() => void reload()} disabled={loading}>
            {loading ? "重试中…" : "重试"}
          </button>
        </div>
      )}

      {/* 运行时配置：子代理深度 */}
      <SettingsGroup title="运行时配置" desc="以下修改需要重启 Agent 后生效。">
          {/* 子代理嵌套深度 */}
          <div className="agent-runtime-row">
            <div className="agent-runtime-row__label">
              <span className="agent-runtime-row__name">子代理嵌套深度</span>
              <span className="agent-runtime-row__hint">
                {subagentDepth === null
                  ? "子代理配置当前不可用，重试成功前不会保存任何默认值。"
                  : `最大子代理派发层级，当前为 ${subagentDepth}。深度 1 表示仅顶层可以派发。`}
              </span>
            </div>
            <div className="agent-runtime-row__control">
              <input
                type="number"
                min={1}
                max={10}
                aria-label="子代理嵌套深度"
                className="settings-input settings-input--narrow"
                value={subagentDraft}
                onChange={(e) => setSubagentDraft(e.target.value)}
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
            </div>
          </div>

          {runtimeMsg && (
            <p className="settings-msg settings-msg--info">{runtimeMsg}</p>
          )}
      </SettingsGroup>

    </SectionShell>
  );
}

// ---------- 通知中心 ----------

const KIND_FILTERS: { key: string; label: string }[] = [
  { key: "all", label: "全部" },
  { key: "permission", label: "权限请求" },
  { key: "folder_trust", label: "文件夹信任" },
  { key: "mcp_status", label: "MCP 状态" },
  { key: "session_complete", label: "任务结果" },
  { key: "error", label: "错误" },
];

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
  const [showTaskTitle, setShowTaskTitle] = useState(false);
  const [previewLoading, setPreviewLoading] = useState(true);
  const [previewSaving, setPreviewSaving] = useState(false);
  const [previewError, setPreviewError] = useState<string | null>(null);
  const [previewReload, setPreviewReload] = useState(0);
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

  useEffect(() => {
    let active = true;
    setPreviewLoading(true);
    setPreviewError(null);
    void desktopPreferencesGet().then((preferences) => {
      if (active) setShowTaskTitle(preferences.showNotificationTaskTitle ?? false);
    }).catch((loadError) => {
      if (active) setPreviewError(String(loadError).replace(/^Error:\s*/, ""));
    }).finally(() => {
      if (active) setPreviewLoading(false);
    });
    return () => { active = false; };
  }, [previewReload]);

  const savePreview = async (enabled: boolean) => {
    setPreviewSaving(true);
    setPreviewError(null);
    try {
      const saved = await desktopNotificationPreviewSave(enabled);
      setShowTaskTitle(saved.showNotificationTaskTitle);
    } catch (saveError) {
      setPreviewError(String(saveError).replace(/^Error:\s*/, ""));
    } finally {
      setPreviewSaving(false);
    }
  };

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
          onToast?.("这项授权请求已处理，已打开关联任务");
        } else if (!pending) {
          onToast?.("已打开关联任务，正在等待授权请求显示");
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
    (e) => filter === "all" || String(e.kind) === filter,
  );
  const unreadCount = entries.filter((e) => !e.read).length;

  return (
    <SectionShell
      title="通知中心"
      desc="集中查看待处理请求、连接问题和任务结果。"
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
      <SettingsGroup title="桌面通知预览" desc="控制系统通知是否显示任务名称；任务通知点击后会打开关联任务。">
        <div className="settings-row settings-row--comfortable">
          <div className="settings-row__label settings-row__label--stacked">
            <span className="settings-row__name">显示任务名称</span>
            <span className="settings-row__description">默认隐藏，避免任务标题出现在锁屏通知中。应用内通知记录仍显示完整信息。</span>
          </div>
          <label className="sk-toggle">
            <input type="checkbox" aria-label="桌面通知显示任务名称" checked={showTaskTitle}
              disabled={previewLoading || previewSaving || Boolean(previewError)}
              onChange={(event) => void savePreview(event.target.checked)} />
            <span className="sk-toggle-track"><span className="sk-toggle-thumb" /></span>
          </label>
        </div>
        {previewError && <p className="settings-msg settings-msg--warn" role="alert">
          桌面通知偏好不可用：{previewError}
          <button type="button" className="settings-btn" onClick={() => setPreviewReload((value) => value + 1)}>重试</button>
        </p>}
      </SettingsGroup>
      <SettingsGroup
        title="通知概览"
        desc="通知仅保存在本机，可随时标记已读或清空。"
        meta={
          <span
            className={`settings-status-badge ${
              unreadCount > 0 ? "settings-status-badge--accent" : "settings-status-badge--ready"
            }`}
          >
            {unreadCount > 0 ? `${unreadCount} 条未读` : "全部已读"}
          </span>
        }
      >
        <div className="settings-row settings-row--comfortable">
          <div className="settings-row__label settings-row__label--stacked">
            <span className="settings-row__name">
              <Mail size={16} /> 通知数量
            </span>
            <span className="settings-row__description">
              当前共记录 {entries.length} 条通知，其中 {unreadCount} 条尚未阅读。
            </span>
          </div>
          <div className="settings-row__control">
            <span className="settings-metric">{unreadCount}</span>
            <span className="settings-metric__suffix">未读 / {entries.length} 全部</span>
          </div>
        </div>
      </SettingsGroup>

      <SettingsGroup
        title="通知记录"
        desc="按事件类型筛选，快速定位需要处理的信息。"
        meta={<span className="settings-group__count">{filtered.length} 条</span>}
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
                className={`notification-row__dot notification-row__dot--${severityToDot(entry.severity)}`}
              />
              <div className="notification-row__body">
                <div className="notification-row__head">
                  <span className="notification-row__kind">
                    {kindLabel(entry.kind as NotificationKind)}
                  </span>
                  <span className="notification-row__title">{entry.title}</span>
                  {!entry.read && <span className="notification-row__unread">未读</span>}
                </div>
                {entry.body && (
                  <pre className="notification-row__body-text">{entry.body}</pre>
                )}
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
    </SectionShell>
  );
}

function kindLabel(kind: NotificationKind | string): string {
  const map: Record<string, string> = {
    permission: "权限请求",
    folder_trust: "文件夹信任",
    task_update: "任务更新",
    plan_mode: "计划模式",
    mcp_status: "MCP 状态",
    models_update: "模型更新",
    summary: "会话标题",
    session_complete: "任务结果",
    error: "错误",
    info: "信息",
  };
  return map[String(kind)] ?? String(kind);
}

function severityToDot(severity: string): string {
  switch (severity) {
    case "error":
      return "error";
    case "warn":
      return "warn";
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
