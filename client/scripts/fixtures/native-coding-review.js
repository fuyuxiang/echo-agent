// Acceptance driver for the actual Echo Code UI and native coding commands.
// Requires the isolated, main-window WKUserScript installer documented in
// native-coding-review.md. No IPC replacement, host ACK fixture, or model mock.
window.__echoNativeCodingReview = async ({ invoke, home, cwd, nonce, sessionId, phase, expected, continueExistingTask = false, continueDeliveryReview = false }) => {
  const report = { environment: 'native WKWebView + production Echo Code/Theia bridge + real Rust subprocesses',
    phase, checks: [], errors: [], expected: expected ?? null, activeStep: 'initialize', steps: [] };
  const filename = '原生代码验收.ts';
  const verifier = 'verify-native-code.cjs';
  const source = (version) => `// EchoAgent 原生代码验收\nexport const 验收值 = ${version};\n// ${'x'.repeat(600)}\n`;
  const command = `node ${verifier}`;
  const taskName = '原生代码阶段与交付验收';
  const requirement = '把原生代码验收.ts中的验收值从1改为2，保留中文注释；真实运行文件内容验证并审阅差异。';
  const settingsSource = JSON.stringify({ 'files.autoSave': 'off' }, null, 2);
  const verifierSource = `const fs = require('node:fs');\nconst assert = require('node:assert/strict');\nassert.match(fs.readFileSync(${JSON.stringify(filename)}, 'utf8'), /export const 验收值 = 2;/);\nconsole.log('native-coding-verification: 中文文件内容正确');\n`;
  const delay = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
  const pending = new Map();
  const traffic = [];
  let frame;
  let sequence = 0;
  const record = (id, ok, evidence) => {
    report.checks.push({ id, result: ok ? 'passed' : 'failed', evidence });
    if (!ok) throw new Error(`Native Echo Code acceptance failed: ${id}`);
  };
  const waitFor = async (check, timeout = 20000, label) => {
    const step = label ?? `UI: ${check.toString().replace(/\s+/g, ' ').slice(0, 180)}`;
    report.activeStep = step;
    report.steps.push({ step, at: new Date().toISOString() });
    const deadline = Date.now() + timeout;
    while (Date.now() < deadline) {
      const result = await check();
      report.activeStep = step;
      if (result) return result;
      await delay(100);
    }
    throw new Error(`Timed out waiting for actual Echo Code step: ${step}`);
  };
  const call = async (name, args = {}, timeout = 20000) => {
    report.activeStep = `Native IPC: ${name}`;
    let timer;
    try {
      return await Promise.race([invoke(name, args), new Promise((_, reject) => {
        timer = setTimeout(() => reject(new Error(`Native command timed out: ${name}`)), timeout);
      })]);
    } finally { clearTimeout(timer); }
  };
  const buttonWithText = (root, text) => [...(root?.querySelectorAll('button') ?? [])]
    .find((button) => button.getClientRects().length > 0 && button.textContent.trim() === text && !button.disabled);
  const nav = (label) => buttonWithText(document.querySelector('.sidebar__nav'), label);
  const modal = (text) => [...document.querySelectorAll('[role="dialog"], [role="alertdialog"]')]
    .find((element) => element.getClientRects().length > 0 && element.textContent.includes(text));
  const visible = (element) => {
    if (!element || element.closest('[hidden]')) return false;
    const rect = element.getBoundingClientRect();
    return rect.width > 0 && rect.height > 0 && getComputedStyle(element).visibility !== 'hidden';
  };
  const ensureSidebar = async () => {
    if (document.querySelector('.app__body--collapsed')) {
      (await waitFor(() => {
        const button = document.querySelector('button[aria-label="展开侧边栏"]');
        return visible(button) ? button : null;
      }, 20000, 'session-navigation:expand-global-sidebar')).click();
      await waitFor(() => !document.querySelector('.app__body--collapsed'), 20000, 'session-navigation:sidebar-expanded');
    }
  };
  const selectRetainedSession = async () => {
    await ensureSidebar();
    const projects = await call('projects_load');
    const owners = projects.filter((project) => project.conversations?.some((item) => item.sessionId === sessionId));
    if (owners.length > 1) throw new Error('The retained session belongs to multiple projects');
    const section = owners.length ? '.sidebar__list-section--projects' : '.sidebar__list-section--tasks';
    const sectionButton = await waitFor(() => {
      const button = document.querySelector(`${section} .sidebar__section-label`);
      return visible(button) ? button : null;
    }, 20000, 'session-navigation:find-retained-session-section');
    if (sectionButton.getAttribute('aria-expanded') !== 'true') {
      sectionButton.click();
      await waitFor(() => sectionButton.getAttribute('aria-expanded') === 'true', 20000, 'session-navigation:section-expanded');
    }
    const revealCatalogItem = (element) => {
      if (!visible(element)) return null;
      if (element.closest('[inert]')) {
        const more = document.querySelector(`${section} .sidebar__catalog-more[aria-expanded="false"]`);
        if (visible(more)) more.click();
        return null;
      }
      element.scrollIntoView?.({ block: 'nearest' });
      return element;
    };
    if (owners.length) {
      const project = owners[0];
      const node = await waitFor(() => {
        const nodes = [...document.querySelectorAll('.sidebar__node--project')].filter((element) => {
          const projectId = element.closest('[data-catalog-project]')?.getAttribute('data-catalog-project');
          return visible(element) && (projectId ? projectId === project.id : element.getAttribute('title') === project.name);
        });
        if (nodes.length > 1) throw new Error('The retained project has multiple visible rows');
        return revealCatalogItem(nodes[0]);
      }, 20000, 'session-navigation:find-retained-project');
      const toggle = node.querySelector('button[aria-expanded]');
      if (!toggle) throw new Error('The retained project has no production conversation expander');
      if (toggle.getAttribute('aria-expanded') !== 'true') {
        toggle.click();
        await waitFor(() => toggle.getAttribute('aria-expanded') === 'true', 20000, 'session-navigation:project-conversations-expanded');
      }
      report.navigation = { sessionId, projectId: project.id, projectName: project.name, expandedViaProductionUi: true };
    }
    const selector = `[data-session-id="${CSS.escape(sessionId)}"] .sidebar__conv-select`;
    const row = await waitFor(() => revealCatalogItem(document.querySelector(selector)),
      20000, 'session-navigation:retained-session-row-visible');
    row.click();
    await waitFor(() => document.querySelector(`${selector}[aria-current="page"]`), 20000, 'session-navigation:retained-session-selected');
    report.navigation = { ...report.navigation, sessionId, selectedViaProductionUi: true };
  };
  const onMessage = (event) => {
    frame ??= document.querySelector('.echo-theia__frame');
    if (!frame || event.source !== frame.contentWindow || event.origin !== new URL(frame.src).origin) return;
    const message = event.data;
    if (!message || typeof message !== 'object') return;
    if (message.type === 'echo/native-coding-result' && message.nonce === nonce) {
      const request = pending.get(message.id);
      if (!request) return;
      pending.delete(message.id);
      message.ok === true ? request.resolve(message.value) : request.reject(new Error(message.error));
      return;
    }
    let bootstrap;
    try { bootstrap = JSON.parse(frame.name.slice('echo-embed:'.length)); } catch { return; }
    if (message.token !== bootstrap.bridgeToken) return;
    if (['echo/ready', 'echo/workspace', 'echo/dirty-state', 'echo/before-mutation', 'echo/after-mutation'].includes(message.type)) {
      traffic.push({ type: message.type, id: message.id, count: message.count, path: message.path,
        paths: message.paths, success: message.success, ticket: message.ticket, error: message.error });
    }
  };
  const child = async (action, args = {}) => {
    report.activeStep = `Theia operation: ${action}`;
    if (!frame?.contentWindow) throw new Error('The production IDE iframe is missing');
    const id = `native-code-${++sequence}`;
    let timer;
    try {
      return await new Promise((resolve, reject) => {
        timer = setTimeout(() => { pending.delete(id); reject(new Error(`Theia review operation timed out: ${action}`)); }, 35000);
        pending.set(id, { resolve, reject });
        frame.contentWindow.postMessage({ type: 'echo/native-coding-operation', nonce, id, action, ...args }, new URL(frame.src).origin);
      });
    } finally { clearTimeout(timer); }
  };
  const openUi = async (taskName) => {
    await ensureSidebar();
    (await waitFor(() => nav('代码开发'))).click();
    frame = await waitFor(() => document.querySelector('.echo-theia__frame'));
    await waitFor(() => !document.querySelector('.echo-theia__cover'), 70000);
    (await waitFor(() => document.querySelector('button[aria-label="切换开发任务"]'))).click();
    (await waitFor(() => [...document.querySelectorAll('.coding-task-switcher__select')]
      .find((button) => button.textContent.includes(taskName)))).click();
    await waitFor(() => document.querySelector('button[aria-label="切换开发任务"]')?.textContent.includes(taskName));
    await child('open', { root: cwd });
    const editorState = await waitFor(async () => {
      const state = await child('state');
      return state.visible ? state : null;
    });
    record('coding-actual-ide-frame', editorState.path === `${cwd}/${filename}`, {
      root: cwd, path: editorState.path, editorVisible: editorState.visible,
      resolvedServices: editorState.resolvedServices,
      url: new URL(frame.src).origin,
    });
  };
  const saveAndLeave = async (rejectExpected) => {
    await ensureSidebar();
    (await waitFor(() => nav('新建任务'))).click();
    const confirmation = await waitFor(() => modal('保存 1 个文件后离开？'));
    record(rejectExpected ? 'coding-rejected-save-dialog' : 'coding-unsaved-confirmation',
      confirmation.textContent.includes('保存成功后继续，保存失败时会留在当前项目'), { text: confirmation.textContent });
    if (!rejectExpected) {
      // Cancelling the first prompt must preserve both the page and buffer.
      buttonWithText(confirmation, '取消').click();
      await waitFor(() => !modal('保存 1 个文件后离开？'));
      record('coding-unsaved-cancel-keeps-buffer', !document.querySelector('.app__coding-surface')?.hidden
        && (await child('state')).text === source(2), { cwd, bufferVersion: 2 });
      nav('新建任务').click();
      buttonWithText(await waitFor(() => modal('保存 1 个文件后离开？')), '保存并继续').click();
      await waitFor(() => document.querySelector('.app__coding-surface')?.hidden);
      await waitFor(async () => (await child('state')).dirty === false);
    } else {
      buttonWithText(confirmation, '保存并继续').click();
      const failed = await waitFor(() => modal('IDE 文件保存失败') || modal('仍有文件未保存'), 25000);
      const rejectedState = await child('state');
      const rejectedRequest = [...traffic].reverse().find((event) => event.type === 'echo/before-mutation'
        && event.paths?.includes(`${cwd}/${filename}`));
      const deniedAcknowledgement = rejectedState.acknowledgements.find((ack) => ack.id === rejectedRequest?.id
        && ack.ok === false && /当前任务阶段不能修改文件/.test(ack.error ?? ''));
      buttonWithText(failed, '取消').click();
      record('coding-verification-rejects-save', !document.querySelector('.app__coding-surface')?.hidden
        && rejectedState.text === source(3) && rejectedState.dirty === true
        && !!deniedAcknowledgement
        && await call('read_text_file', { path: `${cwd}/${filename}`, cwd, maxBytes: 4096 }) === source(2),
      { dialog: failed.textContent, bufferVersion: 3, diskVersion: 2, deniedAcknowledgement });
      await child('undo-rejected-edit');
      await waitFor(async () => (await child('state')).dirty === false);
    }
  };
  const reviewAndDeliver = async (args, diff) => {
    (await waitFor(() => buttonWithText(document, '任务变更 · 1'))).click();
    (await waitFor(() => document.querySelector(`.coding-changeset__row button[title="${filename}"]`))).click();
    await waitFor(() => document.querySelector('[aria-label="任务差异 原生代码验收.ts"] .monaco-diff-editor'));
    (await waitFor(() => buttonWithText(document.querySelector('[aria-label="任务差异 原生代码验收.ts"]'), '标记已审阅'))).click();
    await waitFor(async () => (await call('coding_changeset_get', args)).reviewedFiles.includes(filename));
    record('coding-actual-diff-ui', true, { original: diff.original, modified: diff.modified, modifiedHash: diff.modifiedHash });
    document.querySelector('button[aria-label="关闭差异"]').click();
    (await waitFor(() => document.querySelector('button[aria-label="打开交付报告"]'))).click();
    const reportView = await waitFor(() => document.querySelector('[aria-label="交付报告"]'));
    await waitFor(() => [...reportView.querySelectorAll('tr')]
      .some((row) => row.textContent.includes(command) && row.textContent.includes('通过')));
    (await waitFor(() => buttonWithText(reportView, '人工确认已满足'))).click();
    (await waitFor(() => buttonWithText(reportView, '确认已满足'))).click();
    await waitFor(async () => (await call('coding_task_get', args)).acceptanceCriteria.every((criterion) => criterion.satisfied));
    for (const title of ['需求符合性', '代码质量']) {
      const row = await waitFor(() => [...reportView.querySelectorAll('.coding-report__review-action')]
        .find((element) => element.querySelector('strong')?.textContent === title));
      (await waitFor(() => buttonWithText(row, '审查并确认'))).click();
      (await waitFor(() => buttonWithText(row, '确认已审查'))).click();
      await waitFor(() => !row.isConnected);
    }
    const delivery = await call('coding_delivery_report', args);
    record('coding-native-delivery-gates', delivery.deliverable && delivery.blockers.length === 0,
      { delivery, executionLedger: await call('coding_task_execution_ledger', args) });
  };
  window.addEventListener('message', onMessage);
  try {
    const context = await call('desktop_review_context');
    if (context.home !== home || context.phase !== phase || typeof nonce !== 'string' || nonce.length < 24
      || typeof cwd !== 'string' || !cwd.startsWith(`${home}/`) || cwd.includes('..')) {
      throw new Error('Echo Code review requires a canonical isolated launch and WKUserScript nonce');
    }
    if (sessionId) {
      await selectRetainedSession();
    }
    if (phase === 'restart') {
      if (!expected || expected.cwd !== cwd) throw new Error('The first launch has no valid coding expectation');
      const restored = await call('coding_task_get', { root: cwd, taskId: expected.taskId });
      const delivery = await call('coding_delivery_report', { root: cwd, taskId: expected.taskId });
      record('coding-restart-native-state', restored?.phase === 'delivered' && delivery.deliverable
        && delivery.verifications.length === 1 && delivery.verifications[0].id === expected.verificationId
        && delivery.verifications[0].contentRevision === expected.contentRevision
        && await call('read_text_file', { path: `${cwd}/${filename}`, cwd, maxBytes: 4096 }) === source(2),
      { taskId: restored?.id, phase: restored?.phase, delivery });
      await openUi(expected.taskName);
      record('coding-restart-actual-editor', (await child('state')).text === source(2), { path: `${cwd}/${filename}` });
      (await waitFor(() => document.querySelector('button[aria-label="打开交付报告"]'))).click();
      await waitFor(() => document.querySelector('[aria-label="交付报告"]')?.textContent.includes(command));
      record('coding-restart-delivery-ui', true, { actualCommandVisible: command });
    } else if (continueDeliveryReview) {
      if (continueExistingTask || !expected || expected.cwd !== cwd || expected.taskName !== taskName
        || !/^[A-Za-z0-9_-]+$/.test(expected.taskId)) throw new Error('Invalid retained delivery review scope');
      const args = { root: cwd, taskId: expected.taskId };
      const task = await call('coding_task_get', args);
      const set = await call('coding_changeset_get', args);
      const records = await call('coding_verification_list', args);
      const ledger = await call('coding_task_execution_ledger', args);
      const retainedDelivery = await call('coding_delivery_report', args);
      const submission = ledger.filter((entry) => entry.kind === 'requirement_submitted');
      record('coding-native-delivery-continuation', task?.phase === 'delivered' && task.name === taskName
        && task.requirement === requirement && task.modelId === null && task.sessionId === null
        && task.taskNodes.length === 1 && task.taskNodes[0].status === 'success'
        && submission.length === 1 && set.committedHash === null && set.changes.length === 1
        && set.changes[0].path === filename,
      { taskId: task?.id, phase: task?.phase, originalSubmissionId: submission[0]?.id, noTaskReset: true });
      const verification = records.length === 1 ? records[0] : null;
      record('coding-retained-verification', verification?.status === 'passed' && verification.exitCode === 0
        && verification.command === command && verification.stdout.includes('中文文件内容正确')
        && /^[a-f0-9]{64}$/.test(verification.contentRevision ?? '')
        && retainedDelivery.gates.filter((gate) => gate.id === 'verification_freshness'
          && gate.status === 'satisfied').length === 1
        && retainedDelivery.verifications.length === 1
        && retainedDelivery.verifications[0].id === verification.id
        && retainedDelivery.verifications[0].contentRevision === verification.contentRevision,
      { verification, currentVerificationGate: retainedDelivery.gates.find((gate) => gate.id === 'verification_freshness'),
        noRerunOrEvidenceClearing: true });
      report.expected = { ...expected, verificationId: verification.id, contentRevision: verification.contentRevision };
      await openUi(task.name);
      const diff = await call('coding_changeset_diff', { ...args, path: filename });
      if (diff.original !== source(1) || diff.modified !== source(2)) throw new Error('Retained code differs from the actual original edit');
      await reviewAndDeliver(args, diff);
    } else {
      const tasks = await call('coding_task_list', { root: cwd });
      let created;
      if (continueExistingTask) {
        if (!expected || expected.cwd !== cwd || expected.taskName !== taskName
          || !/^[A-Za-z0-9_-]+$/.test(expected.taskId) || expected.verificationId || expected.contentRevision
          || tasks.length !== 1 || tasks[0].id !== expected.taskId) {
          throw new Error('No unique untouched failed Code task may be continued');
        }
        const args = { root: cwd, taskId: expected.taskId };
        created = await call('coding_task_get', args);
        const set = await call('coding_changeset_get', args);
        const ledger = await call('coding_task_execution_ledger', args);
        if (created.name !== taskName || created.requirement !== requirement || created.phase !== 'discovering'
          || created.blocker !== null || created.planRevision !== null || created.planIssues.length
          || created.contextPaths.length || created.modelId !== null || created.sessionId !== null
          || created.taskNodes.length !== 1 || created.taskNodes[0].planKey !== 'ROOT'
          || created.taskNodes[0].status !== 'running' || created.taskNodes[0].attempt !== 1
          || created.acceptanceCriteria.length !== 1 || created.acceptanceCriteria[0].satisfied
          || created.acceptanceCriteria[0].evidence.length
          || set.taskId !== expected.taskId || set.baselineMode !== 'filesystem' || set.changes.length
          || set.reviewedFiles.length || set.rollbackUnsafeFiles.length || set.committedHash !== null
          || set.committedPaths.length || ledger.length !== 1 || ledger[0].kind !== 'requirement_submitted'
          || ledger[0].taskId !== expected.taskId) {
          throw new Error('The failed Code task progressed or its initial evidence changed');
        }
        for (const [name, content] of [[filename, source(1)], [verifier, verifierSource], ['.theia/settings.json', settingsSource]]) {
          if (await call('read_text_file', { path: `${cwd}/${name}`, cwd, maxBytes: 4096 }) !== content) {
            throw new Error(`Refusing to reset an existing review file: ${name}`);
          }
        }
        record('coding-native-existing-task-continuation', true, { taskId: expected.taskId,
          phase: created.phase, originalSubmissionId: ledger[0].id, untouchedFiles: [filename, verifier, '.theia/settings.json'] });
      } else {
        if (tasks.length) throw new Error('Refusing to alter a workspace with existing coding tasks');
        for (const name of [filename, verifier, '.theia/settings.json']) {
          if ((await call('path_stat', { path: `${cwd}/${name}`, cwd })).exists) throw new Error(`Review file already exists: ${name}`);
        }
        // Real isolated workspace preference: preserve the unsaved scenario.
        await call('write_text_file', { path: '.theia/settings.json', workspaceRoot: cwd, content: settingsSource });
        await call('write_text_file', { path: filename, content: source(1), workspaceRoot: cwd });
        await call('write_text_file', { path: verifier, workspaceRoot: cwd, content: verifierSource });
        created = await call('coding_task_create', { root: cwd, name: taskName, requirement });
      }
      report.expected = { cwd, taskId: created.id, taskName: created.name };
      const args = { root: cwd, taskId: created.id };
      if (!continueExistingTask) {
        await call('coding_changeset_capture_baseline', { ...args, dirtyFiles: [] });
        created = await call('coding_task_submit_requirement', args);
      }
      record('coding-native-task-start', created.phase === 'discovering',
        { id: created.id, phase: created.phase, continuedExistingTask: continueExistingTask });
      await openUi(created.name);
      await child('edit', { version: 2 });
      await waitFor(() => traffic.some((event) => event.type === 'echo/dirty-state' && event.count === 1));
      await saveAndLeave(false);
      record('coding-production-mutation-bridge', traffic.some((event) => event.type === 'echo/before-mutation'
        && event.paths?.includes(`${cwd}/${filename}`)) && traffic.some((event) => event.type === 'echo/after-mutation'
        && event.success === true && event.ticket?.taskId === created.id), { traffic });
      await ensureSidebar();
      (await waitFor(() => nav('代码开发'))).click();
      const set = await call('coding_changeset_get', args);
      const diff = await call('coding_changeset_diff', { ...args, path: filename });
      record('coding-actual-file-and-checkpoint', set.changes.length === 1 && set.changes[0].path === filename
        && diff.original === source(1) && diff.modified === source(2), { changes: set.changes, diff });
      const implemented = await call('coding_orchestrator_report_implementation', args);
      record('coding-native-phase-verifying', implemented.phase === 'verifying', { phase: implemented.phase });
      await waitFor(() => document.querySelector('.echo-theia-agent__evidence .coding-agent__phase')?.textContent.includes('验证'));
      await child('edit', { version: 3 });
      await waitFor(async () => (await child('state')).dirty === true);
      await saveAndLeave(true);
      // Clear the temporary failed/cancelled batch before the final content-bound
      // check, exactly as the production manual "run verification" operation does.
      await call('coding_task_begin_verification', args);
      let missingApproval;
      try { await call('coding_verification_run', { ...args, kind: 'test', command, timeoutSecs: 15 }); }
      catch (error) { missingApproval = String(error); }
      record('coding-verification-requires-approval', /需要用户确认/.test(missingApproval ?? ''), { error: missingApproval });
      const token = await call('coding_verification_approve_plan_command', { ...args, command });
      const verification = await call('coding_verification_run', { ...args, kind: 'test', command,
        timeoutSecs: 15, requestedRunId: 'native-coding-final-verification', approvalToken: token });
      record('coding-real-verification-subprocess', verification.status === 'passed' && verification.exitCode === 0
        && verification.stdout.includes('中文文件内容正确') && !!verification.contentRevision,
      { verification });
      // The production background manager can already close this exact real
      // batch. Avoid a duplicate phase transition; require its persisted proof.
      let completed = await call('coding_task_get', args);
      if (completed.phase !== 'delivered') completed = await call('coding_orchestrator_report_verification', args);
      const completedRecords = await call('coding_verification_list', args);
      const completedDelivery = await call('coding_delivery_report', args);
      if (completedRecords.length !== 1 || completedRecords[0].id !== verification.id
        || completedDelivery.verifications.length !== 1 || completedDelivery.verifications[0].id !== verification.id
        || completedDelivery.gates.filter((gate) => gate.id === 'verification_freshness'
          && gate.status === 'satisfied').length !== 1) throw new Error('Completed task has a different verification batch');
      record('coding-native-completed-evidence', completed.phase === 'delivered', { phase: completed.phase });
      report.expected.verificationId = verification.id;
      report.expected.contentRevision = verification.contentRevision;
      await reviewAndDeliver(args, diff);
    }
    report.passed = report.checks.length > 0 && report.checks.every((check) => check.result === 'passed');
  } catch (error) {
    report.errors.push(String(error)); report.passed = false;
    report.failureContext = {
      activeStep: report.activeStep,
      sidebarCollapsed: !!document.querySelector('.app__body--collapsed'),
      homePresent: !!document.querySelector('.home__title'),
      codeSurfaceHidden: document.querySelector('.app__coding-surface')?.hidden ?? null,
      retainedSessionRowPresent: !!document.querySelector(`[data-session-id="${CSS.escape(sessionId ?? '')}"] .sidebar__conv-select`),
      visibleProjectTitles: [...document.querySelectorAll('.sidebar__node--project')].filter(visible).map((element) => element.getAttribute('title')),
      visibleDialogs: [...document.querySelectorAll('[role="dialog"], [role="alertdialog"]')].filter(visible).map((element) => element.textContent.slice(0, 400)),
      iframePresent: !!document.querySelector('.echo-theia__frame'),
    };
  }
  finally {
    window.removeEventListener('message', onMessage);
    for (const request of pending.values()) request.reject(new Error('Native coding review finished'));
    pending.clear();
  }
  return report;
};
