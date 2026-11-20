import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createServer } from "node:http";
import { createWriteStream } from "node:fs";
import { mkdir, mkdtemp, readFile, readdir, realpath, rm, writeFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { Readable } from "node:stream";
import { pipeline } from "node:stream/promises";
import { setTimeout as delay } from "node:timers/promises";
import { cleanNativeReviewEnvironment } from "./lib/native-review-isolation.mjs";

const fixture = process.argv[3] === "--fixture";
assert.equal(process.argv.length, fixture ? 4 : 3, "Pass the compiled echoagent_lib test executable and optionally --fixture");
const executable = await realpath(resolve(process.argv[2]));
const configPath = process.env.NATIVE_REVIEW_MODEL_CONFIG;
let config, upstream;
if (!fixture) {
  assert.ok(configPath, "Set NATIVE_REVIEW_MODEL_CONFIG or use --fixture for a local model stub");
  config = JSON.parse(await readFile(configPath, "utf8"));
  upstream = new URL(config.baseUrl);
  assert.equal(upstream.protocol, "https:");
  assert.equal(upstream.username + upstream.password + upstream.search + upstream.hash, "");
  if (!upstream.pathname.replace(/\/$/, "").endsWith("/chat/completions")) {
    upstream.pathname = upstream.pathname.replace(/\/$/, "") + "/chat/completions";
  }
  assert.equal(typeof config.apiKey, "string");
  assert.ok(config.apiKey.length);
  assert.equal(typeof config.model, "string");
}

function fixtureCompletion(response, input, primaryTurn, resumedTurn) {
  const shell = primaryTurn && input.tools?.find(tool => tool.function?.parameters?.properties?.command);
  if (primaryTurn) assert.ok(shell, "The initial run must advertise the shell tool");
  const message = shell ? {
    role: "assistant",
    tool_calls: [{
      type: "function",
      id: "fixture-shell-once",
      function: {
        name: shell.function.name,
        arguments: JSON.stringify({
          command: "/usr/bin/printf 'ECHO_RECOVERY_EXTERNAL\\n' | /usr/bin/tee -a recovery-proof.txt",
          description: "Write the isolated recovery marker",
        }),
      },
    }],
  } : { role: "assistant", content: resumedTurn ? "ECHO_RECOVERY_EXTERNAL" : "OK" };
  const base = { id: "fixture", created: 1, model: input.model };
  const finishReason = shell ? "tool_calls" : "stop";
  if (input.stream) {
    const delta = shell ? { ...message, tool_calls: [{ index: 0, ...message.tool_calls[0] }] } : message;
    response.writeHead(200, { "content-type": "text/event-stream" });
    for (const [value, reason] of [[delta, null], [{}, finishReason]]) {
      response.write(`data: ${JSON.stringify({
        ...base, object: "chat.completion.chunk", choices: [{ index: 0, delta: value, finish_reason: reason }],
      })}\n\n`);
    }
    response.end("data: [DONE]\n\n");
  } else {
    response.writeHead(200, { "content-type": "application/json" });
    response.end(JSON.stringify({
      ...base, object: "chat.completion", choices: [{ index: 0, message, finish_reason: finishReason }],
    }));
  }
}

function isUserInstruction(message, text) {
  return message?.role === "user"
    && typeof message.content === "string"
    && message.content.replace(/^<user_query>\s*/, "").startsWith(text);
}

const root = await realpath(await mkdtemp("/tmp/echo-runtime-recovery-"));
const cwd = join(root, "workspace");
await mkdir(cwd);
await mkdir(join(root, "legacy"));
const testName = "agent_runtime::tests::automation_runtime_recovery_real_model";
let current = null;
let gated = false;
let primaryFailures = 0;
let initialInference = 0;
let recoveredInference = 0;
let fixtureFailure;
let phase = "first";
const server = createServer(async (request, response) => {
  try {
    if (request.method !== "POST" || !request.url.endsWith("/chat/completions")) {
      response.writeHead(404).end();
      return;
    }
    if (request.url.startsWith("/unavailable/")) {
      primaryFailures += 1;
      request.resume();
      response.writeHead(503).end();
      return;
    }
    let body = "";
    for await (const piece of request) {
      body += piece;
      if (body.length > 2_000_000) throw new Error("request too large");
    }
    const input = JSON.parse(body);
    const messages = input.messages ?? [];
    const lastMessage = messages.at(-1);
    const toolResult = messages.some(({ role, content }) => role === "tool"
      && JSON.stringify(content)?.includes("ECHO_RECOVERY_EXTERNAL"));
    // Title generation also quotes the initial instruction. The dashboard
    // summary includes the entire recovered transcript, then adds its own turn.
    const titleRequest = messages[0]?.content?.startsWith?.("Generate a concise, distinctive title");
    const primaryTurn = !titleRequest && isUserInstruction(lastMessage, "Use the shell tool exactly once");
    const resumedTurn = isUserInstruction(lastMessage, "The original turn was interrupted.");
    if (phase === "first" && toolResult && lastMessage?.role === "tool") {
      // Hold the second inference while the first Runtime is still alive.
      gated = true;
      return;
    }
    if (phase === "resume" && resumedTurn) {
      assert.ok(toolResult, "Recovered request must carry the previous tool result");
      assert.ok(!input.tools?.length, "Recovered request must be non-executing");
    }
    if (fixture) {
      fixtureCompletion(response, input, phase === "first" && primaryTurn, phase === "resume" && resumedTurn);
      if (phase === "first" && primaryTurn) initialInference += 1;
      if (phase === "resume" && resumedTurn) recoveredInference += 1;
      return;
    }
    input.model = config.model;
    const auth = ["x-api-key", "x_api_key"].includes(config.authScheme)
      ? { "x-api-key": config.apiKey }
      : { Authorization: `Bearer ${config.apiKey}` };
    const result = await fetch(upstream, {
      method: "POST",
      redirect: "error",
      signal: AbortSignal.timeout(50_000),
      headers: { ...auth, "content-type": "application/json" },
      body: JSON.stringify(input),
    });
    if (!result.ok || !result.body) {
      console.error(`Real inference returned HTTP ${result.status} during ${phase}`);
      response.writeHead(result.status).end();
      return;
    }
    if (phase === "first" && primaryTurn) initialInference += 1;
    if (phase === "resume" && resumedTurn) recoveredInference += 1;
    response.writeHead(200, { "content-type": result.headers.get("content-type") || "text/event-stream" });
    await pipeline(Readable.fromWeb(result.body), response);
  } catch (error) {
    if (fixture) {
      fixtureFailure ??= error;
      console.error(`Local fixture failed during ${phase}: ${error.message}`);
    }
    if (!response.headersSent) response.writeHead(502);
    response.end();
  }
});

function launch(selectedPhase) {
  const output = createWriteStream(join(root, `${selectedPhase}.log`), { mode: 0o600 });
  const child = spawn(executable, ["--ignored", "--exact", testName, "--nocapture"], {
    cwd: resolve("src-tauri"),
    env: {
      ...cleanNativeReviewEnvironment(process.env),
      NATIVE_REVIEW_MODEL_CONFIG: "",
      ECHO_AGENT_HOME: root,
      ECHO_AGENT_ALLOW_INSECURE_LOOPBACK_HTTP: "1",
      GROK_HOME: join(root, "legacy"),
      ECHO_AUTOMATION_RECOVERY_PHASE: selectedPhase,
      NO_PROXY: "127.0.0.1,localhost",
      no_proxy: "127.0.0.1,localhost",
    },
    stdio: ["ignore", "pipe", "pipe"],
  });
  child.stdout.pipe(output, { end: false });
  child.stderr.pipe(output, { end: false });
  const finished = new Promise((done, reject) => {
    child.once("error", reject);
    child.once("close", (code, signal) => output.end(() => done({ code, signal })));
  });
  return { child, finished };
}

async function hasCheckpoint() {
  try {
    const [session, prompt] = (await readFile(join(root, "first-session"), "utf8")).split("\n");
    for (const scope of await readdir(join(root, "sessions"))) {
      const candidate = await readFile(join(root, "sessions", scope, session, "desktop-automation-checkpoint"), "utf8")
        .catch(() => "");
      if (candidate === prompt) return true;
    }
    return false;
  } catch {
    return false;
  }
}

try {
  await new Promise((ready, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", ready);
  });
  await writeFile(join(root, "config.toml"), `[models]
default = "recovery/real"
[model_providers.recovery]
provider_kind = "custom"
base_url = "http://127.0.0.1:${server.address().port}/v1"
api_key = "local-test-fixture"
api_backend = "chat_completions"
auth_scheme = "bearer"
context_window = 131072
[model_providers.unavailable]
provider_kind = "custom"
base_url = "http://127.0.0.1:${server.address().port}/unavailable/v1"
api_key = "local-test-fixture"
api_backend = "chat_completions"
auth_scheme = "bearer"
[model."recovery/unavailable"]
model = "unavailable-test"
model_provider = "unavailable"
context_window = 131072
[model."recovery/real"]
model = "recovery-test"
model_provider = "recovery"
max_completion_tokens = 2048
[marketplace]
official_marketplace_auto_installed = true
[memory]
enabled = false
[features]
remote_fetch = false
`, { mode: 0o600 });
  current = launch("first");
  const first = current;
  const deadline = Date.now() + 100_000;
  while (!(gated && await hasCheckpoint())) {
    assert.ok(first.child.exitCode === null && first.child.signalCode === null,
      `First Runtime exited early, see ${root}/first.log`);
    assert.ok(Date.now() < deadline, `Checkpoint not reached, see ${root}/first.log`);
    await delay(80);
  }
  assert.equal(primaryFailures, 1);
  assert.ok(initialInference > 0);
  assert.equal(await readFile(join(cwd, "recovery-proof.txt"), "utf8"), "ECHO_RECOVERY_EXTERNAL\n");
  assert.ok(first.child.kill("SIGKILL"));
  assert.equal((await first.finished).signal, "SIGKILL");
  phase = "resume";
  current = launch("resume");
  const watchdog = setTimeout(() => current.child.kill("SIGKILL"), 95_000);
  try {
    assert.equal((await current.finished).code, 0, `Recovered Runtime failed, see ${root}/resume.log`);
  } finally {
    clearTimeout(watchdog);
  }
  assert.ifError(fixtureFailure);
  assert.ok(recoveredInference > 0);
  assert.match(await readFile(join(root, "resume.log"), "utf8"), /1 passed; 0 failed/);
  assert.equal(await readFile(join(cwd, "recovery-proof.txt"), "utf8"), "ECHO_RECOVERY_EXTERNAL\n");
  console.log(`Passed failed primary probe, ${fixture ? "local fixture" : "real model"} fallback, one shell execution, process SIGKILL, and summary from the restored ACP session.`);
  await rm(root, { recursive: true, force: true });
} finally {
  current?.child.kill();
  server.closeAllConnections();
  server.close();
}
