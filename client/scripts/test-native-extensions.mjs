// Synthetic protocol/scope guards, separate from actual desktop acceptance.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdtempSync, chmodSync, readFileSync, realpathSync, rmSync, existsSync } from 'node:fs';
import { join } from 'node:path';
import { test } from 'node:test';
import { createExtensionResources, extensionsEnvironment, EXTENSIONS_CHECKS, verifyExtensionsReport } from './validate-native-extensions-review.mjs';
import { nativeExtensionsAction } from './lib/native-extensions-actions.mjs';
import { assertSupportedNodeVersion } from './node-runtime-version.mjs';

test('extensions use the same Node 22 / 24 contract as the package and CI', () => {
  for (const version of ['22.23.4', '22.24.0', '24.22.1', '24.23.0']) assert.doesNotThrow(() => assertSupportedNodeVersion(version));
  for (const version of ['20.19.0', '22.23.3', '23.11.0', '24.22.0']) assert.throws(() => assertSupportedNodeVersion(version));
});

test('extension launcher rejects ambient credentials and competing review modes', () => {
  const env = extensionsEnvironment({ HOME: '/real/home', OPENAI_API_KEY: 'foreign', ANTHROPIC_API_KEY: 'foreign',
    ECHO_AGENT_HOME: '/user/profile', ECHO_DESKTOP_REVIEW_CORE: '1', NODE_OPTIONS: '--require=foreign' }, '/owned/empty', 'restart');
  assert.equal(env.HOME, '/real/home'); assert.equal(env.ECHO_DESKTOP_REVIEW_EXTENSIONS, '1');
  for (const k of ['OPENAI_API_KEY', 'ANTHROPIC_API_KEY', 'ECHO_AGENT_HOME', 'ECHO_DESKTOP_REVIEW_CORE', 'NODE_OPTIONS']) assert(!Object.hasOwn(env, k));
  assert.throws(() => extensionsEnvironment({}, '/owned/empty', 'arbitrary-script'));
});
function reportProof() {
  const now = Date.now(), home = '/private/tmp/echo-native-review-extensions-proof', nonce = 'c'.repeat(32), cwd = `${home}/workspaces/default`;
  const scope = { home, cwd, nonce, phase: 'first-launch', modelId: 'echoagent-review/claude-opus-5-5' };
  const e = { ...scope, sessionId: 'owned-session', installedSkillPath: `${home}/skills/native-review-skill`, pluginMarker: 'PLUGIN-owned', skillMarker: 'SKILL-owned' };
  const read = (id, path, marker) => [{ toolCallId: id, rawInput: { variant: 'ReadFile', target_file: path } },
    { toolCallId: id, status: 'completed', rawOutput: { FileContent: { absolute_path: path, raw_output: marker } } }];
  const plugin = read('plugin-read', `${cwd}/extension-fixtures/plugin/skills/plugin-marker/SKILL.md`, e.pluginMarker);
  const managed = [...read('managed-read', `${e.installedSkillPath}/SKILL.md`, e.skillMarker),
    { toolCallId: 'mcp-call', rawInput: { variant: 'UseTool', tool_name: 'native-extensions__echo_deliver', tool_input: { nonce, phase: 'first' } } },
    { toolCallId: 'mcp-call', status: 'completed' }];
  for (const row of plugin) { row.sessionId = e.sessionId; row._meta = { promptId: 'plugin-turn' }; }
  for (const row of managed) { row.sessionId = e.sessionId; row._meta = { promptId: 'managed-turn' }; }
  const task = { plugin: { tools: plugin, completion: { sessionId: e.sessionId, promptId: 'plugin-turn' } },
    managed: { tools: managed, completion: { sessionId: e.sessionId, promptId: 'managed-turn' } } };
  const report = { kind: 'native-extensions', version: 1, fakeIPC: false, passed: true, errors: [], home, nonce, phase: scope.phase,
    nativeContext: { pid: 24680, home, evaluations: 1 }, finishedAt: new Date(now).toISOString(), expected: e,
    checks: EXTENSIONS_CHECKS['first-launch'].map(id => ({ id, result: 'passed', evidence: id === 'extensions-real-skill-plugin-mcp-task' ? task : {} })),
    permissions: [{ toolCallId: 'mcp-call', args: { nonce, phase: 'first' }, sessionId: e.sessionId, pendingBeforeApproval: true, via: 'production allow-once UI' }] };
  return { report, scope, now, task };
}
test('acceptance proof binds both actual skill reads and the approved MCP call', () => {
  const p = reportProof(); assert.equal(verifyExtensionsReport(p.report, p.scope, 24680, p.now - 1000), p.report.expected);
});
for (const [label, change] of [
  ['reply marker without an actual skill read', p => { p.task.plugin.tools.length = 0; }],
  ['reading a different file', p => { p.task.managed.tools[0].rawInput.target_file = '/foreign/SKILL.md'; }],
  ['skill read without the source marker', p => { p.task.managed.tools[1].rawOutput.FileContent.raw_output = 'unrelated'; }],
  ['historical replay presented as current execution', p => { p.task.managed.tools[1]._meta.isReplay = true; }],
  ['skill read from another turn', p => { p.task.plugin.tools[1]._meta.promptId = 'foreign-turn'; }],
  ['MCP completion from another approval', p => { p.report.permissions[0].toolCallId = 'unrelated-call'; }],
  ['unapproved terminal execution', p => { p.task.managed.tools.push({ sessionId: 'owned-session', _meta: { promptId: 'managed-turn' }, rawInput: { variant: 'Bash', command: 'echo forbidden' } }); }],
]) test(`rejects ${label}`, () => {
  const p = reportProof(); change(p); assert.throws(() => verifyExtensionsReport(p.report, p.scope, 24680, p.now - 1000));
});
test('native action scope rejects foreign PID, paths, command and arbitrary approval before OS access', { skip: process.platform !== 'darwin' }, () => {
  const home = realpathSync(mkdtempSync('/private/tmp/echo-native-review-extensions-guards-')); chmodSync(home, 0o700);
  try {
    const nonce = 'a'.repeat(32), r = createExtensionResources(home, nonce);
    const scope = { pid: 24680, home, cwd: r.cwd, nonce, node: r.node, action: 'stdio-allow' };
    let calls = 0; const run = () => { calls++; return '{}'; };
    for (const change of [{ pid: 1 }, { home: '/Users/person/.echo-agent' }, { cwd: '/' }, { nonce: 'wrong' }, { node: '/bin/sh' }, { action: 'allow-everything' }]) {
      assert.throws(() => nativeExtensionsAction({ ...scope, ...change }, run));
    }
    assert.equal(calls, 0);
    const script = [];
    assert.equal(nativeExtensionsAction(scope, (_bin, args) => { script.push(args.at(-1)); return JSON.stringify({ waiting: true }); }), false);
    assert(script[0].includes(r.root + '/server.mjs') && script[0].includes('MCP stdio') && script[0].includes(nonce));
    assert.throws(() => nativeExtensionsAction(scope, () => JSON.stringify({ performed: true, pid: 999, action: 'stdio-allow' })));
  } finally { rmSync(home, { recursive: true, force: true }); }
});
test('actual synthetic MCP transport refuses invalid inputs and writes only its fixed artifact', { skip: process.platform !== 'darwin' }, async () => {
  const home = realpathSync(mkdtempSync('/private/tmp/echo-native-review-extensions-protocol-')); chmodSync(home, 0o700);
  let child;
  try {
    const nonce = 'b'.repeat(32), r = createExtensionResources(home, nonce);
    child = spawn(process.execPath, [join(r.root, 'server.mjs'), home, nonce], { stdio: ['pipe', 'pipe', 'pipe'] });
    const replies = new Map(); let buffer = '', stderr = '';
    child.stderr.on('data', b => { stderr += b; });
    child.stdout.on('data', b => { buffer += b; let i; while ((i = buffer.indexOf('\n')) >= 0) { const reply = JSON.parse(buffer.slice(0, i)); buffer = buffer.slice(i + 1); replies.set(reply.id, reply); } });
    const closed = new Promise(resolve => child.once('close', (code, signal) => resolve({ code, signal })));
    async function request(id, method, params) {
      child.stdin.write(JSON.stringify({ jsonrpc: '2.0', id, method, params }) + '\n');
      const end = Date.now() + 5000;
      while (Date.now() < end) { if (replies.has(id)) return replies.get(id); await new Promise(r => setTimeout(r, 10)); }
      throw new Error(`MCP response ${id} missing: ${stderr}`);
    }
    const init = await request(1, 'initialize', { protocolVersion: '2024-11-05', clientInfo: { name: 'owned-protocol-test', version: '1' } });
    assert.equal(init.result.serverInfo.name, 'native-extensions');
    const list = await request(2, 'tools/list', {}); assert.equal(list.result.tools.length, 2);
    const bad = await request(3, 'tools/call', { name: 'echo_deliver', arguments: { nonce, phase: '../outside' } });
    assert.equal(bad.result.isError, true); assert(!existsSync(join(r.cwd, 'extensions-result-first.txt')));
    const wrong = await request(4, 'tools/call', { name: 'echo_deliver', arguments: { nonce: 'foreign', phase: 'first' } }); assert.equal(wrong.result.isError, true);
    const ok = await request(5, 'tools/call', { name: 'echo_deliver', arguments: { nonce, phase: 'first' } }); assert.equal(ok.result.isError, false);
    assert.equal(readFileSync(join(r.cwd, 'extensions-result-first.txt'), 'utf8'), `MCP-first-${nonce}\n真实 stdio MCP 交付。\n`);
    child.stdin.end(); assert.deepEqual(await closed, { code: 0, signal: null });
    const audit = readFileSync(join(r.cwd, 'extension-mcp-audit.jsonl'), 'utf8').trim().split('\n').map(JSON.parse);
    assert.equal(audit.filter(x => x.event === 'rejected').length, 2); assert.equal(audit.filter(x => x.event === 'tools/call').length, 1);
    assert(audit.every(x => x.pid === child.pid && x.nonce === nonce));
  } finally { if (child && child.exitCode === null) child.kill('SIGKILL'); rmSync(home, { recursive: true, force: true }); }
});
