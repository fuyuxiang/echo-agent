let API_BASE = "/api/v1";

export interface MetaInfo {
  api_prefix: string;
  ws_path: string;
  version: string;
  auth_required: boolean;
}

let _meta: MetaInfo | null = null;

export function getMeta(): MetaInfo | null {
  return _meta;
}

export async function initApiBase(): Promise<void> {
  try {
    const resp = await fetch("/meta");
    if (resp.ok) {
      _meta = await resp.json();
      if (_meta?.api_prefix) API_BASE = _meta.api_prefix;
    }
  } catch {
    // Fallback to default /api/v1 — server may be too old to serve /meta.
  }
}

export const TOKEN_STORAGE_KEY = "echo_token";

/** Where to send the user back after re-authenticating. Session-scoped: a
 *  stale return target from a previous browser session is not useful. */
export const RETURN_TO_STORAGE_KEY = "echo_return_to";

/** Single read path for the stored token — callers that bypass apiFetch
 *  (multipart upload, WS handshake) must use this instead of reaching into
 *  localStorage with their own key string. */
export function getToken(): string {
  return localStorage.getItem(TOKEN_STORAGE_KEY) || "";
}

/**
 * Called on a 401 to drop the session. Assigned by the auth store at module
 * load, so api.ts does not import the store (which imports api.ts back).
 */
let onUnauthorized: (() => void) | null = null;

// A token can be removed and then reused while a request is in flight. Keep a
// generation as well as the token so responses from that earlier login expire.
let authGeneration = 0;
export function invalidateApiRequests() {
  authGeneration += 1;
}

function requestIdentity() {
  const token = getToken();
  const generation = authGeneration;
  return {
    token,
    assertCurrent() {
      if (generation !== authGeneration || token !== getToken()) {
        throw new DOMException("Authentication changed", "AbortError");
      }
    },
  };
}

export function setUnauthorizedHandler(fn: () => void) {
  onUnauthorized = fn;
}

/** Shared by HTTP and the app's persistent dashboard socket. */
export function handleUnauthorized() {
  if (location.pathname !== "/login") {
    sessionStorage.setItem(RETURN_TO_STORAGE_KEY, location.pathname + location.search);
  }
  onUnauthorized?.();
}

export async function apiFetch<T>(path: string, options: RequestInit = {}): Promise<T> {
  const identity = requestIdentity();
  const resp = await fetch(`${API_BASE}${path}`, {
    ...options,
    headers: {
      "Content-Type": "application/json",
      Authorization: `Bearer ${identity.token}`,
      ...options.headers,
    },
  });
  identity.assertCurrent();
  if (resp.status === 401) {
    // Clear the session through the store rather than assigning to
    // window.location: a hard navigation reloads the whole SPA and only
    // happened to leave zustand consistent because the store re-reads
    // localStorage on init. Clearing state lets Layout's <Navigate> route to
    // /login in-app, and remembering the current location makes the trip back
    // land where the user was instead of always on the overview.
    handleUnauthorized();
    throw new Error("Unauthorized");
  }
  if (!resp.ok) {
    const err = await resp.json().catch(() => ({ error: resp.statusText }));
    identity.assertCurrent();
    throw new Error(err.error || resp.statusText);
  }
  const data = await resp.json();
  identity.assertCurrent();
  return data;
}

export async function apiUpload<T = unknown>(path: string, body: FormData): Promise<T> {
  const identity = requestIdentity();
  const resp = await fetch(`${API_BASE}${path}`, {
    method: "POST",
    headers: { Authorization: `Bearer ${identity.token}` },
    body,
  });
  identity.assertCurrent();
  if (resp.status === 401) {
    handleUnauthorized();
    throw new Error("Unauthorized");
  }
  if (!resp.ok) {
    const err = await resp.json().catch(() => ({ error: resp.statusText }));
    identity.assertCurrent();
    throw new Error(err.error || resp.statusText);
  }
  const data = await resp.json();
  identity.assertCurrent();
  return data;
}

/** Download with the same header authentication as the rest of the dashboard. */
export async function apiDownload(path: string): Promise<Blob> {
  const identity = requestIdentity();
  const resp = await fetch(`${API_BASE}${path}`, { headers: { Authorization: `Bearer ${identity.token}` } });
  identity.assertCurrent();
  if (resp.status === 401) {
    handleUnauthorized();
    throw new Error("Unauthorized");
  }
  if (!resp.ok) {
    const error = await resp.json().catch(() => ({ error: resp.statusText }));
    identity.assertCurrent();
    throw new Error(error.error || resp.statusText);
  }
  const data = await resp.blob();
  identity.assertCurrent();
  return data;
}
