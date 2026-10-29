// Synthetic admission checks only; never substitutes for native acceptance.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { runInNewContext } from 'node:vm';
import { BROWSER_CHECKS, verifyBrowserReport } from './validate-native-browser-review.mjs';
const now = Date.now(), home = '/private/tmp/echo-native-review-browser-synthetic', nonce = 'a'.repeat(32);
const manifest = { home, cwd: home + '/workspaces/default', nonce, phase: 'first-launch',
  url: `http://127.0.0.1:34567/review/${nonce}/`, modelId: 'echoagent-ojlab/Echo Reed' };
const base = { version: 1, kind: 'native-browser', passed: true, nativeVerified: false, fakeIPC: false,
  home, nonce, phase: manifest.phase, errors: [], finishedAt: new Date().toISOString(),
  nativeContext: { home, pid: 123, browserRequested: true, evaluations: 1 },
  checks: BROWSER_CHECKS['first-launch'].map(id => ({ id, result: 'passed', evidence: { syntheticAdmissionOnly: true } })),
  expected: { schema: 1, home, cwd: manifest.cwd, nonce, url: manifest.url, sessionId: 'synthetic-session',
    modelId: manifest.modelId,
    download: { passed: true, sessionId: 'synthetic-session' } },
  approvals: [ { action: 'automation_set_private_network', approved: true }, { action: 'browser_type', approved: true },
    { action: 'browser_click', approved: true }, { action: 'browser_click', approved: true }, { action: 'browser_click', approved: false } ] };
assert.deepEqual(verifyBrowserReport(base, manifest, 123, now), base.expected);
let count = 1;
for (const [name, mutate] of [
  ['wrong-pid', value => value.nativeContext.pid++], ['stale', value => value.finishedAt = new Date(0).toISOString()],
  ['wrong-home', value => value.home = '/outside'], ['wrong-nonce', value => value.nonce = 'b'.repeat(32)],
  ['fake-ipc', value => value.fakeIPC = true], ['renderer-native-claim', value => value.nativeVerified = true],
  ['missing-check', value => value.checks.pop()], ['duplicate-check', value => value.checks.at(-1).id = value.checks[0].id],
  ['failure', value => value.checks[0].result = 'failed'], ['error', value => value.errors.push('real failure')],
  ['no-download', value => value.expected.download.passed = false], ['wrong-download-session', value => value.expected.download.sessionId = 'another'],
  ['delete-approved', value => value.approvals.at(-1).approved = true], ['no-network-confirmation', value => value.approvals.shift()],
  ['wrong-bound-model', value => value.expected.modelId = 'echoagent-ojlab/Echo Grove'],
]) {
  const value = structuredClone(base); mutate(value);
  assert.throws(() => verifyBrowserReport(value, manifest, 123, now), undefined, `Accepted ${name}`); count++;
}
let calls = 0;
const context = { window: { __TAURI_INTERNALS__: { invoke() { calls++; throw new Error('No native operation permitted'); } } },
  Date, Promise, Error, String, setTimeout, clearTimeout };
runInNewContext(readFileSync('scripts/fixtures/native-browser-review.js', 'utf8'), context);
const rejected = await context.window.__echoNativeBrowserReview({ home: '/outside', nonce });
assert.equal(rejected.passed, false); assert.equal(calls, 0); assert(rejected.errors.some(value => value.includes('Invalid private'))); count++;
console.log(JSON.stringify({ passed: true, syntheticGuards: count, nativeVerified: false, nativeInvocations: calls }));
// Exercise the exact fixed factory's permission envelope independently of DOM
// or IPC. A matching suffix from another MCP server must never be approved.
const source = readFileSync('scripts/fixtures/native-browser-review.js', 'utf8');
const begin = source.indexOf('const allowedOperation = request => '), end = source.indexOf('  const service = async () =>', begin);
assert(begin >= 0 && end > begin);
const expression = source.slice(begin + 'const allowedOperation = '.length, end).trim().replace(/;$/, '');
const allowed = runInNewContext(`(${expression})`, { sessionId: 'owned', nonce,
  scope: { cwd: manifest.cwd, url: manifest.url }, desired: 'submit-download' });
const request = (tool_name, tool_input, sessionId = 'owned') => ({ sessionId, rawInput: { variant: 'UseTool', tool_name, tool_input } });
assert.equal(allowed(request('echoagent-automation__browser_start', {}))?.name, 'browser_start');
for (const server of ['another-server', 'foreign__echoagent-automation', 'echoagent-automation-fake']) {
  assert.equal(allowed(request(`${server}__browser_start`, {})), null, `Approved foreign MCP server ${server}`);
}
assert.equal(allowed(request('echoagent-automation__browser_start', {}, 'another-session')), null);
assert.equal(allowed(request('echoagent-automation__browser_navigate', { url: 'http://127.0.0.1:9999/foreign' })), null);
assert.equal(allowed(request('echoagent-automation__browser_click', { x: 1, y: 2 })), null);
assert.equal(allowed(request('echoagent-automation__browser_type', { elementRef: 'e1', text: 'foreign text' })), null);
console.log(JSON.stringify({ permissionEnvelopeGuards: 8, passed: true, nativeVerified: false }));
