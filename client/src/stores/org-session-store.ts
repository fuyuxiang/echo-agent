import { create } from "zustand";
import { orgSession, type OrgSession } from "@/lib/org-client";

interface OrgSessionState {
  session: OrgSession | null;
  hydrated: boolean;
  /** Changes whenever the account or its authorized scopes change. */
  boundaryRevision: number;
  hydrate: () => Promise<OrgSession | null>;
  setSession: (session: OrgSession) => void;
  clearSession: () => void;
}

let hydrationInFlight: Promise<OrgSession | null> | null = null;
let sessionMutation = 0;

/** Signed lease timestamps change on every healthy renewal. Only authorization
 * semantics may invalidate a user's in-flight work and cached resource views. */
function authorizationPolicy(policy: Record<string, unknown> | undefined): unknown {
  if (!policy) return null;
  const canonical = (value: unknown): unknown => {
    if (Array.isArray(value)) return value.map(canonical);
    if (value && typeof value === "object") return Object.fromEntries(
      Object.entries(value).sort(([a], [b]) => a.localeCompare(b))
        .map(([key, entry]) => [key, canonical(entry)]),
    );
    return value;
  };
  return canonical(Object.fromEntries(Object.entries(policy)
    .filter(([key]) => key !== "issuedAt" && key !== "expiresAt")));
}

export function organizationSessionBoundary(session: OrgSession | null): string {
  if (!session?.loggedIn) return "signed-out";
  const user = session.user ?? session.bootstrap?.user;
  return JSON.stringify([
    session.serverUrl,
    user?.id,
    user?.role,
    user?.clearance,
    session.bootstrap?.scopes?.map((scope) => [scope.id, scope.kind, scope.canPublishDocuments])
      .sort((a, b) => String(a[0]).localeCompare(String(b[0]))),
    authorizationPolicy(session.bootstrap?.policy),
  ]);
}

export interface OrganizationKnowledgeAvailability {
  available: boolean;
  reason: string;
  /** Account/server identity used to partition renderer-side Runtime acks. */
  identity: string;
}

export function organizationKnowledgeAvailability(
  state: Pick<OrgSessionState, "session" | "hydrated"> = useOrgSessionStore.getState(),
): OrganizationKnowledgeAvailability {
  const { session, hydrated } = state;
  const hasSharedScope = Boolean(session?.bootstrap?.scopes.some(
    (scope) => scope.kind === "team" || scope.kind === "org",
  ));
  const identity = session?.loggedIn && session.serverUrl && session.user?.id
    ? JSON.stringify([session.serverUrl, session.user.id])
    : "signed-out";

  if (!hydrated) {
    return { available: false, reason: "正在检查组织连接状态", identity };
  }
  if (!session?.loggedIn) {
    return { available: false, reason: "登录组织后可用", identity };
  }
  if (!hasSharedScope) {
    return { available: false, reason: "当前账号暂无团队或组织知识权限", identity };
  }
  if (session.organizationMemoryEnabled !== true) {
    return { available: false, reason: "组织服务不可连接或登录已过期", identity };
  }
  return {
    available: true,
    reason: "使用组织账号中你有权限的知识",
    identity,
  };
}

/**
 * Application-wide organization identity.
 *
 * Native code remains authoritative for credentials and access tokens. This
 * store only mirrors the safe session view returned by `org_session`, so the
 * shell (not just the organization page) can render the signed-in identity.
 */
export const useOrgSessionStore = create<OrgSessionState>((set, get) => ({
  session: null,
  hydrated: false,
  boundaryRevision: 0,

  hydrate: async () => {
    if (get().hydrated) return get().session;
    if (hydrationInFlight) return hydrationInFlight;

    const mutation = sessionMutation;
    const pending = (async () => {
      try {
        const session = await orgSession();
        if (mutation !== sessionMutation) return get().session;
        set((state) => ({
          session,
          hydrated: true,
          boundaryRevision: state.boundaryRevision + Number(
            organizationSessionBoundary(state.session) !== organizationSessionBoundary(session),
          ),
        }));
        return session;
      } catch {
        if (mutation !== sessionMutation) return get().session;
        // Organization connectivity must never block the local desktop shell.
        // The organization page retries and surfaces the detailed error.
        set({ session: null, hydrated: true });
        return null;
      } finally {
        if (mutation === sessionMutation) hydrationInFlight = null;
      }
    })();
    hydrationInFlight = pending;
    return hydrationInFlight;
  },

  setSession: (session) => {
    sessionMutation += 1;
    hydrationInFlight = null;
    set((state) => ({
      session,
      hydrated: true,
      boundaryRevision: state.boundaryRevision + Number(
        organizationSessionBoundary(state.session) !== organizationSessionBoundary(session),
      ),
    }));
  },
  clearSession: () => {
    sessionMutation += 1;
    hydrationInFlight = null;
    set((state) => ({
      session: {
        loggedIn: false,
        serverUrl: state.session?.serverUrl,
        username: state.session?.username ?? state.session?.user?.username,
        requiresReauthentication: false,
      },
      hydrated: true,
      boundaryRevision: state.boundaryRevision + 1,
    }));
  },
}));

/** Test/support reset; does not touch native credentials. */
export function resetOrgSessionMirror(): void {
  hydrationInFlight = null;
  sessionMutation += 1;
  useOrgSessionStore.setState({ session: null, hydrated: false, boundaryRevision: 0 });
}
