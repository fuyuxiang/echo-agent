import { readdirSync, statSync } from "node:fs";
import { join, resolve } from "node:path";

const skipped = new Set(["node_modules", "lib", "src-gen", ".git", ".nx", ".browser_modules", "plugins"]);

export function latestTheiaSourceMtime(sourceRoot) {
  const projectRoot = resolve(sourceRoot, "../..");
  let latest = 0;
  const visit = (path, includeSourceLib = false) => {
    const stat = statSync(path);
    if (!stat.isDirectory()) {
      latest = Math.max(latest, stat.mtimeMs);
      return;
    }
    for (const entry of readdirSync(path, { withFileTypes: true })) {
      if ((skipped.has(entry.name) && !(includeSourceLib && entry.name === "lib")) || entry.name.endsWith(".tsbuildinfo")) continue;
      if (entry.isFile() || entry.isDirectory()) visit(join(path, entry.name), includeSourceLib);
    }
  };
  for (const path of [
    join(sourceRoot, "package.json"),
    join(sourceRoot, "package-lock.json"),
    join(sourceRoot, "lerna.json"),
    join(sourceRoot, "configs"),
    join(sourceRoot, "scripts/compile-references.js"),
    join(sourceRoot, "packages"),
    join(sourceRoot, "dev-packages"),
    join(sourceRoot, "security"),
    join(sourceRoot, "examples/browser"),
    join(sourceRoot, "examples/echo-coding-bridge"),
    join(projectRoot, "vendor/theia-language-plugins"),
    join(projectRoot, "scripts/build-theia.mjs"),
    join(projectRoot, "scripts/prepare-theia.mjs"),
    join(projectRoot, "scripts/stage-theia-runtime.mjs"),
    join(projectRoot, "scripts/theia-source-mtime.mjs"),
    join(projectRoot, "scripts/theia-runtime-manifest.mjs"),
    join(projectRoot, "scripts/node-runtime-version.mjs"),
    join(projectRoot, "scripts/patch-theia-scanoss.mjs"),
    join(projectRoot, "scripts/patch-theia-node-pty.mjs"),
    join(projectRoot, "scripts/patch-theia-windowless-helpers.mjs"),
    join(projectRoot, "scripts/patch-theia-git-windowless.mjs"),
    join(projectRoot, "scripts/manage-theia-vsix.mjs"),
    join(projectRoot, "scripts/theia-plugin-validation.mjs"),
    join(projectRoot, "scripts/theia-runtime-package-lock.json"),
    join(projectRoot, "vendor/nodejs"),
  ]) visit(path, path === join(sourceRoot, "security"));
  return latest;
}
