import { withoutRemovedModels } from "@/lib/retired-models";
import { PageHeading } from "../PageHeading";
import { useModelConnectionTests } from "@/lib/use-model-connection-tests";
import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { Plus, RefreshCw, Search, X } from "lucide-react";
import {
  agentAuthStatus,
  agentsDefaultsGet,
  agentsDefaultsSave,
  internalReload,
  providersDeleteModel,
  providersDeleteProvider,
  providersList,
  type ModelEntry,
  type ModelProviderEntry,
  type ProviderListModel,
} from "@/lib/agent-client";
import { listenOrgModelsChanged, orgSyncModelConfig } from "@/lib/org-client";
import { useOrgSessionStore } from "@/stores/org-session-store";
import { useAppDialog } from "../AppDialog";
import { useSettingsDraft } from "@/lib/settings-drafts";
import { Button, IconButton } from "../Button";
import { ConnectionEditor } from "./ConnectionEditor";
import { ProviderCard } from "./ProviderCard";
import {
  connectionName,
  connectionOrigin,
  modelName,
  modelRemoteId,
  providerDisplayName,
  type ModelMutation,
  type PanelMessage,
} from "./provider-presets";

interface ModelConnectionsPanelProps {
  onModelsChanged?: () => void | Promise<void>;
  initialModelId?: string;
  onOpenOrganization?: () => void;
  resetRequest?: number;
}

async function reloadRuntime(): Promise<string | null> {
  try {
    await internalReload("models");
    return null;
  } catch (error) {
    return String(error).replace(/^Error:\s*/, "");
  }
}

