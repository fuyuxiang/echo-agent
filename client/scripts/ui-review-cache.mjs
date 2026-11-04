import { mkdirSync, mkdtempSync, readdirSync, rmSync } from "node:fs";
import { createHash } from "node:crypto";
import { join, resolve } from "node:path";

// Reclaim only this helper's process-owned caches. A live process, an unknown
// name, a symlink or an inaccessible process must never be removed by a peer.
export function pruneAbandonedUiReviewCaches(root) {
  for (const entry of readdirSync(root, { withFileTypes: true })) {
    const owner = entry.name.match(/^([1-9]\d*)-(?:\d+-[a-f0-9]{12}|[a-f0-9]{12}-[A-Za-z0-9]{6})$/);
    if (!entry.isDirectory() || !owner) continue;
    const pid = Number(owner[1]);
    if (!Number.isSafeInteger(pid) || pid > 0x7fffffff) continue;
    try { process.kill(pid, 0); }
    catch (error) {
      if (error.code === "ESRCH") rmSync(join(root, entry.name), { recursive: true, force: true });
    }
  }
}

export function createUiReviewCache(fixture, root = resolve("node_modules/.vite/ui-review")) {
  mkdirSync(root, { recursive: true });
  pruneAbandonedUiReviewCaches(root);
  const digest = createHash("sha256").update(fixture).digest("hex").slice(0, 12);
  const directory = mkdtempSync(join(root, `${process.pid}-${digest}-`));
  return { directory, close: () => rmSync(directory, { recursive: true, force: true }) };
}
