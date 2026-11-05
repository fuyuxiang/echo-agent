import type { GroupCreationSource } from "./group-store";

export interface GroupCreationDraft {
  scope: string;
  name: string;
  goal: string;
  directory: string;
  model: string;
  selected: string[];
  roles?: Record<string, string>;
  memberModels?: Record<string, string>;
  coordinatorName?: string;
  coordinatorRole?: string;
  templateId?: string;
  requestKey?: string;
  requestId?: string;
  reference?: boolean;
  source?: GroupCreationSource;
}

const stringMap = (value: unknown): value is Record<string, string> => !!value
  && typeof value === "object" && !Array.isArray(value)
  && Object.values(value).every(item => typeof item === "string");

function sourceSnapshot(value: unknown): GroupCreationSource | undefined {
  if (!value || typeof value !== "object") return;
  const source = value as GroupCreationSource;
  if (typeof source.sessionId !== "string" || !source.sessionId || !source.team
    || typeof source.team.teamId !== "string" || !source.team.teamId
    || (source.team.description !== undefined && typeof source.team.description !== "string") || !Array.isArray(source.team.members)
    || !source.team.members.every(item => typeof item === "string") || (source.team.roles !== undefined && !stringMap(source.team.roles))
    || (source.referenceText !== undefined && typeof source.referenceText !== "string")) return;
  return { ...source, team: { ...source.team, description: source.team.description ?? "", roles: source.team.roles ?? {} } };
}

/** Restore the submitted input as well as the visible fields: both define creation identity. */
export function readGroupCreationDraft(raw: string | undefined, scope?: string): GroupCreationDraft | null {
  try {
    const value = JSON.parse(raw || "null") as GroupCreationDraft | null;
    if (!value || (scope && value.scope !== scope)
      || ![value.scope, value.name, value.goal, value.directory, value.model].every(item => typeof item === "string")
      || !Array.isArray(value.selected) || !value.selected.every(item => typeof item === "string")
      || [value.roles, value.memberModels].some(item => item !== undefined && !stringMap(item))
      || [value.coordinatorName, value.coordinatorRole, value.templateId, value.requestKey, value.requestId].some(item => item !== undefined && typeof item !== "string")) return null;
    let source = sourceSnapshot(value.source);
    // Older drafts already retained the submitted input in their request key.
    if (!source && value.scope.startsWith("source:")) {
      let input: Record<string, unknown> = {};
      try {
        const candidate = JSON.parse(value.requestKey || "{}");
        if (candidate && typeof candidate === "object" && !Array.isArray(candidate)) input = candidate;
      } catch { /* An unsubmitted draft has no key. */ }
      const match = /^source:([^:]+):(.+)$/.exec(value.scope);
      const sessionId = typeof input.sourceSessionId === "string" ? input.sourceSessionId : match?.[1];
      const teamId = typeof input.sourceTeamId === "string" ? input.sourceTeamId : match?.[2];
      if (sessionId && teamId) source = { sessionId,
        team: { teamId, description: value.goal, members: value.selected, roles: value.roles ?? {} },
        ...(typeof input.referenceText === "string" ? { referenceText: input.referenceText } : {}),
      };
    }
    if (value.scope.startsWith("source:") && !source) return null;
    return { ...value, source };
  } catch { return null; }
}