export function ModelConnectionsPanel({ onModelsChanged, initialModelId, onOpenOrganization, resetRequest = 0 }: ModelConnectionsPanelProps) {
  const [catalog, setCatalog] = useState<ProviderListModel>({ providers: [], models: [] });
  const [loading, setLoading] = useState(true);
  const [message, setMessage] = useState<PanelMessage | null>(null);
  const [addingModel, setAddingModel] = useState(false);
  const [modelEditor, setModelEditor] = useState<{ provider: ModelProviderEntry; model: ModelEntry } | null>(null);
  const [query, setQuery] = useState("");
  const [syncingOrganization, setSyncingOrganization] = useState(false);
  const { results: modelTests, test: testModel } = useModelConnectionTests(catalog);
  const modelRowsRef = useRef(new Map<string, HTMLLIElement>());
  const reloadGenerationRef = useRef(0);
  const pendingModelFocus = useRef(initialModelId);
  const [loadError, setLoadError] = useState("");
  const [readiness, setReadiness] = useState<{ ready: boolean; reason?: string } | null>(null);
  const [defaultModel, setDefaultModel] = useState<{ saved: string; effective?: string } | null>(null);
  const [busy, setBusy] = useState(false);
  const operationRef = useRef(false);
  const [orgSyncState, setOrgSyncState] = useState<"unknown" | "empty" | "saved" | "error">("unknown");
  const organizationSession = useOrgSessionStore((state) => state.session);
  const organizationHydrated = useOrgSessionStore((state) => state.hydrated);
  const organizationBoundary = useOrgSessionStore((state) => state.boundaryRevision);
  const currentOrganizationBoundary = useRef(organizationBoundary); currentOrganizationBoundary.current = organizationBoundary;
  const hydrateOrganization = useOrgSessionStore((state) => state.hydrate);
  const { requestConfirmation, dialog } = useAppDialog();
  useSettingsDraft({ dirty: false, busy: busy || syncingOrganization, label: "模型设置" });
  useEffect(() => {
    if (message?.kind !== "ok") return;
    const dismiss = window.setTimeout(() => setMessage(null), 4500);
    return () => window.clearTimeout(dismiss);
  }, [message]);

  const previousReset = useRef(resetRequest);
  useEffect(() => {
    if (previousReset.current === resetRequest) return;
    previousReset.current = resetRequest;
    setAddingModel(false);
    setModelEditor(null);
    setQuery("");
  }, [resetRequest]);
  useLayoutEffect(() => { pendingModelFocus.current = initialModelId; }, [initialModelId]);
  useLayoutEffect(() => {
    if (loading || addingModel || modelEditor || !pendingModelFocus.current) return;
    const row = modelRowsRef.current.get(pendingModelFocus.current);
    if (!row && catalog.models.some(model => model.modelId === pendingModelFocus.current)) {
      setQuery("");
    }
    if (!row) return;
    row.scrollIntoView?.({ block: "nearest" });
    row.focus({ preventScroll: true });
    pendingModelFocus.current = undefined;
  }, [catalog, addingModel, initialModelId, loading, modelEditor, query]);

  const reload = useCallback(async (quiet = false): Promise<string | null> => {
    if (!quiet) setLoading(true);
    const generation = ++reloadGenerationRef.current;
    try {
      const auth = await agentAuthStatus();
      const [rawCatalog, defaults] = await Promise.all([providersList(), agentsDefaultsGet().catch(() => null)]);
      if (generation !== reloadGenerationRef.current) return null;
      const nextCatalog = withoutRemovedModels(rawCatalog);
      setCatalog(nextCatalog);
      setDefaultModel(defaults ? { saved: defaults.defaultModel.trim(), effective: auth.defaultModelId } : null);
      setLoadError("");
      setReadiness({ ready: auth.ready, reason: auth.reason });
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
    setOrgSyncState("unknown"); setCatalog({ providers: [], models: [] }); setReadiness(null); setDefaultModel(null);
    setQuery("");
    pendingModelFocus.current = initialModelId;
    void reload();
  }, [organizationBoundary, reload, initialModelId]);
  const organizationProviders = useMemo(
    () => catalog.providers.filter((provider) => provider.source === "organization"),
    [catalog.providers],
  );

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

  const handleDeleteModel = (model: ModelEntry) => {
    if (model.managed || catalog.providers.find(value => value.id === model.providerId)?.source === "organization") return;
    const label = modelName(model);
    requestConfirmation({
      title: `删除模型“${label}”？`,
      description: defaultModel?.saved === model.modelId
        ? "删除后应用将自动选择默认模型。已引用此模型的任务可能需要重新选模。"
        : "该模型将从当前连接中移除，且不再可用于新对话。",
      confirmLabel: "删除模型",
      danger: true,
      action: () => runMutation(async () => {
        await providersDeleteModel(model.modelId);
        await finishMutation("模型已删除。");
      }),
      onError: (error) => setMessage({ kind: "err", text: `删除失败：${String(error)}` }),
    });
  };

  const changeDefault = async (model?: ModelEntry) => {
    try {
      await runMutation(async () => {
        const latest = await agentsDefaultsGet();
        await agentsDefaultsSave({ ...latest, defaultModel: model?.modelId ?? "" });
        await finishMutation(model ? `已将「${modelName(model)}」设为默认模型。` : "已恢复自动选择默认模型。");
      });
    } catch (error) {
      setMessage({ kind: "err", text: `更新默认模型失败：${String(error).replace(/^Error:\s*/, "")}` });
    }
  };

  const deleteEmptyConnection = (provider: ModelProviderEntry) => {
    requestConfirmation({
      title: `删除连接「${connectionName(provider)}」？`,
      description: "将移除此连接和保存在此连接中的 API Key。",
      confirmLabel: "删除连接",
      danger: true,
      action: () => runMutation(async () => {
        await providersDeleteProvider(provider.id, true);
        await finishMutation("连接已删除。");
      }),
      onError: error => setMessage({ kind: "err", text: `删除连接失败：${String(error).replace(/^Error:\s*/, "")}` }),
    });
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
  const organizationCopy = organizationExpired ? "组织登录已过期，需要重新登录"
    : orgSyncState === "error" ? "暂时无法获取组织模型" : organizationProviders.length > 0 ? "由组织统一配置，只读"
    : orgSyncState === "empty" ? "组织暂未提供模型" : "尚未获取组织模型，刷新后确认";
  const personalProviders = catalog.providers.filter(provider => provider.source !== "organization");
  const modelsByService = useMemo(() => {
    const groups = new Map<string, ModelEntry[]>();
    for (const model of catalog.models) {
      const models = groups.get(model.providerId);
      if (models) models.push(model);
      else groups.set(model.providerId, [model]);
    }
    return groups;
  }, [catalog.models]);
  const emptyProviders = personalProviders.filter(provider => !provider.managed && !modelsByService.has(provider.id));
  const savedDefault = catalog.models.find(model => model.modelId === defaultModel?.saved);
  const effectiveDefault = catalog.models.find(model => model.modelId === defaultModel?.effective);
  const defaultCopy = defaultModel?.saved
    ? `已设默认 · ${savedDefault ? modelName(savedDefault) : defaultModel.saved}${!savedDefault || (defaultModel.effective && defaultModel.effective !== defaultModel.saved) ? "（当前不可用）" : ""}`
    : effectiveDefault ? `自动选择 · ${modelName(effectiveDefault)}` : "自动选择";
  const reusableProviders = personalProviders.filter(provider => !provider.managed && provider.credentialConfigured !== false);
  const organizationAction = organizationSession?.loggedIn && !organizationExpired
    ? <Button size="compact" disabled={busy || syncingOrganization} onClick={() => void handleOrganizationSync()}><RefreshCw size={14} />{syncingOrganization ? "刷新中…" : "刷新组织模型"}</Button>
    : onOpenOrganization ? <Button size="compact" disabled={busy} onClick={onOpenOrganization}>前往组织登录</Button> : <span>请在“组织”页面重新登录</span>;
  const registerRow = (modelId: string, element: HTMLLIElement | null) => {
    if (element) modelRowsRef.current.set(modelId, element);
    else modelRowsRef.current.delete(modelId);
  };
  const showSearch = catalog.models.length >= 6;
  const search = showSearch ? query.trim().toLowerCase() : "";
  const visibleGroups = [...personalProviders, ...organizationProviders].flatMap(provider => {
    const models = modelsByService.get(provider.id) ?? [];
    if (!models.length) return [];
    const serviceMatches = [providerDisplayName(provider), provider.providerKind, connectionOrigin(provider.baseUrl)]
      .some(value => value?.toLowerCase().includes(search));
    if (serviceMatches) return [{ provider, models }];
    const matches = models.filter(model => [modelName(model), modelRemoteId(model)].some(value => value.toLowerCase().includes(search)));
    return matches.length ? [{ provider, models: matches }] : [];
  });
  const renderProvider = ({ provider, models }: typeof visibleGroups[number]) => <ProviderCard key={provider.id} provider={provider}
    models={models} tests={modelTests} busy={busy} organizationExpired={organizationExpired} registerRow={registerRow}
    onTest={model => void testModel(provider, model)} onEditModel={model => setModelEditor({ provider, model })}
    defaultModelId={defaultModel?.saved}
    onSetDefault={defaultModel && provider.credentialConfigured !== false ? model => void changeDefault(model) : undefined}
    onDeleteModel={handleDeleteModel} />;

  return <div className="model-connections" data-setting-id="model-settings">
    {message && <div className={`model-connections__message model-connections__message--${message.kind}`} role={message.kind === "err" ? "alert" : "status"}>{message.text}{message.retry === "load" && <Button size="compact" disabled={busy} onClick={retryRefresh}>重试加载</Button>}{message.retry === "organization" && <Button size="compact" disabled={busy} onClick={() => void handleOrganizationSync()}>重试获取</Button>}</div>}
    <section className="model-catalog" aria-label={!loading && !loadError ? "模型目录" : undefined}>
      <header className="model-connections__header">
        <PageHeading title="模型" level={2} />
        {!loading && !loadError && <Button disabled={busy} onClick={() => setAddingModel(true)}>
          <Plus size={16} aria-hidden="true" />添加模型
        </Button>}
      </header>
      {loading ? <p className="model-settings__hint" role="status">正在读取模型配置…</p>
        : loadError ? <div className="panel-inline-error" role="alert">
          <span>{loadError}</span><Button size="compact" disabled={busy} onClick={() => void reload()}>重新读取</Button>
        </div> : <>
        {catalog.models.length > 0 && <div className="model-catalog__notice" aria-label="应用默认模型">
          {defaultModel ? <span>应用默认模型 · {defaultCopy}</span> : <span role="alert">无法读取应用默认模型。</span>}
          {defaultModel?.saved
            ? <Button size="compact" disabled={busy} onClick={() => void changeDefault()}>恢复自动选择</Button>
            : !defaultModel && <Button size="compact" disabled={busy} onClick={() => void reload(true)}>重试读取</Button>}
        </div>}
        {showSearch && <div className="model-catalog__search">
          <Search size={15} aria-hidden="true" />
          <input className="form-control" type="search" aria-label="搜索模型"
            placeholder="搜索模型名称、ID 或服务"
            value={query} onChange={event => setQuery(event.target.value)} />
          {query && <IconButton label="清除筛选" onClick={() => setQuery("")}><X size={14} aria-hidden="true" /></IconButton>}
        </div>}
        {readiness?.ready === false && catalog.models.length > 0 && <div className="model-catalog__notice model-catalog__notice--error" role="alert">
          <span>模型暂未就绪。{readiness.reason || "请检查服务连接或重试加载。"}</span>
          <Button size="compact" disabled={busy} onClick={retryRefresh}>重试加载</Button>
        </div>}
        {showOrganization && <div className="model-catalog__notice">
          <div className={`model-settings__organization${organizationExpired ? " model-settings__organization--warning" : ""}`}
            role="status" aria-label="组织模型状态">{organizationCopy}</div>
          {organizationAction}
        </div>}
        {visibleGroups.length ? <div className="model-catalog__groups">{visibleGroups.map(renderProvider)}</div>
          : <p className="model-catalog__no-results" role={search ? "status" : undefined}>{search ? "没有匹配的结果" : "还没有可用模型"}</p>}
        {emptyProviders.length > 0 && <details className="model-catalog__empty-connections">
          <summary>无模型的连接（{emptyProviders.length}）</summary>
          <ul>{emptyProviders.map(provider => <li key={provider.id}>
            <span>{connectionName(provider)}{connectionOrigin(provider.baseUrl) && <small>{connectionOrigin(provider.baseUrl)}</small>}</span>
            <Button size="compact" disabled={busy} aria-label={`删除连接 ${connectionName(provider)}`}
              onClick={() => deleteEmptyConnection(provider)}>删除连接</Button>
          </li>)}</ul>
        </details>}
      </>}
    </section>
      {addingModel && (
        <ConnectionEditor
          providerCapabilities={catalog.providerCapabilities}
          reusableProviders={reusableProviders}
          catalogModels={catalog.models}
          runMutation={runMutation}
          onCancel={() => setAddingModel(false)}
          onSaved={async modelIds => {
            pendingModelFocus.current = modelIds[0];
            setQuery("");
            setAddingModel(false);
            await finishMutation("模型已添加。");
          }}
        />
      )}

      {modelEditor && (
        <ConnectionEditor
          original={modelEditor.provider}
          editedModel={modelEditor.model}
          capabilities={catalog.modelCapabilities?.[modelEditor.model.modelId]}
          reusableProviders={reusableProviders}
          catalogModels={catalog.models}
          runMutation={runMutation}
          onCancel={() => setModelEditor(null)}
          onSaved={async (_modelIds, connectionChanged) => {
            pendingModelFocus.current = modelEditor.model.modelId;
            setModelEditor(null);
            await finishMutation(connectionChanged
              ? "模型已更新。已打开的任务可重新选择此模型以应用连接或上下文与输出上限的变更。"
              : "模型已更新。");
          }}
        />
      )}

    {dialog}
  </div>;
}
