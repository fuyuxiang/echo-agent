import { copyFileSync, existsSync, readFileSync, writeFileSync } from "node:fs";
import { createHash } from "node:crypto";
import { resolve, join } from "node:path";
import { spawnSync } from "node:child_process";
import { patchNodePtyConsoleHelper, patchNodePtyPipeErrors, ensureNodePtyConptySourceUnmodified, syncNodePtyPrebuilds, markNodePtyConptyRebuilt } from "./patch-theia-node-pty.mjs";
import { patchTheiaWindowlessHelpers, isTheiaWindowlessRuntime } from "./patch-theia-windowless-helpers.mjs";

const major = Number(process.versions.node.split(".")[0]);
if (major !== 22 && major !== 24) {
  throw new Error("Echo Code IDE is validated with Node.js 22 or 24. Run this command with one of those versions on PATH.");
}

const sourceRoot = resolve(import.meta.dirname, "../vendor/theia-platform");
const npm = process.platform === "win32" ? "npm.cmd" : "npm";
const env = { ...process.env, PUPPETEER_SKIP_DOWNLOAD: "1" };

function run(args, extraEnv = {}) {
  const result = spawnSync(npm, args, {
    cwd: sourceRoot, env: { ...env, ...extraEnv }, stdio: "inherit",
    shell: process.platform === "win32",
  });
  if (result.status !== 0) {
    throw new Error(`Theia build failed: npm ${args.join(" ")}`);
  }
}

const nodeModules = join(sourceRoot, "node_modules");
const installMarker = join(nodeModules, ".echoagent-install-fingerprint");
const installFingerprint = createHash("sha256")
  .update(readFileSync(join(sourceRoot, "package.json")))
  .update(readFileSync(join(sourceRoot, "package-lock.json")))
  .update(`${process.platform}/${process.arch}/${process.version}/${process.versions.modules}`)
  .digest("hex");
let installedFingerprint;
try { installedFingerprint = readFileSync(installMarker, "utf8").trim(); } catch { /* Missing install. */ }
if (!existsSync(nodeModules) || installedFingerprint !== installFingerprint) {
  run(["ci"]);
  writeFileSync(installMarker, `${installFingerprint}\n`);
}
patchNodePtyConsoleHelper(sourceRoot);
patchTheiaWindowlessHelpers(sourceRoot);
if (process.platform === "win32") {
  patchNodePtyPipeErrors(sourceRoot);
  ensureNodePtyConptySourceUnmodified(sourceRoot);
  run(["rebuild", "node-pty", "--build-from-source"], { npm_config_build_from_source: "true" });
  syncNodePtyPrebuilds(sourceRoot);
}
const plugins = spawnSync(process.execPath, [join(import.meta.dirname, "manage-theia-vsix.mjs"), "stage"], {
  cwd: sourceRoot, env, stdio: "inherit",
});
if (plugins.status !== 0) throw new Error("Failed to stage vendored Theia VSIX plugins.");
// The source snapshot omits generated @theia/core/shared re-export shims.
// Filesystem and other packages import these during TypeScript compilation.
run(["exec", "--", "theia-re-exports", "generate", "@theia/core"]);
run(["run", "compile"]);
if (!isTheiaWindowlessRuntime(sourceRoot)) {
  throw new Error("Theia background workers were compiled without windowless Windows spawn options.");
}
// The vendored source keeps the pinned JSON Schema catalog as a static asset;
// TypeScript does not copy JSON inputs to lib/. The browser bundle requires it.
copyFileSync(
  join(sourceRoot, "packages/core/src/browser/catalog.json"),
  join(sourceRoot, "packages/core/lib/browser/catalog.json"),
);
run(["run", "build:production", "--workspace", "@echoagent/theia-browser"]);
if (process.platform === "win32") markNodePtyConptyRebuilt(sourceRoot);

if (!existsSync(join(sourceRoot, "examples/browser/lib/backend/main.js"))) {
  throw new Error("Theia backend entry point was not produced by the build.");
}
console.log("Echo Code IDE built from vendored Theia source.");
