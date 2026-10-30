import { nativeReviewOutput } from './lib/native-review-output.mjs';
// Synthetic admission tests only. No Native app, network, model, or user data.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFileSync, writeFileSync, mkdirSync } from 'node:fs';
import { join, resolve } from 'node:path';
import vm from 'node:vm';
import ts from 'typescript';
import { CORE_CHECKS, coreReviewProjectName, validateCoreManifest, verifyNativeCoreReport } from './validate-native-core-review.mjs';

const home = '/private/tmp/echo-native-review-core-synthetic', nonce = 'a'.repeat(32), cwd = `${home}/workspaces/default`;
const fixtures = ['xlsx', 'epub', 'ods'].map((format) => ({ format, path: `${cwd}/办公读取/中文 ${format}.${format}`,
  marker: `CORE-${format.toUpperCase()}-${nonce}`, ...(format === 'epub' ? { excludedMarker: `CORE-NAV-EXCLUDED-${nonce}` } : {}) }));
const expected = { version: 1, home, cwd, nonce, modelId: 'echoagent-ojlab/Echo Reed', officeFixtures: fixtures,
  sessionId: 'synthetic-parent', forkedSessionId: 'synthetic-fork', projectForkedSessionId: 'synthetic-project-fork', deletedSessionId: 'synthetic-deleted',
  ownedSessionIds: ['synthetic-parent', 'synthetic-fork', 'synthetic-project-fork', 'synthetic-deleted'], title: `隔离核心会话-${nonce}`,
  project: { id: 'synthetic-project', name: coreReviewProjectName(nonce), cwd: `${home}/workspaces/project`, initialPlans: '[]', initialTasks: '[]' },
  markers: { recovery: `CORE-RECOVER-${nonce}`, queued: `CORE-QUEUED-${nonce}`, project: `CORE-PROJECT-${nonce}` },
  child: { sessionId: 'synthetic-child', marker: `CORE-CHILD-${nonce}` },
  customModel: { providerId: 'custom-1', modelId: 'custom-1/chat-qwen', label: `隔离个人Qwen-${nonce}`, marker: `CORE-CUSTOM-${nonce}` } };
const fixtureSource = readFileSync('scripts/fixtures/native-core-review.js', 'utf8');
const projectExpression = fixtureSource.match(/const name = ([^\n;]+);/)?.[1];
assert(projectExpression, 'Actual fixed project fixture expression missing');
assert.equal(vm.runInNewContext(projectExpression, { nonce }), expected.project.name,
  'Actual native project name and parent admission contract differ');
const manifest = (phase) => ({ schema: 1, home, cwd, nonce, phase, modelId: expected.modelId, officeFixtures: fixtures,
  expected: phase === 'restart' ? expected : null });
