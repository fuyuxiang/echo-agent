// Compile the same locked dependencies that failed during Windows packaging.
// The caller installs the real IDE workspace with npm ci --ignore-scripts first.
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { join, resolve } from "node:path";
import { spawnSync } from "node:child_process";
import { assertSupportedNodeVersion } from "./node-runtime-version.mjs";
import { patchNodePtyConsoleHelper, patchNodePtyPipeErrors, ensureNodePtyConptySourceUnmodified,
  syncNodePtyPrebuilds, markNodePtyConptyRebuilt } from "./patch-theia-node-pty.mjs";

assert.equal(process.platform, "win32", "Native Windows addon validation requires Windows");
assert.equal(process.arch, "x64", "The Windows installer requires x64 Node.js");
assertSupportedNodeVersion();
assert.match(process.env.VisualStudioVersion || "", /^17\./, "Initialize the VS 2022 environment first");
assert.equal(process.env.npm_package_config_node_gyp_msvs_version, "2022", "Use the packaging node-gyp configuration");
const root = resolve(import.meta.dirname, "..");
const source = join(root, "vendor/theia-platform");
const require = createRequire(join(source, "package.json"));
const nodeGyp = require.resolve("node-gyp/bin/node-gyp.js");
function run(args) {
  const result = spawnSync(process.execPath, args, { cwd: source, stdio: "inherit", windowsHide: true, timeout: 300_000 });
  if (result.error) throw result.error;
  assert.equal(result.status, 0, `Native addon command failed: ${args.join(" ")}`);
}

patchNodePtyConsoleHelper(source);
patchNodePtyPipeErrors(source);
ensureNodePtyConptySourceUnmodified(source);
for (const name of ["drivelist", "node-pty"]) {
  run([nodeGyp, "rebuild", "--directory", join(source, "node_modules", name)]);
}
// npm ci --ignore-scripts also skipped the step that stages the ConPTY DLLs.
run([join(source, "node_modules/node-pty/scripts/post-install.js")]);
syncNodePtyPrebuilds(source);
markNodePtyConptyRebuilt(source);

let timeout;
try {
  const drives = await Promise.race([require("drivelist").list(), new Promise((_, reject) => {
    timeout = setTimeout(() => reject(new Error("Native drive enumeration timed out")), 15_000);
  })]);
  assert.ok(Array.isArray(drives), "Native drive enumeration did not return a list");
} finally { clearTimeout(timeout); }
run([join(root, "scripts/validate-theia-pty.mjs"), source]);
console.log("Locked Windows native addons compiled successfully; drive enumeration and ConPTY lifecycle passed.");
