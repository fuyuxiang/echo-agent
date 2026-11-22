import { createHash } from "node:crypto";
import { lstatSync, readFileSync, readdirSync, rmSync } from "node:fs";
import { dirname, join, relative, resolve, sep } from "node:path";

const workspaceGroups = ["packages", "dev-packages", "examples"];
const generatedDirectories = new Set(["node_modules", ".git", ".nx", ".browser_modules"]);
const generatedWorkspaceEntries = new Set([
  "lib", "src-gen",
  "gen-webpack.config.js", "gen-webpack.node.config.js",
  "gen-esbuild.browser.mjs", "gen-esbuild.node.mjs", "gen-esbuild.electron.mjs",
]);

export function theiaBuildStampPath(sourceRoot) {
  return join(sourceRoot, "node_modules", ".echoagent-build-fingerprint");
}

export function clearTheiaBuildOutputs(sourceRoot) {
  for (const group of workspaceGroups) {
    const workspaceRoot = join(sourceRoot, group);
    for (const workspace of readdirSync(workspaceRoot, { withFileTypes: true })) {
      if (!workspace.isDirectory()) continue;
      const project = join(workspaceRoot, workspace.name);
      for (const entry of readdirSync(project, { withFileTypes: true })) {
        if (generatedWorkspaceEntries.has(entry.name) || (entry.isFile() && entry.name.endsWith(".tsbuildinfo"))) {
          rmSync(join(project, entry.name), { recursive: true, force: true });
        }
      }
    }
  }
}

export function theiaSourceFingerprint(sourceRoot) {
  const projectRoot = resolve(sourceRoot, "../..");
  const hash = createHash("sha256");
  const workspaceRoots = new Set(workspaceGroups.map(group => join(sourceRoot, group)));
  const visit = (path, isDirectory) => {
    if (isDirectory) {
      const isWorkspace = workspaceRoots.has(dirname(path));
      const entries = readdirSync(path, { withFileTypes: true });
      entries.sort((left, right) => {
        if (left.name === right.name) return 0;
        return left.name < right.name ? -1 : 1;
      });
      for (const entry of entries) {
        const { name } = entry;
        if (generatedDirectories.has(name) || (isWorkspace && generatedWorkspaceEntries.has(name)) || name.endsWith(".tsbuildinfo")) continue;
        if (entry.isFile() || entry.isDirectory()) visit(join(path, name), entry.isDirectory());
      }
    } else {
      const key = relative(projectRoot, path).split(sep).join("/");
      hash.update(JSON.stringify([key, createHash("sha256").update(readFileSync(path)).digest("hex")]));
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
    join(projectRoot, "scripts/theia-source-fingerprint.mjs"),
    join(projectRoot, "scripts/theia-runtime-manifest.mjs"),
    join(projectRoot, "scripts/node-runtime-version.mjs"),
    join(projectRoot, "scripts/patch-theia-scanoss.mjs"),
    join(projectRoot, "scripts/patch-theia-node-pty.mjs"),
    join(projectRoot, "scripts/patch-theia-windowless-helpers.mjs"),
    join(projectRoot, "scripts/patch-theia-git-windowless.mjs"),
    join(projectRoot, "scripts/manage-theia-vsix.mjs"),
    join(projectRoot, "scripts/theia-plugin-validation.mjs"),
    join(projectRoot, "scripts/theia-runtime-package-lock.json"),
    join(projectRoot, "vendor/theia-vsix/manifest.json"),
    join(projectRoot, "vendor/nodejs"),
  ]) visit(path, lstatSync(path).isDirectory());
  return hash.digest("hex");
}
