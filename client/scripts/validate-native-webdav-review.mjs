import { cleanNativeReviewEnvironment } from "./lib/native-review-isolation.mjs";
import { nativeReviewOutput } from './lib/native-review-output.mjs';
// Dedicated actual macOS CloudStorage UI acceptance. No Cargo, staging, model,
// existing profile, OS login mutation, or transfer-dialog bypass is performed.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import { existsSync, readFileSync, writeFileSync, mkdirSync, mkdtempSync, realpathSync,
  lstatSync, statSync, unlinkSync, copyFileSync, createReadStream } from 'node:fs';
import { resolve, join, relative, isAbsolute, basename, dirname } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { setTimeout as delay } from 'node:timers/promises';

export const REQUIRED_WEBDAV_CHECKS = Object.freeze({
  'first-launch': ['native-webdav-isolation', 'native-webdav-settings-entry', 'native-webdav-config-create',
    'native-webdav-connection-list', 'native-webdav-create-edit-read', 'native-webdav-real-delete',
    'native-webdav-readonly-failure-draft', 'native-webdav-settings-leave-cancel', 'native-webdav-retry-save',
    'native-webdav-disabled-recovery', 'native-webdav-return-shell'],
  restart: ['native-webdav-isolation', 'native-webdav-settings-entry', 'native-webdav-provider-restore',
    'native-webdav-file-restore', 'native-webdav-revoke-preserves-file', 'native-webdav-return-shell'],
});
const within = (root, path) => {
  const inside = relative(root, path);
  return inside !== '' && inside !== '..' && !inside.startsWith('../') && !isAbsolute(inside);
};
const hash = (bytes) => createHash('sha256').update(bytes).digest('hex');
const ordinary = (path, root) => {
  assert(within(root, path), 'Evidence escaped its owned temporary scope');
  assert(lstatSync(path).isFile() && !lstatSync(path).isSymbolicLink(), 'Expected an ordinary scoped evidence file');
  assert.equal(realpathSync(path), path, 'Evidence path is not canonical');
  return readFileSync(path);
};
const noSecrets = (value, scope) => {
  const text = typeof value === 'string' ? value : JSON.stringify(value);
  for (const secret of [scope.password, scope.username]) assert(!text.includes(secret), 'Refusing to persist an unredacted native report');
};

export function verifyNativeWebDavReport({ report, phase, home, pid, startedAt, previousExpected }) {
  assert.equal(report?.version, 1);
  assert.equal(report.kind, 'webdav', 'A generic or fixture report cannot satisfy this native UI gate');
  assert.equal(report.phase, phase);
  assert.equal(report.nativeContext?.phase, phase);
  assert.equal(report.nativeContext?.home, home);
  assert.equal(report.nativeContext?.pid, pid, 'Native report belongs to another process');
  assert.equal(report.nativeContext?.webdavRequested, true, 'Dedicated native dispatch was not enabled');
  assert.equal(report.nativeContext?.evaluations, 1, 'Duplicate ready evaluations cannot overwrite failed acceptance');
  const finishedAt = Date.parse(report.finishedAt);
  assert(Number.isFinite(finishedAt) && finishedAt >= startedAt && finishedAt <= Date.now() + 1000, 'Stale or invalid native report');
  assert.equal(report.passed, true);
  assert(Array.isArray(report.errors) && report.errors.length === 0);
  assert(Array.isArray(report.checks) && report.checks.length === REQUIRED_WEBDAV_CHECKS[phase].length);
  for (const id of REQUIRED_WEBDAV_CHECKS[phase]) {
    const checks = report.checks.filter((check) => check.id === id);
    assert.equal(checks.length, 1, `Missing or duplicate actual native check: ${id}`);
    assert.equal(checks[0].result, 'passed', `Native check did not pass: ${id}`);
  }
  assert.equal(report.modelRequested, false);
  assert.equal(report.uploadDownloadDialogVerified, false, 'This driver cannot claim OS transfer-dialog acceptance');
  assert.equal(report.expected?.version, 1);
  assert.equal(report.expected.home, home);
  assert(/^[a-f0-9]{32}$/.test(report.expected.nonce || ''));
  for (const id of [report.expected.rwId, report.expected.recoveryId]) assert(/^webdav-\d{10,}$/.test(id || ''), 'Invalid UI-created provider ID');
  assert.notEqual(report.expected.rwId, report.expected.recoveryId);
  assert.equal(report.expected.directory, `原生云验收-${report.expected.nonce}`);
  assert.equal(report.expected.fileName, '中文 空格.md');
  assert.equal(report.expected.recoveryName, '失败恢复 中文.md');
  assert.equal(report.expected.deletedName, '仅删合成 临时.txt');
  if (phase === 'restart') assert.deepEqual(report.expected, previousExpected, 'Restart replaced first-launch native expectations');
  return report.expected;
}

