import { cpSync, existsSync, mkdirSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { spawnSync } from "node:child_process";
import { theiaBuildStampPath, theiaSourceFingerprint } from "./theia-source-fingerprint.mjs";
import { ensureTheiaNodeWritable } from "./ensure-theia-node-writable.mjs";
import { patchNodePtyConsoleHelper, patchNodePtyPipeErrors, ensureNodePtyConptySourceUnmodified, syncNodePtyPrebuilds, markNodePtyConptyRebuilt } from "./patch-theia-node-pty.mjs";
import { isTheiaWindowlessRuntime } from "./patch-theia-windowless-helpers.mjs";
import { createHash } from "node:crypto";
import { pinnedTheiaPlugins, validateTheiaRuntimePlugin, theiaPluginTreeFingerprint } from "./theia-plugin-validation.mjs";
import { GIT_WINDOWLESS_PATCH } from "./patch-theia-git-windowless.mjs";
import { createTheiaRuntimeManifest } from "./theia-runtime-manifest.mjs";
import { assertSupportedNodeVersion, assertSupportedNpmVersion } from "./node-runtime-version.mjs";
import { patchTheiaScanoss } from "./patch-theia-scanoss.mjs";

const projectRoot = resolve(import.meta.dirname, "..");
const sourceRoot = join(projectRoot, "vendor/theia-platform");
const appRoot = join(sourceRoot, "examples/browser");
const resourcesRoot = join(projectRoot, "src-tauri/resources/theia");
const runtimeRoot = join(resourcesRoot, "browser");
const platformFile = join(resourcesRoot, "runtime-platform.json");
assertSupportedNodeVersion();
const buildStamp = theiaBuildStampPath(sourceRoot);
const sourceFingerprint = theiaSourceFingerprint(sourceRoot);
if (!existsSync(buildStamp) || readFileSync(buildStamp, "utf8").trim() !== sourceFingerprint
    || !existsSync(join(appRoot, "lib/backend/main.js"))) {
  throw new Error("Theia build is missing or out of date; run pnpm ide:build before staging.");
}
const app = JSON.parse(readFileSync(join(appRoot, "package.json"), "utf8"));
const pluginSpecs = pinnedTheiaPlugins(app);
const runtimePlugins = [
  ...pluginSpecs.map(plugin => ({ ...plugin, source: join(sourceRoot, "plugins", plugin.id) })),
  {
    id: "echoagent.toml-basics",
    source: join(projectRoot, "vendor/theia-language-plugins/echoagent.toml-basics"),
    manifestDir: "",
    version: "1.0.0",
  },
].map(({ id, source, manifestDir, version }) => {
  validateTheiaRuntimePlugin(source, { id, version, manifestDir }, [...pluginSpecs.map(plugin => plugin.id), "echoagent.toml-basics"]);
  return { id, source };
});
const runtimeFile = (path) => !path.endsWith(".map") && !path.endsWith(".d.ts") && !path.endsWith(".tsbuildinfo");
const copyUnlessSameFile = (source, target) => {
  if (existsSync(target) && realpathSync(source) === realpathSync(target)) return;
  cpSync(source, target);
};
const npm = process.platform === "win32" ? "npm.cmd" : "npm";
const npmShell = process.platform === "win32";
const npmVersion = spawnSync(npm, ["--version"], { encoding: "utf8", shell: npmShell });
if (npmVersion.status !== 0) throw new Error(npmVersion.stderr || "npm is unavailable");
assertSupportedNpmVersion(npmVersion.stdout);

if (!isTheiaWindowlessRuntime(sourceRoot)) {
  throw new Error("Theia background workers are not windowless; run pnpm ide:build.");
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

rmSync(platformFile, { force: true });
rmSync(runtimeRoot, { recursive: true, force: true });
mkdirSync(runtimeRoot, { recursive: true });
writeFileSync(join(runtimeRoot, ".keep"), "");
cpSync(join(appRoot, "lib"), join(runtimeRoot, "lib"), { recursive: true, filter: runtimeFile });
const pluginsTarget = join(runtimeRoot, "plugins");
mkdirSync(pluginsTarget, { recursive: true });
for (const { id, source } of runtimePlugins) {
  cpSync(source, join(pluginsTarget, id), { recursive: true, filter: runtimeFile });
  validateTheiaRuntimePlugin(join(pluginsTarget, id),
    [...pluginSpecs, { id: "echoagent.toml-basics", version: "1.0.0", manifestDir: "" }].find(plugin => plugin.id === id),
    runtimePlugins.map(plugin => plugin.id));
}

const stagedManifest = createTheiaRuntimeManifest(app, JSON.parse(readFileSync(join(sourceRoot, "package.json"), "utf8")));
writeFileSync(join(runtimeRoot, "package.json"), `${JSON.stringify(stagedManifest, null, 2)}\n`);
cpSync(join(sourceRoot, "security"), join(runtimeRoot, "security"), { recursive: true, filter: runtimeFile });
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
if (!isTheiaWindowlessRuntime(runtimeRoot)) {
  throw new Error("Staged Theia background workers lost their windowless spawn options.");
}

const install = spawnSync(npm, ["ci", "--omit=dev", "--no-audit", "--no-fund"], {
  cwd: runtimeRoot,
  env: { ...process.env, PUPPETEER_SKIP_DOWNLOAD: "1" },
  stdio: "inherit",
  shell: npmShell,
});
if (install.status !== 0) throw new Error("Theia runtime dependency staging failed");
patchTheiaScanoss(runtimeRoot);
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
licenseCandidates.push(join(projectRoot, `vendor/nodejs/LICENSE-${process.version.slice(1)}`));
const nodeLicense = licenseCandidates.find(existsSync);
if (!nodeLicense) throw new Error("The bundled Node.js runtime needs its LICENSE file.");
copyUnlessSameFile(nodeLicense, join(resourcesRoot, "node/LICENSE"));
writeFileSync(join(resourcesRoot, "node/.keep"), "");
if (theiaSourceFingerprint(sourceRoot) !== sourceFingerprint) {
  throw new Error("Theia sources changed during staging; run pnpm ide:prepare again.");
}
writeFileSync(platformFile, JSON.stringify({
  platform: process.platform,
  arch: process.arch,
  node: process.version,
  sourceFingerprint,
  pluginManifestSha256: createHash("sha256").update(readFileSync(join(projectRoot, "vendor/theia-vsix/manifest.json"))).digest("hex"),
  gitWindowlessPatch: GIT_WINDOWLESS_PATCH,
  pluginTreeSha256: theiaPluginTreeFingerprint(pluginsTarget, runtimePlugins.map(plugin => plugin.id)),
}, null, 2));

console.log(`Staged ${needed.size} local Theia packages, ${runtimePlugins.length} IDE plugins (including Git and Git Base), and Node.js ${process.version} for ${process.platform}/${process.arch}.`);
