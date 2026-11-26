import { Select } from "@/components/Select";
import { useEffect, useMemo, useRef, useState } from "react";
import { ChevronRight, Eye, EyeOff, X } from "lucide-react";
import {
  openExternalUrl,
  providersFetchModelsForProvider,
  providersSaveConnection,
  providersTestConnection,
  providersTestModelConnection,
  type ApiBackend,
  type AuthScheme,
  type FetchedModel,
  type ModelCapabilities,
  type ModelEntry,
  type ModelProviderEntry,
  type ModelReasoningEffort,
} from "@/lib/agent-client";
import { useModalFocus } from "@/lib/use-modal-focus";
import { useUnsavedClose } from "@/lib/use-unsaved-close";
import { Button, IconButton } from "../Button";
import { effortOptionsForProtocol, modelCapabilityProfile, REASONING_EFFORT_LABELS } from "./model-capabilities";
import {
  PROVIDER_PRESETS,
  SERVICE_GROUPS,
  ModelLimitFields,
  connectionBaseUrlError,
  connectionName,
  connectionOrigin,
  draftProvider,
  isCustomConnection,
  modelRemoteId,
  parseModelLimits,
  savedConnectionName,
  serviceKind,
  type ConnectionDraft,
  type ModelLimitDraft,
  type ModelMutation,
  type PanelMessage,
  type ServiceKind,
} from "./provider-presets";

const CAPABILITY_FIELDS = [
  ["tools", "工具调用"],
  ["images", "图片输入"],
  ["reasoning", "思考模式"],
] as const;

function changeProtocol(draft: ConnectionDraft, apiBackend: ApiBackend): ConnectionDraft {
  const baseUrl = draft.baseUrl.trim().replace(/\/+$/, "");
  const origin = connectionOrigin(baseUrl);
  const officialMiniMax = draft.service === "minimax"
    && (origin === "https://api.minimax.cn" || origin === "https://api.minimax.io")
    && [`${origin}/v1`, `${origin}/anthropic`, `${origin}/anthropic/v1`].includes(baseUrl);
  return {
    ...draft,
    apiBackend,
    baseUrl: officialMiniMax ? `${origin}/${apiBackend === "messages" ? "anthropic/v1" : "v1"}` : draft.baseUrl,
  };
}

