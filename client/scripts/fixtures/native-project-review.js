// Fixed project acceptance: production DOM, native IPC, real account/model/files.
window.__echoNativeProjectReview = async scope => {
  if (window.__echoNativeProjectStarted) return;
  window.__echoNativeProjectStarted = true;
  const invoke = window.__TAURI_INTERNALS__.invoke;
  const report = { version: 1, kind: 'native-project', fakeIPC: false, passed: false, home: scope.home,
    nonce: scope.nonce, phase: scope.phase, checks: [], permissions: [], errors: [], expected: scope.expected };
  const deadline = Date.now() + 1080000;
  const completions = [], usages = [], toolUpdates = [], stops = [], handled = new Set();
  let projectId, activeSessionId, busyService = false;
  const assert = (v, why) => { if (!v) throw new Error(why); };
  const delay = ms => new Promise(r => setTimeout(r, ms));
  const visible = el => !!el && el.getClientRects().length > 0 && !el.closest('[hidden]');
  const button = (root, text) => [...(root?.querySelectorAll('button') ?? [])].find(el => visible(el) && !el.disabled && el.textContent.trim() === text);
  const call = async (name, args = {}, timeout = 20000) => {
    let timer; try { return await Promise.race([invoke(name, args), new Promise((_, reject) => {
      timer = setTimeout(() => reject(new Error(`Native IPC timed out: ${name}`)), timeout);
    })]); } finally { clearTimeout(timer); }
  };
  const progress = async label => {
    report.activeStep = label;
    await call('write_text_file', { path: 'project-progress.json', workspaceRoot: scope.cwd,
      content: JSON.stringify({ step: label, checks: report.checks.map(c => ({ id: c.id, result: c.result })), errors: report.errors, at: new Date().toISOString() }) });
  };
  const wait = async (read, label, timeout = 30000) => {
    await progress(label); const end = Math.min(deadline, Date.now() + timeout);
    while (Date.now() < end) {
      const value = await read(); if (value) return value;
      await delay(200);
    }
    const alerts = [...document.querySelectorAll('[role="alert"]')].filter(visible).map(el => el.textContent.slice(0, 700));
    const dialogs = [...document.querySelectorAll('[role="dialog"],[role="alertdialog"]')].filter(visible).map(el => ({
      label: el.getAttribute('aria-label') ?? (el.getAttribute('aria-labelledby') ?? '').split(/\s+/).map(id => document.getElementById(id)?.textContent ?? '').join(' '),
      buttons: [...el.querySelectorAll('button')].filter(visible).map(b => b.textContent.trim()),
    }));
    throw new Error(`${label} timed out; ${JSON.stringify({ alerts, dialogs })}`);
  };
  const click = async (root, text) => (await wait(() => button(root(), text), `UI ${text}`)).click();
  const set = (el, value) => {
    assert(el, 'Missing production form field');
    const proto = el.tagName === 'TEXTAREA' ? HTMLTextAreaElement.prototype : el.tagName === 'SELECT' ? HTMLSelectElement.prototype : HTMLInputElement.prototype;
    Object.getOwnPropertyDescriptor(proto, 'value').set.call(el, value);
    el.dispatchEvent(new Event(el.tagName === 'SELECT' ? 'change' : 'input', { bubbles: true }));
  };
  const field = (root, label) => [...root.querySelectorAll('label')].find(el => el.textContent.trim().startsWith(label))?.querySelector('input,textarea,select');
  const stage = async (id, action) => { await progress(id); const evidence = await action(); report.checks.push({ id, result: 'passed', evidence }); await progress(id+' completed'); return evidence; };
  const projects = () => call('projects_load');
  const project = async () => (await projects()).find(p => p.id === projectId);
  const catalog = () => call('agent_list_all_sessions', { includeArchived: true });
  const team = (action, extra = {}) => call('org_project_request', { action, projectId: report.expected?.remoteProjectId ?? null, body: null, resourceId: null, expectedGeneration: report.accountGeneration, ...extra }, 30000);
  const snapshot = () => team('snapshot');
  const dialog = label => [...document.querySelectorAll('[role="dialog"],[role="alertdialog"]')].find(el => visible(el) && (
    el.getAttribute('aria-label') === label || (el.getAttribute('aria-labelledby') ?? '').split(/\s+/).map(id => document.getElementById(id)?.textContent ?? '').join(' ').trim() === label
  ));
  const closeTaskDialog = async () => wait(() => {
    const d = document.querySelector('.project-task-drawer');
    if (!d) return true;
    button(d, '×')?.click();
    return !document.querySelector('.project-task-drawer');
  }, 'Task drawer closed or unmounted');
  const hasSourceMarker = content => /SOURCE[-=:：]\s*[a-f0-9]{32}/.test(content);
  const tab = async name => (await wait(() => [...document.querySelectorAll('[role="tab"]')].find(t => visible(t) && t.getAttribute('aria-label') === name), `Project tab ${name}`)).click();
  const openProject = async () => {
    const crumb = document.querySelector('.session-header__project-name');
    if (visible(crumb)) { crumb.click(); await wait(() => document.querySelector('.project-workspace'), 'Return to actual project'); return; }
    if (document.querySelector('.project-workspace')) return;
    const nav = await wait(() => document.querySelector('.sidebar__nav-item[aria-label="项目"]') ?? [...document.querySelectorAll('.sidebar__nav-item')].find(el => visible(el) && el.textContent.trim() === '项目'), 'Projects navigation'); nav.click();
    (await wait(() => document.querySelector(`[aria-label="打开项目 ${CSS.escape(report.expected.name)}"]`), 'Owned project card')).click();
    await wait(() => document.querySelector('.project-workspace'), 'Owned project workspace');
  };
  const showContext = async () => { const b = document.querySelector('[aria-label="展开项目上下文"]'); if (visible(b)) b.click(); };
  const read = path => call('read_text_file', { path, cwd: scope.cwd });
  const ownedSessions = () => report.expected?.sessionIds ?? [];
  const allowedFile = path => {
    if (typeof path !== 'string' || path.includes('..') || /[%*?]/.test(path)) return null;
    const abs = path.startsWith('/') ? path : `${scope.cwd}/${path.replace(/^(?:\.\/)+/, '')}`;
    if (['source.csv','project-result.md','project-reuse.md','project-scheduled.md'].some(n => abs === `${scope.cwd}/${n}`)) return abs;
    if (abs === report.expected?.copiedPath) return abs;
    return null;
  };
  const service = async () => {
    if (busyService) return; busyService = true;
    try {
      const pending = await call('agent_list_pending_interactions', { sessionId: null });
      assert(!pending.questions?.length && !pending.folderTrustRequests?.length && !pending.planApprovals?.length, 'Unexpected question/trust/plan in fixed project journey');
      for (const r of pending.permissions ?? []) {
        if (handled.has(r.requestId)) continue;
        let raw = r.rawInput; if (typeof raw === 'string') { try { raw = JSON.parse(raw); } catch { raw = null; } }
        const value = raw?.tool_input ?? raw;
        const rawName = raw?.tool_name?.split(/:|__|\./).pop().toLowerCase() ?? ({ ReadFile: 'read_file', Read: 'read_file', WriteFile: 'write_file', Write: 'write_file', EditFile: 'edit_file', Edit: 'edit_file', CreateFile: 'create_file' })[value?.variant];
        const name = ({ read: 'read_file', write: 'write_file', edit: 'edit_file' })[rawName] ?? rawName;
        const path = allowedFile(value?.file_path ?? value?.path ?? value?.target_file);
        const filePermitted = ownedSessions().includes(r.sessionId) && path && ['read_file','write_file','edit_file','create_file'].includes(name)
          && (![report.expected?.copiedPath, `${scope.cwd}/source.csv`].includes(path) || name === 'read_file');
        let projectReadPermitted = false;
        let projectReadTaskId;
        if (ownedSessions().includes(r.sessionId) && raw?.tool_name === 'echoagent_organization_memory__knowledge_context'
          && typeof value?.task === 'string' && value.task.length <= 8000
          && report.expected.projectScopeId && (value.scope_ids === undefined
            || Array.isArray(value.scope_ids) && value.scope_ids.length === 1 && value.scope_ids[0] === report.expected.projectScopeId)) {
          const s = await snapshot();
          const run = s.runs.find(run => run.sessionId === r.sessionId && (!value.task_id || run.taskId === value.task_id) && run.actorId === scope.expectedUserId);
          projectReadPermitted = !!run; projectReadTaskId = run?.taskId;
        }
        const permitted = filePermitted || projectReadPermitted;
        if (!permitted) {
          await call('agent_resolve_permission', { requestId: r.requestId, optionId: null, cancelled: true });
          throw new Error(`Unexpected tool denied: ${JSON.stringify({ sessionId: r.sessionId, name, variant: value?.variant, keys: Object.keys(value ?? {}) })}`);
        }
        const card = document.getElementById(`permission-${r.requestId}`), allow = button(card, '允许一次');
        if (!visible(card) || !allow) continue;
        allow.click(); handled.add(r.requestId);
        report.permissions.push({ requestId: r.requestId, sessionId: r.sessionId, toolCallId: r.toolCallId, name, path,
          ...(projectReadPermitted ? { scopeIds: [report.expected.projectScopeId], taskId: projectReadTaskId } : {}), via: 'production allow-once UI' });
      }
    } finally { busyService = false; }
  };
  const send = async text => {
    const area = await wait(() => document.querySelector('.pd-composer-shell .echo-composer textarea'), 'Project production composer'); set(area, text);
    (await wait(() => { const b = document.querySelector('.pd-composer-shell button[aria-label="发送"]'); return visible(b) && !b.disabled && b; }, 'Enabled real project send')).click();
    const id = await wait(async () => {
      const p = await project(); const item = p?.tasks?.find(t => t.sessionId && !ownedSessions().includes(t.sessionId));
      return item?.sessionId;
    }, 'New real project task session', 90000);
    report.expected.sessionIds.push(id); activeSessionId = id;
    const info = (await catalog()).find(s => s.sessionId === id);
    assert(info && info.cwd === scope.cwd, 'Actual task cwd differs');
    return id;
  };
  const finish = async (id, filename, marker) => {
    await wait(async () => {
      await service(); const item = (await catalog()).find(s => s.sessionId === id);
      if (item?.status === 'failed') {
        button(document, '查看原因')?.click(); await delay(100);
        report.failedTurn = { sessionId: id, completions: completions.filter(c => c.sessionId === id),
          visibleDetails: [...document.querySelectorAll('.execution-notice,.process-panel,.msg__body,[role="alert"]')].filter(visible).map(n => n.textContent.slice(-1800)).slice(-6) };
        throw new Error(`Real model session failed: ${id}; ${JSON.stringify(report.failedTurn)}`);
      }
      return item?.status === 'completed' && completions.some(c => c.sessionId === id && c.stopReason === 'end_turn');
    }, `Actual model completion ${filename}`, 240000);
    const actual = (await catalog()).find(s => s.sessionId === id);
    assert(actual.currentModelId === scope.modelId, 'Actual session did not use the pinned organization model');
    const content = await read(`${scope.cwd}/${filename}`);
    assert(typeof content === 'string' && content.includes('TOTAL=18') && hasSourceMarker(content) && content.includes(marker), `Actual ${filename} missing computed data/source/current context`);
    const completion = completions.filter(c => c.sessionId === id).at(-1);
    const usage = await wait(() => usages.find(u => u.sessionId === id && u.promptId === completion.promptId && ['inputTokens','outputTokens','totalTokens','promptTokens','completionTokens','input_tokens','output_tokens','total_tokens','prompt_tokens','completion_tokens'].some(k => typeof u.usage?.[k] === 'number' && u.usage[k] > 0)), 'Durable real turn usage', 15000);
    return { sessionId: id, usage, file: `${scope.cwd}/${filename}`, completion, toolCallIds: toolUpdates.filter(t => t.sessionId === id).map(t => t.toolCallId ?? t.tool_call_id).filter(Boolean) };
  };
  const listen = async (event, handler) => {
    const id = window.__TAURI_INTERNALS__.transformCallback(({ payload }) => handler(payload));
    const eventId = await call('plugin:event|listen', { event, target: { kind: 'Any' }, handler: id });
    stops.push(() => call('plugin:event|unlisten', { event, eventId }));
  };
  try {
    report.nativeContext = await call('desktop_review_context');
    assert(report.nativeContext.home === scope.home && report.nativeContext.phase === scope.phase && report.nativeContext.evaluations === 1, 'Native project scope differs');
    await listen('agent://complete', p => completions.push(p));
    await listen('agent://turn-usage', p => usages.push(p));
    await listen('agent://update', p => { if (['tool_call','tool_call_update'].includes(p?.sessionUpdate ?? p?.type)) toolUpdates.push(p); });
    const accountCheck = () => stage('real-account-and-model', async () => {
      const org = await wait(async () => {
        const s = await call('org_session');
        report.accountRestoration = { loggedIn: s.loggedIn, serverUrl: s.serverUrl, userId: s.user?.id, username: s.user?.username, requiresReauthentication: s.requiresReauthentication };
        if (s.user?.id && s.user.id !== scope.expectedUserId) throw new Error('Foreign real account returned during restore');
        return s.loggedIn && s.user?.id && s.user?.username && s.serverUrl && s;
      }, 'Restore configured real organization account', 90000);
      assert(org.serverUrl === scope.serverUrl && org.user?.id === scope.expectedUserId && org.user.username === scope.expectedUsername, 'Real account does not match selected organization');
      report.accountGeneration = org.accountGeneration;
      const caps = await call('org_project_request', { action: 'capabilities', projectId: null, body: null, resourceId: null, expectedGeneration: org.accountGeneration });
      assert(caps.version >= 4 && caps.taskIdentity && caps.projectComments && caps.decisionRevisions, 'Production server migration/capabilities missing');
      await wait(async () => { const a = await call('agent_auth_status'); return a.ready && a.runtimeReady && a.synchronized && a.runtimeModels.includes(scope.modelId); }, 'Real configured model ready', 90000);
      return { serverUrl: org.serverUrl, userId: org.user.id, username: org.user.username, capabilities: caps, modelId: scope.modelId };
    });
    if (scope.phase === 'restart') {
      await accountCheck();
      const e = report.expected; projectId = e.projectId;
      await stage('restart-project-sessions-and-files', async () => {
        const p = await project(); assert(p && p.name === e.name && p.assets.some(a => a.path === e.copiedPath && a.sourceSessionId === e.sessionIds[0]), 'Durable project/copy lost after restart');
        for (const id of e.sessionIds) assert((await catalog()).some(s => s.sessionId === id && s.cwd === scope.cwd), 'Real source session missing after restart');
        for (const filename of ['project-result.md','project-reuse.md','project-scheduled.md']) assert(hasSourceMarker(await read(`${scope.cwd}/${filename}`)), 'Actual file missing after restart');
        const data = await call('automations_snapshot');
        assert(data.automations.find(a => a.id === e.automationId)?.status === 'PAUSED' && data.records.filter(r => r.automationId === e.automationId).length === 1, 'Schedule resumed or duplicated after restart');
        const shared = await snapshot(); assert(shared.id === e.remoteProjectId && shared.tasks.length === 2 && shared.runs.filter(r => r.status === 'completed').length === 2, 'Shared tasks/runs lost after restart');
        await openProject(); await tab('任务');
        return { projectId, remoteProjectId: shared.id, sessionIds: e.sessionIds, automationId: e.automationId, noNewModelTurn: true };
      });
    } else {
      assert((await projects()).length === 0 && (await catalog()).length === 0, 'Refusing a non-empty acceptance home');
      report.expected = { name: `端到端验收-${scope.nonce.slice(0,8)}`, sessionIds: [], contextInitial: `CONTEXT-${scope.nonce}-INITIAL`, contextLatest: `CONTEXT-${scope.nonce}-LATEST` };
      const e = report.expected;
      await stage('native-folder-selection', async () => {
        await wait(() => document.querySelector('.echo-composer textarea'), 'Real desktop shell');
        (await wait(() => document.querySelector('.workspace-picker__trigger'), 'Actual workspace picker')).click();
        await call('write_text_file', { path: 'project-workspace-picker-ready.json', workspaceRoot: scope.cwd, content: JSON.stringify({ nonce: scope.nonce, cwd: scope.cwd }) });
        await click(() => document, '选择文件夹…');
        await wait(() => document.querySelector('.workspace-picker__trigger')?.getAttribute('aria-label') === `工作目录：${scope.cwd}。点击切换`, 'Native exact-folder authorization');
        return { cwd: scope.cwd, via: 'production native folder picker' };
      });
      await accountCheck();
      await stage('project-create-and-publish-ui', async () => {
        (await wait(() => [...document.querySelectorAll('.sidebar__nav-item')].find(el => visible(el) && el.textContent.trim() === '项目'), 'Project navigation')).click();
        await click(() => document, '新建项目'); const d = await wait(() => dialog('新建项目'), 'Create project dialog');
        set(d.querySelector('#create-project-name'), e.name);
        set(d.querySelector('#create-project-description'), '真实账号与模型端到端验收：项目创建、执行、成果复用及定时触发。');
        const details = d.querySelector('details'); details.open = true;
        set(d.querySelector('#create-project-instructions'), `仅使用当前验收工作目录、明确附带的项目文件和当前项目内的共享记录。禁止外部消息、shell、knowledge_feedback 和 knowledge_submit。项目知识只读查询仅限当前项目。本次验收以任务描述的文件和字段为准；未单独填写验收字段不构成阻塞，无需追问。输出文件中包含本项目标记 ${e.contextInitial}。`);
        d.querySelectorAll('details')[1].open = true;
        set(await wait(() => d.querySelector('#create-project-model'), 'Project explicit model preference'), scope.modelId);
        await click(() => d, '创建项目');
        const p = await wait(async () => (await projects()).find(p => p.name === e.name), 'Native durable project creation');
        projectId = e.projectId = p.id; assert(p.cwd === scope.cwd, 'Project creation did not inherit selected cwd');
        await wait(() => document.querySelector('.project-workspace'), 'Actual project overview');
        (await wait(() => document.querySelector('[aria-label="邀请与项目参与者"]'), 'Project participants')).click();
        await click(() => dialog('项目参与者'), '开启协作并选择共享内容');
        await click(() => dialog('开启项目协作'), '确认共享');
        e.remoteProjectId = await wait(async () => Object.values((await project())?.sharedProjects ?? {})[0], 'Actual shared project identity');
        const s = await snapshot(); assert(s.name === e.name && s.role === 'admin' && s.members.length === 1 && s.ownerId === scope.expectedUserId, 'Shared project owner/visibility differs');
        e.projectScopeId = s.scopeId;
        await click(() => dialog('项目参与者'), '×');
        return { localId: projectId, remoteId: e.remoteProjectId, ownerId: s.ownerId, memberCount: s.members.length };
      });
      await stage('real-project-task-and-file', async () => {
        const id = await send('读取当前工作目录的 source.csv，计算 amount 列中数值项的总和，并提取 source_marker 行的原值。使用文件写入工具生成 project-result.md，内容包含 TOTAL=<实际总和>、source_marker 原值和当前项目指令中的 CONTEXT 标记。不要使用 shell、外部互联网、子代理或访问其他文件。完成后简短说明实际文件路径。');
        const result = await finish(id, 'project-result.md', e.contextInitial);
        await wait(async () => { const s = await snapshot(); return s.runs?.some(r => r.sessionId === id && r.status === 'completed') && s.tasks.some(t => t.status === 'review'); }, 'Real public execution report synchronized', 60000);
        return result;
      });
      await stage('output-preview-copy-and-new-task', async () => {
        await openProject(); await tab('资料与成果');
        (await wait(() => [...document.querySelectorAll('.project-file-title')].find(b => b.textContent.trim() === 'project-result.md'), 'Actual generated output row')).click();
        const d = await wait(() => dialog('预览：project-result.md'), 'Actual file preview');
        await wait(() => d.textContent.includes('TOTAL=18') && d.textContent.includes('SOURCE-'), 'Preview reads actual output bytes');
        await click(() => d, '用于新任务');
        const asset = await wait(async () => (await project()).assets.find(a => a.sourceSessionId === e.sessionIds[0] && a.purpose === 'output'), 'Durable private output copy');
        e.copiedPath = asset.path;
        assert(asset.path.startsWith(`${scope.home}/projects/${projectId}/assets/`) && asset.sourcePath.endsWith('project-result.md'), 'Copy escaped project or lost provenance');
        await wait(() => document.querySelector('[aria-label="本次项目资料"]'), 'Selected real project attachment');
        const id = await send('仅依据本次附带的项目成果副本，提取其中 TOTAL 和 SOURCE 标记，生成 project-reuse.md，保留这些实际值和当前项目指令中的 CONTEXT 标记，并写入 FROM_FILE=project-result.md。不要读取工作目录中的 project-result.md 或 source.csv，不要使用 shell、外部互联网或子代理。');
        const result = await finish(id, 'project-reuse.md', e.contextInitial);
        await wait(async () => (await snapshot()).runs?.filter(r => r.status === 'completed').length === 2, 'Second real execution report', 60000);
        return { ...result, copiedPath: e.copiedPath, sourceSessionId: e.sessionIds[0], nativeAttachmentSelected: true };
      });
      await stage('share-generated-file-and-accept', async () => {
        await openProject(); await tab('资料与成果');
        const row = await wait(() => [...document.querySelectorAll('.project-material-row')].find(r => r.textContent.includes('project-result.md') && button(r, '共享到项目')), 'Explicit local outcome sharing');
        button(row, '共享到项目').click(); await click(() => dialog('共享「project-result.md」到项目？'), '共享文件');
        e.documentId = await wait(async () => (await snapshot()).assets.find(a => a.title === 'project-result.md' && a.status === 'ready')?.id, 'Real scanned and ingested shared document', 180000);
        await tab('任务');
        const shared = await snapshot(), item = shared.tasks.find(t => shared.runs.some(r => r.taskId === t.id && r.sessionId === e.sessionIds[1]));
        assert(item, 'Actual reuse task not associated with its public run');
        (await wait(() => [...document.querySelectorAll('.project-task-row')].find(r => r.textContent.includes(item.title)), 'Reuse task row')).click();
        const status = await wait(() => document.querySelector('select[aria-label="任务状态"]'), 'Real shared task acceptance control');
        set(status, 'accepted'); await click(() => dialog('确认验收任务？'), '确认验收');
        await wait(async () => (await snapshot()).tasks.some(t => t.status === 'accepted'), 'Server persisted explicit acceptance');
        return { documentId: e.documentId, accepted: true };
      });
      await stage('scheduled-project-work-actual-due', async () => {
        await tab('概览'); await showContext();
        (await wait(() => document.querySelector('[aria-label="管理项目定时工作"]'), 'Project schedule entry')).click();
        await click(() => document.querySelector('.project-task-drawer'), '新建定时工作');
        const d = await wait(() => {
          const d = document.querySelector('.project-task-drawer'); return d && field(d, '执行内容') && d;
        }, 'Project schedule editor fields');
        const due = new Date(Math.ceil((Date.now()+120000)/60000)*60000);
        e.dueEpoch = due.getTime(); e.scheduleName = `到点验收-${scope.nonce.slice(0,8)}`;
        set(field(d, '名称'), e.scheduleName);
        set(field(d, '执行内容'), '读取当前工作目录的 project-reuse.md，提取其 TOTAL 与 SOURCE 标记，使用文件写入工具生成 project-scheduled.md，保留实际 TOTAL、SOURCE 值及运行当时最新项目指令中的 CONTEXT 标记。禁止 shell、网络、子代理以及其他文件。完成后简短说明文件。');
        set(field(d, '重复'), ['SU','MO','TU','WE','TH','FR','SA'][due.getDay()]);
        set(field(d, '时间'), `${String(due.getHours()).padStart(2,'0')}:${String(due.getMinutes()).padStart(2,'0')}`);
        set(field(d, '模型'), scope.modelId);
        await click(() => d, '保存定时工作');
        const a = await wait(async () => (await call('automations_snapshot')).automations.find(a => a.name === e.scheduleName), 'Real saved project scheduler binding');
        e.automationId = a.id; assert(a.projectId === projectId && Date.parse(a.nextRunAt) === e.dueEpoch, 'Native project schedule due time differs');
        await closeTaskDialog();
        (await wait(() => document.querySelector('[aria-label="项目设置"]'), 'Project settings')).click();
        const settings = await wait(() => dialog('项目设置'), 'Project settings dialog mounted');
        set(field(settings, '本机项目指令'), `仅使用当前验收工作目录、明确附带的项目文件和当前项目内的共享记录。禁止外部消息、shell、knowledge_feedback 和 knowledge_submit。项目知识只读查询仅限当前项目。本次验收以任务描述的文件和字段为准；未单独填写验收字段不构成阻塞，无需追问。输出文件中包含本项目标记 ${e.contextLatest}。`);
        await click(() => settings, '保存设置');
        await wait(async () => (await project()).instructions.includes(e.contextLatest), 'Latest project instructions saved before trigger');
        (await wait(() => document.querySelector('[aria-label="管理项目定时工作"]'), 'Schedule monitoring')).click();
        const record = await wait(async () => (await call('automations_snapshot')).records.find(r => r.automationId === a.id && r.sessionId), 'Actual due trigger without manual run', 240000);
        e.recordId = record.id; e.sessionIds.push(record.sessionId); activeSessionId = record.sessionId;
        assert(Date.parse(record.scheduledFor) === e.dueEpoch && Date.parse(record.startedAt) >= e.dueEpoch, 'Not a real due execution');
        await click(() => document.querySelector('.project-task-drawer'), '打开任务');
        // Closing the editor leaves the actual selected source ChatView usable.
        if (document.querySelector('.project-task-drawer')) await closeTaskDialog();
        await finish(record.sessionId, 'project-scheduled.md', e.contextLatest);
        const done = await wait(async () => (await call('automations_snapshot')).records.find(r => r.id === record.id && r.status === 'success'), 'Native scheduler successful settlement', 60000);
        await openProject(); await showContext();
        (await wait(() => document.querySelector('[aria-label="管理项目定时工作"]'), 'Pause tested recurring schedule')).click();
        const scheduleRow = await wait(() => [...document.querySelectorAll('.project-automation-row')].find(r => r.textContent.includes(e.scheduleName)), 'Owned schedule row');
        await click(() => scheduleRow, '暂停');
        await wait(async () => (await call('automations_snapshot')).automations.find(a => a.id === e.automationId)?.status === 'PAUSED', 'No future acceptance task remains active');
        await closeTaskDialog();
        const linked = await project(); assert(linked.conversations.some(c => c.sessionId === record.sessionId), 'Scheduled session did not return to project');
        return { automationId: a.id, recordId: done.id, scheduledFor: done.scheduledFor, startedAt: done.startedAt, sessionId: done.sessionId, modelId: done.modelId, status: done.status, manualRunInvoked: false, pausedAfterVerification: true };
      });
      await stage('no-duplicate-project-task-or-trigger', async () => {
        await delay(5000);
        const p = await project(), s = await snapshot(), data = await call('automations_snapshot');
        assert((await projects()).length === 1 && s.tasks.length === 2 && s.runs.length === 2 && data.records.filter(r => r.automationId === e.automationId).length === 1, 'Unexpected duplicate persistent work');
        assert(e.sessionIds.length === 3 && new Set(e.sessionIds).size === 3 && e.sessionIds.every(id => p.conversations.some(c => c.sessionId === id)), 'Project/session provenance incomplete');
        return { localProjects: 1, sharedTasks: 2, sharedRuns: 2, scheduledRuns: 1, sessionIds: e.sessionIds };
      });
    }
    report.passed = true;
  } catch (error) {
    report.errors.push(`${String(error?.message ?? error)}\n${String(error?.stack ?? '')}`.slice(0, 5000));
    report.checks.push({ id: report.activeStep ?? 'startup', result: 'failed', evidence: { alerts: [...document.querySelectorAll('[role="alert"]')].filter(visible).map(n => n.textContent.slice(0,1000)) } });
    if (report.expected?.automationId) {
      try {
        await call('automations_set_status', { id: report.expected.automationId, status: 'PAUSED' });
        report.failedSchedulePaused = true;
      } catch (cleanup) { report.errors.push(`Pause failed acceptance schedule: ${String(cleanup)}`); }
    }
  } finally {
    for (const stop of stops) { try { await stop(); } catch {} }
    report.finishedAt = new Date().toISOString();
    await call('desktop_review_complete', { report }, 30000);
  }
};