function validateScope(scope, home) {
  assert.equal(scope.schema, 1);
  assert.equal(scope.home, home);
  assert.equal(scope.readOnlySharesPhysicalRoot, true);
  assert(/^[a-f0-9]{32}$/.test(scope.nonce || ''));
  assert(/^\/private\/tmp\/echo-webdav-real-[^/]+\/physical-root$/.test(scope.root));
  assert(lstatSync(scope.root).isDirectory() && !lstatSync(scope.root).isSymbolicLink());
  assert.equal(realpathSync(scope.root), scope.root);
  assert.equal(realpathSync(join(scope.root, 'rw')), join(scope.root, 'rw'));
  assert.equal(realpathSync(join(scope.root, 'ro')), join(scope.root, 'ro'));
  const urls = [new URL(scope.readWriteUrl), new URL(scope.readOnlyUrl)];
  for (const url of urls) assert(url.protocol === 'http:' && url.hostname === '127.0.0.1' && url.port
    && !url.username && !url.password && !url.search && !url.hash);
  assert.equal(urls[0].origin, urls[1].origin);
  assert.equal(urls[0].pathname, '/rw');
  assert.equal(urls[1].pathname, '/ro');
  assert.equal(scope.seedName, '原始 中文.txt');
  assert(typeof scope.seedContent === 'string' && scope.seedContent.length > 0);
  assert(typeof scope.username === 'string' && scope.username.length > 0);
  assert(typeof scope.password === 'string' && scope.password.length >= 20);
  assert.equal(scope.requestAudit, join(dirname(scope.root), 'requests.redacted.jsonl'));
}

