import { useCallback, useEffect, useMemo, useRef, useState, type WheelEvent } from "react";
import {
  Building2,
  Check,
  CheckCircle2,
  ChevronDown,
  CircleAlert,
  CloudDownload,
  Eye,
  EyeOff,
  KeyRound,
  Loader2,
  LockKeyhole,
  Pencil,
  Plus,
  RefreshCw,
  Server,
  Star,
  Trash2,
  X,
} from "lucide-react";
import {
  agentsDefaultsGet,
  agentsDefaultsSave,
  agentAuthStatus,
  internalReload,
  providersDeleteModel,
  providersDeleteProvider,
  providersFetchModelsForProvider,
  providersList,
  providersSaveConnection,
  providersTestConnection,
  providersTestModelConnection,
  type ApiBackend,
  type AuthScheme,
  type FetchedModel,
  type ModelEntry,
  type ModelProviderEntry,
  type ProviderKind,
  type ProviderListModel,
} from "@/lib/agent-client";
import { useModalFocus } from "@/lib/use-modal-focus";
import { listenOrgModelsChanged, orgSyncModelConfig } from "@/lib/org-client";
import type { AgentDefaults } from "@/lib/types";
import { useOrgSessionStore } from "@/stores/org-session-store";
import { useAppDialog } from "./AppDialog";

interface ModelConnectionsPanelProps {
  onModelsChanged?: () => void | Promise<void>;
}

interface ProviderPreset {
  label: string;
  shortLabel: string;
  baseUrl: string;
  apiBackend: ApiBackend;
  authScheme: AuthScheme;
  placeholderKey: string;
}

const PROVIDER_KINDS: ProviderKind[] = [
  "openai",
  "anthropic",
  "minimax",
  "deepseek",
  "qwen",
  "custom",
  "custom_anthropic",
];

const PROVIDER_PRESETS: Record<ProviderKind, ProviderPreset> = {
  openai: {
    label: "OpenAI API（GPT 模型）",
    shortLabel: "OpenAI",
    baseUrl: "https://api.openai.com/v1",
    apiBackend: "responses",
    authScheme: "bearer",
    placeholderKey: "sk-...",
  },
  anthropic: {
    label: "Anthropic 官方接口",
    shortLabel: "Anthropic",
    baseUrl: "https://api.anthropic.com/v1",
    apiBackend: "messages",
    authScheme: "x_api_key",
    placeholderKey: "sk-ant-...",
  },
  minimax: {
    label: "MiniMax 官方接口",
    shortLabel: "MiniMax",
    baseUrl: "https://api.minimax.cn/v1",
    apiBackend: "chat_completions",
    authScheme: "bearer",
    placeholderKey: "MiniMax API Key",
  },
  deepseek: {
    label: "DeepSeek 官方接口",
    shortLabel: "DeepSeek",
    baseUrl: "https://api.deepseek.com",
    apiBackend: "chat_completions",
    authScheme: "bearer",
    placeholderKey: "sk-...",
  },
  qwen: {
    label: "通义千问官方接口",
    shortLabel: "通义千问",
    baseUrl: "https://dashscope.aliyuncs.com/compatible-mode/v1",
    apiBackend: "chat_completions",
    authScheme: "bearer",
    placeholderKey: "sk-...",
  },
  custom: {
    label: "OpenAI 兼容接口",
    shortLabel: "OpenAI 兼容",
    baseUrl: "",
    apiBackend: "chat_completions",
    authScheme: "bearer",
    placeholderKey: "your-api-key",
  },
  custom_anthropic: {
    label: "Anthropic 兼容接口",
    shortLabel: "Anthropic 兼容",
    baseUrl: "",
    apiBackend: "messages",
    authScheme: "x_api_key",
    placeholderKey: "sk-ant-...",
  },
};

interface ConnectionDraft {
  label: string;
  providerKind: ProviderKind;
  baseUrl: string;
  allowInsecureHttp: boolean;
  apiKey: string;
  apiBackend: ApiBackend;
  authScheme: AuthScheme;
  contextWindow: string;
}

type PanelMessage = { kind: "ok" | "err" | "warn"; text: string };

function connectionName(provider: ModelProviderEntry): string {
  if (provider.source === "organization") return provider.label || "组织提供";
  return provider.label || PROVIDER_PRESETS[provider.providerKind]?.shortLabel || provider.id;
}

function isOjlabBaseUrl(value: string): boolean {
  return value.trim().replace(/\/+$/, "") === "http://123.56.188.16:8088/v1";
}

function connectionBaseUrlError(value: string, allowInsecureHttp: boolean): string | null {
  if (!value.trim()) return "请填写 Base URL。";
  let url: URL;
  try {
    url = new URL(value.trim());
  } catch {
    return "Base URL 格式不正确。";
  }
  if (url.protocol !== "http:" && url.protocol !== "https:") return "Base URL 必须使用 HTTP 或 HTTPS。";
  const misplacedPort = /^\/:([0-9]+)(?:\/|$)/.exec(url.pathname);
  if (misplacedPort) return `端口号位置不对：请把 /:${misplacedPort[1]}/ 改成 :${misplacedPort[1]}/。`;
  if (url.protocol === "http:" && !allowInsecureHttp) {
    return "使用 HTTP 前，请确认下方的明文传输选项；也可以改用 HTTPS。";
  }
  return null;
}

function formatSyncTime(value?: number): string {
  if (!value) return "同步时间未知";
  return new Intl.DateTimeFormat("zh-CN", {
    month: "numeric",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  }).format(new Date(value));
}

function protocolLabel(provider: ModelProviderEntry): string {
  if (provider.apiBackend === "messages") return "Anthropic Messages";
  if (provider.apiBackend === "responses") return "OpenAI Responses";
  return "OpenAI Chat Completions";
}

function modelRemoteId(model: ModelEntry): string {
  return model.remoteModelId || model.modelId;
}

function modelLimitsText(model: ModelEntry, provider: ModelProviderEntry): string | null {
  const contextWindow = model.contextWindow ?? provider.contextWindow;
  const maxOutput = model.maxOutputTokens;
  const limits = [
    contextWindow ? `上下文 ${contextWindow.toLocaleString()}` : null,
    maxOutput ? `最大输出 ${maxOutput.toLocaleString()}` : null,
  ].filter(Boolean);
  return limits.length ? `${limits.join(" · ")} tokens` : null;
}

async function reloadRuntime(): Promise<string | null> {
  try {
    await internalReload("models");
    return null;
  } catch (error) {
    return String(error).replace(/^Error:\s*/, "");
  }
}

