import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { chmodSync, mkdirSync, mkdtempSync, realpathSync, rmSync, symlinkSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { confirmNativeCoreWorkspace, confirmNativeReviewWorkspace } from './lib/native-core-workspace-picker.mjs';

describe.skipIf(process.platform !== 'darwin')('native Core workspace confirmation scope', () => {
  let home, cwd, scope, run;
  beforeEach(() => {
    home = realpathSync(mkdtempSync(join(tmpdir(), 'echo-native-review-core-picker-')));
    chmodSync(home, 0o700);
    cwd = join(home, 'workspaces', 'default');
    mkdirSync(cwd, { recursive: true, mode: 0o700 });
    scope = { pid: 24680, home, cwd };
    run = vi.fn(() => JSON.stringify({ selected: true, pid: scope.pid, cwd }));
  });
  afterEach(() => rmSync(home, { recursive: true, force: true }));

  it.each([0, 1, -1, 1.5, '24680'])('rejects invalid PID %s before any native action', pid => {
    expect(() => confirmNativeCoreWorkspace({ ...scope, pid }, run)).toThrow();
    expect(run).not.toHaveBeenCalled();
  });
  it('never confirms another workspace or a default user profile', () => {
    for (const candidate of [{ ...scope, cwd: `${cwd}-neighbor` }, { ...scope, home: '/Users/person/.echo-agent' }]) {
      expect(() => confirmNativeCoreWorkspace(candidate, run)).toThrow();
    }
    expect(run).not.toHaveBeenCalled();
  });
  it('rejects non-private and symbolic workspaces before native action', () => {
    chmodSync(cwd, 0o755);
    expect(() => confirmNativeCoreWorkspace(scope, run)).toThrow();
    rmSync(cwd, { recursive: true });
    symlinkSync(home, cwd);
    expect(() => confirmNativeCoreWorkspace(scope, run)).toThrow();
    expect(run).not.toHaveBeenCalled();
  });
  it('waits without claiming success when the owned picker is absent', () => {
    run.mockReturnValue(JSON.stringify({ waiting: true }));
    expect(confirmNativeCoreWorkspace(scope, run)).toBe(false);
  });
  it('binds a result to the owned PID and exact private directory', () => {
    expect(confirmNativeCoreWorkspace(scope, run)).toBe(true);
    expect(run.mock.calls[0][0]).toBe('/usr/bin/osascript');
    expect(run.mock.calls[0][2].timeout).toBe(10000);
    for (const result of [{ selected: true, pid: 999, cwd }, { selected: true, pid: scope.pid, cwd: '/' }, { selected: false, ...scope }]) {
      run.mockReturnValue(JSON.stringify(result));
      expect(() => confirmNativeCoreWorkspace(scope, run)).toThrow();
    }
  });
  it('preserves native errors and rejects malformed output', () => {
    run.mockImplementation(() => { throw new Error('Native access denied'); });
    expect(() => confirmNativeCoreWorkspace(scope, run)).toThrow('Native access denied');
    run.mockReturnValue('{');
    expect(() => confirmNativeCoreWorkspace(scope, run)).toThrow();
  });
  it('keeps each journey bound to its own home family', () => {
    for (const kind of ['browser', 'controls', 'collaboration', 'extensions', 'unknown']) {
      expect(() => confirmNativeReviewWorkspace({ ...scope, kind }, run)).toThrow();
    }
    expect(run).not.toHaveBeenCalled();
  });
  it.each(['browser', 'controls', 'collaboration', 'extensions'])('confirms only the fixed private %s root', kind => {
    const other = realpathSync(mkdtempSync(join(tmpdir(), `echo-native-review-${kind}-picker-`)));
    const directory = join(other, 'workspaces/default');
    try {
      chmodSync(other, 0o700); mkdirSync(directory, { recursive: true, mode: 0o700 });
      run.mockReturnValue(JSON.stringify({ selected: true, pid: scope.pid, cwd: directory }));
      expect(confirmNativeReviewWorkspace({ pid: scope.pid, home: other, cwd: directory, kind }, run)).toBe(true);
      expect(() => confirmNativeCoreWorkspace({ pid: scope.pid, home: other, cwd: directory }, run)).toThrow();
    } finally { rmSync(other, { recursive: true, force: true }); }
  });
});
