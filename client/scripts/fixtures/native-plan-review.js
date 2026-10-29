// Explicit isolated acceptance only. Model plans, approvals and the TXT file
// come from production UI/IPC/Runtime; this helper never inserts store state.
window.__echoNativePlanReview = async ({ invoke, home, cwd, phase, expected, returnSessionId }) => {
  const startedAt = Date.now();
  const deadline = startedAt + 180000;
  const restart = phase === 'restart';
  const required = restart
    ? ['native-plan-isolation', 'native-plan-session-restore', 'native-plan-artifact-restore', 'native-plan-return-session']
    : ['native-plan-isolation', 'native-plan-session', 'native-plan-initial-approval', 'native-plan-local-revision',
      'native-plan-revised-approval', 'native-plan-approved-execution', 'native-plan-artifact', 'native-plan-return-session'];
  const report = { version: 1, phase, environment: 'native WebView + production IPC + real MiniMax model',
    passed: false, checks: [], errors: [], expected: restart ? expected || null : null, permissions: [],
    planFileReadback: 'Approval planContent is the real Runtime disk read; parent Node verifies isolated session plan.md physically' };
  const assert = (value, message) => { if (!value) throw new Error(message); };
  const errorText = (error) => String(error?.message || error).slice(0, 1800);
  const record = (id, evidence) => report.checks.push({ id, result: 'passed', evidence });
  const delay = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
  const path = (value) => {
    assert(typeof value === 'string' && value.startsWith('/') && !/[\x00-\x1f]/.test(value), 'Invalid absolute review path');
    const normalized = value.replace(/\\/g, '/').replace(/^\/tmp\//, '/private/tmp/').replace(/^\/var\//, '/private/var/').replace(/\/+$/, '');
    assert(!normalized.split('/').some((part) => part === '.' || part === '..'), 'Review path contains traversal');
    return normalized;
  };
  const bounded = async (operation, label, timeout = 15000) => {
    const remaining = Math.min(timeout, deadline - Date.now());
    assert(remaining > 0, 'Native plan review exceeded its 180s deadline');
    let timer;
    try { return await Promise.race([operation(), new Promise((_resolve, reject) => {
      timer = setTimeout(() => reject(new Error(`${label} timed out after ${remaining}ms`)), remaining);
    })]); } finally { clearTimeout(timer); }
  };
  const call = (command, args = {}, timeout) => bounded(() => invoke(command, args), command, timeout);
  const visible = (element) => element && element.getClientRects().length > 0;
  const selectedId = () => document.querySelector('[data-session-id] .sidebar__conv-select[aria-current="page"]')
    ?.closest('[data-session-id]')?.dataset.sessionId;
  const inputValue = (input, value) => {
    const prototype = input instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
    Object.getOwnPropertyDescriptor(prototype, 'value').set.call(input, value);
    input.dispatchEvent(new Event('input', { bubbles: true }));
  };
  const button = (scope, label) => [...scope.querySelectorAll('button')]
    .find((element) => visible(element) && element.textContent?.trim() === label && !element.disabled);
  const waitFor = async (check, label, timeout = 25000) => {
    const end = Math.min(deadline, Date.now() + timeout);
    while (Date.now() < end) {
      const value = await bounded(check, label, Math.min(15000, end - Date.now()));
      if (value) return value;
      await delay(Math.min(200, Math.max(0, end - Date.now())));
    }
    throw new Error(`${label} timed out`);
  };
  const sessionRow = (sessionId) => document.querySelector(`[data-session-id="${CSS.escape(sessionId)}"] .sidebar__conv-select`);
  const selectSession = async (sessionId) => {
    const collapsed = document.querySelector('[aria-controls="sidebar-task-list"][aria-expanded="false"]');
    collapsed?.click();
    (await waitFor(() => sessionRow(sessionId), 'Real session sidebar row')).click();
    await waitFor(() => selectedId() === sessionId, 'Real session selection');
    return waitFor(async () => {
      try { const info = await call('agent_session_info', { sessionId }); return info.sessionId === sessionId ? info : null; }
      catch (error) { if (errorText(error).includes('尚未建立可信工作区绑定')) return null; throw error; }
    }, 'Loaded Runtime session workspace', 45000);
  };
  const waitForNewTaskView = () => waitFor(() => {
    const homeView = document.querySelector('.home');
    const composer = homeView?.querySelector('.echo-composer');
    return visible(homeView) && visible(composer) && selectedId() === undefined ? composer : null;
  }, 'Actual new-task Home Composer');
  const chooseModel = async (modelId, composerSelector = '.echo-composer') => {
    const selector = `${composerSelector} .model-selector__trigger`;
    const picker = await waitFor(() => {
      const element = document.querySelector(selector);
      return visible(element) && !element.disabled ? element : null;
    }, 'Actual model selector');
    assert(picker.isConnected && document.querySelector(selector) === picker,
      'Model selector was replaced during navigation; refusing a detached trigger');
    picker.click();
    const option = await waitFor(() => [...document.querySelectorAll('.model-selector__item')]
      .find((item) => visible(item) && item.dataset.modelId === modelId), 'Actual model option');
    const label = option.querySelector('.model-selector__item-label')?.textContent;
    option.click();
    await waitFor(() => {
      const element = document.querySelector(selector);
      return visible(element) && !element.disabled && element.textContent?.includes(label);
    }, 'Actual model switch');
  };
  const send = async (text) => {
    const input = await waitFor(() => {
      const element = document.querySelector('.echo-composer textarea');
      return visible(element) && !element.disabled ? element : null;
    }, 'Ready production Composer');
    inputValue(input, text);
    (await waitFor(() => {
      const element = document.querySelector('button[aria-label="发送"]');
      return visible(element) && !element.disabled ? element : null;
    }, 'Enabled production Send')).click();
    await waitFor(() => [...document.querySelectorAll('.msg--user')].some((item) => visible(item) && item.textContent?.includes(text)), 'Actual user message');
  };
  const panel = async () => {
    if (!visible(document.querySelector('.chatview__plan-panel .plan-panel'))) {
      (await waitFor(() => document.querySelector('.chatview__plan-toggle'), 'Actual plan toggle')).click();
    }
    return waitFor(() => {
      const element = document.querySelector('.chatview__plan-panel .plan-panel');
      return visible(element) ? element : null;
    }, 'Production PlanPanel');
  };
  const pending = () => call('agent_list_pending_interactions', { sessionId: report.expected.sessionId });
  const handled = new Set();
  let allowArtifactWrite = false;
  const permissionOperation = (request) => {
    let raw = request.rawInput;
    if (typeof raw === 'string') { try { raw = JSON.parse(raw); } catch { return null; } }
    if (!raw || typeof raw !== 'object' || Array.isArray(raw)) return null;
    const wrapped = typeof raw.tool_name === 'string';
    let name = wrapped ? raw.tool_name.split(/:|__/).pop() : null;
    const input = wrapped ? raw.tool_input : raw;
    if (!input || typeof input !== 'object' || Array.isArray(input)) return null;
    // Direct native Edit/Write schemas are also supported; unknown named tools
    // never inherit this fallback, even if their arguments resemble a write.
    if (!name && input.variant === 'SearchReplace' && request.toolKind === 'edit'
      && typeof input.new_string === 'string' && typeof input.old_string === 'string') name = 'search_replace';
    if (!name && input.variant === 'Write' && request.toolKind === 'edit' && typeof input.content === 'string') name = 'write';
    if (!name && input.variant === 'ReadFile' && request.toolKind === 'read' && typeof input.path === 'string') name = 'read_file';
    if (!name && input.variant === 'TodoWrite' && request.toolKind === 'think' && Array.isArray(input.todos)) name = 'todo_write';
    if (!name && ['EnterPlanMode', 'ExitPlanMode'].includes(input.variant) && request.toolKind === 'other') {
      name = input.variant === 'EnterPlanMode' ? 'enter_plan_mode' : 'exit_plan_mode';
    }
    if (name === 'todo_write') {
      if (!Array.isArray(input.todos) || input.todos.length > 20 || !input.todos.every((item) => item && typeof item.id === 'string'
        && (item.content == null || typeof item.content === 'string')
        && (item.status == null || ['pending', 'in_progress', 'completed', 'cancelled'].includes(item.status)))) return null;
      return { name, owner: 'this session ACP plan entries' };
    }
    if (['enter_plan_mode', 'exit_plan_mode'].includes(name)) {
      if (Object.keys(input).some((key) => key !== 'variant')) return null;
      return { name, owner: 'this session Runtime plan control' };
    }
    if (!['search_replace', 'write', 'read_file'].includes(name)) return null;
    const inputPath = name === 'read_file' ? input.path ?? input.file_path : input.file_path;
    if (typeof inputPath !== 'string') return null;
    const absolute = inputPath.startsWith('/') ? path(inputPath) : path(`${cwd}/${inputPath}`);
    const ownedPlan = absolute.startsWith(`${home}/sessions/`) && absolute.endsWith(`/${report.expected.sessionId}/plan.md`);
    const artifact = absolute === report.expected.artifactPath;
    if (!ownedPlan && !artifact) return null;
    if (name !== 'read_file' && artifact) {
      const content = name === 'write' ? input.content : input.new_string;
      if (!allowArtifactWrite || typeof content !== 'string' || content.trim() !== report.expected.revisedMarker) return null;
    }
    return { name, path: absolute, owner: ownedPlan ? 'this session Runtime plan metadata' : 'explicit approved TXT artifact' };
  };
  const servicePending = async () => {
    assert(selectedId() === report.expected.sessionId, 'Plan task lost its selected session');
    const snapshot = await pending();
    assert(!snapshot.questions?.length && !snapshot.folderTrustRequests?.length, 'Model requested an unexpected question or folder trust; no automatic answer/trust');
    for (const request of snapshot.permissions || []) {
      if (handled.has(request.requestId)) continue;
      const operation = permissionOperation(request);
      if (!operation) {
        const rejected = await call('agent_resolve_permission', { requestId: request.requestId, optionId: null, cancelled: true });
        report.permissions.push({ requestId: request.requestId, title: request.title, result: 'rejected', acknowledged: rejected });
        throw new Error(`Unexpected tool or scope requested; denied: ${request.title}`);
      }
      assert(request.options.some((option) => option.kind === 'allow'), 'Safe operation lacks an allow-once option');
      const card = document.getElementById(`permission-${request.requestId}`);
      if (!visible(card)) continue;
      const allow = button(card, '允许一次');
      if (!allow) continue;
      allow.click();
      handled.add(request.requestId);
      report.permissions.push({ requestId: request.requestId, operation, result: 'allow-once clicked in production UI' });
    }
    const alert = document.querySelector('.plan-panel [role="alert"], .perm-inline [role="alert"]');
    if (visible(alert)) throw new Error(`Actual approval/permission failure: ${alert.textContent}`);
    const summary = (await call('agent_list_all_sessions', { includeArchived: true })).find((item) => item.sessionId === report.expected.sessionId);
    assert(summary && path(summary.cwd) === cwd, 'Plan session lost its isolated workspace');
    if (['failed', 'stopped'].includes(summary.status)) throw new Error(`Actual plan task ended ${summary.status}; ${document.querySelector('.chatview__error')?.textContent || ''}`);
    return { snapshot, summary };
  };
  let activeCheck = required[0];
  let sessionId;
  let didMutate = false;
  const originalSession = returnSessionId || selectedId();
  try {
    assert(invoke === window.__TAURI_INTERNALS__?.invoke, 'Production native invoke required');
    assert(phase === 'first-launch' || phase === 'restart', 'Explicit native review phase required');
    home = path(home); cwd = path(cwd);
    const context = await call('desktop_review_context');
    assert(context.phase === phase && path(context.home) === home && /^\/private\/(tmp\/|var\/folders\/)/.test(home), 'Helper requires the actual isolated temporary profile');
    assert(cwd === `${home}/workspaces/default`, 'Plan helper requires the already authorized default test workspace');
    const stat = await call('path_stat', { path: cwd, cwd });
    assert(stat.exists && stat.kind === 'directory' && path(stat.absolute) === cwd, 'Canonical isolated workspace mismatch');
    record(activeCheck, { home, cwd, nativeReview: true, onlySessionPlanMetadataAndUniqueTxt: true });
    if (restart) {
      activeCheck = required[1];
      assert(expected?.version === 1 && path(expected.home) === home && path(expected.cwd) === cwd
        && typeof expected.content === 'string' && expected.content.trim() === expected.revisedMarker,
        'Missing successful first-launch plan expectation');
      sessionId = expected.sessionId;
      assert(typeof sessionId === 'string' && /^[A-Za-z0-9-]+$/.test(sessionId), 'Invalid saved plan session ID');
      assert(path(expected.artifactPath).startsWith(`${cwd}/原生计划验收-`) && expected.artifactPath.endsWith('/批准后成果.txt'), 'Unscoped saved plan artifact');
      const info = await selectSession(sessionId);
      assert(path(info.cwd) === cwd, 'Restart resident workspace differs');
      const interactions = await pending();
      assert(!interactions.planApprovals?.length, 'Previously approved plan resumed as a pending approval');
      record(activeCheck, { sessionId, cwd: info.cwd, selectedInUi: true, noPendingPlanApproval: true, noNewExecutionRequested: true });
      activeCheck = required[2];
      const content = await call('read_text_file', { path: expected.artifactPath, cwd, maxBytes: 4096 });
      assert(content === expected.content, 'Real plan TXT changed across restart');
      record(activeCheck, { path: expected.artifactPath, content, exactReadback: true });
    } else {
      const auth = await call('agent_auth_status');
      const modelId = 'organization/MiniMax-M3.1-Flash-Preview';
      assert(auth.ready && auth.runtimeReady && auth.synchronized && auth.runtimeModels?.includes(modelId), 'Authorized MiniMax Runtime model is unavailable');
      const token = [...crypto.getRandomValues(new Uint8Array(12))].map((value) => value.toString(16).padStart(2, '0')).join('');
      const initialMarker = `旧计划尚未执行 ${token}`;
      const revisedMarker = `计划修订执行成功 ${token}`;
      const artifactPath = `${cwd}/原生计划验收-${token}/批准后成果.txt`;
      report.expected = { version: 1, home, cwd, modelId, token, initialMarker, revisedMarker, artifactPath,
        sessionId: null, initialApproval: null, revisedApproval: null, content: null };
      activeCheck = required[1];
      const before = new Set((await call('agent_list_all_sessions', { includeArchived: true })).map((item) => item.sessionId));
      (await waitFor(() => button(document, '新建任务'), 'Real new task entry')).click();
      // React commits this navigation after the click handler. Without this
      // gate, an awaited lookup can retain the old ChatView trigger and click
      // it after that Composer has been unmounted, so no menu ever opens.
      await waitForNewTaskView();
      await chooseModel(modelId, '.home .echo-composer');
      const warmup = `原生计划准备 ${token}：请只回复“计划会话已准备”，不要调用工具或读写任何文件。`;
      await send(warmup);
      const created = await waitFor(async () => (await call('agent_list_all_sessions', { includeArchived: true }))
        .find((item) => !before.has(item.sessionId) && item.sessionId === selectedId()), 'Actual new plan session', 30000);
      sessionId = created.sessionId;
      report.expected.sessionId = sessionId;
      assert(path(created.cwd) === cwd, 'New task bound to another workspace');
      await waitFor(async () => {
        const summary = (await call('agent_list_all_sessions', { includeArchived: true })).find((item) => item.sessionId === sessionId);
        assert(summary && !['failed', 'stopped'].includes(summary.status), 'Warmup model task failed');
        return summary.status === 'completed' ? summary : null;
      }, 'Real model warmup completion', 45000);
      const mode = await call('permission_mode_set', { sessionId, mode: 'ask' });
      assert(mode.permissionMode === 'ask' && mode.runtimeSynced, 'Cannot safely require per-operation approval for the isolated plan task');
      await call('set_plan_mode', { sessionId, enabled: true });
      didMutate = true;
      record(activeCheck, { sessionId, cwd, modelId, permissionMode: mode.permissionMode, runtimeSynced: mode.runtimeSynced, planModeRequested: true });
      activeCheck = required[2];
      assert(!(await call('path_stat', { path: artifactPath, cwd })).exists, 'Unique artifact unexpectedly already exists');
      const prompt = `原生计划专项 ${token}。你已进入计划模式。最终任务只是在“${artifactPath}”创建UTF-8纯文本，唯一内容是“${initialMarker}”。现在不能执行该成果写入。请先用todo_write生成至少一条pending计划步骤，步骤包含文件路径和上述完整内容；再用search_replace或write把计划写入系统指定的本会话plan.md，最后调用exit_plan_mode，让桌面真正等待用户审批。之后用户可能修改步骤；必须采用修订内容并再次请求exit_plan_mode审批。只使用计划/Todo和search_replace、write、read_file工具；不要终端、MCP、网络、外部联系人或其它文件。`;
      await send(prompt);
      const initial = await waitFor(async () => {
        const { snapshot } = await servicePending();
        return snapshot.planApprovals?.find((item) => item.sessionId === sessionId && item.planContent?.includes(initialMarker)) || null;
      }, 'Actual first Runtime exit_plan_mode approval', 70000);
      report.expected.initialApproval = { requestId: initial.requestId, toolCallId: initial.toolCallId, planContent: initial.planContent };
      const initialPanel = await panel();
      const rows = [...initialPanel.querySelectorAll('.plan-panel__row-content')];
      assert(rows.length >= 1 && rows.some((item) => item.textContent?.includes(initialMarker)), 'Model did not emit a real ACP plan entry for the original task');
      assert(!(await call('path_stat', { path: artifactPath, cwd })).exists, 'Agent executed artifact before the first approval');
      record(activeCheck, { sessionId, ...report.expected.initialApproval, entries: rows.map((item) => item.textContent), artifactAbsent: true });
      activeCheck = required[3];
      const row = rows.find((item) => item.textContent?.includes(initialMarker));
      row.dispatchEvent(new MouseEvent('dblclick', { bubbles: true }));
      const editor = await waitFor(() => initialPanel.querySelector('.plan-panel__row-edit-input'), 'Actual plan step editor');
      const revisedStep = `批准后只用search_replace或write创建“${artifactPath}”，UTF-8唯一内容为“${revisedMarker}”。先更新plan.md和todo_write并再次exit_plan_mode等待批准，批准前不得写成果；不调用终端、MCP或其他工具。`;
      inputValue(editor, revisedStep);
      editor.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
      const submit = await waitFor(() => button(initialPanel, '同步修改并重新审批'), 'Actual dirty plan approval entry');
      assert(initialPanel.textContent.includes(revisedMarker), 'Saved local plan did not contain the revised content');
      submit.click();
      record(activeCheck, { originalRequestId: initial.requestId, revisedStep, clicked: '同步修改并重新审批', productionPlanPanel: true });
      activeCheck = required[4];
      const revised = await waitFor(async () => {
        const { snapshot } = await servicePending();
        return snapshot.planApprovals?.find((item) => item.sessionId === sessionId && item.requestId !== initial.requestId
          && item.planContent?.includes(revisedMarker)) || null;
      }, 'Actual revised Runtime approval with adopted local content', 70000);
      report.expected.revisedApproval = { requestId: revised.requestId, toolCallId: revised.toolCallId, planContent: revised.planContent };
      assert(!(await call('path_stat', { path: artifactPath, cwd })).exists, 'Artifact executed before reapproval');
      const revisedPanel = await panel();
      assert(revisedPanel.textContent.includes(revisedMarker), 'New model plan is not visible in the production panel');
      const approve = await waitFor(() => button(revisedPanel, '批准执行'), 'Actual clean revised approval');
      record(activeCheck, { sessionId, ...report.expected.revisedApproval, differentRequest: true, artifactAbsent: true });
      activeCheck = required[5];
      approve.click();
      allowArtifactWrite = true;
      const finished = await waitFor(async () => {
        const { snapshot, summary } = await servicePending();
        return !snapshot.planApprovals?.length && summary.status === 'completed' ? summary : null;
      }, 'Real approved plan task completion', 70000);
      record(activeCheck, { sessionId, status: finished.status, modelId, realPlanApprovalDispatched: true });
      activeCheck = required[6];
      const artifactStat = await call('path_stat', { path: artifactPath, cwd });
      assert(artifactStat.exists && artifactStat.kind === 'file' && path(artifactStat.absolute) === artifactPath, 'Actual Agent TXT is missing or outside its unique scoped path');
      const content = await call('read_text_file', { path: artifactPath, cwd, maxBytes: 4096 });
      assert(content.trim() === revisedMarker && !content.includes(initialMarker), 'Agent executed the original plan or produced incorrect TXT content');
      report.expected.content = content;
      record(activeCheck, { path: artifactPath, content, oldPlanNotExecuted: true, producedByRealAgentTool: true });
    }
    activeCheck = required[required.length - 1];
    assert(originalSession && originalSession !== sessionId, 'Missing independent owning session to return to');
    await selectSession(originalSession);
    await chooseModel('echoagent-ojlab/Echo Reed');
    record(activeCheck, { sessionId: originalSession, selectedInUi: true, modelId: 'echoagent-ojlab/Echo Reed' });
  } catch (error) {
    const message = errorText(error);
    report.errors.push({ check: activeCheck, error: message });
    report.checks.push({ id: activeCheck, result: 'failed', evidence: { error: message, sessionId } });
    // Cancel only this helper's actual task; never abandon another session's
    // approval or silently treat cancellation as a successful plan journey.
    if (sessionId && !restart && didMutate) {
      try {
        let timer;
        try { await Promise.race([invoke('agent_cancel', { sessionId }), new Promise((_resolve, reject) => {
          timer = setTimeout(() => reject(new Error('Helper task cancel timed out')), 5000);
        })]); report.cancelCleanup = { requested: true, sessionId }; }
        finally { clearTimeout(timer); }
      } catch (reason) { report.cancelCleanup = { requested: true, sessionId, error: errorText(reason) }; }
    }
  }
  for (const id of required) if (!report.checks.some((item) => item.id === id)) {
    report.checks.push({ id, result: 'blocked', evidence: { reason: 'An earlier actual Native plan stage failed; no synthetic success' } });
  }
  report.passed = report.checks.every((item) => item.result === 'passed') && report.errors.length === 0;
  report.durationMs = Date.now() - startedAt;
  return report;
};
