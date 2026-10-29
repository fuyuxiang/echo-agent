// Fixed native Browser Use journey. Real model, IPC, browser and approvals;
// synthetic loopback page only. Never permits shell, arbitrary URLs or files.
window.__echoNativeBrowserReview = async ({ home, nonce } = {}) => {
  if (window.__echoNativeBrowserReviewStarted) return;
  window.__echoNativeBrowserReviewStarted = true;
  const report = { version: 1, kind: 'native-browser', passed: false, nativeVerified: false,
    checks: [], errors: [], permissions: [], approvals: [], expected: null, fakeIPC: false };
  const completions = [], updates = [], stops = [], deadline = Date.now() + 900000;
  let invoke, scope, sessionId, admitted = false, desired = 'none';
  const assert = (condition, message) => { if (!condition) throw new Error(message); };
  const delay = ms => new Promise(resolve => setTimeout(resolve, ms));
  const visible = element => !!element && !element.closest('[hidden]') && element.getClientRects().length > 0;
  const button = (root, text) => [...(root?.querySelectorAll('button') ?? [])]
    .find(element => visible(element) && !element.disabled && element.textContent.trim() === text);
  const bounded = async (operation, label, ms = 15000) => {
    const remaining = Math.min(ms, deadline - Date.now()); assert(remaining > 0, 'Browser journey exceeded its bound');
    report.activeStep = label; let timer;
    try { return await Promise.race([operation(), new Promise((_, reject) => {
      timer = setTimeout(() => reject(new Error(`${label} timed out`)), remaining);
    })]); } finally { clearTimeout(timer); }
  };
  const call = (command, args = {}, ms) => bounded(() => invoke(command, args), command, ms);
  const wait = async (operation, label, ms = 30000) => {
    const end = Math.min(deadline, Date.now() + ms);
    while (Date.now() < end) { const value = await bounded(operation, label, Math.min(15000, end - Date.now()));
      if (value) return value; await delay(150); }
    throw new Error(`Actual browser condition timed out: ${label}`);
  };
  const stage = async (id, operation) => {
    try { const evidence = await operation(); report.checks.push({ id, result: 'passed', evidence }); return evidence; }
    catch (error) { const message = String(error?.message || error).slice(0, 2000);
      report.errors.push(`${id}: ${message}`); report.checks.push({ id, result: 'failed', evidence: { error: message } }); throw error; }
  };
  const set = (element, value) => {
    const proto = element.tagName === 'TEXTAREA' ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
    Object.getOwnPropertyDescriptor(proto, 'value').set.call(element, value);
    element.dispatchEvent(new Event('input', { bubbles: true }));
  };
  const catalog = () => call('agent_list_all_sessions', { includeArchived: true });
  const runtimeReady = async () => {
    let stableSince = null;
    return wait(async () => {
      const auth = await call('agent_auth_status');
      if (!auth.ready || !auth.runtimeReady || !auth.synchronized || !auth.runtimeModels.includes(scope.modelId)) {
        stableSince = null; return null;
      }
      stableSince ??= Date.now();
      return Date.now() - stableSince >= 1000 ? auth : null;
    }, 'actual stable Runtime readiness after replay', 90000);
  };
  const own = async () => {
    assert(/^[A-Za-z0-9-]{1,128}$/.test(sessionId || ''), 'No owned browser session');
    const entry = (await catalog()).find(item => item.sessionId === sessionId);
    assert(entry?.cwd === scope.cwd, 'Browser session escaped its own workspace');
    assert([scope.modelId, scope.modelId.split('/').at(-1)].includes(entry.currentModelId),
      'Browser session did not bind to the explicitly selected actual model or its persisted built-in alias');
    return entry;
  };
  const reply = () => {
    const jump = document.querySelector('button[aria-label="回到最新消息并恢复自动跟随"]');
    if (visible(jump) && !jump.disabled) jump.click();
    const lastUser = [...document.querySelectorAll('.msg--user')].filter(visible).at(-1);
    return [...document.querySelectorAll('.msg--assistant .msg__body > .markdown-body[data-md-theme="loose"]')]
      .filter(element => visible(element) && (!lastUser
        || lastUser.compareDocumentPosition(element) & Node.DOCUMENT_POSITION_FOLLOWING))
      .map(element => element.innerText).join('\n');
  };
  const send = async text => {
    const area = await wait(() => { const el = document.querySelector('.echo-composer textarea'); return visible(el) && !el.disabled ? el : null; }, 'real Composer');
    set(area, text); await wait(() => area.value === text, 'actual draft');
    (await wait(() => { const el = document.querySelector('.echo-composer button[aria-label="发送"]'); return visible(el) && !el.disabled ? el : null; }, 'actual send')).click();
  };
  const handled = new Set(), handledApprovals = new Set();
  const allowedOperation = request => {
    if (desired === 'restored-no-tools') return null;
    if (request.sessionId !== sessionId) return null;
    let raw = request.rawInput;
    if (typeof raw === 'string') { try { raw = JSON.parse(raw); } catch { return null; } }
    if (!raw || typeof raw.tool_name !== 'string' || !raw.tool_input || Array.isArray(raw.tool_input)) return null;
    if (!raw.tool_name.startsWith('echoagent-automation__')) return null;
    const name = raw.tool_name.slice('echoagent-automation__'.length), args = raw.tool_input;
    if (['browser_start', 'browser_snapshot', 'browser_screenshot', 'browser_downloads', 'automation_status'].includes(name)
      && Object.keys(args).length === 0) return { name, args };
    if (name === 'browser_navigate' && Object.keys(args).join(',') === 'url' && args.url === scope.url) return { name, args };
    // An empty navigate request cannot perform any navigation: the production
    // endpoint rejects its required url. Let that real validation error reach
    // the model so it can recover; an explicit foreign URL remains denied.
    if (name === 'browser_navigate' && typeof args === 'object' && Object.keys(args).length === 0)
      return { name, args, expectedInputValidationFailure: true };
    if (name === 'browser_wait' && Object.keys(args).every(key => key === 'milliseconds')
      && (args.milliseconds == null || Number.isInteger(args.milliseconds) && args.milliseconds >= 50 && args.milliseconds <= 3000)) return { name, args };
    if (name === 'browser_type' && desired === 'submit-download' && args.text === `BROWSER-INPUT-${nonce}`
      && typeof args.elementRef === 'string' && /^[A-Za-z0-9_-]{1,128}$/.test(args.elementRef)
      && Object.keys(args).every(key => ['text', 'elementRef', 'replace'].includes(key)) && args.replace !== false) return { name, args };
    if (name === 'browser_click' && ['submit-download', 'reject-delete'].includes(desired)
      && typeof args.elementRef === 'string' && /^[A-Za-z0-9_-]{1,128}$/.test(args.elementRef)
      && Object.keys(args).every(key => ['elementRef', 'button', 'clickCount'].includes(key))
      && (args.button == null || args.button === 'left') && (args.clickCount == null || args.clickCount === 1)) return { name, args };
    return null;
  };
  const service = async () => {
    const pending = await call('agent_list_pending_interactions', { sessionId: null });
    assert(!pending.questions?.length && !pending.folderTrustRequests?.length && !pending.planApprovals?.length,
      'Unexpected question, trust or plan cannot be approved by this journey');
    for (const request of pending.permissions ?? []) {
      if (handled.has(request.requestId)) continue;
      const operation = allowedOperation(request);
      if (!operation) { await call('agent_resolve_permission', { requestId: request.requestId, optionId: null, cancelled: true });
        let raw = request.rawInput;
        if (typeof raw === 'string') { try { raw = JSON.parse(raw); } catch { raw = null; } }
        report.permissions.push({ requestId: request.requestId, denied: true, diagnostic: {
          variant: raw?.variant, toolName: raw?.tool_name, inputType: typeof raw?.tool_input,
          argumentKeys: raw?.tool_input && typeof raw.tool_input === 'object' ? Object.keys(raw.tool_input) : [],
          sameSession: request.sessionId === sessionId, exactOwnedUrl: raw?.tool_input?.url === scope.url,
        } }); throw new Error('Unexpected tool, URL or input safely denied'); }
      const card = document.getElementById(`permission-${request.requestId}`), allow = button(card, '允许一次');
      if (!visible(card) || !allow) continue;
      allow.click(); handled.add(request.requestId); report.permissions.push({ requestId: request.requestId, operation, via: 'production UI' });
    }
    const approvals = await call('automation_pending_approvals', { sessionId });
    for (const approval of approvals) {
      if (handledApprovals.has(approval.requestId)) continue;
      assert(approval.sessionId === sessionId, 'Approval belongs to another session');
      const details = approval.details, target = details?.target?.name;
      let accept = false;
      if (desired === 'private-network' && approval.tool === 'automation_set_private_network' && details?.allowed === true) accept = true;
      else {
        assert(details?.url === scope.url, 'Approval outside exact synthetic page');
        if (desired === 'submit-download' && approval.tool === 'browser_type') {
          assert(target === '本次验收文本' && details.characters === `BROWSER-INPUT-${nonce}`.length
            && details.passwordField === false, 'Unexpected input field or text size'); accept = true;
        } else if (desired === 'submit-download' && approval.tool === 'browser_click') {
          assert(['发送本次验收文本', '下载中文验收文件'].includes(target), 'Unexpected click target'); accept = true;
        } else assert(desired === 'reject-delete' && approval.tool === 'browser_click' && target === '删除本次测试记录', 'Unexpected action denied');
      }
      const card = document.querySelector('.perm-inline[data-approval-source="automation"]'), action = button(card, accept ? '允许一次' : '拒绝');
      if (!visible(card) || !action) continue;
      assert(card.textContent.includes(approval.title), 'Rendered safety card does not match native request');
      action.click(); handledApprovals.add(approval.requestId);
      report.approvals.push({ requestId: approval.requestId, action: approval.tool, approved: accept, details, via: 'production UI' });
    }
  };
  const submitPrompt = () => `本机隔离网页任务：每一步只通过use_tool分发echoagent-automation服务器的browser_*工具。严格参数示例：browser_start为{"tool_name":"echoagent-automation__browser_start","tool_input":{}}；browser_navigate为{"tool_name":"echoagent-automation__browser_navigate","tool_input":{"url":${JSON.stringify(scope.url)}}}。browser_snapshot及browser_downloads的tool_input严格为{}，不添加sessionId、url或其他参数。用snapshot里的elementRef（禁止坐标）向“本次验收文本”输入完整字符串 ${JSON.stringify(`BROWSER-INPUT-${nonce}`)}，必须包括BROWSER-INPUT-前缀，不能仅输入nonce。browser_type的tool_input只含elementRef和text；点击“发送本次验收文本”，browser_click的tool_input只含elementRef。核对网页成功指纹后点击“下载中文验收文件”，用browser_downloads确认下载完成。绝不能用终端或猜测browser_launch；禁止其他服务器/工具/网址/文件、删除、上传、新tab；最后只回复完整字符串 ${JSON.stringify(`BROWSER-SUBMITTED-${nonce}`)}，必须包含BROWSER-SUBMITTED-前缀，不能仅回复nonce。`;
  const turn = async (text, marker) => {
    const before = text === null ? 0 : completions.length; if (text !== null) await send(text);
    let completedAt = null;
    await wait(async () => { await service(); const event = completions.slice(before).find(item => item.sessionId === sessionId);
      if (!event) return null; assert(event.stopReason === 'end_turn', `Model ended ${event.stopReason}`);
      if ((await own()).status !== 'completed') return null;
      if (!reply().includes(marker)) {
        // The native completion may reach this listener just before React
        // commits the final assistant text. Keep the exact result requirement.
        completedAt ??= Date.now();
        if (Date.now() - completedAt < 1500) return null;
        report.turnFailures = [...(report.turnFailures || []), { sessionId, expectedMarker: marker,
          completion: event, actualReply: reply().slice(0, 2500), toolUpdates: updates.slice(-20),
          renderedChat: [...document.querySelectorAll('.msg--assistant')].slice(-3).map(element => ({
            visible: visible(element), text: element.innerText.slice(-2500),
            answerParts: [...element.querySelectorAll('.msg__body > .markdown-body')].map(part => ({
              visible: visible(part), text: part.innerText.slice(-2500), theme: part.dataset.mdTheme,
            })),
          })), chatPresent: !!document.querySelector('.chatview__scroll'),
          userMessages: [...document.querySelectorAll('.msg--user')].slice(-2).map(element => element.innerText.slice(0, 600)),
        }];
        throw new Error('Actual model completed without the required browser result fingerprint; preserved its real reply and tool events');
      }
      return event;
    // The first journey includes schema discovery, 7+ real MCP operations
    // and two independent approval layers. The preserved XC run completed
    // typing but reached the old 180s budget before its submit click.
    }, 'real model turn + visible fingerprint', text === null ? 600000 : 180000);
    return { sessionId, marker, reply: reply().slice(0, 2500), completion: completions.slice(before).at(-1) };
  };
  const listen = async (event, handler) => {
    const callback = window.__TAURI_INTERNALS__.transformCallback(handler);
    const id = await call('plugin:event|listen', { event, target: { kind: 'Any' }, handler: callback });
    stops.push(async () => { window.__TAURI_EVENT_PLUGIN_INTERNALS__?.unregisterListener(event, id); await call('plugin:event|unlisten', { event, eventId: id }); });
  };
  try {
    assert(typeof home === 'string' && /^\/private\/(?:tmp|var\/folders\/[A-Za-z0-9_-]+\/[A-Za-z0-9_-]+\/T)\/echo-native-review-browser-[A-Za-z0-9_-]+$/.test(home)
      && /^[a-f0-9]{32}$/.test(nonce || '') && typeof window.__TAURI_INTERNALS__?.invoke === 'function', 'Invalid private browser launch');
    invoke = (command, args) => window.__TAURI_INTERNALS__.invoke(command, args);
    scope = await call('desktop_review_browser_manifest'); const context = await call('desktop_review_context');
    assert(context.browserRequested === true && context.evaluations === 1 && context.home === home
      && scope.home === home && scope.nonce === nonce && scope.cwd === `${home}/workspaces/default`
      && ['echoagent-ojlab/Echo Reed', 'echoagent-ojlab/Echo Grove', 'echoagent-review/claude-opus-5-5'].includes(scope.modelId), 'Native browser scope mismatch');
    Object.assign(report, { phase: scope.phase, home, nonce, nativeContext: context }); admitted = true;
    report.expected = scope.phase === 'restart' ? scope.expected : { schema: 1, home, nonce, cwd: scope.cwd, sessionId: null, url: scope.url, modelId: scope.modelId };
    await stage('browser-isolation', async () => {
      const stat = await call('path_stat', { path: scope.cwd, cwd: scope.cwd });
      assert(stat.exists && stat.kind === 'directory' && stat.absolute === scope.cwd, 'Workspace not canonical');
      const capability = await call('automation_capabilities'); assert(capability.browser.available === true, 'No actual controlled browser');
      return { capability, cwd: scope.cwd, pid: context.pid };
    });
    await listen('agent://complete', ({ payload }) => completions.push(payload));
    await listen('agent://update', ({ payload }) => { if (['tool_call', 'tool_call_update'].includes(payload?.sessionUpdate ?? payload?.type) && updates.length < 120) updates.push(payload); });
    await wait(async () => { const auth = await call('agent_auth_status');
      return auth.ready && auth.runtimeReady && auth.synchronized && auth.runtimeModels.includes(scope.modelId); }, 'actual model readiness', 90000);
    if (scope.phase === 'restart') {
      sessionId = report.expected.sessionId; await own();
      await stage('browser-restart-no-profile', async () => {
        const state = await call('automation_status', { sessionId });
        assert(!state.browserRunning && !state.browserHasData && !state.allowPrivateNetwork, 'Restart revived cleared browser data or network consent');
        (await wait(() => document.querySelector(`[data-session-id="${CSS.escape(sessionId)}"] .sidebar__conv-select`), 'owned session sidebar')).click();
        await wait(() => [...document.querySelectorAll('.msg--assistant')].some(el => visible(el) && el.textContent.includes(`BROWSER-SUBMITTED-${nonce}`)), 'real history fingerprint');
        return { state, visibleSavedHistory: true };
      });
      await stage('browser-restored-model-reapply', async () => {
        await runtimeReady();
        const selector = await wait(() => {
          const el = document.querySelector('.echo-composer .model-selector__trigger');
          return visible(el) && !el.disabled ? el : null;
        }, 'restored model selector');
        selector.click();
        const option = await wait(() => [...document.querySelectorAll('.model-selector__item')]
          .find(el => visible(el) && el.dataset.modelId === scope.modelId),
        'exact restored model option');
        assert(option.getAttribute('aria-selected') === 'true', 'Restored model picker lost its actual model');
        // Exercise the real model-change callback for the same selected model.
        option.click();
        await delay(200);
        await wait(() => !selector.disabled && !document.querySelector('.model-selector__menu'), 'same-model reapply finished');
        assert(![...document.querySelectorAll('[role="alert"], .toast')].some(el => visible(el)
          && /MODEL_SWITCH_INCOMPATIBLE_AGENT|requires agent|模型切换失败|该会话无法切换/.test(el.textContent)), 'Same model rejected after Browser restore');
        // A successful UI projection alone is insufficient. Also require the
        // exact production command to acknowledge this unchanged model.
        await runtimeReady();
        await call('agent_set_model', { sessionId, modelId: scope.modelId }, 30000);
        desired = 'restored-no-tools';
        const result = await turn(`只回复完整字符串 ${JSON.stringify(`BROWSER-RESTORED-${nonce}`)}，不调用任何工具，不打开浏览器，不访问网络或文件。`, `BROWSER-RESTORED-${nonce}`);
        assert(report.permissions.length === 0 && report.approvals.length === 0, 'Readonly restored turn attempted a tool or approval');
        const state = await call('automation_status', { sessionId });
        assert(state.paused && !state.browserRunning && !state.browserHasData && !state.allowPrivateNetwork, 'Readonly restored turn revived browser permissions or data');
        return { ...result, modelId: scope.modelId, selectedBeforeReapply: true, state };
      });
    } else {
      assert((await catalog()).length === 0, 'Refuse existing session data');
      const selector = await wait(() => { const el = document.querySelector('.echo-composer .model-selector__trigger');
        return visible(el) && !el.disabled ? el : null; }, 'real Composer model selector'); selector.click();
      (await wait(() => [...document.querySelectorAll('.model-selector__item')].find(el => visible(el)
        && el.dataset.modelId === scope.modelId), 'exact Qwen option')).click();
      await stage('browser-mode-ui', async () => {
        (await wait(() => document.querySelector('.echo-composer button[aria-label="添加"]'), 'add menu')).click();
        (await wait(() => [...document.querySelectorAll('[role="menuitemradio"]')].find(el => visible(el)
          && el.querySelector('.iam-item__label')?.textContent === '操作网页' && el.getAttribute('aria-disabled') !== 'true'), 'actual browser mode')).click();
        desired = 'submit-download'; await send(submitPrompt());
        const created = await wait(async () => (await catalog()).find(item => item.cwd === scope.cwd && !item.hidden), 'real first Browser task session');
        sessionId = created.sessionId; report.expected.sessionId = sessionId;
        const state = await wait(async () => { const s = await call('automation_status', { sessionId }); return s.mode === 'browser_use' ? s : null; }, 'real Browser Use mode');
        assert(!state.allowPrivateNetwork, 'Private network must start blocked');
        const server = await wait(async () => {
          const entries = await call('mcp_list', { sessionId, refresh: true }, 60000);
          const entry = entries.find(item => item.name === 'echoagent-automation');
          if (entry?.status === 'unavailable' || entry?.setupRequired || entry?.authRequired) {
            throw new Error(`Actual automation MCP unavailable: ${entry.disabledReason || entry.status}`);
          }
          return entry?.enabled && entry.live && entry.status === 'ready'
            && ['browser_start', 'browser_navigate', 'browser_snapshot', 'browser_type', 'browser_click', 'browser_downloads']
              .every(name => entry.tools.some(tool => tool.enabled && (tool.name === name || tool.name === `echoagent-automation__${name}`))) ? entry : null;
        }, 'actual session automation MCP + browser tool catalog', 90000);
        return { ...state, mcp: { name: server.name, status: server.status, live: server.live, tools: server.tools.map(tool => tool.name) } };
      });
      await stage('browser-network-consent-ui', async () => {
        desired = 'private-network'; (await wait(() => document.querySelector('.automation-control__more summary'), 'network options')).click();
        const checkbox = await wait(() => document.querySelector('.automation-control__private input'), 'actual private network checkbox');
        assert(!checkbox.checked, 'Consent starts unchecked'); checkbox.click();
        await wait(async () => { await service(); return (await call('automation_status', { sessionId })).allowPrivateNetwork; }, 'real safety approval then network enable');
        assert(report.approvals.some(item => item.action === 'automation_set_private_network' && item.approved), 'No real network confirmation'); return { confirmedViaUi: true };
      });
      await stage('browser-submit-download-model', async () => {
        desired = 'submit-download';
        const result = await turn(null, `BROWSER-SUBMITTED-${nonce}`);
        assert(report.approvals.filter(item => item.approved && item.action === 'browser_type').length === 1
          && report.approvals.filter(item => item.approved && item.action === 'browser_click').length === 2, 'Exactly the input/submit/download approvals required');
        await call('write_text_file', { path: 'browser-download-ready.json', content: JSON.stringify({ nonce, sessionId }), workspaceRoot: scope.cwd });
        const proof = await wait(async () => { try { const data = JSON.parse(await call('read_text_file', { path: `${scope.cwd}/browser-download-proof.json`, cwd: scope.cwd, maxBytes: 8192 }));
          assert(data.nonce === nonce && data.sessionId === sessionId && data.passed, 'Physical download rejected'); return data;
        } catch (error) { if (/Physical download rejected/.test(String(error))) throw error; return null; } }, 'independent parent physical download proof');
        report.expected.download = proof; return { ...result, proof, toolUpdates: updates.slice(-20).map(value => JSON.stringify(value).slice(0, 2500)) };
      });
      await stage('browser-reject-delete-ui', async () => {
        desired = 'reject-delete'; const marker = `BROWSER-REJECTED-${nonce}`;
        const result = await turn(`仍只通过use_tool调度echoagent-automation__browser_snapshot/browser_click，不调用其他工具、不导航。用elementRef尝试点击“删除本次测试记录”一次，等待用户安全确认。用户拒绝后不得重试，也不要执行其他操作；仅回复“${marker}”。`, marker);
        assert(report.approvals.filter(item => !item.approved && item.action === 'browser_click').length === 1, 'No actual rejected delete card'); return result;
      });
      await stage('browser-takeover-resume-ui', async () => {
        desired = 'none'; (await wait(() => button(document.querySelector('.automation-control'), '接管'), 'real takeover')).click();
        const paused = await wait(async () => { const s = await call('automation_status', { sessionId }); return s.paused ? s : null; }, 'native paused boundary');
        assert((await call('automation_pending_approvals', { sessionId })).length === 0, 'Takeover left live approvals');
        (await wait(() => button(document.querySelector('.automation-control'), '继续'), 'real resume')).click();
        await wait(async () => !(await call('automation_status', { sessionId })).paused, 'native resumed'); return { paused, resumedViaUi: true };
      });
      await stage('browser-clear-data-ui', async () => {
        const details = document.querySelector('.automation-control__more'); if (!details.open) details.querySelector('summary').click();
        (await wait(() => button(document.querySelector('.automation-control'), '清除本任务浏览器数据'), 'real clear browser')).click();
        const dialog = await wait(() => [...document.querySelectorAll('[role="alertdialog"]')].find(el => visible(el) && el.textContent.includes('清除该任务的浏览器数据')), 'real clear confirmation');
        button(dialog, '取消')?.click();
        assert((await call('automation_status', { sessionId })).browserHasData, 'Cancel removed browser data');
        (await wait(() => button(document.querySelector('.automation-control'), '清除本任务浏览器数据'), 'clear retry')).click();
        (await wait(() => button([...document.querySelectorAll('[role="alertdialog"]')].find(el => visible(el) && el.textContent.includes('清除该任务的浏览器数据')), '清除数据'), 'explicit clear data')).click();
        const state = await wait(async () => { const s = await call('automation_status', { sessionId }); return s.paused && !s.browserRunning && !s.browserHasData ? s : null; }, 'actual profile removal + paused');
        return { state, cancelPreservedProfile: true, clearConfirmedViaUi: true };
      });
    }
  } catch (error) { report.errors.push(String(error?.message || error).slice(0, 2000)); }
  finally {
    for (const stop of stops) { try { await stop(); } catch (error) { report.errors.push(`cleanup: ${String(error).slice(0, 200)}`); } }
    const required = scope?.phase === 'restart' ? ['browser-isolation', 'browser-restart-no-profile', 'browser-restored-model-reapply']
      : ['browser-isolation', 'browser-mode-ui', 'browser-network-consent-ui', 'browser-submit-download-model', 'browser-reject-delete-ui', 'browser-takeover-resume-ui', 'browser-clear-data-ui'];
    report.passed = admitted && report.errors.length === 0 && report.checks.length === required.length
      && required.every(id => report.checks.filter(item => item.id === id && item.result === 'passed').length === 1);
    report.finishedAt = new Date().toISOString();
    if (admitted) await bounded(() => invoke('desktop_review_complete', { report }), 'real report and graceful exit', 20000);
  }
  return report;
};
