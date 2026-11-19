/** Display only. Never use this value for IO, authorization or comparison. */
export function displayPath(path: string): string {
  if (path.startsWith("\\\\?\\UNC\\")) return `\\\\${path.slice(8)}`;
  if (/^\\\\\?\\[a-z]:\\/i.test(path)) return path.slice(4);
  return path;
}

export function shortWorkspacePath(path: string): string {
  const visible = displayPath(path);
  return visible.replace(/\\/g, "/").split("/").filter(Boolean).slice(-2).join("/") || visible;
}

/** Match the managed task directory layout, including directories reused by another conversation. */
export function isManagedTaskWorkspace(cwd?: string, defaultCwd?: string): boolean {
  if (!cwd || !defaultCwd) return false;
  const root = displayPath(defaultCwd);
  const windows = /^(?:[a-z]:[\\/]|\\\\)/i.test(root);
  const normalize = (path: string) => (windows ? displayPath(path).replace(/\\/g, "/").toLowerCase() : path).replace(/\/+$/, "");
  const prefix = `${normalize(root)}/${windows ? "tasks" : "Tasks"}/`;
  const path = normalize(cwd);
  return path.startsWith(prefix) && /^[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}$/.test(path.slice(prefix.length));
}
