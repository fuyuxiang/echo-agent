// Real offline first-use acceptance with a fresh user profile. Uses the same
// local test-signing boundary as the existing host/runtime smoke checks.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, mkdirSync, realpathSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { cleanNativeReviewEnvironment } from "./lib/native-review-isolation.mjs";
import { runNativeTestProcess } from "./lib/native-test-process.mjs";
import { prepareMacosTestSignatures } from "./prepare-macos-test-signatures.mjs";
import { defaultDirectory, prepareModels } from "./prepare-retrieval-models.mjs";

assert.equal(process.argv.length, 2, "This entry only runs the fixed bundled-model acceptance");
await prepareModels({ check: true });
const home = realpathSync(mkdtempSync(join(tmpdir(), "echo-retrieval-bundle-")));
const legacy = join(home, "empty-legacy"); mkdirSync(legacy, { mode: 0o700 });
const isolated = { ...cleanNativeReviewEnvironment(process.env), ECHO_AGENT_HOME: home, GROK_HOME: legacy,
  ECHO_RETRIEVAL_MODELS: defaultDirectory };
// Block repair downloads in the separately launched macOS test binary. On
// other hosts Cargo may still need build dependencies; the test independently
// rejects any model installation by asserting the user model directory is absent.
const env = process.platform === "darwin" ? { ...isolated,
  HTTP_PROXY: "http://127.0.0.1:9", HTTPS_PROXY: "http://127.0.0.1:9", ALL_PROXY: "http://127.0.0.1:9",
  http_proxy: "http://127.0.0.1:9", https_proxy: "http://127.0.0.1:9", all_proxy: "http://127.0.0.1:9",
} : isolated;
const preparation = prepareMacosTestSignatures(cleanNativeReviewEnvironment(process.env));
async function runFixedCase(test, runEnv) {
 if (process.platform === "darwin") {
  const harness = preparation.signed.find(path => /\/echoagent_lib-[a-f0-9]+$/.test(path));
  assert(harness, "Missing signed host library test harness");
  const listed = spawnSync(harness, [test, "--exact", "--ignored", "--list"], {cwd: realpathSync("src-tauri"), env: runEnv, encoding: "utf8"});
  assert.equal(listed.status, 0, listed.stderr);
  assert(listed.stdout.includes(`${test}: test`), "Refuse zero-test offline acceptance");
  return runNativeTestProcess(harness, [test, "--exact", "--ignored", "--nocapture"], { cwd: realpathSync("src-tauri"), env: runEnv });
 } else {
  const args = ["test", "--locked", "--manifest-path", "src-tauri/Cargo.toml", "--lib", "-j", "2", test,
    "--", "--exact", "--ignored"];
  // Cargo also exits successfully when an exact filter matches zero tests.
  // Require the actual ignored test on Windows/Linux before executing it.
  const listed = spawnSync("cargo", [...args, "--list"], {
    env: runEnv, encoding: "utf8", stdio: ["ignore", "pipe", "inherit"],
  });
  assert.equal(listed.status, 0, listed.error?.message || "Offline acceptance discovery failed");
  assert(listed.stdout.includes(`${test}: test`), "Refuse zero-test offline acceptance");
  return runNativeTestProcess("cargo", [...args, "--nocapture"], { env: runEnv });
 }
}
console.log(`Offline first-use test data: ${home}`);
process.exitCode = await runFixedCase("retrieval_models::tests::bundled_models_work_offline_without_creating_or_copying_user_models", env);
if (process.exitCode === 0) {
 const installed = realpathSync(mkdtempSync(join(tmpdir(), "echo-retrieval-install-")));
 mkdirSync(join(installed, "empty-legacy"), {mode: 0o700});
 console.log(`Offline import test data: ${installed}`);
 process.exitCode = await runFixedCase("retrieval_models::tests::offline_package_installs_verified_models_and_runs_inference",
   {...env, ECHO_AGENT_HOME: installed, GROK_HOME: join(installed, "empty-legacy")});
}
