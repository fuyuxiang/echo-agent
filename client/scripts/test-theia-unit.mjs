// Fixed, Node-compatible contracts for the embedded IDE. Build the packages
// first; missing compiled specs must fail rather than become Mocha warnings.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { lstatSync, mkdirSync, mkdtempSync, readFileSync, realpathSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

assert.equal(process.argv.length, 2, 'This entry runs the complete declared IDE contract suite');
const root = realpathSync(fileURLToPath(new URL('../vendor/theia-platform/', import.meta.url)));
const specs = [
  'dev-packages/request/lib/node-request-service.spec.js',
  'dev-packages/request/lib/proxy.spec.js',
  'packages/ai-vercel-ai/lib/node/vercel-ai-language-model.spec.js',
  'packages/ai-chat-ui/lib/browser/chat-response-renderer/toolcall-part-renderer.spec.js',
  'packages/scm/lib/browser/dirty-diff/diff-computer.spec.js',
  'packages/core/lib/node/hosting/ws-origin-validator.spec.js',
  'packages/core/lib/node/hosting/browser-connection-token.spec.js',
  'packages/core/lib/browser/request/browser-request-service.spec.js',
  'packages/core/lib/common/path.spec.js',
  'packages/core/lib/common/uri.spec.js',
  'packages/core/lib/browser/markdown-rendering/markdown.spec.js',
  'packages/core/lib/common/message-rpc/rpc-message-encoder.spec.js',
  'packages/core/lib/common/message-rpc/uint8-array-message-buffer.spec.js',
  'packages/core/lib/common/message-rpc/channel.spec.js',
  'packages/filesystem/lib/common/files.spec.js',
  'packages/filesystem/lib/common/filesystem-utils.spec.js',
  'packages/filesystem/lib/node/file-change-collection.spec.js',
  'packages/file-search/lib/node/file-search-service-impl.spec.js',
  'packages/process/lib/node/raw-process.spec.js',
  'packages/process/lib/node/terminal-process.spec.js',
].map(spec => join(root, spec));
for (const spec of specs) {
  assert(lstatSync(spec).isFile(), `Missing compiled IDE contract: ${spec}`);
}
const outputRoot = resolve(process.env.THEIA_UNIT_OUTPUT_DIR || tmpdir());
mkdirSync(outputRoot, { recursive: true });
// A unique receipt cannot be supplied by a stale successful run.
const output = join(mkdtempSync(join(outputRoot, 'echo-theia-unit-')), 'results.json');
console.log(`IDE contract results: ${output}`);
const run = spawnSync(process.execPath, [join(root, 'node_modules/mocha/bin/mocha.js'),
  '--config', 'configs/mocharc.yml', '--forbid-only', '--fail-zero', '--timeout', '10000',
  '--reporter', 'json', '--reporter-option', `output=${output}`, ...specs],
{ cwd: root, env: process.env, stdio: 'inherit', timeout: 120000 });
assert.equal(run.status, 0, run.error?.message || `IDE contracts exited ${run.status}, signal ${run.signal}`);
const report = JSON.parse(readFileSync(output, 'utf8'));
assert(report.stats?.end && report.stats.passes > 0, 'IDE runner must finish with executed tests');
assert.equal(report.stats.failures, 0);
assert.equal(report.failures.length, 0);
for (const spec of specs) {
  assert(report.passes.some(test => resolve(root, test.file) === spec),
    `IDE contract file had no executed passing tests: ${spec}`);
}
console.log(`IDE contracts: ${report.stats.passes} passed, ${report.stats.failures} failed, ${report.stats.pending} pending across ${specs.length} required files`);
