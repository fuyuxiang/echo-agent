import { ResourceIcon, ResourceList, ResourceMenu, ResourceRow } from "../../ResourceList";
import { useCallback, useEffect, useRef, useState } from "react";
import {
  XCloseIcon, SearchIcon, ConfigureIcon, McpIcon, RefreshCwIcon,
} from "@/foundation/components/Icon/icons";
import {
  mcpAuthTrigger, mcpDelete, mcpList, mcpSetup, mcpToggle, mcpToggleTool, onMcpStatusEvent,
} from "@/lib/agent-client";
import { ensureSession } from "@/lib/ensure-session";
import { useSessionStore } from "@/stores/session-store";
import type { McpServerEntry } from "@/lib/types";
import { McpConfigEditor } from "./McpConfigEditor";
import { useModalFocus } from "@/lib/use-modal-focus";
import { useAppDialog } from "../../AppDialog";
import { CapabilityEmptyState, CapabilityError, CapabilityPage } from "../shared/CapabilityPage";

/** MCP management with persisted state, live health and per-server tool details. */
export function McpModal({
  onClose, onToast, initialEditing = false, embedded = false,
}: {
  onClose: () => void;
  onToast?: (m: string) => void;
  initialEditing?: boolean;
  /** Render directly inside the connectors page instead of as a modal. */
  embedded?: boolean;
}) {
  const [editing, setEditing] = useState(initialEditing);
  const [servers, setServers] = useState<McpServerEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const [refreshing, setRefreshing] = useState(false);
  const [search, setSearch] = useState("");
  const [error, setError] = useState("");
  const [expanded, setExpanded] = useState<string | null>(null);
  const [mutating, setMutating] = useState<string | null>(null);
  const [setupDrafts, setSetupDrafts] = useState<Record<string, Record<string, string>>>({});
  const [diagnostics, setDiagnostics] = useState<Record<string, { status?: string; reason?: string; detail?: string }>>({});
  const [authError, setAuthError] = useState<{ name: string; message: string } | null>(null);
  const authInFlight = useRef(false);
  const mounted = useRef(true);
  const dialogRef = useModalFocus<HTMLDivElement>(!embedded, onClose);
  const activeSessionId = useSessionStore((state) => state.sessionId);
  const { requestConfirmation, dialog } = useAppDialog(activeSessionId);
  const reloadGeneration = useRef(0);

  const reload = useCallback(async (refresh = false) => {
    const generation = ++reloadGeneration.current;
    refresh ? setRefreshing(true) : setLoading(true);
    setError("");
    try {
      const next = await mcpList(activeSessionId ?? undefined, refresh);
      if (reloadGeneration.current === generation) setServers(next);
    } catch (e) {
      if (reloadGeneration.current !== generation) return;
      const message = String(e).replace(/^Error:\s*/, "");
      setError(message);
      if (refresh) onToast?.(`MCP 诊断失败：${message}`);
    } finally {
      if (reloadGeneration.current === generation) {
        setLoading(false);
        setRefreshing(false);
      }
    }
  }, [activeSessionId, onToast]);

  useEffect(() => {
    void reload();
    return () => {
      reloadGeneration.current += 1;
    };
  }, [reload]);
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let disposed = false;
    onMcpStatusEvent((payload) => {
      if (disposed || payload.sessionId !== activeSessionId) return;
      const status = payload as { name?: string; status?: string; reason?: string; detail?: string };
      if (status.name) {
        setDiagnostics((current) => ({ ...current, [status.name!]: status }));
      }
      if (timer) clearTimeout(timer);
      timer = setTimeout(() => { void reload(); }, 250);
    }).then((fn) => { if (disposed) fn(); else unlisten = fn; }).catch(() => {});
    return () => {
      disposed = true;
      if (timer) clearTimeout(timer);
      unlisten?.();
    };
  }, [reload, activeSessionId]);

  useEffect(() => {
    setDiagnostics({});
    setAuthError(null);
  }, [activeSessionId]);
  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; };
  }, []);

  const handleAuthorize = async (server: McpServerEntry) => {
    if (authInFlight.current) return;
    authInFlight.current = true;
    setAuthError(null);
    setMutating(`auth:${server.name}`);
    let targetSession = activeSessionId;
    try {
      const sessionId = activeSessionId ?? await ensureSession();
      targetSession = sessionId;
      if (!mounted.current) return;
      if (useSessionStore.getState().sessionId !== sessionId) throw new Error("活动会话已改变，请在当前会话重试授权");
      const result = await mcpAuthTrigger(sessionId, server.name);
      if (!mounted.current || useSessionStore.getState().sessionId !== sessionId) return;
      if (result.status !== "authenticated") {
        throw new Error(result.error || (result.status === "setup_required"
          ? "请先完成此服务的配置，再重新授权"
          : "授权未完成或已取消，请重试"));
      }

      // Read the current store session, including one lazily created above.
      const generation = ++reloadGeneration.current;
      try {
        const next = await mcpList(sessionId, true);
        if (mounted.current && reloadGeneration.current === generation && useSessionStore.getState().sessionId === sessionId) setServers(next);
      } catch (reason) {
        if (mounted.current && reloadGeneration.current === generation && useSessionStore.getState().sessionId === sessionId) {
          setError(`授权已完成，但连接状态刷新失败：${String(reason).replace(/^Error:\s*/, "")}。请点击「刷新诊断」。`);
        }
      } finally {
        if (mounted.current && reloadGeneration.current === generation) {
          setLoading(false);
          setRefreshing(false);
        }
      }
    } catch (reason) {
      if (!mounted.current || useSessionStore.getState().sessionId !== targetSession) return;
      const message = String(reason).replace(/^Error:\s*/, "");
      setAuthError({ name: server.name, message });
      onToast?.(`授权失败：${message}`);
    } finally {
      authInFlight.current = false;
      if (mounted.current) setMutating(null);
    }
  };

  const filtered = servers.filter((server) => {
    const query = search.trim().toLowerCase();
    return !query
      || server.name.toLowerCase().includes(query)
      || server.displayName?.toLowerCase().includes(query)
      || server.target?.toLowerCase().includes(query);
  });

  const handleToggle = async (server: McpServerEntry, enabled: boolean) => {
    setMutating(server.name);
    try {
      const result = await mcpToggle(
        activeSessionId ?? undefined,
        server.name,
        enabled,
      );
      if (result.warnings[0]) onToast?.(result.warnings[0]);
      await reload();
    } catch (e) {
      onToast?.(`切换失败：${String(e).replace(/^Error:\s*/, "")}`);
    } finally {
      setMutating(null);
    }
  };

  const handleDelete = (server: McpServerEntry) => {
    if (!server.editable) {
      onToast?.("该服务来自项目或插件，请在对应来源中管理");
      return;
    }
    requestConfirmation({
      title: `删除 MCP 服务“${server.displayName || server.name}”？`,
      description: "将移除本地服务配置。如需撤销第三方授权，还需前往对应服务处理。",
      confirmLabel: "删除服务",
      danger: true,
      action: async () => {
        setMutating(server.name);
        try {
          const result = await mcpDelete(
            activeSessionId ?? undefined,
            server.name,
          );
          if (expanded === server.name) setExpanded(null);
          await reload();
          if (result.warnings[0]) onToast?.(`连接器已移除，但仍需注意：${result.warnings[0]}`);
        } finally {
          setMutating(null);
        }
      },
    });
  };

  const handleSetup = async (server: McpServerEntry) => {
    if (!server.setup) return;
    const draft = setupDrafts[server.name] ?? {};
    const values = Object.fromEntries(server.setup.fields.map((field) => [
      field.id,
      draft[field.id] ?? server.setupValues[field.id] ?? field.default ?? "",
    ]).filter(([, value]) => Boolean(value)));
    const missing = server.setup.fields.find((field) => field.required && !values[field.id]);
    if (missing) {
      onToast?.(`请选择「${missing.label}」`);
      return;
    }
    setMutating(`setup:${server.name}`);
    try {
      const sessionId = activeSessionId ?? await ensureSession();
      await mcpSetup(sessionId, server.name, values);

      await reload(true);
    } catch (e) {
      onToast?.(`配置失败：${String(e).replace(/^Error:\s*/, "")}`);
    } finally {
      setMutating(null);
    }
  };

  const handleToolToggle = async (server: McpServerEntry, toolName: string, enabled: boolean) => {
    const key = `tool:${server.name}:${toolName}`;
    setMutating(key);
    try {
      const sessionId = activeSessionId;
      if (!sessionId) throw new Error("工具尚未加载到活动会话");
      await mcpToggleTool(sessionId, server.name, toolName, enabled);

      await reload();
    } catch (e) {
      onToast?.(`工具切换失败：${String(e).replace(/^Error:\s*/, "")}`);
    } finally {
      setMutating(null);
    }
  };

  const listContent = (
    <>
      {error && <CapabilityError onRetry={() => void reload(true)} retrying={loading || refreshing}>
        {error}{servers.length > 0 ? "。下方保留上次加载的结果。" : ""}
      </CapabilityError>}

      {loading && servers.length === 0 ? (
        <div className="capability-loading" role="status">加载中…</div>
      ) : error && servers.length === 0 ? null : servers.length === 0 ? (
        <CapabilityEmptyState icon={<McpIcon size="lg" />}
          title={embedded ? "还没有已配置的连接器" : "暂无 MCP 服务"}
          description={embedded ? "点击「配置连接器」连接外部工具与服务，配置可随时保存。" : "点击「配置 MCP」连接外部工具与服务，配置可随时保存。"} />
      ) : filtered.length === 0 ? (
        <CapabilityEmptyState icon={<McpIcon size="lg" />}
          title={`没有找到与「${search}」匹配的连接器`} onClear={() => setSearch("")} />
      ) : (
        <ResourceList collectionKey="connectors" layout="compact" summary={`${filtered.length} 个连接器 · ${filtered.filter(server => server.enabled).length} 个已启用`} className="mcp-list" aria-label="已配置连接器">
          {filtered.map((server) => {
            const state = statusOf(server);
            const isExpanded = expanded === server.name;
            return (
              <ResourceRow key={server.name} className={`mcp-item-wrap resource-row--expandable${isExpanded ? " mcp-item-wrap--expanded" : ""}`}>
                <div className="mcp-item">
                  <button type="button" className="mcp-item-main" aria-expanded={isExpanded}
                    onClick={() => setExpanded(isExpanded ? null : server.name)}>
                    <ResourceIcon><McpIcon size="md" /></ResourceIcon>
                    <div className="mcp-item-info">
                      <div className="mcp-item-name">
                        {server.displayName || server.name}
                        {server.displayName && <code>{server.name}</code>}
                      </div>
                      <div className="mcp-item-meta">
                        <span>{transportLabel(server.transport)}</span>
                        <span>·</span>
                        <span className={`mcp-state mcp-state--${state.tone}`}><span aria-hidden="true" className={`mcp-dot mcp-dot--${state.tone}`} />{state.label}</span>
                        {server.live && server.status === "ready" && <><span>·</span><span>{server.tools.length} 个工具</span></>}
                      </div>
                      <div className="mcp-item-target resource-clamp">{server.enabled ? server.live ? "点击查看工具与连接详情" : "已启用，启动任务后连接" : "启用后可供任务使用"}</div>
                    </div>
                  </button>
                  <div className="resource-row__actions">
                  {server.authRequired && (
                    <button type="button" className="um-btn um-btn--primary"
                      aria-label={`授权 MCP 服务 ${server.displayName || server.name}`}
                      disabled={mutating !== null || !server.enabled || server.setupRequired}
                      onClick={() => void handleAuthorize(server)}>
                      {mutating === `auth:${server.name}` ? "等待浏览器授权…" : "授权连接"}
                    </button>
                  )}
                  <label className="sk-toggle" title={server.enabled ? "已启用" : "已停用"}>
                    <input type="checkbox" checked={server.enabled}
                      aria-label={`${server.enabled ? "停用" : "启用"} MCP 服务 ${server.displayName || server.name}`}
                      disabled={mutating !== null || server.setupRequired}
                      onChange={() => void handleToggle(server, !server.enabled)} />
                    <span className="sk-toggle-track"><span className="sk-toggle-thumb" /></span>
                  </label>
                  <span className="resource-switch-status">{server.enabled ? "已启用" : "已停用"}</span>
                  <ResourceMenu label={`连接器 ${server.displayName || server.name}`} disabled={mutating !== null}
                    actions={server.editable ? [{ label: `删除 MCP 服务 ${server.displayName || server.name}`, text: "删除用户配置", danger: true, onSelect: () => { void handleDelete(server); } }] : []} />
                  </div>
                </div>
                {authError?.name === server.name && <div className="ec-error" role="alert">授权失败：{authError.message}。可点击「授权连接」重试。</div>}
                {isExpanded && (
                  <div className="mcp-item-details">
                    <div className="mcp-detail-grid">
                      {server.target && <><span>地址 / 命令</span><strong className="mcp-detail-target">{server.target}</strong></>}
                      <span>来源</span><strong>{server.source || "本地"}{server.vendor ? ` · ${server.vendor}` : ""}</strong>
                      <span>运行方式</span><strong>{server.live ? "当前会话已加载" : "已持久化，等待会话加载"}</strong>
                    </div>
                    {diagnostics[server.name]?.detail && (
                      <div className="mcp-diagnostic-detail">
                        <strong>{diagnostics[server.name].reason || "runtime"}</strong>
                        <span>{diagnostics[server.name].detail}</span>
                      </div>
                    )}
                    {server.setup && server.setup.fields.length > 0 && (
                      <div className="mcp-setup-form">
                        <strong>{server.setupRequired ? "完成服务配置" : "服务配置"}</strong>
                        {server.setup.fields.map((field) => (
                          <label key={field.id}>
                            <span>{field.label}{field.required ? " *" : ""}</span>
                            <select
                              value={setupDrafts[server.name]?.[field.id]
                                ?? server.setupValues[field.id]
                                ?? field.default
                                ?? ""}
                              onChange={(event) => setSetupDrafts((current) => ({
                                ...current,
                                [server.name]: {
                                  ...current[server.name],
                                  [field.id]: event.target.value,
                                },
                              }))}
                            >
                              <option value="">请选择</option>
                              {field.options.map((option) => (
                                <option key={option.value} value={option.value}>{option.label}</option>
                              ))}
                            </select>
                          </label>
                        ))}
                        <button type="button" className="um-btn um-btn--primary"
                          disabled={mutating !== null}
                          onClick={() => void handleSetup(server)}>
                          {mutating === `setup:${server.name}` ? "应用中…" : server.setupRequired ? "应用并启动" : "更新配置"}
                        </button>
                      </div>
                    )}
                    {server.tools.length > 0 ? (
                      <div className="mcp-tools">
                        {server.tools.map((tool) => (
                          <div key={tool.name} className={`mcp-tool${tool.enabled ? "" : " mcp-tool--disabled"}`}>
                            <div>
                              <strong>{tool.displayName || tool.name}</strong>
                              {tool.description && <span>{tool.description}</span>}
                            </div>
                            <label className="sk-toggle mcp-tool-toggle" title="仅影响当前会话">
                              <input type="checkbox" checked={tool.enabled}
                                aria-label={`${tool.enabled ? "停用" : "启用"}工具 ${tool.displayName || tool.name}`}
                                disabled={mutating !== null}
                                onChange={() => void handleToolToggle(server, tool.name, !tool.enabled)} />
                              <span className="sk-toggle-track"><span className="sk-toggle-thumb" /></span>
                            </label>
                          </div>
                        ))}
                      </div>
                    ) : (
                      <div className="mcp-no-tools">
                        {server.live ? "当前未获取到工具，可点击「刷新诊断」重试" : "创建会话后将完成握手并列出工具"}
                      </div>
                    )}
                  </div>
                )}
              </ResourceRow>
            );
          })}
        </ResourceList>
      )}
    </>
  );
  const summary = `${servers.length} 个连接器 · ${servers.filter(server => server.enabled).length} 个已启用`;
  const editor = <McpConfigEditor embedded={embedded} onBack={() => setEditing(false)}
    onSaved={() => void reload(true)} onToast={onToast} />;

  if (embedded) {
    if (editing) return editor;
    return <CapabilityPage title="已配置连接器" className="capability-page--connectors"
      search={{ value: search, onChange: setSearch, label: "搜索连接器", placeholder: "搜索名称、命令或 URL…" }}
      actions={<>
        <button type="button" className="capability-button" aria-label="刷新诊断" title="重新检查连接状态"
          disabled={loading || refreshing || mutating !== null} onClick={() => void reload(true)}>
          <RefreshCwIcon size="sm" /><span>{refreshing ? "诊断中…" : "刷新"}</span>
        </button>
        <button type="button" className="capability-button capability-button--primary" disabled={mutating !== null} onClick={() => setEditing(true)}>
          <ConfigureIcon size="sm" /><span>配置连接器</span>
        </button>
      </>}
      summary={<>
        {servers.length === 0 && (loading || error) && <span>{loading ? "正在读取连接器…" : "连接器列表未加载"}</span>}
        <span>配置会保存到本机，有活动任务时立即应用。</span>
      </>}>
      {listContent}
      {dialog}
    </CapabilityPage>;
  }

  return (
    <div className="modal-overlay mcp-overlay" onClick={onClose}>
      <div ref={dialogRef} className="mcp-modal" onClick={(event) => event.stopPropagation()}
        role="dialog" aria-modal="true" aria-label="MCP 服务管理" tabIndex={-1}>
        <div className="mcp-modal-head">
          <div className="mcp-modal-titlewrap">
            <span className="mcp-modal-glyph"><McpIcon size="md" /></span>
            <div>
              <div className="mcp-modal-title">MCP 服务管理</div>
              <div className="mcp-modal-sub">配置可随时保存；有活动会话时立即应用，并显示连接状态。</div>
            </div>
          </div>
          <div className="mcp-modal-headright">
            {!editing && <>
              <button type="button" className="um-btn um-btn--grey" disabled={refreshing} onClick={() => void reload(true)}>
                <RefreshCwIcon size="sm" /><span>{refreshing ? "诊断中…" : "刷新诊断"}</span>
              </button>
              <button type="button" className="um-btn um-btn--grey" onClick={() => setEditing(true)}>
                <ConfigureIcon size="sm" /><span>配置 MCP</span>
              </button>
            </>}
            <button type="button" className="mcp-modal-close" onClick={onClose} aria-label="关闭" data-modal-initial-focus>
              <XCloseIcon size="md" />
            </button>
          </div>
        </div>
        <div className="mcp-modal-body">
          {editing ? editor : <div className="mcp-panel">
            <div className="mcp-panel-searchrow">
              <div className="um-search um-search--flex">
                <SearchIcon size="sm" className="um-search-icon" />
                <input className="um-search-input" value={search} placeholder="搜索名称、命令或 URL…"
                  aria-label="搜索 MCP 服务" onChange={(event) => setSearch(event.target.value)} />
              </div>
              <span className="mcp-summary">{summary}</span>
            </div>
            {listContent}
          </div>}
        </div>
        {dialog}
      </div>
    </div>
  );
}

function statusOf(server: McpServerEntry): { label: string; tone: string } {
  if (!server.enabled) return { label: "已停用", tone: "off" };
  if (server.authRequired) return { label: "待授权", tone: "warn" };
  if (server.setupRequired) return { label: "待配置", tone: "warn" };
  if (!server.live) return { label: "待会话启动", tone: "idle" };
  switch (server.status) {
    case "ready": return { label: "可用", tone: "ready" };
    case "initializing": return { label: "初始化中", tone: "loading" };
    case "unavailable": return { label: "不可用", tone: "error" };
    case "setuprequired": return { label: "待配置", tone: "warn" };
    default: return { label: "已加载", tone: "ready" };
  }
}

function transportLabel(transport?: string): string {
  if (transport === "stdio") return "本地 stdio";
  if (transport === "streamable_http") return "Streamable HTTP";
  if (transport === "managed_gateway") return "托管网关";
  return transport || "未知传输";
}
