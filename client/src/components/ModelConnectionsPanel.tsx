import { withoutRemovedModels } from "@/lib/retired-models";
import { PageHeading } from "./PageHeading";
import { modelDisplayName } from "@/lib/model-presentation";
import { modelTestDuration, useModelConnectionTests } from "@/lib/use-model-connection-tests";
import { SettingsDisclosure } from "./SettingsDisclosure";
import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import {
  Building2,
  CheckCircle2,
  ChevronDown,
  CircleAlert,
  CloudDownload,
  Eye,
  EyeOff,
  Loader2,
  LockKeyhole,
  Pencil,
  Plus,
  RefreshCw,
  Server,
  Star,
  ArrowLeft,
  ChevronRight,
  Trash2,
  X,
} from "lucide-react";
import {
  agentsDefaultsGet,
  agentsDefaultsSave,
  agentAuthStatus,
  internalReload,
  openExternalUrl,
  providersDeleteModel,
  providersDeleteProvider,
  providersFetchModelsForProvider,
  providersList,
  providersSaveConnection,
  providersTestConnection,
  type ApiBackend,
  type AuthScheme,
  type FetchedModel,
  type ModelEntry,
  type ModelProviderEntry,
  type ProviderKind,
  type ProviderListModel,
} from "@/lib/agent-client";
import { useModalFocus } from "@/lib/use-modal-focus";
import { useUnsavedClose } from "@/lib/use-unsaved-close";
import { listenOrgModelsChanged, orgSyncModelConfig } from "@/lib/org-client";
import type { AgentDefaults } from "@/lib/types";
import { useOrgSessionStore } from "@/stores/org-session-store";
import { useAppDialog } from "./AppDialog";
import { useSettingsDraft } from "@/lib/settings-drafts";
import { Button } from "./Button";

interface ModelConnectionsPanelProps {
  onModelsChanged?: () => void | Promise<void>;
  initialModelId?: string;
  onOpenOrganization?: () => void;
  overviewRequest?: number;
}

interface ProviderPreset {
  label: string;
  shortLabel: string;
  baseUrl: string;
  apiBackend: ApiBackend;
  authScheme: AuthScheme;
  placeholderKey: string;
  keyUrl?: string;
}

type ServiceKind = ProviderKind | "kimi" | "zhipu";
const SERVICE_GROUPS: { label: string; kinds: ServiceKind[] }[] = [
  { label: "国内服务商", kinds: ["deepseek", "qwen", "kimi", "zhipu", "minimax"] },
  { label: "其他服务商", kinds: ["openai", "anthropic"] },
  { label: "自定义接口", kinds: ["custom", "custom_anthropic"] },
];

const PROVIDER_PRESETS: Record<ServiceKind, ProviderPreset> = {
  openai: {
    label: "OpenAI",
    shortLabel: "OpenAI",
    baseUrl: "https://api.openai.com/v1",
    apiBackend: "responses",
    authScheme: "bearer",
    placeholderKey: "sk-...",
    keyUrl: "https://platform.openai.com/api-keys",
  },
  anthropic: {
    label: "Claude（Anthropic）",
    shortLabel: "Claude",
    baseUrl: "https://api.anthropic.com/v1",
    apiBackend: "messages",
    authScheme: "x_api_key",
    placeholderKey: "sk-ant-...",
    keyUrl: "https://console.anthropic.com/settings/keys",
  },
  minimax: {
    label: "MiniMax",
    shortLabel: "MiniMax",
    baseUrl: "https://api.minimax.cn/v1",
    apiBackend: "chat_completions",
    authScheme: "bearer",
    placeholderKey: "MiniMax API Key",
    keyUrl: "https://platform.minimaxi.com",
  },
  deepseek: {
    label: "DeepSeek",
    shortLabel: "DeepSeek",
    baseUrl: "https://api.deepseek.com",
    apiBackend: "chat_completions",
    authScheme: "bearer",
    placeholderKey: "sk-...",
    keyUrl: "https://platform.deepseek.com",
  },
  qwen: {
    label: "阿里云百炼（通义千问）",
    shortLabel: "阿里云百炼",
    baseUrl: "https://dashscope.aliyuncs.com/compatible-mode/v1",
    apiBackend: "chat_completions",
    authScheme: "bearer",
    placeholderKey: "sk-...",
    keyUrl: "https://bailian.console.aliyun.com",
  },
  // UI shortcuts for the existing custom Chat Completions transport. No new
  // provider kind, credential format, or runtime routing is introduced.
  kimi: {
    label: "Kimi（月之暗面）", shortLabel: "Kimi",
    baseUrl: "https://api.moonshot.cn/v1",
    apiBackend: "chat_completions", authScheme: "bearer",
    placeholderKey: "填写 Kimi API 密钥", keyUrl: "https://platform.moonshot.cn",
  },
  zhipu: {
    label: "智谱 GLM", shortLabel: "智谱 GLM",
    baseUrl: "https://open.bigmodel.cn/api/paas/v4/",
    apiBackend: "chat_completions", authScheme: "bearer",
    placeholderKey: "填写智谱 API 密钥", keyUrl: "https://bigmodel.cn/usercenter/proj-mgmt/apikeys",
  },
  custom: {
    label: "自定义（OpenAI 兼容）",
    shortLabel: "OpenAI 兼容",
    baseUrl: "",
    apiBackend: "chat_completions",
    authScheme: "bearer",
    placeholderKey: "your-api-key",
  },
  custom_anthropic: {
    label: "自定义（Anthropic 兼容）",
    shortLabel: "Anthropic 兼容",
    baseUrl: "",
    apiBackend: "messages",
    authScheme: "x_api_key",
    placeholderKey: "sk-ant-...",
  },
};

interface ConnectionDraft {
  service: ServiceKind;
  label: string;
  providerKind: ProviderKind;
  baseUrl: string;
  allowInsecureHttp: boolean;
  apiKey: string;
  apiBackend: ApiBackend;
  authScheme: AuthScheme;
  contextWindow: string;
}

type PanelMessage = { kind: "ok" | "err" | "warn"; text: string; retry?: "load" | "organization" };
type ModelMutation = (action: () => Promise<void>) => Promise<void>;

function serviceKind(provider: ModelProviderEntry): ServiceKind {
  if (provider.providerKind === "custom" && (!provider.apiBackend || provider.apiBackend === "chat_completions")
    && (!provider.authScheme || provider.authScheme === "bearer")) {
    const address = provider.baseUrl?.replace(/\/+$/, "");
    for (const kind of ["kimi", "zhipu"] as const) {
      if (address === PROVIDER_PRESETS[kind].baseUrl.replace(/\/+$/, "")) return kind;
    }
  }
  return provider.providerKind;
}

