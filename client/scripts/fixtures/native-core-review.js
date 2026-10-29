// Fixed private native journey. No injected IPC, stores, events, model output,
// contact actions, shell commands, or arbitrary fixture instructions.
window.__echoNativeCoreReview = async function ({ home, nonce } = {}) {
  if (window.__echoNativeCoreReviewStarted) return;
  window.__echoNativeCoreReviewStarted = true;
  const report = { version: 1, kind: 'native-core', passed: false, checks: [], errors: [],
    permissions: [], expected: null, nativeVerified: false, fakeIPC: false, osPickerVerified: false };
  const deadline = Date.now() + 780000, stops = [], completions = [], toolUpdates = [], subagents = [];
  let invoke, scope, approved = false, activeSessionId;
  const assert = (value, message) => { if (!value) throw new Error(message); };
  const canContinueAfterFailure = (status, queueEmpty) => status === 'completed' && queueEmpty;
  const delay = (ms) => new Promise((accept) => setTimeout(accept, ms));
  const visible = (element) => !!element && !element.closest('[hidden]') && element.getClientRects().length > 0;
  const button = (root, text) => [...(root?.querySelectorAll('button') ?? [])]
    .find((element) => visible(element) && !element.disabled && element.textContent.trim() === text);
  const bounded = async (work, label, ms = 15000) => {
    const remaining = Math.min(ms, deadline - Date.now());
    assert(remaining > 0, 'Core review exceeded its 780s bound');
    report.activeStep = label;
    let timer;
    try { return await Promise.race([work(), new Promise((_accept, reject) => {
      timer = setTimeout(() => reject(new Error(`${label} timed out`)), remaining);
    })]); } finally { clearTimeout(timer); }
  };
  const call = (command, args = {}, ms) => bounded(() => invoke(command, args), `Native IPC ${command}`, ms);
  const wait = async (read, label, ms = 25000) => {
    const end = Math.min(deadline, Date.now() + ms);
    while (Date.now() < end) {
      const value = await bounded(read, label, Math.min(15000, end - Date.now()));
      if (value) return value;
      await delay(150);
    }
    throw new Error(`Actual native condition timed out: ${label}`);
  };
  const record = (id, evidence) => report.checks.push({ id, result: 'passed', evidence });
  const stage = async (id, work) => {
    try { const evidence = await work(); record(id, evidence); return evidence; }
    catch (error) { const message = String(error?.message || error).slice(0, 1800);
      report.checks.push({ id, result: 'failed', evidence: { error: message } }); report.errors.push(`${id}: ${message}`); return null; }
  };
  const set = (element, value) => {
    const proto = element.tagName === 'TEXTAREA' ? HTMLTextAreaElement.prototype
      : element.tagName === 'SELECT' ? HTMLSelectElement.prototype : HTMLInputElement.prototype;
    Object.getOwnPropertyDescriptor(proto, 'value').set.call(element, value);
    element.dispatchEvent(new Event(element.tagName === 'SELECT' ? 'change' : 'input', { bubbles: true }));
  };
  const selected = () => document.querySelector('[data-session-id] .sidebar__conv-select[aria-current="page"]')
    ?.closest('[data-session-id]')?.dataset.sessionId;
  const catalog = () => call('agent_list_all_sessions', { includeArchived: true });
  const ownSession = async (sessionId) => {
    assert(report.expected?.ownedSessionIds.includes(sessionId), 'Session is outside the owned journey');
    const entry = (await catalog()).find((item) => item.sessionId === sessionId);
    assert(entry && entry.cwd === scope.cwd, 'Session lost its exact private workspace');
    return entry;
  };
  const requireSettledAfterFailedStage = async (id, sessionId) => {
    const state = await ownSession(sessionId);
    const queueEmpty = !document.querySelector('.queue-panel');
    const continuing = canContinueAfterFailure(state.status, queueEmpty);
    (report.stageContinuations ??= []).push({ id, sessionId, status: state.status, queueEmpty, continuing });
    assert(continuing, `${id} failed with unfinished session/queue; abort further work`);
  };
  const select = async (sessionId) => {
    await ownSession(sessionId);
    if (document.querySelector('.app__body--collapsed')) {
      (await wait(() => document.querySelector('button[aria-label="展开侧边栏"]'), 'sidebar reveal')).click();
    }
    const project = report.expected.project;
    if (project) {
      const expand = document.querySelector(`button[aria-label="展开${CSS.escape(project.name)}对话"]`);
      if (visible(expand)) expand.click();
    }
    const tasks = document.querySelector('.sidebar__list-section--tasks > .sidebar__section-label');
    if (visible(tasks) && tasks.getAttribute('aria-expanded') !== 'true') tasks.click();
    (await wait(() => {
      const row = document.querySelector(`[data-session-id="${CSS.escape(sessionId)}"] .sidebar__conv-select`);
      return visible(row) ? row : null;
    }, 'owned sidebar session')).click();
    await wait(() => selected() === sessionId, 'actual selected session'); activeSessionId = sessionId;
    const info = await wait(async () => {
      try {
        const info = await call('agent_session_info', { sessionId });
        assert(info.cwd === scope.cwd, 'Loaded session escaped its private workspace');
        return info;
      } catch (error) {
        // The sidebar's selection is synchronous; the real load/binding is not.
        // Retry only the documented not-yet-bound state, never another error.
        if (/当前会话尚未建立可信工作区绑定/.test(String(error))) return null;
        throw error;
      }
    }, 'resident session workspace');
    // A resident backend session can answer before React finishes loading its
    // transcript. Wait for the real Composer readiness before another action.
    await wait(() => {
      const control = document.querySelector('.echo-composer .model-selector__trigger');
      return selected() === sessionId && visible(control) && !control.disabled && control.getAttribute('aria-busy') !== 'true';
    }, 'actual selected conversation UI ready');
    return info;
  };
  const menu = async (sessionId, label) => {
    await ownSession(sessionId);
    (await wait(() => document.querySelector(`[data-session-id="${CSS.escape(sessionId)}"] .sidebar__conv-action`), 'session action')).click();
    (await wait(() => [...document.querySelectorAll('[role="menuitem"]')]
      .find((item) => visible(item) && item.textContent.trim() === label), `session menu ${label}`)).click();
  };
  const input = async (text) => {
    const area = await wait(() => { const el = document.querySelector('.echo-composer textarea'); return visible(el) && !el.disabled ? el : null; }, 'production Composer');
    set(area, text);
    await wait(() => document.querySelector('.echo-composer textarea')?.value === text, 'actual Composer draft');
  };
  const send = async (text, label = '发送') => {
    await input(text);
    (await wait(() => { const el = document.querySelector(`.echo-composer button[aria-label="${label}"]`); return visible(el) && !el.disabled ? el : null; }, `production ${label}`)).click();
  };
  const users = () => [...document.querySelectorAll('.msg--user')].filter(visible).map((el) => el.textContent);
  const reply = () => {
    const last = [...document.querySelectorAll('.msg--user')].filter(visible).at(-1);
    return [...document.querySelectorAll('.msg--assistant .msg__body > .markdown-body[data-md-theme="loose"]')]
      .filter((el) => visible(el) && (!last || last.compareDocumentPosition(el) & Node.DOCUMENT_POSITION_FOLLOWING))
      .map((el) => el.innerText).join('\n');
  };
  const parseOperation = (request, permitted) => {
    if (request.sessionId !== activeSessionId) return null;
    let raw = request.rawInput;
    if (typeof raw === 'string') { try { raw = JSON.parse(raw); } catch { return null; } }
    if (!raw || typeof raw !== 'object' || Array.isArray(raw)) return null;
    const wrapped = typeof raw.tool_name === 'string', value = wrapped ? raw.tool_input : raw;
    if (!value || typeof value !== 'object' || Array.isArray(value)) return null;
    const name = wrapped ? raw.tool_name.split(/:|__/).pop().toLowerCase()
      : ({ ReadFile: 'read_file', Task: 'task' })[value.variant];
    if (permitted?.kind === 'read' && name === 'read_file') {
      const p = value.file_path ?? value.path ?? value.target_file;
      // No ../, encoded path, glob, or sibling is approved, even if canonical
      // resolution could make an unusual spelling point at the fixture.
      const actual = p === permitted.path ? p : p === permitted.relative ? `${scope.cwd}/${p}` : null;
      return actual === permitted.path ? { name, path: actual } : null;
    }
    if (permitted?.kind === 'child' && ['task', 'spawn_subagent'].includes(name)
      && typeof value.prompt === 'string' && value.prompt.replace(/[“”]/g, '"') === permitted.prompt.replace(/[“”]/g, '"')
      && value.subagent_type === 'general-purpose'
      && value.run_in_background !== true && value.background !== true && value.task_id == null
      && !value.resume_from && !value.resume && !value.isolation
      && (value.cwd == null || value.cwd === scope.cwd)
      && (value.model == null || value.model === scope.modelId || value.model === scope.modelId.split('/').at(-1))
      && typeof value.description === 'string' && value.description.length <= 120) return { name, prompt: value.prompt, cwd: scope.cwd };
    return null;
  };
  const handled = new Set();
  const service = async (permitted) => {
    const pending = await call('agent_list_pending_interactions', { sessionId: null });
    assert(!pending.questions?.length && !pending.folderTrustRequests?.length && !pending.planApprovals?.length,
      'Unexpected interactive question, trust or plan; this fixed journey cannot approve it');
    for (const request of pending.permissions ?? []) {
      if (handled.has(request.requestId)) continue;
      const operation = parseOperation(request, permitted);
      if (!operation) {
        const ack = await call('agent_resolve_permission', { requestId: request.requestId, optionId: null, cancelled: true });
        let input = request.rawInput;
        if (typeof input === 'string') { try { input = JSON.parse(input); } catch { input = null; } }
        const args = input?.tool_input ?? input;
        report.permissions.push({ requestId: request.requestId, sessionId: request.sessionId, denied: true, acknowledged: ack,
          diagnostic: { variant: input?.variant, toolName: input?.tool_name, argumentKeys: args && typeof args === 'object' ? Object.keys(args) : [],
            model: args?.model, subagentType: args?.subagent_type, descriptionType: typeof args?.description,
            promptExactlyMatches: typeof args?.prompt === 'string' && args.prompt.replace(/[“”]/g, '"') === permitted?.prompt?.replace(/[“”]/g, '"'),
            background: args?.run_in_background ?? args?.background, isolation: args?.isolation,
            exactOwnedCwd: args?.cwd == null || args.cwd === scope.cwd, sameSession: request.sessionId === activeSessionId } });
        throw new Error('Unapproved tool/path denied; no shell or other action is authorized');
      }
      const card = document.getElementById(`permission-${request.requestId}`), allow = button(card, '允许一次');
      if (!visible(card) || !allow) continue;
      assert(request.options.some((option) => option.kind === 'allow'), 'Tool has no actual allow-once option');
      allow.click(); handled.add(request.requestId);
      report.permissions.push({ requestId: request.requestId, sessionId: request.sessionId, operation, via: 'production allow-once UI' });
    }
  };
  const finish = async (text, marker, start, permitted, modelId = scope.modelId) => {
    let observed = null;
    try { await wait(async () => {
      await service(permitted);
      const completion = completions.slice(start).filter((item) => item.sessionId === activeSessionId).at(-1);
      if (!completion) return null;
      assert(completion.stopReason === 'end_turn', `Actual model ended ${completion.stopReason}`);
      const state = await ownSession(activeSessionId);
      assert(state.status !== 'failed' && state.status !== 'stopped', `Session ended ${state.status}`);
      observed = { sessionId: activeSessionId, marker, completion, status: state.status,
        userMatches: users().some((line) => line.includes(text)), replyMatches: reply().includes(marker),
        visibleUsers: users().slice(-2).map(line => line.slice(0, 2500)), visibleReply: reply().slice(0, 2500) };
      return state.status === 'completed' && observed.userMatches && observed.replyMatches ? completion : null;
    }, 'actual Qwen completed + exact visible marker', 100000); }
    catch (error) {
      // Preserve the real final predicates for diagnosis; no synthesized event,
      // status change or weaker acceptance condition is used on a timeout.
      (report.finishFailures ??= []).push(observed ?? { sessionId: activeSessionId, marker,
        completions: completions.slice(start), visibleUsers: users().slice(-2), visibleReply: reply().slice(0, 2500) });
      throw error;
    }
    return { sessionId: activeSessionId, modelId, marker, completion: completions.slice(start).at(-1), visibleReply: reply().slice(0, 1500) };
  };
  const writeHandoff = (name, data) => call('write_text_file', { path: name, content: JSON.stringify(data), workspaceRoot: scope.cwd });
  const parentAck = (name) => wait(async () => {
    try { const data = JSON.parse(await call('read_text_file', { path: `${scope.cwd}/${name}`, cwd: scope.cwd, maxBytes: 8192 }));
      assert(data.nonce === nonce && data.passed === true, 'Parent physical barrier rejected'); return data; }
    catch (error) { if (/Parent physical barrier/.test(String(error))) throw error; return null; }
  }, 'independent parent physical barrier', 30000);
  try {
    assert(typeof home === 'string' && /^\/private\/(?:tmp|var\/folders\/[A-Za-z0-9_-]+\/[A-Za-z0-9_-]+\/T)\/echo-native-review-core-[A-Za-z0-9_-]+$/.test(home)
      && /^[a-f0-9]{32}$/.test(nonce || ''), 'Invalid private Core launch identity');
    assert(typeof window.__TAURI_INTERNALS__?.invoke === 'function', 'Production invoke is required');
    invoke = (command, args) => window.__TAURI_INTERNALS__.invoke(command, args);
    const context = await call('desktop_review_context'); scope = await call('desktop_review_core_manifest');
    assert(context.coreRequested === true && context.evaluations === 1 && Number.isInteger(context.pid) && context.pid > 0
      && context.home === home && context.phase === scope.phase && scope.schema === 1 && scope.home === home
      && scope.nonce === nonce && scope.cwd === `${home}/workspaces/default`
      && ['first-launch', 'restart'].includes(scope.phase) && ['echoagent-ojlab/Echo Reed', 'echoagent-review/claude-opus-5-5'].includes(scope.modelId), 'Native Core manifest/context mismatch');
    assert(Array.isArray(scope.officeFixtures) && scope.officeFixtures.length === 3
      && scope.officeFixtures.every((item, i) => item.format === ['xlsx', 'epub', 'ods'][i]
        && item.path === `${scope.cwd}/办公读取/中文 ${item.format}.${item.format}`
        && item.marker === `CORE-${item.format.toUpperCase()}-${nonce}`), 'Only fixed private Office fixtures are accepted');
    const stat = await call('path_stat', { path: scope.cwd, cwd: scope.cwd });
    assert(stat.exists && stat.kind === 'directory' && stat.absolute === scope.cwd, 'Canonical private workspace mismatch');
    const permission = await call('permission_mode_get', { sessionId: null });
    assert(permission.permissionMode === 'ask', 'Fixed Core journey requires real Ask approvals');
    approved = true; Object.assign(report, { phase: scope.phase, home, nonce, cwd: scope.cwd, nativeContext: context });
    const restart = scope.phase === 'restart';
    report.expected = restart ? scope.expected : { version: 1, home, nonce, cwd: scope.cwd, modelId: scope.modelId,
      ownedSessionIds: [], sessionId: null, forkedSessionId: null, projectForkedSessionId: null, deletedSessionId: null, project: null,
      officeFixtures: scope.officeFixtures, markers: {}, child: null, customModel: null };
    assert(report.expected?.version === 1 && report.expected.home === home && report.expected.cwd === scope.cwd
      && report.expected.nonce === nonce && report.expected.modelId === scope.modelId
      && Array.isArray(report.expected.ownedSessionIds) && report.expected.ownedSessionIds.every((id) => /^[A-Za-z0-9-]{1,128}$/.test(id)), 'Invalid owned restart expectation');
    record('core-native-isolation', { home, cwd: scope.cwd, pid: context.pid, evaluations: context.evaluations, fixedManifest: true, permissionMode: permission.permissionMode });
    const listen = async (event, handler) => {
      const id = window.__TAURI_INTERNALS__.transformCallback(handler);
      const eventId = await call('plugin:event|listen', { event, target: { kind: 'Any' }, handler: id });
      stops.push(async () => { window.__TAURI_EVENT_PLUGIN_INTERNALS__?.unregisterListener(event, eventId); await call('plugin:event|unlisten', { event, eventId }); });
    };
    await listen('agent://complete', ({ payload }) => { if (completions.length < 80) completions.push(payload); });
    await listen('agent://update', ({ payload }) => {
      if (['tool_call', 'tool_call_update'].includes(payload?.sessionUpdate ?? payload?.type) && toolUpdates.length < 80) toolUpdates.push(payload);
    });
    await listen('agent://subagent', ({ payload }) => { if (subagents.length < 40) subagents.push(payload); });
    if (restart) {
      await stage('core-restart-owned-catalog', async () => {
        const sessions = await catalog(), e = report.expected;
        assert(e.sessionId && e.forkedSessionId && e.deletedSessionId, 'First run did not finish fork/delete');
        assert(!sessions.some((item) => item.sessionId === e.deletedSessionId), 'Deleted test session reappeared');
        for (const id of [e.sessionId, e.forkedSessionId]) assert(sessions.some((item) => item.sessionId === id && !item.archived && item.cwd === scope.cwd), 'Retained session missing/archived/wrong cwd');
        return { sessionId: e.sessionId, forkedSessionId: e.forkedSessionId, deletedSessionAbsent: true };
      });
      await stage('core-restart-project-ui', async () => {
        const e = report.expected, p = (await call('projects_load')).find((item) => item.id === e.project?.id);
        assert(p && p.name === e.project.name && p.conversations.some((item) => item.sessionId === e.sessionId), 'Project association did not persist');
        await select(e.sessionId); assert(users().some((item) => item.includes(e.markers.recovery)), 'Restored source user content missing');
        assert(p.conversations.some((item) => item.sessionId === e.projectForkedSessionId && item.pendingProjectContext === false), 'Project fork association/context did not persist');
        await select(e.projectForkedSessionId);
        assert(users().some((item) => item.includes(e.markers.project)) && reply().includes(e.project.name)
          && reply().includes(e.markers.project), 'Actual project fork last turn did not restore');
        return { project: e.project, sessionId: e.sessionId, sourceUserVisible: true, projectForkedSessionId: selected(), projectForkTurnVisible: true };
      });
      await stage('core-restart-fork-ui', async () => { await select(report.expected.forkedSessionId);
        assert(users().some((item) => item.includes(report.expected.markers.recovery)), 'Fork history missing in actual UI');
        return { sessionId: selected(), sourceSessionId: report.expected.sessionId, inheritedUserVisible: true, newPromptSent: false }; });
      await stage('core-restart-office-fixtures', async () => {
        for (const item of scope.officeFixtures) { const info = await call('path_stat', { path: item.path, cwd: scope.cwd }); assert(info.exists && info.kind === 'file' && info.absolute === item.path, 'Office fixture missing'); }
        return { fixtures: scope.officeFixtures, physicalShaCheckedByParent: true, noModelCall: true };
      });
      await stage('core-restart-custom-model', async () => {
        const saved = report.expected.customModel, binding = await call('desktop_review_model_connection');
        assert(saved?.modelId && saved.providerId, 'First phase did not save a personal Qwen connection');
        const models = await call('providers_list'), provider = models.providers.find((item) => item.id === saved.providerId);
        assert(provider?.source === 'personal' && provider.baseUrl === binding.baseUrl
          && provider.allowInsecureHttp === (new URL(binding.baseUrl).protocol === 'http:') && !provider.managed && Boolean(provider.apiKey) === Boolean(binding.apiKey)
          && models.models.some((item) => item.modelId === saved.modelId && item.remoteModelId === binding.model && item.providerId === saved.providerId), 'Saved personal connection changed');
        await select(report.expected.sessionId);
        assert((await ownSession(report.expected.sessionId)).currentModelId === saved.modelId, 'Source session lost its switched personal model');
        assert(users().some((line) => line.includes(saved.marker)) && reply().includes(saved.marker), 'Actual custom-model last turn was not restored');
        return { modelId: saved.modelId, providerId: saved.providerId, selectedSessionId: selected(), noCredentials: !binding.apiKey, noNewModelCall: true };
      });
    } else {
      assert((await catalog()).length === 0 && (await call('projects_load')).length === 0, 'Refusing pre-existing sessions/projects');
      const ready = await stage('core-qwen-ready', () => wait(async () => {
        const value = await call('agent_auth_status'); return value.ready && value.runtimeReady && value.synchronized && value.runtimeModels?.includes(scope.modelId)
          ? { ready: true, runtimeReady: true, synchronized: true, modelId: scope.modelId } : null;
      }, 'real built-in Qwen readiness', 60000));
      assert(ready, 'Qwen unavailable; local restart evidence may still be attempted by parent');
      const choose = async (modelId = scope.modelId) => {
        (await wait(() => { const p = document.querySelector('.echo-composer .model-selector__trigger'); return visible(p) && !p.disabled ? p : null; }, 'production model chooser')).click();
        (await wait(() => [...document.querySelectorAll('.model-selector__item')].find((item) => visible(item)
          && item.dataset.modelId === modelId), 'actual model option')).click();
      };
      await choose();
      const e = report.expected, m = e.markers;
      Object.assign(m, { initial: `CORE-INITIAL-${nonce}`, queued: `CORE-QUEUED-${nonce}`, immediate: `CORE-NOW-${nonce}`,
        recovery: `CORE-RECOVER-${nonce}`, child: `CORE-CHILD-${nonce}`, project: `CORE-PROJECT-${nonce}` });
      const simple = (marker) => `本机隔离验收：请只回复“${marker}”，不要调用工具、读取或写入文件、访问网络或联系他人。`;
      const initial = simple(m.initial), start = completions.length; await send(initial);
      const first = await wait(async () => (await catalog()).find((item) => item.cwd === scope.cwd && !item.hidden), 'actual new session');
      assert(first.permissionMode === 'ask', 'Model session must retain actual Ask permission mode');
      e.sessionId = first.sessionId; e.ownedSessionIds.push(first.sessionId); activeSessionId = first.sessionId;
      const initialResponse = await stage('core-qwen-initial-response', () => finish(initial, m.initial, start));
      if (!initialResponse) await requireSettledAfterFailedStage('core-qwen-initial-response', e.sessionId);
      const control = await stage('core-queue-and-send-now', async () => {
        const slow = `隔离排队验收 ${nonce}：请直接连续撰写五千字中文说明，不调用工具。`;
        const before = completions.length; await send(slow);
        await wait(() => visible(document.querySelector('.echo-composer button[aria-label="停止生成"]')), 'real streaming turn');
        const queued = simple(m.queued), immediate = simple(m.immediate);
        await send(queued, '加入待发送队列');
        await wait(() => document.querySelector('.queue-row__text')?.textContent === queued, 'durable queue visible');
        await send(immediate, '中断当前回复并立即发送');
        await wait(async () => { await service(null); return completions.slice(before).find((item) => item.sessionId === e.sessionId && item.cancelTrigger === 'send_now'); }, 'actual send_now cancellation', 30000);
        await wait(async () => { await service(null); return canContinueAfterFailure((await ownSession(e.sessionId)).status,
          !document.querySelector('.queue-panel')) ? true : null; }, 'actual queue auto-drain completed and empty', 120000);
        assert(reply().includes(m.queued), 'Queued Qwen reply missing exact marker');
        const lines = users(), nowIndex = lines.findIndex((line) => line.includes(immediate)), queuedIndex = lines.findIndex((line) => line.includes(queued));
        assert(nowIndex >= 0 && queuedIndex > nowIndex && lines.filter((line) => line.includes(queued)).length === 1
          && lines.filter((line) => line.includes(immediate)).length === 1, 'Immediate/queued messages lost, duplicated or reordered');
        const turns = completions.slice(before).filter((item) => item.sessionId === e.sessionId);
        assert(turns.filter((item) => item.stopReason === 'end_turn').length >= 2, 'Both real Qwen responses must complete');
        assert([...document.querySelectorAll('.msg--assistant')].some((item) => item.textContent.includes(m.immediate)), 'Immediate Qwen reply missing');
        return { sessionId: e.sessionId, admittedViaUi: true, immediateBeforeQueued: true, noDuplicate: true, completions: turns };
      });
      if (!control) await requireSettledAfterFailedStage('core-queue-and-send-now', e.sessionId);
      await stage('core-stop-and-resend', async () => {
        const slow = `隔离停止验收 ${nonce}：请直接连续撰写五千字中文说明，不调用工具。`, before = completions.length;
        await send(slow); (await wait(() => { const b = document.querySelector('.echo-composer button[aria-label="停止生成"]'); return visible(b) && !b.disabled ? b : null; }, 'actual stop button')).click();
        const stop = await wait(async () => { await service(null); const event = completions.slice(before).find((item) => item.sessionId === e.sessionId && item.cancelTrigger === 'stop');
          return event && (await ownSession(e.sessionId)).status === 'stopped' ? event : null; }, 'actual stop ACK and terminal', 30000);
        await delay(1000); assert((await ownSession(e.sessionId)).status === 'stopped', 'Late event reversed stopped state');
        const text = simple(m.recovery), recoveryStart = completions.length; await send(text); const result = await finish(text, m.recovery, recoveryStart);
        return { stopped: stop, lateWaitMs: 1000, recovery: result };
      });
      const title = `隔离核心会话-${nonce}`;
      await stage('core-rename-search-ui', async () => {
        await menu(e.sessionId, '重命名'); set(await wait(() => document.querySelector('input[aria-label="会话名称"]'), 'rename input'), title);
        await delay(150);
        (await wait(() => document.querySelector('.context-menu__rename-save'), 'rename save')).click();
        await wait(async () => (await ownSession(e.sessionId)).title === title, 'native rename persistence'); e.title = title;
        (await wait(() => document.querySelector('button[aria-label="搜索"]'), 'search open')).click();
        set(await wait(() => document.querySelector('input[aria-label="搜索会话标题或内容"]'), 'real search input'), m.recovery);
        const hit = await wait(() => [...document.querySelectorAll('.conversation-search-modal__item--remote')]
          .find((item) => item.textContent.includes(m.recovery)), 'actual FTS content result', 30000);
        hit.click(); await wait(() => selected() === e.sessionId && !document.querySelector('.conversation-search-modal'), 'actual search selects original');
        return { sessionId: e.sessionId, title, query: m.recovery, remoteContentHit: true, selectedInUi: true };
      });
      const fork = async () => {
        await select(e.sessionId); const old = new Set((await catalog()).map((item) => item.sessionId));
        (await wait(() => document.querySelector('.rewind-bar button[title="分叉此会话"]'), 'fork UI')).click();
        (await wait(() => button([...document.querySelectorAll('[role="dialog"]')]
          .find(el => visible(el) && el.textContent.includes('分叉此会话？')), '创建分叉'), 'fork confirmation')).click();
        const next = await wait(async () => (await catalog()).find((item) => !old.has(item.sessionId) && !item.hidden), 'real fork ID');
        assert(next.sessionId !== e.sessionId && next.cwd === scope.cwd, 'Fork ID/workspace invalid'); e.ownedSessionIds.push(next.sessionId);
        await wait(() => selected() === next.sessionId, 'actual fork selection');
        await select(next.sessionId); activeSessionId = next.sessionId;
        await wait(() => users().some((line) => line.includes(m.recovery)), 'actual fork history replay visible'); return next;
      };
      await stage('core-fork-original-preserved', async () => {
        await writeHandoff('core-fork-ready.json', { nonce, sessionId: e.sessionId }); await parentAck('core-fork-before.json');
        const next = await fork(); e.forkedSessionId = next.sessionId;
        await writeHandoff('core-fork-done.json', { nonce, sessionId: e.sessionId, forkedSessionId: next.sessionId, marker: m.recovery });
        const proof = await parentAck('core-fork-after.json'); return { sessionId: e.sessionId, forkedSessionId: next.sessionId, proof, selectedInUi: true };
      });
      await select(e.sessionId);
      await stage('core-archive-restore-ui', async () => {
        await menu(e.sessionId, '归档'); await wait(async () => (await ownSession(e.sessionId)).archived === true
          && !document.querySelector(`[data-session-id="${CSS.escape(e.sessionId)}"]`), 'native archived and sidebar absent');
        (await wait(() => document.querySelector('button[aria-label="设置"]'), 'settings')).click();
        (await wait(() => [...document.querySelectorAll('.settings-navigation__item')]
          .find((el) => el.querySelector('.settings-navigation__label')?.textContent.trim() === '数据管理'), 'data settings entry')).click();
        (await wait(() => button(document.querySelector('[role="group"][aria-label="数据视图"]'), '已归档'), 'archive settings tab')).click();
        const row = await wait(() => [...document.querySelectorAll('.archived-settings__item')].find((el) => el.querySelector('.archived-settings__item-title')?.textContent === e.title), 'owned archived row');
        button(row, '恢复并打开').click(); await wait(async () => !(await ownSession(e.sessionId)).archived && selected() === e.sessionId, 'actual restored and selected');
        return { sessionId: e.sessionId, archived: true, restored: true, viaSettingsUi: true };
      });
      await stage('core-project-move-switch-ui', async () => {
        (await wait(() => [...document.querySelectorAll('.sidebar__nav-item')].find((el) => el.textContent.trim() === '项目'), 'project navigation')).click();
        (await wait(() => button(document, '新建项目'), 'project create')).click(); const name = `隔离核心项目${[...nonce.slice(0, 8)].map(char => '零一二三四五六七八九甲乙丙丁戊己'[parseInt(char, 16)]).join('')}`;
        set(await wait(() => document.querySelector('#create-project-name'), 'project name'), name);
        set(await wait(() => document.querySelector('#create-project-model'), 'project default model'), scope.modelId);
        await delay(150);
        (await wait(() => button(document.querySelector('[role="dialog"][aria-label="新建项目"]'), '创建项目'), 'project create confirm')).click();
        const project = await wait(async () => (await call('projects_load')).find((item) => item.name === name), 'real saved project');
        assert(project.cwd.startsWith(`${home}/`), 'Project cwd escaped own home'); e.project = { id: project.id, name, cwd: project.cwd,
          initialPlans: JSON.stringify(project.plans), initialTasks: JSON.stringify(project.tasks) };
        await select(e.sessionId); await menu(e.sessionId, '移入项目…');
        (await wait(() => [...document.querySelectorAll('input[name="target-project"]')].find((el) => el.value === project.id), 'target project radio')).click();
        (await wait(() => button(document.querySelector('.project-move-dialog'), '移入项目'), 'move confirm')).click();
        await wait(async () => (await call('projects_load')).find((item) => item.id === project.id)?.conversations.some((item) => item.sessionId === e.sessionId), 'real project association');
        const branch = await fork(); e.projectForkedSessionId = branch.sessionId;
        const linked = (await call('projects_load')).find((item) => item.id === project.id);
        assert(linked.conversations.find((item) => item.sessionId === e.sessionId)?.pendingProjectContext === true
          && linked.conversations.find((item) => item.sessionId === branch.sessionId)?.pendingProjectContext === true,
        'Project fork did not inherit the source pending contract');
        const text = `仅根据已提供的项目契约，输出两行：第一行逐字输出所属项目名称，保持全部字符，不添加空格或前缀；第二行只输出${m.project}。不要调用工具、读写文件或执行旧工作项。`;
        const before = completions.length; await send(text); const next = await finish(text, m.project, before);
        assert(reply().includes(name), 'Real project fork model reply did not identify its project');
        const saved = (await call('projects_load')).find((item) => item.id === project.id);
        assert(saved.conversations.find((item) => item.sessionId === branch.sessionId)?.pendingProjectContext === false
          && saved.conversations.find((item) => item.sessionId === e.sessionId)?.pendingProjectContext === true,
        'Fork turn consumed or changed the original project context');
        assert(JSON.stringify(saved.plans) === e.project.initialPlans && JSON.stringify(saved.tasks) === e.project.initialTasks,
          'Project fork cloned or changed old work items');
        await select(e.forkedSessionId); await select(e.sessionId);
        return { project: e.project, projectForkedSessionId: branch.sessionId, forkPendingBeforeTurn: true,
          forkPendingAfterTurn: false, sourcePendingUnchanged: true, projectTurn: next,
          originalCwdUnchanged: (await ownSession(e.sessionId)).cwd === scope.cwd, switchedThroughUi: true };
      });
      await stage('core-delete-owned-session-ui', async () => {
        const trash = await fork(); e.deletedSessionId = trash.sessionId; await menu(trash.sessionId, '永久删除');
        (await wait(() => {
          const dialog = [...document.querySelectorAll('[role="alertdialog"]')]
            .find(el => visible(el) && el.textContent.includes('永久删除对话') && el.textContent.includes(e.title));
          return button(dialog, '永久删除');
        }, 'explicit permanent delete dialog')).click();
        await wait(async () => !(await catalog()).some((item) => item.sessionId === trash.sessionId)
          && !document.querySelector(`[data-session-id="${CSS.escape(trash.sessionId)}"]`), 'native deletion + UI absent');
        await ownSession(e.sessionId); await ownSession(e.forkedSessionId); await ownSession(e.projectForkedSessionId);
        return { deletedSessionId: trash.sessionId, onlyOwnData: true, originalsRemain: true };
      });
      await select(e.sessionId);
      for (const item of scope.officeFixtures) await stage(`core-office-${item.format}-runtime-read`, async () => {
        const request = `隔离办公读取验收：仅调用 read_file 读取“${item.path}”。禁止终端、写入、其他路径或工具。根据实际工具内容只回复文件中的指纹及规定顺序，不可猜测。${item.format === 'epub'
          ? '按正文阅读顺序列出两个章节标题；不要输出导航内容。' : item.format === 'ods' ? '保留A与B之间空列；说明重复行和重复列均出现两次。' : '保留20位编号原值，不要转为科学计数。'}`;
        const before = completions.length, toolsBefore = toolUpdates.length, permissionsBefore = report.permissions.length; await send(request);
        const result = await finish(request, item.marker, before, { kind: 'read', path: item.path, relative: item.path.slice(scope.cwd.length + 1) });
        const updates = toolUpdates.slice(toolsBefore).filter((update) => update.sessionId === e.sessionId);
        const final = new Map();
        for (const update of updates) { const id = update.toolCallId ?? update.tool_call_id; if (id) final.set(id, { ...final.get(id), ...update }); }
        const strings = (value) => typeof value === 'string' ? [value]
          : Array.isArray(value) ? value.flatMap(strings) : value && typeof value === 'object' ? Object.values(value).flatMap(strings) : [];
        const reads = [...final.values()].filter((tool) => tool.status === 'completed'
          && /read_file|Read file|读取/i.test(JSON.stringify(tool)));
        const output = reads.map((tool) => {
          const content = strings(tool.content).join('\n'); return content || strings(tool.rawOutput ?? tool.raw_output).join('\n');
        }).join('\n');
        assert(reads.length === 1 && output.includes(item.marker), 'Exactly one successful actual read_file result with fingerprint is required');
        if (item.format === 'epub') assert(!output.includes(item.excludedMarker), 'Non-spine EPUB navigation entered read output');
        if (item.format === 'xlsx') assert(output.includes('00123456789012345678'), 'XLSX 20-digit text was lost');
        if (item.format === 'epub') assert(output.indexOf(`第一章 ${item.marker}`) >= 0
          && output.indexOf(`第二章 ${item.marker}`) > output.indexOf(`第一章 ${item.marker}`), 'EPUB spine order was not preserved');
        if (item.format === 'ods') assert(output.includes('A\t\tB') && output.includes('重复列\t重复列')
          && output.split(`重复行 ${item.marker}`).length - 1 === 2, 'ODS empty/repeated cells or rows were lost');
        return { ...result, path: item.path, permissions: report.permissions.slice(permissionsBefore), toolOutputFingerprint: true,
          updates: updates.map((update) => JSON.stringify(update).slice(0, 6000)).slice(0, 12) };
      });
      await stage('core-subagent-no-side-effects', async () => {
        const childPrompt = `请只回复“${m.child}”；不要调用任何工具、读取或写入文件、访问网络或联系他人。`;
        const input = { prompt: childPrompt, description: '隔离返回固定字符串', subagent_type: 'general-purpose',
          run_in_background: false, model: scope.modelId.split('/').at(-1) };
        const request = `隔离子代理验收：只调用task一次，完整输入严格使用这份JSON：${JSON.stringify(input)}。不要改写prompt或增添参数；禁止其他model、worktree、目录、task_id、resume和其他工具。取得真实子代理结果后只回复该结果。`;
        const before = completions.length, childBefore = subagents.length; await send(request);
        const result = await finish(request, m.child, before, { kind: 'child', prompt: childPrompt });
        const events = subagents.slice(childBefore).filter((event) => event.sessionId === e.sessionId);
        const child = events.find((event) => event.phase === 'finished' && event.status === 'completed' && event.output?.includes(m.child));
        assert(child && child.subagentId !== e.sessionId && events.some((event) => event.phase === 'spawned' && event.subagentId === child.subagentId), 'No actual spawned/completed child with exact output');
        e.child = { sessionId: child.subagentId, marker: m.child };
        const panel = await wait(() => document.querySelector('.subagent-panel[aria-label="子代理运行时"]'), 'actual child panel');
        assert(panel.textContent.includes('已完成'), 'Child completed state absent in UI'); return { ...result, child: e.child, events, noToolAuthorizedForChild: true };
      });
      await stage('core-custom-qwen-connection', async () => {
        const binding = await call('desktop_review_model_connection');
        const privateModel = scope.modelId === 'echoagent-review/claude-opus-5-5';
        assert(binding.model === scope.modelId.split('/').at(-1), 'Configured model binding differs');
        const http = new URL(binding.baseUrl).protocol === 'http:';
        const label = `隔离个人${privateModel ? '模型' : 'Qwen'}-${nonce}`, marker = `CORE-CUSTOM-${nonce}`;
        (await wait(() => document.querySelector('button[aria-label="设置"]'), 'settings for personal model')).click();
        (await wait(() => [...document.querySelectorAll('.settings-navigation__item')].find((el) => el.textContent.trim() === '模型'), 'model settings')).click();
        (await wait(() => button(document.querySelector('.model-connections'), '添加个人连接') || button(document, '添加个人连接'), 'add personal connection')).click();
        const editor = await wait(() => document.querySelector('[role="dialog"][aria-label="添加个人连接"]'), 'personal connection editor');
        set(editor.querySelector('#model-connection-label'), label); set(editor.querySelector('#model-base-url'), binding.baseUrl);
        if (http) {
          const consent = await wait(() => editor.querySelector('.model-connection-editor__http-consent input'), 'actual HTTP consent');
          assert(!consent.checked, 'HTTP consent must start unchecked'); consent.click();
          await wait(() => consent.checked, 'explicit owned HTTP consent');
        }
        assert(editor.querySelector('#model-api-key')?.value === '', 'Credential must start empty');
        if (binding.apiKey) set(editor.querySelector('#model-api-key'), binding.apiKey);
        set(editor.querySelector('#model-id-input'), binding.model);
        await delay(150);
        const testSave = button(editor, '测试并保存'); assert(testSave, 'Actual model test/save action missing'); testSave.click();
        const entry = await wait(async () => {
          const alert = editor.querySelector('.models-settings-panel__error, [role="alert"]');
          if (visible(alert)) throw new Error(`Actual connection test/save failed: ${alert.textContent.slice(0, 500)}`);
          if (document.contains(editor)) return null;
          const data = await call('providers_list'), provider = data.providers.find((item) => item.label === label);
          const model = provider && data.models.find((item) => item.providerId === provider.id && item.remoteModelId === binding.model);
          // credentialConfigured also means an anonymous service is usable;
          // apiKey is the actual persisted-secret indicator in this DTO.
          assert(!provider || provider.source === 'personal' && !provider.managed && Boolean(provider.apiKey) === Boolean(binding.apiKey)
            && provider.baseUrl === binding.baseUrl && provider.allowInsecureHttp === http, 'Personal connection scope/auth was changed');
          return model ? { providerId: provider.id, modelId: model.modelId, label, marker } : null;
        }, 'real model test then persisted normal connection', 90000);
        e.customModel = entry;
        (await wait(() => document.querySelector('button[aria-label="关闭设置"]'), 'close personal model settings')).click();
        await select(e.sessionId); await choose(entry.modelId);
        await wait(async () => (await ownSession(e.sessionId)).currentModelId === entry.modelId, 'actual personal session model switch', 60000);
        const text = simple(marker), before = completions.length; await send(text);
        const response = await finish(text, marker, before, null, entry.modelId);
        return { ...entry, testedAndSavedViaUi: true, explicitHttpConsent: http, noCredentials: !binding.apiKey,
          actualSessionModelId: (await ownSession(e.sessionId)).currentModelId, response };
      });
      await stage('core-automation-draft-navigation-exit', async () => {
        const marker = `CORE-DRAFT-${nonce}`;
        const jobsBefore = await call('automations_snapshot');
        assert(jobsBefore.automations.length === 0, 'Core draft test requires no existing jobs');
        const openAutomation = async () => {
          if (document.querySelector('.app__body--collapsed')) (await wait(() => document.querySelector('button[aria-label="展开侧边栏"]'), 'draft sidebar reveal')).click();
          (await wait(() => [...document.querySelectorAll('.sidebar__nav-item')].find(node => visible(node) && node.textContent.trim().startsWith('更多')), 'actual More navigation')).click();
          (await wait(() => [...document.querySelectorAll('button')].find(node => visible(node) && node.textContent.trim() === '定时任务'), 'actual automation navigation')).click();
        };
        await openAutomation();
        (await wait(() => button(document, '+ 添加自动化'), 'actual new automation form')).click();
        const name = await wait(() => document.querySelector('.atm-modal-input'), 'actual automation name');
        set(name, marker);
        (await wait(() => [...document.querySelectorAll('.sidebar__nav-item')].find(node => visible(node) && node.textContent.trim() === '专家·技能·连接器'), 'leave automation via actual navigation')).click();
        await wait(() => !document.contains(name), 'automation editor genuinely unmounted');
        await openAutomation();
        await wait(() => document.querySelector('.atm-modal-input')?.value === marker, 'actual draft restored after navigation');
        // This truthful incomplete checkpoint requests the production quit
        // path. It cannot qualify a parent pass if the guard lets us exit.
        await invoke('desktop_review_complete', { report: { version: 1, kind: 'core-draft-exit-checkpoint', passed: false,
          incomplete: true, home, nonce, nativeContext: context } });
        const exitDialog = await wait(() => [...document.querySelectorAll('[role="alertdialog"]')]
          .find(node => visible(node) && node.textContent.includes('退出前舍弃自动化草稿？')), 'actual native quit draft confirmation');
        assert(!document.querySelector('.app__exit-overlay'), 'Busy overlay blocks the quit decision');
        button(exitDialog, '继续编辑').click();
        await wait(() => !visible(exitDialog) && !document.querySelector('.app__exit-overlay'), 'quit cancelled and window retained');
        assert(document.querySelector('.atm-modal-input')?.value === marker, 'Cancelled quit lost the draft');
        const auth = await call('agent_auth_status'); assert(auth.ready && auth.runtimeReady && auth.synchronized, 'Cancelled quit stopped the Runtime');
        (await wait(() => button(document, '取消'), 'explicitly cancel synthetic draft')).click();
        const discard = await wait(() => [...document.querySelectorAll('[role="alertdialog"]')]
          .find(node => visible(node) && node.textContent.includes('舍弃未保存的修改？')), 'actual draft discard confirmation');
        button(discard, '舍弃修改').click();
        await wait(() => !document.querySelector('.atm-modal-input') && !visible(discard), 'synthetic draft discarded via UI');
        assert((await call('automations_snapshot')).automations.length === 0, 'Editing a draft created an actual task');
        await select(e.sessionId);
        return { marker, restoredViaNavigation: true, quitConfirmationViaUi: true, exitCancelledAndRuntimeReady: true,
          noTasksCreated: true, discardedViaUi: true };
      });
    }
  } catch (error) { report.errors.push(String(error?.message || error).slice(0, 1800)); }
  finally {
    for (const stop of stops) { try { await stop(); } catch (error) { report.errors.push(`listener cleanup: ${String(error).slice(0, 200)}`); } }
    const required = report.phase === 'restart' ? ['core-native-isolation', 'core-restart-owned-catalog', 'core-restart-project-ui', 'core-restart-fork-ui', 'core-restart-office-fixtures', 'core-restart-custom-model']
      : ['core-native-isolation', 'core-qwen-ready', 'core-qwen-initial-response', 'core-queue-and-send-now', 'core-stop-and-resend', 'core-rename-search-ui',
        'core-fork-original-preserved', 'core-archive-restore-ui', 'core-delete-owned-session-ui', 'core-project-move-switch-ui',
        'core-office-xlsx-runtime-read', 'core-office-epub-runtime-read', 'core-office-ods-runtime-read', 'core-subagent-no-side-effects', 'core-custom-qwen-connection', 'core-automation-draft-navigation-exit'];
    report.passed = approved && report.errors.length === 0 && required.every((id) => report.checks.filter((check) => check.id === id && check.result === 'passed').length === 1)
      && report.checks.length === required.length;
    report.finishedAt = new Date().toISOString();
    // NativeVerified is a parent assertion after normal real process exit and
    // independent physical readback, never a renderer self-attestation.
    if (approved) {
      let timer;
      try { await Promise.race([invoke('desktop_review_complete', { report }), new Promise((_accept, reject) => {
        timer = setTimeout(() => reject(new Error('Native report/graceful request timed out')), 20000);
      })]); } finally { clearTimeout(timer); }
    }
  }
  return report;
};
