import { useEffect, useState } from "react";

import { buildFileIndex } from "@/lib/file-index";
import { isTauriAvailable } from "@/lib/tauri-kb-reader";

/**
 * Load the bounded workspace candidates shared by every Composer surface.
 * Results are abandoned on cwd changes so a slow repository never leaks its
 * paths into the next workspace.
 */
export function useWorkspaceMentions(cwd?: string, enabled = true): {
  filePaths: string[];
} {
  const [indexedRoot, setIndexedRoot] = useState<string>();
  const [filePaths, setFilePaths] = useState<string[]>([]);

  useEffect(() => {
    const root = cwd?.trim() ?? "";
    const signal = { aborted: false };
    setIndexedRoot(root);
    setFilePaths([]);
    if (!enabled || !root || !isTauriAvailable()) return () => { signal.aborted = true; };

    void buildFileIndex(root, {
      signal,
      onProgress: (paths) => {
        if (!signal.aborted) setFilePaths(paths);
      },
    }).then((result) => {
      if (!signal.aborted) setFilePaths(result.paths);
    }).catch(() => undefined);

    return () => { signal.aborted = true; };
  }, [cwd, enabled]);

  return enabled && indexedRoot === (cwd?.trim() ?? "") ? { filePaths } : { filePaths: [] };
}
