/** Legacy identifiers are retained only to exclude removed catalog entries. */
export function isRemovedModelId(id: string): boolean {
  return id.startsWith("echoagent-ojlab/");
}

export function isRemovedModelProvider(provider: { id: string; source?: string }): boolean {
  return provider.id === "echoagent-ojlab" || provider.source === "builtin";
}

export function withoutRemovedModels<T extends { providers: { id: string; source?: string }[]; models: { modelId: string; providerId: string }[] }>(catalog: T): T {
  const providers = catalog.providers.filter(provider => !isRemovedModelProvider(provider));
  const removedProviderIds = new Set(["echoagent-ojlab", ...catalog.providers.filter(isRemovedModelProvider).map(provider => provider.id)]);
  const models = catalog.models.filter(model => !isRemovedModelId(model.modelId) && !removedProviderIds.has(model.providerId));
  return { ...catalog, providers, models };
}
