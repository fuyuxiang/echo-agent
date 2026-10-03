import { cpSync, existsSync, mkdirSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { spawnSync } from "node:child_process";
import { latestTheiaSourceMtime } from "./theia-source-mtime.mjs";
import { ensureTheiaNodeWritable } from "./ensure-theia-node-writable.mjs";
import { patchNodePtyConsoleHelper, patchNodePtyPipeErrors, ensureNodePtyConptySourceUnmodified, syncNodePtyPrebuilds, markNodePtyConptyRebuilt } from "./patch-theia-node-pty.mjs";

const projectRoot = resolve(import.meta.dirname, "..");
const sourceRoot = join(projectRoot, "vendor/theia-platform");
const appRoot = join(sourceRoot, "examples/browser");
const resourcesRoot = join(projectRoot, "src-tauri/resources/theia");
const runtimeRoot = join(resourcesRoot, "browser");
const app = JSON.parse(readFileSync(join(appRoot, "package.json"), "utf8"));
const languagePlugins = [
  ...Object.entries(app.theiaPlugins ?? {}).map(([id, url]) => ({
    id, source: join(sourceRoot, "plugins", id),
    manifestDir: "extension",
    version: url.match(/\/file\/[^/]+-([0-9][^-]+)\.vsix$/)?.[1],
  })),
  {
    id: "echoagent.toml-basics",
    source: join(projectRoot, "vendor/theia-language-plugins/echoagent.toml-basics"),
    manifestDir: "",
    version: "1.0.0",
  },
].map(({ id, source, manifestDir, version }) => {
  const manifestRoot = join(source, manifestDir);
  let manifest;
  try { manifest = JSON.parse(readFileSync(join(manifestRoot, "package.json"), "utf8")); } catch {
    throw new Error(`Language plugin ${id} is missing. Run pnpm ide:build first.`);
  }
  if (`${manifest.publisher}.${manifest.name}`.toLowerCase() !== id.toLowerCase()
      || manifest.version !== version
      || !manifest.contributes?.languages?.length || !manifest.contributes?.grammars?.length) {
    throw new Error(`Language plugin ${id} is incomplete or has the wrong version.`);
  }
  for (const grammar of manifest.contributes.grammars) {
    if (!existsSync(join(manifestRoot, grammar.path))) {
      throw new Error(`Language plugin ${id} is missing grammar ${grammar.path}.`);
    }
  }
  return { id, source };
});
const runtimeFile = (path) => !path.endsWith(".map") && !path.endsWith(".d.ts") && !path.endsWith(".tsbuildinfo");
const copyUnlessSameFile = (source, target) => {
  if (existsSync(target) && realpathSync(source) === realpathSync(target)) return;
  cpSync(source, target);
};
const nodeMajor = Number(process.versions.node.split(".")[0]);
if (nodeMajor !== 22 && nodeMajor !== 24) {
  throw new Error("Echo Code IDE runtime is validated with Node.js 22 or 24.");
}

if (!existsSync(join(appRoot, "lib/backend/main.js"))) {
  throw new Error("Build Theia first: pnpm ide:build");
}

const localPackages = new Map();
for (const group of ["packages", "examples"]) {
  const { readdirSync } = await import("node:fs");
  for (const name of readdirSync(join(sourceRoot, group))) {
    const dir = join(sourceRoot, group, name);
    const manifest = join(dir, "package.json");
    if (!existsSync(manifest)) continue;
    const pkg = JSON.parse(readFileSync(manifest, "utf8"));
    localPackages.set(pkg.name, { dir, group, name, pkg });
  }
}

const needed = new Set();
const queue = Object.keys(app.dependencies ?? {});
while (queue.length) {
  const name = queue.pop();
  if (!name || needed.has(name) || !localPackages.has(name)) continue;
  needed.add(name);
  const pkg = localPackages.get(name).pkg;
  queue.push(...Object.keys(pkg.dependencies ?? {}), ...Object.keys(pkg.optionalDependencies ?? {}));
}

rmSync(runtimeRoot, { recursive: true, force: true });
mkdirSync(runtimeRoot, { recursive: true });
writeFileSync(join(runtimeRoot, ".keep"), "");
cpSync(join(appRoot, "lib"), join(runtimeRoot, "lib"), { recursive: true, filter: runtimeFile });
const pluginsTarget = join(runtimeRoot, "plugins");
mkdirSync(pluginsTarget, { recursive: true });
for (const { id, source } of languagePlugins) {
  cpSync(source, join(pluginsTarget, id), { recursive: true, filter: runtimeFile });
}

const stagedManifest = {
  ...app,
  name: "@echoagent/theia-runtime",
  private: true,
  workspaces: ["packages/*", "examples/*"],
  theiaPluginsDir: "plugins",
  scripts: {},
  devDependencies: {},
};
writeFileSync(join(runtimeRoot, "package.json"), `${JSON.stringify(stagedManifest, null, 2)}\n`);
cpSync(join(projectRoot, "scripts/theia-runtime-package-lock.json"), join(runtimeRoot, "package-lock.json"));

for (const name of needed) {
  const entry = localPackages.get(name);
  const target = join(runtimeRoot, entry.group, entry.name);
  mkdirSync(target, { recursive: true });
  cpSync(entry.dir, target, {
    recursive: true,
    filter: (path) => {
      if (!runtimeFile(path)) return false;
      const relative = path.slice(entry.dir.length).replace(/^[/\\]/, "");
      const first = relative.split(/[/\\]/)[0];
      return !["node_modules", ".git", ".nx", "src", "src-gen", "test", "tests"].includes(first);
    },
  });
  const runtimePackage = { ...entry.pkg, scripts: {}, devDependencies: {} };
  writeFileSync(join(target, "package.json"), `${JSON.stringify(runtimePackage, null, 2)}\n`);
}

const npm = process.platform === "win32" ? "npm.cmd" : "npm";
const npmShell = process.platform === "win32";
const install = spawnSync(npm, ["ci", "--omit=dev", "--no-audit", "--no-fund"], {
  cwd: runtimeRoot,
  env: { ...process.env, PUPPETEER_SKIP_DOWNLOAD: "1" },
  stdio: "inherit",
  shell: npmShell,
});
if (install.status !== 0) throw new Error("Theia runtime dependency staging failed");
patchNodePtyConsoleHelper(runtimeRoot);
if (process.platform === "win32") {
  patchNodePtyPipeErrors(runtimeRoot);
  ensureNodePtyConptySourceUnmodified(runtimeRoot);
  const rebuild = spawnSync(npm, ["rebuild", "node-pty", "--build-from-source", "--no-audit", "--no-fund"], {
    cwd: runtimeRoot,
    env: { ...process.env, npm_config_build_from_source: "true" },
    stdio: "inherit",
    shell: npmShell,
  });
  if (rebuild.status !== 0) throw new Error("Theia Windows ConPTY native rebuild failed");
  syncNodePtyPrebuilds(runtimeRoot);
  markNodePtyConptyRebuilt(runtimeRoot);
}

const nodeTarget = process.platform === "win32"
  ? join(resourcesRoot, "node/node.exe")
  : join(resourcesRoot, "node/bin/node");
mkdirSync(resolve(nodeTarget, ".."), { recursive: true });
ensureTheiaNodeWritable(projectRoot, nodeTarget);
copyUnlessSameFile(process.execPath, nodeTarget);
ensureTheiaNodeWritable(projectRoot, nodeTarget);
const nodeDir = dirname(realpathSync(process.execPath));
const licenseCandidates = [
  join(nodeDir, "LICENSE"),
  join(nodeDir, "../LICENSE"),
  join(nodeDir, `../node_modules/node-${process.platform}-${process.arch}/LICENSE`),
  join(nodeDir, "../share/doc/node/LICENSE"),
];
if (process.version === "v24.21.0") {
  licenseCandidates.push(join(projectRoot, "vendor/nodejs/LICENSE-24.21.0"));
}
const nodeLicense = licenseCandidates.find(existsSync);
if (!nodeLicense) throw new Error("The bundled Node.js runtime needs its LICENSE file.");
copyUnlessSameFile(nodeLicense, join(resourcesRoot, "node/LICENSE"));
writeFileSync(join(resourcesRoot, "node/.keep"), "");
writeFileSync(join(resourcesRoot, "runtime-platform.json"), JSON.stringify({
  platform: process.platform,
  arch: process.arch,
  node: process.version,
  sourceMtimeMs: latestTheiaSourceMtime(sourceRoot),
}, null, 2));

console.log(`Staged ${needed.size} local Theia packages, ${languagePlugins.length} language plugins, and Node.js ${process.version} for ${process.platform}/${process.arch}.`);