const turn = (id, reason = 'end_turn', trigger) => ({ sessionId: expected.sessionId, promptId: id, stopReason: reason, ...(trigger ? { cancelTrigger: trigger } : {}) });
const report = (phase) => {
  const r = { version: 1, kind: 'native-core', phase, home, cwd, nonce, nativeContext: { pid: 12345, home, phase, coreRequested: true, evaluations: 1 },
    finishedAt: new Date().toISOString(), passed: true, errors: [], fakeIPC: false, osPickerVerified: false, nativeVerified: false,
    expected: structuredClone(expected), permissions: [], checks: CORE_CHECKS[phase].map((id) => ({ id, result: 'passed', evidence: {} })) };
  const put = (id, evidence) => { r.checks.find((check) => check.id === id).evidence = evidence; };
  put('core-native-isolation', { permissionMode: 'ask' });
  if (phase === 'first-launch') {
    put('core-queue-and-send-now', { sessionId: expected.sessionId, admittedViaUi: true, immediateBeforeQueued: true, noDuplicate: true,
      completions: [turn('slow-1', 'cancelled', 'send_now'), turn('immediate-1'), turn('queued-1')] });
    put('core-stop-and-resend', { stopped: turn('slow-2', 'cancelled', 'stop'), lateWaitMs: 1000,
      recovery: { marker: expected.markers.recovery, completion: turn('recovery-1') } });
    put('core-automation-draft-navigation-exit', { marker: `CORE-DRAFT-${nonce}`, restoredViaNavigation: true,
      quitConfirmationViaUi: true, exitCancelledAndRuntimeReady: true, noTasksCreated: true, discardedViaUi: true });
    put('core-custom-qwen-connection', { testedAndSavedViaUi: true, explicitHttpConsent: true, noCredentials: true,
      actualSessionModelId: expected.customModel.modelId, response: { modelId: expected.customModel.modelId,
        marker: expected.customModel.marker, completion: turn('custom-1') } });
    put('core-project-move-switch-ui', { projectForkedSessionId: expected.projectForkedSessionId, forkPendingBeforeTurn: true,
      forkPendingAfterTurn: false, sourcePendingUnchanged: true, projectTurn: { sessionId: expected.projectForkedSessionId,
        marker: expected.markers.project, completion: { ...turn('project-fork-turn'), sessionId: expected.projectForkedSessionId } } });
    for (const item of fixtures) put(`core-office-${item.format}-runtime-read`, { path: item.path, toolOutputFingerprint: true, completion: turn(`read-${item.format}`) });
  } else put('core-restart-custom-model', { modelId: expected.customModel.modelId, selectedSessionId: expected.sessionId });
  return r;
};
const args = (phase = 'first-launch') => ({ report: report(phase), manifest: manifest(phase), pid: 12345, startedAt: Date.now() - 1000,
  exitCode: 0, signal: null, previousExpected: expected });