export function ModelConnectionsPanel({ onModelsChanged }: ModelConnectionsPanelProps) {
  const [catalog, setCatalog] = useState<ProviderListModel>({ providers: [], models: [] });
  const [defaults, setDefaults] = useState<AgentDefaults | null>(null);
  const [effectiveDefaultModelId, setEffectiveDefaultModelId] = useState<string | null>(null);
  const [selectedProviderId, setSelectedProviderId] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [message, setMessage] = useState<PanelMessage | null>(null);
  const [connectionEditor, setConnectionEditor] = useState<ModelProviderEntry | "new" | null>(null);
  const [modelEditor, setModelEditor] = useState<{ provider: ModelProviderEntry; model?: ModelEntry } | null>(null);
  const [importProvider, setImportProvider] = useState<ModelProviderEntry | null>(null);
  const [syncingOrganization, setSyncingOrganization] = useState(false);
  const [testingProviderId, setTestingProviderId] = useState<string | null>(null);
  const reloadGenerationRef = useRef(0);
  const sourceListRef = useRef<HTMLElement>(null);
  const detailRef = useRef<HTMLElement>(null);
  const organizationSession = useOrgSessionStore((state) => state.session);
  const hydrateOrganization = useOrgSessionStore((state) => state.hydrate);
  const { requestConfirmation, dialog } = useAppDialog(selectedProviderId);

  const handleWheel = (event: WheelEvent<HTMLDivElement>) => {
    const target = event.target;
    if (event.deltaY === 0 || !(target instanceof Element) || target.closest(
      ".model-connections__detail, .models-settings-panel__editor-overlay, .app-dialog-overlay",
    )) return;

    const sourceList = sourceListRef.current;
    const overSourceList = Boolean(sourceList?.contains(target));
    const canScroll = (element: HTMLElement | null) => element && (event.deltaY > 0
      ? element.scrollTop + element.clientHeight < element.scrollHeight - 1
      : element.scrollTop > 0);
    if (overSourceList && canScroll(sourceList)) return;
    if (getComputedStyle(event.currentTarget).overflowY !== "hidden") return;

    const scrollTarget = [detailRef.current, sourceList].find(canScroll);
    if (!scrollTarget) return;
    const unit = event.deltaMode === 1 ? 16 : event.deltaMode === 2 ? scrollTarget.clientHeight : 1;
    scrollTarget.scrollTop += event.deltaY * unit;
  };

  const reload = useCallback(async (): Promise<string | null> => {
    const generation = ++reloadGenerationRef.current;
    try {
      const auth = await agentAuthStatus();
      const [nextCatalog, nextDefaults] = await Promise.all([
        providersList(),
        agentsDefaultsGet(),
      ]);
      if (generation !== reloadGenerationRef.current) return null;
      setCatalog(nextCatalog);
      setDefaults(nextDefaults);
      setEffectiveDefaultModelId(auth.defaultModelId ?? null);
      setSelectedProviderId((current) => {
        if (current && nextCatalog.providers.some((provider) => provider.id === current)) {
          return current;
        }
        return nextCatalog.providers.find((provider) => provider.source === "organization")?.id
          ?? nextCatalog.providers[0]?.id
          ?? null;
      });
      return null;
    } catch (error) {
      if (generation !== reloadGenerationRef.current) return null;
      const detail = String(error).replace(/^Error:\s*/, "");
      setMessage({ kind: "err", text: `读取模型配置失败：${detail}` });
      return detail;
    } finally {
      if (generation === reloadGenerationRef.current) setLoading(false);
    }
  }, []);

  useEffect(() => {
    void reload();
    void hydrateOrganization();
    let unlisten: (() => void) | undefined;
    void listenOrgModelsChanged(() => void reload()).then((dispose) => { unlisten = dispose; });
    return () => unlisten?.();
  }, [hydrateOrganization, reload]);

  const organizationProviders = useMemo(
    () => catalog.providers.filter((provider) => provider.source === "organization"),
    [catalog.providers],
  );
  const personalProviders = useMemo(
    () => catalog.providers.filter((provider) => provider.source !== "organization" && provider.source !== "builtin"),
    [catalog.providers],
  );
  const builtinProviders = useMemo(
    () => catalog.providers.filter((provider) => provider.source === "builtin"),
    [catalog.providers],
  );
  const selectedProvider = catalog.providers.find((provider) => provider.id === selectedProviderId) ?? null;
  const selectedModels = selectedProvider
    ? catalog.models.filter((model) => model.providerId === selectedProvider.id)
    : [];
  const savedDefaultModelId = defaults?.defaultModel?.trim() || null;
  const savedDefaultMissing = savedDefaultModelId !== null
    && !catalog.models.some((model) => model.modelId === savedDefaultModelId);

  const finishMutation = async (success: string) => {
    const reloadError = await reloadRuntime();
    const catalogError = await reload();
    // Always refresh the shell's authoritative status. On Runtime failure the
    // backend reports ready=false, so the Composer stays locked even though
    // the newly-written model is already visible on disk.
    let shellRefreshError: string | null = null;
    try {
      await onModelsChanged?.();
    } catch (error) {
      shellRefreshError = String(error).replace(/^Error:\s*/, "");
    }
    const refreshErrors = [
      reloadError ? `Agent Runtime 刷新失败：${reloadError}` : null,
      catalogError ? `模型列表重读失败：${catalogError}` : null,
      shellRefreshError ? `全局模型状态刷新失败：${shellRefreshError}` : null,
    ].filter(Boolean);
    setMessage(refreshErrors.length > 0
      ? {
        kind: "err",
        text: `${success} 但后续刷新未完成：${refreshErrors.join("；")}。配置已保存，请刷新或重启应用，不要重复执行删除。`,
      }
      : { kind: "ok", text: success });
  };

  const handleDeleteProvider = (provider: ModelProviderEntry) => {
    if (provider.managed) return;
    const count = catalog.models.filter((model) => model.providerId === provider.id).length;
    requestConfirmation({
      title: `删除连接“${connectionName(provider)}”？`,
      description: count
        ? `该连接下的 ${count} 个模型也会同时删除。此操作无法撤销。`
        : "该连接将从本机删除，此操作无法撤销。",
      confirmLabel: "删除连接",
      danger: true,
      action: async () => {
        await providersDeleteProvider(provider.id);
        await finishMutation("连接及其模型已删除。");
      },
      onError: (error) => setMessage({ kind: "err", text: `删除失败：${String(error)}` }),
    });
  };

  const handleDeleteModel = (model: ModelEntry) => {
    if (model.managed) return;
    const label = model.name || modelRemoteId(model);
    requestConfirmation({
      title: `删除模型“${label}”？`,
      description: "该模型将从当前连接中移除，且不再可用于新对话。",
      confirmLabel: "删除模型",
      danger: true,
      action: async () => {
        await providersDeleteModel(model.modelId);
        await finishMutation("模型已删除。");
      },
      onError: (error) => setMessage({ kind: "err", text: `删除失败：${String(error)}` }),
    });
  };

  const handleSetDefault = async (model: ModelEntry) => {
    try {
      const current = defaults ?? await agentsDefaultsGet();
      await agentsDefaultsSave({ ...current, defaultModel: model.modelId });
      await finishMutation(`已将「${model.name || modelRemoteId(model)}」设为默认模型。`);
    } catch (error) {
      setMessage({ kind: "err", text: `设置默认模型失败：${String(error)}` });
    }
  };

  const handleOrganizationSync = async () => {
    setSyncingOrganization(true);
    setMessage(null);
    try {
      const result = await orgSyncModelConfig();
      const success = result.configured
        ? "组织模型配置已更新。"
        : "组织服务器当前未下发模型配置。";
      await finishMutation(success);
    } catch (error) {
      setMessage({ kind: "err", text: `组织模型同步失败：${String(error)}` });
    } finally {
      setSyncingOrganization(false);
    }
  };

  const handleTestSaved = async (provider: ModelProviderEntry) => {
    const model = catalog.models.find((entry) => entry.providerId === provider.id);
    if (!model) {
      setMessage({ kind: "warn", text: "请先为该连接添加一个 Model ID，再测试实际模型调用。" });
      return;
    }
    setTestingProviderId(provider.id);
    setMessage(null);
    try {
      await providersTestModelConnection(
        { ...provider, apiKey: undefined },
        modelRemoteId(model),
      );
      setMessage({ kind: "ok", text: `模型「${modelRemoteId(model)}」调用成功。` });
    } catch (error) {
      setMessage({ kind: "err", text: `模型调用失败：${String(error)}` });
    } finally {
      setTestingProviderId(null);
    }
  };

  return (
    <div className="model-connections" onWheel={handleWheel}>
      <header className="model-connections__header">
        <div>
          <h2>模型与连接</h2>
          <p>内置对话模型开箱即用。默认顺序：手动指定、组织下发、个人连接、内置 chat-xc。</p>
        </div>
        <button className="echo-button echo-button--primary echo-button--medium" onClick={() => setConnectionEditor("new")}>
          <span className="echo-button__content"><Plus size={15} />添加个人连接</span>
        </button>
      </header>

      {(organizationProviders.length > 0 || organizationSession?.loggedIn) && (
        <section className="model-connections__org-status" aria-label="组织模型同步状态">
          <div className="model-connections__org-status-main">
            {organizationProviders.length > 0 ? <CheckCircle2 size={17} /> : <CircleAlert size={17} />}
            <div>
              <strong>{organizationProviders.length > 0 ? "组织模型已同步" : "组织暂未下发模型"}</strong>
              <span>
                {organizationProviders[0]
                  ? `${formatSyncTime(organizationProviders[0].syncedAt)} · 配置由组织管理员维护`
                  : "登录状态正常，可稍后重试同步"}
              </span>
            </div>
          </div>
          <button className="echo-button echo-button--ghost echo-button--small" onClick={() => void handleOrganizationSync()} disabled={syncingOrganization}>
            <span className="echo-button__content">
              {syncingOrganization ? <Loader2 className="models-settings-panel__spin" size={13} /> : <RefreshCw size={13} />}
              {syncingOrganization ? "同步中…" : "立即同步"}
            </span>
          </button>
        </section>
      )}

      {message && <div className={`model-connections__message model-connections__message--${message.kind}`}>{message.text}</div>}

      {loading ? (
        <div className="model-connections__empty"><Loader2 className="models-settings-panel__spin" size={18} />正在读取模型配置…</div>
      ) : catalog.providers.length === 0 ? (
        <div className="model-connections__empty">
          <Server size={24} />
          <strong>还没有可用模型</strong>
          <span>添加个人连接，填写 Base URL、API Key 和 Model ID 后即可使用。</span>
          <button className="echo-button echo-button--primary echo-button--medium" onClick={() => setConnectionEditor("new")}>添加个人连接</button>
        </div>
      ) : (
        <div className="model-connections__workspace">
          <aside className="model-connections__source-list" aria-label="模型连接" ref={sourceListRef}>
            {organizationProviders.length > 0 && (
              <ConnectionGroup
                title="组织提供"
                providers={organizationProviders}
                catalog={catalog}
                selectedProviderId={selectedProviderId}
                onSelect={setSelectedProviderId}
              />
            )}
            {personalProviders.length > 0 && (
              <ConnectionGroup
                title="我的连接"
                providers={personalProviders}
                catalog={catalog}
                selectedProviderId={selectedProviderId}
                onSelect={setSelectedProviderId}
              />
            )}
            {builtinProviders.length > 0 && (
              <ConnectionGroup
                title="内置模型"
                providers={builtinProviders}
                catalog={catalog}
                selectedProviderId={selectedProviderId}
                onSelect={setSelectedProviderId}
              />
            )}
          </aside>

          {selectedProvider && (
            <section
              key={selectedProvider.id}
              className="model-connections__detail"
              aria-label={`${connectionName(selectedProvider)}连接详情`}
              tabIndex={0}
              ref={detailRef}
            >
              <header className="model-connections__detail-header">
                <div>
                  <div className="model-connections__title-row">
                    <h3>{connectionName(selectedProvider)}</h3>
                    {selectedProvider.managed ? (
                      <span className="model-connections__badge model-connections__badge--managed"><LockKeyhole size={11} />{selectedProvider.source === "builtin" ? "内置只读" : "组织托管"}</span>
                    ) : (
                      <span className="model-connections__badge">个人配置</span>
                    )}
                    {selectedProvider.credentialConfigured && <span className="model-connections__badge model-connections__badge--ready">已配置</span>}
                  </div>
                  <p>{PROVIDER_PRESETS[selectedProvider.providerKind]?.label || selectedProvider.providerKind}</p>
                </div>
                <div className="model-connections__detail-actions">
                  <button className="echo-button echo-button--secondary echo-button--small" onClick={() => void handleTestSaved(selectedProvider)} disabled={testingProviderId === selectedProvider.id}>
                    <span className="echo-button__content">
                      {testingProviderId === selectedProvider.id ? <Loader2 className="models-settings-panel__spin" size={13} /> : <Check size={13} />}
                      测试模型
                    </span>
                  </button>
                  {!selectedProvider.managed && (
                    <>
                      <button className="echo-button echo-button--ghost echo-button--small echo-button--icon-only" onClick={() => setConnectionEditor(selectedProvider)} aria-label="编辑连接" title="编辑连接"><Pencil size={14} /></button>
                      <button className="echo-button echo-button--ghost echo-button--small echo-button--icon-only" onClick={() => void handleDeleteProvider(selectedProvider)} aria-label="删除连接" title="删除连接"><Trash2 size={14} /></button>
                    </>
                  )}
                </div>
              </header>

              <dl className="model-connections__facts">
                <div><dt>接口协议</dt><dd>{protocolLabel(selectedProvider)}</dd></div>
                <div><dt>API Key</dt><dd>{selectedProvider.source === "builtin" || (isOjlabBaseUrl(selectedProvider.baseUrl ?? "") && !selectedProvider.apiKey) ? "此服务无需 API Key" : selectedProvider.managed ? "由组织安全配置 · 不可查看" : selectedProvider.credentialConfigured ? "已保存在本机" : "未配置"}</dd></div>
                <div><dt>Base URL</dt><dd title={selectedProvider.baseUrl}>{selectedProvider.baseUrl || "—"}{selectedProvider.managed && <span> · 只读</span>}</dd></div>
                <div><dt>上下文窗口</dt><dd>{selectedProvider.contextWindow ? `${selectedProvider.contextWindow.toLocaleString()} tokens` : selectedModels.some((model) => model.contextWindow) ? "按模型配置" : "使用模型默认值"}</dd></div>
              </dl>
              {selectedProvider.source !== "organization" && selectedProvider.source !== "builtin" && selectedProvider.baseUrl?.startsWith("http://") && (
                <div className="model-connections__message model-connections__message--warn">
                  此连接使用 HTTP 明文传输；API Key、提问和模型回复可能被网络路径上的其他人看到。
                </div>
              )}
              {savedDefaultMissing && (
                <div className="model-connections__message model-connections__message--warn">
                  已设默认模型「{savedDefaultModelId}」不在当前模型目录中；请重新选择。{effectiveDefaultModelId ? `当前自动使用「${effectiveDefaultModelId}」。` : ""}
                </div>
              )}

              <div className="model-connections__models-header">
                <div><strong>可用模型</strong><span>{selectedModels.length} 个</span></div>
                {!selectedProvider.managed && (
                  <div>
                    <button className="echo-button echo-button--ghost echo-button--small" onClick={() => setModelEditor({ provider: selectedProvider })}><span className="echo-button__content"><Plus size={13} />手动添加</span></button>
                    <button className="echo-button echo-button--secondary echo-button--small" onClick={() => setImportProvider(selectedProvider)}><span className="echo-button__content"><CloudDownload size={13} />同步模型</span></button>
                  </div>
                )}
              </div>

              {selectedModels.length === 0 ? (
                <div className="model-connections__models-empty">该连接还没有模型，请同步模型列表或手动添加远端模型 ID。</div>
              ) : (
                <ul className="model-connections__model-list">
                  {selectedModels.map((model) => {
                    const isSavedDefault = savedDefaultModelId === model.modelId;
                    const isEffective = effectiveDefaultModelId === model.modelId;
                    const limits = modelLimitsText(model, selectedProvider);
                    return (
                      <li key={model.modelId}>
                        <div className="model-connections__model-copy">
                          <strong>{model.name || modelRemoteId(model)}</strong>
                          <span>远端模型 ID：{modelRemoteId(model)}</span>
                          {limits && <span className="model-connections__model-limits">{limits}</span>}
                        </div>
                        <div className="model-connections__model-actions">
                          {isSavedDefault && <span className="model-connections__default"><Star size={12} fill="currentColor" />{isEffective ? "已设默认 · 当前使用" : effectiveDefaultModelId ? "已设默认（当前未使用）" : "已设默认"}</span>}
                          {!isSavedDefault && isEffective && <span className="model-connections__default">当前自动使用</span>}
                          {!isSavedDefault && (
                            <button className="echo-button echo-button--ghost echo-button--small" onClick={() => void handleSetDefault(model)}>设为默认</button>
                          )}
                          {!selectedProvider.managed && (
                            <>
                              <button className="echo-button echo-button--ghost echo-button--small echo-button--icon-only" onClick={() => setModelEditor({ provider: selectedProvider, model })} aria-label={`编辑模型 ${modelRemoteId(model)}`}><Pencil size={13} /></button>
                              <button className="echo-button echo-button--ghost echo-button--small echo-button--icon-only" onClick={() => void handleDeleteModel(model)} aria-label={`删除模型 ${modelRemoteId(model)}`}><Trash2 size={13} /></button>
                            </>
                          )}
                        </div>
                      </li>
                    );
                  })}
                </ul>
              )}
            </section>
          )}
        </div>
      )}

      {connectionEditor && (
        <ConnectionEditor
          original={connectionEditor === "new" ? undefined : connectionEditor}
          existingModels={connectionEditor === "new" ? [] : catalog.models.filter((model) => model.providerId === connectionEditor.id)}
          onCancel={() => setConnectionEditor(null)}
          onSaved={async (providerId, count) => {
            setConnectionEditor(null);
            setSelectedProviderId(providerId);
            await finishMutation(`${connectionEditor === "new" ? "连接已添加" : "连接已更新"}${count ? `，已配置 ${count} 个模型` : ""}。`);
          }}
        />
      )}

      {modelEditor && (
        <ManualModelEditor
          provider={modelEditor.provider}
          original={modelEditor.model}
          onCancel={() => setModelEditor(null)}
          onSaved={async () => {
            setModelEditor(null);
            await finishMutation(modelEditor.model ? "模型已更新。" : "模型已添加。");
          }}
        />
      )}

      {importProvider && (
        <ModelImportDialog
          provider={importProvider}
          existingModels={catalog.models.filter((model) => model.providerId === importProvider.id)}
          onCancel={() => setImportProvider(null)}
          onSaved={async (count) => {
            setImportProvider(null);
            await finishMutation(`已添加 ${count} 个模型。`);
          }}
        />
      )}
      {dialog}
    </div>
  );
}

