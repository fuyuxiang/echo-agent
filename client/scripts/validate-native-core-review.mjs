import { cleanNativeReviewEnvironment } from "./lib/native-review-isolation.mjs";
import { DEFAULT_REVIEW_MODEL_ID, PRIVATE_REVIEW_MODEL_ID, seedReviewModel, redactReviewModelSecrets, captureReviewModelLogs } from './lib/native-review-model.mjs';
import { nativeReviewOutput } from './lib/native-review-output.mjs';
import { activateNativeReviewWindow, assertNativeReviewSessionUnlocked } from './lib/native-review-window.mjs';
// Real macOS Native runner. Importing this module does not launch an app.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createHash, randomBytes } from 'node:crypto';
import { chmodSync, existsSync, lstatSync, mkdirSync, mkdtempSync, readFileSync, readdirSync,
  realpathSync, unlinkSync, writeFileSync } from 'node:fs';
import { homedir, tmpdir } from 'node:os';
import { basename, isAbsolute, join, relative, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { setTimeout as delay } from 'node:timers/promises';
import ExcelJS from 'exceljs';
import { readonlyCompatibilitySourceRevocations } from './lib/native-core-review-isolation.mjs';

export const CORE_CHECKS = Object.freeze({
  'first-launch': ['core-native-isolation', 'core-qwen-ready', 'core-qwen-initial-response', 'core-queue-and-send-now',
    'core-stop-and-resend', 'core-rename-search-ui', 'core-fork-original-preserved', 'core-archive-restore-ui',
    'core-delete-owned-session-ui', 'core-project-move-switch-ui', 'core-office-xlsx-runtime-read',
    'core-office-epub-runtime-read', 'core-office-ods-runtime-read', 'core-subagent-no-side-effects', 'core-custom-qwen-connection', 'core-automation-draft-navigation-exit'],
  restart: ['core-native-isolation', 'core-restart-owned-catalog', 'core-restart-project-ui', 'core-restart-fork-ui', 'core-restart-office-fixtures', 'core-restart-custom-model'],
});
const sha = (value) => createHash('sha256').update(value).digest('hex');
export function coreReviewProjectName(nonce) {
  assert(/^[a-f0-9]{32}$/.test(nonce));
  return `隔离核心项目${[...nonce.slice(0, 8)].map(char => '零一二三四五六七八九甲乙丙丁戊己'[parseInt(char, 16)]).join('')}`;
}
const within = (root, file) => { const r = relative(root, file); return r !== '' && r !== '..' && !r.startsWith('../') && !isAbsolute(r); };
export function validateCoreManifest(manifest) {
  assert.equal(manifest?.schema, 1);
  assert(/^\/private\/(?:tmp|var\/folders\/[A-Za-z0-9_-]+\/[A-Za-z0-9_-]+\/T)\/echo-native-review-core-[A-Za-z0-9_-]+$/.test(manifest.home));
  assert.equal(manifest.cwd, `${manifest.home}/workspaces/default`); assert(/^[a-f0-9]{32}$/.test(manifest.nonce));
  assert(Object.hasOwn(CORE_CHECKS, manifest.phase)); assert([DEFAULT_REVIEW_MODEL_ID, PRIVATE_REVIEW_MODEL_ID].includes(manifest.modelId));
  assert(manifest.phase === 'first-launch' ? manifest.expected === null
    : manifest.expected && typeof manifest.expected === 'object' && !Array.isArray(manifest.expected), 'Invalid phase expectation');
  assert(Array.isArray(manifest.officeFixtures) && manifest.officeFixtures.length === 3);
  for (const [i, format] of ['xlsx', 'epub', 'ods'].entries()) {
    const fixture = manifest.officeFixtures[i]; assert.equal(fixture.format, format);
    assert.equal(fixture.path, `${manifest.cwd}/办公读取/中文 ${format}.${format}`);
    assert.equal(fixture.marker, `CORE-${format.toUpperCase()}-${manifest.nonce}`);
    if (format === 'epub') assert.equal(fixture.excludedMarker, `CORE-NAV-EXCLUDED-${manifest.nonce}`);
    assert.deepEqual(Object.keys(fixture).sort(), (format === 'epub' ? ['format', 'path', 'marker', 'excludedMarker'] : ['format', 'path', 'marker']).sort());
  }
  assert.deepEqual(Object.keys(manifest).sort(), ['schema', 'home', 'cwd', 'nonce', 'phase', 'modelId', 'officeFixtures', 'expected'].sort());
  return manifest;
}
const ordinary = (file, home) => {
  assert(within(home, file), 'Physical path escaped private home'); const info = lstatSync(file);
  assert(info.isFile() && !info.isSymbolicLink(), 'Physical evidence must be an ordinary file');
  assert.equal(realpathSync(file), file); assert(info.size <= 32 * 1024 * 1024, 'Physical evidence exceeded read bound');
  return readFileSync(file);
};
export function verifyNativeCoreReport({ report, manifest, pid, startedAt, exitCode, signal, previousExpected }) {
  validateCoreManifest(manifest);
  assert.equal(exitCode, 0, 'Native application did not exit normally'); assert.equal(signal, null, 'A signal is not graceful success');
  assert.equal(report?.version, 1); assert.equal(report.kind, 'native-core'); assert.equal(report.phase, manifest.phase);
  assert.equal(report.home, manifest.home); assert.equal(report.nonce, manifest.nonce); assert.equal(report.cwd, manifest.cwd);
  assert.equal(report.nativeContext?.pid, pid); assert.equal(report.nativeContext?.home, manifest.home);
  assert.equal(report.nativeContext?.phase, manifest.phase); assert.equal(report.nativeContext?.coreRequested, true);
  assert.equal(report.nativeContext?.evaluations, 1); assert.equal(report.fakeIPC, false); assert.equal(report.osPickerVerified, false);
  assert.equal(report.nativeVerified, false, 'Renderer must leave independent Native qualification to parent');
  const finishedAt = Date.parse(report.finishedAt);
  assert(Number.isFinite(finishedAt) && finishedAt >= startedAt && finishedAt <= Date.now() + 1000, 'Stale renderer report');
  assert.equal(report.passed, true); assert.deepEqual(report.errors, []);
  assert(Array.isArray(report.checks) && report.checks.length === CORE_CHECKS[manifest.phase].length);
  for (const id of CORE_CHECKS[manifest.phase]) {
    const checks = report.checks.filter((check) => check.id === id); assert.equal(checks.length, 1, `Missing/duplicate ${id}`);
    assert.equal(checks[0].result, 'passed');
  }
  const e = report.expected;
  assert.equal(e?.version, 1); for (const key of ['home', 'cwd', 'nonce', 'modelId', 'officeFixtures']) assert.deepEqual(e[key], manifest[key]);
  assert(Array.isArray(e.ownedSessionIds) && e.ownedSessionIds.length === 4 && new Set(e.ownedSessionIds).size === 4
    && e.ownedSessionIds.every((id) => /^[A-Za-z0-9-]{1,128}$/.test(id)), 'Expected exactly four owned parent sessions');
  assert(new Set([e.sessionId, e.forkedSessionId, e.projectForkedSessionId, e.deletedSessionId]).size === 4
    && [e.sessionId, e.forkedSessionId, e.projectForkedSessionId, e.deletedSessionId].every((id) => e.ownedSessionIds.includes(id)), 'Foreign/duplicate session ID');
  assert.equal(e.title, `隔离核心会话-${manifest.nonce}`);
  assert.equal(e.project?.name, coreReviewProjectName(manifest.nonce)); assert(within(manifest.home, e.project.cwd));
  assert.equal(e.markers.recovery, `CORE-RECOVER-${manifest.nonce}`); assert.equal(e.markers.queued, `CORE-QUEUED-${manifest.nonce}`);
  assert.equal(e.markers.project, `CORE-PROJECT-${manifest.nonce}`);
  assert(e.child?.sessionId && !e.ownedSessionIds.includes(e.child.sessionId)); assert.equal(e.child.marker, `CORE-CHILD-${manifest.nonce}`);
  assert(/^[A-Za-z0-9-]{1,128}$/.test(e.child.sessionId));
  assert(e.customModel?.providerId && e.customModel.modelId && e.customModel.modelId !== manifest.modelId);
  assert.equal(e.customModel.label, `隔离个人模型-${manifest.nonce}`); assert.equal(e.customModel.marker, `CORE-CUSTOM-${manifest.nonce}`);
  assert(Array.isArray(report.permissions) && report.permissions.every((entry) => !entry.denied
    && entry.sessionId === e.sessionId && entry.via === 'production allow-once UI'
    && (entry.operation?.name === 'read_file' ? manifest.officeFixtures.some((f) => f.path === entry.operation.path)
      : ['task', 'spawn_subagent'].includes(entry.operation?.name))), 'Unscoped or denied permission cannot pass');
  const evidence = (id) => report.checks.find((check) => check.id === id)?.evidence;
  assert.equal(evidence('core-native-isolation')?.permissionMode, 'ask');
  if (manifest.phase === 'first-launch') {
    const queue = evidence('core-queue-and-send-now'), stop = evidence('core-stop-and-resend');
    assert.equal(queue?.sessionId, e.sessionId); assert.equal(queue.admittedViaUi, true);
    assert.equal(queue.immediateBeforeQueued, true); assert.equal(queue.noDuplicate, true);
    assert(Array.isArray(queue.completions) && queue.completions.some((item) => item.sessionId === e.sessionId
      && item.stopReason === 'cancelled' && item.cancelTrigger === 'send_now'));
    const completed = queue.completions.filter((item) => item.sessionId === e.sessionId && item.stopReason === 'end_turn');
    assert(completed.length >= 2 && completed.every((item) => typeof item.promptId === 'string' && item.promptId.length > 0)
      && new Set(completed.map((item) => item.promptId)).size === completed.length, 'Real distinct queued/immediate completions are missing');
    assert(stop?.stopped?.sessionId === e.sessionId && stop.stopped.cancelTrigger === 'stop' && stop.stopped.stopReason === 'cancelled'
      && stop.lateWaitMs >= 1000 && stop.recovery?.marker === e.markers.recovery && stop.recovery.completion.stopReason === 'end_turn');
    const draft = evidence('core-automation-draft-navigation-exit');
    assert.equal(draft?.marker, `CORE-DRAFT-${manifest.nonce}`);
    for (const key of ['restoredViaNavigation', 'quitConfirmationViaUi', 'exitCancelledAndRuntimeReady', 'noTasksCreated', 'discardedViaUi']) assert.equal(draft[key], true);
    const custom = evidence('core-custom-qwen-connection');
    const project = evidence('core-project-move-switch-ui');
    assert.equal(project?.projectForkedSessionId, e.projectForkedSessionId);
    assert.equal(project.forkPendingBeforeTurn, true); assert.equal(project.forkPendingAfterTurn, false); assert.equal(project.sourcePendingUnchanged, true);
    assert.equal(project.projectTurn?.sessionId, e.projectForkedSessionId); assert.equal(project.projectTurn.marker, e.markers.project);
    assert.equal(project.projectTurn.completion?.stopReason, 'end_turn');
    assert.equal(custom?.testedAndSavedViaUi, true); assert.equal(custom.explicitHttpConsent, false); assert.equal(custom.noCredentials, false);
    assert.equal(custom.actualSessionModelId, e.customModel.modelId); assert.equal(custom.response?.modelId, e.customModel.modelId);
    assert.equal(custom.response.marker, e.customModel.marker); assert.equal(custom.response.completion?.stopReason, 'end_turn');
    for (const item of manifest.officeFixtures) {
      const read = evidence(`core-office-${item.format}-runtime-read`); assert.equal(read?.path, item.path);
      assert.equal(read.toolOutputFingerprint, true); assert.equal(read.completion?.stopReason, 'end_turn');
    }
  } else {
    assert.equal(evidence('core-restart-custom-model')?.modelId, e.customModel.modelId);
    assert.equal(evidence('core-restart-custom-model')?.selectedSessionId, e.sessionId);
  }
  if (manifest.phase === 'restart') assert.deepEqual(e, previousExpected, 'Restart changed exact first-launch expectation');
  return e;
}

// Tiny uncompressed ZIP writer for explicit synthetic EPUB/ODF inputs; output
// is opened independently by Python zipfile/XML in the resource proof.
function zip(entries) {
  let offset = 0; const local = [], central = [];
  const crc = (bytes) => { let v = 0xffffffff; for (const byte of bytes) { v ^= byte; for (let i = 0; i < 8; i++) v = (v >>> 1) ^ ((v & 1) ? 0xedb88320 : 0); } return (v ^ 0xffffffff) >>> 0; };
  for (const [name, text] of entries) {
    const filename = Buffer.from(name), data = Buffer.from(text), check = crc(data), header = Buffer.alloc(30), directory = Buffer.alloc(46);
    header.writeUInt32LE(0x04034b50); header.writeUInt16LE(20, 4); header.writeUInt16LE(0x0800, 6);
    header.writeUInt32LE(check, 14); header.writeUInt32LE(data.length, 18); header.writeUInt32LE(data.length, 22); header.writeUInt16LE(filename.length, 26);
    directory.writeUInt32LE(0x02014b50); directory.writeUInt16LE(20, 4); directory.writeUInt16LE(20, 6); directory.writeUInt16LE(0x0800, 8);
    directory.writeUInt32LE(check, 16); directory.writeUInt32LE(data.length, 20); directory.writeUInt32LE(data.length, 24); directory.writeUInt16LE(filename.length, 28); directory.writeUInt32LE(offset, 42);
    local.push(header, filename, data); central.push(directory, filename); offset += header.length + filename.length + data.length;
  }
  const index = Buffer.concat(central), end = Buffer.alloc(22); end.writeUInt32LE(0x06054b50); end.writeUInt16LE(entries.length, 8); end.writeUInt16LE(entries.length, 10);
  end.writeUInt32LE(index.length, 12); end.writeUInt32LE(offset, 16); return Buffer.concat([...local, index, end]);
}
export async function createCoreFixtures(home, nonce) {
  assert(/^[a-f0-9]{32}$/.test(nonce)); assert(/^echo-native-review-core-[A-Za-z0-9_-]+$/.test(basename(home)));
  assert.equal(realpathSync(home), home); assert(lstatSync(home).isDirectory() && !lstatSync(home).isSymbolicLink());
  assert.equal(lstatSync(home).mode & 0o777, 0o700);
  const cwd = join(home, 'workspaces', 'default'), root = join(cwd, '办公读取'); mkdirSync(root, { recursive: true, mode: 0o700 });
  const fixtures = ['xlsx', 'epub', 'ods'].map((format) => ({ format, path: join(root, `中文 ${format}.${format}`), marker: `CORE-${format.toUpperCase()}-${nonce}`,
    ...(format === 'epub' ? { excludedMarker: `CORE-NAV-EXCLUDED-${nonce}` } : {}) }));
  const workbook = new ExcelJS.Workbook(), sheet = workbook.addWorksheet('中文指纹');
  sheet.addRow(['指纹', '20位文本编号']); sheet.addRow([fixtures[0].marker, '00123456789012345678']); sheet.getCell('B2').numFmt = '@';
  writeFileSync(fixtures[0].path, Buffer.from(await workbook.xlsx.writeBuffer()), { flag: 'wx', mode: 0o600 });
  const epub = fixtures[1];
  writeFileSync(epub.path, zip([
    ['mimetype', 'application/epub+zip'],
    ['META-INF/container.xml', '<?xml version="1.0"?><container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="书籍 内容/package.opf" media-type="application/oebps-package+xml"/></rootfiles></container>'],
    ['书籍 内容/package.opf', '<?xml version="1.0"?><package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="id"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:identifier id="id">synthetic-review</dc:identifier><dc:title>中文章节验收</dc:title><dc:language>zh-CN</dc:language></metadata><manifest><item id="first" href="99-%E7%AB%A0%20%E4%B8%80.xhtml" media-type="application/xhtml+xml"/><item id="second" href="01-last.xhtml" media-type="application/xhtml+xml"/><item id="nav" href="00-nav.xhtml" media-type="application/xhtml+xml" properties="nav"/></manifest><spine><itemref idref="first"/><itemref idref="second"/></spine></package>'],
    ['书籍 内容/99-章 一.xhtml', `<html xmlns="http://www.w3.org/1999/xhtml"><body><p>第一章 ${epub.marker}</p></body></html>`],
    ['书籍 内容/01-last.xhtml', `<html xmlns="http://www.w3.org/1999/xhtml"><body><p>第二章 ${epub.marker}</p></body></html>`],
    ['书籍 内容/00-nav.xhtml', `<html xmlns="http://www.w3.org/1999/xhtml"><body><nav>${epub.excludedMarker}</nav></body></html>`],
  ]), { flag: 'wx', mode: 0o600 });
  const ods = fixtures[2], textCell = (value, repeat = '') => `<table:table-cell office:value-type="string"${repeat}><text:p>${value}</text:p></table:table-cell>`;
  writeFileSync(ods.path, zip([
    ['mimetype', 'application/vnd.oasis.opendocument.spreadsheet'],
    ['META-INF/manifest.xml', '<?xml version="1.0"?><manifest:manifest xmlns:manifest="urn:oasis:names:tc:opendocument:xmlns:manifest:1.0"><manifest:file-entry manifest:full-path="/" manifest:media-type="application/vnd.oasis.opendocument.spreadsheet"/><manifest:file-entry manifest:full-path="content.xml" manifest:media-type="text/xml"/></manifest:manifest>'],
    ['content.xml', `<?xml version="1.0"?><office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" office:version="1.3"><office:body><office:spreadsheet><table:table table:name="中文指纹"><table:table-row>${textCell('A')}<table:table-cell/>${textCell('B')}</table:table-row><table:table-row>${textCell('重复列', ' table:number-columns-repeated="2"')}</table:table-row><table:table-row table:number-rows-repeated="2">${textCell(`重复行 ${ods.marker}`)}</table:table-row><table:table-row table:number-rows-repeated="1048570"><table:table-cell table:number-columns-repeated="16384"/></table:table-row></table:table></office:spreadsheet></office:body></office:document-content>`],
  ]), { flag: 'wx', mode: 0o600 });
  return fixtures;
}
function sessionDirectory(home, sessionId, allowMissing = false) {
  assert(/^[A-Za-z0-9-]{1,128}$/.test(sessionId)); const root = join(home, 'sessions');
  const directories = existsSync(root) ? readdirSync(root, { withFileTypes: true }).filter((entry) => entry.isDirectory() && !entry.isSymbolicLink())
    .map((entry) => join(root, entry.name, sessionId)).filter((path) => existsSync(path)) : [];
  if (allowMissing && directories.length === 0) return null;
  assert.equal(directories.length, 1, 'Session directory missing/ambiguous');
  const directory = directories[0]; assert.equal(realpathSync(directory), directory); assert(lstatSync(directory).isDirectory() && !lstatSync(directory).isSymbolicLink()); return directory;
}
const sessionFiles = (home, sessionId) => {
  const root = sessionDirectory(home, sessionId), history = ordinary(join(root, 'chat_history.jsonl'), home), updates = ordinary(join(root, 'updates.jsonl'), home);
  return { root, history, updates, historySha256: sha(history), updatesSha256: sha(updates) };
};
export function physical(home, expected, fixtures, first) {
  const source = sessionFiles(home, expected.sessionId), fork = sessionFiles(home, expected.forkedSessionId);
  const jobsPath = join(home, 'echoagent-automations.json');
  if (existsSync(jobsPath)) assert.equal(JSON.parse(ordinary(jobsPath, home)).automations.length, 0, 'Draft editor unexpectedly created a persisted task');
  assert.equal(sessionDirectory(home, expected.deletedSessionId, true), null, 'Deleted session data remains');
  for (const entry of [source, fork]) {
    assert(entry.history.toString('utf8').includes(expected.markers.recovery), 'Actual source/fork history lost recovery prompt');
    const summary = JSON.parse(ordinary(join(entry.root, 'summary.json'), home));
    assert.equal(summary.info?.cwd, expected.cwd); assert.equal(summary.generated_title, expected.title);
    const model = entry === source ? expected.customModel.modelId : expected.modelId;
    assert([model, model.split('/').at(-1)].includes(summary.current_model_id), 'Physical source/fork model mismatch');
  }
  if (first) assert.equal(fork.historySha256, first.fork.historySha256, 'Read-only restart changed fork chat history');
  const projects = JSON.parse(ordinary(join(home, 'echoagent-projects.json'), home));
  const project = projects.find((item) => item.id === expected.project.id);
  assert(project?.name === expected.project.name && project.conversations.some((item) => item.sessionId === expected.sessionId), 'Physical project association missing');
  assert(project.conversations.some((item) => item.sessionId === expected.projectForkedSessionId && item.pendingProjectContext === false), 'Physical project fork association/consumed context missing');
  assert(!projects.some((p) => [...p.conversations, ...p.plans, ...p.tasks].some((item) => item.sessionId === expected.deletedSessionId)), 'Deleted fork still referenced by project');
  assert.equal(JSON.stringify(project.plans), expected.project.initialPlans); assert.equal(JSON.stringify(project.tasks), expected.project.initialTasks);
  const projectFork = sessionFiles(home, expected.projectForkedSessionId), projectHistory = projectFork.history.toString('utf8');
  assert(projectHistory.includes(`你正在 EchoAgent 项目「${expected.project.name}」中工作。`)
    && projectHistory.includes(expected.markers.project), 'Real project contract was not sent to the fork Runtime/model');
  if (first) assert.equal(projectFork.historySha256, first.projectFork.historySha256, 'Restart changed project fork chat history');
  const fixtureFiles = fixtures.map((f) => ({ format: f.format, path: f.path, sha256: sha(ordinary(f.path, home)) }));
  if (first) assert.deepEqual(fixtureFiles, first.fixtures, 'Read-only task or restart modified Office source');
  const child = sessionFiles(home, expected.child.sessionId);
  assert.equal(JSON.parse(ordinary(join(child.root, 'summary.json'), home)).info?.cwd, expected.cwd, 'Child escaped parent workspace');
  const updates = child.updates.toString('utf8').trim().split('\n').filter(Boolean).map(JSON.parse);
  assert(updates.some((value) => JSON.stringify(value).includes(expected.child.marker)), 'Actual child output missing on disk');
  assert(!updates.some((value) => ['tool_call', 'tool_call_update'].includes((value.params?.update ?? value.params)?.sessionUpdate)), 'No-tool child invoked a tool');
  return { source: { sessionId: expected.sessionId, historySha256: source.historySha256, updatesSha256: source.updatesSha256 },
    fork: { sessionId: expected.forkedSessionId, historySha256: fork.historySha256, updatesSha256: fork.updatesSha256 },
    child: { sessionId: expected.child.sessionId, historySha256: child.historySha256, noTools: true },
    projectFork: { sessionId: expected.projectForkedSessionId, historySha256: projectFork.historySha256, currentProjectContractInActualHistory: true },
    deletedSessionAbsent: true, projectSha256: sha(ordinary(join(home, 'echoagent-projects.json'), home)), fixtures: fixtureFiles };
}
// Isolation is established by controlled marker tests, never by reading private files.
const foreignSnapshot = () => [];
const managed = (child) => { const s = { child, ended: false, error: null }; s.exit = new Promise((accept) => {
  child.once('error', (e) => { s.error = e; s.ended = true; accept(); }); child.once('exit', () => { s.ended = true; accept(); });
}); return s; };
async function terminate(state) {
  if (!state || state.ended) return;
  state.child.kill('SIGTERM'); await Promise.race([state.exit, delay(5000, null, { ref: false })]);
  if (!state.ended) { state.child.kill('SIGKILL'); await Promise.race([state.exit, delay(3000, null, { ref: false })]); }
}
export async function runNativeCoreReview({ executable, resourceOnly = false } = {}) {
  assert.equal(process.platform, 'darwin', 'Actual review requires macOS Native WebView');
  if (!resourceOnly) assertNativeReviewSessionUnlocked();
  if (!resourceOnly) { assert(executable, 'Pass the newly built fixed Core application'); executable = realpathSync(resolve(executable)); assert(lstatSync(executable).isFile()); }
  const outputRoot = nativeReviewOutput('native-core', 'NATIVE_CORE_REVIEW_OUTPUT');
  mkdirSync(outputRoot, { recursive: true }); const output = mkdtempSync(join(outputRoot, 'run-'));
  const home = realpathSync(mkdtempSync(join(tmpdir(), 'echo-native-review-core-'))); chmodSync(home, 0o700);
  const nonce = randomBytes(16).toString('hex'), cwd = join(home, 'workspaces', 'default'), legacy = join(home, 'empty-legacy'); mkdirSync(legacy, { mode: 0o700 });
  const fixtures = await createCoreFixtures(home, nonce), fixtureBefore = fixtures.map((f) => ({ path: f.path, sha256: sha(ordinary(f.path, home)) }));
  const revocations = readonlyCompatibilitySourceRevocations();
  writeFileSync(join(home, 'config.toml'), `[marketplace]\nofficial_marketplace_auto_installed = true\ndisabled_sources = [${revocations.map((value) => JSON.stringify(value)).join(', ')}]\n[claude_compat]\nimported = true\n`, { flag: 'wx', mode: 0o600 });
  const modelId = seedReviewModel(home);
  const beforeForeign = foreignSnapshot(), result = { version: 1, kind: 'native-core-driver', home, cwd, nonce, output,
    executable: executable || null, binarySha256: executable ? sha(readFileSync(executable)) : null, nativeVerified: false, passed: false,
    phases: [], errors: [], osPickerVerified: false, homeEnvironmentChanged: false, syntheticFixtures: true,
    fixtures: fixtureBefore, compatibilityRevocationCount: revocations.length, foreignBefore: beforeForeign };
  let active, interrupted = false, expected = null, firstPhysical;
  const stop = () => { interrupted = true; active?.child.kill('SIGTERM'); };
  if (!resourceOnly) { process.once('SIGINT', stop); process.once('SIGTERM', stop); }
  try {
    if (resourceOnly) { result.resourcesPrepared = true; return result; }
    const env = { ...cleanNativeReviewEnvironment(process.env), GROK_HOME: legacy, ECHO_DESKTOP_REVIEW: '1', ECHO_DESKTOP_REVIEW_CORE: '1' };
    delete env.ECHO_AGENT_HOME;
    for (const key of Object.keys(env)) if (key.startsWith('ECHO_DESKTOP_REVIEW_') && key !== 'ECHO_DESKTOP_REVIEW_CORE') delete env[key];
    for (const phase of ['first-launch', 'restart']) {
      if (interrupted) throw new Error('Native Core interrupted');
      if (phase === 'restart' && !expected?.ownedSessionIds?.length) { result.errors.push('restart: first phase created no session; recovery not exercised'); break; }
      const manifest = validateCoreManifest({ schema: 1, home, cwd, nonce, phase, modelId, officeFixtures: fixtures, expected });
      writeFileSync(join(home, 'native-core-manifest.private.json'), JSON.stringify(manifest), { mode: 0o600 });
      const reportPath = join(home, 'native-review.json'); if (existsSync(reportPath)) unlinkSync(reportPath);
      const startedAt = Date.now(); active = managed(spawn(executable, ['--data-home', home], { env: { ...env, ECHO_DESKTOP_REVIEW_PHASE: phase }, stdio: ['ignore', 'pipe', 'pipe'] }));
      const logs = captureReviewModelLogs(active.child, join(output, `${phase}-app.log`));
      // This journey asserts actual visible UI. Keep only its own application
      // in front while WebKit is running it, without AX, input, or screenshots.
      // Activation never substitutes for a functional assertion or extends a bound.
      let nextActivation = 0;
      const activateOwnedWindow = () => {
        if (Date.now() < nextActivation || active.ended) return;
        nextActivation = Date.now() + 15000;
        const activated = activateNativeReviewWindow(active.child.pid);
        (result.windowActivation ??= []).push({ phase, pid: active.child.pid, activated, at: new Date().toISOString() });
      };
      let beforeFork, handoffError, beforeHandled = false, afterHandled = false;
      const handoffs = (async () => {
        while (!active.ended) {
          activateOwnedWindow();
          try {
            const readyPath = join(cwd, 'core-fork-ready.json'), donePath = join(cwd, 'core-fork-done.json');
            if (phase === 'first-launch' && !beforeHandled && existsSync(readyPath)) {
              const data = JSON.parse(ordinary(readyPath, home)); assert.equal(data.nonce, nonce);
              beforeFork = sessionFiles(home, data.sessionId); beforeFork.sessionId = data.sessionId; beforeHandled = true;
              writeFileSync(join(cwd, 'core-fork-before.json'), JSON.stringify({ nonce, passed: true, sessionId: data.sessionId,
                historySha256: beforeFork.historySha256, updatesSha256: beforeFork.updatesSha256 }), { flag: 'wx', mode: 0o600 });
            }
            if (phase === 'first-launch' && beforeHandled && !afterHandled && existsSync(donePath)) {
              const data = JSON.parse(ordinary(donePath, home)); assert.equal(data.nonce, nonce); assert.equal(data.sessionId, beforeFork.sessionId);
              assert.equal(data.marker, `CORE-RECOVER-${nonce}`); assert.notEqual(data.forkedSessionId, data.sessionId);
              const after = sessionFiles(home, data.sessionId), fork = sessionFiles(home, data.forkedSessionId);
              assert.equal(after.historySha256, beforeFork.historySha256, 'Fork mutated original chat history');
              assert.equal(after.updatesSha256, beforeFork.updatesSha256, 'Fork mutated original update journal');
              assert(fork.history.toString('utf8').includes(data.marker)); afterHandled = true;
              const proof = { nonce, passed: true, originalHistoryUnchanged: true, originalUpdatesUnchanged: true,
                sourceSessionId: data.sessionId, forkedSessionId: data.forkedSessionId, sourceHistorySha256: after.historySha256,
                sourceUpdatesSha256: after.updatesSha256, forkHistorySha256: fork.historySha256 };
              writeFileSync(join(output, 'fork-physical-proof.json'), JSON.stringify(proof, null, 2), { mode: 0o600 });
              writeFileSync(join(cwd, 'core-fork-after.json'), JSON.stringify(proof), { flag: 'wx', mode: 0o600 });
            }
          } catch (error) { handoffError = String(error); break; }
          await delay(100);
        }
      })();
      try {
        const finished = await Promise.race([active.exit.then(() => true), delay(phase === 'first-launch' ? 820000 : 180000, false, { ref: false })]);
        assert(finished, 'Native Core exceeded process bound'); assert(!active.error, 'Native Core process failed to spawn');
        await logs.finished; assert(!logs.overflow, 'Native app log exceeded limit'); assert(!logs.incomplete, 'Native log pipes did not close after process exit'); await handoffs;
        const report = JSON.parse(ordinary(reportPath, home)); writeFileSync(join(output, `${phase}.json`), redactReviewModelSecrets(readFileSync(reportPath)), { mode: 0o600 });
        // Preserve safe partial expectation even when the model phase is failed.
        if (phase === 'first-launch' && report.expected?.home === home && report.expected.nonce === nonce
          && Array.isArray(report.expected.ownedSessionIds)) expected = report.expected;
        const verified = verifyNativeCoreReport({ report, manifest, pid: active.child.pid, startedAt,
          exitCode: active.child.exitCode, signal: active.child.signalCode, previousExpected: expected });
        assert(!handoffError && (phase !== 'first-launch' || afterHandled), 'Independent fork physical barrier failed/missing');
        const files = physical(home, verified, fixtures, firstPhysical);
        assert.deepEqual(files.fixtures.map(({ path, sha256 }) => ({ path, sha256 })), fixtureBefore, 'Read tool altered input fixtures');
        writeFileSync(join(output, `${phase}-filesystem.json`), JSON.stringify(files, null, 2), { mode: 0o600 });
        if (phase === 'first-launch') { expected = verified; firstPhysical = files; }
        result.phases.push({ phase, pid: active.child.pid, exitCode: active.child.exitCode, signal: active.child.signalCode, passed: true, checks: report.checks.length });
      } catch (error) { result.errors.push(`${phase}: ${String(error?.message || error).slice(0, 2000)}`);
        result.phases.push({ phase, pid: active.child.pid, exitCode: active.child.exitCode, signal: active.child.signalCode, passed: false });
      } finally { await terminate(active); await logs.finished; await handoffs; active = null; }
      assert.deepEqual(foreignSnapshot(), beforeForeign, 'Acceptance changed default Echo/Claude configuration');
    }
    result.passed = result.errors.length === 0 && result.phases.length === 2 && result.phases.every((phase) => phase.passed);
    result.nativeVerified = result.passed;
  } catch (error) { result.errors.push(String(error?.message || error).slice(0, 2000)); }
  finally {
    await terminate(active); process.removeListener('SIGINT', stop); process.removeListener('SIGTERM', stop);
    result.foreignAfter = foreignSnapshot(); result.defaultHomeChanged = JSON.stringify(result.foreignAfter) !== JSON.stringify(beforeForeign);
    if (result.defaultHomeChanged) { result.passed = false; result.nativeVerified = false; result.errors.push('Default/foreign configuration changed'); }
    result.finishedAt = new Date().toISOString(); writeFileSync(join(output, 'result.json'), redactReviewModelSecrets(JSON.stringify(result, null, 2)), { mode: 0o600 });
  }
  return result;
}
if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  const resourceOnly = process.argv.includes('--resource-only');
  const result = await runNativeCoreReview({ executable: resourceOnly ? undefined : process.argv[2], resourceOnly });
  console.log(redactReviewModelSecrets(JSON.stringify({ output: result.output, home: result.home, phases: result.phases, resourcesPrepared: result.resourcesPrepared || false,
    passed: result.passed, nativeVerified: result.nativeVerified, errors: result.errors })));
  process.exitCode = resourceOnly ? (result.resourcesPrepared && result.errors.length === 0 ? 0 : 1) : result.passed ? 0 : 1;
}
