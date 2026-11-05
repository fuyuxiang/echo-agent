import { ResourceDescription, ResourceIcon, ResourceList, ResourceMenu, ResourceRow } from "./ResourceList";
/**
 * 插件面板 - 对接 EchoAgent 的 echo.agent/plugins/list + echo.agent/plugins/action
 *
 * 显示已安装的 EchoAgent 插件（含 skills/agents/hooks/mcp 计数），支持启用/禁用。
 * 对应 EchoAgent 的 plugins-panel。
 *
 * 插件来源：~/.echo-agent/plugins/、项目 .echo-agent/plugins/ 及已配置的本地目录。
 */
import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import {
  PuzzlePieceIcon,
  RefreshCwIcon,
  SparklesIcon,
  SkillIcon,
  AgentToolIcon,
  McpIcon,
} from "@/foundation/components/Icon/icons";
import { pluginsAction, pluginsList } from "@/lib/agent-client";
import { openLocalPath } from "@/lib/markdown-host";
import { ensureSession } from "@/lib/ensure-session";
import { useAppDialog } from "./AppDialog";
import type { PluginEntry } from "@/lib/types";
import { CapabilityEmptyState, CapabilityError, CapabilityPage } from "./experts-panel/shared/CapabilityPage";

interface PluginsPanelProps {
  /** Current session id (for the plugins/list call). */
  sessionId?: string;
  onToast?: (msg: string) => void;
}

