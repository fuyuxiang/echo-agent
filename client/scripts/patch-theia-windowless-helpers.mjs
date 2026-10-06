import { readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";

// These are background IPC workers, never interactive terminals. Keep their
// stdio pipes and IPC channel, but do not inherit or allocate a Windows console.
// The backend's kill-on-close Job owns detached children as well.
const helpers = [
  "packages/core/src/node/messaging/ipc-connection-provider.ts",
  "packages/plugin-ext/src/hosted/node/hosted-plugin-process.ts",
];
const runtimeHelpers = [
  "packages/core/lib/node/messaging/ipc-connection-provider.js",
  "packages/plugin-ext/lib/hosted/node/hosted-plugin-process.js",
];
const oldLine = "            windowsHide: process.platform === 'win32',";
const newLines = `${oldLine}
            // A background IPC worker must not acquire a Windows console.
            // The backend's Job still owns this child and reaps it on exit.
            detached: process.platform === 'win32',`;

function count(source, value) {
  return source.split(value).length - 1;
}

export function patchTheiaWindowlessHelpers(sourceRoot) {
  for (const relative of helpers) {
    const file = join(sourceRoot, relative);
    const source = readFileSync(file, "utf8");
    if (count(source, newLines) === 1 && count(source, oldLine) === 1
        && count(source, "detached: process.platform === 'win32'") === 1) continue;
    if (count(source, oldLine) !== 1
        || count(source, "detached: process.platform === 'win32'") !== 0) {
      throw new Error(`Theia background fork changed: ${relative}; review its Windows console behavior.`);
    }
    writeFileSync(file, source.replace(oldLine, newLines));
  }
}

export function isTheiaWindowlessRuntime(runtimeRoot) {
  try {
    return runtimeHelpers.every(relative => {
      const source = readFileSync(join(runtimeRoot, relative), "utf8");
      return count(source, "windowsHide: process.platform === 'win32'") === 1
        && count(source, "detached: process.platform === 'win32'") === 1
        && count(source, "cp.fork(") === 1;
    });
  } catch {
    return false;
  }
}
