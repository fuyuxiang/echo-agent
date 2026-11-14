// Real production CDP executor in an isolated headless browser. No user profile,
// user tabs, model inference, system input or native desktop permissions.
import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, realpathSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, basename } from 'node:path';
import { cleanNativeReviewEnvironment } from './lib/native-review-isolation.mjs';
import { prepareMacosTestSignatures } from './prepare-macos-test-signatures.mjs';
import { runNativeTestProcess } from './lib/native-test-process.mjs';
assert.equal(process.argv.length, 2);
const home = realpathSync(mkdtempSync(join(tmpdir(), 'echo-cdp-tests-')));
const legacy = join(home, 'empty-legacy');
mkdirSync(legacy, { mode: 0o700 });
const clean = cleanNativeReviewEnvironment(process.env);
const env = { ...clean, ECHO_AGENT_HOME: home, GROK_HOME: legacy,
  ECHO_BROWSER_TEST_HEADLESS: '1', NO_PROXY: '127.0.0.1,localhost,::1', no_proxy: '127.0.0.1,localhost,::1' };
const filter = 'automation::browser::tests::controlled_browser';
console.log(`Isolated CDP test home: ${home}`);
if (process.platform === 'darwin') {
  const prepared = prepareMacosTestSignatures(env);
  const executable = prepared.signed.find(file => basename(file).startsWith('echoagent_lib-'));
  assert(executable, 'No owned host library test executable');
  process.exitCode = await runNativeTestProcess(executable, [filter, '--ignored', '--test-threads=1', '--nocapture'],
    { cwd: realpathSync('src-tauri'), env });
} else {
  process.exitCode = await runNativeTestProcess('cargo', ['test', '--locked', '--manifest-path', 'src-tauri/Cargo.toml',
    '--lib', filter, '--', '--ignored', '--test-threads=1', '--nocapture'], { env });
}
