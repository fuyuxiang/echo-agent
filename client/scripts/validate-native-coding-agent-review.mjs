import { cleanNativeReviewEnvironment } from "./lib/native-review-isolation.mjs";
import { nativeReviewOutput } from './lib/native-review-output.mjs';
// Actual isolated native Code Agent acceptance. The --self-test mode exercises
// only synthetic driver guards and local Node fixtures; it is not Native QA.
import assert from 'node:assert/strict';
import { spawn, spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { readFileSync, writeFileSync, mkdirSync, mkdtempSync, realpathSync, lstatSync,
  existsSync, readdirSync, unlinkSync, copyFileSync, createReadStream } from 'node:fs';
import { join, resolve, relative, isAbsolute, basename } from 'node:path';
import { tmpdir } from 'node:os';
import { setTimeout as delay } from 'node:timers/promises';
import vm from 'node:vm';

const filename = '原生模型代码验收.cjs';
const modelId = 'echoagent-ojlab/Echo Reed';
const packageSource = JSON.stringify({ name: 'echoagent-native-model-acceptance', version: '1.0.0', private: true,
  scripts: { test: 'node --test' } }, null, 2) + '\n';
const testSource = "const test = require('node:test');\nconst assert = require('node:assert/strict');\ntest('原生模型代码验收：实际导出值为42', () => {\n  const generated = require('./原生模型代码验收.cjs');\n  assert.equal(generated.验收值, 42);\n});\n";
const safeSource = (text) => typeof text === 'string' && text.length <= 4096
  && /^\s*exports\s*\.\s*验收值\s*=\s*42\s*;\s*$/.test(text.replace(/\/\*[\s\S]*?\*\/|\/\/[^\r\n\u2028\u2029]*/g, ' '));
const hash = (value) => createHash('sha256').update(value).digest('hex');
const inside = (root, path) => {
  const rel = relative(root, path);
  return rel !== '' && rel !== '..' && !rel.startsWith('../') && !isAbsolute(rel);
};
const seed = (cwd) => {
  mkdirSync(cwd, { recursive: true, mode: 0o700 });
  writeFileSync(join(cwd, 'package.json'), packageSource, { flag: 'wx', mode: 0o600 });
  writeFileSync(join(cwd, 'test.cjs'), testSource, { flag: 'wx', mode: 0o600 });
};
const nodeTest = (cwd) => {
  const child = spawnSync(process.execPath, ['--test'], { cwd, encoding: 'utf8', timeout: 15000,
    env: { ...cleanNativeReviewEnvironment(process.env), NODE_OPTIONS: '', NODE_PATH: '' }, maxBuffer: 1024 * 1024 });
  assert(!child.error && child.signal === null, `Node acceptance execution failed: ${child.error ?? child.signal}`);
  return { command: 'node --test', exitCode: child.status, stdout: child.stdout, stderr: child.stderr,
    environment: 'actual isolated local Node subprocess, not a model response' };
};
const verifyFixtureBaseline = (set) => {
  // sync_from_filesystem adds a non-existing baseline entry for a newly
  // discovered file so rollback can safely remove only that task's addition.
  assert(set.baselineMode === 'filesystem' && Array.isArray(set.baselineEntries) && set.baselineEntries.length === 3
    && new Set(set.baselineEntries.map((entry) => entry.path)).size === 3, 'Unexpected original/addition baseline scope');
  for (const [path, original] of Object.entries({ 'package.json': packageSource, 'test.cjs': testSource })) {
    const entry = set.baselineEntries.find((item) => item.path === path);
    assert(entry && entry.existed === true && entry.hash === hash(Buffer.from(original))
      && Buffer.from(entry.contentBase64, 'base64').toString('utf8') === original, 'Original failing baseline was rewritten');
  }
  const added = set.baselineEntries.find((entry) => entry.path === filename);
  assert(added?.existed === false && added.hash === null && added.contentBase64 === null,
    'The only added file must have a truthful non-existing baseline');
};
const verifyRuntimeRed = ({ tdd, expected, verification, updates, cwd }) => {
  assert(tdd.waiverReason === null && tdd.redRecordId === expected.redRecordId && tdd.redCommand === 'npm test'
    && tdd.redRevision === hash(Buffer.alloc(0)) && tdd.redRevision !== expected.contentRevision, 'No trustworthy initial RED revision or unexpected waiver');
  const red = tdd.redRecord;
  const provenance = tdd.runtimeProvenance;
  assert(provenance?.sessionId === expected.sessionId && provenance.modelId === modelId && provenance.workspace === cwd
    && typeof provenance.toolCallId === 'string' && provenance.toolCallId.length > 0, 'Runtime RED belongs to another task/model/workspace');
  assert(red?.id === `runtime-${provenance.toolCallId}` && red.id === tdd.redRecordId && red.taskId === expected.taskId
    && red.kind === 'test' && red.command === tdd.redCommand && red.status === 'failed' && Number.isInteger(red.exitCode)
    && red.exitCode !== 0 && red.contentRevision === tdd.redRevision && red.finishedAt === tdd.redAt
    && red.stdout.includes('MODULE_NOT_FOUND') && red.stdout.includes('# fail 1'), 'RED is not the actual Runtime failing test receipt');
  const toolEvents = updates.map((event, index) => ({ index, event, update: event.params?.update ?? event.params }));
  // ACP omits its default Pending status on the real typed-input refinement.
  // A completion containing both input and output is never a start receipt.
  const redStart = toolEvents.find(({ update }) => update?.toolCallId === provenance.toolCallId && update.rawInput?.variant === 'Bash'
    && (update.status == null || ['pending', 'in_progress'].includes(update.status)) && update.rawOutput == null);
  const redEnd = toolEvents.find(({ update }) => update?.toolCallId === provenance.toolCallId && update.status === 'completed'
    && update.rawOutput?.type === 'Bash');
  const firstWrite = toolEvents.find(({ update }) => ['Write', 'SearchReplace'].includes(update?.rawInput?.variant));
  assert(redStart && redEnd && firstWrite && redStart.index < redEnd.index && redEnd.index < firstWrite.index,
    'The captured RED must actually finish before the first model write');
  const output = redEnd.update.rawOutput;
  assert(redStart.update.rawInput.command === red.command && redStart.update.rawInput.is_background === false
    && output.command === red.command && output.current_dir === cwd && output.exit_code === red.exitCode
    && output.timed_out === false && output.signal === null && Array.isArray(output.output)
    && Buffer.from(output.output).toString('utf8') === red.stdout, 'Persisted native Bash output differs from the linked RED');
  assert.equal(Date.parse(red.startedAt), redStart.event.params._meta.agentTimestampMs, 'RED start timestamp was fabricated or rebound');
  assert.equal(Date.parse(red.finishedAt), redEnd.event.params._meta.agentTimestampMs, 'RED finish timestamp differs from actual Runtime completion');
  assert(Date.parse(red.finishedAt) <= firstWrite.event.params._meta.agentTimestampMs
    && Date.parse(verification.startedAt) > Date.parse(red.finishedAt) && red.durationMs >= 0, 'RED/GREEN/Write actual chronology is invalid');
  return { red, provenance };
};
if (process.argv[2] === '--self-test') {
  const home = realpathSync(mkdtempSync(join(tmpdir(), 'echo-native-review-agent-guards-')));
  const cwd = join(home, 'workspaces', 'default');
  seed(cwd);
  const red = nodeTest(cwd);
  assert.notEqual(red.exitCode, 0);
  assert((red.stdout + red.stderr).includes('MODULE_NOT_FOUND'));
  assert(!existsSync(join(cwd, filename)));
  const observedSource = '/**\n * 原生模型代码验收测试文件\n */\nexports.验收值 = 42;';
  writeFileSync(join(cwd, filename), observedSource, { flag: 'wx', mode: 0o600 });
  const green = nodeTest(cwd);
  assert.equal(green.exitCode, 0);
  assert(green.stdout.includes('原生模型代码验收'));
  const source = readFileSync(process.argv[3] || new URL('./fixtures/native-coding-agent-review.js', import.meta.url), 'utf8');
  assert.equal(source.split('  void run();').length, 2, 'Synthetic guard hook must remain unique');
  const context = vm.createContext({ window: { __TAURI_INTERNALS__: { invoke: () => { throw new Error('No Native IPC in synthetic guards'); } } },
    Date, setTimeout, clearTimeout });
  vm.runInContext(source.replace('  void run();', '  globalThis.__guardPolicy = permissionOperation;'), context)({ home, nonce: 'synthetic-native-agent-guard-0123456789' });
  const sid = 'synthetic-owned-session';
  const request = (variant, input, sessionId = sid) => ({ sessionId, rawInput: { variant, ...input } });
  const policy = (value) => context.__guardPolicy(value, sid);
  assert(policy(request('Write', { file_path: `${cwd}/${filename}`, content: 'exports.验收值 = 42;\n' })));
  assert(policy(request('ReadFile', { path: `${cwd}/test.cjs` })));
  assert(policy(request('ReadFile', { target_file: `${cwd}/test.cjs` })), 'The actual serde ReadFileInput uses target_file');
  assert(policy(request('Write', { file_path: filename, content: observedSource })), 'The real model block-comment source is a safe literal assignment');
  assert(policy(request('Bash', { command: 'npm run test', cwd })));
  for (const denied of [request('Write', { file_path: '/private/tmp/foreign.cjs', content: 'exports.验收值 = 42;' }),
    request('Write', { file_path: `${cwd}/test.cjs`, content: 'exports.验收值 = 42;' }),
    request('Write', { file_path: `${cwd}/${filename}`, content: 'require("node:child_process").execSync("curl example.com"); exports.验收值 = 42;' }),
    request('Write', { file_path: `${cwd}/${filename}`, content: 'exports.验收值 = 42;' }, 'foreign-session'),
    request('Bash', { command: 'npm test && git commit -am wrong' }), request('Bash', { command: 'node --test', cwd: '/outside' }),
    request('WebFetch', { url: 'https://example.com' }), request('ReadFile', { path: `${cwd}/../secret` }),
    request('Bash', { command: 'npm test', is_background: true }), request('Bash', { command: 123 }),
    request('Write', { file_path: filename, content: '/* 中文说明 */ require("node:child_process").execSync("curl example.com"); exports.验收值 = 42;' }),
    request('Write', { file_path: filename, content: '// 中文说明\u2028require("node:child_process").execSync("curl example.com");\nexports.验收值 = 42;' }),
    request('Write', { file_path: filename, content: '/* 未关闭注释\nexports.验收值 = 42;' })]) assert.equal(policy(denied), null);
  assert(safeSource('// 中文注释\nexports.验收值 = 42;\n'));
  assert(!safeSource('exports.验收值 = 43;'));
  // Synthetic data only: these objects are never written as product receipts.
  const redId = 'synthetic-red-tool';
  const redText = 'MODULE_NOT_FOUND\n# tests 1\n# fail 1\n';
  const initialRevision = hash(Buffer.alloc(0));
  const runtimeReceipt = { waiverReason: null, redRecordId: `runtime-${redId}`, redCommand: 'npm test', redRevision: initialRevision,
    redAt: new Date(2000).toISOString(), runtimeProvenance: { sessionId: sid, modelId, toolCallId: redId, workspace: cwd },
    redRecord: { id: `runtime-${redId}`, taskId: 'synthetic-task', kind: 'test', command: 'npm test', status: 'failed', exitCode: 1,
      stdout: redText, contentRevision: initialRevision, startedAt: new Date(1000).toISOString(), finishedAt: new Date(2000).toISOString(), durationMs: 1000 } };
  const event = (time, update) => ({ params: { update, _meta: { agentTimestampMs: time } } });
  const events = [event(1000, { sessionUpdate: 'tool_call_update', toolCallId: redId, rawInput: { variant: 'Bash', command: 'npm test', is_background: false } }),
    event(2000, { sessionUpdate: 'tool_call_update', toolCallId: redId, status: 'completed', rawOutput: { type: 'Bash', command: 'npm test', current_dir: cwd,
      exit_code: 1, timed_out: false, signal: null, output: [...Buffer.from(redText)] } }),
    event(3000, { sessionUpdate: 'tool_call_update', toolCallId: 'synthetic-write-tool', rawInput: { variant: 'Write' } })];
  const redExpected = { taskId: 'synthetic-task', sessionId: sid, redRecordId: runtimeReceipt.redRecordId, contentRevision: hash(Buffer.from('new implementation')) };
  const checkRed = (overrides = {}) => verifyRuntimeRed({ tdd: runtimeReceipt, expected: redExpected,
    verification: { startedAt: new Date(4000).toISOString() }, updates: events, cwd, ...overrides });
  assert.equal(checkRed().red.id, runtimeReceipt.redRecordId);
  assert.throws(() => checkRed({ tdd: { ...runtimeReceipt, runtimeProvenance: { ...runtimeReceipt.runtimeProvenance, sessionId: 'foreign-session' } } }), /another task/);
  assert.throws(() => checkRed({ tdd: { ...runtimeReceipt, waiverReason: 'fake replacement' } }), /unexpected waiver/);
  assert.throws(() => checkRed({ tdd: { ...runtimeReceipt, redRecord: { ...runtimeReceipt.redRecord, stdout: 'MODULE_NOT_FOUND\n# fail 1\nforged' } } }), /differs from the linked RED/);
  assert.throws(() => checkRed({ updates: [events[0], events[2], events[1]] }), /before the first model write/);
  assert.throws(() => checkRed({ tdd: { ...runtimeReceipt, redRecord: { ...runtimeReceipt.redRecord, startedAt: new Date(999).toISOString() } } }), /timestamp was fabricated/);
  for (const status of ['pending', 'in_progress']) assert.equal(checkRed({ updates: [
    event(1000, { ...events[0].params.update, status }), events[1], events[2],
  ] }).red.id, runtimeReceipt.redRecordId);
  for (const status of ['completed', 'failed']) assert.throws(() => checkRed({ updates: [
    event(1000, { ...events[0].params.update, status }), events[1], events[2],
  ] }), /before the first model write/, `Terminal ${status} input cannot establish a RED start`);
  assert.throws(() => checkRed({ updates: [
    event(1000, { ...events[0].params.update, rawOutput: events[1].params.update.rawOutput }), events[1], events[2],
  ] }), /before the first model write/, 'An output-bearing input refinement cannot establish a RED start');
  assert.throws(() => checkRed({ updates: [
    event(2000, { ...events[1].params.update, rawInput: events[0].params.update.rawInput }), events[2],
  ] }), /before the first model write/, 'A combined completion does not establish an earlier RED start');
  const baselineEntries = Object.entries({ 'package.json': packageSource, 'test.cjs': testSource }).map(([path, original]) => ({
    path, existed: true, hash: hash(Buffer.from(original)), contentBase64: Buffer.from(original).toString('base64'),
  }));
  const addedBaseline = { path: filename, existed: false, hash: null, contentBase64: null };
  verifyFixtureBaseline({ baselineMode: 'filesystem', baselineEntries: [...baselineEntries, addedBaseline] });
  for (const invalid of [
    [...baselineEntries, addedBaseline, { path: 'foreign.cjs', existed: false, hash: null, contentBase64: null }],
    [...baselineEntries, { ...addedBaseline, existed: true }],
    [...baselineEntries, { ...addedBaseline, hash: hash(Buffer.from('foreign')) }],
    [{ ...baselineEntries[0], contentBase64: Buffer.from('mutated manifest').toString('base64') }, baselineEntries[1], addedBaseline],
    [baselineEntries[0], baselineEntries[0], addedBaseline],
  ]) assert.throws(() => verifyFixtureBaseline({ baselineMode: 'filesystem', baselineEntries: invalid }));
  console.log(JSON.stringify({ environment: 'synthetic helper guards + local Node fixture, no native application/model/tools',
    guardCases: 38, actualFixtureRedExit: red.exitCode, actualFixtureGreenExit: green.exitCode, home }));
  process.exit(0);
}

assert.equal(process.platform, 'darwin', 'This driver requires the actual guarded macOS native branch');
const continueRestart = process.argv[3] === '--continue-restart';
assert(process.argv[2] && (process.argv.length === 3 || (continueRestart && process.argv.length === 5)),
  'Usage: node scripts/validate-native-coding-agent-review.mjs <built-executable> [--continue-restart <exact-prior-output>]');
const executable = realpathSync(resolve(process.argv[2]));
assert(lstatSync(executable).isFile() && !lstatSync(resolve(process.argv[2])).isSymbolicLink(), 'An ordinary real built executable is required');
const outputRoot = nativeReviewOutput('native-code-agent', 'NATIVE_CODE_AGENT_REVIEW_OUTPUT');
mkdirSync(outputRoot, { recursive: true, mode: 0o700 });
const priorDirectory = continueRestart ? realpathSync(resolve(process.argv[4])) : null;
const priorArtifact = (name) => {
  const file = join(priorDirectory, name);
  assert(inside(outputRoot, priorDirectory) && basename(priorDirectory).startsWith('run-')
    && !lstatSync(resolve(process.argv[4])).isSymbolicLink(), 'Continuation must name an exact ordinary prior output');
  assert(lstatSync(file).isFile() && !lstatSync(file).isSymbolicLink() && inside(priorDirectory, realpathSync(file)), 'Unsafe prior artifact');
  return readFileSync(file);
};
const priorResult = continueRestart ? JSON.parse(priorArtifact('native-code-agent-result.json')) : null;
const priorReport = continueRestart ? JSON.parse(priorArtifact('first-launch.json')) : null;
const output = mkdtempSync(join(outputRoot, 'run-'));
const home = continueRestart ? realpathSync(priorResult.home) : realpathSync(mkdtempSync(join(tmpdir(), 'echo-native-review-agent-')));
assert(basename(home).startsWith('echo-native-review-agent-') && inside(realpathSync(tmpdir()), home)
  && lstatSync(home).isDirectory() && !lstatSync(continueRestart ? priorResult.home : home).isSymbolicLink(), 'Only an ordinary private temporary Agent review home is allowed');
assert.equal(lstatSync(home).mode & 0o077, 0, 'Review data home must be private');
const cwd = join(home, 'workspaces', 'default');
const legacy = join(home, 'empty-legacy');
if (!continueRestart) {
  mkdirSync(legacy, { mode: 0o700 });
  seed(cwd);
// Explicit real policy, not a permissive fixture backend. All other MCP / web
// tools are denied before bridge approval; filesystem/commands ask per call.
writeFileSync(join(home, 'config.toml'), '[permission]\ndeny = ["WebFetch", "WebSearch", "MCPTool"]\nask = ["*"]\n', { flag: 'wx', mode: 0o600 });
}
const red = continueRestart ? JSON.parse(priorArtifact('actual-initial-red.json')) : nodeTest(cwd);
assert.notEqual(red.exitCode, 0, 'The real initial test must fail without a generated implementation');
assert((red.stdout + red.stderr).includes('MODULE_NOT_FOUND'), 'Initial failure is not caused by the absent target');
if (!continueRestart) assert(!existsSync(join(cwd, filename)), 'The driver must never generate the model target');
writeFileSync(join(output, 'actual-initial-red.json'), JSON.stringify(red, null, 2));
const binaryHash = createHash('sha256');
for await (const bytes of createReadStream(executable)) binaryHash.update(bytes);
const result = { environment: 'actual macOS native Agent Code + real Qwen tools + real native Node verification',
  executable, binarySha256: binaryHash.digest('hex'), home, cwd, output, selectedModelId: modelId,
  initialRed: { exitCode: red.exitCode, missingTarget: true }, phases: [], errors: [], passed: false };
console.log(`Isolated Code Agent home: ${home}`);
console.log(`Native Code Agent evidence: ${output}`);
const regular = (path) => {
  assert(inside(home, path), 'Acceptance artifact is outside the private home');
  assert(lstatSync(path).isFile() && !lstatSync(path).isSymbolicLink() && inside(home, realpathSync(path)), 'Acceptance artifact is symbolic or escapes the home');
  return readFileSync(path);
};
const json = (path) => JSON.parse(regular(path).toString('utf8'));
const jsonl = (path) => regular(path).toString('utf8').trim().split('\n').filter(Boolean).map((line) => JSON.parse(line));
const reportPath = join(home, 'native-review.json');
const historyPath = join(home, 'native-review-history.jsonl');
const required = {
  'first-launch': ['code-agent-isolation', 'code-agent-model-session', 'code-agent-ui-runtime-binding',
    'code-agent-real-model-file', 'code-agent-automatic-node-delivery', 'code-agent-native-verification',
    'code-agent-native-red-evidence', 'code-agent-actual-diff-review', 'code-agent-delivery-gates'],
  restart: ['code-agent-isolation', 'code-agent-restart-state', 'code-agent-restart-ui'],
};
const physicalEvidence = (report) => {
  const expected = report.expected;
  assert(expected?.version === 1 && expected.home === home && expected.cwd === cwd && expected.modelId === modelId
    && /^[A-Za-z0-9_-]+$/.test(expected.taskId) && /^[A-Za-z0-9_-]+$/.test(expected.sessionId)
    && /^[a-f0-9]{64}$/.test(expected.contentRevision), 'Missing actual bound model/task/session/revision');
  assert.equal(regular(join(cwd, 'package.json')).toString('utf8'), packageSource, 'Model changed the test manifest');
  assert.equal(regular(join(cwd, 'test.cjs')).toString('utf8'), testSource, 'Model changed the assertion');
  const content = regular(join(cwd, filename));
  assert.equal(content.toString('utf8'), expected.content);
  assert(safeSource(expected.content), 'Generated target has wrong value or unsafe executable content');
  const workspaceFiles = readdirSync(cwd).sort();
  assert.deepEqual(workspaceFiles, ['package.json', 'test.cjs', filename].sort(), 'Model wrote an unexpected workspace artifact');
  const taskDirectory = join(home, 'coding', hash(Buffer.from(cwd)).slice(0, 16), expected.taskId);
  const task = json(join(taskDirectory, 'task.json'));
  const set = json(join(taskDirectory, 'changeset.json'));
  const verifications = jsonl(join(taskDirectory, 'verifications.jsonl'));
  const reviews = json(join(taskDirectory, 'reviews.json')).records;
  const ledger = jsonl(join(taskDirectory, 'execution-ledger.jsonl'));
  const tdd = json(join(taskDirectory, 'tdd.json'));
  assert(task.id === expected.taskId && task.sessionId === expected.sessionId && task.modelId === modelId && task.phase === 'delivered');
  assert(task.taskNodes.length > 0 && task.taskNodes.every((node) => node.status === 'success'), 'Real automatic nodes did not succeed');
  assert(task.acceptanceCriteria.length > 0 && task.acceptanceCriteria.every((item) => item.satisfied && item.evidence.length), 'Acceptance has no real confirmation evidence');
  assert(set.changes.length === 1 && set.changes[0].path === filename && set.committedHash === null, 'Unexpected change/commit scope');
  verifyFixtureBaseline(set);
  assert.equal(set.changeHashes[filename], hash(content));
  assert.equal(set.reviewedHashes[filename], hash(content));
  const revision = createHash('sha256');
  for (const [path, digest] of Object.entries(set.changeHashes).sort(([a], [b]) => Buffer.compare(Buffer.from(a), Buffer.from(b)))) {
    revision.update(path); revision.update(Buffer.from([0])); revision.update(digest); revision.update(Buffer.from([0]));
  }
  assert.equal(revision.digest('hex'), expected.contentRevision);
  assert.equal(set.verifiedRevision, expected.contentRevision);
  const verification = verifications.find((item) => item.id === expected.verificationId);
  assert(verification?.taskId === expected.taskId && verification.status === 'passed' && verification.exitCode === 0
    && verification.contentRevision === expected.contentRevision && verification.command === expected.command
    && ['npm run test', 'npm test', 'node --test'].includes(verification.command)
    && verification.stdout.includes('原生模型代码验收') && /(?:# pass 1|tests 1|pass 1)/.test(verification.stdout), 'Final native Node check lacks real passing evidence');
  assert(verifications.length >= 1 && verifications.every((item) => ['npm run test', 'npm test', 'node --test'].includes(item.command)), 'An unapproved native verification command ran');
  assert.equal(verifications.filter((item) => item.status === 'passed' && item.contentRevision === expected.contentRevision).length, 1,
    'Duplicate current GREEN verifications were recorded');
  assert.equal(reviews.length, 2);
  assert(reviews.every((item) => item.contentRevision === expected.contentRevision));
  assert.deepEqual(reviews.map((item) => item.kind).sort(), ['code_quality', 'requirements']);
  assert.equal(ledger.filter((item) => item.kind === 'requirement_submitted').length, 1, 'UI start submitted duplicate requirements');
  const candidates = readdirSync(join(home, 'sessions'), { withFileTypes: true }).filter((entry) => entry.isDirectory())
    .map((entry) => join(home, 'sessions', entry.name, expected.sessionId)).filter((path) => existsSync(join(path, 'summary.json')));
  assert.equal(candidates.length, 1, 'Actual Runtime session directory is ambiguous or missing');
  const sessionDirectory = candidates[0];
  const summary = json(join(sessionDirectory, 'summary.json'));
  assert([modelId, 'Echo Reed'].includes(summary.current_model_id), 'Actual saved model differs from the UI-selected model');
  const updates = jsonl(join(sessionDirectory, 'updates.jsonl'));
  const tools = new Map();
  for (const envelope of updates) {
    const update = envelope.params?.update ?? envelope.params;
    if (!['tool_call', 'tool_call_update'].includes(update?.sessionUpdate ?? update?.type)) continue;
    const id = update.toolCallId ?? update.tool_call_id;
    if (!id) continue;
    const previous = tools.get(id) ?? {};
    tools.set(id, { ...previous, ...update, rawInput: update.rawInput ?? update.raw_input ?? previous.rawInput });
  }
  const writes = [...tools.entries()].filter(([_id, tool]) => {
    if (tool.status !== 'completed') return false;
    let raw = tool.rawInput;
    if (typeof raw === 'string') { try { raw = JSON.parse(raw); } catch { return false; } }
    const wrapped = typeof raw?.tool_name === 'string';
    const name = wrapped ? raw.tool_name.split(/:|__/).pop().toLowerCase() : raw?.variant === 'Write' ? 'write' : null;
    const input = wrapped ? raw.tool_input : raw;
    const path = input?.file_path ?? input?.path;
    return name === 'write' && [filename, join(cwd, filename)].includes(path) && safeSource(input?.content);
  });
  assert(writes.length > 0, 'Persisted actual model updates contain no completed scoped Write tool');
  const { red, provenance } = verifyRuntimeRed({ tdd, expected, verification, updates, cwd });
  assert(regular(join(home, 'theia', 'theia.log')).toString('utf8').includes('Frontend application startup sequence completed'), 'Actual packaged Theia did not initialize');
  return { taskId: task.id, sessionId: expected.sessionId, taskDirectory, sessionDirectory,
    generatedFileSha256: hash(content), taskSha256: hash(regular(join(taskDirectory, 'task.json'))),
    changesetSha256: hash(regular(join(taskDirectory, 'changeset.json'))), verificationsSha256: hash(regular(join(taskDirectory, 'verifications.jsonl'))),
    reviewsSha256: hash(regular(join(taskDirectory, 'reviews.json'))), ledgerSha256: hash(regular(join(taskDirectory, 'execution-ledger.jsonl'))),
    tddSha256: hash(regular(join(taskDirectory, 'tdd.json'))), redRecordId: red.id, redToolCallId: provenance.toolCallId,
    contentRevision: expected.contentRevision, verificationId: expected.verificationId,
    completedWriteToolIds: writes.map(([id]) => id), actualToolsRecorded: tools.size };
};
const launchEnv = { ...cleanNativeReviewEnvironment(process.env), GROK_HOME: legacy, ECHO_DESKTOP_REVIEW: '1', ECHO_DESKTOP_REVIEW_CODE_AGENT: '1',
  npm_config_cache: join(home, 'npm-cache'), npm_config_update_notifier: 'false', npm_config_audit: 'false', npm_config_fund: 'false',
  ECHO_DESKTOP_REVIEW_CODE: '0', ECHO_DESKTOP_REVIEW_WEBDAV: '0', ECHO_DESKTOP_REVIEW_MEDIA: '0', ECHO_DESKTOP_REVIEW_EXTENDED: '0' };
for (const name of ['ECHO_AGENT_HOME', 'ECHO_DESKTOP_VALIDATION', 'ECHO_THEIA_NODE', 'NODE_OPTIONS', 'NODE_PATH']) delete launchEnv[name];
let active;
let interrupted = false;
const interrupt = () => { interrupted = true; active?.kill('SIGTERM'); };
process.once('SIGINT', interrupt);
process.once('SIGTERM', interrupt);
let firstFilesystem;
try {
  if (continueRestart) {
    assert.equal(priorResult.binarySha256, result.binarySha256, 'Continuation requires the exact original native binary');
    assert(priorResult.home === home && priorResult.cwd === cwd && priorResult.selectedModelId === modelId
      && priorResult.passed === false && priorResult.phases?.length === 1 && priorResult.errors?.length === 1
      && priorResult.errors[0] === 'first-launch: AssertionError [ERR_ASSERTION]: The real Code start baseline differs from the two initial fixture files',
    'Only the exact original baseline-check failure may continue');
    assert(priorReport.phase === 'first-launch' && priorReport.passed === true && priorReport.errors.length === 0
      && priorReport.nativeContext.home === home && priorReport.nativeContext.codeAgentRequested === true
      && Number.isInteger(priorReport.nativeContext.pid) && priorReport.nativeContext.pid > 0
      && priorReport.workspace === cwd && priorReport.sessionId === priorReport.expected.sessionId,
    'Missing authentic successful first-launch native scope');
    assert.deepEqual(priorResult.phases[0].report, priorReport);
    assert.equal(priorResult.phases[0].exitCode, 0);
    assert.equal(priorReport.checks.length, required['first-launch'].length);
    for (const id of required['first-launch']) assert.equal(priorReport.checks.filter((item) => item.id === id && item.result === 'passed').length, 1);
    assert.deepEqual(json(reportPath), priorReport, 'Current native report changed after the exact prior run');
    assert.deepEqual(jsonl(historyPath), [priorReport], 'History contains an unexpected intervening native evaluation');
    firstFilesystem = physicalEvidence(priorReport);
    result.continuation = { originalOutput: priorDirectory, originalResultSha256: hash(priorArtifact('native-code-agent-result.json')),
      originalFirstReportSha256: hash(priorArtifact('first-launch.json')), retainedNativePid: priorReport.nativeContext.pid,
      originalNativeChecks: priorReport.checks.length, originalFailureRetained: priorResult.errors[0],
      noTaskResetOrEvidenceWrite: true, beforeFilesystem: firstFilesystem };
    writeFileSync(join(output, 'continuation-admission.json'), JSON.stringify(result.continuation, null, 2));
    copyFileSync(join(priorDirectory, 'first-launch.json'), join(output, 'admitted-original-first-launch.json'));
  }
  for (const phase of continueRestart ? ['restart'] : ['first-launch', 'restart']) {
    assert(!interrupted, 'Actual Code Agent acceptance interrupted');
    if (existsSync(reportPath)) unlinkSync(reportPath);
    const historyCount = existsSync(historyPath) ? jsonl(historyPath).length : 0;
    const started = Date.now();
    const app = spawn(executable, ['--data-home', home], { env: { ...launchEnv, ECHO_DESKTOP_REVIEW_PHASE: phase }, stdio: 'inherit' });
    active = app;
    let ended = false;
    let launchError;
    const exited = new Promise((accept) => {
      app.once('exit', () => { ended = true; accept(true); });
      app.once('error', (error) => { launchError = error; ended = true; accept(true); });
    });
    try {
      const timeout = phase === 'first-launch' ? 480000 : 180000;
      assert(await Promise.race([exited, delay(timeout, false, { ref: false })]), 'Native Code Agent acceptance timed out');
      if (launchError) throw launchError;
      assert.equal(app.exitCode, 0, 'Native process did not exit normally');
      const report = json(reportPath);
      copyFileSync(reportPath, join(output, `${phase}.json`));
      result.phases.push({ phase, exitCode: app.exitCode, report });
      console.log(JSON.stringify({ phase, exitCode: app.exitCode, report }));
      assert(report.phase === phase && report.nativeContext?.home === home && report.nativeContext.codeAgentRequested === true && report.nativeContext.pid === app.pid
        && report.workspace === cwd && report.sessionId === report.expected?.sessionId, 'Report belongs to a different launch or scope');
      assert(Date.parse(report.finishedAt) >= started && Date.parse(report.finishedAt) <= Date.now() + 1000, 'Stale native report');
      assert(report.passed === true && report.errors?.length === 0 && report.checks?.every((item) => item.result === 'passed'), 'Actual Code Agent report failed');
      if (continueRestart) assert.deepEqual(report.expected, priorReport.expected, 'Restart localStorage marker differs from the exact prior native expectation');
      for (const id of required[phase]) assert.equal(report.checks.filter((item) => item.id === id).length, 1, `Missing or duplicate actual check ${id}`);
      const history = jsonl(historyPath);
      assert.equal(history.length, historyCount + 1, 'Duplicate renderer evaluations changed the report');
      assert.deepEqual(history.at(-1), report);
      const filesystem = physicalEvidence(report);
      if (phase === 'first-launch') firstFilesystem = filesystem;
      else for (const key of ['taskId', 'sessionId', 'generatedFileSha256', 'taskSha256', 'changesetSha256', 'verificationsSha256', 'reviewsSha256', 'ledgerSha256', 'tddSha256', 'redRecordId', 'redToolCallId', 'contentRevision', 'verificationId']) {
        assert.equal(filesystem[key], firstFilesystem[key], `Restart changed real Code evidence ${key}`);
      }
      const finalNode = nodeTest(cwd);
      assert.equal(finalNode.exitCode, 0, 'Independently rerun actual Node assertion failed');
      writeFileSync(join(output, `${phase}-independent-node.json`), JSON.stringify(finalNode, null, 2));
      writeFileSync(join(output, `${phase}-filesystem.json`), JSON.stringify(filesystem, null, 2));
    } catch (error) {
      result.errors.push(`${phase}: ${String(error)}`);
      console.error(`${phase}: ${String(error)}`);
      break;
    } finally {
      if (!ended) {
        app.kill('SIGTERM');
        await Promise.race([exited, delay(3000, undefined, { ref: false })]);
        if (!ended) app.kill('SIGKILL');
      }
      active = undefined;
    }
  }
} catch (error) { result.errors.push(String(error)); }
finally {
  process.removeListener('SIGINT', interrupt);
  process.removeListener('SIGTERM', interrupt);
  result.passed = result.errors.length === 0 && result.phases.length === (continueRestart ? 1 : 2)
    && (!continueRestart || result.continuation?.originalNativeChecks === 9);
  writeFileSync(join(output, 'native-code-agent-result.json'), JSON.stringify(result, null, 2));
}
console.log(`Native Code Agent evidence retained: ${output}; ${home}`);
if (!result.passed) process.exitCode = 1;
