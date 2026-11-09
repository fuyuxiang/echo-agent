// Synthetic navigation guards only. Does not run an app or replace live IPC.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import vm from 'node:vm';

const script = readFileSync(new URL('./fixtures/native-coding-review.js', import.meta.url), 'utf8');
async function run({ legacy = false, sidebarCollapsed = false, missingRow = false, hiddenProject = false, saveScenario = false, legacySave = false } = {}) {
  let clock = 0;
  let sectionOpen = false;
  let projectOpen = false;
  let selected = false;
  let confirmationOpen = false;
  const clicks = [];
  const home = '/private/tmp/echo-native-review-navigation-guards';
  const sessionId = 'retained-session';
  const cwd = `${home}/workspace`;
  const element = (attributes, click = () => {}, shown = () => true) => ({
    getAttribute: (name) => typeof attributes[name] === 'function' ? attributes[name]() : attributes[name] ?? null,
    getClientRects: () => shown() ? [{}] : [],
    getBoundingClientRect: () => ({ width: shown() ? 200 : 0, height: shown() ? 32 : 0 }),
    closest: () => null, click,
  });
  const section = element({ 'aria-expanded': () => String(sectionOpen) }, () => { sectionOpen = true; clicks.push('section'); });
  const expand = element({ 'aria-expanded': () => String(projectOpen) }, () => { projectOpen = true; clicks.push('project'); });
  const more = element({}, () => { hiddenProject = false; clicks.push('more'); });
  const node = element({ title: '原生验收项目', 'data-catalog-project': 'retained-project' }, () => {}, () => sectionOpen);
  node.querySelector = () => expand;
  node.closest = (selector) => selector === '[data-catalog-project]' || (selector === '[inert]' && hiddenProject) ? node : null;
  const row = element({ 'aria-current': () => selected ? 'page' : null }, () => { selected = true; clicks.push('session'); });
  const sidebarExpand = element({}, () => { sidebarCollapsed = false; clicks.push('sidebar'); });
  const newTask = element({}, () => { confirmationOpen = true; clicks.push('new-task'); }, () => !sidebarCollapsed);
  newTask.textContent = '新建任务';
  newTask.disabled = false;
  const cancel = element({}, () => { confirmationOpen = false; clicks.push('cancel'); });
  cancel.textContent = '取消';
  cancel.disabled = false;
  const confirmation = element({}, () => {}, () => confirmationOpen);
  confirmation.textContent = '保存 1 个文件后离开？保存成功后继续，保存失败时会留在当前项目';
  confirmation.querySelectorAll = () => [cancel];
  const document = {
    querySelector: (selector) => {
      if (selector === '.app__body--collapsed') return sidebarCollapsed ? {} : null;
      if (selector === 'button[aria-label="展开侧边栏"]') return sidebarExpand;
      if (selector === '.sidebar__nav') return { querySelectorAll: () => [newTask] };
      if (selector === '.sidebar__list-section--projects .sidebar__section-label') return section;
      if (selector === '.sidebar__list-section--projects .sidebar__catalog-more[aria-expanded="false"]') return hiddenProject ? more : null;
      if (selector.startsWith('[data-session-id=')) {
        return !missingRow && sectionOpen && projectOpen && (!selector.includes('aria-current') || selected) ? row : null;
      }
      return null;
    },
    querySelectorAll: (selector) => selector === '.sidebar__node--project' ? [node]
      : selector === '[role="dialog"], [role="alertdialog"]' && confirmationOpen ? [confirmation] : [],
  };
  const invoke = async (name) => {
    if (name === 'desktop_review_context') return { home, phase: 'first-launch' };
    if (name === 'projects_load') return [{ id: 'retained-project', name: '原生验收项目', conversations: [{ sessionId }] }];
    if (name === 'coding_task_list') return []; // Stop before any task/file work.
    throw new Error(`Synthetic navigation must not invoke mutation: ${name}`);
  };
  class TestDate extends Date { static now() { return clock; } }
  const context = vm.createContext({ window: { addEventListener() {}, removeEventListener() {} }, document,
    CSS: { escape: (text) => text }, getComputedStyle: () => ({ visibility: 'visible' }), Date: TestDate,
    setTimeout: (callback, milliseconds) => setTimeout(() => {
      if (milliseconds <= 100) clock += milliseconds;
      callback();
    }, milliseconds <= 100 ? 0 : milliseconds), clearTimeout });
  const legacySelection = `(await waitFor(() => document.querySelector('[data-session-id="' + CSS.escape(sessionId) + '"] .sidebar__conv-select'))).click();
      await waitFor(() => document.querySelector('[data-session-id="' + CSS.escape(sessionId) + '"] .sidebar__conv-select[aria-current="page"]'));`;
  let guardedScript = legacy ? script.replace('await selectRetainedSession();', legacySelection) : script;
  if (saveScenario) {
    guardedScript = guardedScript.replace("window.addEventListener('message', onMessage);",
      "window.__testSaveAndLeave = saveAndLeave; window.addEventListener('message', onMessage);");
    if (legacySave) guardedScript = guardedScript.replace('const saveAndLeave = async (rejectExpected) => {\n    await ensureSidebar();',
      'const saveAndLeave = async (rejectExpected) => {');
  }
  vm.runInContext(guardedScript, context);
  const report = await context.window.__echoNativeCodingReview({ invoke, home, cwd, sessionId, phase: 'first-launch',
    nonce: 'synthetic-navigation-guard-0123456789', continueExistingTask: true,
    expected: { cwd, taskId: 'retained-task', taskName: '原生代码阶段与交付验收' } });
  let saveError;
  if (saveScenario) {
    sidebarCollapsed = true; // Actual Code entry state reported by native run.
    clicks.length = 0;
    try { await context.window.__testSaveAndLeave(false); } catch (error) { saveError = String(error); }
  }
  return { report, clicks, saveError };
}
const before = await run({ legacy: true });
assert.equal(before.clicks.length, 0);
assert.match(before.report.errors[0], /Timed out/);
assert.equal(before.report.failureContext.retainedSessionRowPresent, false);
const after = await run();
assert.deepEqual(after.clicks, ['section', 'project', 'session']);
assert.equal(after.report.navigation.selectedViaProductionUi, true);
assert.match(after.report.errors[0], /No unique untouched failed Code task/);
const collapsed = await run({ sidebarCollapsed: true });
assert.deepEqual(collapsed.clicks, ['sidebar', 'section', 'project', 'session']);
assert.equal(collapsed.report.navigation.projectId, 'retained-project');
const overflow = await run({ hiddenProject: true });
assert.deepEqual(overflow.clicks, ['section', 'more', 'project', 'session']);
assert.equal(overflow.report.navigation.selectedViaProductionUi, true);
const missing = await run({ missingRow: true });
assert.match(missing.report.errors[0], /session-navigation:retained-session-row-visible/);
assert.equal(missing.report.activeStep, 'session-navigation:retained-session-row-visible');
assert.deepEqual([...missing.report.failureContext.visibleProjectTitles], ['原生验收项目']);
const oldSave = await run({ saveScenario: true, legacySave: true });
assert.deepEqual(oldSave.clicks, []);
assert.match(oldSave.saveError, /Timed out.*nav\('新建任务'\)/);
const newSave = await run({ saveScenario: true });
assert.deepEqual(newSave.clicks, ['sidebar', 'new-task', 'cancel']);
assert.match(newSave.saveError, /production IDE iframe is missing/); // Stop before buffer inspection; no fake editor.
console.log('Synthetic Code navigation guards passed: 7 cases, including collapsed-sidebar recovery, project overflow disclosure, cancel path and named timeout diagnostics. No app, real IPC, Theia, or Rust was exercised.');