export function ConnectionEditor({
  original,
  editedModel,
  capabilities,
  providerCapabilities,
  reusableProviders,
  catalogModels,
  runMutation,
  onCancel,
  onSaved,
}: {
  original?: ModelProviderEntry;
  editedModel?: ModelEntry;
  capabilities?: ModelCapabilities;
  providerCapabilities?: Record<string, ModelCapabilities>;
  reusableProviders: ModelProviderEntry[];
  catalogModels: ModelEntry[];
  runMutation: ModelMutation;
  onCancel: () => void;
  onSaved: (modelIds: string[], connectionChanged: boolean) => Promise<void>;
}) {
  const [connectionId, setConnectionId] = useState("");
  const reused = reusableProviders.find(provider => provider.id === connectionId);
  const provider = original ?? reused;
  const existingModels = provider ? catalogModels.filter(model => model.providerId === provider.id) : [];
  const initialKind = original ? serviceKind(original) : "custom";
  const [serviceChosen, setServiceChosen] = useState(Boolean(original));
  const initialPreset = PROVIDER_PRESETS[initialKind];
  const [draft, setDraft] = useState<ConnectionDraft>({
    label: original ? savedConnectionName(original) : "",
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
  const [modelLimits, setModelLimits] = useState<ModelLimitDraft>({
    contextWindow: editedModel?.contextWindow?.toString() ?? "",
    maxOutputTokens: editedModel?.maxOutputTokens?.toString() ?? "",
  });
  const [discovering, setDiscovering] = useState(false);
  const [saving, setSaving] = useState(false);
  const [discovered, setDiscovered] = useState<FetchedModel[]>([]);
  const modelIds = existingModels.map(modelRemoteId);
  const [search, setSearch] = useState("");
  const [modelInput, setModelInput] = useState(() => editedModel ? modelRemoteId(editedModel) : "");
  const [modelName, setModelName] = useState(editedModel?.name ?? "");
  const [capabilityOverrides, setCapabilityOverrides] = useState<ModelCapabilities>({});
  const [capabilityNotice, setCapabilityNotice] = useState(false);
  const [showProtocol, setShowProtocol] = useState(() => Boolean(original && (
    draft.apiBackend !== initialPreset.apiBackend || draft.authScheme !== initialPreset.authScheme
  )));
  const [error, setError] = useState<string | null>(null);
  const [invalidField, setInvalidField] = useState<string>();
  const [discoveryMessage, setDiscoveryMessage] = useState<PanelMessage | null>(null);
  const preset = PROVIDER_PRESETS[draft.service];
  const formSnapshot = JSON.stringify({ draft, modelInput, modelName, modelLimits });
  const initialDraft = useRef(formSnapshot);
  const editingModel = Boolean(editedModel);
  const sharedConnection = editingModel && existingModels.length > 1;
  const operationRef = useRef(false);
  const originChanged = Boolean(original && connectionOrigin(original.baseUrl) !== connectionOrigin(draft.baseUrl));
  const canReuseKey = original?.credentialConfigured && !originChanged;
  const { requestClose, closeDialog } = useUnsavedClose({
    dirty: formSnapshot !== initialDraft.current || Object.values(capabilityOverrides).some(value => value !== undefined),
    busy: saving || discovering,
    onClose: onCancel,
  });
  const cancel = () => { if (!operationRef.current) requestClose(); };
  const dialogRef = useModalFocus<HTMLDivElement>(true, cancel);
  const showFields = Boolean(original || serviceChosen || reused);
  const presetAddress = !isCustomConnection(draft.service);
  const expandable = !editingModel && (presetAddress || Boolean(reused));
  const [advancedOpen, setAdvancedOpen] = useState(false);

  const backend = reused?.apiBackend ?? (reused ? PROVIDER_PRESETS[serviceKind(reused)].apiBackend : draft.apiBackend);
  const baseUrl = reused?.baseUrl ?? draft.baseUrl;
  const defaultProfile = modelCapabilityProfile(modelInput, baseUrl, backend);
  const capabilityScope = JSON.stringify([connectionId, draft.service, baseUrl?.trim().replace(/\/+$/, ""), backend, modelInput.trim()]);
  const previousCapabilityScope = useRef(capabilityScope);
  useEffect(() => {
    if (previousCapabilityScope.current === capabilityScope) return;
    previousCapabilityScope.current = capabilityScope;
    if (Object.values(capabilityOverrides).some(value => value !== undefined)) {
      setCapabilityOverrides({});
      setCapabilityNotice(true);
    }
  }, [capabilityScope, capabilityOverrides]);
  const declaredCapabilities = capabilities ?? (reused ? providerCapabilities?.[reused.id] : undefined);
  const capabilityDefaults = {
    tools: declaredCapabilities?.tools ?? defaultProfile?.tools,
    images: declaredCapabilities?.images ?? defaultProfile?.images,
    reasoning: declaredCapabilities?.reasoning ?? (defaultProfile?.reasoningEfforts.length ? true : undefined),
  };
  const reasoningEnabled = (capabilityOverrides.reasoning ?? capabilityDefaults.reasoning) === true;
  const protocolEfforts = effortOptionsForProtocol(backend)
    .filter(effort => !defaultProfile || defaultProfile.reasoningEfforts.includes(effort));
  const configuredEffort = declaredCapabilities?.reasoningEffort;
  const compatibleConfiguredEffort = configuredEffort && protocolEfforts.includes(configuredEffort) ? configuredEffort : undefined;
  const availableEfforts = (capabilityOverrides.reasoningEfforts ?? declaredCapabilities?.reasoningEfforts
    ?? defaultProfile?.reasoningEfforts ?? []).filter(effort => protocolEfforts.includes(effort));
  const selectedEffort = capabilityOverrides.clearReasoningEffort ? undefined : capabilityOverrides.reasoningEffort ?? compatibleConfiguredEffort;
  const offeredEfforts = selectedEffort && protocolEfforts.includes(selectedEffort) && !availableEfforts.includes(selectedEffort)
    ? [...availableEfforts, selectedEffort] : availableEfforts;

  const changeEffort = (effort: ModelReasoningEffort | "") => {
    setCapabilityNotice(false);
    setCapabilityOverrides(current => ({
      ...current,
      reasoningEffort: effort && effort !== configuredEffort ? effort : undefined,
      clearReasoningEffort: !effort && (Boolean(configuredEffort) || Boolean(current.reasoningEfforts)) || undefined,
    }));
  };
  const changeSupportedEffort = (effort: ModelReasoningEffort, enabled: boolean) => {
    setCapabilityNotice(false);
    const next = enabled
      ? protocolEfforts.filter(value => value === effort || availableEfforts.includes(value))
      : availableEfforts.filter(value => value !== effort);
    const resetEffort = !selectedEffort || !next.includes(selectedEffort);
    setCapabilityOverrides(current => ({
      ...current,
      reasoningEfforts: next,
      reasoningEffort: resetEffort ? undefined : current.reasoningEffort,
      clearReasoningEffort: resetEffort || undefined,
    }));
  };

  const updateAddress = (baseUrl: string) => {
    setError(null);
    if (connectionOrigin(baseUrl) !== connectionOrigin(draft.baseUrl)) setShowKey(false);
    setDraft(current => ({
      ...current,
      baseUrl,
      apiKey: connectionOrigin(baseUrl) === connectionOrigin(current.baseUrl) ? current.apiKey : "",
      allowInsecureHttp: baseUrl.trim() === current.baseUrl.trim() && current.allowInsecureHttp,
    }));
  };

  const handleKindChange = (service: ServiceKind) => {
    const next = PROVIDER_PRESETS[service];
    const defaultName = isCustomConnection(service) ? "" : next.shortLabel;
    setServiceChosen(true); setError(null); setShowKey(false);
    setShowProtocol(false);
    setDraft((current) => ({
      ...current,
      service,
      providerKind: service === "kimi" || service === "zhipu" ? "custom" : service,
      label: !current.label || current.label === PROVIDER_PRESETS[current.service].shortLabel ? defaultName : current.label,
      baseUrl: next.baseUrl,
      apiKey: connectionOrigin(next.baseUrl) === connectionOrigin(current.baseUrl) ? current.apiKey : "",
      allowInsecureHttp: false,
      apiBackend: next.apiBackend,
      authScheme: next.authScheme,
    }));
  };

  const validate = (): string | null => {
    setInvalidField(undefined);
    if (reused) return null;
    const fail = (field: string, text: string) => { setInvalidField(field); return text; };
    if (!serviceChosen) return fail("model-provider-kind", "请选择模型服务商。");
    const baseUrlError = connectionBaseUrlError(draft.baseUrl, draft.allowInsecureHttp);
    if (baseUrlError) return fail("model-base-url", baseUrlError);
    if ((!original || originChanged || original.credentialConfigured === false) && (!draft.apiKey.trim() || draft.apiKey.trim().startsWith("•"))) {
      return fail("model-api-key", originChanged ? "服务地址已更换，请填写新地址的 API 密钥。" : "请填写 API 密钥。");
    }
    return null;
  };

  const connectionForRequest = () => {
    const connection = draftProvider(original, draft);
    // The model form has no connection-name field. Preserve the stored label.
    return editingModel ? { ...connection, label: original?.label } : connection;
  };

  const handleDiscover = async () => {
    if (operationRef.current) return;
    const validation = validate();
    if (validation) { setDiscoveryMessage(null); setError(validation); return; }
    if (editingModel && !modelInput.trim()) {
      setInvalidField("model-id-input");
      setError("请填写模型名称（上游模型 ID）。");
      return;
    }
    operationRef.current = true;
    setDiscovering(true);
    setError(null);
    setDiscoveryMessage(null);
    try {
      if (editingModel) {
        await providersTestModelConnection(connectionForRequest(), modelInput.trim());
        setDiscoveryMessage({ kind: "ok", text: "测试通过，模型已成功回复。" });
      } else {
        const found = reused ? await providersFetchModelsForProvider(reused.id) : await providersTestConnection(connectionForRequest());
        const models = found.filter(model => !modelIds.includes(model.id));
        setDiscovered(models);
        setDiscoveryMessage({ kind: models.length ? "ok" : "warn", text: models.length ? `找到 ${models.length} 个模型，可选择或手动填写。` : "服务未返回可添加的模型，可手动填写模型 ID。" });
      }
    } catch (requestError) {
      setDiscovered([]);
      setDiscoveryMessage({
        kind: "warn",
        text: editingModel ? `测试失败：${String(requestError).replace(/^Error:\s*/, "")}`
          : `无法获取模型列表：${String(requestError)}。可手动填写模型 ID 后保存。`,
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
    const remoteModelId = modelInput.trim();
    const remoteIdChanged = !editedModel || remoteModelId !== modelRemoteId(editedModel);
    if (!remoteModelId || (remoteIdChanged && modelIds.includes(remoteModelId))) {
      setInvalidField("model-id-input");
      setError(remoteModelId ? "此模型已添加到这个连接。" : editingModel ? "请填写模型名称（上游模型 ID）。" : "请填写模型 ID。");
      return;
    }
    const limits = parseModelLimits(modelLimits, reused?.contextWindow ?? original?.contextWindow);
    if (typeof limits === "string") {
      setInvalidField(limits.startsWith("上下文") ? "manual-model-context-window" : "manual-model-max-output");
      setError(limits);
      return;
    }
    const allowedEfforts = capabilityOverrides.reasoningEfforts ?? availableEfforts;
    if (capabilityOverrides.reasoningEffort && (!protocolEfforts.includes(capabilityOverrides.reasoningEffort)
      || !allowedEfforts.includes(capabilityOverrides.reasoningEffort))) {
      setError("当前模型不支持所选思考档位。请重新选择思考强度。");
      return;
    }
    operationRef.current = true;
    setSaving(true);
    setError(null);
    try {
      await runMutation(async () => {
        const models = editedModel ? [{
          modelId: editedModel.modelId,
          providerId: editedModel.providerId,
          remoteModelId,
          name: modelName.trim() || undefined,
          ...limits,
          clearMaxOutputTokens: editedModel.maxOutputTokens !== undefined && limits.maxOutputTokens === undefined,
          capabilityOverrides: Object.values(capabilityOverrides).some(value => value !== undefined) ? capabilityOverrides : undefined,
        }] : [{ modelId: "", providerId: reused?.id ?? "", remoteModelId, name: modelName.trim() || undefined, ...limits,
          capabilityOverrides: Object.values(capabilityOverrides).some(value => value !== undefined) ? capabilityOverrides : undefined }];
        const result = await providersSaveConnection(
          reused ? { ...reused, apiKey: undefined } : connectionForRequest(),
          models,
          !reused && !editingModel,
        );
        const connectionChanged = Boolean(original && editedModel && (
          remoteModelId !== modelRemoteId(editedModel) ||
          draft.baseUrl.trim() !== original.baseUrl?.trim() ||
          draft.apiBackend !== (original.apiBackend ?? initialPreset.apiBackend) ||
          draft.authScheme !== (original.authScheme ?? initialPreset.authScheme) ||
          limits.contextWindow !== editedModel.contextWindow ||
          limits.maxOutputTokens !== editedModel.maxOutputTokens ||
          Boolean(draft.apiKey.trim())
        ));
        await onSaved(result.modelIds, connectionChanged);
      });
    } catch (saveError) {
      setError(String(saveError));
    } finally {
      operationRef.current = false;
      setSaving(false);
    }
  };

  const connectionSignature = JSON.stringify([connectionId, draft.service, draft.baseUrl, draft.apiKey, draft.apiBackend, draft.authScheme, draft.allowInsecureHttp, editingModel ? modelInput : undefined]);
  useEffect(() => { setDiscovered([]); setDiscoveryMessage(null); setSearch(""); }, [connectionSignature]);
  const disabled = saving || discovering;
  const openKeyHelp = async () => {
    if (!preset.keyUrl) return;
    try { await openExternalUrl(preset.keyUrl); }
    catch { setInvalidField(undefined); setError("暂时无法打开服务商页面，请稍后重试。"); }
  };
  const visibleModels = discovered.filter(model => !search.trim() || model.id.toLowerCase().includes(search.trim().toLowerCase()));
  const fieldError = (id: string) => error && invalidField === id && <span id={`${id}-error`} className="model-connection-editor__field-error" role="alert">{error}</span>;
  const fieldA11y = (id: string, connectionField = false) => {
    const invalid = Boolean(error && invalidField === id);
    const descriptions = [
      connectionField && sharedConnection ? "model-shared-connection" : undefined,
      invalid ? `${id}-error` : undefined,
    ].filter(Boolean);
    return { "aria-invalid": invalid, "aria-describedby": descriptions.join(" ") || undefined };
  };
  const advancedFields = useMemo(() => new Set([
    "manual-model-context-window", "manual-model-max-output",
    ...(!original && presetAddress ? ["model-base-url"] : []),
  ]), [original, presetAddress]);
  useEffect(() => { if (expandable && error && invalidField && advancedFields.has(invalidField)) setAdvancedOpen(true); }, [expandable, error, invalidField, advancedFields]);
  // An invalid advanced field only becomes focusable after the section opens.
  const invalidFieldVisible = Boolean(invalidField && (!expandable || advancedOpen || !advancedFields.has(invalidField)));
  useEffect(() => {
    if (error && invalidField && invalidFieldVisible) dialogRef.current?.querySelector<HTMLElement>(`#${invalidField}`)?.focus();
  }, [error, invalidField, invalidFieldVisible, dialogRef]);
  const chooseProvider = (value: string) => {
    if (value.startsWith("saved:")) {
      const id = value.slice("saved:".length);
      if (id === connectionId) return;
      setConnectionId(id);
      setModelInput("");
      setError(null);
      setInvalidField(undefined);
      setShowKey(false);
      return;
    }
    if (!reused && serviceChosen && value === draft.service) return;
    setConnectionId("");
    setInvalidField(undefined);
    if (reused) setModelInput("");
    handleKindChange(value as ServiceKind);
  };

  const keyAction = <Button onClick={() => void handleDiscover()}
    data-tip={editingModel ? "用当前表单配置发送一条文本请求，可能消耗少量额度" : "读取服务的模型列表以检查连接"}
    disabled={disabled}>{discovering ? "测试中…" : "测试连接"}</Button>;
  const discoverButton = <Button aria-label="从服务获取模型" onClick={() => void handleDiscover()} disabled={disabled}>
    {discovering ? "获取中…" : "获取模型"}
  </Button>;

  const baseUrlField = <div className="models-settings-panel__field">
    <label className="models-settings-panel__label" htmlFor="model-base-url">接口地址</label>
    <input id="model-base-url" {...fieldA11y("model-base-url", true)} className="form-control models-settings-panel__input"
      value={draft.baseUrl} onChange={event => updateAddress(event.target.value)} placeholder="https://api.example.com/v1" inputMode="url" />
    {fieldError("model-base-url")}
    {draft.baseUrl.trim().toLowerCase().startsWith("http://") && <label className="model-connection-editor__help model-connection-editor__http-consent">
      <input type="checkbox" checked={draft.allowInsecureHttp} onChange={event => setDraft(current => ({ ...current, allowInsecureHttp: event.target.checked }))} />
      允许此个人连接使用 HTTP。我了解 API Key、提问和模型回复会以明文传输。
    </label>}
  </div>;

  const apiKeyField = <div className="models-settings-panel__field">
    <div className="model-connection-editor__row-heading">
      <label className="models-settings-panel__label" htmlFor="model-api-key">API Key</label>
      {editingModel && canReuseKey && <span className="model-connection-editor__help">已保存，留空沿用</span>}
    </div>
    <div className="model-connection-editor__input-action">
      <div className="models-settings-panel__input-shell">
        <input id="model-api-key" {...fieldA11y("model-api-key", true)}
          className={`form-control models-settings-panel__input${draft.apiKey ? " models-settings-panel__input--with-trailing-icon" : ""}`}
          type={showKey ? "text" : "password"} value={draft.apiKey}
          onChange={event => {
            const apiKey = event.target.value;
            if (!apiKey) setShowKey(false);
            setError(null);
            setDraft(current => ({ ...current, apiKey }));
          }}
          placeholder={canReuseKey ? "••••••••••••••••••••" : preset.placeholderKey} autoComplete="off" />
        {draft.apiKey && <IconButton className="models-settings-panel__input-toggle" onClick={() => setShowKey(value => !value)}
          label={showKey ? "隐藏 API 密钥" : "显示 API 密钥"}>
          {showKey ? <EyeOff size={16} aria-hidden="true" /> : <Eye size={16} aria-hidden="true" />}
        </IconButton>}
      </div>
      {keyAction}
    </div>
    {fieldError("model-api-key")}
    {originChanged && <span className="model-connection-editor__help">服务已变更，请填写新服务的密钥。</span>}
    {discoveryMessage && <p className={`model-connections__message model-connections__message--${discoveryMessage.kind}`} role="status">{discoveryMessage.text}</p>}
  </div>;

  const protocolField = <div className="models-settings-panel__field">
    <label className="models-settings-panel__label" htmlFor="model-api-backend">请求协议</label>
    <Select id="model-api-backend" {...fieldA11y("model-api-backend", true)} className="form-control models-settings-panel__select" value={draft.apiBackend}
      onChange={event => {
        const apiBackend = event.target.value as ApiBackend;
        setDraft(current => changeProtocol(current, apiBackend));
      }}>
      <option value="chat_completions">Chat Completions</option>
      <option value="responses">Responses</option>
      <option value="messages">Messages</option>
    </Select>
  </div>;

  const authField = <div className="models-settings-panel__field">
    <label className="models-settings-panel__label" htmlFor="model-auth-scheme">认证方式</label>
    <Select id="model-auth-scheme" {...fieldA11y("model-auth-scheme", true)} className="form-control models-settings-panel__select" value={draft.authScheme}
      onChange={event => setDraft(current => ({ ...current, authScheme: event.target.value as AuthScheme }))}>
      <option value="bearer">Bearer Token</option>
      <option value="x_api_key">X-API-Key</option>
    </Select>
  </div>;

  const modelField = <>
    <div className="models-settings-panel__field">
      <div className="model-connection-editor__row-heading">
        <label className="models-settings-panel__label" htmlFor="model-id-input">模型名称</label>
      </div>
      <div className="model-connection-editor__input-action">
        <input id="model-id-input" {...fieldA11y("model-id-input")} className="form-control models-settings-panel__input"
          value={modelInput} onChange={event => { setModelInput(event.target.value); setError(null); }}
          placeholder="服务商的模型 ID" autoComplete="off" />
        {reused && discoverButton}
      </div>
      {fieldError("model-id-input")}
      {reused && discoveryMessage && <p className={`model-connections__message model-connections__message--${discoveryMessage.kind}`} role="status">{discoveryMessage.text}</p>}
    </div>
    {discovered.length > 0 && <div className="model-connection-editor__discovered">
      {discovered.length > 8 && <input className="form-control models-settings-panel__input" type="search" value={search} onChange={event => setSearch(event.target.value)} aria-label="筛选已获取的模型" placeholder="查找模型" />}
      <div className="model-connection-editor__model-picker" role="group" aria-label="从服务选择模型">
        {visibleModels.map(model => <label key={model.id}><input type="radio" name="discovered-model" checked={modelInput.trim() === model.id}
          onChange={() => { setModelInput(model.id); setError(null); }} /><span><strong>{model.id}</strong></span></label>)}
      </div>
      {visibleModels.length === 0 && <p className="model-settings__hint">没有找到匹配的模型，可直接填写模型 ID。</p>}
    </div>}
  </>;

  const modelAdvanced = <section className="model-connection-editor__advanced-section" aria-labelledby="model-editor-advanced">
    {!expandable && <h3 id="model-editor-advanced" className="models-settings-panel__label">高级配置</h3>}
    {capabilityNotice && <p className="model-settings__hint" role="status">模型或连接已更换，请重新检查能力与思考档位。</p>}
    <div className="model-editor-capabilities" role="group" aria-label="能力与协议">
      {CAPABILITY_FIELDS.map(([key, label]) => {
        const enabled = capabilityOverrides[key] ?? capabilityDefaults[key];
        return <label key={key}>
          <input type="checkbox" checked={enabled === true}
            aria-checked={enabled === undefined ? "mixed" : undefined}
            ref={input => { if (input) input.indeterminate = enabled === undefined; }}
            onChange={event => {
              const checked = event.target.checked;
              setCapabilityNotice(false);
              setCapabilityOverrides(current => ({ ...current, [key]: checked === capabilityDefaults[key] ? undefined : checked }));
            }} />
          {key === "reasoning" && defaultProfile?.alwaysThinks ? "调节思考强度" : label}
        </label>;
      })}
      {!reused && <label><input type="checkbox" checked={showProtocol} onChange={event => {
        setShowProtocol(event.target.checked);
        if (!event.target.checked) {
          setDraft(current => ({ ...changeProtocol(current, preset.apiBackend), authScheme: preset.authScheme }));
        }
      }} />自定义协议</label>}
    </div>
    {!reused && showProtocol && <div className="models-settings-panel__field-row">{protocolField}{authField}</div>}
    {CAPABILITY_FIELDS.some(([key]) => (capabilityOverrides[key] ?? capabilityDefaults[key]) === undefined) &&
      <p className="model-settings__hint">带横线的选项表示模型配置未声明该能力。</p>}
    {reasoningEnabled && <div className="model-editor-reasoning">
      {offeredEfforts.length > 0 && <div className="models-settings-panel__field">
        <label className="models-settings-panel__label" htmlFor="model-reasoning-effort">默认思考强度</label>
        <Select id="model-reasoning-effort" className="form-control models-settings-panel__select"
          value={selectedEffort && offeredEfforts.includes(selectedEffort) ? selectedEffort : ""}
          onChange={event => changeEffort(event.target.value as ModelReasoningEffort | "")}>
          <option value="">自动（使用请求默认值）</option>
          {offeredEfforts.map(effort => <option key={effort} value={effort}>{REASONING_EFFORT_LABELS[effort]}</option>)}
        </Select>
        {defaultProfile?.providerDefaultEffort && !selectedEffort && <p className="model-settings__hint">
          服务商默认强度为「{REASONING_EFFORT_LABELS[defaultProfile.providerDefaultEffort]}」。
        </p>}
      </div>}
      {(!defaultProfile || declaredCapabilities?.reasoningEfforts !== undefined) && <fieldset className="model-editor-efforts">
        <legend className="models-settings-panel__label">支持的思考强度</legend>
        <div className="model-editor-capabilities">
          {protocolEfforts.map(effort => <label key={effort}>
            <input type="checkbox" checked={availableEfforts.includes(effort)}
              onChange={event => changeSupportedEffort(effort, event.target.checked)} />
            {REASONING_EFFORT_LABELS[effort]}
          </label>)}
        </div>
        <p className="model-settings__hint">请选择接口明确支持的档位。全部取消时使用模型默认行为。</p>
      </fieldset>}
    </div>}
    {(capabilityOverrides.reasoning ?? capabilityDefaults.reasoning) === false && <p className="model-settings__hint">
      {defaultProfile?.alwaysThinks ? "该模型始终开启思考，当前由服务商决定强度。" : "已关闭强度调节，思考行为由服务商决定。"}
    </p>}
    <ModelLimitFields draft={modelLimits} onChange={setModelLimits} connectionContextWindow={provider?.contextWindow}
      fieldA11y={fieldA11y} fieldError={fieldError} layout="directions" />
    <details className="model-editor-display-name">
      <summary>显示名称（可选）</summary>
      <input aria-label="模型显示名称" className="form-control models-settings-panel__input" value={modelName}
        placeholder="用于模型列表和选择器" onChange={event => setModelName(event.target.value)} />
    </details>
  </section>;
  const title = editingModel ? "编辑模型" : "添加模型";

  return <div ref={dialogRef} className="models-settings-panel__editor-overlay" role="dialog" aria-modal="true" aria-label={title} tabIndex={-1}>
    <div className="model-connection-editor model-connection-editor--model">
      <header className="models-settings-panel__editor-header">
        <h2 className="models-settings-panel__editor-title">{title}</h2>
        <IconButton label="关闭" onClick={cancel} disabled={disabled} data-modal-initial-focus={editingModel || undefined}><X size={18} aria-hidden="true" /></IconButton>
      </header>
      <div className="model-connection-editor__body">
        <fieldset className="model-connection-editor__fields" disabled={disabled}>
          <div className="models-settings-panel__field">
            <div className="model-connection-editor__row-heading">
              <label className="models-settings-panel__label" htmlFor="model-provider-kind">供应商</label>
              {showFields && !reused && !editingModel && preset.keyUrl && <Button variant="ghost" size="compact" onClick={() => void openKeyHelp()}>
                获取密钥<ChevronRight size={12} aria-hidden="true" />
              </Button>}
            </div>
            <Select id="model-provider-kind" className="form-control models-settings-panel__select"
              value={reused ? `saved:${reused.id}` : serviceChosen ? draft.service : ""}
              data-modal-initial-focus={!editingModel || undefined}
              {...fieldA11y("model-provider-kind", true)}
              onChange={event => chooseProvider(event.target.value)}>
              <option value="" disabled>选择供应商</option>
              {SERVICE_GROUPS.map(group => <optgroup key={group.label} label={group.label}>
                {group.kinds.map(kind => <option key={kind} value={kind}>{PROVIDER_PRESETS[kind].label}</option>)}
              </optgroup>)}
              {!original && reusableProviders.length > 0 && <optgroup label="已保存的连接">
                {reusableProviders.map(saved => <option key={saved.id} value={`saved:${saved.id}`}>{connectionName(saved)}</option>)}
              </optgroup>}
            </Select>
            {sharedConnection && <p id="model-shared-connection" className="model-editor-shared" role="note">
              此连接由 {existingModels.length} 个模型共用。修改供应商、接口地址、API Key 或协议与认证设置会同时影响这些模型。
            </p>}
          </div>
          {showFields && <>
            {!reused && <>
              {(editingModel || !presetAddress) && baseUrlField}
              {apiKeyField}
            </>}
            {modelField}
            {!expandable ? modelAdvanced : <details className="model-connection-editor__advanced" open={advancedOpen}>
              <summary id="model-editor-advanced" onClick={event => { event.preventDefault(); setAdvancedOpen(open => !open); }}>高级配置</summary>
              <div className="model-connection-editor__advanced-body">
                {!reused && presetAddress && baseUrlField}
                {modelAdvanced}
              </div>
            </details>}
          </>}
          {error && !invalidField && <div className="models-settings-panel__editor-error" role="alert">{error}</div>}
        </fieldset>
      </div>
      <footer className="models-settings-panel__editor-footer">
        <Button onClick={cancel} disabled={disabled}>取消</Button>
        <Button variant="primary" aria-label="保存模型" onClick={() => void handleSave()}
          disabled={disabled || !showFields}>{saving ? "保存中…" : "保存"}</Button>
      </footer>
    </div>
    {closeDialog}
  </div>;
}
