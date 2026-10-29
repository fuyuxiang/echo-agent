// Fixed isolated native acceptance factory: production Composer, Runtime,
// model tools, verification and persistence. Never inserts an Agent response,
// plan, store state, verification record, source file or production host ACK.
((scope) => {
  if (window.__echoNativeCodeAgentStarted) return;
  window.__echoNativeCodeAgentStarted = true;
  const invoke = window.__TAURI_INTERNALS__?.invoke;
  const filename = '原生模型代码验收.cjs';
  const modelId = 'echoagent-ojlab/Echo Reed';
  const markerKey = 'echoagent.native-code-agent-review.v1';
  const home = scope?.home;
  const cwd = `${home}/workspaces/default`;
  const report = { version: 1, environment: 'actual native Code Composer + Qwen + Runtime tools + native verification',
    phase: null, passed: false, checks: [], errors: [], permissions: [], steps: [], activeStep: 'guard', expected: null };
  const deadline = Date.now() + 420000;
  const assert = (ok, message) => { if (!ok) throw new Error(message); };
  const delay = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
  const record = (id, evidence) => report.checks.push({ id, result: 'passed', evidence });
  const canonical = (value) => {
    if (typeof value !== 'string' || /[\x00-\x1f]/.test(value)) return null;
    const path = value.replace(/^\/tmp\//, '/private/tmp/').replace(/^\/var\//, '/private/var/').replace(/\/+$/, '');
    return path.startsWith('/') && !path.split('/').some((part) => part === '.' || part === '..') ? path : null;
  };
  const safeSource = (text) => {
    if (typeof text !== 'string' || text.length > 4096) return false;
    // The only executable tokens permitted are this one literal assignment.
    // JS line comments end at Unicode line separators too; block comments may
    // contain Chinese documentation but cannot hide additional executable code.
    const code = text.replace(/\/\*[\s\S]*?\*\/|\/\/[^\r\n\u2028\u2029]*/g, ' ');
    return /^\s*exports\s*\.\s*验收值\s*=\s*42\s*;\s*$/.test(code);
  };
  const bounded = async (work, label, timeout = 15000) => {
    report.activeStep = label;
    const remaining = Math.min(timeout, deadline - Date.now());
    assert(remaining > 0, 'Native Code Agent review exceeded its 420s deadline');
    let timer;
    try { return await Promise.race([work(), new Promise((_resolve, reject) => {
      timer = setTimeout(() => reject(new Error(`${label} timed out after ${remaining}ms`)), remaining);
    })]); } finally { clearTimeout(timer); }
  };
  const call = (command, args = {}, timeout) => bounded(() => invoke(command, args), `Native IPC: ${command}`, timeout);
  const visible = (element) => !!element && !element.closest('[hidden]') && element.getClientRects().length > 0;
  const button = (root, text) => [...(root?.querySelectorAll('button') ?? [])]
    .find((element) => visible(element) && !element.disabled && element.textContent.trim() === text);
  const waitFor = async (check, label, timeout = 25000) => {
    report.steps.push({ step: label, at: new Date().toISOString() });
    const end = Math.min(deadline, Date.now() + timeout);
    while (Date.now() < end) {
      const value = await bounded(check, label, Math.min(15000, end - Date.now()));
      report.activeStep = label;
      if (value) return value;
      await delay(150);
    }
    throw new Error(`Timed out waiting for actual Code Agent step: ${label}`);
  };
  const inputValue = (element, text) => {
    Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value').set.call(element, text);
    element.dispatchEvent(new Event('input', { bubbles: true }));
  };
  const ensureSidebar = async () => {
    if (document.querySelector('.app__body--collapsed')) {
      (await waitFor(() => {
        const element = document.querySelector('button[aria-label="展开侧边栏"]');
        return visible(element) ? element : null;
      }, 'UI: expand visible sidebar')).click();
      await waitFor(() => !document.querySelector('.app__body--collapsed'), 'UI: sidebar expanded');
    }
  };
  const nav = async (label) => {
    await ensureSidebar();
    (await waitFor(() => button(document.querySelector('.sidebar__nav'), label), `UI: visible nav ${label}`)).click();
  };
  const chooseModel = async (selector) => {
    (await waitFor(() => {
      const element = document.querySelector(`${selector} .model-selector__trigger`);
      return visible(element) && !element.disabled ? element : null;
    }, 'UI: ready model trigger')).click();
    const option = await waitFor(() => [...document.querySelectorAll('.model-selector__item')]
      .find((element) => visible(element) && element.dataset.modelId === modelId), 'UI: exact Qwen model option');
    const label = option.querySelector('.model-selector__item-label')?.textContent;
    assert(label, 'Selected Qwen model has no label');
    option.click();
    await waitFor(() => {
      const element = document.querySelector(`${selector} .model-selector__trigger`);
      return visible(element) && !element.disabled && element.textContent.includes(label);
    }, 'UI: actual model switch complete');
    return label;
  };
  const selectedSession = () => document.querySelector('[data-session-id] .sidebar__conv-select[aria-current="page"]')
    ?.closest('[data-session-id]')?.dataset.sessionId;
  const permissionOperation = (request, sessionId) => {
    if (request.sessionId !== sessionId) return null;
    let raw = request.rawInput;
    if (typeof raw === 'string') { try { raw = JSON.parse(raw); } catch { return null; } }
    if (!raw || typeof raw !== 'object' || Array.isArray(raw)) return null;
    const wrapped = typeof raw.tool_name === 'string';
    const input = wrapped ? raw.tool_input : raw;
    if (!input || typeof input !== 'object' || Array.isArray(input)) return null;
    let name = wrapped ? raw.tool_name.split(/:|__/).pop().toLowerCase() : null;
    if (!name) {
      const variants = { ReadFile: 'read_file', Write: 'write', Bash: 'bash', TodoWrite: 'todo_write' };
      name = variants[input.variant];
    }
    if (name === 'todo_write') {
      if (!Array.isArray(input.todos) || input.todos.length > 8 || !input.todos.every((item) => item && typeof item.id === 'string'
        && (item.content == null || typeof item.content === 'string')
        && (item.status == null || ['pending', 'in_progress', 'completed', 'cancelled'].includes(item.status)))) return null;
      return { name, owner: 'only this real session plan metadata' };
    }
    if (name === 'bash') {
      const command = typeof input.command === 'string' ? input.command.trim() : null;
      const actualCwd = input.cwd == null ? cwd : canonical(input.cwd);
      return ['node --test', 'npm test', 'npm run test'].includes(command) && actualCwd === cwd
        && ![true, 'true', 1, '1'].includes(input.is_background)
        ? { name, command, cwd } : null;
    }
    if (!['read_file', 'write'].includes(name)) return null;
    const inputPath = input.file_path ?? input.target_file ?? input.path;
    const path = canonical(typeof inputPath === 'string' && !inputPath.startsWith('/') ? `${cwd}/${inputPath}` : inputPath);
    const knownSource = path === `${cwd}/${filename}`;
    const fixture = [`${cwd}/package.json`, `${cwd}/test.cjs`].includes(path);
    const ownPlan = path?.startsWith(`${home}/sessions/`) && path.endsWith(`/${sessionId}/plan.md`);
    if (name === 'read_file' && (knownSource || fixture || ownPlan)) return { name, path };
    if (name === 'write' && knownSource && safeSource(input.content)) return { name, path, content: input.content };
    return null;
  };
  const handled = new Set();
  const servicePending = async (sessionId) => {
    const interactions = await call('agent_list_pending_interactions', { sessionId });
    assert(!interactions.questions?.length && !interactions.folderTrustRequests?.length,
      'Unexpected question or folder trust; no automatic answer or trust');
    if (interactions.planApprovals?.length) {
      for (const approval of interactions.planApprovals) await call('agent_resolve_plan_approval', {
        requestId: approval.requestId, outcome: 'cancelled', feedback: '本次固定验收只授权已明确的单文件任务，不批准额外范围。',
      });
      throw new Error('Unexpected interactive plan approval for the fixed one-file task; cancelled');
    }
    for (const request of interactions.permissions ?? []) {
      if (handled.has(request.requestId)) continue;
      const operation = permissionOperation(request, sessionId);
      if (!operation) {
        const acknowledged = await call('agent_resolve_permission', { requestId: request.requestId, optionId: null, cancelled: true });
        report.permissions.push({ requestId: request.requestId, title: request.title, result: 'denied outside fixed scope', acknowledged });
        throw new Error(`Unexpected tool or scope denied: ${request.title}`);
      }
      const card = document.getElementById(`permission-${request.requestId}`);
      const allow = button(card, '允许一次');
      if (!visible(card) || !allow) continue;
      assert(request.options.some((option) => option.kind === 'allow'), 'Operation has no allow-once option');
      allow.click();
      handled.add(request.requestId);
      report.permissions.push({ requestId: request.requestId, toolCallId: request.toolCallId,
        operation, result: 'allow-once via production permission UI' });
    }
    const alert = document.querySelector('.perm-inline [role="alert"], .coding-agent__warning[role="alert"]');
    if (visible(alert)) throw new Error(`Actual Code Agent error: ${alert.textContent}`);
  };
  const selectSession = async (sessionId) => {
    await ensureSidebar();
    const section = document.querySelector('.sidebar__list-section--tasks > .sidebar__section-label');
    if (visible(section) && section.getAttribute('aria-expanded') !== 'true') section.click();
    (await waitFor(() => {
      const element = document.querySelector(`[data-session-id="${CSS.escape(sessionId)}"] .sidebar__conv-select`);
      return visible(element) ? element : null;
    }, 'UI: retained real model session row')).click();
    await waitFor(() => selectedSession() === sessionId, 'UI: retained model session selected');
    await waitFor(async () => {
      try { const info = await call('agent_session_info', { sessionId }); return canonical(info.cwd) === cwd ? info : null; }
      catch (error) { if (String(error).includes('尚未建立可信工作区绑定')) return null; throw error; }
    }, 'Native: restored Runtime workspace binding', 60000);
  };
  const openCode = async () => {
    await nav('代码开发');
    await waitFor(() => visible(document.querySelector('.echo-theia__frame')), 'UI: actual Theia iframe', 70000);
    await waitFor(() => !document.querySelector('.echo-theia__cover'), 'UI: actual Theia ready', 70000);
  };
  const deliveryUi = async () => {
    (await waitFor(() => document.querySelector('button[aria-label="打开交付报告"]'), 'UI: actual delivery entry')).click();
    return waitFor(() => {
      const view = document.querySelector('[aria-label="交付报告"]');
      return visible(view) ? view : null;
    }, 'UI: actual delivery report');
  };
  const run = async () => {
    try {
      assert(invoke && typeof home === 'string' && canonical(home) === home && home.includes('/echo-native-review-')
        && typeof scope.nonce === 'string' && scope.nonce.length >= 24, 'Guarded private native Agent review scope required');
      const context = await call('desktop_review_context');
      report.nativeContext = context;
      report.phase = context.phase;
      assert(context.home === home && context.codeAgentRequested === true && ['first-launch', 'restart'].includes(context.phase), 'Native scope/phase/Agent flag mismatch');
      report.workspace = cwd;
      const stat = await call('path_stat', { path: cwd, cwd });
      assert(stat.exists && stat.kind === 'directory' && canonical(stat.absolute) === cwd, 'Actual default workspace missing or noncanonical');
      const packageText = await call('read_text_file', { path: `${cwd}/package.json`, cwd, maxBytes: 4096 });
      const testText = await call('read_text_file', { path: `${cwd}/test.cjs`, cwd, maxBytes: 4096 });
      const expectedPackage = JSON.stringify({ name: 'echoagent-native-model-acceptance', version: '1.0.0', private: true,
        scripts: { test: 'node --test' } }, null, 2) + '\n';
      const expectedTest = "const test = require('node:test');\nconst assert = require('node:assert/strict');\ntest('原生模型代码验收：实际导出值为42', () => {\n  const generated = require('./原生模型代码验收.cjs');\n  assert.equal(generated.验收值, 42);\n});\n";
      assert(packageText === expectedPackage && testText === expectedTest, 'Seeded real failing test/manifest was changed');
      record('code-agent-isolation', { home, cwd, explicitNativeScope: true, fixtureOnly: ['package.json', 'test.cjs'], target: filename });
      const stored = localStorage.getItem(markerKey);
      const saved = stored ? JSON.parse(stored) : null;
      if (context.phase === 'restart') {
        assert(saved?.version === 1 && saved.home === home && saved.cwd === cwd && saved.modelId === modelId
          && saved.taskId && saved.sessionId && saved.contentRevision && safeSource(saved.content), 'No successful first-launch expectation to restore');
        report.expected = saved;
        report.sessionId = saved.sessionId;
        const task = await call('coding_task_get', { root: cwd, taskId: saved.taskId });
        const delivery = await call('coding_delivery_report', { root: cwd, taskId: saved.taskId });
        assert(task.phase === 'delivered' && task.modelId === modelId && task.sessionId === saved.sessionId
          && task.taskNodes.length > 0 && task.taskNodes.every((node) => node.status === 'success')
          && delivery.deliverable && delivery.verifications.some((item) => item.id === saved.verificationId
            && item.status === 'passed' && item.exitCode === 0 && item.contentRevision === saved.contentRevision), 'Real delivered task/verification binding did not restore');
        assert(await call('read_text_file', { path: `${cwd}/${filename}`, cwd, maxBytes: 4096 }) === saved.content,
          'Actual model-generated file changed after restart');
        record('code-agent-restart-state', { taskId: task.id, sessionId: saved.sessionId, modelId,
          contentRevision: saved.contentRevision, verificationId: saved.verificationId, delivered: true });
        await selectSession(saved.sessionId);
        await openCode();
        (await waitFor(() => document.querySelector('button[aria-label="切换开发任务"]'), 'UI: real task picker')).click();
        const retainedTask = await waitFor(() => {
          const candidates = [...document.querySelectorAll('.coding-task-switcher__select')]
            .filter((element) => visible(element) && element.textContent.includes(saved.taskName));
          assert(candidates.length <= 1, 'Ambiguous retained Code task name');
          return candidates[0];
        }, 'UI: exact retained Code task option');
        retainedTask.click();
        await waitFor(() => document.querySelector('button[aria-label="切换开发任务"]')?.textContent.includes(saved.taskName), 'UI: retained Code task selected');
        const view = await deliveryUi();
        await waitFor(() => view.textContent.includes(saved.command) && view.textContent.includes('通过'), 'UI: restored actual verification result');
        record('code-agent-restart-ui', { sessionId: saved.sessionId, taskId: task.id, command: saved.command, reportVisible: true, noNewTaskRequested: true });
      } else {
        assert(!saved, 'Refusing to reset an existing Agent review marker');
        const existing = await call('agent_list_all_sessions', { includeArchived: true });
        assert(existing.length === 0 && (await call('coding_task_list', { root: cwd })).length === 0,
          'Agent review requires a new isolated profile without sessions or tasks');
        assert(!(await call('path_stat', { path: `${cwd}/${filename}`, cwd })).exists, 'Model target must not exist before execution');
        const auth = await waitFor(async () => {
          const state = await call('agent_auth_status');
          return state.ready && state.runtimeReady && state.synchronized && state.runtimeModels?.includes(modelId) ? state : null;
        }, 'Native: actual built-in Qwen readiness', 60000);
        const mode = await call('permission_mode_get', { sessionId: null });
        assert(mode.permissionMode === 'ask', 'New-task permission mode must be ask');
        await waitFor(() => visible(document.querySelector('.home .echo-composer')), 'UI: new Home Composer');
        await chooseModel('.home .echo-composer');
        const warmup = '原生代码模型会话准备：请只回复“代码会话已准备”，不要调用工具或读取写入文件。';
        const input = await waitFor(() => {
          const element = document.querySelector('.home .echo-composer textarea');
          return visible(element) && !element.disabled ? element : null;
        }, 'UI: ready Home input');
        inputValue(input, warmup);
        (await waitFor(() => {
          const element = document.querySelector('.home .echo-composer button[aria-label="发送"]');
          return visible(element) && !element.disabled ? element : null;
        }, 'UI: Home send ready')).click();
        const warmSession = await waitFor(async () => (await call('agent_list_all_sessions', { includeArchived: true }))
          .find((item) => item.sessionId === selectedSession()), 'Native: real Home-created session', 30000);
        assert(canonical(warmSession.cwd) === cwd && warmSession.permissionMode === 'ask', 'Warmup session scope or mode mismatch');
        await waitFor(async () => {
          const pending = await call('agent_list_pending_interactions', { sessionId: warmSession.sessionId });
          assert(!pending.permissions?.length && !pending.questions?.length && !pending.planApprovals?.length
            && !pending.folderTrustRequests?.length, 'Warmup unexpectedly requested tools or approval');
          const item = (await call('agent_list_all_sessions', { includeArchived: true })).find((item) => item.sessionId === warmSession.sessionId);
          assert(item && !['failed', 'stopped'].includes(item.status), 'Actual Qwen warmup failed');
          return item.status === 'completed' ? item : null;
        }, 'Native: real Qwen warmup complete', 60000);
        record('code-agent-model-session', { warmupSessionId: warmSession.sessionId, modelId, runtimeReady: auth.runtimeReady,
          permissionMode: 'ask', selectedViaProductionUi: true });
        await openCode();
        const composerSelector = '.echo-theia-agent__composer[aria-label="新建开发任务"]';
        await waitFor(() => visible(document.querySelector(composerSelector)), 'UI: real new Code composer');
        const label = await chooseModel(composerSelector);
        const requirement = '原生模型代码交付验收：这是只新增1个文件、无依赖和公共接口变化的明确小任务。请先真实读取 package.json 和 test.cjs，并运行 npm test 确认现有测试因缺少目标文件而失败；随后仅通过真实 Write 工具创建“原生模型代码验收.cjs”，唯一可执行语句必须为 exports.验收值 = 42;，可有中文注释。不要修改 package.json 或 test.cjs，不安装依赖，不读写其他文件，不联网，不调用连接器，不访问联系人，不运行 Git。验证命令只允许 node --test 或 npm test（工作台可使用等价 npm run test）。完成后真实运行测试，核对文件和输出，再结束本轮交给工作台记录验证与交付；不要只回复示例，不需要额外交互审批计划或子Agent。';
        const codeInput = await waitFor(() => {
          const element = document.querySelector(`${composerSelector} textarea[aria-label="任务描述"]`);
          return visible(element) && !element.disabled ? element : null;
        }, 'UI: actual Code requirement input');
        inputValue(codeInput, requirement);
        (await waitFor(() => {
          const element = document.querySelector(`${composerSelector} button[aria-label="开始 Agent 任务"]`);
          return visible(element) && !element.disabled ? element : null;
        }, 'UI: actual Code start button')).click();
        const task = await waitFor(async () => {
          const tasks = await call('coding_task_list', { root: cwd });
          assert(tasks.length <= 1, 'Duplicate Code task created by one UI start');
          if (!tasks.length) return null;
          const task = await call('coding_task_get', { root: cwd, taskId: tasks[0].id });
          assert(!['blocked', 'stopped'].includes(task.phase), `Actual Code start failed: ${task.blocker}`);
          return task.sessionId && task.modelId ? task : null;
        }, 'Native: production startTask bound Runtime session', 60000);
        assert(task.requirement === requirement && task.modelId === modelId && task.sessionId !== warmSession.sessionId,
          'Code session/model/requirement did not originate from the actual UI start');
        report.sessionId = task.sessionId;
        report.expected = { version: 1, home, cwd, modelId, taskId: task.id, taskName: task.name, sessionId: task.sessionId, warmupSessionId: warmSession.sessionId };
        record('code-agent-ui-runtime-binding', { taskId: task.id, sessionId: task.sessionId, modelId, label,
          requirementTyped: true, productionStartClicked: true, createdByNative: true });
        const args = { root: cwd, taskId: task.id };
        const delivered = await waitFor(async () => {
          await servicePending(task.sessionId);
          const current = await call('coding_task_get', args);
          assert(current.sessionId === task.sessionId && current.modelId === modelId, 'Runtime task binding changed');
          assert(!['blocked', 'stopped', 'paused'].includes(current.phase), `Actual Code task ended ${current.phase}: ${current.blocker ?? ''}`);
          const consent = [...document.querySelectorAll('[role="dialog"], [role="alertdialog"]')]
            .find((element) => visible(element) && element.textContent.includes('确认运行计划中的验证命令'));
          if (consent) {
            const commands = [...consent.querySelectorAll('li code')].map((element) => element.textContent.trim());
            assert(commands.length > 0 && commands.every((command) => ['npm test', 'npm run test', 'node --test'].includes(command)),
              'Unexpected verification command requires consent');
            (await waitFor(() => button(consent, '确认并运行'), 'UI: scoped actual verification consent')).click();
          }
          return current.phase === 'delivered' ? current : null;
        }, 'Native: actual model tools and automatic verification delivery', 240000);
        assert(delivered.taskNodes.length > 0 && delivered.taskNodes.every((node) => node.status === 'success') && !delivered.blocker,
          'Automatic model node completion is missing');
        const content = await call('read_text_file', { path: `${cwd}/${filename}`, cwd, maxBytes: 4096 });
        assert(safeSource(content), 'Actual model file is missing, unsafe or has wrong exports');
        const writeApproval = report.permissions.find((item) => item.operation?.name === 'write' && item.operation.path === `${cwd}/${filename}`);
        assert(writeApproval, 'No real scoped model write approval was observed');
        record('code-agent-real-model-file', { path: `${cwd}/${filename}`, content, actualWriteToolCallId: writeApproval.toolCallId,
          permissions: report.permissions.length, replyAloneNotAccepted: true });
        record('code-agent-automatic-node-delivery', { taskId: task.id, phase: delivered.phase,
          nodes: delivered.taskNodes.map((node) => ({ id: node.id, planKey: node.planKey, status: node.status })) });
        const delivery = await call('coding_delivery_report', args);
        const verification = delivery.verifications.filter((item) => ['npm run test', 'npm test', 'node --test'].includes(item.command)
          && item.status === 'passed' && item.exitCode === 0 && item.contentRevision).at(-1);
        assert(verification && verification.stdout.includes('原生模型代码验收') && /(?:# pass 1|tests 1|pass 1)/.test(verification.stdout),
          'Actual successful native Node verification/content revision is missing');
        record('code-agent-native-verification', { verification });
        const nativeTdd = await call('coding_tdd_status', args);
        report.tdd = nativeTdd;
        assert(nativeTdd.redRecordId && nativeTdd.redCommand === 'npm test'
          && nativeTdd.redRevision !== verification.contentRevision && !nativeTdd.waiverReason
          && nativeTdd.redRecord?.status === 'failed' && nativeTdd.redRecord.exitCode !== 0
          && nativeTdd.runtimeProvenance?.sessionId === task.sessionId
          && nativeTdd.runtimeProvenance?.workspace === cwd, 'Trusted Runtime RED was not linked to this task before implementation');
        record('code-agent-native-red-evidence', { nativeTdd, existingTestUnchanged: true, noRendererCreatedRecord: true });
        report.expected.content = content;
        report.expected.contentRevision = verification.contentRevision;
        report.expected.verificationId = verification.id;
        report.expected.command = verification.command;
        report.expected.redRecordId = nativeTdd.redRecordId;
        (await waitFor(() => button(document, '任务变更 · 1'), 'UI: actual model changeset entry')).click();
        (await waitFor(() => document.querySelector(`.coding-changeset__row button[title="${filename}"]`), 'UI: actual model file diff entry')).click();
        const diffSelector = `[aria-label="任务差异 ${filename}"]`;
        await waitFor(() => document.querySelector(`${diffSelector} .monaco-diff-editor`), 'UI: actual Monaco model diff');
        (await waitFor(() => button(document.querySelector(diffSelector), '标记已审阅'), 'UI: mounted current diff review')).click();
        await waitFor(async () => (await call('coding_changeset_get', args)).reviewedFiles.includes(filename), 'Native: actual reviewed hash');
        record('code-agent-actual-diff-review', { path: filename, actualMonacoDiffMounted: true });
        document.querySelector('button[aria-label="关闭差异"]').click();
        const view = await deliveryUi();
        await waitFor(() => [...view.querySelectorAll('tr')].some((row) => row.textContent.includes(verification.command)
          && row.textContent.includes('通过')), 'UI: actual native verification visible');
        while ((await call('coding_task_get', args)).acceptanceCriteria.some((criterion) => !criterion.satisfied)) {
          (await waitFor(() => button(view, '人工确认已满足'), 'UI: manual acceptance entry')).click();
          (await waitFor(() => button(view, '确认已满足'), 'UI: actual acceptance confirmation')).click();
          await delay(250);
        }
        for (const title of ['需求符合性', '代码质量']) {
          const row = await waitFor(() => [...view.querySelectorAll('.coding-report__review-action')]
            .find((element) => element.querySelector('strong')?.textContent === title), `UI: ${title} actual review gate`);
          (await waitFor(() => button(row, '审查并确认'), `UI: ${title} review entry`)).click();
          (await waitFor(() => button(row, '确认已审查'), `UI: ${title} confirm review`)).click();
          await waitFor(() => !row.isConnected, `UI: ${title} satisfied gate`);
        }
        const final = await call('coding_delivery_report', args);
        report.delivery = final;
        assert(final.deliverable && final.blockers.length === 0, 'Actual model delivery gates are incomplete');
        record('code-agent-delivery-gates', { deliverable: final.deliverable, blockers: final.blockers,
          ledger: await call('coding_task_execution_ledger', args) });
        localStorage.setItem(markerKey, JSON.stringify(report.expected));
      }
      report.passed = report.checks.length > 0 && report.errors.length === 0;
    } catch (error) {
      report.errors.push(String(error));
      report.passed = false;
      report.failureContext = { activeStep: report.activeStep, sessionId: report.sessionId,
        sidebarCollapsed: !!document.querySelector('.app__body--collapsed'),
        visibleDialogs: [...document.querySelectorAll('[role="dialog"], [role="alertdialog"]')]
          .filter(visible).map((element) => element.textContent.slice(0, 1200)),
        codeWarning: document.querySelector('.coding-agent__warning[role="alert"]')?.textContent,
        phaseLabel: document.querySelector('.echo-theia-agent__evidence .coding-agent__phase')?.textContent };
      if (report.sessionId) {
        try { await call('agent_cancel', { sessionId: report.sessionId }); } catch (cleanup) { report.errors.push(`Owned task cancel: ${String(cleanup)}`); }
      }
    }
    report.finishedAt = new Date().toISOString();
    await invoke('desktop_review_complete', { report });
  };
  void run();
})
