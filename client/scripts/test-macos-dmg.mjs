import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdtempSync, readFileSync, realpathSync, rmSync, symlinkSync, writeFileSync, existsSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { delimiter, dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';
import { bundleDmg, detachImage, mountedImages, rememberAttachment } from './macos-dmg.mjs';

function fixture(t) {
  const directory = realpathSync(mkdtempSync(join(tmpdir(), "echo dmg's test-")));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const image = join(directory, 'rw.123.EchoAgent_0.3.11_x64.dmg');
  writeFileSync(image, 'temporary disk image');
  const entity = { 'dev-entry': '/dev/disk42', 'mount-point': '/Volumes/echo-dmg-test' };
  const mounted = { 'image-path': image, 'system-entities': [entity] };
  return { directory, image, mounted };
}

for (const entry of ['direct', 'directory link', 'file link']) {
  test(`the ${entry} CLI entry executes bundling validation and detach ownership checks`, t => {
    const { directory } = fixture(t);
    const script = fileURLToPath(new URL('./macos-dmg.mjs', import.meta.url));
    let invoked = script;
    if (entry === 'directory link') {
      const alias = join(directory, 'linked scripts');
      symlinkSync(dirname(script), alias, process.platform === 'win32' ? 'junction' : 'dir');
      invoked = join(alias, 'macos-dmg.mjs');
    } else if (entry === 'file link') {
      invoked = join(directory, 'linked dmg.mjs');
      try { symlinkSync(script, invoked); } catch (error) {
        if (process.platform === 'win32' && error.code === 'EPERM') {
          t.skip('file symlinks require Windows Developer Mode or elevated privileges');
          return;
        }
        throw error;
      }
    }
    const statePath = join(directory, 'images.json');
    writeFileSync(statePath, JSON.stringify({ devices: {}, failedImages: [] }));
    const run = (...args) => spawnSync(process.execPath, [invoked, ...args], {
      encoding: 'utf8', timeout: 10_000, env: { ...process.env, ECHO_DMG_STATE: statePath },
    });
    // Both checks fail before any system utility or packaging command can run.
    const bundle = run('bundle', directory, 'invalid-filename.txt');
    assert.equal(bundle.status, 1, bundle.error?.message || bundle.stdout + bundle.stderr);
    assert.match(bundle.stderr, /Invalid DMG filename/);
    const detach = run('hdiutil', 'detach', '/dev/disk42');
    assert.equal(detach.status, 1, detach.error?.message || detach.stdout + detach.stderr);
    assert.match(detach.stderr, /Refusing to detach a disk not created by this build/);
  });
}

test('a DiskArbitration timeout retries the same owned image and can recover', t => {
  const { image, mounted } = fixture(t);
  const calls = [], delays = [], messages = [];
  assert.equal(detachImage(image, '/dev/disk42', {
    inventory: () => [mounted], sleep: ms => delays.push(ms), report: text => messages.push(text),
    run: (args, options) => {
      calls.push(args); assert.equal(options.timeout, 60_000);
      return calls.length === 1 ? { status: 1, stderr: 'hdiutil: detach: timeout for DiskArbitration expired' } : { status: 0 };
    },
  }), 0);
  assert.deepEqual(calls, [['detach', '/dev/disk42'], ['detach', '/dev/disk42']]);
  assert.deepEqual(delays, [1000]);
  assert.ok(messages.some(text => text.includes('Retrying detach')));
});

test('repeated busy errors use one final force attempt, then stop without restarting create-dmg retries', t => {
  const { image, mounted } = fixture(t);
  const calls = [], delays = [];
  assert.equal(detachImage(image, '/dev/disk42', {
    inventory: () => [mounted], sleep: ms => delays.push(ms), report: () => {},
    run: args => { calls.push(args); return { status: 16, stderr: 'Resource busy' }; },
  }), 1);
  assert.equal(calls.length, 4);
  assert.deepEqual(calls.at(-1), ['detach', '-force', '/dev/disk42']);
  assert.deepEqual(delays, [1000, 2000, 3000]);
});

test('a killed detach request is bounded and retries just like an explicit timeout', t => {
  const { image, mounted } = fixture(t);
  let calls = 0;
  assert.equal(detachImage(image, '/dev/disk42', {
    inventory: () => [mounted], sleep: () => {}, report: () => {},
    run: () => ++calls === 1 ? { status: null, error: { code: 'ETIMEDOUT', message: 'timed out' } } : { status: 0 },
  }), 0);
  assert.equal(calls, 2);
});

test('an unrelated failure is preserved without retries or force', t => {
  const { image, mounted } = fixture(t);
  let calls = 0;
  assert.equal(detachImage(image, '/dev/disk42', {
    inventory: () => [mounted], sleep: () => assert.fail('must not retry'), report: () => {},
    run: () => { calls++; return { status: 7, stderr: 'permission denied' }; },
  }), 7);
  assert.equal(calls, 1);
});

test('a request that completes after timing out succeeds without detaching again', t => {
  const { image, mounted } = fixture(t);
  let checks = 0, calls = 0;
  assert.equal(detachImage(image, '/dev/disk42', {
    inventory: () => checks++ === 0 ? [mounted] : [], report: () => {},
    run: () => { calls++; return { status: 1, stderr: 'timeout' }; },
  }), 0);
  assert.equal(calls, 1);
});

test('a device reused for another image is never retried or forced', t => {
  const { directory, image, mounted } = fixture(t);
  const other = join(directory, 'personal.dmg'); writeFileSync(other, 'unrelated');
  let checks = 0, calls = 0;
  assert.throws(() => detachImage(image, '/dev/disk42', {
    inventory: () => [checks++ === 0 ? mounted : { ...mounted, 'image-path': other }], report: () => {},
    run: () => { calls++; return { status: 1, stderr: 'DiskArbitration timeout' }; },
  }), /Refusing to detach/);
  assert.equal(calls, 1);
});

test('unreadable inventory fails closed, while already detached and deleted images need no action', t => {
  const { image } = fixture(t);
  assert.throws(() => detachImage(image, '/dev/disk42', {
    inventory: () => { throw new Error('cannot inspect disks'); }, run: () => assert.fail('must not detach'),
  }), /cannot inspect disks/);
  rmSync(image);
  assert.equal(detachImage(image, '/dev/disk42', {
    inventory: () => [], run: () => assert.fail('must not detach'),
  }), 0);
  assert.throws(() => mountedImages(() => ({ status: 1 }), () => ({})), /Cannot inspect/);
  assert.throws(() => mountedImages(() => ({ status: 0 }), () => ({})), /Invalid disk image/);
  assert.throws(() => mountedImages(() => ({ status: 0 }), () => ({ images: [{}] })), /Invalid disk image/);
});

test('ownership records only regular staging images inside this build directory', t => {
  const { directory, image } = fixture(t);
  const state = {}, name = 'EchoAgent_0.3.11_x64.dmg';
  rememberAttachment(['attach', image], '/dev/disk42\tGUID_partition_scheme\n/dev/disk42s1\tApple_HFS\n', state, directory, name);
  assert.deepEqual(state, { '/dev/disk42': image, '/dev/disk42s1': image });
  for (const [path, dir, expected] of [[image, tmpdir(), name], [image, directory, 'another.dmg']]) {
    const rejected = {}; rememberAttachment(['attach', path], '/dev/disk99\tApple_HFS\n', rejected, dir, expected);
    assert.deepEqual(rejected, {});
  }
  const link = join(directory, 'rw.999.EchoAgent_0.3.11_x64.dmg');
  try { symlinkSync(image, link); } catch (error) {
    if (process.platform === 'win32' && error.code === 'EPERM') return;
    throw error;
  }
  const rejected = {}; rememberAttachment(['attach', link], '/dev/disk99\tApple_HFS\n', rejected, directory, name);
  assert.deepEqual(rejected, {});
});

test('verification mount paths are compared canonically, including symlinked temporary directories', t => {
  const { directory, image, mounted } = fixture(t);
  const alias = join(directory, 'mount-alias');
  symlinkSync(directory, alias, process.platform === 'win32' ? 'junction' : 'dir');
  let calls = 0;
  assert.equal(detachImage(image, alias, {
    inventory: () => [{ ...mounted, 'system-entities': [{ 'mount-point': directory }] }],
    run: () => { calls++; return { status: 0 }; },
  }), 0);
  assert.equal(calls, 1);
});

test('cleanup failure cannot turn a failed bundle into success or conceal a leaked mount', t => {
  const { directory, image } = fixture(t);
  for (const status of [0, 7]) {
    assert.equal(bundleDmg(directory, 'EchoAgent_0.3.11_x64.dmg', {
      run: (_command, _args, options) => {
        writeFileSync(options.env.ECHO_DMG_STATE, JSON.stringify({ devices: { '/dev/disk42': image }, failedImages: [] }));
        return { status };
      }, cleanup: () => 1,
    }), status || 1);
  }
});

test('bundling scopes its utility shim, preserves failures, cleans up and never recompiles', t => {
  const { directory, image } = fixture(t);
  for (const status of [0, 7]) {
    let shimDir, cleaned = 0;
    assert.equal(bundleDmg(directory, 'EchoAgent_0.3.11_x64.dmg', {
      run: (command, args, options) => {
        assert.equal(command, 'pnpm');
        assert.deepEqual(args, ['tauri', 'bundle', '--bundles', 'dmg', '--verbose']);
        shimDir = options.env.PATH.split(delimiter)[0];
        assert.match(readFileSync(join(shimDir, 'hdiutil'), 'utf8'), /hdiutil "\$@"/);
        writeFileSync(options.env.ECHO_DMG_STATE, JSON.stringify({ devices: { '/dev/disk42': image }, failedImages: [] }));
        return { status };
      },
      cleanup: (owned, target) => { assert.equal(owned, image); assert.equal(target, '/dev/disk42'); cleaned++; return 0; },
    }), status);
    assert.equal(cleaned, 1);
    assert.equal(existsSync(shimDir), false);
  }
});

test('bundle cleanup does not restart an exhausted detach retry loop or retry each partition', t => {
  const { directory, image } = fixture(t);
  let calls = 0;
  assert.equal(bundleDmg(directory, 'EchoAgent_0.3.11_x64.dmg', {
    run: (_command, _args, options) => {
      writeFileSync(options.env.ECHO_DMG_STATE, JSON.stringify({
        devices: { '/dev/disk42': image, '/dev/disk42s1': image }, failedImages: [image],
      }));
      return { status: 1 };
    }, cleanup: () => { calls++; return 0; },
  }), 1);
  assert.equal(calls, 0);
});
