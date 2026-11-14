import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { lstatSync, realpathSync } from 'node:fs';

// Confirm only the runner-owned native picker. The production component opens
// it at the fixed workspace; the renderer and parent separately verify the
// exact selected path and persisted native grant before qualifying a pass.
export function confirmNativeCoreWorkspace({ pid, home, cwd }, run = execFileSync) {
  return confirmNativeReviewWorkspace({ pid, home, cwd, kind: 'core' }, run);
}

export function confirmNativeReviewWorkspace({ pid, home, cwd, kind }, run = execFileSync) {
  assert(Number.isSafeInteger(pid) && pid > 1, 'Invalid owned application PID');
  assert(['core', 'browser', 'controls', 'collaboration', 'extensions', 'knowledge'].includes(kind), 'Unknown fixed native journey');
  assert(new RegExp(`^/private/(?:tmp|var/folders/[A-Za-z0-9_-]+/[A-Za-z0-9_-]+/T)/echo-native-review-${kind}-[A-Za-z0-9_-]+$`).test(home), 'Invalid private journey home');
  assert.equal(cwd, `${home}/workspaces/default`, 'Only the fixed private workspace may be selected');
  for (const path of [home, cwd]) {
    const info = lstatSync(path);
    assert(info.isDirectory() && !info.isSymbolicLink() && info.uid === process.getuid()
      && !(info.mode & 0o077) && realpathSync(path) === path, 'Picker workspace must be ordinary, private and owned');
  }
  const script = `JSON.stringify((() => {
    // Do not evaluate process objects into name-based references: two apps
    // named echoagent otherwise both resolve to the first running instance.
    const processes = Application('System Events').applicationProcesses;
    const processIndex = processes.unixId().indexOf(${pid});
    if (processIndex < 0) return {waiting:true};
    const p = processes.at(processIndex);
    if (p.unixId() !== ${pid}) throw new Error('Owned PID changed during picker lookup');
    if (p.name().toLowerCase() !== 'echoagent') throw new Error('Owned PID is not EchoAgent');
    const panel = () => p.windows().flatMap(w => [w,...w.sheets()]).find(w =>
      w.role() === 'AXSheet' && w.staticTexts().some(t => t.name() === '选择 EchoAgent 可访问的文件夹'));
    const current = panel();
    if (!current) return {waiting:true};
    p.frontmost = true; delay(.3);
    if (!p.frontmost() || !current.popUpButtons().some(b => b.value() === 'default'))
      throw new Error('Owned picker is not at the proposed workspace');
    const open = current.buttons().find(b => ['Open','打开','选择'].includes(b.name()) && b.enabled());
    if (!open) throw new Error('Owned picker confirmation unavailable');
    try { open.click(); } catch (error) {
      if (!/无效的索引|Invalid index|-1719/.test(String(error))) throw error;
      delay(.2); if (panel()) throw error;
    }
    return {selected:true,pid:${pid},cwd:${JSON.stringify(cwd)}};
  })())`;
  const result = JSON.parse(run('/usr/bin/osascript', ['-l', 'JavaScript', '-e', script], { encoding: 'utf8', timeout: 10000 }).trim());
  if (result.waiting === true && result.selected !== true) return false;
  assert(result.selected === true && result.pid === pid && result.cwd === cwd, 'Unbound native picker result');
  return true;
}