function ConnectionGroup({
  title,
  providers,
  catalog,
  selectedProviderId,
  onSelect,
}: {
  title: string;
  providers: ModelProviderEntry[];
  catalog: ProviderListModel;
  selectedProviderId: string | null;
  onSelect: (id: string) => void;
}) {
  return (
    <div className="model-connections__source-group">
      <div className="model-connections__source-heading"><span>{title}</span><span>{providers.length}</span></div>
      {providers.map((provider) => {
        const count = catalog.models.filter((model) => model.providerId === provider.id).length;
        return (
          <button
            key={provider.id}
            type="button"
            className={`model-connections__source${provider.id === selectedProviderId ? " model-connections__source--active" : ""}`}
            onClick={() => onSelect(provider.id)}
            aria-current={provider.id === selectedProviderId ? "true" : undefined}
          >
            <span className="model-connections__source-icon">{provider.source === "organization" ? <Building2 size={15} /> : <Server size={15} />}</span>
            <span><strong>{connectionName(provider)}</strong><small>{count} 个模型 · {provider.managed ? "只读" : "个人"}</small></span>
          </button>
        );
      })}
    </div>
  );
}

function draftProvider(original: ModelProviderEntry | undefined, draft: ConnectionDraft): ModelProviderEntry {
  return {
    id: original?.id ?? "",
    label: draft.label.trim(),
    providerKind: draft.providerKind,
    baseUrl: draft.baseUrl.trim(),
    allowInsecureHttp: draft.allowInsecureHttp,
    apiKey: draft.apiKey.trim() || undefined,
    apiBackend: draft.apiBackend,
    authScheme: draft.authScheme,
    contextWindow: draft.contextWindow ? Number(draft.contextWindow) : undefined,
    source: original?.source,
  };
}

