/** Use the ordinary spelling for display and comparison; keep the native grant unchanged. */
export function normalizeWorkspacePath(path: string): string {
  const slashed = path.replaceAll("\\", "/");
  const disk = /^\/\/\?\/([A-Za-z]:\/.*)$/.exec(slashed);
  if (disk) return disk[1];
  const unc = /^\/\/\?\/UNC\/(.+)$/i.exec(slashed);
  if (unc) return `//${unc[1]}`;
  return slashed;
}

/** Compare paths when Windows paths may use either spelling. */
export function relativeToWorkspace(root: string, path: string): string | null {
  const rootPath = normalizeWorkspacePath(root);
  const normalizedRoot = rootPath === "/" || /^[A-Za-z]:\/$/.test(rootPath)
    ? rootPath : rootPath.replace(/\/+$/, "");
  const candidatePath = normalizeWorkspacePath(path);
  const normalizedPath = candidatePath === "/" || /^[A-Za-z]:\/$/.test(candidatePath)
    ? candidatePath : candidatePath.replace(/\/+$/, "");
  if (!normalizedRoot || normalizedPath.split("/").includes("..")) return null;
  const windows = /^[A-Za-z]:\//.test(normalizedRoot) || /^\/\/[^/]+\/[^/]+/.test(normalizedRoot);
  const compareRoot = windows ? normalizedRoot.toLowerCase() : normalizedRoot;
  const comparePath = windows ? normalizedPath.toLowerCase() : normalizedPath;
  if (comparePath === compareRoot) return "";
  const prefix = compareRoot.endsWith("/") ? compareRoot : `${compareRoot}/`;
  return comparePath.startsWith(prefix)
    ? normalizedPath.slice(prefix.length)
    : null;
}