function verifyFilesystem({ report, scope, phase, firstFilesystem }) {
  const expected = report.expected;
  assert.equal(expected.root, scope.root);
  assert.equal(expected.nonce, scope.nonce);
  assert.equal(expected.readWriteUrl, scope.readWriteUrl);
  assert.equal(expected.readOnlyUrl, scope.readOnlyUrl);
  const directory = join(scope.root, 'rw', expected.directory);
  assert(lstatSync(directory).isDirectory() && !lstatSync(directory).isSymbolicLink());
  assert.equal(realpathSync(directory), directory);
  const files = [];
  for (const [name, content] of [[expected.fileName, expected.editedContent], [expected.recoveryName, expected.recoveryContent]]) {
    const file = join(directory, name);
    const bytes = ordinary(file, scope.root);
    assert.equal(bytes.toString('utf8'), content, 'Actual physical WebDAV file differs from native UI/IPC result');
    files.push({ path: file, size: bytes.length, sha256: hash(bytes) });
  }
  assert(!existsSync(join(directory, expected.deletedName)), 'UI scratch deletion did not delete the physical file');
  for (const share of ['rw', 'ro']) assert.equal(ordinary(join(scope.root, share, scope.seedName), scope.root).toString('utf8'), scope.seedContent,
    'Original synthetic seed changed');
  const configPath = join(scope.home, 'echoagent-storage-providers.json');
  const providers = JSON.parse(ordinary(configPath, scope.home)).providers;
  assert(Array.isArray(providers));
  if (phase === 'first-launch') {
    assert.equal(providers.length, 2);
    for (const [id, label] of [[expected.rwId, expected.providerLabels.rw], [expected.recoveryId, expected.providerLabels.recovery]]) {
      const matches = providers.filter((provider) => provider.id === id);
      assert.equal(matches.length, 1);
      assert.equal(matches[0].label, label);
      assert.equal(matches[0].baseUrl, scope.readWriteUrl);
      assert.equal(matches[0].enabled, true);
      assert.equal(matches[0].username, scope.username);
      assert.equal(matches[0].password, scope.password, 'Masked UI config save lost the real synthetic credential');
    }
  } else {
    assert.equal(providers.length, 0, 'Actual UI revocation did not persist');
    assert.deepEqual(files, firstFilesystem.files, 'Restart/revocation changed the physical saved files');
  }
  const ledger = ordinary(scope.requestAudit, dirname(scope.root)).toString('utf8').trim().split('\n').filter(Boolean).map((line) => JSON.parse(line));
  const puts = (share, name) => ledger.filter((entry) => entry.method === 'PUT'
    && entry.path === `/${share}/${expected.directory}/${name}`);
  assert.deepEqual(puts('rw', expected.fileName).map((entry) => entry.status), [201, 204], 'Text create/edit was duplicated or did not execute');
  assert.deepEqual(puts('ro', expected.recoveryName).map((entry) => entry.status), [403], 'Readonly failure was not a real unique standard-server PUT');
  assert.deepEqual(puts('rw', expected.recoveryName).map((entry) => entry.status), [201, 204], 'Recovery draft retry was duplicated or never saved');
  assert.equal(ledger.filter((entry) => entry.method === 'DELETE' && entry.path === `/rw/${expected.directory}/${expected.deletedName}` && entry.status === 204).length, 1);
  return { files, originalSeedsPreserved: true, ownScratchDeleted: true, providerCount: providers.length,
    readonlyPutStatus: 403, successfulRetryWrites: 1, requestAudit: scope.requestAudit,
    requestAuditSha256: hash(ordinary(scope.requestAudit, dirname(scope.root))), transferDialogVerified: false };
}

async function waitForFile(path, processState, timeout) {
  const deadline = Date.now() + timeout;
  while (!existsSync(path) && !processState.ended && Date.now() < deadline) await delay(100);
  assert(existsSync(path), 'Owned native/service process did not produce current evidence before timeout');
}

async function terminate(state) {
  if (!state || state.ended) return;
  state.child.kill('SIGTERM');
  await Promise.race([state.exited, delay(5000, undefined, { ref: false })]);
  if (!state.ended) {
    state.child.kill('SIGKILL');
    await Promise.race([state.exited, delay(3000, undefined, { ref: false })]);
  }
}

function managed(child) {
  const state = { child, ended: false, error: null };
  state.exited = new Promise((accept) => {
    child.once('exit', () => { state.ended = true; accept(); });
    child.once('error', (error) => { state.error = error; state.ended = true; accept(); });
  });
  return state;
}