function ConnectionEditor({
  original,
  existingModels,
  onCancel,
  onSaved,
}: {
  original?: ModelProviderEntry;
  existingModels: ModelEntry[];
  onCancel: () => void;
  onSaved: (providerId: string, modelCount: number) => Promise<void>;
}) {
  const initialKind = original?.providerKind ?? "custom";
  const initialPreset = PROVIDER_PRESETS[initialKind];
  const [draft, setDraft] = useState<ConnectionDraft>({
    label: original?.label ?? (original ? connectionName(original) : ""),
    providerKind: initialKind,
    baseUrl: original?.baseUrl ?? initialPreset.baseUrl,
    allowInsecureHttp: original?.allowInsecureHttp ?? false,
    apiKey: "",
    apiBackend: original?.apiBackend ?? initialPreset.apiBackend,
    authScheme: original?.authScheme ?? initialPreset.authScheme,
    contextWindow: original?.contextWindow ? String(original.contextWindow) : "",
  });
  const [showKey, setShowKey] = useState(false);
  const [showAdvanced, setShowAdvanced] = useState(false);
  const [testing, setTesting] = useState(false);
  const [discovering, setDiscovering] = useState(false);
  const [saving, setSaving] = useState(false);
  const [discovered, setDiscovered] = useState<FetchedModel[]>([]);
  const [modelIds, setModelIds] = useState<Set<string>>(
    new Set(existingModels.map(modelRemoteId)),
  );
  const [search, setSearch] = useState("");
  const [modelInput, setModelInput] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [discoveryMessage, setDiscoveryMessage] = useState<PanelMessage | null>(null);
  const preset = PROVIDER_PRESETS[draft.providerKind];
  const dialogRef = useModalFocus<HTMLDivElement>(true, onCancel);

  const handleKindChange = (providerKind: ProviderKind) => {
    const next = PROVIDER_PRESETS[providerKind];
    setDraft((current) => ({
      ...current,
      providerKind,
      label: current.label || next.shortLabel,
      baseUrl: next.baseUrl,
      allowInsecureHttp: false,
      apiBackend: next.apiBackend,
      authScheme: next.authScheme,
    }));
  };

  const validate = (): string | null => {
    const baseUrlError = connectionBaseUrlError(draft.baseUrl, draft.allowInsecureHttp);
    if (baseUrlError) return baseUrlError;
    if (!original && !draft.apiKey.trim() && !isOjlabBaseUrl(draft.baseUrl)) return "请填写 API Key。";
    if (draft.contextWindow && Number(draft.contextWindow) <= 0) return "上下文窗口必须大于 0。";
    return null;
  };

  const parsedInputModels = () => modelInput
    .split(/[\n,，]+/)
    .map((value) => value.trim())
    .filter(Boolean);

  const allModelIds = () => [...new Set([...modelIds, ...parsedInputModels()])];

  const requireModelIds = (): string[] | null => {
    const ids = allModelIds();
    if (ids.length === 0) {
      setError("请至少填写一个 Model ID。");
      return null;
    }
    return ids;
  };

  const addInputModels = () => {
    const values = parsedInputModels();
    if (values.length === 0) return;
    setModelIds((current) => new Set([...current, ...values]));
    setModelInput("");
    setError(null);
  };

  const buildModels = (ids: string[]) => ids.map((remoteModelId) => {
    const existing = existingModels.find((model) => modelRemoteId(model) === remoteModelId);
    return {
      modelId: existing?.modelId ?? "",
      remoteModelId,
      providerId: original?.id ?? "",
      name: existing?.name,
      contextWindow: existing?.contextWindow,
      maxOutputTokens: existing?.maxOutputTokens,
    } satisfies ModelEntry;
  });

  const persist = async (ids: string[]) => providersSaveConnection(
    draftProvider(original, draft),
    buildModels(ids),
    true,
  );

  const handleDiscover = async () => {
    const validation = validate();
    if (validation) { setDiscoveryMessage(null); setError(validation); return; }
    setDiscovering(true);
    setError(null);
    setDiscoveryMessage(null);
    try {
      const models = await providersTestConnection(draftProvider(original, draft));
      const remoteIds = new Set(models.map((model) => model.id));
      const locallyConfigured = existingModels
        .filter((model) => !remoteIds.has(modelRemoteId(model)))
        .map((model) => ({ id: modelRemoteId(model), ownedBy: "本地已配置" }));
      setDiscovered([...models, ...locallyConfigured]);
      setDiscoveryMessage(models.length > 0
        ? { kind: "ok", text: `获取到 ${models.length} 个模型，勾选后会随连接一起保存。` }
        : { kind: "warn", text: "服务端没有返回模型列表，请直接填写 Model ID。" });
    } catch (requestError) {
      setDiscovered(existingModels.map((model) => ({
        id: modelRemoteId(model),
        ownedBy: "本地已配置",
      })));
      setDiscoveryMessage({
        kind: "warn",
        text: `无法获取模型列表：${String(requestError)}。这不影响手动填写、测试和保存。`,
      });
    } finally {
      setDiscovering(false);
    }
  };

  const handleSave = async () => {
    const validation = validate();
    if (validation) { setError(validation); return; }
    const ids = requireModelIds();
    if (!ids) return;
    setSaving(true);
    setError(null);
    try {
      const result = await persist(ids);
      await onSaved(result.providerId, result.modelIds.length);
    } catch (saveError) {
      setError(String(saveError));
    } finally {
      setSaving(false);
    }
  };

  const handleTestAndSave = async () => {
    const validation = validate();
    if (validation) { setError(validation); return; }
    const ids = requireModelIds();
    if (!ids) return;
    const target = parsedInputModels()[0] ?? ids[0];
    setTesting(true);
    setError(null);
    try {
      await providersTestModelConnection(draftProvider(original, draft), target);
    } catch (testError) {
      setError(`测试失败，尚未保存：${String(testError)}。如果服务需要特殊参数，可确认后选择“直接保存”。`);
      setTesting(false);
      return;
    }
    setTesting(false);
    setSaving(true);
    try {
      const result = await persist(ids);
      await onSaved(result.providerId, result.modelIds.length);
    } catch (saveError) {
      setError(`模型测试通过，但保存失败：${String(saveError)}`);
    } finally {
      setSaving(false);
    }
  };

  const visibleModels = discovered.filter((model) => (
    model.id.toLowerCase().includes(search.trim().toLowerCase())
  ));

  return (
    <div ref={dialogRef} className="models-settings-panel__editor-overlay" role="dialog" aria-modal="true" aria-label={original ? "编辑连接" : "添加个人连接"} tabIndex={-1}>
      <div className="model-connection-editor">
        <header className="models-settings-panel__editor-header">
          <div><div className="models-settings-panel__editor-title">{original ? "编辑个人连接" : "添加个人连接"}</div><div className="models-settings-panel__editor-note">{isOjlabBaseUrl(draft.baseUrl)
            ? "填写 Base URL 和 Model ID；此服务无需 API Key。"
            : original?.credentialConfigured
              ? "修改连接信息；API Key 留空则沿用已保存凭据。"
              : original
                ? "填写 Base URL 和 Model ID；如服务需要鉴权，请填写 API Key。"
                : "填写 Base URL、Model ID 和 API Key。"}</div></div>
          <button className="echo-button echo-button--ghost echo-button--small echo-button--icon-only" onClick={onCancel} aria-label="关闭"><X size={14} /></button>
        </header>

        <div className="model-connection-editor__body">
          <section className="model-connection-editor__section">
            <div className="model-connection-editor__section-title"><span>连接信息</span><small>{isOjlabBaseUrl(draft.baseUrl) ? "此服务无需 API Key" : original?.credentialConfigured ? "Base URL 必填 · API Key 可留空复用" : original ? "Base URL 必填 · API Key 按服务要求填写" : "Base URL 与 API Key 必填"}</small></div>
            <div className="models-settings-panel__field">
              <label className="models-settings-panel__label" htmlFor="model-provider-kind">接口类型</label>
              <div className="models-settings-panel__select-shell">
                <select id="model-provider-kind" className="models-settings-panel__select" value={draft.providerKind} onChange={(event) => handleKindChange(event.target.value as ProviderKind)}>
                  {PROVIDER_KINDS.map((kind) => <option key={kind} value={kind}>{PROVIDER_PRESETS[kind].label}</option>)}
                </select>
                <ChevronDown className="models-settings-panel__select-arrow" size={14} />
              </div>
            </div>
            <div className="models-settings-panel__field">
              <label className="models-settings-panel__label" htmlFor="model-connection-label">连接名称 <span className="model-connection-editor__hint">可选，用于区分多个接口</span></label>
              <input id="model-connection-label" className="models-settings-panel__input" value={draft.label} onChange={(event) => setDraft((current) => ({ ...current, label: event.target.value }))} placeholder={`例如：工作用 ${preset.shortLabel}`} data-modal-initial-focus />
            </div>
            <div className="models-settings-panel__field">
              <label className="models-settings-panel__label" htmlFor="model-base-url">Base URL</label>
              <input id="model-base-url" className="models-settings-panel__input" value={draft.baseUrl} onChange={(event) => { setError(null); setDiscoveryMessage(null); setDraft((current) => isOjlabBaseUrl(event.target.value)
                ? { ...current, baseUrl: event.target.value, providerKind: "custom", apiBackend: "chat_completions", authScheme: "bearer", apiKey: "", allowInsecureHttp: false }
                : { ...current, baseUrl: event.target.value, allowInsecureHttp: event.target.value.trim() === current.baseUrl.trim() && current.allowInsecureHttp }); }} placeholder="https://api.example.com/v1" inputMode="url" />
              <span className="model-connection-editor__help">填写 API 根地址，例如 https://example.com:60100/v1；端口写在主机名后。HTTPS 无需额外设置。</span>
              {draft.baseUrl.trim().toLowerCase().startsWith("http://") && (
                <label className="model-connection-editor__help model-connection-editor__http-consent">
                  <input type="checkbox" checked={draft.allowInsecureHttp} onChange={(event) => setDraft((current) => ({ ...current, allowInsecureHttp: event.target.checked }))} />
                  允许此个人连接使用 HTTP。我了解{isOjlabBaseUrl(draft.baseUrl) ? "提问和模型回复" : "API Key、提问和模型回复"}会以明文传输。
                </label>
              )}
            </div>
            <div className="models-settings-panel__field">
              <label className="models-settings-panel__label" htmlFor="model-api-key">API Key</label>
              <div className="models-settings-panel__input-shell">
                <input id="model-api-key" className="models-settings-panel__input models-settings-panel__input--with-trailing-icon" type={showKey ? "text" : "password"} value={draft.apiKey} onChange={(event) => setDraft((current) => ({ ...current, apiKey: event.target.value }))} placeholder={isOjlabBaseUrl(draft.baseUrl) ? "无需填写" : original?.credentialConfigured ? "已保存；留空表示不更换" : preset.placeholderKey} autoComplete="off" disabled={isOjlabBaseUrl(draft.baseUrl)} />
                <button className="echo-button echo-button--ghost echo-button--small echo-button--icon-only models-settings-panel__input-toggle" onClick={() => setShowKey((current) => !current)} type="button" aria-label={showKey ? "隐藏 API Key" : "显示 API Key"} disabled={isOjlabBaseUrl(draft.baseUrl)}>{showKey ? <EyeOff size={14} /> : <Eye size={14} />}</button>
              </div>
              <span className="model-connection-editor__help">
                <KeyRound size={11} />
                {draft.providerKind === "openai"
                  ? "使用 OpenAI Platform API Key；ChatGPT 订阅登录不能作为模型 API 凭据。"
                  : draft.providerKind === "minimax"
                    ? "录音转写页会检测此接口和 API Key 是否具备语音能力；语音额度可能需要单独开通。"
                  : draft.providerKind === "custom"
                    ? "保存在本机私有配置中；MiniMax 模型可在录音转写页检测 /speech_to_text 能力。"
                    : "保存在本机私有配置中；编辑时无需重复输入。"}
              </span>
            </div>
          </section>

          <section className="model-connection-editor__section model-connection-editor__section--model">
            <div className="model-connection-editor__section-title"><span>Model ID</span><small>必填 · 不依赖模型列表</small></div>
            <div className="models-settings-panel__field">
              <label className="models-settings-panel__label" htmlFor="model-id-input">模型名称 / ID</label>
              <div className="model-connection-editor__manual-inline">
                <input id="model-id-input" className="models-settings-panel__input" value={modelInput} onChange={(event) => setModelInput(event.target.value)} onKeyDown={(event) => { if (event.key === "Enter") { event.preventDefault(); addInputModels(); } }} placeholder="例如 gpt-4o、deepseek-chat、MiniMax-M3" autoComplete="off" />
                <button className="echo-button echo-button--secondary echo-button--small" type="button" onClick={addInputModels}>加入</button>
              </div>
              <span className="model-connection-editor__help">必须与请求参数 model 完全一致；可用逗号一次填写多个。测试时优先使用输入框中的第一个模型。</span>
            </div>
            {modelIds.size > 0 && (
              <div className="model-connection-editor__chips" aria-label="待保存模型">
                {[...modelIds].map((id) => <span key={id}><code>{id}</code><button type="button" onClick={() => setModelIds((current) => { const next = new Set(current); next.delete(id); return next; })} aria-label={`移除模型 ${id}`}><X size={11} /></button></span>)}
              </div>
            )}
          </section>

          <button className="models-settings-panel__advanced-toggle" type="button" onClick={() => setShowAdvanced((current) => !current)}><ChevronDown size={14} className={showAdvanced ? "model-connection-editor__chevron--open" : ""} />高级设置</button>
          {showAdvanced && (
            <div className="models-settings-panel__advanced">
              <div className="models-settings-panel__field-row">
                <div className="models-settings-panel__field"><label className="models-settings-panel__label" htmlFor="model-api-backend">请求协议</label><select id="model-api-backend" className="models-settings-panel__select" value={draft.apiBackend} onChange={(event) => setDraft((current) => ({ ...current, apiBackend: event.target.value as ApiBackend }))} disabled={isOjlabBaseUrl(draft.baseUrl)}><option value="chat_completions">Chat Completions</option><option value="responses">Responses</option><option value="messages">Messages</option></select></div>
                <div className="models-settings-panel__field"><label className="models-settings-panel__label" htmlFor="model-auth-scheme">认证方式</label><select id="model-auth-scheme" className="models-settings-panel__select" value={draft.authScheme} onChange={(event) => setDraft((current) => ({ ...current, authScheme: event.target.value as AuthScheme }))} disabled={isOjlabBaseUrl(draft.baseUrl)}><option value="bearer">Bearer Token</option><option value="x_api_key">X-API-Key</option></select></div>
              </div>
              <div className="models-settings-panel__field"><label className="models-settings-panel__label" htmlFor="model-context-window">默认上下文窗口（可选）</label><input id="model-context-window" className="models-settings-panel__input" type="number" min={1} value={draft.contextWindow} onChange={(event) => setDraft((current) => ({ ...current, contextWindow: event.target.value }))} placeholder="例如 128000" /></div>
            </div>
          )}

          <section className="model-connection-editor__discovery">
            <div><strong>不知道 Model ID？</strong><span>可尝试读取服务的 /models；不支持时仍可手动配置。</span></div>
            <button className="echo-button echo-button--ghost echo-button--small" type="button" onClick={() => void handleDiscover()} disabled={discovering}>
              <span className="echo-button__content">{discovering ? <Loader2 className="models-settings-panel__spin" size={13} /> : <CloudDownload size={13} />}{discovering ? "正在获取…" : "获取模型列表（可选）"}</span>
            </button>
          </section>
          {discoveryMessage && <div className={`model-connections__message model-connections__message--${discoveryMessage.kind}`}>{discoveryMessage.text}</div>}
          {discovered.length > 0 && (
            <div className="model-connection-editor__discovered">
              <input className="models-settings-panel__input" type="search" value={search} onChange={(event) => setSearch(event.target.value)} placeholder="筛选已获取的模型" />
              <div className="model-connection-editor__model-picker">
                {visibleModels.length === 0 ? <div className="model-connections__models-empty">没有匹配的模型。</div> : visibleModels.map((model) => (
                  <label key={model.id}>
                    <input type="checkbox" checked={modelIds.has(model.id)} onChange={() => setModelIds((current) => { const next = new Set(current); if (next.has(model.id)) next.delete(model.id); else next.add(model.id); return next; })} />
                    <span><strong>{model.id}</strong>{model.ownedBy && <small>{model.ownedBy}</small>}</span>
                    {existingModels.some((entry) => modelRemoteId(entry) === model.id) && <em>已配置</em>}
                  </label>
                ))}
              </div>
            </div>
          )}
          {error && <div className="models-settings-panel__editor-error">{error}</div>}
        </div>

        <footer className="models-settings-panel__editor-footer model-connection-editor__footer">
          <p className="model-connection-editor__test-note">测试会向所选模型发送一条极短请求，可能产生少量 API 用量；直接保存不会发起请求。</p>
          <button className="echo-button echo-button--secondary echo-button--medium" onClick={onCancel}>取消</button>
          <div className="model-connection-editor__footer-actions">
            <button className="echo-button echo-button--secondary echo-button--medium" onClick={() => void handleSave()} disabled={saving || testing}>{saving ? "保存中…" : "直接保存"}</button>
            <button className="echo-button echo-button--primary echo-button--medium" onClick={() => void handleTestAndSave()} disabled={saving || testing}><span className="echo-button__content">{testing || saving ? <Loader2 className="models-settings-panel__spin" size={13} /> : <Check size={13} />}{testing ? "正在测试模型…" : saving ? "保存中…" : "测试并保存"}</span></button>
          </div>
        </footer>
      </div>
    </div>
  );
}

