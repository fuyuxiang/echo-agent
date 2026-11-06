// Fixed native notification/meeting recovery acceptance. Production DOM and IPC
// remain intact; the only HTTP peer is the guarded, owned loopback receiver.
window.__echoNativeLocalReview = async ({ invoke, context, scope }) => {
  const phase = context?.phase, restart = phase === 'restart';
  const required = restart
    ? ['native-local-isolation', 'native-local-channel-restored', 'native-local-meeting-restored',
      'native-local-channel-remove', 'native-local-return-shell']
    : ['native-local-isolation', 'native-local-notification-settings', 'native-local-channel-create',
      'native-local-webhook-success', 'native-local-channel-disabled-gate', 'native-local-webhook-failure-record',
      'native-local-webhook-retry', 'native-local-channel-edit-preserves-endpoint', 'native-local-meeting-missing-file',
      'native-local-meeting-fixture-recovery', 'native-local-return-shell'];
  const report = { version: 1, kind: 'local', phase, nativeContext: context,
    environment: 'actual native WebView + production IPC + owned standard HTTP receiver',
    passed: false, checks: [], errors: [], expected: null, modelRequested: false,
    osNotificationVerified: false, playbackSuccessVerified: false, recordingOrAsrVerified: false,
    meetingInput: 'explicit isolated manifest and synthetic silent WAV fixture' };
  const deadline = Date.now() + 180000;
  const assert = (condition, message) => { if (!condition) throw new Error(message); };
  const visible = (element) => Boolean(element?.isConnected && element.getClientRects().length);
  const delay = (ms) => new Promise((accept) => setTimeout(accept, ms));
  const record = (id, evidence) => report.checks.push({ id, result: 'passed', evidence });
  const waitFor = async (check, label, timeout = 20000) => {
    const end = Math.min(deadline, Date.now() + timeout);
    while (Date.now() < end) {
      let timer;
      try {
        const result = await Promise.race([Promise.resolve().then(check), new Promise((_accept, reject) => {
          timer = setTimeout(() => reject(new Error(`${label} timed out`)), end - Date.now());
        })]);
        if (result) return result;
      } finally { clearTimeout(timer); }
      await delay(Math.min(100, Math.max(0, end - Date.now())));
    }
    throw new Error(`${label} timed out`);
  };
  const allowed = new Set(['notify_channels_list', 'notification_list', 'notify_channel_test', 'meeting_list', 'meeting_get']);
  const call = async (command, args = {}) => {
    assert(allowed.has(command), 'Unexpected native command');
    const remaining = Math.min(25000, deadline - Date.now());
    assert(remaining > 0, 'Native local review exceeded its 180s deadline');
    let timer;
    try { return await Promise.race([invoke(command, args), new Promise((_accept, reject) => {
      timer = setTimeout(() => reject(new Error(`${command} timed out`)), remaining);
    })]); } finally { clearTimeout(timer); }
  };
  const service = async (suffix, method = 'GET', body) => {
    assert(['/audit', '/state', '/repair'].includes(suffix), 'Unexpected owned receiver path');
    const remaining = Math.min(10000, deadline - Date.now());
    assert(remaining > 0, 'Owned receiver deadline expired');
    const controller = new AbortController(), timer = setTimeout(() => controller.abort(), remaining);
    try {
      const response = await fetch(scope.receiverUrl + suffix, { method, signal: controller.signal,
        headers: body === undefined ? {} : { 'Content-Type': 'application/json' },
        body: body === undefined ? undefined : JSON.stringify(body) });
      assert(response.ok, 'Owned receiver observation or fixture handoff failed');
      return await response.json();
    } finally { clearTimeout(timer); }
  };
  const button = (parent, label) => [...(parent?.querySelectorAll('button') || [])]
    .find((element) => visible(element) && !element.disabled && element.textContent?.trim() === label);
  const click = async (parent, label) => {
    const element = await waitFor(() => button(typeof parent === 'function' ? parent() : parent, label), `Actual ${label} button`);
    assert(visible(element) && !element.disabled, 'Unavailable native DOM control');
    element.click();
  };
  const input = (element, value) => {
    assert(visible(element) && !element.disabled, 'Unavailable native form control');
    const prototype = element instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype
      : element instanceof HTMLSelectElement ? HTMLSelectElement.prototype : HTMLInputElement.prototype;
    Object.getOwnPropertyDescriptor(prototype, 'value').set.call(element, value);
    element.dispatchEvent(new Event(element instanceof HTMLSelectElement ? 'change' : 'input', { bubbles: true }));
  };
  const snapshots = () => new Set(document.querySelectorAll('.toast__message'));
  const toast = (before, text) => waitFor(() => [...document.querySelectorAll('.toast__message')]
    .find((element) => visible(element) && !before.has(element) && element.textContent?.includes(text)), `Actual ${text} feedback`, 25000);
  const panel = () => document.querySelector('.settings-modal-overlay .notify-panel[aria-label="通知设置"]');
  const row = (label) => [...(panel()?.querySelectorAll('.notify-panel__row') || [])]
    .find((element) => element.querySelector('.notify-panel__row-label')?.textContent === label);
  const notificationPanelReady = () => {
    const current = panel();
    if (!visible(current) || current.querySelector('.panel-inline-error')) return null;
    for (const selector of ['[aria-label="通知渠道类型"]', '[aria-label="通知渠道显示名"]', '[aria-label="Webhook URL"]']) {
      const control = current.querySelector(selector);
      if (!visible(control) || control.disabled) return null;
    }
    return current;
  };
  const configured = async (label) => {
    const channels = await call('notify_channels_list');
    assert(Array.isArray(channels) && channels.length === 1 && channels[0].label === label
      && channels[0].kind === 'generic-webhook', 'Unexpected channel outside the owned isolated review');
    assert(/^ch_\d{10,}$/.test(channels[0].id), 'Expected a real UI-created channel ID');
    assert(!String(channels[0].endpoint).includes(scope.nonce), 'Backend leaked the full webhook endpoint');
    return channels[0];
  };
  const openSettings = async () => {
    (await waitFor(() => {
      const element = document.querySelector('.sidebar__icon-btn[aria-label="设置"]');
      return visible(element) && !element.disabled ? element : null;
    }, 'Actual Settings entry')).click();
    await click(() => document.querySelector('.settings-modal-overlay .settings-navigation'), '通知');
    await click(() => document.querySelector('.settings-modal-overlay [aria-label="通知视图"]'), '通知设置');
    const advanced = await waitFor(() => {
      const current = panel(), toggle = current?.querySelector('.notify-advanced__toggle');
      return visible(toggle) && current.querySelector('.notify-advanced__count')?.textContent !== '正在读取…'
        && !current.querySelector('.panel-inline-error') ? toggle : null;
    }, 'Actual loaded external notification disclosure');
    if (advanced.getAttribute('aria-expanded') !== 'true') advanced.click();
    await waitFor(() => panel()?.querySelector('.notify-advanced__toggle')?.getAttribute('aria-expanded') === 'true', 'External notification disclosure expanded');
    await click(panel, '添加渠道');
    await waitFor(notificationPanelReady, 'Actual notification channel panel loaded');
  };
  const closeSettings = async () => {
    const element = document.querySelector('.settings-modal__close[aria-label="关闭设置"]');
    assert(visible(element) && !element.disabled, 'Actual Settings close unavailable');
    element.click();
    await waitFor(() => !visible(document.querySelector('.settings-modal-overlay')), 'Settings closed');
  };
  const save = async (label, endpoint, editing) => {
    if (editing) {
      await click(() => row(report.expected.label), '编辑');
      await waitFor(() => button(panel(), '保存修改'), 'Actual channel edit form');
      assert(panel().querySelector('[aria-label="Webhook URL"]').value === '', 'Editing leaked the hidden endpoint into the form');
    } else input(panel().querySelector('[aria-label="通知渠道类型"]'), 'generic-webhook');
    input(panel().querySelector('[aria-label="通知渠道显示名"]'), label);
    input(panel().querySelector('[aria-label="Webhook URL"]'), endpoint);
    await click(panel, editing ? '保存修改' : '+ 添加');
    // Toasts deliberately coalesce repeated messages. The persistent panel
    // feedback, closed editor, native readback and later real HTTP requests
    // establish this save without assuming every save creates a new toast node.
    await waitFor(() => visible(row(label)) && !panel()?.querySelector('.notify-panel__editor')
      && panel()?.querySelector('.settings-msg[role="status"]')?.textContent === `通知渠道“${label}”已保存，立即用于后续通知。`, 'Saved feedback, persisted channel row and closed editor');
    return configured(label);
  };
  const send = async (failure = false) => {
    await click(() => row(report.expected.label), '测试');
    const feedback = await waitFor(() => {
      const current = panel()?.querySelector(failure ? '.settings-msg[role="alert"]' : '.settings-msg[role="status"]');
      return visible(current) && current.textContent.includes(failure ? '发送失败：' : '测试通知已向渠道提交') ? current : null;
    }, 'Actual persistent channel test feedback');
    if (failure) assert(feedback.textContent.includes('403'), 'Failure did not expose the actual HTTP rejection');
  };
  const denial = async (id) => {
    let error;
    try { await call('notify_channel_test', { id }); } catch (caught) { error = String(caught); }
    assert(error?.includes('渠道不存在、未启用或被策略禁用'), 'Disabled or removed channel bypassed real backend enforcement');
    return error;
  };
  const auditCounts = async (successes, failures) => {
    const audit = await service('/audit');
    assert(Array.isArray(audit.notifications) && audit.notifications.length === successes + failures, 'Owned receiver saw missing or duplicate notification POSTs');
    assert(audit.notifications.filter((item) => item.path === new URL(scope.receiverUrl).pathname && item.status === 200).length === successes,
      'Unexpected number of real successful webhook requests');
    assert(audit.notifications.filter((item) => item.path === new URL(scope.receiverUrl).pathname + '/fail' && item.status === 403).length === failures,
      'HTTP 403 was retried or not delivered to the owned receiver');
    for (const item of audit.notifications) assert(item.payload?.title === '测试通知'
      && item.payload.body === '来自 EchoAgent 的测试消息' && item.payload.level === 'info'
      && item.payload.sessionId === null && Number.isFinite(Date.parse(item.payload.timestamp)), 'Actual webhook payload differs from the production test message');
    return audit;
  };
  const meetingPanel = () => document.querySelector('.meeting-workbench');
  const openMeeting = async () => {
    const homeButton = await waitFor(() => [...document.querySelectorAll('.sidebar__nav-item')]
      .find((element) => visible(element) && !element.disabled && element.textContent.trim() === '新建任务'), 'Actual Home entry');
    homeButton.click();
    const add = await waitFor(() => {
      const element = document.querySelector('.echo-composer button[aria-label="添加"]');
      return visible(element) && !element.disabled ? element : null;
    }, 'Actual Home composer add menu');
    add.click();
    const meetingEntry = await waitFor(() => [...document.querySelectorAll('[aria-label="添加内容"] .iam-item')]
      .find((element) => visible(element) && !element.disabled && element.getAttribute('aria-disabled') !== 'true'
        && element.querySelector('.iam-item__label')?.textContent === '会议录音与纪要'), 'Actual meeting-history menu entry without ASR');
    meetingEntry.click();
    await waitFor(() => visible(meetingPanel()) && meetingPanel().querySelector('h1')?.textContent === '录音转写', 'Actual meeting page without an ASR model');
    if (!meetingPanel().querySelector('.meeting-start')) await click(meetingPanel, '新建录音');
    assert(meetingPanel().textContent.includes('尚未配置转写连接，可以先录音，配置后再补转。'), 'Missing-ASR capability feedback was lost');
    const start = [...meetingPanel().querySelectorAll('button')].find((element) => element.textContent.trim() === '开始录音');
    assert(visible(start) && !start.disabled, 'Local recording unavailable without optional ASR connection');
    const library = meetingPanel().querySelector('button[aria-label="展开录音列表"]');
    if (library) library.click();
    const selected = await waitFor(() => [...meetingPanel().querySelectorAll('.meeting-library__item')]
      .find((element) => element.querySelector('strong')?.textContent === report.expected.meetingTitle), 'Actual isolated meeting list');
    selected.click();
    await waitFor(() => meetingPanel()?.querySelector('.meeting-workbench__record-header h2')?.textContent === report.expected.meetingTitle
      && visible(meetingPanel().querySelector('[aria-label="录音播放器"]')) && button(meetingPanel(), '编辑纪要'), 'Selected isolated meeting and saved controls without ASR');
  };
  let active = required[0];
  try {
    assert(invoke === window.__TAURI_INTERNALS__?.invoke, 'Actual native IPC required');
    assert(phase === 'first-launch' || restart, 'Explicit native phase required');
    assert(context.localRequested === true && context.evaluations === 1, 'Dedicated single-evaluation native local scope required');
    assert(scope?.schema === 1 && context.home === scope.home
      && /^\/private\/(?:tmp|var\/folders)\/(?:[^/]+\/)*echo-native-review-local-[^/]+$/.test(scope.home), 'Refusing non-private native review home');
    assert(/^[a-f0-9]{32}$/.test(scope.nonce || ''), 'Invalid owned receiver nonce');
    const url = new URL(scope.receiverUrl);
    assert(url.protocol === 'http:' && url.hostname === '127.0.0.1' && url.port && !url.username && !url.password
      && !url.search && !url.hash && url.pathname === `/echo-review/${scope.nonce}`, 'Refusing external receiver');
    const n = scope.nonce;
    const meetingId = `${n.slice(0, 8)}-${n.slice(8, 12)}-4${n.slice(13, 16)}-a${n.slice(17, 20)}-${n.slice(20, 32)}`;
    const initial = { version: 1, home: scope.home, nonce: n, label: `本地通知验收-${n}`,
      finalLabel: `本地通知恢复-${n}`, channelId: null, meetingId, meetingTitle: `隔离缺文件恢复-${n}`,
      audioPath: `${scope.home}/meetings/${meetingId}/原生缺文件恢复.wav`,
      originalMinutes: `# 显式隔离测试记录\n这是验收 fixture，未录音、未转写。${n}`,
      minutes: `# 本地恢复校对\n缺文件错误后可继续编辑和保存。${n}` };
    if (restart) {
      report.expected = (await service('/state')).expected;
      assert(report.expected?.version === 1 && report.expected.home === scope.home && report.expected.nonce === n
        && report.expected.meetingId === meetingId && report.expected.audioPath === initial.audioPath
        && report.expected.minutes === initial.minutes && /^ch_\d{10,}$/.test(report.expected.channelId), 'Restart lost exact first-launch expectations');
    } else report.expected = initial;
    record(active, { home: scope.home, productionInvoke: true, loopbackOnly: true, noModels: true, fixtureMeeting: true });
    active = required[1];
    await openSettings();
    if (restart) {
      active = required[1];
      const channel = await configured(report.expected.finalLabel);
      assert(channel.id === report.expected.channelId && channel.enabled, 'Restart lost the UI-saved enabled channel');
      await waitFor(() => visible(row(report.expected.finalLabel)), 'Restored native channel row');
      await auditCounts(3, 1);
      const failures = (await call('notification_list')).filter((entry) => entry.title === '通知投递失败' && entry.body?.includes('403'));
      assert(failures.length === 1, 'Restart lost or duplicated the real notification failure inbox');
      record(active, { channelId: channel.id, failureInboxCount: 1, noRestartSend: true });
      active = required[2];
      await closeSettings();
      await openMeeting();
      const meeting = await call('meeting_get', { meetingId });
      assert(meeting.minutes === report.expected.minutes && meeting.audioPath === report.expected.audioPath && meeting.status === 'completed', 'Meeting restart lost the UI-saved recovery');
      assert(meetingPanel().textContent.includes('缺文件错误后可继续编辑和保存。'), 'Saved recovery minutes did not render after restart');
      record(active, { meetingId, exactSavedMinutes: true, noAsrPageEntry: true, retryPlayerControlAvailable: true });
      active = required[3];
      await openSettings();
      const remove = row(report.expected.finalLabel)?.querySelector(`button[aria-label="移除 ${CSS.escape(report.expected.finalLabel)}"]`);
      assert(visible(remove) && !remove.disabled, 'Actual channel remove unavailable');
      remove.click();
      const dialog = await waitFor(() => [...document.querySelectorAll('.app-dialog')]
        .find((element) => visible(element) && element.querySelector('.app-dialog__title')?.textContent === '移除通知渠道？'), 'Actual remove confirmation');
      await click(dialog, '移除');
      await waitFor(() => !visible(dialog) && !row(report.expected.finalLabel), 'Removed channel row and closed confirmation');
      assert((await call('notify_channels_list')).length === 0, 'UI channel removal did not persist');
      await denial(report.expected.channelId);
      await auditCounts(3, 1);
      record(active, { removedThroughUi: true, backendRevocationDenied: true, noNewNotificationRequest: true });
    } else {
      active = required[1];
      assert((await call('notify_channels_list')).length === 0, 'Isolated notification profile is not empty');
      record(active, { actualSettingsAndChannelTabs: true, emptyProfile: true });
      active = required[2];
      const created = await save(report.expected.label, scope.receiverUrl, false);
      assert(created.enabled, 'New UI-created channel is not enabled');
      report.expected.channelId = created.id;
      record(active, { channelId: created.id, kind: created.kind, redactedEndpoint: created.endpoint });
      active = required[3];
      await send();
      await auditCounts(1, 0);
      record(active, { actualReceiverStatus: 200, requestCount: 1, actualPayloadChecked: true, osFallbackExercised: false });
      active = required[4];
      row(report.expected.label).querySelector(`button[aria-label="禁用 ${CSS.escape(report.expected.label)}"]`).click();
      await waitFor(async () => !(await configured(report.expected.label)).enabled && row(report.expected.label)?.querySelector('button[title="测试发送"]')?.disabled, 'Actual disabled state');
      await denial(created.id);
      await auditCounts(1, 0);
      row(report.expected.label).querySelector(`button[aria-label="启用 ${CSS.escape(report.expected.label)}"]`).click();
      await waitFor(async () => (await configured(report.expected.label)).enabled && button(row(report.expected.label), '测试'), 'UI channel enabled again');
      record(active, { disabledUiTestBlocked: true, nativeDisabledDenied: true, enabledThroughUi: true, noRequestWhileDisabled: true });
      active = required[5];
      await save(report.expected.label, scope.receiverUrl + '/fail', true);
      await send(true);
      await auditCounts(1, 1);
      const failures = (await call('notification_list')).filter((entry) => entry.title === '通知投递失败' && entry.body?.includes('403'));
      assert(failures.length === 1 && failures[0].body.includes(report.expected.label), 'Real 403 failure was not persisted to the inbox');
      record(active, { actualReceiverStatus: 403, uniqueFailureRequest: true, persistedInboxId: failures[0].id });
      active = required[6];
      await save(report.expected.label, scope.receiverUrl, true);
      await send();
      await auditCounts(2, 1);
      record(active, { correctedThroughUi: true, exactlyOneManualRetry: true, actualReceiverStatus: 200 });
      active = required[7];
      const revised = await save(report.expected.finalLabel, '', true);
      assert(revised.id === created.id && revised.enabled, 'Blank URL edit replaced the real channel identity');
      report.expected.label = report.expected.finalLabel;
      await send();
      await auditCounts(3, 1);
      record(active, { channelId: revised.id, blankUrlPreservedEndpoint: true, exactOneSuccessAfterEdit: true });
      active = required[8];
      await closeSettings();
      await openMeeting();
      const fixture = await call('meeting_get', { meetingId });
      assert(fixture.minutes === report.expected.originalMinutes && fixture.audioPath === report.expected.audioPath, 'Unexpected isolated meeting seed');
      const before = snapshots();
      await click(meetingPanel, '系统播放器');
      const failure = await toast(before, '无法打开录音，请检查文件是否仍在本机');
      assert(meetingPanel().querySelector('.meeting-workbench__record-header h2')?.textContent === report.expected.meetingTitle
        && button(meetingPanel(), '编辑纪要'), 'Missing audio error made the meeting unusable');
      record(active, { actualPlayerButton: true, actualNativeFailure: failure.textContent, noAsrPageEntry: true, localRecordingAvailableWithoutAsr: true, recordingStarted: false });
      active = required[9];
      const repair = await service('/repair', 'POST');
      assert(repair.repaired === true && repair.path === report.expected.audioPath && repair.fixture === true, 'Owned parent did not repair the synthetic missing WAV');
      await click(meetingPanel, '编辑纪要');
      const editor = await waitFor(() => meetingPanel()?.querySelector('textarea[aria-label="编辑会议纪要"]'), 'Actual meeting editor after player failure');
      input(editor, report.expected.minutes);
      await click(meetingPanel, '保存纪要');
      await waitFor(() => !meetingPanel()?.querySelector('textarea[aria-label="编辑会议纪要"]')
        && meetingPanel()?.textContent.includes('缺文件错误后可继续编辑和保存。'), 'Saved minutes rendered and editor closed');
      const saved = await call('meeting_get', { meetingId });
      assert(saved.minutes === report.expected.minutes && saved.status === 'completed' && saved.audioPath === report.expected.audioPath, 'UI minutes recovery did not persist');
      await service('/state', 'POST', { expected: report.expected });
      record(active, { minutesSavedThroughUiAndNative: true, syntheticWavRepairedByParent: true, wavSha256: repair.sha256,
        successfulPlayerPlaybackVerified: false, asrOrRecordingVerified: false });
    }
    active = required.at(-1);
    if (visible(document.querySelector('.settings-modal-overlay'))) await closeSettings();
    const homeButton = [...document.querySelectorAll('.sidebar__nav-item')]
      .find((element) => visible(element) && !element.disabled && element.textContent.trim() === '新建任务');
    assert(homeButton, 'Actual return-to-Home control unavailable');
    homeButton.click();
    await waitFor(() => !visible(meetingPanel()) && visible(document.querySelector('.echo-composer')), 'Returned to actual Home composer');
    record(active, { settingsClosed: true, meetingSaved: true, actualHome: true });
    report.passed = required.every((id) => report.checks.filter((check) => check.id === id && check.result === 'passed').length === 1);
  } catch (error) {
    const message = String(error?.message || error).slice(0, 1800);
    report.errors.push(`${active}: ${message}`);
    if (typeof document !== 'undefined') report.failureDom = {
      headings: [...document.querySelectorAll('h1,h2,h3')].filter(visible).map(element => element.textContent?.slice(0, 120)),
      notificationText: panel()?.textContent?.slice(0, 3500),
      disclosureExpanded: panel()?.querySelector('.notify-advanced__toggle')?.getAttribute('aria-expanded'),
      visibleButtons: [...document.querySelectorAll('button')].filter(visible).map(element => ({
        label: element.getAttribute('aria-label') || element.textContent?.trim(), disabled: element.disabled,
      })).slice(-60),
    };
    if (!report.checks.some((check) => check.id === active)) report.checks.push({ id: active, result: 'failed', evidence: message });
    for (const id of required) if (!report.checks.some((check) => check.id === id)) report.checks.push({ id, result: 'blocked', evidence: 'Earlier actual native gate failed' });
  }
  report.durationMs = 180000 - Math.max(0, deadline - Date.now());
  report.finishedAt = new Date().toISOString();
  return report;
};

window.__echoNativeLocalReviewLaunch = async (scope) => {
  if (window.__echoNativeLocalReviewStarted) return;
  window.__echoNativeLocalReviewStarted = true;
  const invoke = window.__TAURI_INTERNALS__?.invoke;
  let report;
  try {
    if (typeof invoke !== 'function') throw new Error('Production native IPC unavailable');
    const context = await invoke('desktop_review_context');
    report = await window.__echoNativeLocalReview({ invoke, context, scope });
  } catch (error) {
    report = { version: 1, kind: 'local', phase: 'unknown', passed: false, checks: [],
      errors: [String(error?.message || error).slice(0, 1800)], finishedAt: new Date().toISOString() };
  }
  await invoke('desktop_review_complete', { report });
};