const cases = [];
{
  // Run the actual fixed fixture predicate against production display names.
  // A stale route/display name must fail offline before a real draft guard can
  // retain the app during an expensive native/model acceptance run.
  const navigation = readFileSync('src/lib/capability-navigation.ts', 'utf8');
  const labelLiteral = navigation.match(/CAPABILITY_SECTION_LABEL\s*=\s*("[^"\n]+")/)?.[1];
  assert(labelLiteral, 'Production capability navigation display label missing');
  const label = JSON.parse(labelLiteral);
  // Inspect the real menu contract rather than a particular JSX expression.
  // Sidebar.test.tsx separately exercises its rendered label and navigation.
  const menu = ts.createSourceFile('sidebar-menu.tsx', readFileSync('src/lib/sidebar-menu.tsx', 'utf8'),
    ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
  let definition;
  const visit = node => {
    if (ts.isVariableDeclaration(node) && node.name.getText(menu) === 'SIDEBAR_MENU_ITEMS') definition = node.initializer;
    ts.forEachChild(node, visit);
  };
  visit(menu);
  while (definition && (ts.isAsExpression(definition) || ts.isSatisfiesExpression(definition))) definition = definition.expression;
  const property = (object, name) => object && ts.isObjectLiteralExpression(object)
    ? object.properties.find(item => ts.isPropertyAssignment(item) && item.name.getText(menu) === name)?.initializer
    : undefined;
  assert(definition, 'Production Sidebar menu definition missing');
  const capability = property(definition, 'capabilities');
  assert(capability, 'Production capability menu entry missing');
  const display = property(capability, 'label'), route = property(capability, 'route');
  assert(display && (ts.isStringLiteral(display) ? display.text === label
    : ts.isIdentifier(display) && display.text === 'CAPABILITY_SECTION_LABEL'),
  'Production capability menu display label differs from the native navigation contract');
  assert(route && ts.isStringLiteral(route) && route.text === '专家·技能·连接器',
    'Production capability menu route differs from the native navigation contract');
  const predicate = fixtureSource.match(/\.find\((node => visible\(node\) && node\.textContent\.trim\(\) === '[^']+')\), 'leave automation via actual navigation'/)?.[1];
  assert(predicate, 'Actual automation-draft leave navigation predicate missing');
  const matches = vm.runInNewContext(`(${predicate})`, { visible: node => node.visible });
  const nodes = [{ visible: false, textContent: label }, { visible: true, textContent: '扩展' },
    { visible: true, textContent: `  ${label}  ` }];
  assert.equal(nodes.find(matches), nodes[2], 'Actual draft leave selector differs from the production visible Sidebar label');
  cases.push({ name: 'draft navigation uses current production capability display label', passed: true,
    label, actualNative: false });
}
{
  const continuationExpression = fixtureSource.match(/const canContinueAfterFailure = ([^\n;]+);/)?.[1];
  assert(continuationExpression, 'Actual failed-stage continuation predicate missing');
  const mayContinue = vm.runInNewContext(`(${continuationExpression})`);
  for (const status of ['completed', 'streaming', 'failed', 'stopped', 'paused', undefined]) {
    for (const queueEmpty of [true, false]) {
      assert.equal(mayContinue(status, queueEmpty), status === 'completed' && queueEmpty);
    }
  }
  cases.push({ name: 'failed model stages continue only from completed session and empty queue', passed: true, actualNative: false });

  const recordAndStage = fixtureSource.slice(fixtureSource.indexOf('  const record ='), fixtureSource.indexOf('  const set ='));
  const overallExpression = fixtureSource.match(/report\.passed = ([\s\S]*?);\s*\n\s*report\.finishedAt/)?.[1];
  assert(recordAndStage.includes('report.errors.push') && overallExpression, 'Actual failure recorder or final admission expression missing');
  const failed = await vm.runInNewContext(`(async () => {
    const report = { checks: [], errors: [] }, approved = true;
    const required = ['core-qwen-initial-response', 'synthetic-local-continuation'];
    ${recordAndStage}
    await stage(required[0], async () => { throw new Error('Exact model marker mismatch'); });
    if ((${continuationExpression})('completed', true)) await stage(required[1], async () => ({ localCoverage: true }));
    report.passed = ${overallExpression};
    return report;
  })()`);
  assert.equal(failed.checks.length, 2);
  assert.equal(failed.checks[0].result, 'failed');
  assert.equal(failed.checks[1].result, 'passed');
  assert.equal(failed.errors.length, 1);
  assert.equal(failed.passed, false, 'Continuing local coverage must never promote a bad model marker to overall pass');
  cases.push({ name: 'bad model marker stays failed with error after later local coverage succeeds', passed: true, actualNative: false });

  const markerAssertion = fixtureSource.match(/assert\(reply\(\)\.includes\(m\.queued\), 'Queued Qwen reply missing exact marker'\);/)?.[0];
  assert(markerAssertion, 'Strict queued reply marker assertion was removed');
  assert.throws(() => vm.runInNewContext(markerAssertion, { assert, reply: () => 'wrong nonce', m: { queued: 'required-exact-nonce' } }));
  vm.runInNewContext(markerAssertion, { assert, reply: () => 'required-exact-nonce', m: { queued: 'required-exact-nonce' } });
  cases.push({ name: 'completed empty queue does not bypass exact queued reply marker assertion', passed: true, actualNative: false });
}
function rejects(name, change) { const value = args(); change(value); assert.throws(() => verifyNativeCoreReport(value)); cases.push({ name, passed: true }); }
rejects('wrong process PID', (v) => { v.report.nativeContext.pid++; });
rejects('stale prior report', (v) => { v.report.finishedAt = '2000-01-01T00:00:00Z'; });
rejects('SIGTERM is not graceful success', (v) => { v.exitCode = null; v.signal = 'SIGTERM'; });
rejects('partial model failure cannot pass overall', (v) => { v.report.checks[3].result = 'failed'; });
rejects('duplicate/missing mandatory check', (v) => { v.report.checks[3] = v.report.checks[2]; });
rejects('fake IPC renderer claim', (v) => { v.report.fakeIPC = true; });
rejects('renderer cannot qualify parent Native readback', (v) => { v.report.nativeVerified = true; });
rejects('foreign session ID is rejected', (v) => { v.report.expected.deletedSessionId = 'foreign-session'; });
rejects('queue cancelled for wrong trigger', (v) => { v.report.checks.find((c) => c.id === 'core-queue-and-send-now').evidence.completions[0].cancelTrigger = 'stop'; });
rejects('duplicated queued completion cannot masquerade as two turns', (v) => { const c = v.report.checks.find((c) => c.id === 'core-queue-and-send-now').evidence.completions; c[2].promptId = c[1].promptId; });
rejects('failed stop recovery is not completed', (v) => { v.report.checks.find((c) => c.id === 'core-stop-and-resend').evidence.recovery.completion.stopReason = 'error'; });
rejects('unknown permission scope is rejected', (v) => { v.report.permissions.push({ sessionId: expected.sessionId, via: 'production allow-once UI', operation: { name: 'bash', command: 'echo' } }); });
rejects('Office reply alone is not real tool output', (v) => { v.report.checks.find((c) => c.id === 'core-office-epub-runtime-read').evidence.toolOutputFingerprint = false; });
rejects('custom model HTTP consent cannot be fabricated as omitted', (v) => { v.report.checks.find((c) => c.id === 'core-custom-qwen-connection').evidence.explicitHttpConsent = false; });
rejects('draft quit must preserve real Runtime after cancellation', v => { v.report.checks.find(c => c.id === 'core-automation-draft-navigation-exit').evidence.exitCancelledAndRuntimeReady = false; });
rejects('builtin selector alone does not prove custom switch', (v) => { v.report.expected.customModel.modelId = expected.modelId; });
for (const phase of ['first-launch', 'restart']) { verifyNativeCoreReport(args(phase)); cases.push({ name: `valid synthetic ${phase} admission only`, passed: true, actualNative: false }); }
{
  const value = args('restart'); value.report.expected.title += '-changed'; assert.throws(() => verifyNativeCoreReport(value));
  cases.push({ name: 'restart exact prior expectation required', passed: true });
}
for (const mutate of [(m) => { m.cwd = '/private/tmp/foreign'; }, (m) => { m.officeFixtures[0].path = '/private/tmp/foreign.xlsx'; },
  (m) => { m.script = 'arbitrary'; }, (m) => { m.expected = {}; }, (m) => { m.officeFixtures[0].command = 'arbitrary'; }]) {
  const m = structuredClone(manifest('first-launch')); mutate(m); assert.throws(() => validateCoreManifest(m));
}
cases.push({ name: 'fixed manifest rejects foreign cwd/file and arbitrary script', passed: true });
const factoryPath = resolve('scripts/fixtures/native-core-review.js'), bytes = readFileSync(factoryPath);
for (const [name, identity, nativeContext] of [
  ['invalid home before IPC', { home: '/private/tmp/default', nonce }, null],
  ['unqualified native context before mutation', { home, nonce }, { pid: 12345, home, phase: 'first-launch', coreRequested: false, evaluations: 1 }],
]) {
  const calls = [], window = { __TAURI_INTERNALS__: { invoke: async (command) => { calls.push(command);
    if (command === 'desktop_review_context') return nativeContext;
    if (command === 'desktop_review_core_manifest') return manifest('first-launch');
    throw new Error(`Forbidden synthetic invocation ${command}`); } } };
  const context = vm.createContext({ window, setTimeout, clearTimeout, Date, Error, Object, Array, Set, Map, Promise, Number, JSON, console: undefined });
  vm.runInContext(bytes.toString('utf8'), context, { filename: factoryPath });
  const result = await window.__echoNativeCoreReview(identity); assert.equal(result.passed, false);
  assert(calls.every((command) => ['desktop_review_context', 'desktop_review_core_manifest'].includes(command)));
  assert(!calls.includes('desktop_review_complete'));
  cases.push({ name: `real fixed factory ${name}: zero mutation/exit`, passed: true, commands: calls });
}
const output = process.argv[2] ? resolve(process.argv[2]) : join(nativeReviewOutput('native-core-guards'), 'result.json');
mkdirSync(resolve(output, '..'), { recursive: true });
writeFileSync(output, JSON.stringify({ version: 1, kind: 'synthetic-native-core-admission-guards', actualNative: false,
  modelCalled: false, appLaunched: false, productionIPC: false, passed: true, count: cases.length, cases,
  factorySha256: createHash('sha256').update(bytes).digest('hex'), recordedAt: new Date().toISOString() }, null, 2));
console.log(JSON.stringify({ passed: true, syntheticGuards: cases.length, actualNative: false, output }));