function ManualModelEditor({ provider, original, onCancel, onSaved }: { provider: ModelProviderEntry; original?: ModelEntry; onCancel: () => void; onSaved: () => Promise<void> }) {
  const [remoteModelId, setRemoteModelId] = useState(original ? modelRemoteId(original) : "");
  const [name, setName] = useState(original?.name ?? "");
  const [contextWindow, setContextWindow] = useState(original?.contextWindow ? String(original.contextWindow) : "");
  const [maxOutputTokens, setMaxOutputTokens] = useState(original?.maxOutputTokens ? String(original.maxOutputTokens) : "");
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const dialogRef = useModalFocus<HTMLDivElement>(true, onCancel);

  const save = async () => {
    if (!remoteModelId.trim()) { setError("请填写远端模型 ID。"); return; }
    const parsedContext = contextWindow.trim() ? Number(contextWindow) : undefined;
    const parsedMaxOutput = maxOutputTokens.trim() ? Number(maxOutputTokens) : undefined;
    if (parsedContext !== undefined && (!Number.isSafeInteger(parsedContext) || parsedContext <= 0)) { setError("上下文窗口必须是大于 0 的整数。"); return; }
    if (parsedMaxOutput !== undefined && (!Number.isInteger(parsedMaxOutput) || parsedMaxOutput <= 0 || parsedMaxOutput > 0xffffffff)) { setError("最大输出必须是大于 0 的整数，且不超过 4,294,967,295。"); return; }
    if (parsedMaxOutput !== undefined && parsedMaxOutput > (parsedContext ?? provider.contextWindow ?? Infinity)) { setError("最大输出不能超过上下文窗口。"); return; }
    setSaving(true);
    try {
      await providersSaveConnection(
        { ...provider, apiKey: undefined },
        [{
          modelId: original?.modelId ?? "",
          remoteModelId: remoteModelId.trim(),
          providerId: provider.id,
          name: name.trim() || undefined,
          contextWindow: parsedContext,
          maxOutputTokens: parsedMaxOutput,
          clearMaxOutputTokens: original?.maxOutputTokens !== undefined && parsedMaxOutput === undefined,
        }],
      );
      await onSaved();
    } catch (saveError) {
      setError(String(saveError));
    } finally {
      setSaving(false);
    }
  };

  return (
    <div ref={dialogRef} className="models-settings-panel__editor-overlay" role="dialog" aria-modal="true" aria-label={original ? "编辑模型" : "手动添加模型"} tabIndex={-1}>
      <div className="models-settings-panel__editor models-settings-panel__editor--narrow">
        <header className="models-settings-panel__editor-header"><div><div className="models-settings-panel__editor-title">{original ? "编辑模型" : "手动添加模型"}</div><div className="models-settings-panel__editor-note">连接：{connectionName(provider)}</div></div><button className="echo-button echo-button--ghost echo-button--small echo-button--icon-only" onClick={onCancel} aria-label="关闭"><X size={14} /></button></header>
        <div className="models-settings-panel__editor-body">
          <div className="models-settings-panel__field"><label className="models-settings-panel__label">远端模型 ID</label><input className="models-settings-panel__input" value={remoteModelId} onChange={(event) => setRemoteModelId(event.target.value)} placeholder="例如 MiniMax-M3、gpt-5" data-modal-initial-focus /><span className="model-connection-editor__help">必须与 API 请求中使用的 model 值完全一致。</span></div>
          <div className="models-settings-panel__field"><label className="models-settings-panel__label">显示名称（可选）</label><input className="models-settings-panel__input" value={name} onChange={(event) => setName(event.target.value)} placeholder="例如 工作用 MiniMax" /></div>
          <div className="models-settings-panel__field"><label className="models-settings-panel__label">上下文窗口（可选）</label><input className="models-settings-panel__input" type="number" min={1} value={contextWindow} onChange={(event) => setContextWindow(event.target.value)} placeholder="留空使用连接默认值" /></div>
          <div className="models-settings-panel__field"><label className="models-settings-panel__label" htmlFor="manual-model-max-output">最大输出 tokens（可选）</label><input id="manual-model-max-output" className="models-settings-panel__input" type="number" min={1} step={1} max={4294967295} value={maxOutputTokens} onChange={(event) => setMaxOutputTokens(event.target.value)} placeholder="留空使用模型默认值" /></div>
          {error && <div className="models-settings-panel__editor-error">{error}</div>}
        </div>
        <footer className="models-settings-panel__editor-footer"><button className="echo-button echo-button--secondary echo-button--medium" onClick={onCancel}>取消</button><button className="echo-button echo-button--primary echo-button--medium" onClick={() => void save()} disabled={saving}>{saving ? "保存中…" : "保存模型"}</button></footer>
      </div>
    </div>
  );
}

