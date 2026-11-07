// Negative guards for the acceptance driver; these are not desktop evidence.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { collaborationEnvironment, COLLABORATION_CHECKS, verifyCollaborationReport } from './validate-native-collaboration-review.mjs';

function proof() {
  const now = Date.now(), nonce = 'a'.repeat(32), home = '/private/tmp/echo-native-review-collaboration-guards';
  const scope = { home, cwd: `${home}/workspaces/default`, nonce, phase: 'first-launch', modelId: 'echoagent-review/claude-opus-5-5', expected: null };
  const expected = { ...scope, groupId: 'group', taskId: 'task', recordId: 'record', workCount: 5,
    finalMarker: `RESUMED-${nonce}`, taskMarker: `SCHEDULED-${nonce}`, dueEpoch: now - 2000, scheduledFor: new Date(now - 2000).toISOString() };
  const record = { id: 'record', scheduledFor: expected.scheduledFor, startedAt: new Date(now - 1000).toISOString(),
    modelId: scope.modelId, cwd: scope.cwd, sessionId: 'session', promptId: 'prompt' };
  const data = {
    'group-parallel-model-summary': { works: [
      { memberId: 'a', status: 'completed', tokens: 10, output: `PARALLEL-${nonce}` },
      { memberId: 'b', status: 'completed', tokens: 12, output: `PARALLEL-${nonce}` },
      { memberId: 'c', summary: true, status: 'completed', tokens: 15, output: `PARALLEL-${nonce}` },
    ] },
    'group-stream-stop': { streamedCharacters: 100, status: 'cancelled', quietMs: 3500 },
    'scheduler-create-ui': { manualRunInvoked: false },
    'scheduler-actual-due-model': { record, output: { text: expected.taskMarker } },
  };
  const report = { kind: 'native-collaboration', version: 1, fakeIPC: false, passed: true, errors: [],
    home, nonce, phase: scope.phase, nativeContext: { pid: 24680, home, phase: scope.phase, evaluations: 1 },
    finishedAt: new Date(now).toISOString(), expected,
    checks: COLLABORATION_CHECKS['first-launch'].map(id => ({ id, result: 'passed', evidence: data[id] ?? {} })) };
  return { scope, report, now, evidence: id => report.checks.find(c => c.id === id).evidence };
}
test('launch environment removes ambient credentials and competing native modes', () => {
  const base = { HOME: '/real/os-home', OPENAI_API_KEY: 'foreign', ANTHROPIC_API_KEY: 'foreign', ECHO_DESKTOP_REVIEW_BROWSER: '1',
    ECHO_AGENT_HOME: '/production', NODE_OPTIONS: '--require=foreign', HTTP_PROXY: 'http://127.0.0.1:1234' };
  const env = collaborationEnvironment(base, '/private/tmp/owned-empty', 'restart');
  assert.equal(env.HOME, base.HOME); assert.equal(env.HTTP_PROXY, base.HTTP_PROXY);
  assert.equal(env.ECHO_DESKTOP_REVIEW_COLLABORATION, '1'); assert.equal(env.ECHO_DESKTOP_REVIEW_PHASE, 'restart');
  for (const key of ['OPENAI_API_KEY', 'ANTHROPIC_API_KEY', 'ECHO_AGENT_HOME', 'ECHO_DESKTOP_REVIEW_BROWSER', 'NODE_OPTIONS']) assert(!Object.hasOwn(env, key));
  assert.equal(base.ECHO_AGENT_HOME, '/production');
});
test('report guard accepts its complete typed evidence', () => {
  const p = proof(); assert.equal(verifyCollaborationReport(p.report, p.scope, 24680, p.now - 3000), p.report.expected);
});
for (const [name, mutate] of [
  ['foreign native PID', p => { p.report.nativeContext.pid++; }],
  ['stale renderer report', p => { p.report.finishedAt = new Date(p.now - 10000).toISOString(); }],
  ['missing group check', p => { p.report.checks.splice(2, 1); }],
  ['one member presented as two experts', p => { p.evidence('group-parallel-model-summary').works[1].memberId = 'a'; }],
  ['stop before actual streamed content', p => { p.evidence('group-stream-stop').streamedCharacters = 0; }],
  ['manual run masquerading as scheduled execution', p => { p.evidence('scheduler-create-ui').manualRunInvoked = true; }],
  ['run without native scheduled occurrence', p => { p.evidence('scheduler-actual-due-model').record.scheduledFor = null; }],
  ['early run before wall clock due time', p => { p.evidence('scheduler-actual-due-model').record.startedAt = new Date(p.now - 3000).toISOString(); }],
  ['foreign model execution', p => { p.evidence('scheduler-actual-due-model').record.modelId = 'foreign/model'; }],
  ['missing real model output marker', p => { p.evidence('scheduler-actual-due-model').output.text = 'unrelated'; }],
]) test(`rejects ${name}`, () => {
  const p = proof(); mutate(p); assert.throws(() => verifyCollaborationReport(p.report, p.scope, 24680, p.now - 3000));
});
