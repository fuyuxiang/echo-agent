import { nativeReviewOutput } from './lib/native-review-output.mjs';
import { seedReviewModel } from './lib/native-review-model.mjs';
// Private real Native Git journey. No repository push, global Git write or model.
import assert from 'node:assert/strict';
import { execFileSync, spawn } from 'node:child_process';
import { createHash, randomBytes } from 'node:crypto';
import { createServer } from 'node:http';
import { chmodSync, existsSync, lstatSync, mkdirSync, mkdtempSync, readFileSync,
  realpathSync, unlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { pathToFileURL } from 'node:url';
import { auditNativeGitConfiguration, nativeGitProcessEnvironment,
  runNativeGitCommand } from './lib/native-git-review.mjs';
import { readonlyCompatibilitySourceRevocations } from './lib/native-core-review-isolation.mjs';
import { assertNativeReviewSessionUnlocked, activateNativeReviewWindow } from './lib/native-review-window.mjs';

const digest = value => createHash('sha256').update(value).digest('hex');
const checks = ['ide-private-native-isolation', 'ide-git-provider-native', 'ide-chinese-editor-native',
  'ide-search-native', 'ide-terminal-native', 'ide-git-diff-native', 'ide-git-stage-physical',
  'ide-git-commit-physical', 'ide-own-preview-native', 'ide-unsaved-cancel-native', 'ide-unsaved-save-native',
  'ide-disconnect-dirty-cancel-native', 'ide-disconnect-confirm-recovery-native', 'ide-parallel-worktree-native',
  'ide-project-switch-dirty-cancel-native', 'ide-project-switch-save-native', 'ide-project-switch-return-native'];
export function selectOwnedTheiaBackend(table, appPid, nodePath, mainPath) {
  assert(Number.isSafeInteger(appPid) && appPid > 1);
  const matches = table.split('\n').map(line => /^\s*(\d+)\s+(\d+)\s+(.+)$/.exec(line)).filter(Boolean)
    .filter(row => Number(row[2]) === appPid && row[3].startsWith(`${nodePath} ${mainPath} --port=`)
      && / --hostname=127\.0\.0\.1 --plugins=local-dir:plugins$/.test(row[3]));
  assert.equal(matches.length, 1, 'Expected exactly one actual child of the private app with fixed packaged Theia arguments');
  const pid = Number(matches[0][1]); assert(Number.isSafeInteger(pid) && pid > 1 && pid !== appPid);
  return pid;
}
export function processHasExited(status, state) {
  assert([0, 1].includes(status), 'Process status read failed');
  if (status === 1) return true;
  const value = state.trim();
  if (!value) return false;
  assert(/^[A-Z?][A-Za-z0-9?+<> -]*$/.test(value), `Invalid kernel process state ${JSON.stringify(value)}`);
  return value.startsWith('Z');
}
export function verifyIdeReport(report, manifest, pid, startedAt) {
  assert.equal(report?.version, 1); assert.equal(report.kind, 'native-ide-git');
  assert.equal(report.passed, true); assert.deepEqual(report.errors, []);
  assert.equal(report.nativeVerified, false); assert.equal(report.fakeIPC, false); assert.equal(report.osPickerVerified, false);
  assert.equal(report.nativeContext?.pid, pid); assert.equal(report.nativeContext.ideRequested, true);
  assert.equal(report.nativeContext.evaluations, 1); assert.equal(report.nativeContext.home, manifest.home);
  for (const name of ['home', 'cwd', 'nonce', 'generation', 'phase']) assert.equal(report[name], manifest[name]);
  assert(Date.parse(report.finishedAt) >= startedAt && Date.parse(report.finishedAt) <= Date.now() + 1000);
  assert.equal(report.checks?.length, checks.length);
  for (const id of checks) {
    const matched = report.checks.filter(check => check.id === id); assert.equal(matched.length, 1); assert.equal(matched[0].result, 'passed');
  }
  const provider = report.checks.find(check => check.id === 'ide-git-provider-native').evidence;
  assert.equal(provider.provider.id, 'git'); assert.equal(provider.root, manifest.cwd);
  assert.equal(report.expected.initialHead, manifest.initialHead); assert.notEqual(report.expected.committedHead, manifest.initialHead);
  assert.equal(report.expected.version3, manifest.version3); assert.equal(report.expected.file, manifest.file);
  const disconnected = report.checks.find(check => check.id === 'ide-disconnect-dirty-cancel-native').evidence;
  assert.equal(disconnected.stopped.ownedParentPid, pid); assert.equal(disconnected.stopped.processStopped, true);
  assert.equal(disconnected.sameFrame, true); assert.equal(disconnected.dirty, true);
  assert.equal(disconnected.text, manifest.version1); assert.equal(disconnected.physicalVersion, 3);
  const recovered = report.checks.find(check => check.id === 'ide-disconnect-confirm-recovery-native').evidence;
  assert.equal(recovered.newFrame, true); assert.equal(recovered.dirty, false);
  assert.equal(recovered.text, manifest.version3); assert.equal(recovered.provider.id, 'git');
  const worktree = report.checks.find(check => check.id === 'ide-parallel-worktree-native').evidence;
  const root = report.expected.worktreeRoot;
  assert(root.startsWith(`${manifest.home}/coding-worktrees/`)
    && /^[a-f0-9-]{36}\/worktree$/.test(root.slice(`${manifest.home}/coding-worktrees/`.length)));
  assert.equal(worktree.isolation.root, root); assert.equal(worktree.isolation.sourceRoot, manifest.cwd);
  assert.equal(worktree.isolation.baseHead, report.expected.committedHead); assert.equal(worktree.actualUiNewTask, true);
  assert.equal(worktree.committedFile.text, manifest.version2);
  assert.equal(worktree.persistedSyntheticTask.id, report.expected.originalTaskId);
  assert.equal(worktree.persistedSyntheticTask.phase, 'discovering'); assert(!worktree.persistedSyntheticTask.sessionId);
  const cancelled = report.checks.find(check => check.id === 'ide-project-switch-dirty-cancel-native').evidence;
  assert.equal(cancelled.root, root); assert.equal(cancelled.dirty, true); assert.equal(cancelled.text, manifest.version1); assert.equal(cancelled.sameFrame, true);
  const returned = report.checks.find(check => check.id === 'ide-project-switch-return-native').evidence;
  assert.equal(returned.root, root); assert.equal(returned.dirty, false); assert.equal(returned.text, manifest.version1);
  assert.equal(report.expected.worktreeSavedVersion, manifest.version1);
  return report.expected;
}

export function ideRestartManifest(manifest, expected) {
  assert.equal(manifest.phase, 'first-launch'); assert.equal(manifest.expected, null);
  assert.equal(expected.initialHead, manifest.initialHead); assert.equal(expected.file, manifest.file);
  assert.equal(expected.version3, manifest.version3); assert.equal(expected.worktreeSavedVersion, manifest.version1);
  assert(/^(?:[a-f0-9]{40}|[a-f0-9]{64})$/.test(expected.committedHead));
  assert.notEqual(expected.committedHead, expected.initialHead);
  assert(/^[a-f0-9-]{36}$/.test(expected.originalTaskId));
  assert(expected.worktreeRoot.startsWith(`${manifest.home}/coding-worktrees/`)
    && /^[a-f0-9-]{36}\/worktree$/.test(expected.worktreeRoot.slice(`${manifest.home}/coding-worktrees/`.length)));
  return { ...manifest, phase: 'restart', expected: structuredClone(expected) };
}

export function verifyIdeRestartReport(report, manifest, pid, startedAt) {
  assert.equal(manifest.phase, 'restart'); assert(manifest.expected);
  assert.equal(report?.version, 1); assert.equal(report.kind, 'native-ide-git');
  assert.equal(report.passed, true); assert.deepEqual(report.errors, []);
  assert.equal(report.nativeVerified, false); assert.equal(report.fakeIPC, false); assert.equal(report.osPickerVerified, false);
  assert.equal(report.nativeContext?.pid, pid); assert.equal(report.nativeContext.ideRequested, true);
  assert.equal(report.nativeContext.evaluations, 1); assert.equal(report.nativeContext.home, manifest.home);
  assert.equal(report.nativeContext.phase, 'restart');
  for (const name of ['home', 'cwd', 'nonce', 'generation', 'phase']) assert.equal(report[name], manifest[name]);
  assert(Date.parse(report.finishedAt) >= startedAt && Date.parse(report.finishedAt) <= Date.now() + 1000);
  assert.deepEqual(report.expected, manifest.expected);
  const ids = ['ide-private-native-isolation', 'ide-restart-worktree-native', 'ide-restart-source-editor-native', 'ide-restart-source-task-native'];
  assert.equal(report.checks?.length, ids.length);
  const evidence = Object.fromEntries(ids.map(id => {
    const matched = report.checks.filter(check => check.id === id);
    assert.equal(matched.length, 1); assert.equal(matched[0].result, 'passed'); return [id, matched[0].evidence];
  }));
  const isolation = evidence[ids[0]], worktree = evidence[ids[1]], source = evidence[ids[2]], task = evidence[ids[3]];
  assert.deepEqual(isolation, { pid, cwd: manifest.cwd, fixedManifest: true });
  assert.equal(worktree.workspace.root, manifest.expected.worktreeRoot); assert.equal(worktree.sameWorktree, true);
  assert.equal(worktree.savedWorktree.root, manifest.expected.worktreeRoot);
  assert.equal(worktree.savedWorktree.path, join(manifest.expected.worktreeRoot, manifest.file));
  assert.equal(worktree.savedWorktree.text, manifest.version1); assert.equal(worktree.savedWorktree.dirty, false);
  assert.equal(worktree.savedWorktree.visible, true);
  assert.equal(source.source.path, join(manifest.cwd, manifest.file)); assert.equal(source.source.visible, true);
  assert.equal(source.source.text, manifest.version3); assert.equal(source.source.dirty, false);
  assert.equal(source.provider.id, 'git'); assert.equal(source.noNewCommit, true);
  assert.deepEqual(task, { originalTaskId: manifest.expected.originalTaskId, phase: 'discovering', visibleInUi: true, modelExecuted: false });
  return report.expected;
}

export function verifyIdeReportHistory(history, reports, prefix = '') {
  assert(history.startsWith(prefix), 'Native report history changed an earlier launch');
  assert(history.endsWith('\n'), 'Native report history is incomplete');
  const entries = history.slice(0, -1).split('\n').map(line => JSON.parse(line));
  assert.deepEqual(entries, reports, 'Expected exactly one preserved native report per launch');
  return { entries: entries.length, sha256: digest(history), preservedPrefixSha256: digest(prefix) };
}

export function verifyIdeRestartPhysical(before, after, manifest) {
  const expected = manifest.expected;
  for (const facts of [before, after]) {
    assert.equal(facts.sourceRoot, manifest.cwd); assert.equal(facts.worktreeRoot, expected.worktreeRoot);
    for (const hash of [facts.sourceHistorySha256, facts.worktreeHistorySha256, facts.terminalSha256, facts.task?.sha256]) assert(/^[a-f0-9]{64}$/.test(hash));
    assert.equal(facts.sourceHead, expected.committedHead); assert.equal(facts.worktreeHead, expected.committedHead);
    assert.equal(facts.sourceSha256, digest(manifest.version3)); assert.equal(facts.worktreeSha256, digest(manifest.version1));
    assert.equal(facts.commitsSinceBaseline, '1'); assert.deepEqual(facts.sourceDiffPaths, [manifest.file]);
    assert.deepEqual(facts.stagedPaths, []); assert.equal(facts.remotes, '');
    assert.deepEqual(facts.baselineFiles, manifest.baselineFiles);
    assert.equal(facts.task.id, expected.originalTaskId); assert.equal(facts.task.phase, 'discovering');
    assert.equal(facts.task.name, `隔离并行夹具-${manifest.generation}`); assert.equal(facts.task.sessionId, null);
  }
  assert.deepEqual(after, before, 'Restart changed private source/worktree/Git/task/terminal history');
  return { passed: true, before, after, unchanged: true };
}

// Pure launch admission/environment logic is guard-tested without GUI access.
export function nativeIdeReviewEnvironment(base, legacy, phase = 'first-launch') {
  assert(['first-launch', 'restart'].includes(phase));
  const env = { ...base, GROK_HOME: legacy, ECHO_DESKTOP_REVIEW: '1', ECHO_DESKTOP_REVIEW_IDE: '1', ECHO_DESKTOP_REVIEW_PHASE: phase };
  for (const key of Object.keys(env)) if ((key.startsWith('ECHO_DESKTOP_REVIEW_') && !['ECHO_DESKTOP_REVIEW_IDE', 'ECHO_DESKTOP_REVIEW_PHASE'].includes(key))
    || ['ECHO_AGENT_HOME', 'ECHO_DESKTOP_VALIDATION', 'ECHO_THEIA_NODE', 'NODE_OPTIONS', 'NODE_PATH'].includes(key)) delete env[key];
  // Capture native admission dispatch while leaving all other module filters intact.
  env.RUST_LOG = `${env.RUST_LOG || 'warn'},echoagent_lib::desktop_validation=info,echoagent_lib::native_ide_validation=info`;
  return env;
}

export function collectIdeLaunchDiagnostics({ home, cwd, pid, executable, startedAt, ended, exitCode, signal, phase = 'first-launch', now = Date.now() }) {
  const boundedLogTail = path => {
    if (!existsSync(path)) return null;
    const stat = lstatSync(path);
    if (!stat.isFile() || stat.isSymbolicLink() || realpathSync(path) !== path) return '[not an owned ordinary log file]';
    if (stat.size > 8 * 1024 * 1024) return '[log exceeds bounded diagnostic size]';
    return readFileSync(path, 'utf8').slice(-12000);
  };
  return {
    pid, executable, phase, elapsedMs: now - startedAt, ended, exitCode: exitCode ?? null, signal: signal ?? null,
    nativeReportExists: existsSync(join(home, 'native-review.json')),
    nativeHistoryExists: existsSync(join(home, 'native-review-history.jsonl')),
    terminalProofExists: existsSync(join(cwd, '.ide-terminal-proof.json')),
    barriers: Object.fromEntries(['stage', 'commit', 'disconnect'].map(phase => [phase, {
      ready: existsSync(join(cwd, `.native-ide-${phase}-ready.json`)), proof: existsSync(join(cwd, `.native-ide-${phase}-proof.json`)),
    }])),
    // A debug binary using Vite's localhost devUrl is deliberately rejected by
    // require_main_internal. Do not weaken that boundary to make a test pass.
    requiredPageOrigin: 'tauri://localhost or http(s)://tauri.localhost without a port; use the embedded custom-protocol build',
    theiaLogTail: boundedLogTail(join(home, 'theia', 'theia.log')),
  };
}

export function requireIdeRendererReport(diagnostics) {
  const file = `${diagnostics.phase || 'first-launch'}-diagnostics.json`;
  assert(diagnostics.ended, `IDE Native process exceeded its bound; see ${file}`);
  assert.equal(diagnostics.exitCode, 0, `IDE Native process failed; see ${file}`);
  assert.equal(diagnostics.signal, null, `IDE Native process was signalled; see ${file}`);
  assert(diagnostics.nativeReportExists,
    `IDE application exited without a renderer report (PID ${diagnostics.pid}, exit 0, ${diagnostics.elapsedMs}ms). `
    + `No functional result was verified. Check the embedded-page/custom-protocol build and scope rejection in launch logs; see ${file}. An exit code alone is not acceptance.`);
}

export async function runNativeIdeGitReview({ executable, resourceOnly = false, crashSignal = null } = {}) {
  assert.equal(process.platform, 'darwin');
  assert([null, 'SIGTERM', 'SIGKILL'].includes(crashSignal));
  if (!resourceOnly) { assertNativeReviewSessionUnlocked(); assert(executable); executable = realpathSync(resolve(executable)); assert(lstatSync(executable).isFile()); }
  const output = nativeReviewOutput('native-ide-git', 'NATIVE_IDE_GIT_REVIEW_OUTPUT');
  const home = realpathSync(mkdtempSync(join(tmpdir(), 'echo-native-review-ide-git-'))); chmodSync(home, 0o700);
  const cwd = join(home, 'workspaces', 'default'), legacy = join(home, 'empty-legacy');
  mkdirSync(cwd, { recursive: true, mode: 0o700 }); mkdirSync(legacy, { mode: 0o700 });
  // Browser Theia defaults to afterDelay (1000ms), which correctly removes
  // dirty state during an expensive independent process/hash observation.
  // Exercise the explicit user-supported unsaved configuration instead.
  const preferencesDirectory = join(home, 'theia', 'theia-config');
  mkdirSync(preferencesDirectory, { recursive: true, mode: 0o700 });
  writeFileSync(join(preferencesDirectory, 'settings.json'), JSON.stringify({ 'files.autoSave': 'off' }), { flag: 'wx', mode: 0o600 });
  const nonce = randomBytes(16).toString('hex'), generation = randomBytes(16).toString('hex');
  const file = 'IDE边界验收.txt', version = index => `IDE_CONTENT_${generation}\nEcho IDE boundary version ${index}\n`;
  const terminal = `'use strict';\nconst fs = require('node:fs');\nconst proof = { marker: ${JSON.stringify(`IDE_TERMINAL_${generation}`)}, cwd: process.cwd(), pid: process.pid };\nfs.writeFileSync('.ide-terminal-proof.json', JSON.stringify(proof));\nconsole.log(proof.marker);\n`;
  const ignore = '.theia/\n.echoagent/\n.echo-agent/\n.native-ide-*\n.ide-terminal-proof.json\n';
  const synthetic = { [file]: version(1), 'ide-terminal-proof.cjs': terminal, '.gitignore': ignore };
  for (const [name, content] of Object.entries(synthetic)) writeFileSync(join(cwd, name), content, { flag: 'wx', mode: 0o600 });
  const result = { kind: 'real-native-ide-git-driver', home, cwd, output, nativeVerified: false,
    passed: false, errors: [], modelUsed: false, globalGitWritten: false, repositoryPushed: false, resourceOnly,
    isolatedUserPreferences: { 'files.autoSave': 'off' } };
  let active, server, gitAudit, previewRequests = 0, crashOwned = [];
  if (crashSignal) result.kind = 'real-native-ide-owner-crash';
  const table = () => execFileSync('/bin/ps', ['-axo', 'pid=,ppid=,command='], { encoding: 'utf8', maxBuffer: 4 * 1024 * 1024 });
  const exitedPid = pid => {
    try { return processHasExited(0, execFileSync('/bin/ps', ['-p', String(pid), '-o', 'stat='], { encoding: 'utf8' })); }
    catch (error) { return processHasExited(error.status, ''); }
  };
  const ownedTheiaTree = () => {
    if (!active?.pid) return [];
    const snapshot = table(), candidates = [];
    for (const root of [resolve(dirname(executable), '../Resources/theia'), resolve('src-tauri/resources/theia')]) {
      if (!existsSync(join(root, 'node/bin/node')) || !existsSync(join(root, 'browser/lib/backend/main.js'))) continue;
      try { candidates.push(selectOwnedTheiaBackend(snapshot, active.pid, realpathSync(join(root, 'node/bin/node')), realpathSync(join(root, 'browser/lib/backend/main.js')))); }
      catch (error) { if (!String(error).includes('0 !== 1')) throw error; }
    }
    const roots = [...new Set(candidates)]; assert(roots.length <= 1, 'Ambiguous owned backend');
    const rows = snapshot.split('\n').map(line => /^\s*(\d+)\s+(\d+)\s+/.exec(line)).filter(Boolean);
    const owned = new Set(roots);
    for (let added = true; added;) {
      added = false;
      for (const row of rows) if (owned.has(Number(row[2])) && !owned.has(Number(row[1]))) { owned.add(Number(row[1])); added = true; }
    }
    return [...owned];
  };
  const env = nativeGitProcessEnvironment();
  const git = async args => {
    const reply = await runNativeGitCommand('git', args, { cwd, env });
    assert.equal(reply.exitCode, 0, `Owned Git command failed: ${reply.stderr.slice(-1200)}`);
    assert(!reply.timedOut && !reply.overflow); return reply.stdout;
  };
  const physical = path => {
    const stat = lstatSync(path); assert(stat.isFile() && !stat.isSymbolicLink()); assert.equal(realpathSync(path), path);
    assert(stat.size < 128 * 1024); return readFileSync(path);
  };
  try {
    // Empty init template prevents copying external executables; inspect actual
    // system/global/local configuration before any checkout, stage or commit.
    await git(['init', '--quiet', '--template=', '--initial-branch=review']); chmodSync(join(cwd, '.git'), 0o700);
    gitAudit = await auditNativeGitConfiguration('file:///private/unused-owned-ide-fixture', cwd, Object.keys(synthetic));
    assert(gitAudit.safe, 'Effective Git configuration needs a safe dedicated test resource');
    result.globalGitAudit = gitAudit;
    await git(['config', 'user.name', 'Echo IDE synthetic review']); await git(['config', 'user.email', 'review@example.invalid']);
    await git(['config', 'commit.gpgsign', 'false']);
    assert.equal((await git(['remote'])).trim(), '', 'Synthetic repository must not have an origin');
    await git(['add', '--', ...Object.keys(synthetic)]); await git(['commit', '--quiet', '-m', 'Private IDE baseline']);
    const initialHead = (await git(['rev-parse', 'HEAD'])).trim(); assert(/^(?:[a-f0-9]{40}|[a-f0-9]{64})$/.test(initialHead));
    writeFileSync(join(cwd, file), version(2), { mode: 0o600 });
    assert.deepEqual((await git(['diff', '--name-only', '-z'])).split('\0').filter(Boolean), [file]);
    const revocations = readonlyCompatibilitySourceRevocations();
    writeFileSync(join(home, 'config.toml'), `[marketplace]\nofficial_marketplace_auto_installed = true\ndisabled_sources = [${revocations.map(value => JSON.stringify(value)).join(', ')}]\n[claude_compat]\nimported = true\n`, { mode: 0o600 });
    result.compatibilityRevocationCount = revocations.length;
    const baselineFiles = Object.fromEntries(['ide-terminal-proof.cjs', '.gitignore'].map(name => [name, digest(physical(join(cwd, name)))]));
    if (resourceOnly) { result.resourcesPrepared = true; result.initialHead = initialHead; return result; }
    // The production readiness gate requires an explicit model connection even
    // though this Git/editor journey does not send a model prompt.
    result.configuredModelId = seedReviewModel(home);
    server = createServer((request, response) => {
      if (request.method !== 'GET' || request.url !== `/ide-preview/${generation}`) { response.writeHead(404); response.end(); return; }
      previewRequests++; response.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8', 'Cache-Control': 'no-store' });
      response.end(`<html lang="zh-CN"><title>私有IDE预览</title><h1>IDE_PREVIEW_${generation}</h1></html>`);
    });
    await new Promise((accept, reject) => { server.once('error', reject); server.listen(0, '127.0.0.1', accept); });
    const manifest = { schema: 1, kind: 'ide-git-boundary', home, cwd, nonce, generation, phase: 'first-launch', expected: null,
      file, initialHead, previewUrl: `http://127.0.0.1:${server.address().port}/ide-preview/${generation}`,
      version1: version(1), version2: version(2), version3: version(3), commitMessage: 'Private IDE fixture change', baselineFiles };
    writeFileSync(join(home, 'native-ide-manifest.private.json'), JSON.stringify(manifest), { mode: 0o600 });
    const nativeEnv = nativeIdeReviewEnvironment(env, legacy);
    const child = spawn(executable, ['--data-home', home], { cwd, env: nativeEnv, stdio: ['ignore', 'pipe', 'pipe'] });
    active = child; let ended = false, exitCode, signal, bytes = 0, stageHandled = false, commitHandled = false, disconnectHandled = false, barrierFailure;
    const exited = new Promise((accept, reject) => { child.once('error', reject); child.once('close', (code, sig) => { ended = true; exitCode = code; signal = sig; accept(); }); });
    const log = chunk => { bytes += chunk.length; if (bytes > 8 * 1024 * 1024) { child.kill('SIGTERM'); return; }
      writeFileSync(join(output, 'first-launch-app.log'), chunk, { flag: 'a', mode: 0o600 }); };
    child.stdout.on('data', log); child.stderr.on('data', log);
    const activateOwnedWindow = async (stage, ownedChild = child) => {
      // macOS can suspend a completely occluded WKWebView's frame callbacks.
      // Activate only the application PID this runner spawned, without AX,
      // screenshots, keyboard input, or selecting an app by its display name.
      let activated = false;
      for (let attempt = 0; attempt < 20 && ownedChild.exitCode === null && ownedChild.signalCode === null; attempt++) {
        try {
          activated = activateNativeReviewWindow(ownedChild.pid);
          if (activated) break;
        } catch { /* Keep every functional assertion even if activation fails. */ }
        await delay(250);
      }
      (result.windowActivation ??= []).push({ stage, pid: ownedChild.pid, activated });
    };
    const start = Date.now();
    await activateOwnedWindow('start');
    let lastActivation = Date.now();
    while (!ended && Date.now() - start < 660000) {
      try {
        const partialPath = join(home, 'native-review.json');
        if (existsSync(partialPath)) {
          const partial = JSON.parse(physical(partialPath));
          if (partial.passed === false) {
            writeFileSync(join(output, 'first-launch.json'), physical(partialPath), { mode: 0o600 });
            throw new Error(`Actual IDE renderer failed: ${JSON.stringify(partial.errors)}`);
          }
        }
        // Startup can take longer than the initial five-second activation
        // window. Retry only this child PID while awaiting the first handoff.
        if (!stageHandled && Date.now() - lastActivation >= 5000) {
          await activateOwnedWindow('await-first-handoff'); lastActivation = Date.now();
        }
        for (const phase of ['stage', 'commit', 'disconnect']) {
          if ((phase === 'stage' && stageHandled) || (phase === 'commit' && commitHandled) || (phase === 'disconnect' && disconnectHandled)) continue;
          const path = join(cwd, `.native-ide-${phase}-ready.json`); if (!existsSync(path)) continue;
          const handoff = JSON.parse(physical(path)); assert.deepEqual(handoff, { nonce, generation, stage: phase });
          let proof;
          if (phase === 'stage') {
            assert.deepEqual((await git(['diff', '--cached', '--name-only', '-z'])).split('\0').filter(Boolean), [file]);
            assert.equal(await git(['show', `:${file}`]), version(2));
            proof = { nonce, generation, stage: phase, passed: true, indexPaths: [file], indexSha256: digest(version(2)) };
            stageHandled = true;
            if (crashSignal) {
              crashOwned = ownedTheiaTree(); assert(crashOwned.length > 0, 'No real backend to crash-test');
              const evidence = { signal: crashSignal, applicationPid: child.pid, backendPid: crashOwned[0], ownedPids: crashOwned, actualNative: true };
              const packagedGuard = resolve(dirname(executable), '../Resources/theia/parent-guard.cjs');
              if (existsSync(packagedGuard)) {
                evidence.guardSha256 = digest(readFileSync(packagedGuard));
                assert.equal(evidence.guardSha256, digest(readFileSync(resolve('src-tauri/theia-parent-guard.cjs'))));
              }
              writeFileSync(join(output, 'crash-before.json'), JSON.stringify(evidence, null, 2), { mode: 0o600 });
              assert(child.kill(crashSignal));
              await Promise.race([exited, delay(5000)]); assert(ended && signal === crashSignal, 'Application did not receive the exact crash signal');
              const deadline = Date.now() + 12000;
              while (Date.now() < deadline && crashOwned.some(pid => !exitedPid(pid))) await delay(100);
              evidence.remainingPids = crashOwned.filter(pid => !exitedPid(pid));
              writeFileSync(join(output, 'crash-after.json'), JSON.stringify(evidence, null, 2), { mode: 0o600 });
              assert.deepEqual(evidence.remainingPids, [], 'Owned backend survived the application crash');
              result.crash = evidence; result.nativeVerified = true; result.passed = true;
              return result;
            }
          } else if (phase === 'commit') {
            const head = (await git(['rev-parse', 'HEAD'])).trim(); assert.notEqual(head, initialHead);
            assert.equal((await git(['rev-list', '--count', `${initialHead}..HEAD`])).trim(), '1');
            assert.equal((await git(['log', '-1', '--format=%s'])).trim(), manifest.commitMessage);
            assert.equal(await git(['show', `HEAD:${file}`]), version(2));
            assert.deepEqual((await git(['diff', '--cached', '--name-only', '-z'])).split('\0').filter(Boolean), []);
            proof = { nonce, generation, stage: phase, passed: true, head, oneCommit: true, committedSha256: digest(version(2)) };
            commitHandled = true;
          } else {
            assert(stageHandled && commitHandled && !ended, 'Disconnect must follow the actual completed Git journey');
            assert.equal(physical(join(cwd, file)).toString('utf8'), version(3));
            const resources = resolve(dirname(executable), '../Resources/theia');
            const staged = resolve('src-tauri/resources/theia');
            const runtimeFiles = ['node/bin/node', 'browser/lib/backend/main.js', 'browser/lib/frontend/bundle.js', 'browser/packages/plugin-ext/lib/plugin/scm.js'];
            const runtimeProof = runtimeFiles.map(name => {
              const stagedFile = join(staged, name), bundledFile = join(resources, name);
              assert(lstatSync(stagedFile).isFile() && lstatSync(bundledFile).isFile());
              const stagedHash = digest(readFileSync(stagedFile)), bundledHash = digest(readFileSync(bundledFile));
              assert.equal(stagedHash, bundledHash, 'Debug staged Theia must match the frozen packaged resource');
              return { name, sha256: stagedHash };
            });
            const table = () => execFileSync('/bin/ps', ['-axo', 'pid=,ppid=,command='], { encoding: 'utf8', maxBuffer: 4 * 1024 * 1024 });
            // Theia intentionally prefers fixed staged assets in debug builds.
            // Permit only that known byte-identical location or this package.
            const currentTable = table(), candidates = [];
            for (const [source, root] of [['bundled', resources], ['debug-staged', staged]]) {
              const nodePath = realpathSync(join(root, 'node/bin/node')), mainPath = realpathSync(join(root, 'browser/lib/backend/main.js'));
              try { candidates.push({ source, nodePath, mainPath, pid: selectOwnedTheiaBackend(currentTable, child.pid, nodePath, mainPath) }); }
              catch (error) { if (!String(error).includes('0 !== 1')) throw error; }
            }
            assert.equal(candidates.length, 1, 'Expected one owned backend across the two fixed byte-identical asset locations');
            const { source: runtimeSource, nodePath, mainPath, pid: backendPid } = candidates[0];
            assert.equal(selectOwnedTheiaBackend(table(), child.pid, nodePath, mainPath), backendPid);
            process.kill(backendPid, 'SIGTERM');
            let stopped = false;
            for (let i = 0; i < 100; i++) {
              let state, status = 0;
              try { state = execFileSync('/bin/ps', ['-p', String(backendPid), '-o', 'stat='], { encoding: 'utf8', maxBuffer: 8192 }); }
              catch (error) { status = error.status; state = ''; }
              // A terminated child can remain a zombie until Rust reaps it on
              // reconnect. It has already stopped serving or executing code.
              if (processHasExited(status, state)) { stopped = true; break; } await delay(100);
            }
            assert(stopped, 'Owned backend did not terminate');
            proof = { nonce, generation, stage: phase, passed: true, processStopped: true, ownedParentPid: child.pid, backendPid, physicalVersion: 3, runtimeSource, runtimeProof };
            disconnectHandled = true;
            await activateOwnedWindow('backend-recovery');
          }
          writeFileSync(join(cwd, `.native-ide-${phase}-proof.json`), JSON.stringify(proof), { flag: 'wx', mode: 0o600 });
          writeFileSync(join(output, `${phase}-physical-proof.json`), JSON.stringify(proof, null, 2), { mode: 0o600 });
        }
      } catch (error) { barrierFailure = error; break; }
      await delay(150);
    }
    result.launch = collectIdeLaunchDiagnostics({ home, cwd, pid: child.pid, executable, startedAt: start, ended, exitCode, signal });
    writeFileSync(join(output, 'first-launch-diagnostics.json'), JSON.stringify(result.launch, null, 2), { mode: 0o600 });
    if (barrierFailure) throw barrierFailure;
    requireIdeRendererReport(result.launch); await exited;
    const raw = join(home, 'native-review.json');
    // Validate an ordinary owned file before copying any bytes into evidence.
    writeFileSync(join(output, 'first-launch.json'), physical(raw), { mode: 0o600 });
    const report = JSON.parse(physical(raw)), expected = verifyIdeReport(report, manifest, child.pid, start);
    assert(stageHandled && commitHandled && disconnectHandled); assert(previewRequests > 0, 'Actual preview made no HTTP request');
    assert.equal(physical(join(cwd, file)).toString('utf8'), version(3));
    for (const [name, hash] of Object.entries(baselineFiles)) assert.equal(digest(physical(join(cwd, name))), hash);
    const term = JSON.parse(physical(join(cwd, '.ide-terminal-proof.json')));
    assert.equal(term.marker, `IDE_TERMINAL_${generation}`); assert.equal(term.cwd, cwd); assert(term.pid > 0);
    assert.equal((await git(['rev-parse', 'HEAD'])).trim(), expected.committedHead);
    const worktreeRoot = expected.worktreeRoot;
    assert(lstatSync(worktreeRoot).isDirectory() && !lstatSync(worktreeRoot).isSymbolicLink()); assert.equal(realpathSync(worktreeRoot), worktreeRoot);
    assert.equal(physical(join(worktreeRoot, file)).toString('utf8'), version(1));
    const worktreeHead = await runNativeGitCommand('git', ['rev-parse', 'HEAD'], { cwd: worktreeRoot, env });
    assert.equal(worktreeHead.exitCode, 0); assert.equal(worktreeHead.stdout.trim(), expected.committedHead);
    assert.equal(physical(join(cwd, file)).toString('utf8'), version(3));
    result.worktreePhysical = { root: worktreeRoot, head: worktreeHead.stdout.trim(), savedSha256: digest(version(1)), originalSourceSha256: digest(version(3)), originalHeadUnchanged: true };
    assert(/^[a-f0-9-]{36}$/.test(expected.originalTaskId));
    const taskPath = join(home, 'coding', digest(cwd).slice(0, 16), expected.originalTaskId, 'task.json');
    const savedTask = JSON.parse(physical(taskPath));
    assert.equal(savedTask.id, expected.originalTaskId); assert.equal(savedTask.phase, 'discovering');
    assert.equal(savedTask.name, `隔离并行夹具-${generation}`); assert(!savedTask.sessionId);
    result.taskPhysical = { id: savedTask.id, phase: savedTask.phase, sha256: digest(physical(taskPath)), syntheticSetupWithoutModel: true };
    assert.deepEqual((await git(['diff', '--name-only', '-z'])).split('\0').filter(Boolean), [file]); assert.equal((await git(['remote'])).trim(), '');
    result.expected = expected; result.pid = child.pid; result.exitCode = exitCode; result.signal = signal;
    result.previewRequests = previewRequests; result.terminal = term;
    result.phases = [{ phase: 'first-launch', pid: child.pid, checks: checks.length, passed: true }];
    // A successful first launch is insufficient: the compiled restart factory
    // must reopen the exact prior worktree and source task in a new process.
    const restartManifest = ideRestartManifest(manifest, expected);
    const historyPath = join(home, 'native-review-history.jsonl'), historyBefore = physical(historyPath).toString('utf8');
    verifyIdeReportHistory(historyBefore, [report]);
    const worktreeGit = async args => {
      const reply = await runNativeGitCommand('git', args, { cwd: worktreeRoot, env });
      assert.equal(reply.exitCode, 0, `Owned worktree Git command failed: ${reply.stderr.slice(-1200)}`);
      assert(!reply.timedOut && !reply.overflow); return reply.stdout;
    };
    const snapshot = async () => {
      const taskRaw = physical(taskPath), task = JSON.parse(taskRaw);
      return {
        sourceRoot: cwd, worktreeRoot,
        sourceHead: (await git(['rev-parse', 'HEAD'])).trim(), worktreeHead: (await worktreeGit(['rev-parse', 'HEAD'])).trim(),
        sourceHistorySha256: digest(await git(['log', '--all', '--format=%H%x00%P%x00%s', '--no-show-signature'])),
        worktreeHistorySha256: digest(await worktreeGit(['log', '--all', '--format=%H%x00%P%x00%s', '--no-show-signature'])),
        commitsSinceBaseline: (await git(['rev-list', '--count', `${initialHead}..HEAD`])).trim(),
        sourceSha256: digest(physical(join(cwd, file))), worktreeSha256: digest(physical(join(worktreeRoot, file))),
        sourceDiffPaths: (await git(['diff', '--name-only', '-z'])).split('\0').filter(Boolean),
        stagedPaths: (await git(['diff', '--cached', '--name-only', '-z'])).split('\0').filter(Boolean),
        remotes: (await git(['remote'])).trim(),
        baselineFiles: Object.fromEntries(Object.keys(baselineFiles).map(name => [name, digest(physical(join(cwd, name)))])),
        terminalSha256: digest(physical(join(cwd, '.ide-terminal-proof.json'))),
        task: { id: task.id, name: task.name, phase: task.phase, sessionId: task.sessionId ?? null, sha256: digest(taskRaw) },
      };
    };
    const beforeRestart = await snapshot();
    verifyIdeRestartPhysical(beforeRestart, beforeRestart, restartManifest);
    writeFileSync(join(output, 'restart-before-physical-proof.json'), JSON.stringify(beforeRestart, null, 2), { mode: 0o600 });
    const manifestPath = join(home, 'native-ide-manifest.private.json'); physical(manifestPath);
    writeFileSync(manifestPath, JSON.stringify(restartManifest), { mode: 0o600 });
    physical(raw); unlinkSync(raw); // A stale first-launch report must never admit restart.
    const restartStartedAt = Date.now();
    const restarted = spawn(executable, ['--data-home', home], { cwd,
      env: nativeIdeReviewEnvironment(env, legacy, 'restart'), stdio: ['ignore', 'pipe', 'pipe'] });
    active = restarted;
    let restartEnded = false, restartExitCode, restartSignal, restartBytes = 0, restartSpawnError;
    const restartExited = new Promise(accept => {
      restarted.once('error', error => { restartSpawnError = error; restartEnded = true; accept(); });
      restarted.once('close', (code, sig) => { restartEnded = true; restartExitCode = code; restartSignal = sig; accept(); });
    });
    const restartLog = chunk => {
      restartBytes += chunk.length; if (restartBytes > 8 * 1024 * 1024) { restarted.kill('SIGTERM'); return; }
      writeFileSync(join(output, 'restart-app.log'), chunk, { flag: 'a', mode: 0o600 });
    };
    restarted.stdout.on('data', restartLog); restarted.stderr.on('data', restartLog);
    assert.notEqual(restarted.pid, child.pid, 'Restart requires a new owned native process');
    await activateOwnedWindow('restart-start', restarted); lastActivation = Date.now();
    while (!restartEnded && Date.now() - restartStartedAt < 660000) {
      if (Date.now() - lastActivation >= 5000) {
        await activateOwnedWindow('restart-await-report', restarted); lastActivation = Date.now();
      }
      await delay(150);
    }
    result.restartLaunch = collectIdeLaunchDiagnostics({ home, cwd, pid: restarted.pid, executable,
      startedAt: restartStartedAt, ended: restartEnded, exitCode: restartExitCode, signal: restartSignal, phase: 'restart' });
    writeFileSync(join(output, 'restart-diagnostics.json'), JSON.stringify(result.restartLaunch, null, 2), { mode: 0o600 });
    if (restartSpawnError) throw restartSpawnError;
    requireIdeRendererReport(result.restartLaunch); await restartExited;
    const restartBytesRaw = physical(raw); writeFileSync(join(output, 'restart.json'), restartBytesRaw, { mode: 0o600 });
    const restartReport = JSON.parse(restartBytesRaw);
    verifyIdeRestartReport(restartReport, restartManifest, restarted.pid, restartStartedAt);
    const restartPhysical = verifyIdeRestartPhysical(beforeRestart, await snapshot(), restartManifest);
    restartPhysical.nativeHistory = verifyIdeReportHistory(physical(historyPath).toString('utf8'), [report, restartReport], historyBefore);
    writeFileSync(join(output, 'restart-physical-proof.json'), JSON.stringify(restartPhysical, null, 2), { mode: 0o600 });
    result.restartPhysical = restartPhysical;
    result.phases.push({ phase: 'restart', pid: restarted.pid, checks: restartReport.checks.length, passed: true });
    assert.deepEqual(result.phases.map(phase => [phase.phase, phase.passed, phase.checks]), [['first-launch', true, 17], ['restart', true, 4]]);
    result.nativeVerified = true; result.passed = true;
  } catch (error) { result.errors.push(String(error?.message || error).slice(0, 2500)); }
  finally {
    // Failure cleanup is separate from the crash assertion and cannot make a
    // failed crash test green. Capture only the exact direct backend child.
    const cleanup = [...new Set([...crashOwned, ...ownedTheiaTree()])];
    if (active && active.exitCode === null && active.signalCode === null) { active.kill('SIGTERM'); await delay(2000); if (active.exitCode === null && active.signalCode === null) active.kill('SIGKILL'); }
    for (const pid of cleanup) if (!exitedPid(pid)) { try { process.kill(pid, 'SIGTERM'); } catch (error) { if (error.code !== 'ESRCH') throw error; } }
    if (cleanup.length) await delay(1000);
    for (const pid of cleanup) if (!exitedPid(pid)) { try { process.kill(pid, 'SIGKILL'); } catch (error) { if (error.code !== 'ESRCH') throw error; } }
    if (server) { await new Promise(accept => { server.close(accept); server.closeAllConnections(); }); }
    result.serverStopped = !server?.listening; result.finishedAt = new Date().toISOString();
    writeFileSync(join(output, 'result.json'), JSON.stringify(result, null, 2), { mode: 0o600 });
  }
  return result;
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  const args = process.argv.slice(2); assert(args.length === 1 || args.length === 2 && /^--crash=SIG(?:TERM|KILL)$/.test(args[1]));
  const result = await runNativeIdeGitReview(args[0] === '--resource-only' ? { resourceOnly: true } : { executable: args[0], crashSignal: args[1]?.slice('--crash='.length) });
  process.stdout.write(JSON.stringify({ passed: result.passed, resourcesPrepared: !!result.resourcesPrepared, nativeVerified: result.nativeVerified,
    output: result.output, errors: result.errors }) + '\n');
  if (!result.passed && !result.resourcesPrepared) process.exitCode = 1;
}
