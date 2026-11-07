import type { ModelProviderEntry, ProviderListModel } from "../../src/lib/agent-client";

/** Isolated model settings responses; no real credentials, requests or disk writes. */
export function modelSettingsNativeReview(query: URLSearchParams) {
  if (!query.has("modelSettingsReview")) return null;
  const account = query.get("modelAccount") ?? "personal";
  const personal: ModelProviderEntry = { id: "review-personal", label: "工作模型服务", providerKind: "custom", source: "personal", baseUrl: "https://review.invalid/v1", apiBackend: "chat_completions", authScheme: "bearer", credentialConfigured: true, contextWindow: 64000 };
  const organization: ModelProviderEntry = { ...personal, id: "review-organization", label: "团队模型服务", source: "organization", managed: true };
  const catalog: ProviderListModel = { providers: account === "none" || account === "org-empty" ? [] : account === "personal" ? [personal] : [organization, personal], models: account === "none" || account === "org-empty" ? [] : [
    ...(account === "personal" ? [] : [{ modelId: "review-organization/team", remoteModelId: "team-model", name: "团队模型", providerId: organization.id, managed: true }]),
    { modelId: "review-personal/main", remoteModelId: "main-model", name: "工作模型", providerId: personal.id, contextWindow: 64000, maxOutputTokens: 4096 },
    { modelId: "review-personal/backup", remoteModelId: "backup-model", name: "备用模型", providerId: personal.id },
  ] };
  if (query.has("modelLongCatalog")) {
    catalog.models.push(...Array.from({ length: 24 }, (_, index) => ({
      modelId: `${personal.id}/extra-${index}`, remoteModelId: `extra-${index}`,
      name: `长目录模型 ${index + 1}`, providerId: personal.id,
    })));
    catalog.providers.push(...Array.from({ length: 24 }, (_, index) => ({
      ...personal, id: `review-connection-${index}`, label: `备用连接 ${index + 1}`,
    })));
  }
  let defaults = { defaultModel: catalog.models.find(model => model.providerId === personal.id)?.modelId ?? "", defaultPermission: "ask", rememberToolApprovals: true };
  const calls = { saves: 0, defaults: 0, reloads: 0, discoveries: 0, syncs: 0, logins: 0, tests: [] as string[] };
  Object.assign(window, { modelSettingsReviewCalls: calls });
  let reads = 0;
  const commands = new Set(["providers_list", "agents_defaults_get", "agents_defaults_save", "agent_auth_status", "providers_save_connection", "providers_test_connection", "providers_test_model_connection", "internal_reload", "org_session", "org_sync_model_config"]);
  return { calls, commands, invoke: async (command: string, args: any) => {
    switch (command) {
      case "providers_list": if (query.has("modelReadError") && reads++ === 0) throw new Error("隔离验证：配置暂不可读"); return structuredClone(catalog);
      case "agents_defaults_get": return defaults;
      case "agents_defaults_save": calls.defaults++; defaults = args.defaults; return;
      case "agent_auth_status": return { ready: catalog.models.length > 0, providers: catalog.models.map(model => model.modelId), runtimeReady: true, runtimeAuthReady: true, synchronized: true, runtimeModels: catalog.models.map(model => model.modelId), defaultModelId: defaults.defaultModel || catalog.models[0]?.modelId };
      case "internal_reload": calls.reloads++; return;
      case "providers_save_connection": {
        calls.saves++;
        const id = args.provider.id || `review-new-${calls.saves}`;
        const previous = catalog.providers.find(provider => provider.id === id);
        const provider = { ...previous, ...args.provider, id, apiKey: undefined, credentialConfigured: Boolean(args.provider.apiKey || previous?.credentialConfigured) };
        catalog.providers = [...catalog.providers.filter(provider => provider.id !== id), provider];
        const models = args.models.map((model: any) => ({ ...model, modelId: model.modelId || `${id}/${model.remoteModelId}`, providerId: id }));
        catalog.models = [...catalog.models.filter(model => args.replaceModels ? model.providerId !== id : !models.some((next: any) => next.modelId === model.modelId)), ...models];
        return { providerId: id, modelIds: models.map((model: any) => model.modelId) };
      }
      case "providers_test_connection": calls.discoveries++; if (query.has("modelDiscoveryError")) throw new Error("404 /models"); return [{ id: "main-model" }, { id: "backup-model" }, { id: "new-model" }];
      case "providers_test_model_connection": calls.tests.push(args.modelId); await new Promise(resolve => setTimeout(resolve, 120)); if (args.modelId === "failed-model") throw new Error("模型访问被拒绝，请检查模型 ID 和密钥权限"); return;
      case "org_session": return { loggedIn: account === "mixed" || account === "org-empty", serverUrl: account === "personal" || account === "none" ? undefined : "https://organization.review.invalid", requiresReauthentication: account === "expired", bootstrap: { scopes: account === "mixed" || account === "org-empty" ? [{ id: "review-org", kind: "org", name: "验证组织" }] : [] } };
      case "org_sync_model_config": calls.syncs++; if (query.has("modelSyncError")) throw new Error("组织服务暂时不可连接"); return { configured: account === "mixed", syncedAt: Date.now() };
    }
  } };
}
