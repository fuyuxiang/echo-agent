import { cleanNativeReviewEnvironment } from "./lib/native-review-isolation.mjs";
import { nativeReviewOutput } from './lib/native-review-output.mjs';
// Real packaged application lifecycle review; no default profile or OS item.
import assert from 'node:assert/strict';
import { chmodSync, copyFileSync, existsSync, lstatSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, realpathSync, renameSync, rmdirSync, statSync, unlinkSync, writeFileSync } from 'node:fs';
import { createHash, randomBytes } from 'node:crypto';
import { spawn } from 'node:child_process';
import { join, resolve, dirname } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';

const hash = (bytes) => createHash('sha256').update(bytes).digest('hex');
// Session creation requires a configured route. This lifecycle fixture never
// sends a prompt, so a closed loopback endpoint is sufficient.
const offlineRuntimeConfig = `
[models]
default = "exit-review/offline"
[model_providers.exit-review]
provider_kind = "custom"
base_url = "https://127.0.0.1:9/v1"
api_key = "synthetic-exit-review-only"
api_backend = "chat_completions"
auth_scheme = "bearer"
[model."exit-review/offline"]
model = "offline"
model_provider = "exit-review"
[marketplace]
official_marketplace_auto_installed = true
[features]
remote_fetch = false
`;
const required = {
  'init-failure': ['native-exit-isolation', 'native-init-failure-visible', 'native-no-recording', 'native-init-failure-graceful-request'],
  'flush-failure': ['native-exit-isolation', 'native-dirty-plan-update', 'native-save-failure-retained', 'native-runtime-recovered', 'native-final-graceful-request'],
  restart: ['native-exit-isolation', 'native-restart-plan-restored', 'native-final-graceful-request'],
};
export function verifyNativeExitReport({ report, scope, pid, startedAt, exitCode, signal }) {
  assert.equal(exitCode, 0, 'Native application must exit normally');
  assert.equal(signal, null, 'Signal termination is not graceful exit acceptance');
  assert.equal(report.kind, 'real-native-exit-review');
  assert.equal(report.passed, true); assert.deepEqual(report.errors, []);
  assert.equal(report.phase, scope.phase); assert.equal(report.nonce, scope.nonce);
  assert.equal(report.home, scope.home); assert.equal(report.cwd, scope.cwd); assert.equal(report.pid, pid);
  assert.equal(report.nativeContext.pid, pid); assert.equal(report.nativeContext.evaluations, 1);
  assert.deepEqual(report.nativeContext.scope, scope);
  assert(Date.parse(report.finishedAt) >= startedAt && Date.parse(report.finishedAt) <= Date.now() + 10000);
  assert(Array.isArray(report.checks));
  const seen = new Set();
  for (const check of report.checks) { assert(!seen.has(check.id), 'Repeated native check'); seen.add(check.id); assert.equal(check.result, 'passed'); }
  assert.deepEqual([...seen].sort(), [...required[scope.phase]].sort());
  const evidence = (id) => report.checks.find((check) => check.id === id).evidence;
  if (scope.phase === 'init-failure') {
    const failure = evidence('native-init-failure-visible');
    assert.match(failure.error, /^初始化未完成：/);
    assert.match(failure.error, /synthetic-invalid-runtime-memory-type/);
    assert.match(failure.error, /重试初始化/);
    assert.equal(failure.context.hasRuntimeSender, false); assert.equal(failure.context.hasInitOutcome, false);
    assert.equal(evidence('native-no-recording').count, 0);
    assert.equal(typeof evidence('native-no-recording').listenerReadyObserved, 'boolean');
  } else if (scope.phase === 'flush-failure') {
    assert.match(report.sessionId, /^[a-zA-Z0-9-]{1,128}$/);
    const dirty = evidence('native-dirty-plan-update');
    assert.equal(dirty.sessionId, report.sessionId); assert.equal(dirty.sessionUpdate, 'current_mode_update'); assert.equal(dirty.currentModeId, 'plan');
    const failure = evidence('native-save-failure-retained');
    assert.equal(failure.context.hasRuntimeHandle, true); assert.equal(failure.context.lifecycle.exitReady, false); assert.equal(failure.context.lifecycle.exitInProgress, false);
    assert.equal(failure.failureVisible, true); assert.match(`${failure.failure.message} ${failure.context.shutdownFailure}`, /保存|denied|权限|Permission|permission|持久化/);
    const recovery = evidence('native-runtime-recovered');
    assert.equal(recovery.context.hasRuntimeSender, true); assert.equal(recovery.context.hasInitOutcome, true);
    assert(Number.isInteger(recovery.initialInitGeneration) && recovery.context.initGeneration > recovery.initialInitGeneration, 'Recovery did not replace the stopped Runtime');
    assert.equal(recovery.sameSessionId, report.sessionId); assert.equal(recovery.afterPhysicalRepair, true);
  } else {
    assert.equal(report.sessionId, scope.expectedSessionId);
    const restored = evidence('native-restart-plan-restored');
    assert.equal(restored.sessionId, scope.expectedSessionId); assert.equal(restored.cwd, scope.cwd);
    assert.equal(restored.selectedInUi, true); assert.equal(restored.update.currentModeId, 'plan'); assert.equal(restored.noModelTurnSent, true);
  }
  return true;
}

