import { cleanNativeReviewEnvironment } from "./lib/native-review-isolation.mjs";
import { isReviewModelId, reviewModelId, seedReviewModel, redactReviewModelSecrets, captureReviewModelLogs } from './lib/native-review-model.mjs';
import { nativeReviewOutput } from './lib/native-review-output.mjs';
// Parent accepts only real native reports, Chrome requests and physical bytes.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createHash, randomBytes } from 'node:crypto';
import { chmodSync, existsSync, lstatSync, mkdirSync, mkdtempSync,
  readFileSync, realpathSync, unlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, relative, resolve, isAbsolute } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { pathToFileURL } from 'node:url';
import { createBrowserReviewResource } from './lib/browser-review-resource.mjs';
import { readonlyCompatibilitySourceRevocations } from './lib/native-core-review-isolation.mjs';

const digest = value => createHash('sha256').update(value).digest('hex');
export const BROWSER_CHECKS = Object.freeze({
  'first-launch': ['browser-isolation', 'browser-mode-ui', 'browser-network-consent-ui',
    'browser-submit-download-model', 'browser-reject-delete-ui', 'browser-takeover-resume-ui', 'browser-clear-data-ui'],
  restart: ['browser-isolation', 'browser-restart-no-profile', 'browser-restored-model-reapply'],
});
export function verifyBrowserReport(report, manifest, pid, startedAt) {
  assert.equal(report?.version, 1); assert.equal(report.kind, 'native-browser');
  assert.equal(report.passed, true); assert.deepEqual(report.errors, []);
  assert.equal(report.fakeIPC, false); assert.equal(report.nativeVerified, false);
  assert.equal(report.nativeContext?.pid, pid); assert.equal(report.nativeContext.browserRequested, true);
  assert.equal(report.nativeContext.evaluations, 1); assert.equal(report.nativeContext.home, manifest.home);
  for (const name of ['home', 'nonce', 'phase']) assert.equal(report[name], manifest[name]);
  assert(Date.parse(report.finishedAt) >= startedAt && Date.parse(report.finishedAt) <= Date.now() + 1000, 'Stale or future report');
  assert.equal(report.checks?.length, BROWSER_CHECKS[manifest.phase].length);
  for (const id of BROWSER_CHECKS[manifest.phase]) {
    const found = report.checks.filter(item => item.id === id); assert.equal(found.length, 1); assert.equal(found[0].result, 'passed');
  }
  const expected = report.expected;
  assert.equal(expected?.schema, 1); assert.equal(expected.home, manifest.home); assert.equal(expected.cwd, manifest.cwd);
  assert.equal(expected.nonce, manifest.nonce); assert.equal(expected.url, manifest.url);
  assert.equal(expected.modelId, manifest.modelId);
  assert(/^[A-Za-z0-9-]{1,128}$/.test(expected.sessionId));
  assert.equal(expected.download?.passed, true); assert.equal(expected.download.sessionId, expected.sessionId);
  if (manifest.phase === 'restart') assert.deepEqual(expected, manifest.expected);
  else {
    assert(report.approvals.some(item => item.action === 'automation_set_private_network' && item.approved));
    assert.equal(report.approvals.filter(item => item.action === 'browser_type' && item.approved).length, 1);
    assert.equal(report.approvals.filter(item => item.action === 'browser_click' && item.approved).length, 2);
    assert.equal(report.approvals.filter(item => item.action === 'browser_click' && !item.approved).length, 1);
  }
  return expected;
}

