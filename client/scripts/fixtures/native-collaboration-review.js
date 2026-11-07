// Fixed private journey. Actual production DOM, Runtime, model and scheduler.
// Expert definitions are synthetic inputs; model replies and IPC are never replaced.
window.__echoNativeCollaborationReview = async scope => {
  if (window.__echoNativeCollaborationStarted) return;
  window.__echoNativeCollaborationStarted = true;
  const invoke = window.__TAURI_INTERNALS__.invoke;
  const report = { version: 1, kind: 'native-collaboration', home: scope.home, nonce: scope.nonce,
    phase: scope.phase, fakeIPC: false, passed: false, checks: [], errors: [], expected: scope.expected };
  const deadline = Date.now() + 1000000;
  const assert = (value, text) => { if (!value) throw new Error(text); };
  const delay = ms => new Promise(resolve => setTimeout(resolve, ms));
  const visible = el => !!el && el.getClientRects().length > 0;
  const button = (root, text) => [...root.querySelectorAll('button')].find(el => visible(el) && !el.disabled && el.textContent.trim() === text);
  const call = async (name, args = {}) => {
    let timer;
    try { return await Promise.race([invoke(name, args), new Promise((_, reject) => {
      timer = setTimeout(() => reject(new Error(`Native IPC timed out: ${name}`)), 20000);
    })]); } finally { clearTimeout(timer); }
  };
  const progress = () => call('write_text_file', { path: 'collaboration-progress.json', workspaceRoot: scope.cwd,
    content: JSON.stringify({ step: report.activeStep, checks: report.checks, at: new Date().toISOString() }) });
  const wait = async (read, label, ms = 30000) => {
    report.activeStep = label; await progress();
    const end = Math.min(deadline, Date.now() + ms);
    while (Date.now() < end) { const value = await read(); if (value) return value; await delay(200); }
    throw new Error(`Native condition timed out: ${label}`);
  };
  const click = async (root, text) => (await wait(() => button(root(), text), `UI ${text}`)).click();
  const nav = async text => {
    if (text === '专家团') {
      (await wait(() => document.querySelector('.sidebar__nav-item[aria-label="专家·技能·连接器"]'), 'Capabilities navigation')).click();
      await click(() => document.querySelector('nav[aria-label="专家·技能·连接器管理"]') ?? document, '专家团');
    } else if (text === '定时任务') {
      const direct = document.querySelector('.sidebar__nav-item[aria-label="定时任务"]');
      if (visible(direct)) direct.click();
      else {
        (await wait(() => document.querySelector('.sidebar__nav-item[aria-haspopup="menu"]'), 'More navigation')).click();
        await click(() => document.querySelector('#sidebar-more-menu'), '定时任务');
      }
    } else (await wait(() => [...document.querySelectorAll('.sidebar__nav-item')]
      .find(el => visible(el) && !el.disabled && el.textContent.trim() === text), `Navigation ${text}`)).click();
  };
  const set = (el, value) => {
    assert(el, 'Missing real form field');
    const proto = el.tagName === 'TEXTAREA' ? HTMLTextAreaElement.prototype : el.tagName === 'SELECT' ? HTMLSelectElement.prototype : HTMLInputElement.prototype;
    Object.getOwnPropertyDescriptor(proto, 'value').set.call(el, value);
    el.dispatchEvent(new Event(el.tagName === 'SELECT' ? 'change' : 'input', { bubbles: true }));
  };
  const field = (root, label) => [...root.querySelectorAll('label')].find(el => el.textContent.startsWith(label))?.querySelector('input,textarea,select');
  const stage = async (id, action) => {
    report.activeStep = id; await progress();
    try { const evidence = await action(); report.checks.push({ id, result: 'passed', evidence }); await progress(); }
    catch (error) { report.checks.push({ id, result: 'failed', error: String(error) }); throw error; }
  };
  const root = () => document;
  let groupId;
  const snapshot = () => call('group_snapshot', { groupId });
  const terminal = work => ['completed', 'cancelled', 'failed'].includes(work.status);
  const concise = group => ({ id: group.id, revision: group.revision, paused: group.paused,
    members: group.members.map(m => ({ id: m.id, name: m.name, modelId: m.modelId, sessionId: m.sessionId, discussionSessionId: m.discussionSessionId })),
    works: group.works.map(w => ({ id: w.id, rootId: w.rootId, memberId: w.memberId, status: w.status, sessionId: w.sessionId,
      discussion: w.discussion, summary: w.summary, tokens: w.tokens, output: w.output.slice(0, 1500), error: w.error })), requests: group.requests });
  const send = async text => {
    const input = await wait(() => document.querySelector('.group-chat .echo-composer textarea'), 'Real group Composer');
    set(input, text);
    (await wait(() => { const b = document.querySelector('.group-chat button[aria-label="发送"]'); return b && !b.disabled && b; }, 'Enabled group send')).click();
  };
  const chooseRecipients = async text => {
    const details = await wait(() => document.querySelector('.group-recipient-picker'), 'Member picker');
    details.open = true; await click(() => details, text); details.open = false;
  };
  const openGroup = async name => {
    await nav('专家团');
    const back = document.querySelector('[aria-label="返回专家团列表"]'); if (visible(back)) back.click();
    (await wait(() => document.querySelector(`[aria-label="打开专家团 ${name}"]`), 'Owned group catalog row')).click();
    await wait(() => document.querySelector('.group-chat'), 'Owned actual group chat');
  };
  try {
    report.nativeContext = await call('desktop_review_context');
    await stage('collaboration-isolation', async () => {
      assert(report.nativeContext.home === scope.home && report.nativeContext.phase === scope.phase
        && report.nativeContext.evaluations === 1 && /^\/private\/tmp\/echo-native-review-collaboration-[A-Za-z0-9_-]+$/.test(scope.home)
        && scope.cwd === `${scope.home}/workspaces/default`, 'Private native scope mismatch');
      await wait(async () => { const auth = await call('agent_auth_status'); return auth.ready && auth.runtimeReady
        && auth.synchronized && auth.runtimeModels.includes(scope.modelId) && auth; }, 'Actual authorized model readiness', 90000);
      await wait(() => document.querySelector('.echo-composer textarea'), 'Real native shell');
      return { modelId: scope.modelId, home: scope.home, cwd: scope.cwd };
    });
    if (scope.phase === 'restart') {
      const e = scope.expected; groupId = e.groupId;
      await stage('group-restart', async () => {
        const g = await snapshot(); assert(g.cwd === scope.cwd && g.name === e.groupName && g.works.length === e.workCount
          && g.works.every(terminal) && !g.paused, 'Restored group state mismatch');
        await openGroup(e.groupName);
        await wait(() => document.querySelector('.group-chat')?.textContent.includes(e.finalMarker), 'Real persisted group answer');
        await delay(6000); const after = await snapshot();
        assert(after.works.length === e.workCount && after.works.every(terminal), 'Restart dispatched duplicate group work');
        return concise(after);
      });
      await stage('scheduler-restart', async () => {
        await nav('定时任务'); await delay(6500);
        const s = await call('automations_snapshot'), task = s.automations.find(a => a.id === e.taskId);
        const records = s.records.filter(r => r.automationId === e.taskId);
        assert(task?.status === 'PAUSED' && !task.nextRunAt && records.length === 1 && records[0].id === e.recordId
          && records[0].status === 'success' && records[0].scheduledFor === e.scheduledFor, 'Restart repeated or lost the scheduled run');
        assert(document.body.textContent.includes(e.taskName), 'Restored task absent from actual catalog');
        return { taskId: task.id, status: task.status, records };
      });
    } else {
      report.expected = { schema: 1, home: scope.home, cwd: scope.cwd, nonce: scope.nonce, modelId: scope.modelId,
        groupName: `原生协作验收-${scope.nonce.slice(0, 8)}`, taskName: `原生到点验收-${scope.nonce.slice(0, 8)}` };
      const e = report.expected;
      const model = await wait(() => document.querySelector('.echo-composer .model-selector__trigger'), 'Initial model picker'); model.click();
      (await wait(() => document.querySelector(`.model-selector__item[data-model-id="${scope.modelId}"]`), 'Exact authorized model option')).click();
      (await wait(() => document.querySelector('.workspace-picker__trigger'), 'Workspace picker')).click();
      await call('write_text_file', { path: 'collaboration-workspace-picker-ready.json', workspaceRoot: scope.cwd,
        content: JSON.stringify({ nonce: scope.nonce, cwd: scope.cwd }) });
      await click(root, '选择文件夹…');
      await wait(() => document.querySelector('.workspace-picker__trigger')?.getAttribute('aria-label') === `工作目录：${scope.cwd}。点击切换`, 'Owned native workspace grant');
      report.workspaceSelection = { cwd: scope.cwd, via: 'native folder picker' };
      await stage('group-create-ui', async () => {
        assert((await call('group_list')).length === 0 && (await call('automations_snapshot')).automations.length === 0, 'Review home not empty');
        for (const [name, role] of [['甲方专家', '检查目标完整性'], ['乙方专家', '检查验收可操作性']]) {
          const raw = await call('agents_template', { name, description: role, systemPrompt: `你是${name}，负责${role}。只用中文文字回答，不读取文件，不调用任何工具。回答保留用户提供的验收标记。`, avatar: null, modelTags: [] });
          await call('agents_save', { name, raw });
        }
        await nav('专家团'); await click(root, '创建专家团');
        const form = await wait(() => document.querySelector('.group-form:not(.group-form--settings)'), 'Real group create form');
        set(field(form, '专家团名称'), e.groupName); set(field(form, '协作目标'), '只用文字协作，验证目标与验收标准；不调用工具。');
        for (const name of ['甲方专家', '乙方专家']) {
          const box = await wait(() => [...form.querySelectorAll('.group-expert')].find(el => el.textContent.includes(name))?.querySelector('input'), `Actual expert ${name}`); box.click();
        }
        await click(root, '创建并进入专家团');
        const g = await wait(async () => (await call('group_list')).find(g => g.name === e.groupName), 'Native group durable creation');
        groupId = e.groupId = g.id;
        const full = await snapshot(); assert(full.cwd === scope.cwd && full.members.length === 3
          && full.members.every(m => m.modelId === scope.modelId), 'Group member model/workspace mismatch');
        await wait(() => document.querySelector('.group-chat'), 'Actual created group UI');
        return concise(full);
      });
      await stage('group-parallel-model-summary', async () => {
        set(document.querySelector('select[aria-label="处理方式"]'), 'discussion');
        await chooseRecipients('@全体');
        const marker = `PARALLEL-${scope.nonce}`;
        await send(`请两位专家各从自身职责给出两条简短验收建议，每人回答不超过100字，必须包含 ${marker}。只讨论，不调用工具，不访问文件。`);
        const g = await wait(async () => {
          const g = await snapshot(); if (g.works.some(w => w.status === 'failed')) throw new Error(JSON.stringify(concise(g)));
          return g.works.filter(w => !w.summary).length >= 2 && g.works.some(w => w.summary) && g.works.every(terminal) && g;
        }, 'Two real expert turns and completed summary', 240000);
        const experts = g.works.filter(w => !w.summary);
        assert(new Set(experts.map(w => w.memberId)).size === 2 && experts.every(w => w.status === 'completed' && w.tokens > 0 && w.output.includes(marker)), 'Actual expert outputs missing marker/usage');
        assert(g.works.some(w => w.summary && w.status === 'completed' && w.output.includes(marker)), 'Actual coordinator summary missing');
        await wait(() => document.querySelector('.group-chat')?.textContent.includes(marker), 'Visible actual expert answer');
        return concise(g);
      });
      await stage('group-stream-stop', async () => {
        await click(root, '另起任务'); await chooseRecipients('甲方专家');
        await send(`请连续撰写五千字中文测试设计，开头写 STOP-${scope.nonce}，逐段详细展开，不调用任何工具。`);
        const running = await wait(async () => { const g = await snapshot(); return g.works.find(w => !terminal(w) && w.output.length >= 80); }, 'Actual expert streamed output before stop', 120000);
        await click(root, '停止本次任务');
        const stopped = await wait(async () => { const g = await snapshot(); const w = g.works.find(w => w.id === running.id); return w?.status === 'cancelled' && g.works.every(terminal) && g; }, 'Actual group cancellation acknowledgement', 45000);
        const before = stopped.works.find(w => w.id === running.id); await delay(3500);
        const after = (await snapshot()).works.find(w => w.id === running.id);
        assert(after.status === 'cancelled' && after.output === before.output, 'Cancelled expert produced late output');
        return { workId: running.id, rootId: running.rootId, streamedCharacters: running.output.length,
          finalCharacters: after.output.length, status: after.status, quietMs: 3500 };
      });
      await stage('group-pause-resume-fresh-turn', async () => {
        await click(root, '查看工作');
        const details = await wait(() => document.querySelector('.group-operation-options'), 'Dispatch controls'); details.open = true;
        await click(() => details, '暂停后续派发');
        await wait(async () => (await snapshot()).paused, 'Native dispatch paused');
        await click(root, '恢复派发'); await wait(async () => !(await snapshot()).paused, 'Native dispatch resumed');
        const close = document.querySelector('[aria-label="收起专家团详情"]'); if (visible(close)) close.click();
        await click(root, '另起任务'); await chooseRecipients('交给协调员');
        e.finalMarker = `RESUMED-${scope.nonce}`;
        const prior = (await snapshot()).works.length;
        await send(`根据群内已有两位专家的简短建议，用一段话汇总，最后写 ${e.finalMarker}。不要继续五千字任务，不调用工具。`);
        const g = await wait(async () => { const g = await snapshot(); return g.works.length > prior && g.works.every(terminal) && g; }, 'Actual coordinator response after resume', 150000);
        assert(g.works.slice(prior).some(w => w.status === 'completed' && w.tokens > 0 && w.output.includes(e.finalMarker)), 'Resumed actual model response missing');
        e.workCount = g.works.length; return concise(g);
      });
      await stage('scheduler-create-ui', async () => {
        await nav('定时任务'); await click(root, '新建定时任务');
        const editor = await wait(() => document.querySelector('[aria-label="新建定时任务"]'), 'Actual schedule editor');
        const name = [...editor.querySelectorAll('input')].find(el => el.type === 'text');
        set(name, e.taskName); e.taskMarker = `SCHEDULED-${scope.nonce}`;
        set(editor.querySelector('textarea.atm-prompt-textarea'), `只回复 ${e.taskMarker}，不要读取文件或调用工具。`);
        await click(() => editor, '单次');
        const due = new Date(Date.now() + 125000); due.setSeconds(0, 0);
        e.dueEpoch = due.getTime(); e.scheduledDate = `${due.getFullYear()}-${String(due.getMonth() + 1).padStart(2, '0')}-${String(due.getDate()).padStart(2, '0')}`;
        e.scheduledTime = `${String(due.getHours()).padStart(2, '0')}:${String(due.getMinutes()).padStart(2, '0')}`;
        editor.querySelector('.atm-time-picker-trigger').click();
        const columns = await wait(() => document.querySelectorAll('.atm-time-picker-column').length === 2 && document.querySelectorAll('.atm-time-picker-column'), 'Real time picker columns');
        await click(() => columns[0], e.scheduledTime.slice(0, 2)); await click(() => columns[1], e.scheduledTime.slice(3));
        editor.querySelector('.atm-single-date-picker [role="button"]').click();
        assert(e.scheduledDate === `${new Date().getFullYear()}-${String(new Date().getMonth() + 1).padStart(2, '0')}-${String(new Date().getDate()).padStart(2, '0')}`, 'Bounded review must run before midnight');
        await click(root, '今天');
        await click(() => editor, '高级设置');
        (await wait(() => editor.querySelector('button[aria-label^="选择定时任务模型："]'), 'Explicit task model picker')).click();
        (await wait(() => document.querySelector(`.model-selector__item[data-model-id="${scope.modelId}"]`), 'Exact scheduled task model')).click();
        assert(editor.querySelector('#automation-workspace-path')?.value === scope.cwd, 'Editor must use the granted private workspace');
        await click(() => editor, '创建任务');
        const task = await wait(async () => (await call('automations_snapshot')).automations.find(a => a.name === e.taskName), 'Native active once task saved by UI');
        e.taskId = task.id; e.scheduledFor = task.nextRunAt;
        assert(task.status === 'ACTIVE' && task.scheduleType === 'once' && task.modelId === scope.modelId
          && task.cwds === scope.cwd && task.permissionMode !== 'fullAccess' && !task.pushToWeChat
          && Date.parse(task.nextRunAt) === e.dueEpoch && e.dueEpoch > Date.now(), 'Actual schedule input mismatch');
        const s = await call('automations_snapshot'); assert(s.records.length === 0, 'Task ran before its due time');
        return { taskId: task.id, status: task.status, scheduledFor: task.nextRunAt, modelId: task.modelId,
          cwd: task.cwds, manualRunInvoked: false, externalPush: false };
      });
      await stage('scheduler-actual-due-model', async () => {
        const s = await wait(async () => { const s = await call('automations_snapshot'); const records = s.records.filter(r => r.automationId === e.taskId);
          assert(records.length <= 1, 'Duplicate scheduler dispatch'); if (records[0]?.status === 'failed') throw new Error(records[0].error);
          return records[0]?.status === 'success' && s;
        }, 'Real wall-clock scheduler occurrence and model completion', 300000);
        const r = s.records.find(r => r.automationId === e.taskId), t = s.automations.find(a => a.id === e.taskId);
        assert(r.scheduledFor === e.scheduledFor && Date.parse(r.startedAt) >= e.dueEpoch && r.sessionId && r.promptId
          && r.cwd === scope.cwd && r.modelId === scope.modelId && t.status === 'PAUSED' && !t.nextRunAt, 'Scheduled occurrence proof mismatch');
        const output = await call('automation_run_result', { id: r.id });
        assert(JSON.stringify(output).includes(e.taskMarker), 'Actual scheduler model output missing marker');
        e.recordId = r.id; await delay(6500);
        assert((await call('automations_snapshot')).records.filter(x => x.automationId === e.taskId).length === 1, 'Finished once task dispatched again');
        return { record: r, output, observation: 'actual future wall clock; no manual run, clock override or scheduler invocation', quietMs: 6500 };
      });
    }
    report.passed = report.checks.every(c => c.result === 'passed');
  } catch (error) {
    report.errors.push(String(error?.message || error).slice(0, 6000));
    report.diagnostic = { activeStep: report.activeStep, visibleText: document.body.innerText.slice(-8000) };
    if (groupId) try { report.groupAtFailure = concise(await snapshot()); } catch { /* retain original failure */ }
  }
  report.finishedAt = new Date().toISOString();
  await invoke('desktop_review_complete', { report });
};