export async function runNativeWebDavReview({ executable, resourceOnly = false } = {}) {
  assert.equal(process.platform, 'darwin', 'This native driver targets the actual macOS WKWebView');
  if (!resourceOnly) {
    assert(executable, 'Pass the final built executable with the dedicated fixed WebDAV factory included');
    executable = realpathSync(resolve(executable));
    assert(lstatSync(executable).isFile(), 'Expected an ordinary built executable');
  }
  const outputRoot = nativeReviewOutput('native-webdav', 'NATIVE_WEBDAV_REVIEW_OUTPUT');
  mkdirSync(outputRoot, { recursive: true });
  const output = mkdtempSync(join(outputRoot, 'run-'));
  const home = realpathSync(mkdtempSync(join(tmpdir(), 'echo-native-review-')));
  assert(basename(home).startsWith('echo-native-review-') && statSync(home).mode % 0o1000 === 0o700);
  assert(within(realpathSync(tmpdir()), home) || within(realpathSync('/private/tmp'), home));
  const legacy = join(home, 'empty-legacy');
  mkdirSync(legacy, { mode: 0o700 });
  mkdirSync(join(home, 'workspaces', 'default'), { recursive: true, mode: 0o700 });
  const manifest = join(home, 'native-webdav-manifest.private.json');
  const serviceEvidence = join(output, 'standard-server.json');
  const result = { version: 1, environment: 'actual native CloudStorage UI acceptance', home, output,
    resourceOnly, nativeVerified: false, phases: [], errors: [], passed: false };
  const python = process.env.WEBDAV_REVIEW_PYTHON || 'python3';
  const serviceScript = fileURLToPath(new URL('./standard-webdav-review.py', import.meta.url));
  const serviceEnv = { ...cleanNativeReviewEnvironment(process.env) };
  if (process.env.WEBDAV_REVIEW_PYTHONPATH) serviceEnv.PYTHONPATH = process.env.WEBDAV_REVIEW_PYTHONPATH;
  const serviceLog = join(output, 'standard-server.log');
  const service = managed(spawn(python, [serviceScript, '--serve', '--home', home, '--manifest', manifest, '--output', serviceEvidence],
    { env: serviceEnv, stdio: ['ignore', 'pipe', 'pipe'] }));
  const serviceLines = [];
  service.child.stdout?.on('data', (data) => serviceLines.push(data.toString()));
  service.child.stderr?.on('data', (data) => serviceLines.push(data.toString()));
  let scope, active, interrupted = false, firstFilesystem, previousExpected;
  const interrupt = () => { interrupted = true; active?.child.kill('SIGTERM'); service.child.kill('SIGTERM'); };
  process.once('SIGINT', interrupt);
  process.once('SIGTERM', interrupt);
  try {
    await waitForFile(serviceEvidence, service, 30000);
    assert(!service.ended && !service.error, 'Standard WebDAV service exited during preparation');
    const resource = JSON.parse(readFileSync(serviceEvidence, 'utf8'));
    assert.equal(resource.passed, true);
    assert.equal(resource.live, true);
    assert.equal(resource.resourceChecks?.length, 9);
    assert(resource.resourceChecks.every((check) => check.passed === true));
    scope = JSON.parse(ordinary(manifest, home));
    assert.equal(statSync(manifest).mode & 0o077, 0, 'Manifest credentials must remain private');
    validateScope(scope, home);
    result.standardServer = { versions: resource.versions, root: scope.root,
      readWriteUrl: scope.readWriteUrl, readOnlyUrl: scope.readOnlyUrl, resourceChecks: resource.resourceChecks };
    if (resourceOnly) {
      result.environment = 'real standard WebDAV resource preparation; native UI not executed';
      result.passed = true;
    } else {
      const executableHash = createHash('sha256');
      for await (const chunk of createReadStream(executable)) executableHash.update(chunk);
      result.executable = executable;
      result.binarySha256 = executableHash.digest('hex');
      const launchEnv = { ...cleanNativeReviewEnvironment(process.env), GROK_HOME: legacy, ECHO_DESKTOP_REVIEW: '1',
        ECHO_DESKTOP_REVIEW_WEBDAV: '1', ECHO_DESKTOP_REVIEW_MEDIA: '0', ECHO_DESKTOP_REVIEW_EXTENDED: '0',
        ECHO_WEBDAV_REVIEW_MANIFEST: manifest, NO_PROXY: '127.0.0.1,localhost,::1', no_proxy: '127.0.0.1,localhost,::1' };
      for (const name of ['ECHO_AGENT_HOME', 'ECHO_DESKTOP_VALIDATION', 'ECHO_DESKTOP_REVIEW_CODE',
        'NATIVE_REVIEW_SEED_CONFIG', 'NATIVE_REVIEW_AUDIO']) delete launchEnv[name];
      const reportPath = join(home, 'native-review.json');
      const historyPath = join(home, 'native-review-history.jsonl');
      for (const phase of ['first-launch', 'restart']) {
        assert(!interrupted && !service.ended, 'Native acceptance interrupted or standard server stopped');
        if (existsSync(reportPath)) unlinkSync(reportPath);
        const historyCount = existsSync(historyPath) ? ordinary(historyPath, home).toString('utf8').trim().split('\n').filter(Boolean).length : 0;
        const startedAt = Date.now();
        active = managed(spawn(executable, ['--data-home', home], {
          env: { ...launchEnv, ECHO_DESKTOP_REVIEW_PHASE: phase }, stdio: ['ignore', 'pipe', 'pipe'] }));
        const appLines = [];
        active.child.stdout?.on('data', (data) => appLines.push(data.toString()));
        active.child.stderr?.on('data', (data) => appLines.push(data.toString()));
        let phasePassed = false;
        try {
          const finished = await Promise.race([active.exited.then(() => true), delay(240000, false, { ref: false })]);
          assert(finished, 'Native WebDAV phase did not finish within 240s');
          if (active.error) throw active.error;
          assert.equal(active.child.exitCode, 0, 'Native application did not exit normally');
          const report = JSON.parse(ordinary(reportPath, home));
          noSecrets(report, scope);
          copyFileSync(reportPath, join(output, `${phase}.json`));
          result.phases.push({ phase, exitCode: active.child.exitCode, report });
          const expected = verifyNativeWebDavReport({ report, phase, home, pid: active.child.pid, startedAt, previousExpected });
          const history = ordinary(historyPath, home).toString('utf8').trim().split('\n').filter(Boolean).map((line) => JSON.parse(line));
          assert.equal(history.length, historyCount + 1, 'Duplicate native evaluations obscured prior evidence');
          assert.deepEqual(history.at(-1), report, 'Native history does not match the report');
          const filesystem = verifyFilesystem({ report, scope, phase, firstFilesystem });
          writeFileSync(join(output, `${phase}-filesystem.json`), JSON.stringify(filesystem, null, 2));
          if (phase === 'first-launch') {
            firstFilesystem = filesystem;
            previousExpected = expected;
            scope.expected = expected;
            const temporary = join(home, '.native-webdav-manifest.part');
            writeFileSync(temporary, JSON.stringify(scope), { mode: 0o600, flag: 'wx' });
            // Private test expectation only, never a production provider/store write.
            const { renameSync } = await import('node:fs');
            renameSync(temporary, manifest);
          }
          phasePassed = true;
        } finally {
          await terminate(active);
          let log = appLines.join('');
          for (const secret of [scope.password, scope.username]) log = log.split(secret).join('[redacted]');
          writeFileSync(join(output, `${phase}.log`), log);
          active = undefined;
        }
        assert(phasePassed, 'A failed first launch cannot establish restart acceptance');
      }
      result.nativeVerified = true;
      result.passed = result.phases.length === 2;
    }
  } catch (error) {
    let message = String(error?.message || error);
    for (const secret of [scope?.password, scope?.username]) if (secret) message = message.split(secret).join('[redacted]');
    result.errors.push(message);
    result.passed = false;
  } finally {
    await terminate(active);
    await terminate(service);
    process.removeListener('SIGINT', interrupt);
    process.removeListener('SIGTERM', interrupt);
    let log = serviceLines.join('');
    for (const secret of [scope?.password, scope?.username]) if (secret) log = log.split(secret).join('[redacted]');
    writeFileSync(serviceLog, log);
    result.serverStopped = service.ended;
    if (existsSync(serviceEvidence)) {
      const stopped = JSON.parse(readFileSync(serviceEvidence, 'utf8'));
      result.serverStopped = service.ended && stopped.serverStopped === true;
    }
    if (!result.serverStopped) { result.passed = false; result.errors.push('Owned standard service did not stop cleanly'); }
    if (scope) noSecrets(result, scope);
    writeFileSync(join(output, 'native-webdav-result.json'), JSON.stringify(result, null, 2));
  }
  console.log(JSON.stringify({ passed: result.passed, nativeVerified: result.nativeVerified, resourceOnly,
    phases: result.phases.length, errors: result.errors, serverStopped: result.serverStopped, output, home }));
  return result;
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  const arguments_ = process.argv.slice(2);
  const resourceOnly = arguments_.length === 1 && arguments_[0] === '--resource-only';
  assert(resourceOnly || arguments_.length === 1, 'Usage: node scripts/validate-native-webdav-review.mjs <built-executable> | --resource-only');
  const result = await runNativeWebDavReview({ executable: resourceOnly ? undefined : arguments_[0], resourceOnly });
  if (!result.passed) process.exitCode = 1;
}
