import { spawnSync } from 'node:child_process';
import { existsSync, lstatSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { basename, delimiter, dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const script = fileURLToPath(import.meta.url);
const hdiutil = '/usr/bin/hdiutil';
const pause = ms => Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ms);
const statusOf = result => Number.isInteger(result.status) ? result.status : 1;
const quote = value => `'${value.replaceAll("'", "'\"'\"'")}'`;
const canonical = path => existsSync(path) ? realpathSync(path) : resolve(path);

function command(args, options = {}) {
  return spawnSync(hdiutil, args, { encoding: 'utf8', maxBuffer: 8 * 1024 * 1024, ...options });
}

export function mountedImages(run = command, convert = text => {
  const result = spawnSync('/usr/bin/plutil', ['-convert', 'json', '-o', '-', '--', '-'],
    { input: text, encoding: 'utf8', timeout: 10_000, maxBuffer: 8 * 1024 * 1024 });
  if (result.status !== 0) throw new Error(`Cannot read disk image inventory: ${result.stderr || result.error}`);
  return JSON.parse(result.stdout);
}) {
  const result = run(['info', '-plist'], { timeout: 10_000 });
  if (result.status !== 0) throw new Error(`Cannot inspect mounted disk images: ${result.stderr || result.error}`);
  const data = convert(result.stdout);
  if (!Array.isArray(data.images) || data.images.some(item => typeof item['image-path'] !== 'string'
    || !Array.isArray(item['system-entities']))) throw new Error('Invalid disk image inventory');
  return data.images;
}

// A device number can be reused after a timeout. Recheck its image identity
// before every attempt, including the final forced detach of our own image.
export function detachImage(image, target, {
  run = command, inventory = () => mountedImages(), sleep = pause,
  report = text => process.stderr.write(`${text}\n`),
} = {}) {
  const current = () => {
    const matches = inventory().filter(item => item['system-entities']?.some(entity =>
      entity['dev-entry'] === target || typeof entity['mount-point'] === 'string'
        && canonical(entity['mount-point']) === canonical(target)));
    if (matches.length === 0) return false;
    if (matches.length !== 1 || !matches[0]['image-path']
      || realpathSync(matches[0]['image-path']) !== realpathSync(image)) {
      throw new Error(`Refusing to detach a different disk image at ${target}`);
    }
    return true;
  };
  for (let attempt = 0; attempt < 4; attempt++) {
    if (!current()) return 0; // A timed-out request may already have detached it.
    const force = attempt === 3;
    const result = run(['detach', ...(force ? ['-force'] : []), target], { timeout: 60_000 });
    if (result.stdout) process.stdout.write(result.stdout);
    if (result.stderr) report(result.stderr.trimEnd());
    if (result.error) report(result.error.message);
    if (result.status === 0) return 0;
    if (!current()) return 0;
    const transient = result.status === 16 || result.error?.code === 'ETIMEDOUT'
      || /DiskArbitration|timeout|timed out|resource busy/i.test(result.stderr || '');
    if (!transient) return statusOf(result);
    // Do not return EBUSY: create-dmg would start another retry loop around us.
    if (force) {
      report(`Could not detach this build's disk image after four attempts: ${target}`);
      return 1;
    }
    report(`Retrying detach of this build's disk image (${attempt + 1}/3): ${target}`);
    sleep(1_000 * (attempt + 1));
  }
  return 1;
}

export function rememberAttachment(args, output, state, sourceDir, dmgName) {
  // Only create-dmg's newly created writable staging image is eligible.
  const path = args.at(-1);
  if (!path || !existsSync(path) || !lstatSync(path).isFile() || lstatSync(path).isSymbolicLink()) return;
  const image = realpathSync(path);
  const prefix = basename(image).match(/^rw\.(\d+)\.(.+)$/);
  if (!prefix || prefix[2] !== dmgName || dirname(image) !== realpathSync(sourceDir)) return;
  for (const match of output.matchAll(/^(\/dev\/disk\d+(?:s\d+)*)\s/gm)) state[match[1]] = image;
}

function forwardHdiutil(args) {
  const statePath = process.env.ECHO_DMG_STATE;
  if (!statePath) throw new Error('Missing isolated DMG build state');
  const state = JSON.parse(readFileSync(statePath, 'utf8'));
  if (args[0] === 'detach') {
    const target = args[1];
    if (args.length !== 2 || !Object.hasOwn(state.devices, target)) {
      throw new Error('Refusing to detach a disk not created by this build');
    }
    let status = 1;
    try {
      status = detachImage(state.devices[target], target);
      return status;
    } finally {
      if (status !== 0) {
        state.failedImages.push(state.devices[target]);
        writeFileSync(statePath, JSON.stringify(state));
      }
    }
  }
  if (args[0] !== 'attach') return statusOf(command(args, { stdio: 'inherit' }));
  const result = command(args);
  if (result.stdout) process.stdout.write(result.stdout);
  if (result.stderr) process.stderr.write(result.stderr);
  if (result.status === 0) {
    rememberAttachment(args, result.stdout, state.devices, process.env.ECHO_DMG_SOURCE, process.env.ECHO_DMG_NAME);
    writeFileSync(statePath, JSON.stringify(state));
  }
  return statusOf(result);
}

export function bundleDmg(sourceDir, dmgName, {
  root = resolve(dirname(script), '..'), run = spawnSync, cleanup = detachImage,
} = {}) {
  if (!dmgName || basename(dmgName) !== dmgName || !dmgName.endsWith('.dmg')) throw new Error('Invalid DMG filename');
  const tools = mkdtempSync(join(tmpdir(), 'echo-dmg-tools-'));
  const statePath = join(tools, 'images.json');
  writeFileSync(statePath, JSON.stringify({ devices: {}, failedImages: [] }));
  // Scope the shim to this one Tauri command; never replace a system utility.
  writeFileSync(join(tools, 'hdiutil'), `#!/bin/sh\nexec ${quote(process.execPath)} ${quote(script)} hdiutil "$@"\n`, { mode: 0o700 });
  let status = 1;
  try {
    const result = run('pnpm', ['tauri', 'bundle', '--bundles', 'dmg', '--verbose'], {
      cwd: root, stdio: 'inherit', env: { ...process.env,
        PATH: `${tools}${delimiter}${process.env.PATH || ''}`, ECHO_DMG_STATE: statePath,
        ECHO_DMG_SOURCE: resolve(sourceDir), ECHO_DMG_NAME: dmgName },
    });
    status = statusOf(result);
    if (result.error) console.error(result.error.message);
  } finally {
    try {
      const state = JSON.parse(readFileSync(statePath, 'utf8'));
      const cleanedImages = new Set(state.failedImages);
      if (state.failedImages.length) {
        console.error(`Automatic detach recovery stopped for: ${state.failedImages.join(', ')}`);
        status = status || 1;
      }
      for (const [target, image] of Object.entries(state.devices)) {
        if (cleanedImages.has(image)) continue;
        cleanedImages.add(image);
        try {
          if (cleanup(image, target) !== 0) status = status || 1;
        } catch (error) {
          console.error(`Cannot clean up this build's disk image: ${error.message}`);
          status = status || 1;
        }
      }
    } finally {
      rmSync(tools, { recursive: true, force: true });
    }
  }
  return status;
}

// Node resolves import.meta.url to the real file, while argv can retain a
// checkout or script symlink. Compare real paths so the CLI cannot silently skip.
if (process.argv[1] && canonical(process.argv[1]) === canonical(script)) {
  try {
    const [mode, ...args] = process.argv.slice(2);
    if (mode === 'hdiutil') process.exitCode = forwardHdiutil(args);
    else if (mode === 'bundle' && args.length === 2) process.exitCode = bundleDmg(...args);
    else if (mode === 'detach' && args.length === 2) process.exitCode = detachImage(...args);
    else throw new Error('Usage: node scripts/macos-dmg.mjs bundle <staging-dir> <dmg-name> | detach <image> <device-or-mount>');
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
