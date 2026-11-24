import { cleanNativeReviewEnvironment } from "./lib/native-review-isolation.mjs";
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import { constants, copyFileSync, createReadStream, existsSync, readFileSync, writeFileSync, mkdirSync, renameSync, realpathSync, lstatSync, statSync, linkSync, unlinkSync } from 'node:fs';
import { resolve, join } from 'node:path';

// A reversible verification wrapper, run only after Cargo and staging finish.
// Hide exactly the four development entry files to exercise bundle fallbacks.
const repository = process.cwd();
const executable = resolve(process.argv[2]);
const resources = resolve(executable, '../../Resources');
assert.ok(process.env.PACKAGE_REVIEW_OUTPUT, 'Set PACKAGE_REVIEW_OUTPUT to the evidence directory');
const output = resolve(process.env.PACKAGE_REVIEW_OUTPUT);
assert.ok(existsSync(executable));
mkdirSync(output, { recursive: true });
const journal = [];
const id = `echo-review-${Date.now()}`;
const digest = async (path) => {
  const hash = createHash('sha256');
  for await (const chunk of createReadStream(path)) hash.update(chunk);
  return hash.digest('hex');
};
let child;
let interrupted = false;
const signal = () => { interrupted = true; child?.kill('SIGTERM'); };
process.once('SIGINT', signal);
process.once('SIGTERM', signal);
const run = (command, args, env, timeout) => new Promise((accept, reject) => {
  if (interrupted) return reject(new Error('Package validation interrupted'));
  child = spawn(command, args, { env, stdio: 'inherit' });
  const processChild = child;
  const timer = setTimeout(() => { processChild.kill('SIGTERM'); }, timeout);
  const force = setTimeout(() => { processChild.kill('SIGKILL'); }, timeout + 3000);
  processChild.once('error', (error) => { clearTimeout(timer); clearTimeout(force); child = null; reject(error); });
  processChild.once('exit', (code, termination) => {
    clearTimeout(timer); clearTimeout(force); child = null;
    if (interrupted) reject(new Error('Package validation interrupted'));
    else if (termination) reject(new Error(`Validation process terminated: ${termination}`));
    else accept(code);
  });
});
const result = { executable, binarySha256: await digest(executable), resources, checks: [], stagedFiles: journal, errors: [] };
try {
  for (const relative of ['office/node/bin/node', 'office/node/LICENSE', 'office/worker.mjs', 'office/fonts/NotoSansCJKsc-Regular.otf']) {
    assert.equal(interrupted, false, 'Package validation interrupted');
    const source = join(repository, 'src-tauri/resources', relative);
    const bundled = join(resources, relative);
    assert.ok(existsSync(source) && existsSync(bundled), `Resource missing: ${relative}`);
    assert.ok(lstatSync(source).isFile() && !lstatSync(source).isSymbolicLink(), `Staged entry must be an ordinary file: ${relative}`);
    const backup = `${source}.${id}`;
    assert.equal(existsSync(backup), false);
    const entry = { relative, source, backup, originalMode: statSync(source).mode & 0o7777,
      originalSha256: await digest(source), bundledSha256: await digest(bundled), restored: false };
    assert.equal(interrupted, false, 'Package validation interrupted');
    journal.push(entry);
    writeFileSync(join(output, 'package-resource-review.json'), JSON.stringify(result, null, 2));
    renameSync(source, backup);
    entry.hidden = true;
    writeFileSync(join(output, 'package-resource-review.json'), JSON.stringify(result, null, 2));
  }
  const env = { ...cleanNativeReviewEnvironment(process.env) };
  const nativeCode = await run(process.execPath, [join(repository, 'scripts/validate-native-review.mjs'), executable], {
    ...env, UI_REVIEW_OUTPUT: join(output, 'native'),
  }, 1550000);
  result.checks.push({ id: 'native-bundled-journey', exitCode: nativeCode, passed: nativeCode === 0 });
  const officeCode = await run(process.execPath, [join(repository, 'scripts/validate-packaged-desktop.mjs'), executable], {
    ...env, ECHO_DESKTOP_VALIDATION_OUTPUT: join(output, 'startup'),
  }, 180000);
  result.checks.push({ id: 'bundled-webview-and-office', exitCode: officeCode, passed: officeCode === 0 });
} catch (error) {
  result.errors.push(String(error));
} finally {
  for (const entry of [...journal].reverse()) {
    if (!entry.hidden) continue;
    const restoredCopy = `${entry.backup}.restore-copy`;
    let createdCopy = false;
    try {
      assert.equal(existsSync(entry.source), false, `Source reappeared; preserving both files: ${entry.source}`);
      assert.equal(await digest(entry.backup), entry.originalSha256, `Backup changed: ${entry.backup}`);
      // Prepare the entire independent copy under a private name, then publish
      // it atomically without replacing a concurrent source. An in-place stage
      // rebuild cannot race the copying or mutate the original recovery backup.
      copyFileSync(entry.backup, restoredCopy, constants.COPYFILE_EXCL);
      createdCopy = true;
      linkSync(restoredCopy, entry.source);
      assert.equal(await digest(entry.source), entry.originalSha256, `Published resource changed: ${entry.source}`);
      assert.equal(statSync(entry.source).mode & 0o7777, entry.originalMode, `Resource mode changed: ${entry.source}`);
      unlinkSync(entry.backup);
      entry.restored = true;
    } catch (error) { result.errors.push(String(error)); }
    finally {
      if (createdCopy) {
        try { unlinkSync(restoredCopy); } catch (error) { result.errors.push(String(error)); }
      }
    }
  }
  result.passed = result.errors.length === 0 && result.checks.length === 2 && result.checks.every((check) => check.passed)
    && journal.length === 4 && journal.every((entry) => entry.restored);
  writeFileSync(join(output, 'package-resource-review.json'), JSON.stringify(result, null, 2));
  console.log(JSON.stringify({ passed: result.passed, checks: result.checks, errors: result.errors,
    stagedRestored: journal.every((entry) => entry.restored), journal: join(output, 'package-resource-review.json') }));
  if (!result.passed) process.exitCode = 1;
}