function ModelImportDialog({ provider, existingModels, onCancel, onSaved }: { provider: ModelProviderEntry; existingModels: ModelEntry[]; onCancel: () => void; onSaved: (count: number) => Promise<void> }) {
  const [models, setModels] = useState<FetchedModel[]>([]);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const dialogRef = useModalFocus<HTMLDivElement>(true, onCancel);
  const existing = useMemo(() => new Set(existingModels.map(modelRemoteId)), [existingModels]);

  useEffect(() => {
    void providersFetchModelsForProvider(provider.id)
      .then(setModels)
      .catch((requestError) => setError(String(requestError)))
      .finally(() => setLoading(false));
  }, [provider.id]);

  const save = async () => {
    setSaving(true);
    try {
      await providersSaveConnection(
        { ...provider, apiKey: undefined },
        [...selected].map((remoteModelId) => ({
          modelId: "",
          remoteModelId,
          providerId: provider.id,
        })),
      );
      await onSaved(selected.size);
    } catch (saveError) {
      setError(String(saveError));
    } finally {
      setSaving(false);
    }
  };

  return (
    <div ref={dialogRef} className="models-settings-panel__editor-overlay" role="dialog" aria-modal="true" aria-label="同步模型" tabIndex={-1}>
      <div className="models-settings-panel__editor">
        <header className="models-settings-panel__editor-header"><div><div className="models-settings-panel__editor-title">同步模型</div><div className="models-settings-panel__editor-note">直接使用「{connectionName(provider)}」中已保存的密钥</div></div><button className="echo-button echo-button--ghost echo-button--small echo-button--icon-only" onClick={onCancel} aria-label="关闭" data-modal-initial-focus><X size={14} /></button></header>
        <div className="models-settings-panel__editor-body">
          {loading ? <div className="model-connections__empty"><Loader2 className="models-settings-panel__spin" size={18} />正在获取模型列表…</div> : error ? <div className="models-settings-panel__editor-error">{error}</div> : (
            <div className="model-connection-editor__model-picker">
              {models.map((model) => {
                const configured = existing.has(model.id);
                return <label key={model.id} className={configured ? "disabled" : ""}><input type="checkbox" checked={configured || selected.has(model.id)} disabled={configured} onChange={() => setSelected((current) => { const next = new Set(current); if (next.has(model.id)) next.delete(model.id); else next.add(model.id); return next; })} /><span><strong>{model.id}</strong>{model.ownedBy && <small>{model.ownedBy}</small>}</span>{configured && <em>已配置</em>}</label>;
              })}
              {models.length === 0 && <div className="model-connections__models-empty">服务端没有返回模型，请关闭后使用“手动添加”。</div>}
            </div>
          )}
        </div>
        <footer className="models-settings-panel__editor-footer"><button className="echo-button echo-button--secondary echo-button--medium" onClick={onCancel}>取消</button><button className="echo-button echo-button--primary echo-button--medium" onClick={() => void save()} disabled={saving || selected.size === 0}>{saving ? "保存中…" : `添加 ${selected.size || ""} 个模型`}</button></footer>
      </div>
    </div>
  );
}