const ordinary = (path, root, directory = false) => {
  assert(path.startsWith(`${root}/`) || path === root, 'Owned path escaped private root');
  const info = lstatSync(path);
  assert(!info.isSymbolicLink() && (directory ? info.isDirectory() : info.isFile()), 'Owned path is not ordinary');
  assert.equal(realpathSync(path), path); return info;
};
const readJson = (path, root) => { ordinary(path, root); assert(statSync(path).size <= 131072); return JSON.parse(readFileSync(path, 'utf8')); };
const writeJson = (path, value) => writeFileSync(path, JSON.stringify(value, null, 2), { mode: 0o600 });
const wait = async (read, label, child, timeout = 90000) => {
  const end = Date.now() + timeout;
  while (Date.now() < end) {
    if (child && (child.exitCode !== null || child.signalCode !== null || child.exitReviewSpawnError)) throw new Error(`Application exited while waiting for ${label}`);
    const result = read(); if (result) return result; await delay(100);
  }
  throw new Error(`Timed out waiting for ${label}`);
};
const sessionDirectory = (home, id) => {
  assert.match(id, /^[a-zA-Z0-9-]{1,128}$/);
  const sessions = join(home, 'sessions'); ordinary(sessions, home, true);
  const found = [];
  for (const entry of readdirSync(sessions, { withFileTypes: true })) {
    if (!entry.isDirectory() || entry.isSymbolicLink()) continue;
    const parent = join(sessions, entry.name); ordinary(parent, home, true);
    const path = join(parent, id);
    if (existsSync(path)) { ordinary(path, home, true); found.push(path); }
  }
  assert.equal(found.length, 1, 'Session must have one owned physical directory'); return found[0];
};
const physical = (home, id) => {
  const directory = sessionDirectory(home, id);
  const plan = join(directory, 'plan_mode.json'), updates = join(directory, 'updates.jsonl');
  const snapshot = readJson(plan, home);
  ordinary(updates, home); assert(statSync(updates).size <= 16 * 1024 * 1024);
  const lines = readFileSync(updates, 'utf8').trim().split('\n').filter(Boolean).map(JSON.parse);
  assert.equal(snapshot.state, 'Pending', 'Dirty plan snapshot was not persisted after repair');
  const matching = lines.filter((line) => {
    const update = line?.params?.update ?? line?.update ?? line;
    return update.sessionUpdate === 'current_mode_update' && (update.currentModeId ?? update.current_mode_id) === 'plan';
  });
  assert(matching.length > 0, 'Dirty real ACP plan update was not persisted');
  return { directory, planState: snapshot.state, planSha256: hash(readFileSync(plan)), updatesSha256: hash(readFileSync(updates)), planUpdates: matching.length, updateLines: lines.length };
};

