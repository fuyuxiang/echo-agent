import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createHash, randomBytes } from 'node:crypto';
import { chmodSync, existsSync, lstatSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, unlinkSync, writeFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { DatabaseSync } from 'node:sqlite';
import { setTimeout as delay } from 'node:timers/promises';
import { pathToFileURL } from 'node:url';
import { cleanNativeReviewEnvironment } from './lib/native-review-isolation.mjs';
import { PRIVATE_REVIEW_MODEL_ID, seedReviewModel, captureReviewModelLogs, redactReviewModelSecrets } from './lib/native-review-model.mjs';
import { nativeReviewOutput } from './lib/native-review-output.mjs';
import { activateNativeReviewWindow, assertNativeReviewSessionUnlocked } from './lib/native-review-window.mjs';
import { confirmNativeReviewWorkspace } from './lib/native-core-workspace-picker.mjs';
import { readonlyCompatibilitySourceRevocations } from './lib/native-core-review-isolation.mjs';

export const COLLABORATION_CHECKS = Object.freeze({
  'first-launch': ['collaboration-isolation', 'group-create-ui', 'group-parallel-model-summary',
    'group-stream-stop', 'group-pause-resume-fresh-turn', 'scheduler-create-ui', 'scheduler-actual-due-model'],
  restart: ['collaboration-isolation', 'group-restart', 'scheduler-restart'],
});
export function collaborationEnvironment(base, legacy, phase) {
  assert(Object.hasOwn(COLLABORATION_CHECKS, phase));
  const env = cleanNativeReviewEnvironment(base);
  for (const key of Object.keys(env)) if (key.startsWith('ECHO_DESKTOP_REVIEW')
    || ['ECHO_AGENT_HOME', 'ECHO_DESKTOP_VALIDATION', 'ECHO_THEIA_NODE', 'NODE_OPTIONS', 'NODE_PATH'].includes(key)) delete env[key];
  return { ...env, GROK_HOME: legacy, ECHO_DESKTOP_REVIEW: '1', ECHO_DESKTOP_REVIEW_COLLABORATION: '1', ECHO_DESKTOP_REVIEW_PHASE: phase };
}
export function verifyCollaborationReport(report, scope, pid, startedAt) {
  assert.equal(report?.kind, 'native-collaboration'); assert.equal(report.version, 1);
  assert.equal(report.fakeIPC, false); assert.equal(report.passed, true, JSON.stringify(report.errors)); assert.deepEqual(report.errors, []);
  for (const key of ['home', 'nonce', 'phase']) assert.equal(report[key], scope[key]);
  assert.equal(report.nativeContext?.pid, pid); assert.equal(report.nativeContext.home, scope.home);
  assert.equal(report.nativeContext.phase, scope.phase); assert.equal(report.nativeContext.evaluations, 1);
  assert(Date.parse(report.finishedAt) >= startedAt && Date.parse(report.finishedAt) <= Date.now() + 1000);
  assert.deepEqual(report.checks.map(c => c.id), COLLABORATION_CHECKS[scope.phase]);
  assert(report.checks.every(c => c.result === 'passed'));
  const e = report.expected;
  for (const key of ['home', 'cwd', 'nonce', 'modelId']) assert.equal(e[key], scope[key]);
  assert(e.groupId && e.taskId && e.recordId && e.finalMarker && e.taskMarker && e.workCount >= 5);
  assert.equal(Date.parse(e.scheduledFor), e.dueEpoch);
  if (scope.phase === 'restart') assert.deepEqual(e, scope.expected);
  else {
    const evidence = id => report.checks.find(c => c.id === id).evidence;
    const parallel = evidence('group-parallel-model-summary').works;
    assert.equal(new Set(parallel.filter(w => !w.summary).map(w => w.memberId)).size, 2);
    assert(parallel.every(w => w.status === 'completed' && w.tokens > 0 && w.output.includes(`PARALLEL-${scope.nonce}`)));
    assert(parallel.some(w => w.summary));
    const stop = evidence('group-stream-stop');
    assert(stop.streamedCharacters >= 80 && stop.status === 'cancelled' && stop.quietMs >= 3500);
    assert.equal(evidence('scheduler-create-ui').manualRunInvoked, false);
    const fired = evidence('scheduler-actual-due-model');
    assert.equal(fired.record.id, e.recordId); assert.equal(fired.record.scheduledFor, e.scheduledFor);
    assert(Date.parse(fired.record.startedAt) >= e.dueEpoch);
    assert.equal(fired.record.modelId, scope.modelId); assert.equal(fired.record.cwd, scope.cwd);
    assert(fired.record.sessionId && fired.record.promptId && fired.output.text.includes(e.taskMarker));
  }
  return e;
}
function physicalProof(home, expected) {
  const db = new DatabaseSync(join(home, 'collaboration.sqlite'), { readOnly: true });
  let group, works, messages;
  try {
    group = JSON.parse(db.prepare('SELECT data FROM groups WHERE id=?').get(expected.groupId).data);
    works = db.prepare('SELECT data FROM work_items WHERE group_id=?').all(expected.groupId).map(r => JSON.parse(r.data));
    messages = db.prepare('SELECT data FROM messages WHERE group_id=?').all(expected.groupId).map(r => JSON.parse(r.data));
  } finally { db.close(); }
  assert.equal(group.cwd, expected.cwd); assert.equal(group.name, expected.groupName);
  assert.equal(works.length, expected.workCount); assert(works.every(w => ['completed', 'cancelled'].includes(w.status)));
  assert(messages.some(m => m.kind === 'assistant' && m.text.includes(expected.finalMarker)));
  const records = JSON.parse(readFileSync(join(home, 'echoagent-automation-records.json'))).records.filter(r => r.automationId === expected.taskId);
  assert.equal(records.length, 1); assert.equal(records[0].id, expected.recordId); assert.equal(records[0].status, 'success');
  assert.equal(records[0].scheduledFor, expected.scheduledFor); assert.equal(records[0].cwd, expected.cwd);
  const task = JSON.parse(readFileSync(join(home, 'echoagent-automations.json'))).automations.find(t => t.id === expected.taskId);
  assert.equal(task.status, 'PAUSED'); assert(!task.nextRunAt); assert.equal(task.pushToWeChat, false);
  return { groupId: group.id, workCount: works.length, messageCount: messages.length, statuses: works.map(w => w.status),
    taskId: task.id, recordId: records[0].id, scheduledFor: records[0].scheduledFor, startedAt: records[0].startedAt,
    sessionId: records[0].sessionId, promptId: records[0].promptId, modelId: records[0].modelId };
}
export async function runNativeCollaborationReview(executable) {
  assert.equal(process.platform, 'darwin'); assertNativeReviewSessionUnlocked();
  executable = realpathSync(resolve(executable)); assert(lstatSync(executable).isFile());
  const fixture = readFileSync(new URL('./fixtures/native-collaboration-review.js', import.meta.url)), binary = readFileSync(executable);
  assert(binary.includes(fixture), 'Rebuild the native executable with the current fixed collaboration fixture');
  const sha = bytes => createHash('sha256').update(bytes).digest('hex');
  const output = nativeReviewOutput('native-collaboration', 'NATIVE_COLLABORATION_REVIEW_OUTPUT');
  const home = realpathSync(mkdtempSync('/private/tmp/echo-native-review-collaboration-')); chmodSync(home, 0o700);
  const cwd = join(home, 'workspaces/default'), legacy = join(home, 'empty-legacy');
  mkdirSync(cwd, { recursive: true, mode: 0o700 }); mkdirSync(legacy, { mode: 0o700 });
  writeFileSync(join(home, 'config.toml'), `[marketplace]\nofficial_marketplace_auto_installed = true\ndisabled_sources = [${readonlyCompatibilitySourceRevocations().map(JSON.stringify).join(', ')}]\n[claude_compat]\nimported = true\n`, { mode: 0o600 });
  seedReviewModel(home);
  const result = { version: 1, kind: 'real-native-collaboration-driver', home, cwd, output, modelId: PRIVATE_REVIEW_MODEL_ID,
    binarySha256: sha(binary), fixtureSha256: sha(fixture), passed: false, nativeVerified: false, phases: [], errors: [], meetingRecordingTested: false };
  let active, logs, expected, interrupted = false;
  const interrupt = () => { interrupted = true; active?.kill('SIGTERM'); };
  process.once('SIGINT', interrupt); process.once('SIGTERM', interrupt);
  try {
    const nonce = randomBytes(16).toString('hex');
    for (const phase of ['first-launch', 'restart']) {
      assert(!interrupted, 'Owned collaboration journey interrupted');
      const scope = { schema: 1, home, cwd, nonce, phase, modelId: PRIVATE_REVIEW_MODEL_ID, expected: expected ?? null };
      writeFileSync(join(home, 'native-collaboration-manifest.private.json'), JSON.stringify(scope), { mode: 0o600 });
      const reportPath = join(home, 'native-review.json'); if (existsSync(reportPath)) unlinkSync(reportPath);
      const start = Date.now(); active = spawn(executable, ['--data-home', home], { cwd, env: collaborationEnvironment(process.env, legacy, phase), stdio: ['ignore', 'pipe', 'pipe'] });
      const child = active; let ended = false, code, signal, error;
      child.once('error', e => { error = e; ended = true; });
      const closed = new Promise(resolve => child.once('close', (c, s) => { code = c; signal = s; ended = true; resolve(); }));
      logs = captureReviewModelLogs(child, join(output, `${phase}-app.log`));
      writeFileSync(join(output, `${phase}-launch.json`), JSON.stringify({ pid: child.pid, ...scope }), { mode: 0o600 });
      let picked = false, nextActivate = 0;
      while (!ended && Date.now() - start < (phase === 'first-launch' ? 1100000 : 180000)) {
        if (Date.now() > nextActivate) { activateNativeReviewWindow(child.pid); nextActivate = Date.now() + 15000; }
        const ready = join(cwd, 'collaboration-workspace-picker-ready.json');
        if (phase === 'first-launch' && !picked && existsSync(ready)) {
          assert.deepEqual(JSON.parse(readFileSync(ready)), { nonce, cwd });
          picked = confirmNativeReviewWorkspace({ pid: child.pid, home, cwd, kind: 'collaboration' });
        }
        await Promise.race([closed, delay(250)]);
      }
      assert(ended, 'Owned native journey exceeded its deadline'); await closed; await logs.finished;
      if (error) throw error; assert(!logs.overflow && !logs.incomplete);
      const report = JSON.parse(readFileSync(reportPath));
      writeFileSync(join(output, `${phase}.json`), redactReviewModelSecrets(JSON.stringify(report, null, 2)), { mode: 0o600 });
      result.phases.push({ phase, pid: child.pid, exitCode: code, signal, seconds: (Date.now() - start) / 1000 });
      assert.equal(code, 0); assert.equal(signal, null);
      expected = verifyCollaborationReport(report, scope, child.pid, start);
      if (phase === 'first-launch') {
        assert(picked); assert.deepEqual(JSON.parse(readFileSync(join(home, 'echoagent-picked-workspaces.json'))), [cwd]);
        result.osPickerVerified = true;
      }
      result.phases.at(-1).physicalProof = physicalProof(home, expected); active = undefined;
    }
    result.expected = expected; result.nativeVerified = result.passed = true;
  } catch (error) { result.errors.push(String(error?.stack || error).slice(0, 6000)); }
  finally {
    if (active && active.exitCode === null && active.signalCode === null) {
      active.kill('SIGTERM'); await delay(1500); if (active.exitCode === null && active.signalCode === null) active.kill('SIGKILL');
    }
    await logs?.finished;
    process.removeListener('SIGINT', interrupt); process.removeListener('SIGTERM', interrupt);
    result.finishedAt = new Date().toISOString(); writeFileSync(join(output, 'result.json'), redactReviewModelSecrets(JSON.stringify(result, null, 2)), { mode: 0o600 });
  }
  console.log(JSON.stringify({ passed: result.passed, nativeVerified: result.nativeVerified, home, output, errors: result.errors })); return result;
}
if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  assert.equal(process.argv.length, 3, 'Pass a built native executable and an explicit NATIVE_REVIEW_MODEL_CONFIG');
  if (!(await runNativeCollaborationReview(process.argv[2])).passed) process.exitCode = 1;
}
