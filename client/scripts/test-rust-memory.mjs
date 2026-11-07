// Run the dependency crate's own tests; host tests do not execute those suites.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { lstatSync, mkdirSync, mkdtempSync, realpathSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { basename, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { cleanNativeReviewEnvironment, withNativeReviewLoopbackBypass } from './lib/native-review-isolation.mjs';
import { ownedTestExecutables } from './prepare-macos-test-signatures.mjs';
import { runNativeTestProcess } from './lib/native-test-process.mjs';

assert.equal(process.argv.length, 2, 'This entry runs the complete memory library suite');
const root = realpathSync(fileURLToPath(new URL('../', import.meta.url)));
const workspace = realpathSync(join(root, 'vendor/echo-agent-build/crates/codegen/echo-agent-memory'));
const targetPath = join(root, 'src-tauri/target');
const home = realpathSync(mkdtempSync(join(tmpdir(), 'echo-memory-tests-')));
const legacy = join(home, 'empty-legacy'); mkdirSync(legacy, { mode: 0o700 });
const env = withNativeReviewLoopbackBypass({ ...cleanNativeReviewEnvironment(process.env),
  ECHO_AGENT_HOME: home, GROK_HOME: legacy, CARGO_TARGET_DIR: targetPath });
console.log(`Isolated memory library test data: ${home}`);
const build = spawnSync('cargo', ['test', '--locked', '--manifest-path', join(root, 'vendor/echo-agent-build/Cargo.toml'),
  '-p', 'echo-agent-memory', '--lib', '-j', '2', '--no-run', '--message-format=json'],
{ cwd: root, env, encoding: 'utf8', maxBuffer: 32 * 1024 * 1024, stdio: ['ignore', 'pipe', 'inherit'] });
const messages = (build.stdout ?? '').split('\n').filter(Boolean).map(line => {
  try { return JSON.parse(line); } catch { return {}; }
});
for (const message of messages) {
  if (message.reason === 'compiler-message' && message.message?.rendered) process.stderr.write(message.message.rendered);
}
assert.equal(build.status, 0, build.error?.message || 'Memory library test compilation failed');
const programs = ownedTestExecutables(messages, workspace, realpathSync(targetPath));
assert.equal(programs.length, 1, 'Expected exactly the memory library harness');
const program = programs[0];
assert.match(basename(program), /^echo_agent_memory-[a-f0-9]{16}(?:\.exe)?$/);
assert.equal(realpathSync(program), resolve(program));
const stat = lstatSync(program);
assert(stat.isFile() && !stat.isSymbolicLink());
if (process.getuid) assert.equal(stat.uid, process.getuid());
if (process.platform === 'darwin') {
  for (const args of [['--force', '--sign', '-', program], ['--verify', program]]) {
    const signed = spawnSync('/usr/bin/codesign', args, { env, encoding: 'utf8' });
    assert.equal(signed.status, 0, signed.stderr || 'Local memory test signature failed');
  }
}
process.exitCode = await runNativeTestProcess(program, ['--test-threads=2'], { cwd: workspace, env });
