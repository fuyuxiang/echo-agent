import assert from "node:assert/strict";
import { build } from "esbuild";
import { spawn, spawnSync } from "node:child_process";
import { copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const project = join(root, "scripts/stability-check");
assert.ok(["darwin", "win32"].includes(process.platform), "Native fixture requires macOS or Windows");
await build({ entryPoints: [join(root,"src/lib/renderer-recovery.ts")], bundle: true, format: "iife", globalName: "EchoRendererHealth", platform: "browser", target: "es2021", minify: true, outfile: join(project,"fixture/renderer-health.js") });
const built = spawnSync("cargo", ["build", "--locked", "--manifest-path", join(project,"Cargo.toml"), "--features", "native-fixture", "--profile", "stability"], { cwd: root, stdio: "inherit" });
assert.equal(built.status, 0, "Native stability fixture did not build");
const target = process.env.CARGO_TARGET_DIR ? resolve(root, process.env.CARGO_TARGET_DIR) : join(project, "target");
const executable = join(target, "stability", `echo-stability-fixture${process.platform === "win32" ? ".exe" : ""}`);
if (process.platform === "darwin") {
  const signed = spawnSync("/usr/bin/codesign", ["--force", "--sign", "-", "--timestamp=none", executable], { stdio: "inherit" });
  assert.equal(signed.status, 0, "Native fixture could not be signed");
}
const profile = realpathSync(mkdtempSync(join(tmpdir(), "echo-stability-")));
const evidencePath = join(profile, "evidence.jsonl");
console.log(`Native stability evidence: ${profile}`);
const child = spawn(executable, [], { cwd: root, env: { ...process.env, ECHO_STABILITY_PROFILE: profile }, stdio: ["ignore", "ignore", "pipe"] });
let stderr = "";
let spawnError;
child.stderr.on("data", data => { stderr = (stderr + data).slice(-8000); });
child.on("error", error => { spawnError = error; stderr += String(error); });
let exit;
child.on("exit", (code, signal) => { exit = { code, signal }; });
const evidence = () => existsSync(evidencePath) ? readFileSync(evidencePath,"utf8").trim().split("\n").filter(Boolean).flatMap(line => { try { return [JSON.parse(line)]; } catch { return []; } }) : [];
const finishedNormally = rows => rows.some(row => row.value.complete && rows.some(exit => exit.pid === row.pid && exit.value.normalExitObserved));
try {
  const deadline = Date.now() + 90_000;
  while (Date.now() < deadline) {
    if (spawnError) throw spawnError;
    const rows = evidence();
    const error = rows.find(row => row.value.error);
    assert.ok(!error, `Native recovery failed: ${JSON.stringify(error)}\n${stderr}`);
    if (finishedNormally(rows)) break;
    if (exit && exit.code !== 73) throw new Error(`Fixture exited unexpectedly: ${JSON.stringify(exit)}; evidence: ${JSON.stringify(rows)}\n${stderr}`);
    await new Promise(resolve => setTimeout(resolve, 200));
  }
  // A deliberate exit must not be mistaken for another crash and resurrected.
  await new Promise(resolve => setTimeout(resolve, 2000));
  const rows = evidence();
  assert.ok(rows.some(row => row.value.complete), `Native recovery timed out; original host: ${JSON.stringify(exit)}; evidence: ${JSON.stringify(rows)}\n${stderr}`);
  assert.ok(finishedNormally(rows), "The final native event loop must complete intentional exit");
  assert.equal(child.exitCode, 73, "The original host must actually fail during injection");
  const boots = rows.filter(row => row.value.document);
  const expected = process.platform === "win32" ? 6 : 5;
  assert.equal(boots.length, expected);
  assert.equal(new Set(boots.map(row => row.value.document)).size, expected, "Recovery must create a new document");
  assert.equal(new Set(boots.slice(0,-2).map(row => row.pid)).size, 1, "Renderer recovery must retain the healthy native host");
  assert.notEqual(boots.at(-2).pid, boots[0].pid, "Host failure must be recovered by the external watcher");
  assert.notEqual(boots.at(-1).pid, boots.at(-2).pid, "Controlled restart must replace the native host");
  assert.ok(boots.every(row => row.value.draft === "恢复后保留草稿：中文、空格与附件说明"));
  assert.ok(boots.slice(1,-1).every(row => row.value.status.phase === "restored"));
  assert.equal(boots.at(-1).value.status.phase, "ready", "Controlled restart must not inherit a stale crash notice");
  console.log(JSON.stringify({ passed: true, platform: process.platform, documents: boots.length, rendererRecoveries: expected - 3, hostRestarts: 1, controlledRestarts: 1, draftPreserved: true, evidencePath }));
} finally {
  // The fixed fixture backend observes this private stop marker even when its
  // renderer is dead; no process-name kills or production profiles are used.
  writeFileSync(join(profile,"stop"), "stop\n");
  await new Promise(resolve => setTimeout(resolve, 1500));
  if (process.env.STABILITY_EVIDENCE_DIR) {
    mkdirSync(process.env.STABILITY_EVIDENCE_DIR, { recursive: true });
    if (existsSync(evidencePath)) copyFileSync(evidencePath, join(process.env.STABILITY_EVIDENCE_DIR, "evidence.jsonl"));
    writeFileSync(join(process.env.STABILITY_EVIDENCE_DIR, "native-stderr.log"), stderr);
    writeFileSync(join(process.env.STABILITY_EVIDENCE_DIR, "runner.json"), JSON.stringify({ platform: process.platform, arch: process.arch, node: process.version, exit, evidence: evidence() }, null, 2));
  }
}
