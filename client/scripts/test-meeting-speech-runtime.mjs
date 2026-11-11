// Exercise the pinned native speech resources before building a full installer.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { copyFileSync, cpSync, mkdirSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

assert.equal(process.argv.length, 2, "This acceptance prepares and exercises the host speech worker");
const target = {
  "win32/x64": "x86_64-pc-windows-msvc",
  "darwin/x64": "x86_64-apple-darwin",
  "darwin/arm64": "aarch64-apple-darwin",
  "linux/x64": "x86_64-unknown-linux-gnu",
}[`${process.platform}/${process.arch}`];
assert.ok(target, `Unsupported native speech test platform: ${process.platform}/${process.arch}`);
const root = fileURLToPath(new URL("../", import.meta.url));
const project = join(root, "src-tauri");
const prepare = join(root, "scripts/prepare-meeting-speech.mjs");
function run(args) {
  const result = spawnSync(process.execPath, [prepare, ...args], {
    cwd: root, stdio: "inherit", windowsHide: true,
  });
  if (result.error) throw result.error;
  assert.equal(result.status, 0, `Native speech acceptance failed: ${result.signal ?? result.status}`);
}
run(["--target", target]);
const fixture = mkdtempSync(join(tmpdir(), "echo-native-speech-"));
try {
  const mac = process.platform === "darwin";
  const binaryDirectory = mac ? join(fixture, "Speech.app/Contents/MacOS") : fixture;
  const modelDirectory = mac ? join(fixture, "Speech.app/Contents/Resources/meeting-speech")
    : join(fixture, "meeting-speech");
  const extension = process.platform === "win32" ? ".exe" : "";
  const worker = join(binaryDirectory, `echo-meeting-speech${extension}`);
  mkdirSync(binaryDirectory, { recursive: true });
  mkdirSync(dirname(modelDirectory), { recursive: true });
  copyFileSync(join(project, "binaries", `echo-meeting-speech-${target}${extension}`), worker);
  cpSync(join(project, "resources/meeting-speech"), modelDirectory, { recursive: true });
  run(["--installed-app", worker]);
  console.log(`Native speech resource, worker and offline inference acceptance passed: ${target}`);
} finally {
  rmSync(fixture, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 });
}
