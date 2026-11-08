// Host tests do not run the Runtime dependency's own library tests.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { lstatSync, mkdirSync, mkdtempSync, realpathSync } from 'node:fs';
import { homedir, tmpdir } from 'node:os';
import { basename, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { cleanNativeReviewEnvironment, withNativeReviewLoopbackBypass } from './lib/native-review-isolation.mjs';
import { ownedTestExecutables } from './prepare-macos-test-signatures.mjs';
import { runNativeTestProcess } from './lib/native-test-process.mjs';

assert.equal(process.argv.length, 2, 'This entry runs the complete Runtime library suite');
const root = realpathSync(fileURLToPath(new URL('../', import.meta.url)));
const workspace = realpathSync(join(root, 'vendor/echo-agent-build/crates/codegen/echo-agent-runtime'));
const target = join(root, 'src-tauri/target');
const toolHome = homedir();
const home = realpathSync(mkdtempSync(join(tmpdir(), 'echo-runtime-tests-')));
const legacy = join(home, 'empty-legacy'); mkdirSync(legacy, { mode: 0o700 });
const env = withNativeReviewLoopbackBypass({ ...cleanNativeReviewEnvironment(process.env),
  HOME: home, USERPROFILE: home, ECHO_AGENT_HOME: home, GROK_HOME: legacy,
  XDG_CONFIG_HOME: join(home, 'config'), XDG_DATA_HOME: join(home, 'data'), XDG_CACHE_HOME: join(home, 'cache'),
  CARGO_HOME: process.env.CARGO_HOME || join(toolHome, '.cargo'),
  RUSTUP_HOME: process.env.RUSTUP_HOME || join(toolHome, '.rustup'), CARGO_TARGET_DIR: target });
// Normal-mode auth/home contracts differ deliberately from native acceptance
// mode. Private HOME/data/config paths and stripped credentials isolate these
// unit tests without changing which production branch they exercise.
for (const name of ['ECHO_AGENT_REVIEW_ISOLATED', 'ECHO_DESKTOP_REVIEW', 'ECHO_DESKTOP_VALIDATION', 'RUST_MIN_STACK']) delete env[name];
if (process.platform === 'win32') {
  env.APPDATA = join(home, 'AppData/Roaming');
  env.LOCALAPPDATA = join(home, 'AppData/Local');
}
console.log(`Isolated Runtime library test data: ${home}`);
const build = spawnSync('cargo', ['test', '--locked', '--manifest-path', join(root, 'vendor/echo-agent-build/Cargo.toml'),
  '-p', 'echo-agent-runtime', '--lib', '-j', '2', '--no-run', '--message-format=json'],
{ cwd: root, env, encoding: 'utf8', maxBuffer: 32 * 1024 * 1024, stdio: ['ignore', 'pipe', 'inherit'] });
const messages = (build.stdout ?? '').split('\n').filter(Boolean).map(line => {
  try { return JSON.parse(line); } catch { return {}; }
});
for (const message of messages) {
  if (message.reason === 'compiler-message' && message.message?.rendered) process.stderr.write(message.message.rendered);
}
assert.equal(build.status, 0, build.error?.message || 'Runtime library test compilation failed');
const programs = ownedTestExecutables(messages, workspace, realpathSync(target));
assert.equal(programs.length, 1, 'Expected exactly the Runtime library harness');
const program = programs[0];
assert.match(basename(program), /^echo_agent_runtime-[a-f0-9]{16}(?:\.exe)?$/);
const stat = lstatSync(program);
assert(stat.isFile() && !stat.isSymbolicLink());
if (process.getuid) assert.equal(stat.uid, process.getuid());
assert.equal(realpathSync(program), program);
if (process.platform === 'darwin') {
  for (const args of [['--force', '--sign', '-', program], ['--verify', program]]) {
    const signed = spawnSync('/usr/bin/codesign', args, { env, encoding: 'utf8' });
    assert.equal(signed.status, 0, signed.stderr || 'Local Runtime test signature failed');
  }
}
const options = { cwd: workspace, env };
const discoveryOptions = { ...options, encoding: 'utf8', maxBuffer: 4 * 1024 * 1024 };
const listed = spawnSync(program, ['--list'], discoveryOptions);
assert.equal(listed.status, 0, listed.error?.message || listed.stderr);
const interruptedRequest = 'session::persistence::durable_update_tests::user_request_replay_survives_actor_abort_before_model_response: test';
assert(listed.stdout.includes(interruptedRequest), 'Refuse zero-test success or loss of the interrupted-request regression');
// libtest's ordinary --list also includes ignored tests. Presence alone does
// not prove that this critical regression will execute in the default suite.
const ignored = spawnSync(program, ['--ignored', '--list'], discoveryOptions);
assert.equal(ignored.status, 0, ignored.error?.message || ignored.stderr);
assert(!ignored.stdout.includes(interruptedRequest), 'The interrupted-request regression must execute, not remain ignored');
const testNames = output => output.split('\n').filter(line => line.endsWith(': test')).map(line => line.slice(0, -6));
const allTests = new Set(testNames(listed.stdout));
const ignoredTests = new Set(testNames(ignored.stdout));
const actorFilter = 'session::acp_session::';
function groupEnvironment(label) {
  const groupHome = realpathSync(mkdtempSync(join(home, `${label}-`)));
  const groupLegacy = join(groupHome, 'empty-legacy'); mkdirSync(groupLegacy, { mode: 0o700 });
  return { ...env, HOME: groupHome, USERPROFILE: groupHome, ECHO_AGENT_HOME: groupHome, GROK_HOME: groupLegacy,
    XDG_CONFIG_HOME: join(groupHome, 'config'), XDG_DATA_HOME: join(groupHome, 'data'), XDG_CACHE_HOME: join(groupHome, 'cache'),
    ...(process.platform === 'win32' ? { APPDATA: join(groupHome, 'AppData/Roaming'), LOCALAPPDATA: join(groupHome, 'AppData/Local') } : {}) };
}
const groups = [
  { name: 'Runtime library/default test stack', args: ['--skip', actorFilter], env: groupEnvironment('library') },
  // spawn_session_on_thread in acp_session_impl/spawn.rs uses 8 MiB for
  // SessionActor and its sampler. Keep all other tests on their default stack.
  // This child-process setting also affects unspecified worker-thread stacks;
  // native host tests separately retain the ordinary worker environment.
  { name: 'SessionActor/production session stack', args: [actorFilter], env: { ...groupEnvironment('actor'), RUST_MIN_STACK: String(8 * 1024 * 1024) } },
];
const covered = new Set();
for (const group of groups) {
  const discovery = spawnSync(program, [...group.args, '--list'], { ...discoveryOptions, env: group.env });
  assert.equal(discovery.status, 0, discovery.error?.message || discovery.stderr);
  const names = testNames(discovery.stdout);
  assert(names.some(name => !ignoredTests.has(name)), `${group.name} must execute real tests`);
  for (const name of names) {
    assert(allTests.has(name) && !covered.has(name), 'Runtime test groups must be disjoint subsets of the complete harness');
    covered.add(name);
  }
  console.log(`${group.name}: ${names.filter(name => !ignoredTests.has(name)).length} active, ${names.filter(name => ignoredTests.has(name)).length} ignored`);
}
assert.equal(covered.size, allTests.size, 'Every discovered Runtime test must belong to exactly one execution group');
let status = 0;
for (const group of groups) {
  console.log(`Running ${group.name}`);
  const result = await runNativeTestProcess(program, [...group.args, '--test-threads=2'], { ...options, env: group.env });
  if (result !== 0) status = result;
}
process.exitCode = status;