export function PluginsPanel({ sessionId, onToast }: PluginsPanelProps) {
  const [plugins, setPlugins] = useState<PluginEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [search, setSearch] = useState("");
  const dialog = useAppDialog("plugin-management");
  const previousSession = useRef(sessionId);
  useLayoutEffect(() => { if (previousSession.current && previousSession.current !== sessionId) dialog.dismiss(); previousSession.current = sessionId; }, [sessionId]);
  const reloadGeneration = useRef(0);
  const actionInFlight = useRef(false);
  const currentSession = useRef(sessionId);
  const scope = useRef({ id: sessionId, epoch: 0 });
  if (scope.current.id !== sessionId) scope.current = { id: sessionId, epoch: scope.current.epoch + 1 };
  const mounted = useRef(true);
  useEffect(() => { mounted.current = true; return () => { mounted.current = false; }; }, []);
  currentSession.current = sessionId;

  const loadSnapshot = useCallback(async (target: string | undefined, rescan = false) => {
    const generation = ++reloadGeneration.current;
    const epoch = scope.current.epoch;
    const active = () => mounted.current && generation === reloadGeneration.current && epoch === scope.current.epoch;
    setLoading(true);
    setLoadError(null);
    try {
      if (rescan && target) {
        await pluginsAction(target, { type: "reload" });
        if (!active()) return null;
      }
      const resp = await pluginsList(target);
      if (!active()) return null;
      setPlugins(resp.plugins ?? []);
      return resp.plugins ?? [];
    } catch (e) {
      if (!active()) return null;
      const message = String(e).replace(/^Error:\s*/, "");
      setLoadError(message);
      throw e;
    } finally {
      if (active()) setLoading(false);
    }
  }, []);
  const reload = useCallback(async (rescan = false) => {
    try { await loadSnapshot(sessionId, rescan); }
    catch (e) { onToast?.(`加载插件失败：${String(e).replace(/^Error:\s*/, "")}`); }
  }, [sessionId, onToast, loadSnapshot]);

  useEffect(() => {
    void reload();
    return () => { reloadGeneration.current += 1; };
  }, [reload]);

  useEffect(() => { setPlugins([]); }, [sessionId]);

  const handleToggle = useCallback(
    async (plugin: PluginEntry) => {
      if (actionInFlight.current) return;
      const pluginId = plugin.id ?? plugin.name;
      actionInFlight.current = true;
      const start = scope.current;
      setBusy(pluginId);
      try {
        const targetSession = sessionId ?? await ensureSession();
        const active = () => mounted.current && (scope.current.epoch === start.epoch || (!start.id && scope.current.epoch === start.epoch + 1 && currentSession.current === targetSession));
        if (!active()) return;
        await pluginsAction(targetSession, {
          type: plugin.enabled ? "disable" : "enable",
          // Runtime actions use the stable backend-generated ID; display names
          // are not unique across project/user scopes.
          pluginId,
        });
        if (!active()) return;

        await loadSnapshot(targetSession);
      } catch (e) {
        if (mounted.current && (scope.current.epoch === start.epoch || (!start.id && scope.current.epoch === start.epoch + 1))) onToast?.(`操作失败：${String(e).replace(/^Error:\s*/, "")}`);
      } finally {
        actionInFlight.current = false;
        setBusy(null);
      }
    },
    [sessionId, onToast, loadSnapshot],
  );

  const removePlugin = (plugin: PluginEntry) => {
    const uninstall = plugin.management?.uninstallable;
    const names = plugin.management?.packagePlugins ?? [];
    const label = uninstall ? "卸载插件" : "移除配置";
    let applied = false;
    dialog.requestConfirmation({ title: `${label}“${plugin.name}”？`, danger: true, confirmLabel: label,
      description: uninstall ? (names.length > 1 ? `该安装包包含 ${names.join("、")}，卸载会一并移除这些插件。工作区文件保留。` : "将移除本机安装包及其中的能力。工作区文件保留。") : `将从插件路径配置中移除，原始目录和文件会保留。${(plugin.management?.pathPlugins?.length ?? 0) > 1 ? `此路径还提供：${plugin.management!.pathPlugins!.join("、")}。` : ""}`,
      action: async () => {
        if (actionInFlight.current) throw new Error("其他插件操作正在进行，请稍后重试");
        actionInFlight.current = true; setBusy(plugin.id ?? plugin.name);
        const start = scope.current;
        try {
          const target = sessionId ?? await ensureSession();
          const active = () => mounted.current && (scope.current.epoch === start.epoch || (!start.id && scope.current.epoch === start.epoch + 1 && currentSession.current === target));
          if (!active()) return;
          if (!applied) {
            await pluginsAction(target, uninstall ? { type: "uninstall", pluginId: plugin.id, confirmed: true } : { type: "remove", path: plugin.management?.removalPath ?? plugin.root });
            applied = true;
          }
          if (!active()) return;
          const response = await loadSnapshot(target, true);
          if (!active() || !response) return;
          if (response.some(entry => entry.id === plugin.id && entry.root === plugin.root)) {
            throw new Error("操作已提交，但插件仍在加载列表中，请刷新后检查来源配置。");
          }
        } finally { actionInFlight.current = false; setBusy(null); }
      },
    });
  };

  const enabledCount = plugins.filter((p) => p.enabled).length;
  const trustedCount = plugins.filter((p) => p.trusted).length;
  const query = search.trim().toLowerCase();
  const visible = plugins.filter(plugin => !query || [plugin.name, plugin.description, plugin.root, scopeLabel(plugin.scope)]
    .some(value => value?.toLowerCase().includes(query)));

  return (
    <CapabilityPage title="已安装插件" className="capability-page--plugins"
      search={{ value: search, onChange: setSearch, label: "搜索插件", placeholder: "搜索插件…" }}
      actions={
        <button
          type="button" className="capability-button"
          onClick={() => void reload(true)}
          disabled={loading || busy !== null}
          title="刷新"
        >
          <RefreshCwIcon size="sm" /> 刷新
        </button>
      }
      summary={loadError && plugins.length === 0
          ? "插件列表未加载"
          : loading && plugins.length === 0 ? "正在读取插件…" : undefined}>


      <div className="plugins-panel__list">
        {loadError && (
          <CapabilityError onRetry={() => void reload(true)} retrying={loading || busy !== null}>
            插件列表加载失败：{loadError}{plugins.length > 0 ? "。下方保留上次加载的结果。" : ""}
          </CapabilityError>
        )}
        {plugins.length === 0 && !loading && !loadError && (
          <CapabilityEmptyState icon={<PuzzlePieceIcon size="lg" />} title="暂无插件。"
            description="支持加载本地插件，安装位置和加载方式见下方说明。" />
        )}
        {plugins.length > 0 && visible.length === 0 && (
          <CapabilityEmptyState icon={<PuzzlePieceIcon size="lg" />} title={`没有找到与「${search}」匹配的插件`} onClear={() => setSearch("")} />
        )}
        {visible.length > 0 && <ResourceList collectionKey="plugins" summary={query ? `${visible.length} 个匹配插件` : `${plugins.length} 个插件 · ${enabledCount} 启用 · ${trustedCount} 受信任`} aria-label="已安装插件">{visible.map((p) => (
          <ResourceRow
            key={p.id ?? p.name}
            className={`plugins-panel__item ${p.enabled ? "" : "plugins-panel__item--muted"}`}
          >
            <ResourceIcon><PuzzlePieceIcon size="md" /></ResourceIcon>
            <div className="plugins-panel__item-content resource-row__body">
              <div className="plugins-panel__item-name resource-row__title">
                {p.name}
                {p.version && (
                  <span className="plugins-panel__version resource-tag">v{p.version}</span>
                )}
                <span className={`plugins-panel__scope resource-tag plugins-panel__scope--${p.scope}`}>
                  {scopeLabel(p.scope)}
                </span>
                {!p.trusted && (
                  <span className="plugins-panel__warn resource-tag resource-action--danger">未受信任</span>
                )}
              </div>
              {p.description && (
                <ResourceDescription text={p.description} label={`插件「${p.name}」的说明`} className="plugins-panel__item-desc" />
              )}
              <div className="plugins-panel__item-meta resource-row__meta">
                {p.skillCount !== undefined && p.skillCount > 0 && (
                  <span title="技能"><SkillIcon size="sm" /> {p.skillCount}</span>
                )}
                {p.agentCount !== undefined && p.agentCount > 0 && (
                  <span title="专家"><AgentToolIcon size="sm" /> {p.agentCount}</span>
                )}
                {p.mcpServerCount !== undefined && p.mcpServerCount > 0 && (
                  <span title="MCP"><McpIcon size="sm" /> {p.mcpServerCount}</span>
                )}
                {p.hookCount !== undefined && p.hookCount > 0 && (
                  <span title="Hooks"><SparklesIcon size="sm" /> {p.hookCount}</span>
                )}
              </div>
            </div>
            <div className="plugins-panel__item-actions resource-row__actions">
              <button
                type="button"
                aria-pressed={p.enabled}
                aria-label={`${p.enabled ? "禁用" : "启用"}插件 ${p.name}`}
                className={`plugins-panel__toggle ${p.enabled ? "plugins-panel__toggle--on" : ""}`}
                onClick={() => handleToggle(p)}
                disabled={loading || busy !== null}
                title={p.enabled ? "禁用" : "启用"}
              >
                {busy === (p.id ?? p.name) ? "…" : p.enabled ? "已启用" : "已禁用"}
              </button>
              <ResourceMenu label={`插件 ${p.name}`} disabled={loading || busy !== null} actions={[
                ...(p.root ? [{ label: "来源目录", onSelect: () => { void openLocalPath(p.root!, { revealFile: true }).catch(error => onToast?.(`无法打开插件来源：${String(error)}`)); } }] : []),
                ...(p.management?.uninstallable || p.management?.removable ? [{ label: `${p.management.uninstallable ? "卸载" : "移除"}插件 ${p.name}`, text: p.management.uninstallable ? "卸载插件" : "移除插件", danger: true, onSelect: () => removePlugin(p) }] : []),
              ]} />
            </div>
          </ResourceRow>
        ))}</ResourceList>}
        {loading && <div className="capability-loading" role="status">加载中…</div>}
      </div>
      <details className="capability-help">
        <summary>本地插件加载说明</summary>
        <p>默认用户插件目录为 <code>~/.echo-agent/plugins/</code>，项目插件目录为 <code>.echo-agent/plugins/</code>；自定义数据目录下使用对应的 <code>plugins/</code> 文件夹。</p>
        <p>新建任务时会加载本地插件；已有任务可点击「刷新」重新加载。</p>
      </details>
      {dialog.dialog}
    </CapabilityPage>
  );
}

function scopeLabel(scope?: string): string {
  switch (scope) {
    case "user":
      return "用户";
    case "project":
      return "项目";
    case "config":
      return "配置";
    case "cli":
      return "命令行";
    default:
      return scope ?? "";
  }
}
