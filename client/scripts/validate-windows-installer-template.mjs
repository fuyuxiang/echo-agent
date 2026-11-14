// Compile the complete production NSIS template with the pinned Tauri bundler.
// Only an isolated dummy application is packaged; no installer is executed.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { copyFileSync, mkdirSync, mkdtempSync, readdirSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const project = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const fixture = mkdtempSync(join(tmpdir(), "echo-installer-template-"));
const native = join(fixture, "src-tauri");
const output = process.env.ECHO_NSIS_TEST_OUTPUT || fixture;
const target = "x86_64-pc-windows-msvc";
mkdirSync(join(native, "src"), { recursive: true });
mkdirSync(join(fixture, "frontend"), { recursive: true });
mkdirSync(output, { recursive: true });
writeFileSync(join(fixture, "package.json"), JSON.stringify({ name: "echo-installer-fixture", version: "1.0.0", private: true }));
writeFileSync(join(native, "Cargo.toml"), '[package]\nname = "echo-installer-fixture"\nversion = "1.0.0"\nedition = "2021"\n');
writeFileSync(join(native, "src/main.rs"), "fn main() {}\n");
writeFileSync(join(fixture, "frontend/index.html"), "<!doctype html><title>Installer compiler fixture</title>");
const binaryDirectory = join(native, "target", target, "debug");
mkdirSync(binaryDirectory, { recursive: true });
const binary = join(binaryDirectory, "echo-installer-fixture.exe");
writeFileSync(binary, "NSIS compiler fixture: packaged data, never executed.\n");
writeFileSync(join(native, "tauri.conf.json"), JSON.stringify({
  productName: "Echo installer fixture", version: "1.0.0", identifier: "com.echoagent.installer-fixture",
  build: { frontendDist: "../frontend" },
  bundle: {
    active: true, useLocalToolsDir: true, targets: ["nsis"], icon: [],
    windows: { webviewInstallMode: { type: "skip" }, allowDowngrades: false, nsis: {
      installMode: "currentUser", languages: ["SimpChinese", "English"], compression: "none",
      template: process.env.ECHO_NSIS_TEMPLATE || join(project, "src-tauri/windows/installer.nsi"),
      installerHooks: join(project, "src-tauri/windows/install-path.nsh"),
    } },
  },
}, null, 2));
const result = spawnSync(process.execPath, [join(project, "node_modules/@tauri-apps/cli/tauri.js"),
  "bundle", "--debug", "--bundles", "nsis", "--target", target, "--ci", "--no-sign", "--no-binary-patching"],
{ cwd: native, env: { ...process.env, CARGO_TARGET_DIR: join(native, "target") },
  encoding: "utf8", timeout: 180_000, maxBuffer: 8 * 1024 * 1024 });
writeFileSync(join(output, "nsis-template-compile.log"), `${result.stdout || ""}\n${result.stderr || ""}`);
if (result.error || result.status !== 0) {
  const message = `${result.error || `Exit code ${result.status}`}\n${result.stderr || ""}\n${result.stdout || ""}`.slice(-2800);
  console.error(`::error title=Production NSIS template compilation failed::${message.replaceAll("%", "%25").replaceAll("\r", "%0D").replaceAll("\n", "%0A")}`);
  throw new Error(`NSIS template compilation failed; evidence: ${output}`);
}
const bundles = join(binaryDirectory, "bundle/nsis");
const installer = readdirSync(bundles).find(name => name.endsWith("-setup.exe"));
assert.ok(installer, "The actual bundler must produce an NSIS installer");
copyFileSync(join(bundles, installer), join(output, "template-fixture-setup.exe"));
console.log(JSON.stringify({ passed: true, fixture, output, installer }));
