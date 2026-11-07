import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
import { execFileSync } from 'node:child_process';
import { lstatSync, realpathSync } from 'node:fs';
export function nativeLifecycleFolder({ pid, home, cwd, nonce, action }, run = execFileSync) {
  assert(Number.isSafeInteger(pid) && pid > 1);
  assert(/^\/private\/tmp\/echo-native-review-lifecycle-[A-Za-z0-9_-]+$/.test(home));
  assert(/^[a-f0-9]{32}$/.test(nonce));
  assert.equal(cwd, fileURLToPath(new URL(`../../src-tauri/target/native-lifecycle-review/${nonce}/workspace`, import.meta.url)));
  assert.equal(action, 'workspace');
  const folder = 'workspace', path = cwd;
  for (const dir of [home, cwd, cwd.slice(0,-10), path]) {
    const s = lstatSync(dir); assert(s.isDirectory() && !s.isSymbolicLink() && s.uid === process.getuid()
      && !(s.mode & 0o077) && realpathSync(dir) === dir);
  }
  const script = `JSON.stringify((() => {
    const se = Application('System Events'), ps = se.applicationProcesses.whose({unixId:${pid}})();
    if (ps.length !== 1) return {waiting:true};
    const p = ps[0]; if (p.name().toLowerCase() !== 'echoagent') throw new Error('Foreign application');
    const windows = () => p.windows().flatMap(w => [w,...w.sheets(),...w.sheets().flatMap(s => s.sheets())]);
    const texts = w => w.staticTexts().map(t => { let v=''; try { v=String(t.value()); } catch {} return v+' '+String(t.name()); }).join('\\n');
    const picker = windows().find(w => w.role() === 'AXSheet' && texts(w).includes('选择 EchoAgent 可访问的文件夹'));
    if (!picker) return {waiting:true};
    p.frontmost = true; if (!p.frontmost()) throw new Error('Owned picker not frontmost');
    // The system remembers the last selected folder, including another test
    // profile's identically named directory. Always navigate by the full path.
    const fields = w => [...w.comboBoxes(),...w.textFields()];
    const go = picker.sheets().find(w => fields(w).length === 1
      && w.buttons().every(b => !b.name() || ['Go','前往','Cancel','取消'].includes(b.name())))
      || windows().find(w => fields(w).length > 0 && /前往文件夹|Go to.*folder/i.test(texts(w) + ' ' + w.name()));
    if (!go) { se.keystroke('g',{using:['command down','shift down']}); return {waiting:true}; }
    const field = fields(go)[0]; field.value = ${JSON.stringify(path)};
    if (![${JSON.stringify(path)},${JSON.stringify(path + '/')}].includes(String(field.value()))) throw new Error('Exact knowledge path input did not stick');
    if (!p.frontmost()) throw new Error('Picker focus changed before navigation');
    se.keyCode(36); delay(.4);
    if (!picker.popUpButtons().some(b => b.value() === ${JSON.stringify(folder)})) return {waiting:true};
    const open = picker.buttons().find(b => ['打开','Open','选择'].includes(b.name()) && b.enabled());
    if (!open) throw new Error('Owned knowledge picker Open absent');
    try { open.click(); } catch (error) {
      if (!/Invalid index|无效的索引|-1719/.test(String(error))) throw error;
      delay(.2); if (windows().some(w => w.role() === 'AXSheet' && texts(w).includes('选择 EchoAgent 可访问的文件夹'))) throw error;
    }
    return {performed:true,pid:${pid},action:${JSON.stringify(action)},path:${JSON.stringify(path)}};
  })())`;
  const result = JSON.parse(run('/usr/bin/osascript', ['-l', 'JavaScript', '-e', script], { encoding: 'utf8', timeout: 10000 }).trim());
  if (result.waiting) return false;
  assert(result.performed && result.pid === pid && result.action === action && result.path === path);
  return true;
}
