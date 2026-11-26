// Explicit no-model acceptance, separate from the unfiltered host unit suite.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, mkdirSync, realpathSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { basename, join, resolve } from "node:path";
import { cleanNativeReviewEnvironment, withNativeReviewLoopbackBypass } from "./lib/native-review-isolation.mjs";
import { compileDesktopTestHarnesses, prepareMacosTestSignatures } from "./prepare-macos-test-signatures.mjs";
import { reportNativeTestFailure, runNativeTestProcess } from "./lib/native-test-process.mjs";

assert.equal(process.argv.length, 2, "No arbitrary ignored tests or data homes are accepted");
const clean = withNativeReviewLoopbackBypass(cleanNativeReviewEnvironment(process.env));
let executable;
try {
  // Cargo may need to compile or relink the harness on a cold runner. Finish
  // that work once before the per-scenario listing and execution clocks start.
  const artifacts = process.platform === "darwin"
    ? prepareMacosTestSignatures(clean).signed
    : compileDesktopTestHarnesses(clean);
  executable = artifacts.find(path => /^echoagent_lib-[a-f0-9]{16}(?:\.exe)?$/.test(basename(path)));
  assert.ok(executable, "Compiled host library harness required");
} catch (error) {
  reportNativeTestFailure(1, ["", String(error.stack ?? error)], clean, "Runtime smoke compilation failed");
  throw error;
}
const cwd = resolve("src-tauri");
const runtimeSmoke = "harness::agent_runtime::tests::spawn_smoke_spawn_initialize_new_session";
const retiredSelection = '[models]\ndefault = "echoagent-ojlab/Echo Reed"\nallowed_models = ["echoagent-ojlab/*"]\n'
  + '[model_providers.echoagent-ojlab]\nbase_url = "http://removed.invalid/v1"\nechoagent_managed_by = "builtin"\n'
  + ["Echo Brook", "Echo Cedar", "Echo Grove", "Echo Reed", "Echo Willow", "chat-xc", "chat-glm", "chat-qwen"]
    .map(id => `[model.${JSON.stringify(`echoagent-ojlab/${id}`)}]\nmodel = ${JSON.stringify(id)}\nmodel_provider = "echoagent-ojlab"\n`).join('');
for (const { name, selection, scenario, storage = '' } of [
  { name: runtimeSmoke, selection: '[models]\ndefault = "review/mock"', scenario: "explicit personal model" },
  { name: runtimeSmoke, selection: "", scenario: "Auto without a saved preference" },
  { name: runtimeSmoke, selection: '[models]\ndefault = "  "', scenario: "Auto with a blank preference" },
  { name: runtimeSmoke, selection: retiredSelection, scenario: "retired catalog, preference, and model filters" },
  {
    name: "harness::agent_runtime::tests::persisted_sessions_survive_clock_skew_on_initialize_and_load",
    selection: '[models]\ndefault = "review/mock"',
    scenario: "past and future session timestamps with a legacy retention setting",
    storage: '[storage]\ncleanup_ttl_days = 1',
  },
  { name: "harness::skills::runtime_tests::resident_skill_reload_smoke", selection: '[models]\ndefault = "review/mock"', scenario: "resident skill reload" },
  { name: "mcp::tests::global_toggle_applies_to_loaded_sessions_new_sessions_and_agent_pool", selection: '[models]\ndefault = "review/mock"', scenario: "durable MCP disable, restore and deletion across live sessions and agent pool" },
  { name: "providers::tests::unsaved_provider_probes_never_send_an_inherited_key_to_another_origin", selection: '[models]\ndefault = "review/mock"', scenario: "provider probes with distinct loopback origins" },
]) {
  const home = realpathSync(mkdtempSync(join(tmpdir(), "echo-runtime-smoke-")));
  mkdirSync(join(home, "empty-legacy"));
  mkdirSync(join(home, "compat-home"));
  writeFileSync(join(home, "config.toml"), `
${selection}
${storage}
[model_providers.review]
provider_kind = "custom"
base_url = "https://127.0.0.1:9/v1"
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
  const args = ["--ignored", "--exact", name];
  const listed = spawnSync(executable, [...args, "--list"], { cwd, env, encoding: "utf8", timeout: 120_000 });
  const found = listed.stdout?.includes(`${name}: test`);
  if (listed.status !== 0 || !found) {
    const detail = listed.error?.message ?? (listed.stderr || `Missing test: ${name}`);
    reportNativeTestFailure(listed.status || 1, [listed.stdout ?? "", `${scenario}\n${detail}`], env, "Runtime smoke test listing failed");
  }
  assert.equal(listed.status, 0, listed.error?.message || listed.stderr);
  assert.ok(found, `Refuse zero-test success: ${name}`);
  console.log(`Isolated no-model Runtime acceptance: ${scenario}; ${name}; data=${home}`);
  const status = await runNativeTestProcess(executable, [...args, "--nocapture", "--test-threads=1"], { cwd, env, timeout: 180_000 });
  if (status !== 0) { process.exitCode = status; break; }
  assert.doesNotMatch(readFileSync(join(home, "config.toml"), "utf8"), /echoagent-ojlab/, "Retired models must not be reinstalled after startup");
}
