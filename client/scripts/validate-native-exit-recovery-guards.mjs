// Synthetic admission guards only. No application, model, native IPC or home.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import { verifyNativeExitReport } from './validate-native-exit-recovery-review.mjs';

const checks = {
  'init-failure': ['native-exit-isolation', 'native-init-failure-visible', 'native-no-recording', 'native-init-failure-graceful-request'],
  'flush-failure': ['native-exit-isolation', 'native-dirty-plan-update', 'native-save-failure-retained', 'native-runtime-recovered', 'native-final-graceful-request'],
  restart: ['native-exit-isolation', 'native-restart-plan-restored', 'native-final-graceful-request'],
};
const build = (phase = 'flush-failure') => {
  const scope = { schema: 1, home: '/private/tmp/echo-native-review-exit-synthetic', cwd: '/private/tmp/echo-native-review-exit-synthetic/workspaces/default', nonce: 'a'.repeat(32), phase, expectedSessionId: phase === 'restart' ? 'synthetic-session-1' : null };
  const context = { scope, pid: 42, evaluations: 1, hasRuntimeSender: false, hasInitOutcome: false };
  const evidence = {
    'native-init-failure-visible': {
      error: '初始化未完成：synthetic-invalid-runtime-memory-type 重试初始化',
      context,
    },
    'native-no-recording': { count: 0, listenerReadyObserved: true },
    'native-dirty-plan-update': { sessionId: 'synthetic-session-1', sessionUpdate: 'current_mode_update', currentModeId: 'plan' },
    'native-save-failure-retained': { context: { hasRuntimeHandle: true, lifecycle: { exitReady: false, exitInProgress: false }, shutdownFailure: 'Permission denied' }, failure: { message: '未退出，应用已保留：保存失败' }, failureVisible: true },
    'native-runtime-recovered': { context: { hasRuntimeSender: true, hasInitOutcome: true, initGeneration: 2 }, initialInitGeneration: 1, sameSessionId: 'synthetic-session-1', afterPhysicalRepair: true },
    'native-restart-plan-restored': { sessionId: 'synthetic-session-1', cwd: scope.cwd, selectedInUi: true, update: { currentModeId: 'plan' }, noModelTurnSent: true },
  };
  const report = { kind: 'real-native-exit-review', passed: true, errors: [], phase, nonce: scope.nonce, home: scope.home, cwd: scope.cwd, pid: 42, nativeContext: context, sessionId: phase === 'init-failure' ? undefined : 'synthetic-session-1', finishedAt: new Date().toISOString(), checks: checks[phase].map((id) => ({ id, result: 'passed', evidence: evidence[id] ?? {} })) };
  return { report, scope, pid: 42, startedAt: Date.now() - 1000, exitCode: 0, signal: null };
};
const results = [];
const rejected = (name, change, phase) => {
  const input = build(phase); change(input);
  assert.throws(() => verifyNativeExitReport(input)); results.push({ name, passed: true, expected: 'rejected' });
};
rejected('wrong-process', (input) => { input.pid = 43; });
rejected('old-report', (input) => { input.report.finishedAt = new Date(0).toISOString(); });
rejected('signal-is-not-graceful', (input) => { input.signal = 'SIGTERM'; });
rejected('incomplete-faulted-exit', (input) => { input.report.passed = false; });
rejected('no-real-dirty-update', (input) => { input.report.checks = input.report.checks.filter((check) => check.id !== 'native-dirty-plan-update'); });
rejected('discarded-save-handle', (input) => { input.report.checks.find((check) => check.id === 'native-save-failure-retained').evidence.context.hasRuntimeHandle = false; });
rejected('runtime-still-offline', (input) => { input.report.checks.find((check) => check.id === 'native-runtime-recovered').evidence.context.hasRuntimeSender = false; });
rejected('runtime-not-replaced', (input) => { input.report.checks.find((check) => check.id === 'native-runtime-recovered').evidence.context.initGeneration = 1; });
rejected('foreign-restart-session', (input) => { input.report.sessionId = 'foreign'; }, 'restart');
rejected('no-init-failure', (input) => { input.report.checks.find((check) => check.id === 'native-init-failure-visible').evidence.context.hasInitOutcome = true; }, 'init-failure');
for (const phase of Object.keys(checks)) { assert.equal(verifyNativeExitReport(build(phase)), true); results.push({ name: `${phase}-complete-synthetic-admission`, passed: true, expected: 'admitted' }); }
const calls = [];
const bad = build('init-failure').report.nativeContext;
bad.scope.home = '/outside/echo-native-review-exit-synthetic'; bad.scope.cwd = `${bad.scope.home}/workspaces/default`;
const sandbox = { window: { __TAURI_INTERNALS__: { invoke: async (command) => { calls.push(command); assert.equal(command, 'native_exit_review_context'); return bad; } } }, setTimeout, Date };
vm.createContext(sandbox);
vm.runInContext(readFileSync(new URL('./fixtures/native-exit-recovery-review.js', import.meta.url), 'utf8'), sandbox);
const refused = await sandbox.window.__echoNativeExitRecoveryReview();
assert.equal(refused.passed, false); assert(calls.length === 1);
results.push({ name: 'fixed-factory-invalid-scope-no-mutation-or-exit', passed: true, expected: 'rejected before any operation' });
// A Runtime handle/sender may exist before model synchronization is ready.
// Execute the actual fixed factory and verify it cannot create a session in
// that interval. This uses explicit synthetic IPC, never a Native pass report.
const readyInput = build(), readinessCalls = [];
let authChecks = 0;
const readyContext = { ...readyInput.report.nativeContext, hasRuntimeSender: true,
  hasInitOutcome: true, initGeneration: 1, lifecycle: { listenerReady: true } };
const readySandbox = { window: { __TAURI_INTERNALS__: {
  transformCallback: () => 1,
  invoke: async (command) => {
    readinessCalls.push(command);
    if (command === 'native_exit_review_context') return readyContext;
    if (command === 'agent_auth_status') {
      authChecks++;
      return { ready: authChecks > 1, runtimeReady: true, synchronized: authChecks > 1 };
    }
    if (command === 'plugin:event|listen') return 1;
    if (command === 'agent_list_all_sessions') { assert(authChecks > 1); return []; }
    if (command === 'agent_new_session') { assert(authChecks > 1); return 'synthetic-session-1'; }
    if (command === 'agent_rename_session') return;
    if (command === 'write_text_file') throw new Error('synthetic guard stops before the physical fault barrier');
    if (command === 'plugin:event|unlisten' || command === 'desktop_review_complete') return;
    throw new Error(`Unexpected synthetic readiness command: ${command}`);
  },
} }, setTimeout, Date, document: {} };
vm.createContext(readySandbox);
vm.runInContext(readFileSync(new URL('./fixtures/native-exit-recovery-review.js', import.meta.url), 'utf8'), readySandbox);
const gated = await readySandbox.window.__echoNativeExitRecoveryReview();
assert.equal(gated.passed, false);
assert(authChecks >= 2 && readinessCalls.includes('agent_new_session'));
results.push({ name: 'fixed-factory-waits-for-real-auth-readiness-before-session', passed: true,
  expected: 'sender presence alone does not admit session creation', nativeVerified: false });
console.log(JSON.stringify({ kind: 'synthetic native exit report guards; not actual native acceptance', passed: true, guardCount: results.length, applicationStarted: false, nativeIPCUsed: false, results }, null, 2));
