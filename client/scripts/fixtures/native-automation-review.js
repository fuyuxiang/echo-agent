// Loaded by the isolated native review only. This helper does not auto-run,
// replace IPC, synthesize model output, or enable a scheduled/background fire.
// The caller must persist returned `expected` even when `passed` is false so
// the restart can inspect whatever the first launch actually saved.
window.__echoNativeAutomationReview = async ({ invoke, home, cwd, phase, expected }) => {
  const restarting = phase === 'restart';
  const suffix = restarting ? '-restore' : '';
  const required = [
    'native-automation-isolation',
    `native-automation-paused-once${suffix}`,
    `native-automation-run-record${suffix}`,
    `native-automation-session-workspace${suffix}`,
    `native-automation-model-response${suffix}`,
  ];
  const report = {
    version: 1, phase, environment: 'native WebView + production IPC',
    passed: false, checks: [], errors: [], expected: restarting ? expected || null : null,
    observed: {},
  };
  const deadline = Date.now() + (restarting ? 90_000 : 180_000);
  const delay = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
  const record = (id, result, evidence) => report.checks.push({ id, result, evidence });
  const errorText = (error) => String(error?.message || error).slice(0, 2000);
  const assert = (condition, message) => { if (!condition) throw new Error(message); };
  const bounded = async (operation, label, limit = 15_000) => {
    const remaining = Math.min(limit, deadline - Date.now());
    assert(remaining > 0, 'Native automation review deadline exceeded');
    let timer;
    try {
      return await Promise.race([
        operation(),
        new Promise((_, reject) => {
          timer = setTimeout(() => reject(new Error(`${label} timed out after ${remaining}ms`)), remaining);
        }),
      ]);
    } finally { clearTimeout(timer); }
  };
  const call = (command, args, limit) => bounded(() => invoke(command, args), command, limit);
  const waitFor = async (check, label, limit = 20_000) => {
    const end = Math.min(deadline, Date.now() + limit);
    while (Date.now() < end) {
      const value = await bounded(check, label, Math.min(15_000, end - Date.now()));
      if (value) return value;
      await delay(Math.min(250, Math.max(0, end - Date.now())));
    }
    throw new Error(`${label} timed out`);
  };
  const stage = async (id, operation) => {
    try {
      const evidence = await operation();
      record(id, 'passed', evidence);
      return evidence;
    } catch (error) {
      const message = errorText(error);
      report.errors.push({ check: id, error: message });
      record(id, 'failed', { error: message, record: report.observed.record || null });
      return null;
    }
  };
  // Comparing native-canonical cwd with native home catches a symlink escape;
  // reject ambiguous spellings before calling any command that can write.
  const absolutePath = (value) => {
    assert(typeof value === 'string' && value.length > 1 && !/[\x00-\x1f,]/.test(value), 'Invalid isolation path');
    const normalized = value.replace(/\\/g, '/').replace(/\/$/, '');
    assert(/^(\/[^/]|[A-Za-z]:\/)/.test(normalized), 'Isolation path must be absolute');
    assert(!normalized.split('/').some((part) => part === '.' || part === '..'), 'Isolation path contains traversal');
    return normalized;
  };
  const recordFields = (item) => ({
    id: item.id, automationId: item.automationId, automationName: item.automationName,
    status: item.status, startedAt: item.startedAt, finishedAt: item.finishedAt || null,
    sessionId: item.sessionId || null, cwd: item.cwd || null, modelId: item.modelId || null,
    error: item.error || null, archived: !!item.archived, scheduledFor: item.scheduledFor || null,
  });
  const sameAutomation = (actual, saved) => actual && saved
    && ['id', 'name', 'prompt', 'cwds', 'status', 'modelId', 'permissionMode', 'scheduleType',
      'scheduledDate', 'scheduledTime', 'pushToWeChat', 'createdAt'].every((key) => actual[key] === saved[key])
    && actual.skills.length === 0 && actual.connectorIds.length === 0
    && !actual.expertId && !actual.expertName && !actual.modelIsThinking && !actual.nextRunAt;
  let isolated = false;
  try {
    const isolation = await stage(required[0], async () => {
      assert(typeof invoke === 'function', 'Production native invoke is unavailable');
      assert(invoke === window.__TAURI_INTERNALS__?.invoke, 'Helper requires the actual Native invoke function');
      assert(phase === 'first-launch' || phase === 'restart', 'Explicit review phase is required');
      const context = await call('desktop_review_context');
      const nativeHome = absolutePath(context.home);
      const claimedHome = absolutePath(home);
      const claimedCwd = absolutePath(cwd);
      assert(context.phase === phase && nativeHome === claimedHome, 'Helper does not match the isolated native launch');
      assert(claimedCwd.startsWith(`${nativeHome}/`), 'Automation cwd must be a child of the isolated home');
      const stat = await call('path_stat', { path: cwd, cwd });
      const canonicalCwd = absolutePath(stat.absolute);
      assert(stat.exists && stat.kind === 'directory' && canonicalCwd.startsWith(`${nativeHome}/`),
        'Native-canonical workspace is missing or outside isolated home');
      assert(canonicalCwd === claimedCwd, 'Pass the native-canonical workspace to the helper');
      if (restarting) {
        assert(expected?.version === 1 && expected.home === nativeHome && expected.cwd === canonicalCwd,
          'Missing or mismatched first-launch automation expectation');
      }
      return { home: nativeHome, cwd: canonicalCwd, phase: context.phase,
        isolatedReviewGate: true, osLoginWritesSuppressedByNativeReview: true };
    });
    if (!isolation) return report;
    isolated = true;
    const snapshot = await call('automations_snapshot');
    assert(Array.isArray(snapshot.automations) && Array.isArray(snapshot.records), 'Invalid native automation snapshot');
    if (restarting) {
      await stage(required[1], async () => {
        const automation = snapshot.automations.find((item) => item.id === expected.automationId);
        assert(sameAutomation(automation, expected.automation), 'Saved paused once task differs after restart');
        report.observed.automation = automation;
        return { automation, notRescheduled: true };
      });
      await stage(required[2], async () => {
        const records = snapshot.records.filter((item) => item.automationId === expected.automationId);
        const run = records.find((item) => item.id === expected.recordId);
        assert(run && expected.record, 'First launch has no durable execution record to compare');
        report.observed.record = recordFields(run);
        assert(records.length === 1 && JSON.stringify(recordFields(run)) === JSON.stringify(expected.record),
          'Durable execution record changed or an extra run appeared after restart');
        assert(['success', 'failed'].includes(run.status), 'First launch did not finish; pending recovery is not proven success');
        return { record: recordFields(run), noAdditionalRun: true, firstRunSucceeded: run.status === 'success' };
      });
    } else {
      assert(snapshot.automations.length === 0 && snapshot.records.length === 0,
        'First launch automation store is not empty; refusing to mutate existing tasks');
      // randomUUID is restricted to secure contexts; the macOS custom scheme
      // may still expose the standard getRandomValues API without it.
      const token = typeof crypto.randomUUID === 'function' ? crypto.randomUUID()
        : [...crypto.getRandomValues(new Uint8Array(16))]
          .map((value) => value.toString(16).padStart(2, '0')).join('');
      const date = new Date();
      date.setDate(date.getDate() + 1);
      const pad = (number) => String(number).padStart(2, '0');
      const reply = `定时验收成功 ${token}`;
      const automation = {
        id: '', name: `原生暂停单次验收 ${token}`,
        prompt: `原生定时验收 ${token}：请只回复“${reply}”。不要调用工具、读取文件、联系他人或修改任何数据。`,
        cwds: isolation.cwd, status: 'PAUSED', modelId: 'echoagent-ojlab/Echo Reed',
        modelIsThinking: false, skills: [], connectorIds: [], permissionMode: 'default',
        scheduleType: 'once', schedule: { freq: 'DAILY', interval: 1,
          byday: ['MO', 'TU', 'WE', 'TH', 'FR', 'SA', 'SU'], bymonthday: [], bymonth: [],
          byhour: 12, byminute: 0, intervalHours: 1 },
        scheduledDate: `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}`,
        scheduledTime: '12:00', pushToWeChat: false, createdAt: '',
      };
      report.expected = { version: 1, home: isolation.home, cwd: isolation.cwd,
        modelId: automation.modelId, prompt: automation.prompt, reply,
        automationId: null, recordId: null, sessionId: null, automation: null, record: null };
      const saved = await stage(required[1], async () => {
        const result = await call('automations_save', { automation });
        // Save IDs before readback so a failed later check still yields a useful
        // restart expectation. Never retry the creating/running mutation.
        report.expected.automationId = result.id;
        report.expected.automation = result;
        report.observed.automation = result;
        assert(typeof result.id === 'string' && result.id.length > 0, 'Save did not return a task ID');
        assert(result.status === 'PAUSED' && result.scheduleType === 'once' && result.cwds === isolation.cwd
          && result.modelId === automation.modelId && result.permissionMode === 'default'
          && result.name === automation.name && result.prompt === automation.prompt
          && result.scheduledDate === automation.scheduledDate && result.scheduledTime === automation.scheduledTime
          && !result.pushToWeChat && !result.nextRunAt, 'Native save changed task input, isolation or paused status');
        const readback = (await call('automations_snapshot')).automations.find((item) => item.id === result.id);
        assert(sameAutomation(readback, result), 'Paused once task did not read back through production store');
        return { automation: result, futureDate: result.scheduledDate, automaticFireEnabled: false };
      });
      if (saved) {
        await stage(required[2], async () => {
          const auth = await call('agent_auth_status');
          report.observed.readiness = { ready: auth.ready, runtimeReady: auth.runtimeReady,
            synchronized: auth.synchronized, qwenAvailable: auth.runtimeModels?.includes(automation.modelId), reason: auth.reason };
          assert(auth.ready && auth.runtimeReady && auth.synchronized && auth.runtimeModels?.includes(automation.modelId),
            `Qwen Runtime not ready: ${auth.reason || 'model unavailable'}`);
          const runId = await call('automations_run', { id: report.expected.automationId });
          assert(typeof runId === 'string' && runId.length > 0, 'Manual run did not return a durable record ID');
          report.expected.recordId = runId;
          const run = await waitFor(async () => {
            const current = await call('automations_snapshot');
            const item = current.records.find((entry) => entry.id === runId);
            if (item) {
              report.observed.record = recordFields(item);
              report.expected.record = recordFields(item);
              report.expected.sessionId = item.sessionId || null;
            }
            return item && ['success', 'failed'].includes(item.status) ? item : null;
          }, 'Manual automation terminal record', 120_000);
          assert(run.automationId === report.expected.automationId && run.cwd === isolation.cwd
            && run.modelId === automation.modelId && !!run.sessionId && !!run.finishedAt,
          'Terminal run is missing its automation/session/workspace/model binding');
          assert(run.status === 'success' && !run.error, `Automation actually failed: ${run.error || run.status}`);
          const current = await call('automations_snapshot');
          const task = current.automations.find((item) => item.id === report.expected.automationId);
          assert(sameAutomation(task, report.expected.automation)
            && current.records.filter((item) => item.automationId === task.id).length === 1,
          'Manual run activated/changed the task or dispatched more than once');
          return { record: recordFields(run), queueAckIsNotCompletion: true, taskRemainsPaused: true };
        });
      }
    }
    const sessionId = report.expected?.sessionId;
    if (sessionId) {
      await stage(required[3], async () => {
        const session = await waitFor(async () => (await call('agent_list_all_sessions', { includeArchived: true }))
          .find((item) => item.sessionId === sessionId), 'Automation persisted session');
        // Persisted summary.json may store the Runtime's short model ID; the
        // run record above still verifies the requested qualified binding.
        const persistedModelMatches = session.currentModelId === report.expected.modelId
          || session.currentModelId === report.expected.modelId.split('/').slice(-1)[0];
        assert(session.cwd === isolation.cwd && persistedModelMatches,
          'Automation session persisted outside the bound workspace/model');
        const selector = `[data-session-id="${CSS.escape(sessionId)}"] .sidebar__conv-select`;
        const expandTasks = document.querySelector('[aria-controls="sidebar-task-list"][aria-expanded="false"]');
        if (expandTasks) expandTasks.click();
        const row = await waitFor(() => document.querySelector(selector), 'Actual automation session sidebar row');
        row.click();
        await waitFor(() => document.querySelector(`${selector}[aria-current="page"]`), 'Automation session selected in UI');
        await waitFor(() => [...document.querySelectorAll('.msg--user')]
          .some((item) => item.getClientRects().length > 0 && item.textContent?.includes(report.expected.prompt)),
        'Automation user message replay');
        let lastInfoError;
        const info = await waitFor(async () => {
          try {
            const value = await call('agent_session_info', { sessionId });
            return value.sessionId === sessionId ? value : null;
          } catch (error) {
            // Native load replays history before it publishes the resident
            // workspace binding. Retry that one transient state, not an actual
            // Runtime failure, and never infer readiness from visible history.
            lastInfoError = errorText(error);
            if (!lastInfoError.includes('尚未建立可信工作区绑定')) throw error;
            return null;
          }
        }, 'Automation resident sessionWorkspace', 45_000).catch((error) => {
          throw new Error(`${errorText(error)}${lastInfoError ? `; last native error: ${lastInfoError}` : ''}`);
        });
        assert(info.sessionId === sessionId && info.cwd === isolation.cwd,
          'Resident Runtime sessionWorkspace differs from the persisted run cwd');
        report.observed.sessionWorkspace = { sessionId, cwd: info.cwd, currentModelId: session.currentModelId };
        return { ...report.observed.sessionWorkspace, selectedInUi: true, userMessageVisible: true, restarted: restarting };
      });
      await stage(required[4], async () => {
        assert(document.querySelector(`[data-session-id="${CSS.escape(sessionId)}"] .sidebar__conv-select[aria-current="page"]`),
          'Automation session is not selected; another task reply is not acceptable evidence');
        const answer = await waitFor(() => {
          const user = [...document.querySelectorAll('.msg--user')].filter((item) => item.getClientRects().length > 0)
            .find((item) => item.textContent?.includes(report.expected.prompt));
          if (!user) return null;
          const text = [...document.querySelectorAll('.msg--assistant .msg__body > .markdown-body[data-md-theme="loose"]')]
            .filter((item) => item.getClientRects().length > 0
              && (user.compareDocumentPosition(item) & Node.DOCUMENT_POSITION_FOLLOWING))
            .map((item) => item.innerText || '').join('\n');
          return text.includes(report.expected.reply) ? text : null;
        }, 'Real Qwen automation reply', 20_000);
        assert(report.observed.record?.status === 'success' && !report.observed.record.error,
          'A rendered response cannot override a failed or unfinished run');
        const toolCalls = [...document.querySelectorAll('.toolcall')].filter((item) => item.getClientRects().length > 0);
        assert(toolCalls.length === 0, 'The no-tool acceptance prompt unexpectedly used tools');
        report.observed.answer = answer.slice(0, 2000);
        return { sessionId, modelId: report.expected.modelId, answer: report.observed.answer,
          expectedReply: report.expected.reply, toolCalls: toolCalls.length, restarted: restarting };
      });
    }
  } catch (error) {
    report.errors.push({ check: 'native-automation-review', error: errorText(error) });
  } finally {
    for (const id of required) {
      if (!report.checks.some((item) => item.id === id)) {
        record(id, 'blocked', { reason: isolated ? 'Earlier native stage did not produce the required task/run/session'
          : 'Isolated native launch was not verified', expected: report.expected });
      }
    }
    report.passed = report.checks.length === required.length
      && report.checks.every((item) => item.result === 'passed') && report.errors.length === 0;
  }
  return report;
};
