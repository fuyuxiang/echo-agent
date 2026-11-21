// Embedded fixed factory. Every operation uses real native IPC/DOM/events.
window.__echoNativeExitRecoveryReview = async function () {
  if (window.__echoNativeExitRecoveryStarted) return;
  window.__echoNativeExitRecoveryStarted = true;
  const delay = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
  const wait = async (read, label, timeout = 45000) => {
    const end = Date.now() + timeout;
    while (Date.now() < end) { const value = await read(); if (value) return value; await delay(150); }
    throw new Error(`Timed out waiting for ${label}`);
  };
  const require = (condition, message) => { if (!condition) throw new Error(message); };
  const report = { kind: 'real-native-exit-review', checks: [], errors: [], passed: false };
  let invoke, scope, context, scopeApproved = false;
  const stops = [];
  const record = (id, evidence) => report.checks.push({ id, result: 'passed', evidence });
  try {
    await wait(() => window.__TAURI_INTERNALS__?.invoke, 'native invoke');
    invoke = (command, args) => window.__TAURI_INTERNALS__.invoke(command, args);
    context = await wait(async () => {
      try { const value = await invoke('native_exit_review_context'); return value.evaluations === 1 ? value : null; }
      catch { return null; }
    }, 'private native context');
    scope = context.scope;
    require(scope?.schema === 1 && /^\/private\/(?:tmp|var\/folders\/[a-zA-Z0-9_-]+\/[a-zA-Z0-9_-]+\/T)\/echo-native-review-exit-[a-zA-Z0-9_-]+$/.test(scope.home)
      && Number.isInteger(context.pid) && context.pid > 0
      && scope.cwd === `${scope.home}/workspaces/default` && /^[a-f0-9]{32}$/.test(scope.nonce)
      && ['init-failure', 'flush-failure', 'restart'].includes(scope.phase)
      && (scope.phase === 'restart' ? typeof scope.expectedSessionId === 'string' : scope.expectedSessionId === null),
    'Native exit scope is invalid');
    scopeApproved = true;
    Object.assign(report, { phase: scope.phase, nonce: scope.nonce, nativeContext: context, pid: context.pid, home: scope.home, cwd: scope.cwd });
    record('native-exit-isolation', { pid: context.pid, evaluations: context.evaluations, scope });
    const freshContext = () => invoke('native_exit_review_context');
    const write = (name, value) => invoke('write_text_file', { path: `${scope.cwd}/${name}`, content: JSON.stringify(value), workspaceRoot: scope.cwd });
    const control = async (stage) => {
      try {
        const value = JSON.parse(await invoke('read_text_file', { path: `${scope.cwd}/exit-control.json`, cwd: scope.cwd, maxBytes: 8192 }));
        return value.nonce === scope.nonce && value.stage === stage ? value : null;
      } catch { return null; }
    };
    const listen = async (event, callback) => {
      require(typeof window.__TAURI_INTERNALS__.transformCallback === 'function', 'Native event callbacks unavailable');
      const eventId = await invoke('plugin:event|listen', { event, target: { kind: 'Any' }, handler: window.__TAURI_INTERNALS__.transformCallback(callback) });
      stops.push(async () => {
        window.__TAURI_EVENT_PLUGIN_INTERNALS__?.unregisterListener(event, eventId);
        await invoke('plugin:event|unlisten', { event, eventId });
      });
    };
    if (scope.phase === 'init-failure') {
      const error = await wait(() => [...document.querySelectorAll('.app__notice')]
        .find((element) => /初始化(?:失败|未完成)/.test(element.textContent || ''))?.textContent, 'actual initialization failure UI');
      context = await freshContext();
      require(!context.hasRuntimeSender && !context.hasInitOutcome, 'Failed initialization left a ready Runtime');
      require((await invoke('meeting_list')).length === 0, 'Init failure fixture has recordings');
      record('native-init-failure-visible', { error: error.slice(0, 1500), context });
      record('native-no-recording', { count: 0, listenerReadyObserved: context.lifecycle.listenerReady });
      record('native-init-failure-graceful-request', { productionCommand: 'desktop_review_complete', noSignalUsedForSuccess: true });
    } else {
      const initialReady = await wait(async () => {
        const value = await freshContext();
        if (!value.hasRuntimeSender || !value.hasInitOutcome || !value.lifecycle.listenerReady) return null;
        const auth = await invoke('agent_auth_status');
        return auth.ready && auth.runtimeReady && auth.synchronized ? value : null;
      }, 'actual ready Runtime');
      let sessionId = scope.expectedSessionId;
      let planUpdate;
      let exitFailure;
      await listen('agent://update', ({ payload }) => {
        if (payload?.sessionId === sessionId && payload.sessionUpdate === 'current_mode_update'
          && (payload.currentModeId ?? payload.current_mode_id) === 'plan') {
          planUpdate = { sessionId: payload.sessionId, sessionUpdate: payload.sessionUpdate, currentModeId: 'plan' };
        }
      });
      await listen('desktop://exit-failed', ({ payload }) => { exitFailure = payload; });
      if (scope.phase === 'flush-failure') {
        require((await invoke('agent_list_all_sessions', { includeArchived: true })).length === 0, 'New exit profile has old sessions');
        sessionId = await invoke('agent_new_session', { cwd: scope.cwd, permissionMode: 'ask' });
        require(typeof sessionId === 'string' && /^[a-zA-Z0-9-]{1,128}$/.test(sessionId), 'Invalid real session id');
        const title = `退出保存恢复 ${scope.nonce.slice(0, 8)}`;
        await invoke('agent_rename_session', { sessionId, title, cwd: scope.cwd });
        await write('exit-session-ready.json', { nonce: scope.nonce, sessionId, cwd: scope.cwd, title });
        const fault = await wait(() => control('fault-applied'), 'owned plan write fault applied');
        require(fault.sessionId === sessionId, 'Parent fault applied to a different session');
        await invoke('set_plan_mode', { sessionId, enabled: true });
        const update = await wait(() => planUpdate, 'real dirty CurrentModeUpdate');
        record('native-dirty-plan-update', update);
        await write('exit-dirty-ready.json', { nonce: scope.nonce, sessionId, update });
        // This incomplete report must not count as acceptance. Native failure,
        // retained handle, physical repair and the final graceful exit follow.
        await invoke('desktop_review_complete', { report: { ...report, sessionId, passed: false, attempt: 'faulted-exit' } });
        // The Runtime can retry a durable write for up to 60 seconds before
        // the host reports the failed exit.
        const failed = await wait(() => exitFailure, 'real failed graceful exit', 75000);
        await wait(() => document.body.textContent.includes('未退出，应用已保留'), 'actual exit-failed UI');
        // App immediately retries init after native cleanup failed. During
        // that bounded retry the retained handle is owned by the init future,
        // rather than state.handle. Observe it requeued after failure; never
        // interpret that temporary ownership transfer as data loss.
        context = await wait(async () => {
          const value = await freshContext();
          return value.hasRuntimeHandle && !value.lifecycle.exitReady && !value.lifecycle.exitInProgress ? value : null;
        }, 'actual retained save handle after failed recovery attempt', 75000);
        require(/保存|Permission|permission|denied|权限|持久化/.test(`${failed.message} ${context.shutdownFailure || ''}`), 'Failure did not describe the actual save error');
        record('native-save-failure-retained', { failure: failed, context, failureVisible: true });
        await write('exit-failed-ready.json', { nonce: scope.nonce, sessionId, failure: failed, context });
        const repaired = await wait(() => control('permissions-restored'), 'parent permissions restored');
        require(repaired.sessionId === sessionId, 'Repair has a different session');
        await wait(async () => {
          const value = await freshContext();
          const retry = [...document.querySelectorAll('button')].find((element) => element.textContent?.trim() === '重试初始化');
          if (retry && !retry.disabled) retry.click();
          if (!value.hasRuntimeSender || !value.hasInitOutcome || value.initGeneration <= initialReady.initGeneration) return null;
          const auth = await invoke('agent_auth_status');
          return auth.ready && auth.runtimeReady && auth.synchronized ? value : null;
        }, 'actual Runtime recovery after repair', 60000);
        context = await freshContext();
        record('native-runtime-recovered', { context, initialInitGeneration: initialReady.initGeneration, sameSessionId: sessionId, afterPhysicalRepair: true });
      } else {
        const sessions = await invoke('agent_list_all_sessions', { includeArchived: true });
        const session = sessions.find((item) => item.sessionId === sessionId);
        require(session && session.cwd === scope.cwd, 'Restart did not retain the exact session/workspace');
        const row = await wait(() => document.querySelector(`[data-session-id="${CSS.escape(sessionId)}"] .sidebar__conv-select`), 'actual restored sidebar row');
        row.click();
        await wait(() => planUpdate, 'actual loaded plan mode update');
        const selected = await wait(() => document.querySelector(`[data-session-id="${CSS.escape(sessionId)}"] .sidebar__conv-select[aria-current="page"]`), 'actual selected restored session');
        record('native-restart-plan-restored', { sessionId, cwd: session.cwd, title: session.title, update: planUpdate, selectedInUi: Boolean(selected), noModelTurnSent: true });
      }
      report.sessionId = sessionId;
      record('native-final-graceful-request', { productionCommand: 'desktop_review_complete', noSignalUsedForSuccess: true });
    }
    report.passed = true;
  } catch (error) {
    report.errors.push(String(error?.message || error).slice(0, 2000));
    report.passed = false;
  } finally {
    for (const stop of stops) { try { await stop(); } catch { /* Process exit owns the remaining listener. */ } }
    report.finishedAt = new Date().toISOString();
    if (invoke && scopeApproved) await invoke('desktop_review_complete', { report });
  }
  return report;
};