function connectionName(provider: ModelProviderEntry): string {
  if (provider.source === "organization") return provider.label || "组织提供";
  return provider.label || PROVIDER_PRESETS[serviceKind(provider)]?.shortLabel || provider.id;
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

function protocolLabel(provider: ModelProviderEntry): string {
  if (provider.apiBackend === "messages") return "Anthropic Messages";
  if (provider.apiBackend === "responses") return "OpenAI Responses";
  return "OpenAI Chat Completions";
}

function modelRemoteId(model: ModelEntry): string {
  return model.remoteModelId || model.modelId;
}

function modelName(model: ModelEntry): string {
  return modelDisplayName({ id: model.modelId, label: model.name, remoteModelId: model.remoteModelId });
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

export function ModelConnectionsPanel({ onModelsChanged, initialModelId, onOpenOrganization, overviewRequest = 0 }: ModelConnectionsPanelProps) {
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
  const { results: modelTests, test: testModel } = useModelConnectionTests(catalog);
  const modelRowsRef = useRef(new Map<string, HTMLLIElement>());
  const locatedModelRef = useRef<string>();
  const reloadGenerationRef = useRef(0);
  const panelRef = useRef<HTMLDivElement>(null);
  const pendingModelFocus = useRef(initialModelId);
  const requestedModel = useRef(initialModelId);
  requestedModel.current = initialModelId;
  const [view, setView] = useState<"overview" | "connections" | "detail">(initialModelId ? "detail" : "overview");
  const [loadError, setLoadError] = useState("");
  const [readiness, setReadiness] = useState<{ ready: boolean; reason?: string } | null>(null);
  const [busy, setBusy] = useState(false);
  const operationRef = useRef(false);
  const [orgSyncState, setOrgSyncState] = useState<"unknown" | "empty" | "saved" | "error">("unknown");
  const organizationSession = useOrgSessionStore((state) => state.session);
  const organizationHydrated = useOrgSessionStore((state) => state.hydrated);
  const organizationBoundary = useOrgSessionStore((state) => state.boundaryRevision);
  const currentOrganizationBoundary = useRef(organizationBoundary); currentOrganizationBoundary.current = organizationBoundary;
  const hydrateOrganization = useOrgSessionStore((state) => state.hydrate);
  const { requestConfirmation, dialog } = useAppDialog(selectedProviderId);
  useSettingsDraft({ dirty: false, busy: busy || syncingOrganization, label: "模型设置" });
  const openDetail = (id: string, modelId?: string) => { pendingModelFocus.current = modelId; setSelectedProviderId(id); setView("detail"); };
  const previousOverviewRequest = useRef(overviewRequest);
  useEffect(() => {
    if (previousOverviewRequest.current === overviewRequest) return;
    previousOverviewRequest.current = overviewRequest;
    pendingModelFocus.current = undefined;
    setConnectionEditor(null); setModelEditor(null); setImportProvider(null); setView("overview");
  }, [overviewRequest]);
  const previousView = useRef(view);
  useEffect(() => {
    if (loading) return;
    const changedView = previousView.current !== view;
    previousView.current = view;
    const row = pendingModelFocus.current ? modelRowsRef.current.get(pendingModelFocus.current) : null;
    if (row) { row.scrollIntoView?.({ block: "nearest" }); row.focus({ preventScroll: true }); pendingModelFocus.current = undefined; }
    else if (changedView) {
      panelRef.current?.scrollTo?.({ top: 0 });
      const heading = panelRef.current?.querySelector<HTMLElement>("h2"); heading?.setAttribute("tabindex", "-1"); heading?.focus({ preventScroll: true });
    }
  }, [view, selectedProviderId, loading, catalog]);
  const reload = useCallback(async (quiet = false): Promise<string | null> => {
    if (!quiet) setLoading(true);
    const generation = ++reloadGenerationRef.current;
    try {
      const auth = await agentAuthStatus();
      const [rawCatalog, nextDefaults] = await Promise.all([
        providersList(),
        agentsDefaultsGet(),
      ]);
      if (generation !== reloadGenerationRef.current) return null;
      const nextCatalog = withoutRemovedModels(rawCatalog);
      setCatalog(nextCatalog);
      setLoadError("");
      setReadiness({ ready: auth.ready, reason: auth.reason });
      setDefaults(nextDefaults);
      setEffectiveDefaultModelId(auth.defaultModelId ?? null);
      setSelectedProviderId((current) => {
        if (current && nextCatalog.providers.some((provider) => provider.id === current)) {
          return current;
        }
        return nextCatalog.models.find(model => model.modelId === requestedModel.current)?.providerId
          ?? nextCatalog.providers.find((provider) => provider.source === "organization")?.id
          ?? nextCatalog.providers[0]?.id
          ?? null;
      });
      return null;
    } catch (error) {
      if (generation !== reloadGenerationRef.current) return null;
      const detail = String(error).replace(/^Error:\s*/, "");
      setLoadError(`读取模型配置失败：${detail}`);
      return detail;
    } finally {
      if (generation === reloadGenerationRef.current) setLoading(false);
    }
  }, []);

  useEffect(() => {
    void reload();
    void hydrateOrganization();
    let unlisten: (() => void) | undefined;
    let disposed = false;
    void listenOrgModelsChanged(() => void reload(true)).then(dispose => { if (disposed) dispose(); else unlisten = dispose; });
    return () => { disposed = true; reloadGenerationRef.current += 1; unlisten?.(); };
  }, [hydrateOrganization, reload]);

  const previousOrganizationBoundary = useRef(organizationBoundary);
  useLayoutEffect(() => {
    if (previousOrganizationBoundary.current === organizationBoundary) return;
    previousOrganizationBoundary.current = organizationBoundary;
    setOrgSyncState("unknown"); setCatalog({ providers: [], models: [] }); setDefaults(null); setReadiness(null);
    setSelectedProviderId(null); pendingModelFocus.current = undefined;
    void reload();
  }, [organizationBoundary, reload]);
  const previousModelRequest = useRef(initialModelId);
  useEffect(() => {
    if (previousModelRequest.current !== initialModelId) {
      previousModelRequest.current = initialModelId;
      locatedModelRef.current = undefined;
    }
    if (!initialModelId || loading || locatedModelRef.current === initialModelId) return;
    const model = catalog.models.find(value => value.modelId === initialModelId);
    if (model) {
      locatedModelRef.current = initialModelId;
      if (view !== "detail" || selectedProviderId !== model.providerId) openDetail(model.providerId, model.modelId);
      else { const row = modelRowsRef.current.get(model.modelId); row?.scrollIntoView?.({ block: "nearest" }); row?.focus({ preventScroll: true }); pendingModelFocus.current = undefined; }
    }
  }, [initialModelId, loading, catalog, view, selectedProviderId]);

  const organizationProviders = useMemo(
    () => catalog.providers.filter((provider) => provider.source === "organization"),
    [catalog.providers],
  );
  const selectedProvider = catalog.providers.find((provider) => provider.id === selectedProviderId) ?? null;
  const selectedModels = selectedProvider
    ? catalog.models.filter((model) => model.providerId === selectedProvider.id)
    : [];
  const savedDefaultModelId = defaults?.defaultModel?.trim() || null;
  const savedDefaultMissing = savedDefaultModelId !== null
    && !catalog.models.some((model) => model.modelId === savedDefaultModelId);

  const runMutation: ModelMutation = async (action) => {
    if (operationRef.current) throw new Error("模型设置正在处理，请等待完成后重试。");
    operationRef.current = true; setBusy(true);
    try { await action(); } finally { operationRef.current = false; setBusy(false); }
  };
  const finishMutation = async (success: string) => {
    const reloadError = await reloadRuntime();
    const catalogError = await reload(true);
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
        retry: "load",
        text: `${success} 配置已保存，但暂未生效：${refreshErrors.join("；")}。请重试加载，不要重复保存或删除。`,
      }
      : { kind: "ok", text: success });
  };

  const handleDeleteProvider = (provider: ModelProviderEntry) => {
    if (provider.managed || provider.source === "organization") return;
    const count = catalog.models.filter((model) => model.providerId === provider.id).length;
    requestConfirmation({
      title: `删除连接“${connectionName(provider)}”？`,
      description: count
        ? `该连接下的 ${count} 个模型也会同时删除。${catalog.models.some(model => model.providerId === provider.id && model.modelId === savedDefaultModelId) ? "其中包含默认模型，删除后请重新选择默认模型。" : ""}此操作无法撤销。`
        : "该连接将从本机删除，此操作无法撤销。",
      confirmLabel: "删除连接",
      danger: true,
      action: () => runMutation(async () => {
        await providersDeleteProvider(provider.id);
        await finishMutation("连接及其模型已删除。");
        setView("connections");
      }),
      onError: (error) => setMessage({ kind: "err", text: `删除失败：${String(error)}` }),
    });
  };

  const handleDeleteModel = (model: ModelEntry) => {
    if (model.managed || catalog.providers.find(value => value.id === model.providerId)?.source === "organization") return;
    const label = modelName(model);
    requestConfirmation({
      title: `删除模型“${label}”？`,
      description: "该模型将从当前连接中移除，且不再可用于新对话。",
      confirmLabel: "删除模型",
      danger: true,
      action: () => runMutation(async () => {
        await providersDeleteModel(model.modelId);
        await finishMutation("模型已删除。");
      }),
      onError: (error) => setMessage({ kind: "err", text: `删除失败：${String(error)}` }),
    });
  };

  const handleSetDefault = async (model: ModelEntry) => {
    try {
      await runMutation(async () => {
        const current = defaults ?? await agentsDefaultsGet();
        await agentsDefaultsSave({ ...current, defaultModel: model.modelId });
        await finishMutation(`已将「${modelName(model)}」设为默认模型。`);
      });
    } catch (error) {
      setMessage({ kind: "err", text: `设置默认模型失败：${String(error)}` });
    }
  };

  const handleOrganizationSync = async () => {
    if (operationRef.current || !organizationSession?.loggedIn || organizationSession.requiresReauthentication) return;
    const boundary = currentOrganizationBoundary.current;
    await runMutation(async () => {
      setSyncingOrganization(true); setMessage(null);
      try {
        const result = await orgSyncModelConfig();
        if (boundary !== currentOrganizationBoundary.current) return;
        setOrgSyncState(result.configured ? "saved" : "empty");
        await finishMutation(result.configured ? "组织模型配置已更新。" : "组织暂未提供模型。");
      } catch (error) {
        if (boundary === currentOrganizationBoundary.current) { setOrgSyncState("error"); setMessage({ kind: "err", retry: "organization", text: `暂时无法获取组织模型：${String(error).replace(/^Error:\s*/, "")}` }); }
      } finally { setSyncingOrganization(false); }
    });
  };
  const retryRefresh = () => void runMutation(async () => { await finishMutation("模型状态已刷新。"); });
  const organizationExpired = Boolean(organizationSession?.requiresReauthentication) || (organizationHydrated && organizationProviders.length > 0 && !organizationSession?.loggedIn);
  // Shared scopes establish organization context, never model entitlement.
  // A server login alone can belong to a personal account; downloaded models
  // remain authoritative even for accounts without shared knowledge scopes.
  const hasOrganizationContext = Boolean(organizationSession?.loggedIn && organizationSession.bootstrap?.scopes.some(scope => scope.kind === "team" || scope.kind === "org"));
  const showOrganization = organizationProviders.length > 0 || hasOrganizationContext || Boolean(organizationSession?.requiresReauthentication);
  const organizationCopy = organizationExpired ? "需要重新登录组织"
    : orgSyncState === "error" ? "暂时无法获取组织模型" : organizationProviders.length > 0 ? "由组织管理员维护"
    : orgSyncState === "empty" ? "组织暂未提供模型" : "尚未获取组织模型，刷新后确认";
  const modelProvider = (model: ModelEntry) => catalog.providers.find(value => value.id === model.providerId);
  const defaultModel = catalog.models.find(model => model.modelId === savedDefaultModelId);
  const defaultProvider = defaultModel ? modelProvider(defaultModel) : undefined;
  const effectiveModel = catalog.models.find(model => model.modelId === effectiveDefaultModelId);
  const displayedProvider = defaultProvider ?? (!savedDefaultModelId && effectiveModel ? modelProvider(effectiveModel) : undefined);
  const modelChoiceName = (model: ModelEntry) => catalog.models.filter(value => modelName(value) === modelName(model)).length > 1 ? `${modelName(model)} · ${connectionName(modelProvider(model)!)}` : modelName(model);
  const personalProviders = catalog.providers.filter(provider => provider.source !== "organization");
  const renderConnections = (providers: ModelProviderEntry[]) => <div className="model-settings__connections">{providers.map(provider => <button className="model-settings__connection" type="button" key={provider.id} disabled={busy} onClick={() => openDetail(provider.id)}>
    <span className="model-settings__connection-icon">{provider.source === "organization" ? <Building2 size={19} /> : <Server size={19} />}</span>
    <span><strong>{connectionName(provider)}</strong>{provider.source === "organization" || provider.managed
      ? <span>由组织管理</span>
      : !catalog.models.some(model => model.providerId === provider.id) ? <span>尚未添加模型</span>
      : provider.credentialConfigured === false ? <span>尚未配置密钥</span>
      : connectionName(provider) !== PROVIDER_PRESETS[serviceKind(provider)]?.shortLabel && <span>{PROVIDER_PRESETS[serviceKind(provider)]?.label}</span>}</span><ChevronRight size={16} />
  </button>)}</div>;
  const organizationStatus = showOrganization && <section className="model-settings__organization" aria-label="组织模型状态">
    <div><strong>组织提供</strong><p>{organizationCopy}</p></div>
    {organizationSession?.loggedIn && !organizationExpired ? <Button size="compact" disabled={busy || syncingOrganization} onClick={() => void handleOrganizationSync()}><RefreshCw size={14} />{syncingOrganization ? "刷新中…" : "刷新组织模型"}</Button>
      : onOpenOrganization ? <Button size="compact" disabled={busy} onClick={onOpenOrganization}>前往组织登录</Button> : <span>请在“组织”页面重新登录</span>}
  </section>;
  return <div ref={panelRef} className={`model-connections model-connections--${view}`}>
    {view !== "overview" && <Button variant="ghost" size="compact" className="model-settings__back" disabled={busy} onClick={() => setView(view === "detail" ? "connections" : "overview")}><ArrowLeft size={14} />{view === "detail" ? "返回连接管理" : "返回模型设置"}</Button>}
    <header className="model-connections__header" data-setting-id="model-settings">
      <PageHeading title={view === "overview" ? "模型" : view === "connections" ? "连接管理" : "连接详情"} level={2} description={view === "overview" ? "选择默认使用的模型，管理模型连接。" : undefined} />
      {!loading && !loadError && view === "connections" && <Button variant="primary" disabled={busy} onClick={() => setConnectionEditor("new")}><Plus size={15} />添加模型连接</Button>}
    </header>
    {message && <div className={`model-connections__message model-connections__message--${message.kind}`} role={message.kind === "err" ? "alert" : "status"}>{message.text}{message.retry === "load" && <Button size="compact" disabled={busy} onClick={retryRefresh}>重试加载</Button>}{message.retry === "organization" && <Button size="compact" disabled={busy} onClick={() => void handleOrganizationSync()}>重试获取</Button>}</div>}
    {loadError && <div className="panel-inline-error" role="alert"><span>{loadError}</span><Button size="compact" disabled={loading || busy} onClick={() => void reload()}>重新读取</Button></div>}
    {loading ? <p className="model-settings__hint" role="status">正在读取模型配置…</p> : !loadError && <>
      {view === "overview" && <>
        {catalog.models.length > 0 ? <>
          <section className="model-settings__default" aria-label="默认模型">
            <label htmlFor="settings-default-model">默认模型</label>
            <div className="model-settings__select-shell">
              <select id="settings-default-model" className="form-control model-settings__default-select" value={savedDefaultModelId ?? ""} disabled={busy} onChange={event => {
                const model = catalog.models.find(value => value.modelId === event.target.value);
                const provider = model && modelProvider(model);
                if (model && !(provider?.source === "organization" && organizationExpired)) void handleSetDefault(model);
              }}>
                {!savedDefaultModelId && <option value="" disabled>自动选择{effectiveModel ? ` · ${modelName(effectiveModel)}` : ""}</option>}
                {savedDefaultMissing && <option value={savedDefaultModelId!} disabled>{modelDisplayName(savedDefaultModelId)}（不可用）</option>}
                {(["organization", "personal"] as const).map(source => {
                  const models = catalog.models.filter(model => { const provider = modelProvider(model); return provider && (source === "organization" ? provider.source === "organization" : provider.source !== "organization"); });
                  return models.length > 0 && <optgroup key={source} label={source === "organization" ? "组织提供" : "个人连接"}>{models.map(model => {
                    return <option key={model.modelId} value={model.modelId} disabled={source === "organization" && organizationExpired}>{modelChoiceName(model)}{source === "organization" && organizationExpired ? "（需重新登录）" : ""}</option>;
                  })}</optgroup>;
                })}
              </select><ChevronDown size={17} aria-hidden="true" />
            </div>
            <p>{displayedProvider ? `${displayedProvider.source === "organization" ? "组织提供" : "来自个人连接"} · ${connectionName(displayedProvider)}` : "请选择默认使用的模型。"}</p>
            {effectiveModel && savedDefaultModelId && savedDefaultModelId !== effectiveDefaultModelId && <p>当前自动使用「{modelName(effectiveModel)}」</p>}
            {savedDefaultMissing && <p role="note">已设默认模型「{modelDisplayName(savedDefaultModelId)}」不在当前模型目录中，请重新选择。</p>}
            {readiness?.ready === false && <div className="model-settings__readiness" role="alert"><span>模型暂未就绪：{readiness.reason || "请检查连接或重试加载。"}</span><Button size="compact" disabled={busy} onClick={retryRefresh}>重试加载</Button></div>}
          </section>
        </> : <div className="model-connections__empty"><Server size={24} aria-hidden="true" /><strong>还没有可用模型</strong><span>添加模型连接，填写服务信息后即可配置模型。</span><Button variant="primary" disabled={busy} onClick={() => setConnectionEditor("new")}>添加模型连接</Button></div>}
        {(catalog.providers.length > 0 || showOrganization) && <button type="button" className="model-settings__manage-entry" aria-label="管理模型连接" disabled={busy} onClick={() => setView("connections")}><span className="model-settings__connection-icon"><Server size={20} /></span><span><strong>管理模型连接</strong><span>添加服务、管理密钥和可用模型</span></span><ChevronRight size={18} aria-hidden="true" /></button>}
      </>}
      {view === "connections" && <div className="model-settings__connection-groups" role="group" aria-label="模型连接">
        <section className="model-settings__connection-group" aria-label="个人连接"><h3>个人连接</h3>{personalProviders.length > 0 ? renderConnections(personalProviders) : <p className="model-settings__hint model-settings__group-empty">尚未添加个人连接，点击右上方“添加模型连接”即可接入。</p>}</section>
        {showOrganization && <section className="model-settings__connection-group">{organizationStatus}{renderConnections(organizationProviders)}</section>}
      </div>}
      {view === "detail" && selectedProvider && <>
        {selectedProvider.source === "organization" && organizationStatus}
            <section
              key={selectedProvider.id}
              className="model-connections__detail"
              aria-label={`${connectionName(selectedProvider)}连接详情`}
              tabIndex={0}
            >
              <header className="model-connections__detail-header">
                <div>
                  <div className="model-connections__title-row">
                    <h3>{connectionName(selectedProvider)}</h3>
                    {selectedProvider.managed || selectedProvider.source === "organization" ? (
                      <span className="model-connections__badge model-connections__badge--managed"><LockKeyhole size={11} />组织托管</span>
                    ) : (
                      <span className="model-connections__badge">个人配置</span>
                    )}
                    {selectedProvider.credentialConfigured && <span className="model-connections__badge">密钥已配置</span>}
                  </div>
                  <p>{PROVIDER_PRESETS[serviceKind(selectedProvider)]?.label || selectedProvider.providerKind}</p>
                </div>
                <div className="model-connections__detail-actions">
                  {!(selectedProvider.managed || selectedProvider.source === "organization") && (
                    <>
                      <button className="echo-button echo-button--ghost echo-button--small echo-button--icon-only" disabled={busy} onClick={() => setConnectionEditor(selectedProvider)} aria-label="编辑连接" title="编辑连接"><Pencil size={14} /></button>
                      <button className="echo-button echo-button--ghost echo-button--small echo-button--icon-only echo-button--danger" disabled={busy} onClick={() => void handleDeleteProvider(selectedProvider)} aria-label="删除连接" title="删除连接"><Trash2 size={14} /></button>
                    </>
                  )}
                </div>
              </header>


              {selectedProvider.source !== "organization" && selectedProvider.baseUrl?.startsWith("http://") && (
                <div className="model-connections__message model-connections__message--warn">
                  此连接使用 HTTP 明文传输；API Key、提问和模型回复可能被网络路径上的其他人看到。
                </div>
              )}
              {savedDefaultMissing && (
                <div className="model-connections__message model-connections__message--warn">
                  已设默认模型「{modelDisplayName(savedDefaultModelId)}」不在当前模型目录中；请重新选择。{effectiveDefaultModelId ? `当前自动使用「${modelDisplayName(effectiveDefaultModelId)}」。` : ""}
                </div>
              )}

              <div className="model-connections__models-header">
                <div><strong>已配置模型</strong><span>{selectedModels.length} 个</span></div>
                {!(selectedProvider.managed || selectedProvider.source === "organization") && (
                  <div>
                    <button className="echo-button echo-button--ghost echo-button--small" disabled={busy} onClick={() => setModelEditor({ provider: selectedProvider })}><span className="echo-button__content"><Plus size={13} />手动添加</span></button>
                    <button className="echo-button echo-button--secondary echo-button--small" disabled={busy} onClick={() => setImportProvider(selectedProvider)}><span className="echo-button__content"><CloudDownload size={13} />获取更多模型</span></button>
                  </div>
                )}
              </div>

              {selectedModels.length > 0 && <p className="model-connections__test-hint">测试会发送一次简短请求，可能消耗少量额度。耗时仅代表本次请求。</p>}

              {selectedModels.length === 0 ? (
                <div className="model-connections__models-empty">该连接还没有模型，请同步模型列表或手动添加远端模型 ID。</div>
              ) : (
                <ul className="model-connections__model-list">
                  {selectedModels.map((model) => {
                    const isSavedDefault = savedDefaultModelId === model.modelId;
                    const isEffective = effectiveDefaultModelId === model.modelId;
                    const limits = modelLimitsText(model, selectedProvider);
                    const testResult = modelTests[model.modelId];
                    return (
                      <li key={model.modelId} tabIndex={-1} data-model-id={model.modelId} data-model-highlighted={model.modelId === initialModelId || undefined} ref={element => {
                        if (element) modelRowsRef.current.set(model.modelId, element);
                        else modelRowsRef.current.delete(model.modelId);
                      }}>
                        <div className="model-connections__model-copy">
                          <strong>{modelName(model)}</strong>
                          <span>远端模型 ID：{model.remoteModelId || modelDisplayName(model.modelId)}</span>
                          {limits && <span className="model-connections__model-limits">{limits}</span>}
                        </div>
                        <div className="model-connections__model-actions">
                          <button type="button" className="echo-button echo-button--secondary echo-button--small"
                            aria-label={`测试模型 ${modelName(model)}`} aria-busy={testResult?.status === "testing"}
                            disabled={busy || testResult?.status === "testing" || (selectedProvider.source === "organization" && organizationExpired)} onClick={() => void testModel(selectedProvider, model)}>
                            <span className="echo-button__content">
                              {testResult?.status === "testing" && <Loader2 className="models-settings-panel__spin" size={13} aria-hidden="true" />}
                              {testResult?.status === "testing" ? "测试中…" : testResult ? "重新测试" : "测试模型"}
                            </span>
                          </button>
                          {isSavedDefault && <span className="model-connections__default"><Star size={12} fill="currentColor" />{isEffective ? "已设默认 · 当前使用" : effectiveDefaultModelId ? "已设默认（当前未使用）" : "已设默认"}</span>}
                          {!isSavedDefault && isEffective && <span className="model-connections__default">当前自动使用</span>}
                          {!isSavedDefault && (
                            <button className="echo-button echo-button--ghost echo-button--small" disabled={busy || (selectedProvider.source === "organization" && organizationExpired)} onClick={() => void handleSetDefault(model)}>设为默认</button>
                          )}
                          {!(selectedProvider.managed || selectedProvider.source === "organization") && (
                            <span className="model-connections__edit-actions">
                              <button className="echo-button echo-button--ghost echo-button--small echo-button--icon-only" disabled={busy} onClick={() => setModelEditor({ provider: selectedProvider, model })} aria-label={`编辑模型 ${modelRemoteId(model)}`}><Pencil size={13} /></button>
                              <button className="echo-button echo-button--ghost echo-button--small echo-button--icon-only echo-button--danger" disabled={busy} onClick={() => void handleDeleteModel(model)} aria-label={`删除模型 ${modelRemoteId(model)}`}><Trash2 size={13} /></button>
                            </span>
                          )}
                        </div>
                        {testResult && testResult.status !== "testing" && <div role="status" className={`model-connections__test-result model-connections__test-result--${testResult.status}`}>
                          <div>{testResult.status === "success" ? <><CheckCircle2 size={14} aria-hidden="true" /><span>测试通过 · 耗时 {modelTestDuration(testResult.durationMs)}</span></>
                            : <><CircleAlert size={14} aria-hidden="true" /><span>上次测试失败</span></>}
                            <time dateTime={new Date(testResult.completedAt).toISOString()} title={new Date(testResult.completedAt).toLocaleString("zh-CN")}>
                              测试于 {new Date(testResult.completedAt).toLocaleTimeString("zh-CN", { hour: "2-digit", minute: "2-digit" })}
                            </time>
                          </div>
                          {testResult.error && <p>{testResult.error}</p>}
                        </div>}
                      </li>
                    );
                  })}
                </ul>
              )}
              <SettingsDisclosure title="连接信息" description="服务地址与高级配置">
              <dl className="model-connections__facts">
                <div><dt>接口协议</dt><dd>{protocolLabel(selectedProvider)}</dd></div>
                <div><dt>API Key</dt><dd>{selectedProvider.managed ? "由组织安全配置 · 不可查看" : selectedProvider.credentialConfigured ? "已保存在本机" : "未配置"}</dd></div>
                <div><dt>Base URL</dt><dd title={selectedProvider.baseUrl}>{selectedProvider.baseUrl || "—"}{selectedProvider.managed && <span> · 只读</span>}</dd></div>
                <div><dt>上下文窗口</dt><dd>{selectedProvider.contextWindow ? `${selectedProvider.contextWindow.toLocaleString()} tokens` : selectedModels.some((model) => model.contextWindow) ? "按模型配置" : "使用模型默认值"}</dd></div>
              </dl>
                <p className="settings-hint">自动选择优先级：已设默认、组织模型、个人模型。仅会使用当前可用的模型。</p>
              </SettingsDisclosure>
            </section>
      </>}
      {view === "detail" && !selectedProvider && (busy ? <p className="model-settings__hint" role="status">正在加载连接…</p> : <Button onClick={() => setView("connections")}>查看其他连接</Button>)}
    </>}
      {connectionEditor && (
        <ConnectionEditor
          original={connectionEditor === "new" ? undefined : connectionEditor}
          existingModels={connectionEditor === "new" ? [] : catalog.models.filter((model) => model.providerId === connectionEditor.id)}
          runMutation={runMutation}
          onCancel={() => setConnectionEditor(null)}
          onSaved={async (providerId, count) => {
            setConnectionEditor(null);
            openDetail(providerId);
            await finishMutation(`${connectionEditor === "new" ? "连接已添加" : "连接已更新"}${count ? `，已配置 ${count} 个模型` : ""}。`);
          }}
        />
      )}

      {modelEditor && (
        <ManualModelEditor
          provider={modelEditor.provider}
          original={modelEditor.model}
          runMutation={runMutation}
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
          runMutation={runMutation}
          onCancel={() => setImportProvider(null)}
          onSaved={async count => {
            setImportProvider(null);
            await finishMutation(`已添加 ${count} 个模型。`);
          }}
        />
      )}

    {dialog}
  </div>;
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
  runMutation,
  onCancel,
  onSaved,
}: {
  original?: ModelProviderEntry;
  existingModels: ModelEntry[];
  runMutation: ModelMutation;
  onCancel: () => void;
  onSaved: (providerId: string, modelCount: number) => Promise<void>;
}) {
  const initialKind = original ? serviceKind(original) : "custom";
  const [serviceChosen, setServiceChosen] = useState(Boolean(original));
  const initialPreset = PROVIDER_PRESETS[initialKind];
  const [draft, setDraft] = useState<ConnectionDraft>({
    label: original?.label ?? (original ? connectionName(original) : ""),
    service: initialKind,
    providerKind: original?.providerKind ?? "custom",
    baseUrl: original?.baseUrl ?? initialPreset.baseUrl,
    allowInsecureHttp: original?.allowInsecureHttp ?? false,
    apiKey: "",
    apiBackend: original?.apiBackend ?? initialPreset.apiBackend,
    authScheme: original?.authScheme ?? initialPreset.authScheme,
    contextWindow: original?.contextWindow ? String(original.contextWindow) : "",
  });
  const [showKey, setShowKey] = useState(false);
  const [editingAddress, setEditingAddress] = useState(false);
  const [showAdvanced, setShowAdvanced] = useState(false);
  const [discovering, setDiscovering] = useState(false);
  const [saving, setSaving] = useState(false);
  const [discovered, setDiscovered] = useState<FetchedModel[]>([]);
  const modelIds = useMemo(() => existingModels.map(modelRemoteId), [existingModels]);
  const [search, setSearch] = useState("");
  const [modelInput, setModelInput] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [invalidField, setInvalidField] = useState<string>();
  const [discoveryMessage, setDiscoveryMessage] = useState<PanelMessage | null>(null);
  const preset = PROVIDER_PRESETS[draft.service];
  const initialDraft = useRef(JSON.stringify(draft));
  const operationRef = useRef(false);
  const { requestClose, closeDialog } = useUnsavedClose({
    dirty: JSON.stringify(draft) !== initialDraft.current || Boolean(modelInput),
    busy: saving || discovering,
    onClose: onCancel,
  });
  const cancel = () => { if (!operationRef.current) requestClose(); };
  const dialogRef = useModalFocus<HTMLDivElement>(true, cancel);

  const handleKindChange = (service: ServiceKind) => {
    const next = PROVIDER_PRESETS[service];
    setServiceChosen(true); setError(null);
    setDraft((current) => ({
      ...current,
      service,
      providerKind: service === "kimi" || service === "zhipu" ? "custom" : service,
      label: !current.label || current.label === PROVIDER_PRESETS[current.service].shortLabel ? next.shortLabel : current.label,
      baseUrl: next.baseUrl,
      allowInsecureHttp: false,
      apiBackend: next.apiBackend,
      authScheme: next.authScheme,
    }));
  };

  const validate = (): string | null => {
    setInvalidField(undefined);
    const fail = (field: string, text: string) => { setInvalidField(field); if (field === "model-context-window") setShowAdvanced(true); return text; };
    if (!serviceChosen) return fail("model-provider-kind", "请选择模型服务商。");
    const baseUrlError = connectionBaseUrlError(draft.baseUrl, draft.allowInsecureHttp);
    if (baseUrlError) return fail("model-base-url", baseUrlError);
    if (!original && !draft.apiKey.trim()) return fail("model-api-key", "请填写 API 密钥。");
    if (draft.contextWindow && (!Number.isSafeInteger(Number(draft.contextWindow)) || Number(draft.contextWindow) <= 0)) return fail("model-context-window", "上下文窗口必须是大于 0 的整数。");
    return null;
  };

  const allModelIds = () => modelIds.length > 0 ? modelIds : modelInput.trim() ? [modelInput.trim()] : [];
  const requireModelIds = (): string[] | null => {
    const ids = allModelIds();
    if (ids.length === 0) { setInvalidField("model-id-input"); setError("请填写模型 ID。"); return null; }
    return ids;
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
    if (operationRef.current) return;
    const validation = validate();
    if (validation) { setDiscoveryMessage(null); setError(validation); return; }
    operationRef.current = true;
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
        ? { kind: "ok", text: `获取到 ${models.length} 个模型，选择需要使用的模型。` }
        : { kind: "warn", text: "服务端没有返回模型列表，请直接填写 Model ID。" });
    } catch (requestError) {
      setDiscovered(existingModels.map((model) => ({
        id: modelRemoteId(model),
        ownedBy: "本地已配置",
      })));
      setDiscoveryMessage({
        kind: "warn",
        text: `无法获取模型列表：${String(requestError)}。可手动填写模型 ID 后保存。`,
      });
    } finally {
      operationRef.current = false;
      setDiscovering(false);
    }
  };

  const handleSave = async () => {
    if (operationRef.current) return;
    const validation = validate();
    if (validation) { setError(validation); return; }
    const ids = requireModelIds();
    if (!ids) return;
    operationRef.current = true;
    setSaving(true);
    setError(null);
    try {
      await runMutation(async () => {
        const result = await persist(ids);
        await onSaved(result.providerId, result.modelIds.length);
      });
    } catch (saveError) {
      setError(String(saveError));
    } finally {
      operationRef.current = false;
      setSaving(false);
    }
  };

  const connectionSignature = JSON.stringify([draft.service, draft.baseUrl, draft.apiKey, draft.apiBackend, draft.authScheme]);
  useEffect(() => { setDiscovered([]); setDiscoveryMessage(null); }, [connectionSignature]);
  const customAddress = draft.service === "custom" || draft.service === "custom_anthropic";
  const showAddress = serviceChosen && (customAddress || editingAddress || draft.baseUrl !== preset.baseUrl || draft.baseUrl.trim().startsWith("http://"));
  const disabled = saving || discovering;
  const openKeyHelp = async () => {
    if (!preset.keyUrl) return;
    try { await openExternalUrl(preset.keyUrl); }
    catch { setInvalidField(undefined); setError("暂时无法打开服务商页面，请稍后重试。"); }
  };
  const visibleModels = discovered.filter(model => !search.trim() || model.id.toLowerCase().includes(search.trim().toLowerCase()));
  const fieldError = (id: string) => error && invalidField === id && <span id={`${id}-error`} className="model-connection-editor__field-error" role="alert">{error}</span>;
  const fieldA11y = (id: string) => ({ "aria-invalid": Boolean(error && invalidField === id), "aria-describedby": error && invalidField === id ? `${id}-error` : undefined });
  useEffect(() => { if (error && invalidField) dialogRef.current?.querySelector<HTMLElement>(`#${invalidField}`)?.focus(); }, [error, invalidField, dialogRef]);

  return <div ref={dialogRef} className="models-settings-panel__editor-overlay" role="dialog" aria-modal="true" aria-label={original ? "编辑连接" : "添加模型连接"} tabIndex={-1}>
    <div className="model-connection-editor model-connection-editor--simple">
      <header className="models-settings-panel__editor-header"><div className="models-settings-panel__editor-title">{original ? "编辑连接" : "添加模型连接"}</div><Button variant="ghost" size="compact" onClick={cancel} disabled={disabled} aria-label="关闭"><X size={16} /></Button></header>
      <div className="model-connection-editor__body">
        <fieldset className="model-connection-editor__fields" disabled={disabled}>
          <div className="models-settings-panel__field"><label className="models-settings-panel__label" htmlFor="model-provider-kind">模型服务商</label><select id="model-provider-kind" {...fieldA11y("model-provider-kind")} className="form-control models-settings-panel__select" value={serviceChosen ? draft.service : ""} onChange={event => { setEditingAddress(false); handleKindChange(event.target.value as ServiceKind); }} data-modal-initial-focus><option value="" disabled>请选择模型服务商</option>{SERVICE_GROUPS.map(group => <optgroup key={group.label} label={group.label}>{group.kinds.map(kind => <option key={kind} value={kind}>{PROVIDER_PRESETS[kind].label}</option>)}</optgroup>)}</select>{fieldError("model-provider-kind")}</div>
          {showAddress ? <div className="models-settings-panel__field"><label className="models-settings-panel__label" htmlFor="model-base-url">服务地址（Base URL）</label><input id="model-base-url" {...fieldA11y("model-base-url")} className="form-control models-settings-panel__input" value={draft.baseUrl} onChange={event => { setError(null); setDiscoveryMessage(null); setDraft(current => ({ ...current, baseUrl: event.target.value, allowInsecureHttp: event.target.value.trim() === current.baseUrl.trim() && current.allowInsecureHttp })); }} placeholder="填写服务提供方给出的 API 地址" inputMode="url" />{fieldError("model-base-url")}
            {draft.baseUrl.trim().toLowerCase().startsWith("http://") && <label className="model-connection-editor__help model-connection-editor__http-consent"><input type="checkbox" checked={draft.allowInsecureHttp} onChange={event => setDraft(current => ({ ...current, allowInsecureHttp: event.target.checked }))} />允许此个人连接使用 HTTP。我了解 API Key、提问和模型回复会以明文传输。</label>}
          </div> : serviceChosen && <div className="model-connection-editor__address"><span>服务地址：{draft.baseUrl}</span><Button variant="ghost" size="compact" onClick={() => setEditingAddress(true)}>修改地址</Button></div>}
          <div className="models-settings-panel__field"><div className="model-connection-editor__model-heading"><label className="models-settings-panel__label" htmlFor="model-api-key">API 密钥</label>{serviceChosen && preset.keyUrl && <Button variant="ghost" size="compact" onClick={() => void openKeyHelp()}>获取密钥<ChevronRight size={12} /></Button>}</div><div className="models-settings-panel__input-shell"><input id="model-api-key" {...fieldA11y("model-api-key")} className="form-control models-settings-panel__input models-settings-panel__input--with-trailing-icon" type={showKey ? "text" : "password"} value={draft.apiKey} onChange={event => { setError(null); setDraft(current => ({ ...current, apiKey: event.target.value })); }} placeholder={original?.credentialConfigured ? "已保存，留空不更换" : preset.placeholderKey} autoComplete="off" /><button className="echo-button echo-button--ghost echo-button--small echo-button--icon-only models-settings-panel__input-toggle" onClick={() => setShowKey(value => !value)} type="button" aria-label={showKey ? "隐藏 API 密钥" : "显示 API 密钥"}>{showKey ? <EyeOff size={14} /> : <Eye size={14} />}</button></div>{fieldError("model-api-key")}<span className="model-connection-editor__help">{original?.credentialConfigured ? "留空将保留本机已保存的密钥。" : "密钥保存在本机，用于连接所选模型服务。"}</span></div>
          <div className="models-settings-panel__field">
            <div className="model-connection-editor__model-heading"><label className="models-settings-panel__label" htmlFor={modelIds.length ? undefined : "model-id-input"}>{modelIds.length ? "已配置模型" : "模型 ID"}</label>{modelIds.length === 0 && <Button variant="ghost" size="compact" onClick={() => void handleDiscover()} disabled={disabled}>{discovering ? "正在获取…" : "从服务获取模型"}</Button>}</div>
            {modelIds.length ? <><div className="model-connection-editor__chips" aria-label="已配置模型">{modelIds.map(id => <span key={id}><code>{id}</code></span>)}</div><span className="model-connection-editor__help">可在连接详情中添加、编辑和删除模型。</span></> : <input id="model-id-input" {...fieldA11y("model-id-input")} className="form-control models-settings-panel__input" value={modelInput} onChange={event => { setModelInput(event.target.value); setError(null); }} onKeyDown={event => { if (event.key === "Enter" && !event.nativeEvent.isComposing && event.keyCode !== 229) event.preventDefault(); }} placeholder="填写服务商提供的模型 ID" autoComplete="off" />}{fieldError("model-id-input")}
          </div>
          {discoveryMessage && <p className={`model-connections__message model-connections__message--${discoveryMessage.kind}`} role="status">{discoveryMessage.text}</p>}
          {discovered.length > 0 && modelIds.length === 0 && <div className="model-connection-editor__discovered">{discovered.length > 8 && <input className="form-control models-settings-panel__input" type="search" value={search} onChange={event => setSearch(event.target.value)} aria-label="筛选已获取的模型" placeholder="查找模型" />}<div className="model-connection-editor__model-picker" role="group" aria-label="从服务选择模型">{visibleModels.map(model => <label key={model.id}><input type="radio" name="discovered-model" checked={modelInput.trim() === model.id} onChange={() => { setModelInput(model.id); setError(null); }} /><span><strong>{model.id}</strong></span></label>)}</div>{visibleModels.length === 0 && <p className="model-settings__hint">没有找到匹配的模型，可直接填写模型 ID。</p>}</div>}
          <Button className="models-settings-panel__advanced-toggle" variant="ghost" size="compact" onClick={() => setShowAdvanced(value => !value)} aria-expanded={showAdvanced}><ChevronDown size={14} className={showAdvanced ? "model-connection-editor__chevron--open" : ""} />更多设置</Button>
          {showAdvanced && (
            <div className="models-settings-panel__advanced">
              <div className="models-settings-panel__field"><label className="models-settings-panel__label" htmlFor="model-connection-label">连接名称</label><input id="model-connection-label" className="form-control models-settings-panel__input" value={draft.label} onChange={event => setDraft(current => ({ ...current, label: event.target.value }))} placeholder={preset.shortLabel} /></div>
              <div className="models-settings-panel__field-row">
                <div className="models-settings-panel__field"><label className="models-settings-panel__label" htmlFor="model-api-backend">请求协议</label><select id="model-api-backend" className="form-control models-settings-panel__select" value={draft.apiBackend} onChange={(event) => setDraft((current) => ({ ...current, apiBackend: event.target.value as ApiBackend }))}><option value="chat_completions">Chat Completions</option><option value="responses">Responses</option><option value="messages">Messages</option></select></div>
                <div className="models-settings-panel__field"><label className="models-settings-panel__label" htmlFor="model-auth-scheme">认证方式</label><select id="model-auth-scheme" className="form-control models-settings-panel__select" value={draft.authScheme} onChange={(event) => setDraft((current) => ({ ...current, authScheme: event.target.value as AuthScheme }))}><option value="bearer">Bearer Token</option><option value="x_api_key">X-API-Key</option></select></div>
              </div>
              <div className="models-settings-panel__field"><label className="models-settings-panel__label" htmlFor="model-context-window">默认上下文窗口（可选）</label><input id="model-context-window" {...fieldA11y("model-context-window")} className="form-control models-settings-panel__input" type="number" min={1} value={draft.contextWindow} onChange={(event) => setDraft((current) => ({ ...current, contextWindow: event.target.value }))} placeholder="例如 128000" />{fieldError("model-context-window")}</div>
            </div>

          )}
          {error && !invalidField && <div className="models-settings-panel__editor-error" role="alert">{error}</div>}
        </fieldset>
      </div>
      <footer className="models-settings-panel__editor-footer"><Button onClick={cancel} disabled={disabled}>取消</Button><Button variant="primary" onClick={() => void handleSave()} disabled={disabled}>{saving ? "保存中…" : "保存"}</Button></footer>
    </div>
    {closeDialog}
  </div>;
}

