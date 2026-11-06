import { cleanNativeReviewEnvironment } from "./lib/native-review-isolation.mjs";
import { nativeReviewOutput } from './lib/native-review-output.mjs';
// Dedicated actual native notifications and meeting-error acceptance. This
// driver owns a new home and a standard HTTP receiver; it never uses user keys.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createHash, randomBytes } from 'node:crypto';
import { createServer } from 'node:http';
import { copyFileSync, existsSync, lstatSync, mkdirSync, mkdtempSync, readFileSync,
  realpathSync, statSync, unlinkSync, writeFileSync, createReadStream } from 'node:fs';
import { basename, isAbsolute, join, relative, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { setTimeout as delay } from 'node:timers/promises';

export const REQUIRED_LOCAL_CHECKS = Object.freeze({
  'first-launch': ['native-local-isolation', 'native-local-notification-settings', 'native-local-channel-create',
    'native-local-webhook-success', 'native-local-channel-disabled-gate', 'native-local-webhook-failure-record',
    'native-local-webhook-retry', 'native-local-channel-edit-preserves-endpoint', 'native-local-meeting-missing-file',
    'native-local-meeting-fixture-recovery', 'native-local-return-shell'],
  restart: ['native-local-isolation', 'native-local-channel-restored', 'native-local-meeting-restored',
    'native-local-channel-remove', 'native-local-return-shell'],
});
const hash = (value) => createHash('sha256').update(value).digest('hex');
const within = (root, file) => {
  const rel = relative(root, file);
  return rel !== '' && rel !== '..' && !rel.startsWith('../') && !isAbsolute(rel);
};
const ordinary = (file, root) => {
  assert(within(root, file), 'Evidence escaped its owned scope');
  const info = lstatSync(file);
  assert(info.isFile() && !info.isSymbolicLink(), 'Expected an ordinary scoped evidence file');
  assert.equal(realpathSync(file), file, 'Evidence file must be canonical');
  return readFileSync(file);
};
export function localMeetingFixture(home, nonce, timestamp = Date.now()) {
  assert(/^\/private\/(?:tmp|var\/folders)\/(?:[^/]+\/)*echo-native-review-local-[^/]+$/.test(home));
  assert(/^[a-f0-9]{32}$/.test(nonce));
  const id = `${nonce.slice(0, 8)}-${nonce.slice(8, 12)}-4${nonce.slice(13, 16)}-a${nonce.slice(17, 20)}-${nonce.slice(20, 32)}`;
  return { id, title: `隔离缺文件恢复-${nonce}`, createdAt: timestamp, updatedAt: timestamp,
    status: 'completed', providerId: 'isolated-native-review-fixture', modelId: 'isolated/unconfigured',
    // The later repaired silent WAV has 320 samples at 16 kHz. Nonzero audio
    // metadata makes the production player available for the missing-file path.
    durationSeconds: 0.02, recordedSamples: 320, audioPath: join(home, 'meetings', id, '原生缺文件恢复.wav'),
    captureSource: 'imported', originalFileName: '显式隔离验收 fixture.wav', transcript: [], chunks: [],
    live: { processedSamples: 0 }, minutesStale: false,
    minutes: `# 显式隔离测试记录\n这是验收 fixture，未录音、未转写。${nonce}` };
}
export function verifyNativeLocalReport({ report, phase, home, nonce, pid, startedAt, previousExpected }) {
  assert.equal(report?.version, 1);
  assert.equal(report.kind, 'local', 'Fixture or generic reports cannot satisfy this native acceptance');
  assert(Object.hasOwn(REQUIRED_LOCAL_CHECKS, phase));
  assert.equal(report.phase, phase);
  assert.equal(report.nativeContext?.phase, phase);
  assert.equal(report.nativeContext?.home, home);
  assert.equal(report.nativeContext?.pid, pid);
  assert.equal(report.nativeContext?.localRequested, true);
  assert.equal(report.nativeContext?.evaluations, 1);
  const finishedAt = Date.parse(report.finishedAt);
  assert(Number.isFinite(finishedAt) && finishedAt >= startedAt && finishedAt <= Date.now() + 1000, 'Stale or invalid native report');
  assert.equal(report.passed, true);
  assert(Array.isArray(report.errors) && report.errors.length === 0);
  assert(Array.isArray(report.checks) && report.checks.length === REQUIRED_LOCAL_CHECKS[phase].length);
  for (const id of REQUIRED_LOCAL_CHECKS[phase]) {
    const matches = report.checks.filter((check) => check.id === id);
    assert.equal(matches.length, 1, `Missing or duplicate actual native check ${id}`);
    assert.equal(matches[0].result, 'passed', `Actual native gate did not pass: ${id}`);
  }
  for (const key of ['modelRequested', 'osNotificationVerified', 'playbackSuccessVerified', 'recordingOrAsrVerified'])
    assert.equal(report[key], false, `This isolated driver cannot claim ${key}`);
  const expected = report.expected, fixture = localMeetingFixture(home, nonce, 0);
  assert.equal(expected?.version, 1);
  assert.equal(expected.home, home);
  assert.equal(expected.nonce, nonce);
  assert.equal(expected.meetingId, fixture.id);
  assert.equal(expected.meetingTitle, fixture.title);
  assert.equal(expected.audioPath, fixture.audioPath);
  assert.equal(expected.originalMinutes, fixture.minutes);
  assert.equal(expected.minutes, `# 本地恢复校对\n缺文件错误后可继续编辑和保存。${nonce}`);
  assert.equal(expected.finalLabel, `本地通知恢复-${nonce}`);
  assert.equal(expected.label, expected.finalLabel);
  assert(/^ch_\d{10,}$/.test(expected.channelId || ''));
  if (phase === 'restart') assert.deepEqual(expected, previousExpected, 'Restart changed exact first-launch expectations');
  return expected;
}
export function verifyLocalReceiverAudit({ notifications, receiverUrl }) {
  const url = new URL(receiverUrl), base = url.pathname;
  assert(url.protocol === 'http:' && url.hostname === '127.0.0.1' && url.port && !url.username && !url.password
    && !url.search && !url.hash && /^\/echo-review\/[a-f0-9]{32}$/.test(base), 'Audit receiver is outside the fixed loopback scope');
  assert(Array.isArray(notifications) && notifications.length === 4, 'Expected exactly four actual production notification POSTs');
  assert.deepEqual(notifications.map(({ path, status }) => ({ path, status })), [
    { path: base, status: 200 }, { path: base + '/fail', status: 403 },
    { path: base, status: 200 }, { path: base, status: 200 },
  ], 'Failure/retry/blank-URL lifecycle requests were missing or duplicated');
  for (const entry of notifications) {
    assert.equal(entry.method, 'POST');
    assert.equal(entry.payload?.title, '测试通知');
    assert.equal(entry.payload.body, '来自 EchoAgent 的测试消息');
    assert.equal(entry.payload.level, 'info');
    assert.equal(entry.payload.sessionId, null);
    assert(Number.isFinite(Date.parse(entry.payload.timestamp)));
    assert.equal(entry.contentType, 'application/json');
    assert.equal(entry.bodySha256, hash(Buffer.from(entry.rawBody, 'utf8')));
    assert.deepEqual(JSON.parse(entry.rawBody), entry.payload, 'Receiver payload differs from the actual received body');
  }
  return { requests: 4, success200: 3, nonRetryFailure403: 1, manualRetrySuccesses: 1,
    blankEndpointEditSuccesses: 1, disabledOrRevokedRequests: 0, osFallbackVerified: false };
}
function silentFixtureWav() {
  const samples = 320, bytes = Buffer.alloc(44 + samples * 2);
  bytes.write('RIFF', 0); bytes.writeUInt32LE(bytes.length - 8, 4); bytes.write('WAVEfmt ', 8);
  bytes.writeUInt32LE(16, 16); bytes.writeUInt16LE(1, 20); bytes.writeUInt16LE(1, 22);
  bytes.writeUInt32LE(16000, 24); bytes.writeUInt32LE(32000, 28); bytes.writeUInt16LE(2, 32);
  bytes.writeUInt16LE(16, 34); bytes.write('data', 36); bytes.writeUInt32LE(samples * 2, 40);
  return bytes;
}
function filesystem({ home, fixture, expected, phase, receiverUrl, wav, first }) {
  const manifestPath = join(home, 'meetings', fixture.id, 'manifest.json');
  const meeting = JSON.parse(ordinary(manifestPath, home));
  // Explicitly verify the current backup contract before comparing every
  // remaining field; saving minutes must preserve the former saved version.
  const { previousMinutes, ...saved } = meeting;
  assert.equal(previousMinutes, fixture.minutes, 'Meeting save lost the previous saved minutes');
  const normalized = { ...saved, minutes: fixture.minutes, updatedAt: fixture.updatedAt };
  assert.deepEqual(normalized, fixture, 'Meeting save modified fields outside the minutes, backup and update time');
  assert.equal(meeting.minutes, expected.minutes);
  assert(Number.isSafeInteger(meeting.updatedAt) && meeting.updatedAt >= fixture.updatedAt);
  const audio = ordinary(fixture.audioPath, home);
  assert.deepEqual(audio, wav, 'Restored synthetic WAV bytes changed');
  assert.equal(audio.subarray(0, 4).toString(), 'RIFF');
  assert.equal(audio.subarray(8, 12).toString(), 'WAVE');
  const configs = JSON.parse(ordinary(join(home, 'echoagent-notify-channels.json'), home)).channels;
  assert(Array.isArray(configs));
  if (phase === 'first-launch') {
    assert.equal(configs.length, 1);
    assert.equal(configs[0].id, expected.channelId);
    assert.equal(configs[0].kind, 'generic-webhook');
    assert.equal(configs[0].label, expected.finalLabel);
    assert.equal(configs[0].endpoint, receiverUrl);
    assert.equal(configs[0].enabled, true);
  } else assert.equal(configs.length, 0, 'Restart UI removal did not persist');
  const inboxPath = join(home, 'echoagent-notifications.json');
  const inbox = JSON.parse(ordinary(inboxPath, home));
  const entries = Array.isArray(inbox) ? inbox : inbox.entries;
  assert(Array.isArray(entries), 'Unexpected production notification inbox format');
  const failures = entries.filter((entry) => entry.title === '通知投递失败' && entry.body?.includes('403'));
  assert.equal(failures.length, 1);
  assert(failures[0].body.includes(`本地通知验收-${expected.nonce}`));
  const result = { phase, providerCount: configs.length, meetingManifest: { path: manifestPath, sha256: hash(ordinary(manifestPath, home)) },
    syntheticAudio: { path: fixture.audioPath, size: audio.length, sha256: hash(audio), fixture: true },
    persistedMinutes: expected.minutes, previousMinutesPreserved: true, failureInboxId: failures[0].id, failureInboxCount: 1,
    fixtureOriginalFieldsPreserved: true, successfulPlaybackVerified: false, asrOrRecordingVerified: false };
  if (phase === 'restart') {
    assert.deepEqual(result.syntheticAudio, first.syntheticAudio, 'Restart changed the restored fixture WAV');
    assert.deepEqual(result.meetingManifest, first.meetingManifest, 'Restart changed the UI-saved meeting manifest');
    assert.equal(result.failureInboxId, first.failureInboxId);
  }
  return result;
}
function managed(child) {
  const state = { child, ended: false, error: null };
  state.exited = new Promise((accept) => {
    child.once('exit', () => { state.ended = true; accept(); });
    child.once('error', (error) => { state.error = error; state.ended = true; accept(); });
  });
  return state;
}
async function terminate(state) {
  if (!state || state.ended) return;
  state.child.kill('SIGTERM');
  await Promise.race([state.exited, delay(5000, undefined, { ref: false })]);
  if (!state.ended) { state.child.kill('SIGKILL'); await Promise.race([state.exited, delay(3000, undefined, { ref: false })]); }
}
export async function runNativeLocalReview({ executable, resourceOnly = false } = {}) {
  if (!resourceOnly) {
    assert.equal(process.platform, 'darwin', 'This native review targets an actual macOS WebView');
    assert(executable, 'Pass the built application executable containing the fixed local factory');
    executable = realpathSync(resolve(executable));
    assert(lstatSync(executable).isFile());
  }
  const outputRoot = nativeReviewOutput('native-local', 'NATIVE_LOCAL_REVIEW_OUTPUT');
  mkdirSync(outputRoot, { recursive: true });
  const output = mkdtempSync(join(outputRoot, 'run-'));
  const home = realpathSync(mkdtempSync('/private/tmp/echo-native-review-local-'));
  assert.equal(statSync(home).mode & 0o777, 0o700);
  assert(basename(home).startsWith('echo-native-review-local-'));
  const nonce = randomBytes(16).toString('hex'), fixture = localMeetingFixture(home, nonce), wav = silentFixtureWav();
  mkdirSync(join(home, 'meetings', fixture.id), { recursive: true, mode: 0o700 });
  const meetingPath = join(home, 'meetings', fixture.id, 'manifest.json');
  writeFileSync(meetingPath, JSON.stringify(fixture, null, 2), { mode: 0o600, flag: 'wx' });
  assert(!existsSync(fixture.audioPath));
  const legacy = join(home, 'empty-legacy');
  mkdirSync(legacy, { mode: 0o700 });
  mkdirSync(join(home, 'workspaces', 'default'), { recursive: true, mode: 0o700 });
  const notifications = [], receiverErrors = [], controls = [];
  const result = { version: 1, home, output, resourceOnly, passed: false, nativeVerified: false,
    layer: resourceOnly ? 'real owned loopback HTTP resource preparation only' : 'actual native notification and meeting-error recovery',
    phases: [], errors: [], modelRequested: false, osNotificationVerified: false,
    playbackSuccessVerified: false, recordingOrAsrVerified: false };
  let expected, repaired = false, active, interrupted = false, receiverUrl;
  const base = `/echo-review/${nonce}`;
  const server = createServer(async (request, response) => {
    const headers = { 'Content-Type': 'application/json', 'Access-Control-Allow-Origin': '*',
      'Access-Control-Allow-Methods': 'GET, POST, OPTIONS', 'Access-Control-Allow-Headers': 'Content-Type', 'Cache-Control': 'no-store' };
    const reply = (status, value) => { response.writeHead(status, headers); response.end(JSON.stringify(value)); };
    try {
      if (request.method === 'OPTIONS' && [base + '/audit', base + '/state', base + '/repair'].includes(request.url)) { reply(204, {}); return; }
      if (request.method === 'GET' && request.url === base + '/audit') { reply(200, { notifications }); return; }
      if (request.method === 'GET' && request.url === base + '/state') { reply(200, { expected }); return; }
      if (request.method !== 'POST' || ![base, base + '/fail', base + '/state', base + '/repair'].includes(request.url)) { reply(404, { error: 'Outside owned review scope' }); return; }
      const chunks = []; let size = 0;
      for await (const chunk of request) { size += chunk.length; assert(size <= 16384, 'Owned receiver request exceeded 16KiB'); chunks.push(chunk); }
      const bytes = Buffer.concat(chunks), rawBody = bytes.toString('utf8');
      if (request.url === base + '/repair') {
        assert(!repaired && !existsSync(fixture.audioPath), 'Synthetic repair may run exactly once');
        writeFileSync(fixture.audioPath, wav, { flag: 'wx', mode: 0o600 }); repaired = true;
        controls.push({ action: 'repair-synthetic-wav', path: fixture.audioPath, sha256: hash(wav), fixture: true });
        reply(200, { repaired: true, path: fixture.audioPath, sha256: hash(wav), fixture: true }); return;
      }
      if (request.url === base + '/state') {
        assert(!expected, 'First-launch expectations may be established exactly once');
        const parsed = JSON.parse(rawBody);
        assert(parsed.expected?.home === home && parsed.expected.nonce === nonce && parsed.expected.meetingId === fixture.id);
        expected = parsed.expected; controls.push({ action: 'retain-first-launch-expectations' });
        reply(200, { retained: true }); return;
      }
      assert.equal(request.headers['content-type']?.split(';')[0], 'application/json');
      const payload = JSON.parse(rawBody), status = request.url === base + '/fail' ? 403 : 200;
      notifications.push({ method: request.method, path: request.url, status, payload, rawBody,
        contentType: request.headers['content-type'].split(';')[0], bodySha256: hash(bytes), receivedAt: new Date().toISOString() });
      reply(status, status === 403 ? { error: 'Intentional owned receiver rejection' } : { received: true });
    } catch (error) { receiverErrors.push(String(error.message || error)); if (!response.headersSent) reply(400, { error: 'Owned receiver validation failed' }); else response.end(); }
  });
  server.requestTimeout = 15000;
  let firstFilesystem, previousExpected;
  const interrupt = () => { interrupted = true; active?.child.kill('SIGTERM'); };
  process.once('SIGINT', interrupt); process.once('SIGTERM', interrupt);
  try {
    await new Promise((accept, reject) => { server.once('error', reject); server.listen(0, '127.0.0.1', accept); });
    receiverUrl = `http://127.0.0.1:${server.address().port}${base}`;
    const scope = { schema: 1, home, nonce, receiverUrl };
    const manifest = join(home, 'native-local-manifest.private.json');
    writeFileSync(manifest, JSON.stringify(scope), { mode: 0o600, flag: 'wx' });
    result.receiver = { url: receiverUrl, type: 'standard Node HTTP server', externalContacts: false };
    result.fixtureSeed = { path: meetingPath, sha256: hash(ordinary(meetingPath, home)), audioInitiallyAbsent: true, recordingOrAsrPerformed: false };
    if (resourceOnly) {
      const response = await fetch(receiverUrl + '/audit');
      assert.equal(response.status, 200); assert.deepEqual((await response.json()).notifications, []);
      const outside = await fetch(receiverUrl + '/outside'); assert.equal(outside.status, 404);
      assert.equal(notifications.length, 0); assert(!existsSync(fixture.audioPath));
      result.resourceChecks = [{ id: 'real-http-listen-and-json-audit', passed: true }, { id: 'unknown-path-denied', passed: true },
        { id: 'private-home-manifest-and-explicit-seed', passed: statSync(manifest).mode % 0o1000 === 0o600 }];
      assert(result.resourceChecks.every((check) => check.passed)); result.passed = true;
    } else {
      result.executable = executable;
      const binaryHash = createHash('sha256');
      for await (const bytes of createReadStream(executable)) binaryHash.update(bytes);
      result.binarySha256 = binaryHash.digest('hex');
      const launchEnv = { ...cleanNativeReviewEnvironment(process.env) };
      for (const name of Object.keys(launchEnv)) if (name.startsWith('ECHO_DESKTOP_REVIEW') || name.startsWith('ECHO_LOCAL_REVIEW')
        || name.startsWith('ECHO_WEBDAV_REVIEW') || ['ECHO_AGENT_HOME', 'ECHO_DESKTOP_VALIDATION', 'ECHO_THEIA_NODE', 'NATIVE_REVIEW_SEED_CONFIG', 'NATIVE_REVIEW_AUDIO'].includes(name)) delete launchEnv[name];
      Object.assign(launchEnv, { GROK_HOME: legacy, ECHO_DESKTOP_REVIEW: '1', ECHO_DESKTOP_REVIEW_LOCAL: '1',
        ECHO_DESKTOP_REVIEW_MEDIA: '0', ECHO_DESKTOP_REVIEW_EXTENDED: '0', ECHO_LOCAL_REVIEW_MANIFEST: manifest,
        NO_PROXY: '127.0.0.1,localhost,::1', no_proxy: '127.0.0.1,localhost,::1' });
      const reportPath = join(home, 'native-review.json'), historyPath = join(home, 'native-review-history.jsonl');
      for (const phase of ['first-launch', 'restart']) {
        assert(!interrupted, 'Owned local acceptance interrupted');
        if (existsSync(reportPath)) unlinkSync(reportPath);
        const oldHistory = existsSync(historyPath) ? ordinary(historyPath, home).toString().trim().split('\n').filter(Boolean).map(JSON.parse) : [];
        const startedAt = Date.now();
        active = managed(spawn(executable, ['--data-home', home], { env: { ...launchEnv, ECHO_DESKTOP_REVIEW_PHASE: phase }, stdio: ['ignore', 'pipe', 'pipe'] }));
        const logs = []; active.child.stdout?.on('data', (data) => logs.push(data.toString())); active.child.stderr?.on('data', (data) => logs.push(data.toString()));
        try {
          const finished = await Promise.race([active.exited.then(() => true), delay(240000, false, { ref: false })]);
          assert(finished, 'Native local phase exceeded 240s'); if (active.error) throw active.error;
          assert.equal(active.child.exitCode, 0, 'Native application did not exit normally');
          const report = JSON.parse(ordinary(reportPath, home)); copyFileSync(reportPath, join(output, `${phase}.json`));
          result.phases.push({ phase, pid: active.child.pid, exitCode: active.child.exitCode, report });
          const current = verifyNativeLocalReport({ report, phase, home, nonce, pid: active.child.pid, startedAt, previousExpected });
          const history = ordinary(historyPath, home).toString().trim().split('\n').filter(Boolean).map(JSON.parse);
          assert.equal(history.length, oldHistory.length + 1); assert.deepEqual(history.slice(0, -1), oldHistory); assert.deepEqual(history.at(-1), report);
          assert.deepEqual(current, expected, 'Native report does not match retained first-launch expectations');
          assert.equal(receiverErrors.length, 0); verifyLocalReceiverAudit({ notifications, receiverUrl });
          assert.equal(repaired, true);
          const proof = filesystem({ home, fixture, expected: current, phase, receiverUrl, wav, first: firstFilesystem });
          writeFileSync(join(output, `${phase}-filesystem.json`), JSON.stringify(proof, null, 2));
          result.phases.at(-1).filesystem = proof;
          if (phase === 'first-launch') { firstFilesystem = proof; previousExpected = current; }
        } finally { await terminate(active); writeFileSync(join(output, `${phase}.log`), logs.join('')); active = undefined; }
      }
      result.nativeVerified = true; result.passed = result.phases.length === 2;
      result.receiverVerification = verifyLocalReceiverAudit({ notifications, receiverUrl });
    }
  } catch (error) { result.errors.push(String(error?.message || error).slice(0, 2000)); result.passed = false; }
  finally {
    await terminate(active); process.removeListener('SIGINT', interrupt); process.removeListener('SIGTERM', interrupt);
    if (server.listening) {
      const closed = new Promise((accept) => server.close(accept)); server.closeAllConnections(); await closed;
    }
    result.serverStopped = !server.listening;
    assert(result.serverStopped);
    result.receiverErrors = receiverErrors; if (receiverErrors.length) result.passed = false;
    writeFileSync(join(output, 'receiver-requests.json'), JSON.stringify({ notifications, controls, receiverErrors }, null, 2));
    result.requestAudit = join(output, 'receiver-requests.json'); result.requestAuditSha256 = hash(readFileSync(result.requestAudit));
    writeFileSync(join(output, 'native-local-result.json'), JSON.stringify(result, null, 2));
  }
  console.log(JSON.stringify({ passed: result.passed, nativeVerified: result.nativeVerified, resourceOnly,
    phases: result.phases.length, errors: result.errors, serverStopped: result.serverStopped, output, home }));
  return result;
}
if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  const args = process.argv.slice(2), resourceOnly = args.length === 1 && args[0] === '--resource-only';
  assert(resourceOnly || args.length === 1 && !args[0].startsWith('--'), 'Usage: node scripts/validate-native-local-review.mjs <built-executable> | --resource-only');
  const result = await runNativeLocalReview({ executable: resourceOnly ? undefined : args[0], resourceOnly });
  if (!result.passed) process.exitCode = 1;
}
