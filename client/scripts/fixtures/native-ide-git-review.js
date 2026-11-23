// Fixed private main factory. Actual UI, cached Theia services and production
// Git commands; independent index/HEAD/file observations belong to Node parent.
((launch) => {
  void (async () => {
    if (window.__echoNativeIdeGitStarted) return;
    window.__echoNativeIdeGitStarted = true;
    const report = { version: 1, kind: 'native-ide-git', passed: false, checks: [], errors: [],
      nativeVerified: false, fakeIPC: false, osPickerVerified: false };
    const delay = ms => new Promise(resolve => setTimeout(resolve, ms));
    const assert = (condition, message) => { if (!condition) throw new Error(message); };
    const deadline = Date.now() + 600000, pending = new Map();
    let invoke, scope, frame, sequence = 0, approved = false;
    const visible = node => !!node?.isConnected && !node.closest('[hidden]') && node.getClientRects().length > 0;
    const wait = async (read, label, timeout = 30000) => {
      report.activeStep = label;
      const end = Math.min(deadline, Date.now() + timeout);
      while (Date.now() < end) { const value = await read(); if (value) return value; await delay(150); }
      throw new Error(`Actual IDE condition timed out: ${label}`);
    };
    const button = (root, label) => [...(root?.querySelectorAll('button') || [])]
      .find(node => visible(node) && !node.disabled && node.textContent.trim() === label);
    const record = (id, evidence) => report.checks.push({ id, result: 'passed', evidence });
    const call = async (name, args = {}) => {
      assert(['desktop_review_context', 'desktop_review_ide_manifest', 'agent_auth_status',
        'path_stat', 'read_text_file', 'write_text_file', 'desktop_review_complete', 'coding_task_create',
        'coding_task_submit_requirement', 'coding_task_get', 'coding_isolation_info'].includes(name), 'Unexpected private IDE command');
      if (name === 'coding_task_create') assert(args.root === scope.cwd && args.name === `隔离并行夹具-${scope.generation}`
        && args.requirement === '隔离原生验收的已持久化分析任务；不执行模型或终端，保留原任务供项目切换核对。', 'Task fixture escaped its private source');
      if (['coding_task_submit_requirement', 'coding_task_get'].includes(name)) assert(args.root === scope.cwd
        && args.taskId === (report.fixtureTask?.id ?? scope.expected?.originalTaskId), 'Task operation escaped the owned fixture');
      if (name === 'coding_isolation_info') assert(typeof args.root === 'string' && args.root.startsWith(`${scope.home}/coding-worktrees/`)
        && /^[a-f0-9-]{36}\/worktree$/.test(args.root.slice(`${scope.home}/coding-worktrees/`.length)), 'Worktree inspection escaped its managed root');
      if (name === 'write_text_file') assert(args.workspaceRoot === scope.cwd
        && /^\.native-ide-(?:stage|commit|disconnect)-ready\.json$/.test(args.path), 'IDE handoff escaped fixed files');
      return invoke(name, args);
    };
    const sidebar = async () => {
      const reveal = document.querySelector('button[aria-label="展开侧边栏"]');
      if (visible(reveal)) reveal.click();
      return wait(() => {
        const nav = document.querySelector('.sidebar__nav');
        const entry = button(nav, '代码开发');
        if (!entry) nav?.querySelector('.sidebar__more-wrap > button[aria-expanded="false"]')?.click();
        return entry;
      }, 'Code navigation');
    };
    const listener = event => {
      if (!frame || event.source !== frame.contentWindow || event.origin !== new URL(frame.src).origin) return;
      if (['echo/backend-connection', 'echo/dirty-state'].includes(event.data?.type)) {
        const bootstrap = JSON.parse(frame.name.slice('echo-embed:'.length));
        if (event.data.token === bootstrap.bridgeToken) {
          report.bridgeEvents = [...(report.bridgeEvents || []), { type: event.data.type,
            ...(typeof event.data.online === 'boolean' ? { online: event.data.online } : {}),
            ...(Number.isSafeInteger(event.data.count) ? { count: event.data.count } : {}), at: Date.now() }].slice(-100);
        }
        return;
      }
      if (event.data?.type !== 'echo/native-ide-git-result' || event.data.nonce !== scope.nonce
        || event.data.generation !== scope.generation) return;
      const request = pending.get(event.data.id); if (!request) return;
      pending.delete(event.data.id);
      event.data.ok === true ? request.resolve(event.data.value) : request.reject(new Error(event.data.error));
    };
    const operation = async action => {
      assert(frame?.contentWindow, 'Actual Theia frame missing'); report.activeStep = `Theia ${action}`;
      const id = `private-ide-${++sequence}`;
      return new Promise((resolve, reject) => {
        const timer = setTimeout(() => { pending.delete(id); reject(new Error(`Theia ${action} timed out`)); }, 90000);
        pending.set(id, { resolve: value => { clearTimeout(timer); resolve(value); },
          reject: error => { clearTimeout(timer); reject(error); } });
        frame.contentWindow.postMessage({ type: 'echo/native-ide-git-operation', id, action,
          nonce: scope.nonce, generation: scope.generation }, new URL(frame.src).origin);
      });
    };
    const parentBarrier = async stage => {
      await call('write_text_file', { path: `.native-ide-${stage}-ready.json`, workspaceRoot: scope.cwd,
        content: JSON.stringify({ nonce: scope.nonce, generation: scope.generation, stage }) });
      return wait(async () => {
        try {
          const value = JSON.parse(await call('read_text_file', { path: `${scope.cwd}/.native-ide-${stage}-proof.json`, cwd: scope.cwd, maxBytes: 8192 }));
          assert(value.nonce === scope.nonce && value.generation === scope.generation && value.stage === stage, 'Foreign IDE barrier');
          assert(value.passed === true, 'Independent Git observation rejected the operation'); return value;
        } catch (error) { if (/Foreign IDE|Independent Git/.test(String(error))) throw error; return null; }
      }, `independent ${stage} proof`);
    };
    const switchProject = async target => {
      (await wait(() => document.querySelector('button[aria-label="切换项目"]'), 'real project switcher')).click();
      (await wait(() => [...document.querySelectorAll('.coding-project-switcher__select')]
        .find(node => visible(node) && node.getAttribute('aria-current') !== 'true' && node.querySelector('small')?.textContent === target), 'exact owned project option')).click();
    };
    try {
      assert(typeof window.__TAURI_INTERNALS__?.invoke === 'function', 'Actual native IPC unavailable');
      invoke = (name, args) => window.__TAURI_INTERNALS__.invoke(name, args);
      const context = await call('desktop_review_context'); scope = await call('desktop_review_ide_manifest');
      assert(context.ideRequested === true && context.evaluations === 1 && context.pid > 0
        && scope.schema === 1 && scope.kind === 'ide-git-boundary' && ['first-launch', 'restart'].includes(scope.phase)
        && context.phase === scope.phase
        && scope.home === launch.home && scope.nonce === launch.nonce && scope.generation === launch.generation
        && context.home === scope.home && scope.cwd === `${scope.home}/workspaces/default`
        && /^[a-f0-9]{32}$/.test(scope.nonce) && /^[a-f0-9]{32}$/.test(scope.generation), 'IDE native scope mismatch');
      approved = true; Object.assign(report, { nativeContext: context, home: scope.home, cwd: scope.cwd,
        nonce: scope.nonce, generation: scope.generation, phase: scope.phase });
      const stat = await call('path_stat', { path: scope.cwd, cwd: scope.cwd });
      assert(stat.exists && stat.absolute === scope.cwd && stat.kind === 'directory', 'IDE workspace was not canonical');
      assert(await call('read_text_file', { path: `${scope.cwd}/${scope.file}`, cwd: scope.cwd, maxBytes: 8192 })
        === (scope.phase === 'restart' ? scope.version3 : scope.version2), 'Wrong initial physical fixture');
      record('ide-private-native-isolation', { pid: context.pid, cwd: scope.cwd, fixedManifest: true });
      await wait(async () => { const auth = await call('agent_auth_status'); return auth.ready && auth.runtimeReady && auth.synchronized; }, 'actual Runtime synchronization', 90000);
      window.addEventListener('message', listener);
      if (scope.phase === 'restart') {
        const expected = scope.expected;
        assert(expected?.worktreeRoot && expected.originalTaskId, 'No exact first-launch worktree result');
        (await sidebar()).click();
        frame = await wait(() => { const node = document.querySelector('.echo-theia__frame'); return visible(node) ? node : null; }, 'restored production IDE iframe', 90000);
        await wait(() => !document.querySelector('.echo-theia__cover'), 'restored production IDE ready', 90000);
        const workspace = await operation('worktree-metadata'), savedWorktree = await operation('worktree-open-saved');
        assert(workspace.root === expected.worktreeRoot && savedWorktree.text === scope.version1 && !savedWorktree.dirty, 'Restart changed selected worktree or saved file');
        record('ide-restart-worktree-native', { workspace, savedWorktree, sameWorktree: true });
        const worktreeFrame = frame; await switchProject(scope.cwd);
        frame = await wait(() => { const next = document.querySelector('.echo-theia__frame'); return next && next !== worktreeFrame ? next : null; }, 'restarted source project iframe', 90000);
        await wait(() => !document.querySelector('.echo-theia__cover'), 'restarted source project ready', 90000);
        const source = await operation('open-saved'), provider = await operation('metadata');
        assert(source.text === scope.version3 && !source.dirty && provider.provider.id === 'git', 'Source editor or Git did not restore');
        record('ide-restart-source-editor-native', { source, provider: provider.provider, noNewCommit: true });
        const task = await call('coding_task_get', { root: scope.cwd, taskId: expected.originalTaskId });
        assert(task?.id === expected.originalTaskId && task.phase === 'discovering' && !task.sessionId, 'Original source analysis task did not persist');
        (await wait(() => document.querySelector('button[aria-label="切换开发任务"]'), 'restored task switcher')).click();
        await wait(() => [...document.querySelectorAll('[aria-label="开发任务"] [role="menuitem"]')]
          .some(node => visible(node) && node.textContent.includes(`隔离并行夹具-${scope.generation}`)), 'restored actual task menu entry');
        record('ide-restart-source-task-native', { originalTaskId: task.id, phase: task.phase, visibleInUi: true, modelExecuted: false });
        report.expected = expected; report.passed = report.checks.length === 4;
        return;
      }
      report.fixtureTask = await call('coding_task_create', { root: scope.cwd, name: `隔离并行夹具-${scope.generation}`,
        requirement: '隔离原生验收的已持久化分析任务；不执行模型或终端，保留原任务供项目切换核对。' });
      report.fixtureTask = await call('coding_task_submit_requirement', { root: scope.cwd, taskId: report.fixtureTask.id });
      assert(report.fixtureTask.phase === 'discovering' && !report.fixtureTask.sessionId, 'Expected a real persisted analysis fixture without a model session');
      (await sidebar()).click();
      frame = await wait(() => { const node = document.querySelector('.echo-theia__frame'); return visible(node) ? node : null; }, 'production IDE iframe', 90000);
      await wait(() => !document.querySelector('.echo-theia__cover'), 'production IDE ready', 90000);
      const metadata = await operation('metadata');
      assert(metadata.provider.id === 'git' && metadata.root === scope.cwd, 'Real Git provider missing');
      record('ide-git-provider-native', { ...metadata, actualIframe: true });
      const editor = await operation('open'); assert(editor.visible && editor.text === scope.version2, 'Actual editor mismatch');
      record('ide-chinese-editor-native', editor);
      record('ide-search-native', await operation('search'));
      const terminal = await operation('terminal');
      assert(terminal.processId > 0 && terminal.output.includes(`IDE_TERMINAL_${scope.generation}`), 'No actual terminal output');
      record('ide-terminal-native', terminal);
      record('ide-git-diff-native', await operation('git-diff'));
      const staged = await operation('git-stage'), index = await parentBarrier('stage');
      record('ide-git-stage-physical', { ...staged, independent: index });
      const committed = await operation('git-commit'), commit = await parentBarrier('commit');
      assert(commit.head !== scope.initialHead, 'Git commit did not change HEAD');
      record('ide-git-commit-physical', { ...committed, independent: commit });
      record('ide-own-preview-native', await operation('preview'));
      const dirty = await operation('dirty'); assert(dirty.dirty && dirty.text === scope.version3, 'No real unsaved edit');
      await sidebar();
      const leave = () => [...document.querySelectorAll('.sidebar__nav-item')]
        .find(node => visible(node) && node.textContent.trim() === '新建任务');
      assert(leave(), 'Actual leave navigation unavailable'); leave().click();
      const modal = () => [...document.querySelectorAll('[role="dialog"]')]
        .find(node => visible(node) && /保存 [1-9]\d* 个文件后离开？/.test(node.textContent));
      const unsavedDialog = await wait(modal, 'real unsaved confirmation');
      const confirmationTitle = unsavedDialog.querySelector('.app-dialog__title')?.textContent ?? unsavedDialog.textContent;
      assert(confirmationTitle === '保存 1 个文件后离开？', 'One shared dirty document was counted more than once');
      button(unsavedDialog, '取消').click();
      await wait(() => !modal(), 'cancelled leave confirmation');
      const retained = await operation('state'); assert(retained.dirty && retained.text === scope.version3, 'Cancel lost unsaved edit');
      assert(await call('read_text_file', { path: `${scope.cwd}/${scope.file}`, cwd: scope.cwd, maxBytes: 8192 }) === scope.version2, 'Cancel wrote the unsaved buffer');
      record('ide-unsaved-cancel-native', { ...retained, physicalVersion: 2, confirmationTitle });
      leave().click(); button(await wait(modal, 'real save-before-leaving confirmation'), '保存并继续').click();
      await wait(() => document.querySelector('.app__coding-surface')?.hidden, 'actual leave after save');
      await wait(async () => await call('read_text_file', { path: `${scope.cwd}/${scope.file}`, cwd: scope.cwd, maxBytes: 8192 }) === scope.version3, 'physical saved version3');
      const saved = await operation('state'); assert(!saved.dirty && saved.text === scope.version3, 'Actual editor stayed dirty');
      record('ide-unsaved-save-native', { ...saved, physicalVersion: 3 });
      (await sidebar()).click();
      await wait(() => !document.querySelector('.app__coding-surface')?.hidden, 'return to actual IDE');
      const disconnectedBuffer = await operation('dirty-again');
      const oldFrame = frame, stopped = await parentBarrier('disconnect');
      assert(stopped.processStopped && stopped.ownedParentPid === context.pid, 'No independently stopped owned backend');
      const warning = await wait(() => document.querySelector('.echo-theia__connection-warning'), 'real dirty disconnect warning', 60000);
      assert(warning.textContent.includes('1 个未保存文件'), 'Dirty disconnect did not preserve known document count');
      assert(document.querySelector('.echo-theia__frame') === oldFrame, 'Disconnected editor was silently replaced');
      button(warning, '重新连接').click();
      const reconnectModal = () => [...document.querySelectorAll('[role="alertdialog"]')]
        .find(node => visible(node) && node.textContent.includes('保留编辑内容后重新连接？'));
      const firstReconnect = await wait(reconnectModal, 'real destructive reconnect confirmation');
      assert(firstReconnect.textContent.includes('重新连接会丢失'), 'Reconnect did not explain data loss');
      button(firstReconnect, '保留编辑内容').click();
      await wait(() => !reconnectModal(), 'cancel reconnect');
      const offlineState = await operation('state');
      assert(offlineState.dirty && offlineState.text === disconnectedBuffer.text
        && offlineState.text === scope.version1 && document.querySelector('.echo-theia__frame') === oldFrame, 'Cancel reconnect discarded actual buffer');
      assert(await call('read_text_file', { path: `${scope.cwd}/${scope.file}`, cwd: scope.cwd, maxBytes: 8192 }) === scope.version3, 'Disconnected buffer changed disk');
      record('ide-disconnect-dirty-cancel-native', { ...offlineState, stopped, physicalVersion: 3, sameFrame: true });
      button(document.querySelector('.echo-theia__connection-warning'), '重新连接').click();
      button(await wait(reconnectModal, 'second explicit reconnect confirmation'), '已保留内容，重新连接').click();
      frame = await wait(() => { const next = document.querySelector('.echo-theia__frame'); return next && next !== oldFrame ? next : null; }, 'new real iframe after confirmed reconnect', 90000);
      await wait(() => !document.querySelector('.echo-theia__cover'), 'real restarted Theia ready', 90000);
      const reconnected = await operation('open-saved');
      assert(reconnected.text === scope.version3 && !reconnected.dirty, 'Reconnect did not reopen the saved physical file');
      const recoveredProvider = await operation('metadata');
      record('ide-disconnect-confirm-recovery-native', { ...reconnected, provider: recoveredProvider.provider, newFrame: true });
      (await wait(() => document.querySelector('button[aria-label="切换开发任务"]'), 'actual task switcher')).click();
      (await wait(() => [...document.querySelectorAll('[aria-label="开发任务"] button[role="menuitem"]')]
        .find(node => visible(node) && node.getAttribute('aria-label')?.startsWith(`打开任务：隔离并行夹具-${scope.generation}，`)), 'select the existing persisted analysis task')).click();
      await wait(() => document.querySelector('button[aria-label="切换开发任务"]')?.textContent.includes(`隔离并行夹具-${scope.generation}`), 'actual selected source task');
      (await wait(() => document.querySelector('button[aria-label="切换开发任务"]'), 'selected task switcher')).click();
      (await wait(() => button(document.querySelector('[aria-label="开发任务"]'), '开始新任务'), 'parallel task menu')).click();
      const beforeWorktreeFrame = frame;
      frame = await wait(() => { const next = document.querySelector('.echo-theia__frame'); return next && next !== beforeWorktreeFrame ? next : null; }, 'actual parallel workspace iframe', 90000);
      await wait(() => !document.querySelector('.echo-theia__cover'), 'actual worktree IDE ready', 90000);
      const workspace = await operation('worktree-metadata'), committedFile = await operation('worktree-open');
      const isolation = await call('coding_isolation_info', { root: workspace.root });
      assert(isolation.root === workspace.root && isolation.sourceRoot === scope.cwd && isolation.baseHead === commit.head, 'Worktree provenance drift');
      assert(committedFile.text === scope.version2, 'Worktree copied unsaved/uncommitted source changes');
      record('ide-parallel-worktree-native', { workspace, isolation, committedFile, actualUiNewTask: true, persistedSyntheticTask: report.fixtureTask });
      const worktreeDirty = await operation('worktree-dirty'), dirtyWorktreeFrame = frame;
      await switchProject(scope.cwd);
      const switchDialog = await wait(modal, 'save worktree before switching');
      button(switchDialog, '取消').click();
      await wait(() => !modal(), 'cancelled project switch');
      const cancelledWorktree = await operation('worktree-state');
      assert(frame === dirtyWorktreeFrame && document.querySelector('.echo-theia__frame') === frame
        && cancelledWorktree.dirty && cancelledWorktree.text === worktreeDirty.text, 'Project cancel discarded worktree edit');
      record('ide-project-switch-dirty-cancel-native', { ...cancelledWorktree, sameFrame: true });
      await switchProject(scope.cwd);
      button(await wait(modal, 'save worktree switch retry'), '保存并继续').click();
      frame = await wait(() => { const next = document.querySelector('.echo-theia__frame'); return next && next !== dirtyWorktreeFrame ? next : null; }, 'original project iframe after saved switch', 90000);
      await wait(() => !document.querySelector('.echo-theia__cover'), 'original project restored ready', 90000);
      const sourceReopened = await operation('open-saved');
      assert(sourceReopened.text === scope.version3 && !sourceReopened.dirty, 'Project switch overwrote original source');
      const originalTask = await call('coding_task_get', { root: scope.cwd, taskId: report.fixtureTask.id });
      assert(originalTask.id === report.fixtureTask.id && originalTask.phase === 'discovering', 'Switch lost the original analysis task');
      record('ide-project-switch-save-native', { sourceReopened, originalTaskId: originalTask.id, worktreeRoot: workspace.root, worktreeSavedVersion: 1 });
      const sourceFrame = frame; await switchProject(workspace.root);
      frame = await wait(() => { const next = document.querySelector('.echo-theia__frame'); return next && next !== sourceFrame ? next : null; }, 'return to actual saved worktree', 90000);
      await wait(() => !document.querySelector('.echo-theia__cover'), 'returned worktree IDE ready', 90000);
      record('ide-project-switch-return-native', await operation('worktree-open-saved'));
      report.expected = { initialHead: scope.initialHead, committedHead: commit.head, version3: scope.version3, file: scope.file,
        worktreeRoot: workspace.root, originalTaskId: originalTask.id, worktreeSavedVersion: scope.version1 };
      report.passed = report.checks.length === 17;
    } catch (error) {
      report.errors.push(String(error?.message || error).slice(0, 2500));
      report.failureUI = {
        dialogs: [...document.querySelectorAll('[role="dialog"], [role="alertdialog"]')]
          .filter(visible).map(node => ({ role: node.getAttribute('role'), text: node.textContent.slice(0, 2000) })),
        codingHidden: document.querySelector('.app__coding-surface')?.hidden,
        framePresent: Boolean(document.querySelector('.echo-theia__frame')),
        coverText: document.querySelector('.echo-theia__cover')?.textContent,
        warningText: document.querySelector('.echo-theia__connection-warning')?.textContent,
        text: document.body.innerText.slice(-6000),
      };
    }
    finally {
      window.removeEventListener('message', listener);
      for (const request of pending.values()) request.reject(new Error('IDE review ended')); pending.clear();
      report.finishedAt = new Date().toISOString();
      if (approved) await call('desktop_review_complete', { report });
    }
  })();
})