function ManualModelEditor({ provider, original, runMutation, onCancel, onSaved }: { provider: ModelProviderEntry; original?: ModelEntry; runMutation: ModelMutation; onCancel: () => void; onSaved: () => Promise<void> }) {
  const [remoteModelId, setRemoteModelId] = useState(original ? modelRemoteId(original) : "");
  const [name, setName] = useState(original?.name ?? "");
  const [contextWindow, setContextWindow] = useState(original?.contextWindow ? String(original.contextWindow) : "");
  const [maxOutputTokens, setMaxOutputTokens] = useState(original?.maxOutputTokens ? String(original.maxOutputTokens) : "");
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const initialDraft = useRef(JSON.stringify({ remoteModelId, name, contextWindow, maxOutputTokens }));
  const savingRef = useRef(false);
  const { requestClose, closeDialog } = useUnsavedClose({
    dirty: JSON.stringify({ remoteModelId, name, contextWindow, maxOutputTokens }) !== initialDraft.current,
    busy: saving,
    onClose: onCancel,
  });
  const cancel = () => { if (!savingRef.current) requestClose(); };
  const dialogRef = useModalFocus<HTMLDivElement>(true, cancel);

  const save = async () => {
    if (savingRef.current) return;
    if (!remoteModelId.trim()) { setError("请填写远端模型 ID。"); return; }
    const parsedContext = contextWindow.trim() ? Number(contextWindow) : undefined;
    const parsedMaxOutput = maxOutputTokens.trim() ? Number(maxOutputTokens) : undefined;
    if (parsedContext !== undefined && (!Number.isSafeInteger(parsedContext) || parsedContext <= 0)) { setError("上下文窗口必须是大于 0 的整数。"); return; }
    if (parsedMaxOutput !== undefined && (!Number.isInteger(parsedMaxOutput) || parsedMaxOutput <= 0 || parsedMaxOutput > 0xffffffff)) { setError("最大输出必须是大于 0 的整数，且不超过 4,294,967,295。"); return; }
    if (parsedMaxOutput !== undefined && parsedMaxOutput > (parsedContext ?? provider.contextWindow ?? Infinity)) { setError("最大输出不能超过上下文窗口。"); return; }
    savingRef.current = true;
    setSaving(true);
    try {
      await runMutation(async () => {
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
      });
    } catch (saveError) {
      setError(String(saveError));
    } finally {
      savingRef.current = false;
      setSaving(false);
    }
  };

  return (
    <div ref={dialogRef} className="models-settings-panel__editor-overlay" role="dialog" aria-modal="true" aria-label={original ? "编辑模型" : "手动添加模型"} tabIndex={-1}>
      <div className="models-settings-panel__editor models-settings-panel__editor--narrow">
        <header className="models-settings-panel__editor-header"><div><div className="models-settings-panel__editor-title">{original ? "编辑模型" : "手动添加模型"}</div><div className="models-settings-panel__editor-note">连接：{connectionName(provider)}</div></div><button className="echo-button echo-button--ghost echo-button--small echo-button--icon-only" onClick={cancel} disabled={saving} aria-label="关闭"><X size={14} /></button></header>
        <div className="models-settings-panel__editor-body">
        <fieldset className="models-settings-panel__editor-fields" disabled={saving}>
          <div className="models-settings-panel__field"><label className="models-settings-panel__label" htmlFor="manual-model-id">远端模型 ID</label><input id="manual-model-id" className="form-control models-settings-panel__input" value={remoteModelId} onChange={(event) => setRemoteModelId(event.target.value)} placeholder="例如 MiniMax-M3、gpt-5" data-modal-initial-focus /><span className="model-connection-editor__help">必须与 API 请求中使用的 model 值完全一致。</span></div>
          <div className="models-settings-panel__field"><label className="models-settings-panel__label" htmlFor="manual-model-name">显示名称（可选）</label><input id="manual-model-name" className="form-control models-settings-panel__input" value={name} onChange={(event) => setName(event.target.value)} placeholder="例如 工作用 MiniMax" /></div>
          <div className="models-settings-panel__field"><label className="models-settings-panel__label" htmlFor="manual-model-context-window">上下文窗口（可选）</label><input id="manual-model-context-window" className="form-control models-settings-panel__input" type="number" min={1} value={contextWindow} onChange={(event) => setContextWindow(event.target.value)} placeholder="留空使用连接默认值" /></div>
          <div className="models-settings-panel__field"><label className="models-settings-panel__label" htmlFor="manual-model-max-output">最大输出 tokens（可选）</label><input id="manual-model-max-output" className="form-control models-settings-panel__input" type="number" min={1} step={1} max={4294967295} value={maxOutputTokens} onChange={(event) => setMaxOutputTokens(event.target.value)} placeholder="留空使用模型默认值" /></div>
          {error && <div className="models-settings-panel__editor-error">{error}</div>}
        </fieldset>
        </div>
        <footer className="models-settings-panel__editor-footer"><button className="echo-button echo-button--secondary echo-button--medium" onClick={cancel} disabled={saving}>取消</button><button className="echo-button echo-button--primary echo-button--medium" onClick={() => void save()} disabled={saving}>{saving ? "保存中…" : "保存模型"}</button></footer>
      </div>
      {closeDialog}
    </div>
  );
}

