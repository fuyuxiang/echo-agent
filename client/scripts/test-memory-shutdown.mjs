// Real Runtime/session-actor acceptance with a model that never responds.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, mkdirSync, realpathSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { basename, join, resolve } from "node:path";
import { cleanNativeReviewEnvironment } from "./lib/native-review-isolation.mjs";
import { prepareMacosTestSignatures } from "./prepare-macos-test-signatures.mjs";

assert.equal(process.argv.length, 2, "This acceptance uses fixed isolated data and one fixed test");
const clean = cleanNativeReviewEnvironment(process.env);
const prepared = prepareMacosTestSignatures(clean);
const executable = prepared.signed.find(path => basename(path).startsWith("echoagent_lib-"));
if (process.platform === "darwin") assert.ok(executable);
const home = realpathSync(mkdtempSync(join(tmpdir(), "echo-memory-shutdown-")));
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
enabled = true
[compaction.memory_flush]
idle_timeout_secs = 1
[features]
remote_fetch = false
`);
const name = "agent_runtime::tests::memory_shutdown_checkpoints_and_cancels_a_stalled_model";
const args = executable ? ["--ignored", "--exact", name] : ["test", "--locked", "--manifest-path", "src-tauri/Cargo.toml", "--lib", name, "--", "--ignored", "--exact"];
// A proxy may retain its upstream socket after the client cancels. This
// acceptance must observe the client transport directly at the local fixture.
const options = { cwd: executable ? resolve("src-tauri") : process.cwd(), env: {
  ...clean, NO_PROXY: "127.0.0.1,localhost,::1", no_proxy: "127.0.0.1,localhost,::1",
  ECHO_AGENT_HOME: home, GROK_HOME: join(home, "empty-legacy"),
} };
const listed = spawnSync(executable || "cargo", [...args, "--list"], { ...options, encoding: "utf8" });
assert.equal(listed.status, 0, listed.error?.message || listed.stderr);
assert(listed.stdout.includes(`${name}: test`), "Refuse zero-test success");
console.log(`Real memory shutdown acceptance; isolated data=${home}`);
const result = spawnSync(executable || "cargo", [...args, "--nocapture", "--test-threads=1"], { ...options, stdio: "inherit" });
if (result.error) throw result.error;
process.exitCode = result.status ?? 1;