export async function runNativeBrowserReview(executable, modelId = 'echoagent-ojlab/Echo Reed') {
  modelId = reviewModelId(modelId);
  assert(isReviewModelId(modelId), 'Only explicitly authorized model resources are permitted');
  assert.equal(process.platform, 'darwin'); executable = realpathSync(resolve(executable));
  assert(lstatSync(executable).isFile());
  const output = nativeReviewOutput('native-browser', 'NATIVE_BROWSER_REVIEW_OUTPUT');
  const home = realpathSync(mkdtempSync(join(tmpdir(), 'echo-native-review-browser-'))); chmodSync(home, 0o700);
  const cwd = join(home, 'workspaces/default'), legacy = join(home, 'empty-legacy');
  mkdirSync(cwd, { recursive: true, mode: 0o700 }); mkdirSync(legacy, { mode: 0o700 });
  const nonce = randomBytes(16).toString('hex'), revocations = readonlyCompatibilitySourceRevocations();
  writeFileSync(join(home, 'config.toml'), `[marketplace]\nofficial_marketplace_auto_installed = true\ndisabled_sources = [${revocations.map(JSON.stringify).join(', ')}]\n[claude_compat]\nimported = true\n`, { mode: 0o600 });
  seedReviewModel(home);
  const result = { version: 1, kind: 'real-native-browser-driver', nativeVerified: false, passed: false,
    home, cwd, output, binarySha256: digest(readFileSync(executable)), phases: [], errors: [], defaultHomeChanged: false,
    resourceRequestsMadeByParent: false, modelOutputInjected: false, modelId };
  let resource, active, activeLogs, expected, downloadObserved = false, interrupted = false;
  const physical = path => {
    const rel = relative(home, path); assert(rel && !isAbsolute(rel) && rel !== '..' && !rel.startsWith('../'));
    const stat = lstatSync(path); assert(stat.isFile() && !stat.isSymbolicLink() && stat.size <= 128 * 1024);
    assert.equal(realpathSync(path), path); return readFileSync(path);
  };
  const terminate = async () => {
    if (!active || active.exitCode !== null || active.signalCode !== null) return;
    active.kill('SIGTERM'); await delay(1500);
    if (active.exitCode === null && active.signalCode === null) { active.kill('SIGKILL'); await delay(100); }
  };
  const interrupt = () => { interrupted = true; active?.kill('SIGTERM'); };
  process.once('SIGINT', interrupt); process.once('SIGTERM', interrupt);
  try {
    resource = await createBrowserReviewResource(nonce);
    result.resource = { url: resource.url, filename: resource.filename, sha256: resource.sha256, scope: 'synthetic loopback only' };
    for (const phase of ['first-launch', 'restart']) {
      assert(!interrupted, 'Owned browser journey interrupted');
      const manifest = { schema: 1, home, cwd, nonce, phase, modelId, url: resource.url,
        expected: phase === 'restart' ? expected : null };
      writeFileSync(join(home, 'native-browser-manifest.private.json'), JSON.stringify(manifest), { mode: 0o600 });
      const reportPath = join(home, 'native-review.json'); if (existsSync(reportPath)) unlinkSync(reportPath);
      const env = { ...cleanNativeReviewEnvironment(process.env), ECHO_DESKTOP_REVIEW: '1', ECHO_DESKTOP_REVIEW_BROWSER: '1',
        ECHO_DESKTOP_REVIEW_PHASE: phase, GROK_HOME: legacy };
      for (const key of Object.keys(env)) if ((key.startsWith('ECHO_DESKTOP_REVIEW_')
        && !['ECHO_DESKTOP_REVIEW_BROWSER', 'ECHO_DESKTOP_REVIEW_PHASE'].includes(key))
        || ['HTTP_PROXY', 'HTTPS_PROXY', 'ALL_PROXY', 'http_proxy', 'https_proxy', 'all_proxy',
          'ECHO_AGENT_HOME', 'ECHO_DESKTOP_VALIDATION', 'ECHO_THEIA_NODE', 'NODE_OPTIONS', 'NODE_PATH'].includes(key)) delete env[key];
      assert.equal(env.HOME, process.env.HOME);
      const start = Date.now(); const child = spawn(executable, ['--data-home', home], { cwd, env, stdio: ['ignore', 'pipe', 'pipe'] });
      active = child; let ended = false, code, signal, childError;
      const exited = new Promise(accept => { child.once('error', error => { childError = error; ended = true; accept(); });
        child.once('close', (value, sig) => { ended = true; code = value; signal = sig; accept(); }); });
      const logs = captureReviewModelLogs(child, join(output, `${phase}-app.log`)); activeLogs = logs;
      while (!ended && Date.now() - start < (phase === 'first-launch' ? 960000 : 180000)) {
        const ready = join(cwd, 'browser-download-ready.json');
        if (phase === 'first-launch' && !downloadObserved && existsSync(ready)) {
          const data = JSON.parse(physical(ready)); assert.deepEqual(Object.keys(data).sort(), ['nonce', 'sessionId']);
          assert.equal(data.nonce, nonce); assert(/^[A-Za-z0-9-]{1,128}$/.test(data.sessionId));
          const profile = join(home, 'automation/browser-profiles', digest(data.sessionId).slice(0, 32));
          const file = join(profile, 'Downloads', resource.filename);
          if (existsSync(file)) {
            const content = physical(file); assert.deepEqual(content, resource.bytes); assert.equal(digest(content), resource.sha256);
            const snapshot = resource.snapshot();
            assert.equal(snapshot.submitted, true); assert.equal(snapshot.deleted, false);
            assert.equal(snapshot.events.filter(item => item.route === 'submit' && item.status === 200).length, 1);
            assert.equal(snapshot.events.filter(item => item.route === 'download' && item.status === 200).length, 1);
            assert(snapshot.events.filter(item => item.status === 200).every(item => /Chrome\//.test(item.userAgent)), 'Page/download did not come from real Chrome');
            const proof = { nonce, sessionId: data.sessionId, passed: true, profile, filename: resource.filename,
              sha256: resource.sha256, size: content.length, observedAt: new Date().toISOString() };
            writeFileSync(join(cwd, 'browser-download-proof.json'), JSON.stringify(proof), { flag: 'wx', mode: 0o600 });
            writeFileSync(join(output, 'download-physical-proof.json'), JSON.stringify(proof, null, 2)); downloadObserved = true;
          }
        }
        await Promise.race([exited, delay(200)]);
      }
      assert(ended, 'Actual browser process exceeded its bound'); await exited; await logs.finished;
      assert(!logs.overflow, 'Native browser log exceeded limit'); assert(!logs.incomplete, 'Native log pipes did not close after process exit'); if (childError) throw childError;
      if (existsSync(reportPath)) writeFileSync(join(output, `${phase}.json`), redactReviewModelSecrets(readFileSync(reportPath)), { mode: 0o600 });
      result.phases.push({ phase, pid: child.pid, exitCode: code, signal, elapsedSeconds: (Date.now() - start) / 1000 });
      assert.equal(code, 0); assert.equal(signal, null);
      expected = verifyBrowserReport(JSON.parse(physical(reportPath)), manifest, child.pid, start);
      const profile = join(home, 'automation/browser-profiles', digest(expected.sessionId).slice(0, 32));
      assert(!existsSync(profile), 'Clear/restart left the task browser profile');
      assert(downloadObserved, 'No physical download was observed before clear');
      const snapshot = resource.snapshot(); assert(snapshot.submitted && !snapshot.deleted);
      assert.equal(snapshot.events.filter(item => item.route === 'delete').length, 0, 'Rejected delete reached the web server');
      result.phases.at(-1).profileAbsent = true; active = undefined;
    }
    result.nativeVerified = true; result.passed = result.phases.length === 2; result.expected = expected;
  } catch (error) { result.errors.push(String(error?.message || error).slice(0, 2500)); }
  finally {
    await terminate(); await activeLogs?.finished; process.removeListener('SIGINT', interrupt); process.removeListener('SIGTERM', interrupt);
    if (resource) { await resource.close(); result.resourceSnapshot = resource.snapshot(); }
    result.finishedAt = new Date().toISOString(); writeFileSync(join(output, 'result.json'), redactReviewModelSecrets(JSON.stringify(result, null, 2)), { mode: 0o600 });
  }
  console.log(redactReviewModelSecrets(JSON.stringify({ passed: result.passed, nativeVerified: result.nativeVerified, output, errors: result.errors })));
  return result;
}
if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  assert([3, 4].includes(process.argv.length));
  assert(process.argv[3] === undefined || process.argv[3] === '--model=echo-grove');
  const result = await runNativeBrowserReview(process.argv[2], process.argv[3] ? 'echoagent-ojlab/Echo Grove' : undefined);
  if (!result.passed) process.exitCode = 1;
}