function ModelImportDialog({ provider, existingModels, runMutation, onCancel, onSaved }: { provider: ModelProviderEntry; existingModels: ModelEntry[]; runMutation: ModelMutation; onCancel: () => void; onSaved: (count: number) => Promise<void> }) {
  const [models, setModels] = useState<FetchedModel[]>([]);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [saveError, setSaveError] = useState<string | null>(null);
  const loadGenerationRef = useRef(0);
  const loadingRef = useRef(false);
  const savingRef = useRef(false);
  const { requestClose, closeDialog } = useUnsavedClose({ dirty: selected.size > 0, busy: saving, onClose: onCancel });
  const cancel = () => { if (!savingRef.current) requestClose(); };
  const dialogRef = useModalFocus<HTMLDivElement>(true, cancel);
  const existing = useMemo(() => new Set(existingModels.map(modelRemoteId)), [existingModels]);

  const loadModels = useCallback(async () => {
    if (loadingRef.current) return;
    loadingRef.current = true;
    const generation = ++loadGenerationRef.current;
    setLoading(true);
    setLoadError(null);
    try {
      const fetched = await providersFetchModelsForProvider(provider.id);
      if (loadGenerationRef.current === generation) setModels(fetched);
    } catch (requestError) {
      if (loadGenerationRef.current === generation) {
        setLoadError(`读取模型列表失败：${String(requestError).replace(/^Error:\s*/, "")}`);
      }
    } finally {
      if (loadGenerationRef.current === generation) {
        loadingRef.current = false;
        setLoading(false);
      }
    }
  }, [provider.id]);

  useEffect(() => {
    setModels([]);
    setSelected(new Set());
    setSaveError(null);
    void loadModels();
    return () => {
      loadGenerationRef.current += 1;
      loadingRef.current = false;
    };
  }, [loadModels]);

  const save = async () => {
    if (savingRef.current || loadingRef.current || loadError || selected.size === 0) return;
    savingRef.current = true;
    setSaving(true);
    setSaveError(null);
    try {
      await runMutation(async () => {
        await providersSaveConnection(
          { ...provider, apiKey: undefined },
          [...selected].map((remoteModelId) => ({
            modelId: "",
            remoteModelId,
            providerId: provider.id,
          })),
        );
        await onSaved(selected.size);
      });
    } catch (saveError) {
      setSaveError(`保存模型失败：${String(saveError).replace(/^Error:\s*/, "")}`);
    } finally {
      savingRef.current = false;
      setSaving(false);
    }
  };

  return (
    <div ref={dialogRef} className="models-settings-panel__editor-overlay" role="dialog" aria-modal="true" aria-label="添加更多模型" tabIndex={-1}>
      <div className="models-settings-panel__editor">
        <header className="models-settings-panel__editor-header"><div><div className="models-settings-panel__editor-title">添加更多模型</div><div className="models-settings-panel__editor-note">直接使用「{connectionName(provider)}」中已保存的密钥</div></div><button className="echo-button echo-button--ghost echo-button--small echo-button--icon-only" onClick={cancel} disabled={saving} aria-label="关闭" data-modal-initial-focus><X size={14} /></button></header>
        <div className="models-settings-panel__editor-body">
          {loading ? <div className="model-connections__empty" role="status"><Loader2 className="models-settings-panel__spin" size={18} />正在获取模型列表…</div> : loadError ? (
            <div className="models-settings-panel__editor-error" role="alert">
              <p>{loadError}</p>
              <button type="button" className="echo-button echo-button--secondary echo-button--small" onClick={() => void loadModels()}>重试获取模型</button>
            </div>
          ) : (
            <div className="model-connection-editor__model-picker">
              {models.map((model) => {
                const configured = existing.has(model.id);
                return <label key={model.id} className={configured ? "disabled" : ""}><input type="checkbox" checked={configured || selected.has(model.id)} disabled={configured || saving} onChange={() => { setSaveError(null); setSelected((current) => { const next = new Set(current); if (next.has(model.id)) next.delete(model.id); else next.add(model.id); return next; }); }} /><span><strong>{model.id}</strong>{model.ownedBy && <small>{model.ownedBy}</small>}</span>{configured && <em>已配置</em>}</label>;
              })}
              {models.length === 0 && <div className="model-connections__models-empty">服务端没有返回模型，请关闭后使用“手动添加”。</div>}
            </div>
          )}
          {saveError && <div className="models-settings-panel__editor-error" role="alert">{saveError}</div>}
        </div>
        <footer className="models-settings-panel__editor-footer"><button className="echo-button echo-button--secondary echo-button--medium" onClick={cancel} disabled={saving}>取消</button><button className="echo-button echo-button--primary echo-button--medium" onClick={() => void save()} disabled={loading || Boolean(loadError) || saving || selected.size === 0}>{saving ? "保存中…" : `添加 ${selected.size || ""} 个模型`}</button></footer>
      </div>
      {closeDialog}
    </div>
  );
}
