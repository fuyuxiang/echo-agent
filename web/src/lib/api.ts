const API_BASE = "/api/v1";

export const TOKEN_STORAGE_KEY = "echo_token";

/** Single read path for the stored token — callers that bypass apiFetch
 *  (multipart upload, WS handshake) must use this instead of reaching into
 *  localStorage with their own key string. */
export function getToken(): string {
  return localStorage.getItem(TOKEN_STORAGE_KEY) || "";
}

export async function apiFetch<T>(path: string, options: RequestInit = {}): Promise<T> {
  const resp = await fetch(`${API_BASE}${path}`, {
    ...options,
    headers: {
      "Content-Type": "application/json",
      Authorization: `Bearer ${getToken()}`,
      ...options.headers,
    },
  });
  if (resp.status === 401) {
    localStorage.removeItem("echo_token");
    window.location.href = "/login";
    throw new Error("Unauthorized");
  }
  if (!resp.ok) {
    const err = await resp.json().catch(() => ({ error: resp.statusText }));
    throw new Error(err.error || resp.statusText);
  }
  return resp.json();
}
