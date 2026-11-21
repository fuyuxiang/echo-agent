// Fixed, isolated native CloudStorage acceptance. All mutations use production
// DOM handlers. Native IPC is never replaced; no model, contact, upload/download
// dialog, arbitrary endpoint, or existing user profile is involved.
window.__echoNativeWebDavReview = async ({ invoke, context, scope }) => {
  const phase = context?.phase;
  const restart = phase === 'restart';
  const required = restart
    ? ['native-webdav-isolation', 'native-webdav-settings-entry', 'native-webdav-provider-restore',
      'native-webdav-file-restore', 'native-webdav-revoke-preserves-file', 'native-webdav-return-shell']
    : ['native-webdav-isolation', 'native-webdav-settings-entry', 'native-webdav-config-create',
      'native-webdav-connection-list', 'native-webdav-create-edit-read', 'native-webdav-real-delete',
      'native-webdav-readonly-failure-draft', 'native-webdav-settings-leave-cancel',
      'native-webdav-retry-save', 'native-webdav-disabled-recovery', 'native-webdav-return-shell'];
  const report = { version: 1, kind: 'webdav', phase, environment: 'actual native WebView + production IPC + standard WsgiDAV',
    passed: false, checks: [], errors: [], expected: null, nativeContext: context,
    uploadDownloadDialogVerified: false, modelRequested: false };
  const deadline = Date.now() + 180000;
  const assert = (condition, message) => { if (!condition) throw new Error(message); };
  const path = (value) => {
    assert(typeof value === 'string' && value.startsWith('/') && !/[\x00-\x1f\\]/.test(value), 'Invalid canonical scope path');
    assert(!value.split('/').some((part) => part === '.' || part === '..') && !value.endsWith('/'), 'Scope path is not canonical');
    return value;
  };
  const redacted = (error) => {
    let message = String(error?.message || error);
    for (const secret of [scope?.password, scope?.username]) if (typeof secret === 'string' && secret) message = message.split(secret).join('[redacted]');
    return message.slice(0, 1500);
  };
  const record = (id, evidence) => report.checks.push({ id, result: 'passed', evidence });
  const visible = (element) => Boolean(element?.isConnected && element.getClientRects().length);
  const delay = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
  const waitFor = async (check, label, timeout = 20000) => {
    const end = Math.min(deadline, Date.now() + timeout);
    while (Date.now() < end) {
      let timer;
      try {
        const remaining = end - Date.now();
        const result = await Promise.race([Promise.resolve().then(check), new Promise((_resolve, reject) => {
          timer = setTimeout(() => reject(new Error(`${label} timed out`)), remaining);
        })]);
        if (result) return result;
      } finally { clearTimeout(timer); }
      await delay(Math.min(100, Math.max(0, end - Date.now())));
    }
    throw new Error(`${label} timed out`);
  };
  const allowed = new Set(['storage_providers_list', 'storage_provider_test', 'storage_list', 'storage_read_text']);
  const call = async (command, args = {}) => {
    assert(allowed.has(command), 'Unexpected native command; this helper only observes production storage');
    const remaining = Math.min(25000, deadline - Date.now());
    assert(remaining > 0, 'Native WebDAV review exceeded its 180s deadline');
    let timer;
    try { return await Promise.race([invoke(command, args), new Promise((_resolve, reject) => {
      timer = setTimeout(() => reject(new Error(`${command} timed out`)), remaining);
    })]); } finally { clearTimeout(timer); }
  };
  const panel = () => document.querySelector('.settings-modal-overlay .storage-panel[aria-label="云存储"]');
  const button = (parent, label) => [...(parent?.querySelectorAll('button') || [])]
    .find((element) => visible(element) && !element.disabled && element.textContent?.trim() === label);
  const clickButton = async (parent, label) => {
    const element = await waitFor(() => button(typeof parent === 'function' ? parent() : parent, label), `Actual ${label} button`);
    assert(visible(element) && !element.disabled, 'Refusing an unavailable or detached native DOM button');
    element.click();
  };
  const inputValue = (element, value) => {
    assert(visible(element) && !element.disabled, 'Refusing an unavailable native input');
    const prototype = element instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype
      : element instanceof HTMLSelectElement ? HTMLSelectElement.prototype : HTMLInputElement.prototype;
    Object.getOwnPropertyDescriptor(prototype, 'value').set.call(element, value);
    element.dispatchEvent(new Event(element instanceof HTMLSelectElement ? 'change' : 'input', { bubbles: true }));
  };
  const toastSnapshot = () => new Set(document.querySelectorAll('.toast__message'));
  const newToast = (previous, text) => waitFor(() => [...document.querySelectorAll('.toast__message')]
    .find((element) => visible(element) && !previous.has(element) && element.textContent?.includes(text)), `Actual ${text} feedback`, 25000);
  const appDialog = (title) => waitFor(() => [...document.querySelectorAll('.app-dialog')]
    .find((element) => visible(element) && element.querySelector('.app-dialog__title')?.textContent === title), `Actual ${title} dialog`);
  const fillDialog = (dialog, label, value) => {
    const field = [...dialog.querySelectorAll('.app-dialog__field')]
      .find((element) => element.querySelector('span')?.textContent?.replace(/\s*\*$/, '').trim() === label);
    inputValue(field?.querySelector('input,textarea'), value);
  };
  const openSettings = async () => {
    if (!visible(document.querySelector('.settings-modal-overlay'))) {
      (await waitFor(() => {
        const element = document.querySelector('.sidebar__icon-btn[aria-label="设置"]');
        return visible(element) && !element.disabled ? element : null;
      }, 'Actual Settings entry')).click();
    }
    await clickButton(() => document.querySelector('.settings-modal-overlay .settings-navigation'), '云存储');
    return waitFor(() => {
      const current = panel();
      return visible(current) && button(current, '添加存储源') ? current : null;
    }, 'Loaded production CloudStoragePanel');
  };
  const selected = () => panel()?.querySelector('select[aria-label="选择云存储源"]');
  const select = async (id) => {
    const picker = await waitFor(() => {
      const element = selected();
      return visible(element) && [...element.options].some((option) => option.value === id) ? element : null;
    }, 'Actual configured storage option');
    inputValue(picker, id);
    await waitFor(() => selected()?.value === id, 'Actual storage selection');
  };
  const waitListing = (name) => waitFor(() => {
    const current = panel();
    if (!visible(current) || current.querySelector('.storage-panel__loading,.panel-inline-error')) return null;
    return [...current.querySelectorAll('.storage-panel__entry')]
      .find((entry) => entry.querySelector('.storage-panel__entry-name')?.textContent === name);
  }, 'Actual remote directory listing', 25000);
  const navigateDirectory = async (name) => {
    const entry = await waitListing(name);
    const opener = entry.querySelector('button[title="打开目录"]');
    assert(visible(opener) && !opener.disabled, 'Actual directory is not navigable');
    opener.click();
    await waitFor(() => panel()?.querySelector('.storage-panel__path')?.textContent === `/${name}`, 'Actual directory path');
  };
  const previewInput = (name) => panel()?.querySelector(`textarea[aria-label="编辑 ${CSS.escape(name)}"]`);
  const openFile = async (name, content) => {
    const entry = await waitListing(name);
    const reader = entry.querySelector(`button[aria-label="读取 ${CSS.escape(name)}"]`);
    assert(visible(reader) && !reader.disabled, 'Actual read entry unavailable');
    reader.click();
    return waitFor(() => {
      const input = previewInput(name);
      return visible(input) && input.value === content ? input : null;
    }, 'Real UTF-8 file preview', 25000);
  };
  const createText = async (name, content) => {
    await clickButton(panel, '新建文本');
    const dialog = await appDialog('新建文本文件');
    fillDialog(dialog, '文件名', name);
    fillDialog(dialog, '文件内容', content);
    await clickButton(dialog, '创建文件');
    await waitFor(() => !visible(dialog), 'Native text creation completes', 25000);
    await waitListing(name);
  };
  const editConfig = async (changes = {}) => {
    await clickButton(panel, '编辑配置');
    const config = await waitFor(() => {
      const element = panel()?.querySelector('.storage-panel__config');
      return visible(element) ? element : null;
    }, 'Actual provider configuration editor');
    if (changes.baseUrl) inputValue(config.querySelector('[aria-label="WebDAV 地址"]'), changes.baseUrl);
    if (typeof changes.enabled === 'boolean') {
      const enabled = config.querySelector('.storage-panel__enabled input[type="checkbox"]');
      assert(visible(enabled) && !enabled.disabled, 'Actual enabled checkbox unavailable');
      if (enabled.checked !== changes.enabled) enabled.click();
      await waitFor(() => enabled.checked === changes.enabled, 'Actual enabled toggle');
    }
    await clickButton(config, '保存');
    await waitFor(() => !visible(config), 'Provider editor completes');
  };
  const createConfig = async (label, baseUrl) => {
    await clickButton(panel, '添加存储源');
    const config = await waitFor(() => {
      const element = panel()?.querySelector('.storage-panel__config');
      return visible(element) ? element : null;
    }, 'Actual new provider editor');
    for (const [aria, value] of [['存储源显示名', label], ['WebDAV 地址', baseUrl],
      ['WebDAV 用户名', scope.username], ['WebDAV 密码或应用令牌', scope.password]]) {
      inputValue(config.querySelector(`[aria-label="${aria}"]`), value);
    }
    await clickButton(config, '保存');
    await waitFor(() => !visible(config), 'Actual new provider saved');
    const providers = await call('storage_providers_list');
    const matches = providers.filter((provider) => provider.label === label && provider.baseUrl === baseUrl);
    assert(matches.length === 1 && matches[0].enabled && matches[0].password === '••••', 'Provider UI save did not persist a unique enabled masked config');
    return matches[0].id;
  };
  let activeCheck = required[0];
  try {
    assert(invoke === window.__TAURI_INTERNALS__?.invoke, 'Production native invoke required');
    assert(phase === 'first-launch' || restart, 'Explicit actual native phase required');
    assert(context.webdavRequested === true && scope?.schema === 1, 'Explicit guarded WebDAV review scope required');
    const home = path(scope.home), root = path(scope.root);
    assert(path(context.home) === home && /^\/private\/(?:tmp|var\/folders)\/(?:[^/]+\/)*echo-native-review-[^/]+$/.test(home),
      'Refusing a non-temporary native home');
    assert(/^\/private\/tmp\/echo-webdav-real-[^/]+\/physical-root$/.test(root), 'Refusing a non-test physical WebDAV root');
    assert(/^[a-f0-9]{32}$/.test(scope.nonce || '') && scope.readOnlySharesPhysicalRoot === true, 'Missing unique standard service scope');
    const rwUrl = new URL(scope.readWriteUrl), roUrl = new URL(scope.readOnlyUrl);
    for (const url of [rwUrl, roUrl]) assert(url.protocol === 'http:' && url.hostname === '127.0.0.1' && url.port
      && !url.username && !url.password && !url.search && !url.hash, 'Only the scoped loopback standard service is allowed');
    assert(rwUrl.origin === roUrl.origin && rwUrl.pathname === '/rw' && roUrl.pathname === '/ro', 'Standard service share mismatch');
    assert(typeof scope.username === 'string' && scope.username && typeof scope.password === 'string' && scope.password, 'Private synthetic credential missing');
    const directory = `原生云验收-${scope.nonce}`;
    const initial = `中文 WebDAV 初始内容 ${scope.nonce}\n| 列一 | 列二 |\n| --- | --- |\n| 中文 | 空格 |\n`;
    const edited = `中文 WebDAV 编辑保存 ${scope.nonce}\n第1行\n第2行 保留空格\n`;
    const recovery = `只读失败后重试成功 ${scope.nonce}\n本地草稿必须保留。\n`;
    const fileName = '中文 空格.md', recoveryName = '失败恢复 中文.md', deletedName = '仅删合成 临时.txt';
    const providerLabels = { rw: `读写验收-${scope.nonce}`, recovery: `恢复验收-${scope.nonce}` };
    report.expected = restart ? scope.expected : { version: 1, home, root, nonce: scope.nonce,
      directory, fileName, recoveryName, deletedName, editedContent: edited, recoveryContent: recovery,
      readWriteUrl: scope.readWriteUrl, readOnlyUrl: scope.readOnlyUrl, providerLabels, rwId: null, recoveryId: null };
    assert(!restart || report.expected?.version === 1 && report.expected.home === home && report.expected.root === root
      && report.expected.nonce === scope.nonce && report.expected.directory === directory
      && report.expected.fileName === fileName && report.expected.recoveryName === recoveryName
      && report.expected.editedContent === edited && report.expected.recoveryContent === recovery
      && report.expected.readWriteUrl === scope.readWriteUrl && report.expected.readOnlyUrl === scope.readOnlyUrl,
    'Restart does not match successful first-launch expectations');
    record(activeCheck, { home, root, standardLoopbackOnly: true, productionInvoke: true, modelRequested: false });
    activeCheck = required[1];
    await openSettings();
    record(activeCheck, { settings: true, cloudPanel: true, actualDom: true });
    if (restart) {
      activeCheck = required[2];
      const configs = await call('storage_providers_list');
      assert(configs.length === 2 && [report.expected.rwId, report.expected.recoveryId]
        .every((id) => configs.filter((item) => item.id === id && item.enabled && item.password === '••••').length === 1),
      'Restart did not reload both real provider configurations');
      await select(report.expected.rwId);
      await navigateDirectory(directory);
      record(activeCheck, { providerIds: configs.map((item) => item.id), reselectedInUi: true, realRemoteDirectory: true });
      activeCheck = required[3];
      await openFile(fileName, edited);
      assert(await call('storage_read_text', { id: report.expected.rwId, path: `/${directory}/${fileName}` }) === edited, 'Restart native GET differs');
      await openFile(recoveryName, recovery);
      assert(await call('storage_read_text', { id: report.expected.recoveryId, path: `/${directory}/${recoveryName}` }) === recovery, 'Restart recovered native GET differs');
      record(activeCheck, { fileName, recoveryName, exactContent: true, actualUiAndIpc: true });
      activeCheck = required[4];
      for (const [id, label] of [[report.expected.rwId, providerLabels.rw], [report.expected.recoveryId, providerLabels.recovery]]) {
        await select(id);
        await clickButton(panel, '移除配置');
        const dialog = await appDialog(`移除存储源“${label}”？`);
        await clickButton(dialog, '移除配置');
        await waitFor(() => !visible(dialog), 'Actual provider revocation');
        let rejected = false;
        try { await call('storage_read_text', { id, path: `/${directory}/${fileName}` }); }
        catch (error) { rejected = redacted(error).includes('不存在或未启用'); }
        assert(rejected, 'Revoked provider remained accessible through production IPC');
      }
      assert((await call('storage_providers_list')).length === 0, 'UI revocation did not persist');
      record(activeCheck, { providers: 0, uiConfirmedBothRemovals: true, nativeAccessDenied: true,
        physicalFilesCheckedByParent: true });
    } else {
      activeCheck = required[2];
      assert((await call('storage_providers_list')).length === 0 && !scope.expected, 'Preserving a nonempty profile; fresh isolated acceptance required');
      report.expected.rwId = await createConfig(providerLabels.rw, scope.readWriteUrl);
      report.expected.recoveryId = await createConfig(providerLabels.recovery, scope.readOnlyUrl);
      assert(report.expected.rwId !== report.expected.recoveryId, 'Provider identifiers collided');
      record(activeCheck, { providerIds: [report.expected.rwId, report.expected.recoveryId], createdThroughUi: true, maskedPersistence: true });
      activeCheck = required[3];
      await select(report.expected.rwId);
      await waitListing(scope.seedName);
      const toasts = toastSnapshot();
      await clickButton(panel, '测试连接');
      await newToast(toasts, '连接正常');
      assert(await call('storage_read_text', { id: report.expected.rwId, path: `/${scope.seedName}` }) === scope.seedContent, 'Real seed content mismatch');
      record(activeCheck, { connectionFeedback: true, chineseSpacedSeed: true, realGet: true });
      activeCheck = required[4];
      await clickButton(panel, '新建文件夹');
      const folder = await appDialog('新建文件夹');
      fillDialog(folder, '文件夹名称', directory);
      await clickButton(folder, '创建');
      await waitFor(() => !visible(folder), 'Native folder creation');
      await navigateDirectory(directory);
      await createText(fileName, initial);
      const text = await openFile(fileName, initial);
      inputValue(text, edited);
      await clickButton(() => panel()?.querySelector('.storage-panel__preview'), '保存');
      await waitFor(() => [...(panel()?.querySelectorAll('.storage-panel__preview button') || [])].some(element => element.textContent?.trim() === '已保存' && element.disabled), 'Text saved inline');
      assert(await call('storage_read_text', { id: report.expected.rwId, path: `/${directory}/${fileName}` }) === edited, 'UI text save did not persist actual bytes');
      await createText(recoveryName, initial);
      record(activeCheck, { directory, fileName, content: edited, actualUiCreationAndSave: true, exactNativeGet: true });
      activeCheck = required[5];
      await createText(deletedName, '只删除本轮创建的合成文件。');
      const scratch = await waitListing(deletedName);
      scratch.querySelector(`button[aria-label="删除 ${CSS.escape(deletedName)}"]`).click();
      const deletion = await appDialog(`删除文件“${deletedName}”？`);
      await clickButton(deletion, '从远端删除');
      await waitFor(() => !visible(deletion), 'Native scratch deletion');
      const entries = await call('storage_list', { id: report.expected.rwId, path: `/${directory}` });
      assert(!entries.some((item) => item.name === deletedName), 'Actual scratch deletion did not occur');
      record(activeCheck, { name: deletedName, ownScratchOnly: true, actualIpcList: true });
      activeCheck = required[6];
      await select(report.expected.recoveryId);
      await navigateDirectory(directory);
      const recoveryInput = await openFile(recoveryName, initial);
      inputValue(recoveryInput, recovery);
      const deniedToasts = toastSnapshot();
      await clickButton(() => panel()?.querySelector('.storage-panel__preview'), '保存');
      const deniedToast = await newToast(deniedToasts, '保存失败');
      assert(deniedToast.textContent.includes('没有访问权限'), 'Readonly save should explain the permission problem');
      const details = deniedToast.closest('.toast')?.querySelector('.toast__details');
      if (details && !details.open) details.querySelector('summary').click();
      const failure = details?.querySelector('pre') ?? deniedToast;
      assert(visible(failure) && /\bHTTP\s+403\b/i.test(failure.textContent)
        && previewInput(recoveryName)?.value === recovery,
        'Readonly save did not expose the real HTTP 403 and retain the draft');
      assert(await call('storage_read_text', { id: report.expected.rwId, path: `/${directory}/${recoveryName}` }) === initial, 'Readonly failure modified physical source');
      record(activeCheck, { status: 403, draftRetained: true, remoteInitialContentPreserved: true });
      activeCheck = required[7];
      document.querySelector('.settings-modal__close[aria-label="关闭设置"]').click();
      const leave = await appDialog('舍弃云端文件未保存的修改？');
      await clickButton(leave, '取消');
      assert(visible(panel()) && previewInput(recoveryName)?.value === recovery, 'Cancel leaving Settings lost cloud draft');
      record(activeCheck, { realLeaveConfirmation: true, cancelled: true, draftRetained: true });
      activeCheck = required[8];
      await editConfig({ baseUrl: scope.readWriteUrl });
      assert(previewInput(recoveryName)?.value === recovery, 'Endpoint recovery dropped local draft');
      await clickButton(() => panel()?.querySelector('.storage-panel__preview'), '保存');
      await waitFor(() => [...(panel()?.querySelectorAll('.storage-panel__preview button') || [])].some(element => element.textContent?.trim() === '已保存' && element.disabled), 'Retried save shown inline');
      assert(await call('storage_read_text', { id: report.expected.recoveryId, path: `/${directory}/${recoveryName}` }) === recovery, 'Retry did not persist exact retained draft');
      record(activeCheck, { recoveredThroughConfigUi: true, retryThroughUi: true, exactContent: recovery });
      activeCheck = required[9];
      await select(report.expected.rwId);
      await editConfig({ enabled: false });
      const disabledPicker = await waitFor(() => selected(), 'Disabled provider management selector');
      assert([...disabledPicker.options].some((option) => option.value === report.expected.rwId && option.textContent.includes('已禁用')),
        'Disabled provider lost its UI recovery entry');
      await select(report.expected.rwId);
      assert(button(panel(), '编辑配置') && button(panel(), '移除配置') && !panel().querySelector('.storage-panel__breadcrumb'),
        'Disabled provider must be editable/removable without remote file operations');
      let disabledDenied = false;
      try { await call('storage_read_text', { id: report.expected.rwId, path: `/${directory}/${fileName}` }); }
      catch (error) { disabledDenied = redacted(error).includes('未启用'); }
      assert(disabledDenied, 'Disabled provider bypassed backend enforcement');
      await editConfig({ enabled: true });
      await navigateDirectory(directory);
      await openFile(fileName, edited);
      record(activeCheck, { disabledOptionReachable: true, noDisabledBrowse: true, nativeAccessDenied: true, reenabledThroughUi: true });
    }
    activeCheck = required.at(-1);
    document.querySelector('.settings-modal__close[aria-label="关闭设置"]').click();
    await waitFor(() => !visible(document.querySelector('.settings-modal-overlay')), 'Settings closes after saved data');
    record(activeCheck, { savedData: true, settingsClosed: true });
    report.passed = required.every((id) => report.checks.filter((check) => check.id === id && check.result === 'passed').length === 1);
  } catch (error) {
    report.errors.push(`${activeCheck}: ${redacted(error)}`);
    if (!report.checks.some((check) => check.id === activeCheck)) report.checks.push({ id: activeCheck, result: 'failed', evidence: redacted(error) });
    for (const id of required) if (!report.checks.some((check) => check.id === id)) report.checks.push({ id, result: 'blocked', evidence: 'Earlier actual native check failed' });
  }
  report.durationMs = 180000 - Math.max(0, deadline - Date.now());
  report.finishedAt = new Date().toISOString();
  return report;
};

// Root injects only this fixed factory with a validated private service scope.
// The factory completes a native report even when a real UI gate fails.
window.__echoNativeWebDavReviewLaunch = async (scope) => {
  if (window.__echoNativeWebDavReviewStarted) return;
  window.__echoNativeWebDavReviewStarted = true;
  const invoke = window.__TAURI_INTERNALS__?.invoke;
  let report;
  try {
    if (typeof invoke !== 'function') throw new Error('Production native IPC unavailable');
    const context = await invoke('desktop_review_context');
    report = await window.__echoNativeWebDavReview({ invoke, context, scope });
  } catch (error) {
    let message = String(error?.message || error);
    for (const secret of [scope?.password, scope?.username]) if (typeof secret === 'string' && secret) message = message.split(secret).join('[redacted]');
    report = { version: 1, kind: 'webdav', phase: 'unknown', environment: 'actual native WebView',
      passed: false, checks: [], errors: [message.slice(0, 1000)], finishedAt: new Date().toISOString() };
  }
  await invoke('desktop_review_complete', { report });
};
