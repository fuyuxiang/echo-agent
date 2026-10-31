import { sanitizeModelLabel } from "./model-branding";

/** Presentation metadata only. IDs remain unchanged for persistence and routing. */
export interface PresentableModel {
  id: string;
  label?: string;
  remoteModelId?: string;
  providerId?: string;
  providerLabel?: string;
  source?: "personal" | "organization" | "builtin" | "legacy";
}

/** Only remove our reserved namespaces, never arbitrary vendor/model slugs. */
function withoutManagedPrefix(id: string): string {
  return id.replace(/^(?:echoagent-ojlab|organization)\//, "");
}

export function modelDisplayName(
  model: PresentableModel | string | null | undefined,
  fallback = "未指定模型",
): string {
  if (!model) return fallback;
  if (typeof model === "string") return sanitizeModelLabel(withoutManagedPrefix(model), fallback);
  const label = model.label?.trim();
  const remote = model.remoteModelId?.trim();
  const name = label && label !== model.id
    ? label
    : remote && remote !== model.id
    ? remote
    : withoutManagedPrefix(model.id);
  return sanitizeModelLabel(name, fallback);
}

export function modelSourceLabel(model: PresentableModel | string): string {
  const id = typeof model === "string" ? model : model.id;
  const source = typeof model === "string" ? undefined : model.source;
  if (source === "builtin" || (!source && id.startsWith("echoagent-ojlab/"))) return "内置";
  if (source === "organization" || (!source && id.startsWith("organization/"))) return "组织";
  return "个人";
}

/** Keep source groups consistent with ModelConnectionsPanel; preserve the
 * catalog order within each group instead of reordering by selection/speed. */
export function groupModelsBySource<T extends PresentableModel>(models: readonly T[]): { label: string; models: T[] }[] {
  return [
    { source: "组织", label: "组织提供" },
    { source: "个人", label: "我的连接" },
    { source: "内置", label: "内置模型" },
  ].map(({ source, label }) => ({ label, models: models.filter(model => modelSourceLabel(model) === source) }))
    .filter(group => group.models.length > 0);
}

/** Friendly connection name distinguishes models sharing the same name/source. */
export function modelConnectionLabel(model: PresentableModel): string | undefined {
  if (modelSourceLabel(model) === "内置") return undefined;
  return model.providerLabel?.trim() || model.providerId?.trim() || undefined;
}

/** For native selects, tooltips, and other places without room for a badge. */
function baseChoiceLabel(model: PresentableModel): string {
  const source = modelSourceLabel(model);
  const connection = modelConnectionLabel(model);
  return [modelDisplayName(model), source, connection !== source ? connection : undefined].filter(Boolean).join(" · ");
}

/** Resolve collisions progressively, including aliases on the same connection.
 * Routing IDs are a last resort; ordinary unique choices stay concise. */
export function modelChoiceLabel(model: PresentableModel, catalog: readonly PresentableModel[] = []): string {
  const base = baseChoiceLabel(model);
  const peers = catalog.filter(item => item.id !== model.id && baseChoiceLabel(item) === base);
  if (!peers.length) return base;
  const remote = model.remoteModelId?.trim();
  if (remote && !peers.some(item => item.remoteModelId?.trim() === remote)) return `${base} · ${sanitizeModelLabel(remote)}`;
  if (model.providerId && !peers.some(item => item.providerId === model.providerId)) return `${base} · ${model.providerId}`;
  return `${base} · ${sanitizeModelLabel(model.id)}`;
}

export function findModelDisplayName(
  models: readonly PresentableModel[],
  id?: string,
  fallback = "未指定模型",
): string {
  return modelDisplayName(models.find((model) => model.id === id) ?? id, fallback);
}

export function findModelChoiceLabel(
  models: readonly PresentableModel[],
  id?: string,
  fallback = "未指定模型",
): string {
  return id ? modelChoiceLabel(models.find((model) => model.id === id) ?? { id }, models) : fallback;
}
