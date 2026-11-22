import { existsSync, readFileSync } from "node:fs";
import { resolve, join } from "node:path";
import { spawnSync } from "node:child_process";
import { theiaSourceFingerprint } from "./theia-source-fingerprint.mjs";
import { ensureTheiaNodeWritable } from "./ensure-theia-node-writable.mjs";
import { isNodePtyConsoleHelperHidden, isNodePtyPipeErrorsHandled, isNodePtyConptyReady } from "./patch-theia-node-pty.mjs";
import { isTheiaWindowlessRuntime } from "./patch-theia-windowless-helpers.mjs";
import { createHash } from "node:crypto";
import { pinnedTheiaPlugins, validateTheiaRuntimePlugin, theiaPluginTreeFingerprint } from "./theia-plugin-validation.mjs";
import { GIT_WINDOWLESS_PATCH } from "./patch-theia-git-windowless.mjs";
import { assertSupportedNodeVersion } from "./node-runtime-version.mjs";

const root = resolve(import.meta.dirname, "..");
assertSupportedNodeVersion();
const resources = join(root, "src-tauri/resources/theia");
const runtime = join(resources, "browser/lib/backend/main.js");
const runtimeDependencies = [
  "browser/package-lock.json",
  "browser/node_modules/@theia/core/package.json",
  "browser/node_modules/inversify/package.json",
  "node/LICENSE",
];
const app = JSON.parse(readFileSync(join(root, "vendor/theia-platform/examples/browser/package.json"), "utf8"));
const pluginSpecs = [...pinnedTheiaPlugins(app), { id: "echoagent.toml-basics", version: "1.0.0", manifestDir: "" }];
const pluginIds = pluginSpecs.map(plugin => plugin.id);
let pluginsReady = false, pluginTreeSha256;
try {
  for (const plugin of pluginSpecs) validateTheiaRuntimePlugin(join(resources, "browser/plugins", plugin.id), plugin, pluginIds);
  pluginTreeSha256 = theiaPluginTreeFingerprint(join(resources, "browser/plugins"), pluginIds);
  pluginsReady = true;
} catch { /* Missing or damaged plugins require the ordinary prepare chain. */ }
const pluginManifestSha256 = createHash("sha256").update(readFileSync(join(root, "vendor/theia-vsix/manifest.json"))).digest("hex");
const node = process.platform === "win32"
  ? join(resources, "node/node.exe")
  : join(resources, "node/bin/node");
let platform;
try {
  platform = JSON.parse(readFileSync(join(resources, "runtime-platform.json"), "utf8"));
} catch {
  platform = null;
}
const stagedNodeVersion = existsSync(node)
  ? spawnSync(node, ["--version"], { encoding: "utf8", timeout: 5000 })
  : null;

if (existsSync(runtime) && existsSync(node)
    && isTheiaWindowlessRuntime(join(resources, "browser"))
    && runtimeDependencies.every(path => existsSync(join(resources, path)))
    && pluginsReady && platform?.pluginTreeSha256 === pluginTreeSha256
    && platform?.pluginManifestSha256 === pluginManifestSha256
    && platform?.gitWindowlessPatch === GIT_WINDOWLESS_PATCH
    && isNodePtyConsoleHelperHidden(join(resources, "browser"))
    && (process.platform !== "win32" || (
      isNodePtyPipeErrorsHandled(join(resources, "browser"))
      && isNodePtyConptyReady(join(resources, "browser"))
    ))
    && platform?.platform === process.platform && platform?.arch === process.arch
    && platform?.node === process.version
    && stagedNodeVersion?.status === 0 && stagedNodeVersion.stdout.trim() === process.version
    && platform?.sourceFingerprint === theiaSourceFingerprint(join(root, "vendor/theia-platform"))) {
  ensureTheiaNodeWritable(root, node);
  console.log("Echo Code IDE runtime is already staged.");
  process.exit(0);
}

const pnpm = process.platform === "win32" ? "pnpm.cmd" : "pnpm";
for (const script of ["ide:build", "ide:stage"]) {
  const result = spawnSync(pnpm, [script], {
    cwd: root, stdio: "inherit", shell: process.platform === "win32",
  });
  if (result.error) throw new Error(`${script} failed to start`, { cause: result.error });
  if (result.status !== 0) throw new Error(`${script} failed`);
}
