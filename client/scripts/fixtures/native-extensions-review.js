// Fixed actual desktop journey. No replaced IPC, model replies or Runtime tools.
window.__echoNativeExtensionsReview = async scope => {
  if (window.__echoNativeExtensionsStarted) return;
  window.__echoNativeExtensionsStarted = true;
  const invoke = window.__TAURI_INTERNALS__.invoke, sleep = ms => new Promise(r => setTimeout(r, ms));
  const report = { version: 1, kind: 'native-extensions', home: scope.home, nonce: scope.nonce, phase: scope.phase,
    fakeIPC: false, passed: false, checks: [], errors: [], permissions: [], expected: scope.expected };
  const completions = [], updates = [], listeners = [], handled = new Set(), deadline = Date.now() + 1100000;
  let sessionId, turnPhase;
  const assert = (v, t) => { if (!v) throw new Error(t); };
  const call = async (command, args = {}) => {
    let timer; try { return await Promise.race([invoke(command, args), new Promise((_, reject) => {
      timer = setTimeout(() => reject(new Error(`Native IPC timed out: ${command}`)), 30000);
    })]); } finally { clearTimeout(timer); }
  };
  const visible = el => !!el && el.getClientRects().length > 0;
  const button = (root, text) => [...(root?.querySelectorAll('button') ?? [])].find(el => visible(el) && !el.disabled && el.textContent.trim() === text);
  const writeProof = (path, content) => call('write_text_file', { path, workspaceRoot: scope.cwd, content: JSON.stringify(content) });
  const progress = () => writeProof('extensions-progress.json', { step: report.activeStep, checks: report.checks, at: new Date().toISOString() });
  const wait = async (read, label, ms = 30000) => {
    report.activeStep = label; await progress(); const end = Math.min(deadline, Date.now() + ms);
    while (Date.now() < end) { const v = await read(); if (v) return v; await sleep(200); }
    throw new Error(`Actual native condition timed out: ${label}`);
  };
  const click = async (root, label) => (await wait(() => button(root(), label), `UI ${label}`)).click();
  const control = (selector, label, root = () => document) => wait(() => {
    const el = root()?.querySelector(selector); return el && !el.disabled && el;
  }, label);
  const set = (el, value) => { assert(el, 'Missing actual form field'); Object.getOwnPropertyDescriptor(
    el.tagName === 'TEXTAREA' ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype, 'value').set.call(el, value);
    el.dispatchEvent(new Event('input', { bubbles: true })); };
  const stage = async (id, action) => { report.activeStep = id; await progress();
    try { const evidence = await action(); report.checks.push({ id, result: 'passed', evidence }); await progress(); }
    catch (error) { report.checks.push({ id, result: 'failed', error: String(error) }); throw error; } };
  const nav = async label => {
    (await wait(() => document.querySelector('.sidebar__nav-item[aria-label="专家·技能·连接器"]'), 'Native capabilities navigation')).click();
    await click(() => document.querySelector('nav[aria-label="专家·技能·连接器管理"]'), label);
  };
  const catalog = () => call('agent_list_all_sessions', { includeArchived: true });
  const skills = () => call('skills_list', { cwd: scope.cwd });
  const servers = (refresh = false) => call('mcp_list', { sessionId, refresh });
  const pluginList = async () => (await call('plugins_list', { sessionId })).plugins;
  const managed = list => list.find(s => s.name === 'native-review-skill');
  const pluginSkill = list => list.find(s => s.name.includes('plugin-marker'));
  const ownServer = list => list.find(s => s.name === 'native-extensions');
  const ownPlugin = list => list.find(p => p.name === 'native-review-plugin');
  const canonical = value => Array.isArray(value) ? value.map(canonical) : value && typeof value === 'object'
    ? Object.fromEntries(Object.keys(value).sort().map(key => [key, canonical(value[key])])) : value;
  const sameConfig = (a, b) => JSON.stringify(canonical(JSON.parse(a))) === JSON.stringify(canonical(JSON.parse(b)));
  const reply = () => { const last = [...document.querySelectorAll('.msg--user')].filter(visible).at(-1);
    return [...document.querySelectorAll('.msg--assistant .msg__body > .markdown-body[data-md-theme="loose"]')]
      .filter(el => visible(el) && (!last || last.compareDocumentPosition(el) & Node.DOCUMENT_POSITION_FOLLOWING)).map(el => el.innerText).join('\n'); };
  const select = async () => {
    (await wait(() => document.querySelector(`[data-session-id="${sessionId}"] .sidebar__conv-select`), 'Original native session row')).click();
    await wait(() => { const p = document.querySelector('.echo-composer .model-selector__trigger'); return visible(p) && !p.disabled && p; }, 'Restored actual Composer ready', 60000);
  };
  const send = async text => {
    const area = await wait(() => { const e = document.querySelector('.echo-composer textarea'); return visible(e) && !e.disabled && e; }, 'Real task Composer'); set(area, text);
    (await wait(() => { const b = document.querySelector('button[aria-label="发送"]'); return visible(b) && !b.disabled && b; }, 'Real send button')).click();
  };
  const servicePermissions = async () => {
    const p = await call('agent_list_pending_interactions', { sessionId: null });
    assert(!p.questions?.length && !p.folderTrustRequests?.length && !p.planApprovals?.length, 'Unexpected interactive operation');
    for (const request of p.permissions ?? []) {
      if (handled.has(request.requestId)) continue;
      let raw = request.rawInput; if (typeof raw === 'string') { try { raw = JSON.parse(raw); } catch { raw = null; } }
      let name = raw?.tool_name ?? '', args = raw?.tool_input;
      if (name.toLowerCase() === 'callmcptool') { name = args?.name ?? args?.tool_name ?? ''; args = args?.arguments; }
      if (name.split(':').at(-1) === 'use_tool') { name = args?.tool_name ?? ''; args = args?.tool_input; }
      if (typeof args === 'string') { try { args = JSON.parse(args); } catch { args = null; } }
      const allowed = request.sessionId === sessionId && ['native-extensions__echo_deliver', 'native-extensions__echo_probe',
        'mcp__native-extensions__echo_deliver', 'mcp__native-extensions__echo_probe'].includes(name)
        && args?.nonce === scope.nonce && args?.phase === turnPhase && Object.keys(args).sort().join(',') === 'nonce,phase';
      if (!allowed) {
        await call('agent_resolve_permission', { requestId: request.requestId, optionId: null, cancelled: true });
        throw new Error(`Refused unexpected permission: ${JSON.stringify({ sessionId: request.sessionId, raw })}`);
      }
      const card = document.getElementById(`permission-${request.requestId}`), allow = button(card, '允许一次');
      if (!visible(card) || !allow) continue;
      assert(!updates.some(u => u.sessionId === sessionId && u.toolCallId === request.toolCallId && u.status === 'completed'), 'Tool completed before its actual approval');
      allow.click(); handled.add(request.requestId); report.permissions.push({ requestId: request.requestId, toolCallId: request.toolCallId,
        sessionId, name, args, via: 'production allow-once UI', pendingBeforeApproval: true });
    }
  };
  const finish = async (start, marker) => wait(async () => {
    await servicePermissions(); const c = completions.slice(start).find(c => c.sessionId === sessionId);
    if (!c) return null; assert(c.stopReason === 'end_turn', `Real model ended ${c.stopReason}`);
    const text = reply(); assert(text.includes(marker), `Actual model reply missing ${marker}: ${text.slice(0, 1200)}`);
    return { completion: c, reply: text.slice(0, 5000), tools: updates.filter(u => u.sessionId === sessionId
      && u._meta?.promptId === c.promptId && u._meta?.isReplay !== true).slice(-50) };
  }, 'Actual model completion, tool results and visible reply', 240000);
  const listen = async (event, fn) => {
    const handler = window.__TAURI_INTERNALS__.transformCallback(({ payload }) => fn(payload));
    const id = await call('plugin:event|listen', { event, target: { kind: 'Any' }, handler }); listeners.push({ event, id });
  };
  const toolRow = () => [...document.querySelectorAll('.mcp-item-wrap')].find(el => el.querySelector('.mcp-item-name')?.textContent.includes('native-extensions'));
  try {
    report.nativeContext = await call('desktop_review_context');
    await stage('extensions-isolation', async () => {
      assert(report.nativeContext.home === scope.home && report.nativeContext.phase === scope.phase && report.nativeContext.evaluations === 1
        && /^\/private\/tmp\/echo-native-review-extensions-[A-Za-z0-9_-]+$/.test(scope.home) && scope.cwd === `${scope.home}/workspaces/default`, 'Native scope mismatch');
      await wait(async () => { const a = await call('agent_auth_status'); return a.ready && a.runtimeReady && a.synchronized && a.runtimeModels.includes(scope.modelId); }, 'Explicit native model readiness', 90000);
      await listen('agent://complete', c => completions.push(c));
      await listen('agent://update', u => { if (['tool_call', 'tool_call_update'].includes(u.sessionUpdate ?? u.type) && updates.length < 300) updates.push(u); });
      return { home: scope.home, cwd: scope.cwd, modelId: scope.modelId };
    });
    const input = JSON.parse(await call('read_text_file', { path: `${scope.cwd}/extension-fixtures/inputs.json`, cwd: scope.cwd, maxBytes: 4096 }));
    assert(input.nonce === scope.nonce && input.server === `${scope.cwd}/extension-fixtures/server.mjs`, 'Invalid fixed fixture inputs');
    const skillMarker = `SKILL-${[...scope.nonce].reverse().join('')}`, pluginMarker = `PLUGIN-${scope.nonce.slice(16)}${scope.nonce.slice(0, 16)}`;
    if (scope.phase === 'first-launch') {
      report.expected = { schema: 1, home: scope.home, cwd: scope.cwd, nonce: scope.nonce, modelId: scope.modelId, skillMarker, pluginMarker };
      const e = report.expected;
      (await wait(() => document.querySelector('.model-selector__trigger'), 'Initial model selector')).click();
      (await wait(() => document.querySelector(`.model-selector__item[data-model-id="${scope.modelId}"]`), 'Exact configured model')).click();
      (await wait(() => document.querySelector('.workspace-picker__trigger'), 'Workspace selector')).click();
      await writeProof('extensions-workspace-picker-ready.json', { nonce: scope.nonce, cwd: scope.cwd }); await click(() => document, '选择文件夹…');
      await wait(() => document.querySelector('.workspace-picker__trigger')?.getAttribute('aria-label') === `工作目录：${scope.cwd}。点击切换`, 'Native private workspace selected');
      report.workspaceSelection = { cwd: scope.cwd, via: 'native folder picker' };
      await stage('extensions-real-session', async () => {
        const start = completions.length; await send(`只回复 BOOT-${scope.nonce}，不要调用工具。`);
        const s = await wait(async () => (await catalog()).find(s => s.cwd === scope.cwd && !s.hidden), 'Real original task'); sessionId = e.sessionId = s.sessionId;
        return finish(start, `BOOT-${scope.nonce}`);
      });
      await stage('connector-config-validation-and-consent', async () => {
        await nav('连接器'); await click(() => document, '配置连接器');
        const editor = await wait(() => document.querySelector('textarea[aria-label="MCP 配置内容"]'), 'Actual connector editor');
        const original = await call('mcp_config_read'); set(editor, '{ invalid'); await click(() => document, '保存');
        await wait(() => document.querySelector('.mcp-editor-error')?.textContent.includes('无效的 JSON'), 'Invalid JSON feedback');
        assert(sameConfig((await call('mcp_config_read')).content, original.content), 'Invalid editor input changed configuration');
        const config = JSON.parse(original.content); config.mcpServers ??= {};
        config.mcpServers['native-extensions'] = { command: input.node, args: [input.server, scope.home, scope.nonce] };
        set(editor, JSON.stringify(config, null, 2));
        await writeProof('extensions-native-action.json', { nonce: scope.nonce, action: 'stdio-cancel' }); await click(() => document, '保存');
        await wait(() => document.querySelector('.mcp-editor-error')?.textContent.includes('用户未允许'), 'Actual native process consent cancellation', 45000);
        assert(sameConfig((await call('mcp_config_read')).content, original.content), 'Cancelled consent changed configuration');
        await writeProof('extensions-native-action.json', { nonce: scope.nonce, action: 'stdio-allow' }); await click(() => document, '保存');
        await wait(async () => { const s = ownServer(await servers()); return s?.live && s.status === 'ready' && s.tools.length === 2 && s; }, 'Real MCP initialize and tools/list after UI consent', 60000);
        await click(() => document, '返回连接器列表');
        await wait(() => toolRow()?.textContent.includes('可用'), 'Native connector live ready state');
        return { nativeConsentCancelledThenApproved: true, invalidJsonDidNotMutate: true, server: ownServer(await servers()) };
      });
      await stage('connector-tool-and-server-toggle', async () => {
        toolRow().querySelector('.mcp-item-main').click();
        const probe = ownServer(await servers()).tools.find(t => t.name === 'echo_probe' || t.name === 'native-extensions__echo_probe');
        assert(probe, 'Actual read-only probe tool absent'); const label = probe.displayName || probe.name;
        (await control(`[aria-label="停用工具 ${CSS.escape(label)}"]`, 'Actual individual tool toggle', toolRow)).click();
        await wait(async () => ownServer(await servers())?.tools.find(t => t.name === 'echo_probe' || t.name === 'native-extensions__echo_probe')?.enabled === false, 'Native tool disabled');
        (await control(`[aria-label="启用工具 ${CSS.escape(label)}"]`, 'Tool reenable', toolRow)).click();
        await wait(async () => ownServer(await servers())?.tools.find(t => t.name === 'echo_probe' || t.name === 'native-extensions__echo_probe')?.enabled, 'Native tool reenabled');
        (await control('[aria-label="停用 MCP 服务 native-extensions"]', 'Enabled actual server disable control', toolRow)).click();
        await wait(async () => !ownServer(await servers())?.enabled, 'Native server disabled');
        await writeProof('extensions-native-action.json', { nonce: scope.nonce, action: 'stdio-reenable' });
        (await control('[aria-label="启用 MCP 服务 native-extensions"]', 'Actual server reenable', toolRow)).click();
        return wait(async () => { const s = ownServer(await servers()); return s?.enabled && s.status === 'ready' && s; }, 'Native server reconnected', 60000);
      });
      await stage('skill-native-inspection-install', async () => {
        await nav('技能'); await click(() => document, '添加技能');
        await writeProof('extensions-native-action.json', { nonce: scope.nonce, action: 'skill-picker' });
        await click(() => document, '或选择技能文件夹（自动识别全部技能）');
        const row = await wait(() => [...document.querySelectorAll('.sk-batch-row--ready')].find(el => el.textContent.includes('native-review-skill')), 'Actual local package inspection', 60000);
        const approval = row.querySelector('input[type="checkbox"]'); if (approval && !approval.checked) approval.click();
        await click(() => row, '安装技能');
        const installed = await wait(async () => { const s = managed(await skills()); return s?.managed && s.enabled && s; }, 'Managed package installed and live reload acknowledged', 60000);
        e.installedSkillPath = installed.path;
        (await control('button[aria-label="关闭"]', 'Close actual skill import')).click();
        return installed;
      });
      await stage('skill-and-plugin-live-toggle', async () => {
        (await control('input[aria-label="停用native-review-skill"]', 'Managed skill toggle')).click();
        await wait(async () => managed(await skills())?.enabled === false, 'Native skill disabled');
        (await control('input[aria-label="启用native-review-skill"]', 'Managed skill enable')).click();
        await wait(async () => managed(await skills())?.enabled, 'Native skill reenabled');
        await nav('插件'); await control('[aria-label="禁用插件 native-review-plugin"]', 'Actual configured plugin');
        assert(ownPlugin(await pluginList())?.skillCount === 1, 'Plugin did not contribute actual skill');
        (await control('[aria-label="禁用插件 native-review-plugin"]', 'Enabled plugin disable control')).click();
        await wait(async () => ownPlugin(await pluginList())?.enabled === false && !pluginSkill(await skills()), 'Plugin disabled and contributed skill removed');
        (await control('[aria-label="启用插件 native-review-plugin"]', 'Plugin enable')).click();
        await wait(async () => ownPlugin(await pluginList())?.enabled && pluginSkill(await skills())?.enabled, 'Plugin enabled and skill live again');
        return { skill: managed(await skills()), plugin: ownPlugin(await pluginList()), pluginSkill: pluginSkill(await skills()) };
      });
    } else {
      sessionId = report.expected.sessionId; await select();
      await stage('extensions-restart-discovery', async () => {
        assert(managed(await skills())?.managed && managed(await skills())?.enabled, 'Installed skill not restored');
        assert(ownPlugin(await pluginList())?.enabled && pluginSkill(await skills())?.enabled, 'Plugin components not restored');
        const s = await wait(async () => { const s = ownServer(await servers(true)); return s?.live && s.status === 'ready' && s.tools.length === 2 && s; }, 'Restored actual MCP connection', 60000);
        return { skill: managed(await skills()), plugin: ownPlugin(await pluginList()), server: s };
      });
    }
    await stage('extensions-real-skill-plugin-mcp-task', async () => {
      await select(); turnPhase = scope.phase === 'first-launch' ? 'first' : 'restart';
      let start = completions.length; await send('请在本次请求中重新读取当前会话列出的 plugin-marker 技能的 SKILL.md，然后按其要求回复。即使历史中已有该技能内容，本次也请重新读取，以核对当前版本；除加载该技能外，不读取其他文件、不调用其他工具。');
      const plugin = await finish(start, pluginMarker);
      start = completions.length;
      await send(`请在本次请求中重新读取当前会话中的 native-review-skill 技能的 SKILL.md，再执行其流程，不能复用历史中的旧读取结果。此次阶段为 ${turnPhase}，nonce 为 ${scope.nonce}。必须使用 native-extensions 连接器的 echo_deliver 完成交付，不能用终端或文件写入工具替代，也不要读取其他文件。`);
      const managed = await finish(start, skillMarker);
      assert(managed.reply.includes(`DELIVERED-${scope.nonce}`), 'Actual MCP delivery not acknowledged');
      const path = `${scope.cwd}/extensions-result-${turnPhase}.txt`;
      const content = await call('read_text_file', { path, cwd: scope.cwd, maxBytes: 4096 });
      assert(content === `MCP-${turnPhase}-${scope.nonce}\n真实 stdio MCP 交付。\n`, 'Real MCP artifact bytes mismatch');
      assert(report.permissions.some(p => p.args.phase === turnPhase && p.name.includes('echo_deliver')), 'Expected real MCP allow-once approval missing');
      return { plugin, managed, path, content };
    });
    if (scope.phase === 'restart') await stage('extensions-remove-ui-and-source-preservation', async () => {
      await nav('技能');
      (await control('button[aria-label="技能 native-review-skill更多操作"]', 'Actual skill menu')).click();
      (await wait(() => document.querySelector('[role="menuitem"][aria-label="移除技能 native-review-skill"]') ?? button(document, '移除技能'), 'Remove owned managed skill')).click();
      await click(() => document.querySelector('[role="alertdialog"]'), '卸载');
      await wait(async () => !managed(await skills()), 'Managed skill physically uninstalled and runtime removed');
      await nav('插件');
      (await control('button[aria-label="插件 native-review-plugin更多操作"]', 'Actual plugin menu')).click();
      await click(() => document, '移除插件'); await click(() => document.querySelector('[role="alertdialog"]'), '移除配置');
      await wait(async () => !ownPlugin(await pluginList()) && !pluginSkill(await skills()), 'Configured plugin and contributed skill removed');
      await nav('连接器');
      (await control('button[aria-label="连接器 native-extensions更多操作"]', 'Actual connector menu', toolRow)).click();
      await click(() => document, '删除用户配置'); await click(() => document.querySelector('[role="alertdialog"]'), '删除服务');
      await wait(async () => !ownServer(await servers()), 'Actual MCP config and resident server removed');
      return { managedSkillAbsent: true, pluginAbsent: true, mcpAbsent: true };
    });
    report.passed = report.checks.every(c => c.result === 'passed');
  } catch (error) {
    report.errors.push(String(error?.message || error).slice(0, 6000));
    report.diagnostic = { step: report.activeStep, visibleText: document.body.innerText.slice(-8000) };
  }
  for (const { event, id } of listeners) { window.__TAURI_EVENT_PLUGIN_INTERNALS__?.unregisterListener(event, id); await invoke('plugin:event|unlisten', { event, eventId: id }); }
  report.finishedAt = new Date().toISOString(); await invoke('desktop_review_complete', { report });
};