export async function runNativeExitReview(executableArgument) {
  const executable = realpathSync(resolve(executableArgument)); assert(lstatSync(executable).isFile());
  const output = nativeReviewOutput('native-exit', 'NATIVE_EXIT_REVIEW_OUTPUT');
  const result = { kind: 'real packaged native exit recovery review', executable, executableSha256: hash(readFileSync(executable)), output, phases: [], errors: [], nativeVerified: false, passed: false };
  let active, planFault;
  const repairPlan = () => {
    if (!planFault) return;
    const { path, backup } = planFault;
    rmdirSync(path);
    if (backup) renameSync(backup, path);
    planFault = undefined;
  };
  const clean = async () => {
    repairPlan();
    if (active && !active.child.exitReviewSpawnError && active.child.exitCode === null && active.child.signalCode === null) {
      active.child.kill('SIGTERM'); await Promise.race([active.exited, delay(5000, undefined, { ref: false })]);
      if (active.child.exitCode === null) { active.child.kill('SIGKILL'); await active.exited; }
    }
  };
  let interrupted = false;
  const interrupt = () => { interrupted = true; void clean(); };
  process.once('SIGINT', interrupt); process.once('SIGTERM', interrupt);
  try {
    const homes = {};
    for (const phase of ['init-failure', 'flush-failure', 'restart']) {
      assert(!interrupted, 'Owned review interrupted');
      const home = phase === 'restart' ? homes['flush-failure'].home : realpathSync(mkdtempSync('/private/tmp/echo-native-review-exit-'));
      chmodSync(home, 0o700);
      const cwd = join(home, 'workspaces/default'); mkdirSync(cwd, { recursive: true, mode: 0o700 }); chmodSync(dirname(cwd), 0o700); chmodSync(cwd, 0o700);
      const legacy = join(home, 'empty-legacy'); mkdirSync(legacy, { recursive: true, mode: 0o700 });
      const scope = { schema: 1, home, cwd, nonce: phase === 'restart' ? homes['flush-failure'].nonce : randomBytes(16).toString('hex'), phase, expectedSessionId: phase === 'restart' ? homes['flush-failure'].sessionId : null };
      const manifest = join(home, 'native-exit-manifest.private.json'); writeJson(manifest, scope); chmodSync(manifest, 0o600);
      // A syntax-corrupt file is quarantined by the real desktop reader.
      // Keep valid TOML with an invalid Runtime field type so init must fail.
      if (phase === 'init-failure') writeFileSync(join(home, 'config.toml'), "memory = 'synthetic-invalid-runtime-memory-type'\n", { mode: 0o600 });
      if (phase === 'flush-failure') writeFileSync(join(home, 'config.toml'), offlineRuntimeConfig, { mode: 0o600 });
      const reportPath = join(home, 'native-review.json');
      const beforeHash = existsSync(reportPath) ? hash(readFileSync(reportPath)) : null;
      const env = { ...cleanNativeReviewEnvironment(process.env) };
      for (const key of Object.keys(env)) if (key.startsWith('ECHO_DESKTOP_REVIEW') || key.startsWith('ECHO_EXIT_REVIEW') || ['ECHO_AGENT_HOME', 'ECHO_DESKTOP_VALIDATION', 'GROK_AUTH', 'GROK_AUTH_PATH'].includes(key)) delete env[key];
      Object.assign(env, { GROK_HOME: legacy, ECHO_DESKTOP_REVIEW: '1', ECHO_DESKTOP_REVIEW_EXIT: '1', ECHO_EXIT_REVIEW_PHASE: phase, ECHO_EXIT_REVIEW_MANIFEST: manifest });
      const startedAt = Date.now();
      const child = spawn(executable, ['--data-home', home], { cwd, env, stdio: ['ignore', 'pipe', 'pipe'] });
      const logs = []; child.stdout.on('data', (data) => logs.push(data.toString())); child.stderr.on('data', (data) => logs.push(data.toString()));
      const state = { child, exited: new Promise((accept) => {
        child.once('exit', accept);
        child.once('error', (error) => { child.exitReviewSpawnError = error; accept(); });
      }) }; active = state;
      let physicalBefore;
      try {
        if (phase === 'flush-failure') {
          const readyPath = join(cwd, 'exit-session-ready.json');
          const ready = await wait(() => existsSync(readyPath) && readJson(readyPath, home), 'real new session', child);
          assert.equal(ready.nonce, scope.nonce); assert.equal(ready.cwd, cwd);
          scope.createdSessionId = ready.sessionId;
          const directory = sessionDirectory(home, ready.sessionId);
          const planBefore = join(directory, 'plan_mode.json');
          const existing = existsSync(planBefore);
          physicalBefore = { directory, state: existing ? readJson(planBefore, home).state : null, fault: 'plan path occupied by a directory' };
          assert.notEqual(physicalBefore.state, 'Pending', 'Session was already dirty before the production action');
          // The Runtime can reapply owner-only permissions during persistence.
          // A directory occupying the plan file path forces its atomic rename
          // to fail until this driver repairs that exact path.
          const backup = existing ? join(directory, `plan-before-${scope.nonce}.json`) : undefined;
          if (backup) renameSync(planBefore, backup);
          try { mkdirSync(planBefore, { mode: 0o700 }); }
          catch (error) {
            if (backup) renameSync(backup, planBefore);
            throw error;
          }
          planFault = { path: planBefore, backup };
          const probe = join(directory, `plan-probe-${scope.nonce}.tmp`);
          writeFileSync(probe, 'synthetic write probe', { flag: 'wx', mode: 0o600 });
          try {
            assert.throws(() => renameSync(probe, planBefore), { code: 'EISDIR' });
          } finally { unlinkSync(probe); }
          writeJson(join(cwd, 'exit-control.json'), { nonce: scope.nonce, stage: 'fault-applied', sessionId: ready.sessionId });
          const failedPath = join(cwd, 'exit-failed-ready.json');
          // The native exit and its automatic recovery can each use the
          // Runtime's full 60-second save window while the fault is held.
          const failed = await wait(() => existsSync(failedPath) && readJson(failedPath, home), 'actual failed graceful exit', child, 155000);
          assert.equal(failed.nonce, scope.nonce); assert.equal(failed.sessionId, ready.sessionId);
          assert.equal(child.exitCode, null); assert.equal(failed.context.hasRuntimeHandle, true); assert.equal(failed.context.lifecycle.exitReady, false);
          assert(existsSync(join(cwd, 'exit-dirty-ready.json')), 'Faulted session was never dirtied');
          repairPlan();
          writeJson(join(cwd, 'exit-control.json'), { nonce: scope.nonce, stage: 'permissions-restored', sessionId: ready.sessionId });
          homes['flush-failure'] = { home, nonce: scope.nonce, sessionId: ready.sessionId };
          delete scope.createdSessionId;
        }
        const ended = await Promise.race([state.exited.then(() => true), delay(180000, false, { ref: false })]);
        assert(ended, 'Native exit phase exceeded its bound'); assert(!interrupted);
        if (child.exitReviewSpawnError) throw child.exitReviewSpawnError;
        const report = readJson(reportPath, home); assert.notEqual(hash(readFileSync(reportPath)), beforeHash, 'Read an old native report');
        assert(statSync(reportPath).mtimeMs >= startedAt);
        verifyNativeExitReport({ report, scope, pid: child.pid, startedAt, exitCode: child.exitCode, signal: child.signalCode });
        copyFileSync(reportPath, join(output, `${phase}.json`));
        const proof = phase === 'init-failure' ? { noRecording: true, actualInitFailure: true } : physical(home, report.sessionId);
        if (phase === 'flush-failure') proof.before = physicalBefore;
        if (phase === 'restart') assert.equal(proof.planSha256, result.phases.find((item) => item.phase === 'flush-failure').filesystem.planSha256, 'Restart changed the repaired plan snapshot');
        writeJson(join(output, `${phase}-filesystem.json`), proof);
        result.phases.push({ phase, pid: child.pid, exitCode: child.exitCode, signal: child.signalCode, scope, report, filesystem: proof });
      } finally { writeFileSync(join(output, `${phase}.log`), logs.join('')); await clean(); active = undefined; }
    }
    result.nativeVerified = true; result.passed = result.phases.length === 3;
  } catch (error) { result.errors.push(String(error?.message || error).slice(0, 2000)); }
  finally {
    await clean(); process.removeListener('SIGINT', interrupt); process.removeListener('SIGTERM', interrupt);
    writeJson(join(output, 'native-exit-result.json'), result);
  }
  console.log(JSON.stringify({ passed: result.passed, nativeVerified: result.nativeVerified, phases: result.phases.length, errors: result.errors, output }));
  return result;
}

if (import.meta.main) {
  assert.equal(process.argv.length, 3, 'Usage: node scripts/validate-native-exit-recovery-review.mjs <packaged-executable>');
  const result = await runNativeExitReview(process.argv[2]); if (!result.passed) process.exitCode = 1;
}
