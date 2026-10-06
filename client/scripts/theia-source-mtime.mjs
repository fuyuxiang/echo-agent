import { readdirSync, statSync } from "node:fs";
import { join, resolve } from "node:path";

const skipped = new Set(["node_modules", "lib", "src-gen", ".git", ".nx", ".browser_modules", "plugins"]);

export function latestTheiaSourceMtime(sourceRoot) {
  const projectRoot = resolve(sourceRoot, "../..");
  let latest = 0;
  const visit = (path) => {
    const stat = statSync(path);
    if (!stat.isDirectory()) {
      latest = Math.max(latest, stat.mtimeMs);
      return;
    }
    for (const entry of readdirSync(path, { withFileTypes: true })) {
      if (skipped.has(entry.name) || entry.name.endsWith(".tsbuildinfo")) continue;
      if (entry.isFile() || entry.isDirectory()) visit(join(path, entry.name));
    }
  };
  for (const path of [
    join(sourceRoot, "package.json"),
    join(sourceRoot, "package-lock.json"),
    join(sourceRoot, "packages"),
    join(sourceRoot, "examples/browser"),
    join(sourceRoot, "examples/echo-coding-bridge"),
    join(projectRoot, "vendor/theia-language-plugins"),
    join(projectRoot, "scripts/build-theia.mjs"),
    join(projectRoot, "scripts/stage-theia-runtime.mjs"),
    join(projectRoot, "scripts/patch-theia-node-pty.mjs"),
    join(projectRoot, "scripts/patch-theia-windowless-helpers.mjs"),
    join(projectRoot, "scripts/theia-runtime-package-lock.json"),
    join(projectRoot, "vendor/nodejs"),
  ]) visit(path);
  return latest;
}
