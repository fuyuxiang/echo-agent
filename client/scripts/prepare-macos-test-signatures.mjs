// macOS Cargo test executables can be very large and unsigned. Retain all
// test assertions/deadlines; sign only this workspace's own compiled harnesses.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { lstatSync, realpathSync } from 'node:fs';
import { basename, dirname, isAbsolute, relative, resolve, sep } from 'node:path';
import { pathToFileURL } from 'node:url';

export function ownedTestExecutables(messages, workspace, targetDirectory) {
  const inside = (root, file) => {
    const rel = relative(root, file);
    return rel !== '' && rel !== '..' && !rel.startsWith('..' + sep) && !isAbsolute(rel);
  };
  const paths = new Set();
  for (const message of messages) {
    if (message.reason !== 'compiler-artifact' || !message.executable) continue;
    // Cargo also emits the ordinary application needed by integration tests.
    // It is not a test harness and must never be signed by this helper.
    if (message.profile?.test !== true) continue;
    assert(inside(workspace, message.target?.src_path ?? ''), 'Refuse another workspace');
    assert(inside(targetDirectory, message.executable), 'Refuse output outside Cargo target');
    const executableName = basename(message.executable);
    const testHash = /^echoagent-([a-f0-9]{16})$/.exec(executableName)?.[1];
    const outputDirectory = dirname(message.executable);
    const hashDirectory = dirname(outputDirectory);
    const crateDirectory = dirname(hashDirectory);
    // Cargo's newer layout places harnesses beside their build output rather
    // than deps/. Require the exact crate and matching hash in both positions.
    const harnessDirectory = basename(outputDirectory) === 'deps'
      || (basename(outputDirectory) === 'out'
        && basename(hashDirectory) === testHash
        && basename(crateDirectory) === 'echoagent'
        && basename(dirname(crateDirectory)) === 'build');
    const binaryTest = message.target.kind?.includes('bin')
      && message.target.name === 'echoagent'
      && message.target.src_path === resolve(workspace, 'src/main.rs')
      && harnessDirectory && Boolean(testHash);
    assert(message.target.kind?.some(kind => ['lib', 'rlib', 'staticlib', 'cdylib', 'test'].includes(kind)) || binaryTest,
      `Refuse application binary: ${JSON.stringify({ name: message.target.name, kind: message.target.kind, src: message.target.src_path, executable: message.executable })}`);
    paths.add(message.executable);
  }
  // Library + main harness + 15 integration harnesses, including session_continuation.
  assert(paths.size > 0 && paths.size <= 17, `Unexpected test artifact count: ${paths.size}`);
  return [...paths];
}

export function prepareMacosTestSignatures(env = process.env) {
  if (process.platform !== 'darwin') return { signed: [], platform: process.platform };
  const workspace = realpathSync(resolve('src-tauri'));
  const metadata = spawnSync('cargo', ['metadata', '--locked', '--no-deps', '--format-version', '1',
    '--manifest-path', 'src-tauri/Cargo.toml'], { env, encoding: 'utf8', maxBuffer: 16 * 1024 * 1024 });
  assert.equal(metadata.status, 0, metadata.stderr || 'Cargo metadata failed');
  const targetPath = JSON.parse(metadata.stdout).target_directory;
  const build = spawnSync('cargo', ['test', '--locked', '--manifest-path', 'src-tauri/Cargo.toml',
    '--lib', '--tests', '-j', '2', '--no-run', '--message-format=json'],
  // Keep artifact JSON private to this parser while showing compilation and
  // lock-wait progress immediately, including during long cold builds.
  { env, encoding: 'utf8', maxBuffer: 32 * 1024 * 1024, stdio: ['ignore', 'pipe', 'inherit'] });
  const diagnostics = [];
  if (build.status !== 0) {
    for (const line of (build.stdout ?? '').split('\n').filter(Boolean)) {
      try {
        const message = JSON.parse(line);
        if (message.reason === 'compiler-message' && message.message?.rendered) {
          process.stderr.write(message.message.rendered);
          diagnostics.push(message.message.rendered);
        }
      } catch { /* Cargo may emit an unstructured diagnostic on failure. */ }
    }
  }
  assert.equal(build.status, 0, `Test compilation failed; no signing performed\n${build.error?.message ?? diagnostics.join('\n').slice(-2400)}`);
  // A fresh checkout has no target directory until Cargo creates it.
  const target = realpathSync(targetPath);
  const messages = build.stdout.split('\n').filter(Boolean).map(line => JSON.parse(line));
  const artifacts = ownedTestExecutables(messages, workspace, target);
  // Validate every path before mutating any artifact. Cargo supplies the paths;
  // neither caller arguments nor a directory glob can select another binary.
  for (const path of artifacts) {
    const stat = lstatSync(path);
    assert(stat.isFile() && !stat.isSymbolicLink() && stat.uid === process.getuid(), 'Unowned or symbolic artifact');
    assert.equal(realpathSync(path), path, 'Noncanonical artifact');
  }
  for (const path of artifacts) {
    const signed = spawnSync('/usr/bin/codesign', ['--force', '--sign', '-', path],
      { env, encoding: 'utf8', maxBuffer: 1024 * 1024 });
    assert.equal(signed.status, 0, signed.stderr || 'Local test signing failed');
    const verified = spawnSync('/usr/bin/codesign', ['--verify', path], { env, encoding: 'utf8' });
    assert.equal(verified.status, 0, verified.stderr || 'Local test signature invalid');
  }
  return { signed: artifacts, platform: process.platform, localAdHocOnly: true };
}
if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  assert.equal(process.argv.length, 2, 'No arbitrary signing paths are accepted');
  console.log(JSON.stringify(prepareMacosTestSignatures()));
}
