import assert from 'node:assert/strict';
import { spawn, execFileSync } from 'node:child_process';
import { createHash, randomBytes } from 'node:crypto';
import { chmodSync, existsSync, lstatSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, unlinkSync, writeFileSync } from 'node:fs';
import { join, resolve, relative, isAbsolute } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { pathToFileURL } from 'node:url';
import { cleanNativeReviewEnvironment } from './lib/native-review-isolation.mjs';
import { PRIVATE_REVIEW_MODEL_ID, seedReviewModel, captureReviewModelLogs, redactReviewModelSecrets } from './lib/native-review-model.mjs';
import { nativeReviewOutput } from './lib/native-review-output.mjs';
import { activateNativeReviewWindow, assertNativeReviewSessionUnlocked } from './lib/native-review-window.mjs';
import { confirmNativeReviewWorkspace } from './lib/native-core-workspace-picker.mjs';
import { nativeExtensionsAction } from './lib/native-extensions-actions.mjs';
import { readonlyCompatibilitySourceRevocations } from './lib/native-core-review-isolation.mjs';
import { assertSupportedNodeVersion } from './node-runtime-version.mjs';

const sha = value => createHash('sha256').update(value).digest('hex');
export const EXTENSIONS_CHECKS = Object.freeze({
  'first-launch': ['extensions-isolation', 'extensions-real-session', 'connector-config-validation-and-consent',
    'connector-tool-and-server-toggle', 'skill-native-inspection-install', 'skill-and-plugin-live-toggle', 'extensions-real-skill-plugin-mcp-task'],
  restart: ['extensions-isolation', 'extensions-restart-discovery', 'extensions-real-skill-plugin-mcp-task', 'extensions-remove-ui-and-source-preservation'],
});
export function extensionsEnvironment(base, legacy, phase) {
  assert(Object.hasOwn(EXTENSIONS_CHECKS, phase)); const env = cleanNativeReviewEnvironment(base);
  for (const key of ['ECHO_AGENT_HOME', 'ECHO_THEIA_NODE', 'NODE_OPTIONS', 'NODE_PATH']) delete env[key];
  return { ...env, GROK_HOME: legacy, RUST_LOG: 'warn,echoagent_lib::mcp=info', ECHO_DESKTOP_REVIEW: '1', ECHO_DESKTOP_REVIEW_EXTENSIONS: '1', ECHO_DESKTOP_REVIEW_PHASE: phase };
}
export function createExtensionResources(home, nonce) {
  assertSupportedNodeVersion();
  assert(/^\/private\/tmp\/echo-native-review-extensions-[A-Za-z0-9_-]+$/.test(home)); assert(/^[a-f0-9]{32}$/.test(nonce));
  const cwd = join(home, 'workspaces/default'), root = join(cwd, 'extension-fixtures'), skill = join(root, 'managed-skill'), plugin = join(root, 'plugin');
  for (const p of [cwd, skill, join(plugin, 'skills/plugin-marker'), join(home, 'empty-legacy')]) mkdirSync(p, { recursive: true, mode: 0o700 });
  const node = realpathSync(process.execPath);
  const files = new Map([
    [join(root, 'server.mjs'), readFileSync(new URL('./fixtures/native-extensions-mcp.mjs', import.meta.url))],
    [join(root, 'inputs.json'), JSON.stringify({ nonce, node, server: join(root, 'server.mjs') })],
    [join(skill, 'SKILL.md'), `---\nname: native-review-skill\ndescription: Synthetic native MCP delivery acceptance workflow\nversion: 1.0.0\n---\n\n在最终答复中原样输出 SKILL-${[...nonce].reverse().join('')}。根据用户给出的 first 或 restart 阶段，通过 native-extensions 连接器调用 echo_deliver，参数为 nonce=${nonce} 与 phase=用户给出的阶段。调用成功后原样输出 DELIVERED-${nonce} 及工具返回的文件路径。按会话实际加载方式使用本技能，仅可读取本技能说明，不读其他文件，不使用终端或文件写入工具。\n`],
    [join(plugin, 'plugin.json'), JSON.stringify({ name: 'native-review-plugin', version: '1.0.0', description: 'Owned synthetic plugin with one prompt skill' })],
    [join(plugin, 'skills/plugin-marker/SKILL.md'), `---\nname: plugin-marker\ndescription: Synthetic plugin skill activation proof\n---\n\n只回复 PLUGIN-${nonce.slice(16)}${nonce.slice(0, 16)}。不要调用其他工具。\n`],
  ]);
  for (const [p, content] of files) writeFileSync(p, content, { mode: 0o600, flag: 'wx' });
  return { cwd, root, skill, plugin, node, files: [...files.keys()].map(path => ({ path, sha256: sha(readFileSync(path)) })) };
}
export function verifyExtensionsReport(report, scope, pid, startedAt) {
  assert.equal(report?.kind, 'native-extensions'); assert.equal(report.version, 1); assert.equal(report.fakeIPC, false);
  assert.equal(report.passed, true, JSON.stringify(report.errors)); assert.deepEqual(report.errors, []);
  for (const key of ['home', 'nonce', 'phase']) assert.equal(report[key], scope[key]);
  assert.equal(report.nativeContext?.pid, pid); assert.equal(report.nativeContext.home, scope.home); assert.equal(report.nativeContext.evaluations, 1);
  assert(Date.parse(report.finishedAt) >= startedAt && Date.parse(report.finishedAt) <= Date.now() + 1000);
  assert.deepEqual(report.checks.map(c => c.id), EXTENSIONS_CHECKS[scope.phase]); assert(report.checks.every(c => c.result === 'passed'));
  const e = report.expected; for (const key of ['home', 'cwd', 'nonce', 'modelId']) assert.equal(e[key], scope[key]);
  assert(/^[A-Za-z0-9-]+$/.test(e.sessionId)); assert(e.installedSkillPath.startsWith(`${scope.home}/`));
  assert.equal(report.permissions.length, 1); assert.equal(report.permissions[0].args.phase, scope.phase === 'first-launch' ? 'first' : 'restart');
  assert.equal(report.permissions[0].args.nonce, scope.nonce); assert.equal(report.permissions[0].sessionId, e.sessionId);
  assert.equal(report.permissions[0].pendingBeforeApproval, true); assert.equal(report.permissions[0].via, 'production allow-once UI');
  const task = report.checks.find(c => c.id === 'extensions-real-skill-plugin-mcp-task').evidence;
  for (const turn of [task.plugin, task.managed]) {
    assert(turn.completion?.promptId && turn.completion.sessionId === e.sessionId);
    assert(turn.tools.every(u => u.sessionId === e.sessionId && u._meta?.promptId === turn.completion.promptId
      && u._meta?.isReplay !== true), 'Historical replay or another turn cannot prove the current skill execution');
  }
  const completedRead = (rows, path, marker) => {
    const ids = new Set(rows.filter(u => u.rawInput?.variant === 'ReadFile' && u.rawInput.target_file === path).map(u => u.toolCallId));
    assert.equal(ids.size, 1, 'No unique actual skill read');
    const done = rows.find(u => ids.has(u.toolCallId) && u.status === 'completed'
      && u.rawOutput?.FileContent?.absolute_path === path && u.rawOutput.FileContent.raw_output?.includes(marker));
    assert(done, 'Actual skill read did not return its scoped source marker');
  };
  const pluginPath = `${scope.cwd}/extension-fixtures/plugin/skills/plugin-marker/SKILL.md`;
  completedRead(task.plugin.tools, pluginPath, e.pluginMarker);
  const installedPath = e.installedSkillPath.endsWith('/SKILL.md') ? e.installedSkillPath : `${e.installedSkillPath}/SKILL.md`;
  completedRead(task.managed.tools, installedPath, e.skillMarker);
  const rows = task.managed.tools, approval = report.permissions[0];
  const invoked = rows.find(u => u.toolCallId === approval.toolCallId && u.rawInput?.variant === 'UseTool'
    && u.rawInput.tool_name === 'native-extensions__echo_deliver' && u.rawInput.tool_input.nonce === scope.nonce
    && u.rawInput.tool_input.phase === approval.args.phase);
  assert(invoked && rows.some(u => u.toolCallId === approval.toolCallId && u.status === 'completed'), 'Completed MCP call does not match the actual approved call');
  for (const u of rows.filter(u => u.rawInput?.variant)) {
    assert(['ReadFile', 'SearchTool', 'UseTool'].includes(u.rawInput.variant), 'Unexpected Runtime tool executed');
    if (u.rawInput.variant === 'ReadFile') assert([pluginPath, installedPath].includes(u.rawInput.target_file), 'Read escaped the two owned skill definitions');
    if (u.rawInput.variant === 'UseTool') assert.equal(u.rawInput.tool_name, 'native-extensions__echo_deliver');
  }
  if (scope.phase === 'restart') assert.deepEqual(e, scope.expected);
  return e;
}
function fileBytes(path, home) {
  const r = relative(home, path); assert(r && !r.startsWith('../') && r !== '..' && !isAbsolute(r));
  const s = lstatSync(path); assert(s.isFile() && !s.isSymbolicLink() && realpathSync(path) === path && s.size <= 8 * 1024 * 1024);
  return readFileSync(path);
}
function captureOwnedWindow(pid, path) {
  assert(Number.isSafeInteger(pid) && pid > 1);
  const script = `ObjC.import('CoreGraphics'); ObjC.bindFunction('CGWindowListCopyWindowInfo',['id',['uint32','uint32']]); JSON.stringify(ObjC.deepUnwrap($.CGWindowListCopyWindowInfo(1,0)).filter(w=>w.kCGWindowOwnerPID===${pid}&&w.kCGWindowLayer===0&&w.kCGWindowBounds.Width>=700).map(w=>w.kCGWindowNumber))`;
  const ids = JSON.parse(execFileSync('/usr/bin/osascript', ['-l', 'JavaScript', '-e', script], { encoding: 'utf8', timeout: 5000 }));
  if (!Array.isArray(ids) || ids.length !== 1) return null;
  assert(Number.isSafeInteger(ids[0]) && ids[0] > 0);
  execFileSync('/usr/sbin/screencapture', ['-x', '-l', String(ids[0]), path], { timeout: 5000 });
  return { path, ownerPid: pid, windowId: ids[0], sha256: sha(readFileSync(path)) };
}
function physicalProof(home, e, resources, phase, pids) {
  for (const f of resources.files) assert.equal(sha(fileBytes(f.path, home)), f.sha256, 'Product operation changed the source fixture');
  const audit = fileBytes(join(e.cwd, 'extension-mcp-audit.jsonl'), home).toString().trim().split('\n').map(JSON.parse);
  assert(audit.some(x => x.event === 'initialize') && audit.some(x => x.event === 'tools/list'));
  assert(!audit.some(x => x.event === 'rejected'), 'Actual MCP server rejected an out-of-scope request');
  assert(audit.every(x => x.nonce === e.nonce && pids.includes(x.ppid)), 'MCP activity did not belong to the owned native process');
  const processIds = [...new Set(audit.map(x => x.pid))];
  for (const pid of processIds) {
    assert(Number.isSafeInteger(pid) && pid > 1);
    let absent = false; try { process.kill(pid, 0); } catch (error) { assert.equal(error.code, 'ESRCH'); absent = true; }
    assert(absent, 'Owned MCP process remained after native exit');
  }
  const calls = audit.filter(x => x.event === 'tools/call');
  const phases = phase === 'first-launch' ? ['first'] : ['first', 'restart'];
  assert.equal(calls.length, phases.length); const artifacts = [];
  for (const p of phases) {
    assert.equal(calls.filter(c => c.phase === p && c.tool === 'echo_deliver' && c.artifactExists).length, 1);
    const path = join(e.cwd, `extensions-result-${p}.txt`), content = `MCP-${p}-${e.nonce}\n真实 stdio MCP 交付。\n`;
    const bytes = fileBytes(path, home); assert.equal(bytes.toString(), content); artifacts.push({ path, bytes: bytes.length, sha256: sha(bytes) });
  }
  const installed = e.installedSkillPath;
  if (phase === 'first-launch') {
    assert(existsSync(installed)); const path = lstatSync(installed).isDirectory() ? join(installed, 'SKILL.md') : installed;
    assert.equal(sha(fileBytes(path, home)), resources.files.find(f => f.path === join(resources.skill, 'SKILL.md')).sha256);
  } else assert(!existsSync(installed), 'Managed uninstall left the installed package');
  const config = JSON.parse(execFileSync('python3', ['-c', 'import sys,tomllib,json; c=tomllib.load(open(sys.argv[1],"rb")); print(json.dumps({"mcp":c.get("mcp_servers",{}).get("native-extensions"),"plugins":c.get("plugins",{}).get("paths",[])}))', join(home, 'config.toml')], { encoding: 'utf8' }));
  const mirror = JSON.parse(fileBytes(join(home, 'mcp.json'), home)).mcpServers ?? {};
  if (phase === 'first-launch') { assert(config.mcp && mirror['native-extensions']); assert(config.plugins.includes(resources.plugin)); }
  else { assert(!config.mcp && !mirror['native-extensions']); assert(!config.plugins.includes(resources.plugin)); }
  return { sourceFilesUnchanged: true, artifacts, successfulMcpCalls: calls, initializedProcesses: processIds, mcpProcessesAbsentAfterExit: true,
    managedSkillPresent: existsSync(installed), configuredConnectorPresent: Boolean(config.mcp), configuredPluginPresent: config.plugins.includes(resources.plugin) };
}
export async function runNativeExtensionsReview(executable) {
  assert.equal(process.platform, 'darwin'); assertNativeReviewSessionUnlocked(); executable = realpathSync(resolve(executable));
  const binary = readFileSync(executable), fixture = readFileSync(new URL('./fixtures/native-extensions-review.js', import.meta.url));
  assert(binary.includes(fixture), 'Rebuild native executable with the current fixed extension fixture');
  const output = nativeReviewOutput('native-extensions', 'NATIVE_EXTENSIONS_REVIEW_OUTPUT');
  const home = realpathSync(mkdtempSync('/private/tmp/echo-native-review-extensions-')); chmodSync(home, 0o700);
  const nonce = randomBytes(16).toString('hex'), resources = createExtensionResources(home, nonce), { cwd } = resources;
  writeFileSync(join(home, 'config.toml'), `[marketplace]\nofficial_marketplace_auto_installed = true\ndisabled_sources = [${readonlyCompatibilitySourceRevocations().map(JSON.stringify).join(', ')}]\n[claude_compat]\nimported = true\n[memory]\nenabled = false\n[plugins]\npaths = [${JSON.stringify(resources.plugin)}]\n[permission]\nask = ["mcp__native-extensions__echo_deliver"]\n`, { mode: 0o600 });
  seedReviewModel(home);
  const result = { version: 1, kind: 'real-native-extensions-driver', home, cwd, output, passed: false, nativeVerified: false,
    binarySha256: sha(binary), fixtureSha256: sha(fixture), resources: resources.files, phases: [], errors: [], modelId: PRIVATE_REVIEW_MODEL_ID, externalAccountsUsed: false };
  let active, logs, expected, interrupted = false;
  const interrupt = () => { interrupted = true; active?.kill('SIGTERM'); }; process.once('SIGINT', interrupt); process.once('SIGTERM', interrupt);
  try {
    for (const phase of ['first-launch', 'restart']) {
      assert(!interrupted, 'Owned journey interrupted');
      const scope = { schema: 1, home, cwd, nonce, phase, modelId: PRIVATE_REVIEW_MODEL_ID, expected: expected ?? null };
      writeFileSync(join(home, 'native-extensions-manifest.private.json'), JSON.stringify(scope), { mode: 0o600 });
      const reportPath = join(home, 'native-review.json'); if (existsSync(reportPath)) unlinkSync(reportPath);
      const startedAt = Date.now(); active = spawn(executable, ['--data-home', home], { cwd, env: extensionsEnvironment(process.env, join(home, 'empty-legacy'), phase), stdio: ['ignore', 'pipe', 'pipe'] });
      const child = active; let ended = false, code, signal, error;
      child.once('error', e => { ended = true; error = e; });
      const closed = new Promise(resolve => child.once('close', (c, s) => { ended = true; code = c; signal = s; resolve(); }));
      logs = captureReviewModelLogs(child, join(output, `${phase}-app.log`));
      writeFileSync(join(output, `${phase}-launch.json`), JSON.stringify({ pid: child.pid, ...scope }), { mode: 0o600 });
      let picked = false, nextActivate = 0; const actions = new Set(), actionEvidence = [], screenshots = [];
      while (!ended && Date.now() - startedAt < 1150000) {
        if (Date.now() > nextActivate) { activateNativeReviewWindow(child.pid); nextActivate = Date.now() + 15000; }
        const ready = join(cwd, 'extensions-workspace-picker-ready.json');
        if (phase === 'first-launch' && !picked && existsSync(ready)) {
          assert.deepEqual(JSON.parse(fileBytes(ready, home)), { nonce, cwd });
          picked = confirmNativeReviewWorkspace({ pid: child.pid, home, cwd, kind: 'extensions' });
        }
        const request = join(cwd, 'extensions-native-action.json');
        if (phase === 'first-launch' && existsSync(request)) {
          const value = JSON.parse(fileBytes(request, home)); assert.equal(value.nonce, nonce); assert.deepEqual(Object.keys(value).sort(), ['action', 'nonce']);
          if (!actions.has(value.action)) {
            if (value.action === 'stdio-allow') {
              assert(actions.has('stdio-cancel'), 'No owned native cancel action observed');
              assert(!existsSync(join(cwd, 'extension-mcp-audit.jsonl')), 'Cancelled confirmation started the MCP process');
              assert(!existsSync(join(home, 'mcp.json')), 'Cancelled confirmation wrote the MCP mirror');
              const saved = execFileSync('python3', ['-c', 'import sys,tomllib; c=tomllib.load(open(sys.argv[1],"rb")); print("native-extensions" in c.get("mcp_servers",{}))', join(home, 'config.toml')], { encoding: 'utf8' }).trim();
              assert.equal(saved, 'False', 'Cancelled confirmation persisted the native connector');
            }
            if (nativeExtensionsAction({ pid: child.pid, home, cwd, nonce, node: resources.node, action: value.action })) {
              actions.add(value.action); actionEvidence.push({ action: value.action, pid: child.pid, at: new Date().toISOString() });
            }
          }
        }
        const artifact = join(cwd, `extensions-result-${phase === 'first-launch' ? 'first' : 'restart'}.txt`);
        if (!screenshots.length && existsSync(artifact)) {
          const shot = captureOwnedWindow(child.pid, join(output, `${phase}-native-delivery.png`));
          if (shot) screenshots.push(shot);
        }
        await Promise.race([closed, delay(250)]);
      }
      assert(ended, 'Actual native extension journey exceeded deadline'); await closed; await logs.finished; if (error) throw error;
      assert(!logs.overflow && !logs.incomplete);
      const report = JSON.parse(readFileSync(reportPath)); writeFileSync(join(output, `${phase}.json`), redactReviewModelSecrets(JSON.stringify(report, null, 2)), { mode: 0o600 });
      result.phases.push({ phase, pid: child.pid, exitCode: code, signal, seconds: (Date.now() - startedAt) / 1000,
        nativeActions: [...actions], nativeActionEvidence: actionEvidence, screenshots });
      assert.equal(code, 0); assert.equal(signal, null); expected = verifyExtensionsReport(report, scope, child.pid, startedAt);
      if (phase === 'first-launch') {
        assert(picked && ['stdio-cancel', 'stdio-allow', 'stdio-reenable', 'skill-picker'].every(a => actions.has(a)));
        const grants = JSON.parse(fileBytes(join(home, 'echoagent-picked-workspaces.json'), home));
        assert(grants.includes(cwd) && grants.includes(resources.skill)); result.osPickerVerified = true;
      }
      result.phases.at(-1).physicalProof = physicalProof(home, expected, resources, phase, result.phases.map(p => p.pid)); active = undefined;
    }
    result.expected = expected; result.nativeVerified = result.passed = true;
  } catch (error) { result.errors.push(String(error?.stack || error).slice(0, 6000)); }
  finally {
    if (active && active.exitCode === null && active.signalCode === null) { active.kill('SIGTERM'); await delay(1500); if (active.exitCode === null && active.signalCode === null) active.kill('SIGKILL'); }
    await logs?.finished; process.removeListener('SIGINT', interrupt); process.removeListener('SIGTERM', interrupt);
    result.finishedAt = new Date().toISOString(); writeFileSync(join(output, 'result.json'), redactReviewModelSecrets(JSON.stringify(result, null, 2)), { mode: 0o600 });
  }
  console.log(JSON.stringify({ passed: result.passed, nativeVerified: result.nativeVerified, home, output, errors: result.errors })); return result;
}
if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  assert.equal(process.argv.length, 3, 'Pass a built native executable and explicit NATIVE_REVIEW_MODEL_CONFIG');
  if (!(await runNativeExtensionsReview(process.argv[2])).passed) process.exitCode = 1;
}
