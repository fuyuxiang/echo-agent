// Explicit no-model acceptance, separate from the unfiltered host unit suite.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, mkdirSync, realpathSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { basename, join, resolve } from "node:path";
import { cleanNativeReviewEnvironment } from "./lib/native-review-isolation.mjs";
import { prepareMacosTestSignatures } from "./prepare-macos-test-signatures.mjs";

assert.equal(process.argv.length, 2, "No arbitrary ignored tests or data homes are accepted");
const clean = cleanNativeReviewEnvironment(process.env);
const prepared = prepareMacosTestSignatures(clean);
const executable = prepared.signed.find(path => basename(path).startsWith("echoagent_lib-"));
if (process.platform === "darwin") assert.ok(executable, "Compiled host library harness required");
for (const name of [
  "agent_runtime::tests::spawn_smoke_spawn_initialize_new_session",
  "skills::runtime_tests::resident_skill_reload_smoke",
]) {
  const home = realpathSync(mkdtempSync(join(tmpdir(), "echo-runtime-smoke-")));
  mkdirSync(join(home, "empty-legacy"));
  mkdirSync(join(home, "compat-home"));
  writeFileSync(join(home, "config.toml"), `
[models]
default = "review/mock"
[model_providers.review]
provider_kind = "custom"
base_url = "http://127.0.0.1:9/v1"
api_key = "synthetic-no-model-calls"
api_backend = "chat_completions"
auth_scheme = "bearer"
[model."review/mock"]
model = "mock"
model_provider = "review"
max_completion_tokens = 1024
[marketplace]
official_marketplace_auto_installed = true
[memory]
enabled = false
[features]
remote_fetch = false
`);
  const env = { ...clean, ECHO_AGENT_HOME: home, GROK_HOME: join(home, "empty-legacy") };
  const command = executable || "cargo";
  const args = executable ? ["--ignored", "--exact", name] : [
    "test", "--locked", "--manifest-path", "src-tauri/Cargo.toml", "--lib", name,
    "--", "--ignored", "--exact",
  ];
  const cwd = executable ? resolve("src-tauri") : process.cwd();
  const listed = spawnSync(command, [...args, "--list"], { cwd, env, encoding: "utf8" });
  assert.equal(listed.status, 0, listed.error?.message || listed.stderr);
  assert.ok(listed.stdout.includes(`${name}: test`), `Refuse zero-test success: ${name}`);
  console.log(`Isolated no-model Runtime acceptance: ${name}; data=${home}`);
  const result = spawnSync(command, [...args, "--nocapture", "--test-threads=1"], { cwd, env, stdio: "inherit" });
  if (result.error) throw result.error;
  if (result.status !== 0) { process.exitCode = result.status ?? 1; break; }
}
