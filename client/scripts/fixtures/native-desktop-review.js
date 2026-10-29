// Injected only by the isolated native review launch. Uses production DOM and
// native IPC; no fixtures, replaced providers, or simulated model responses.
void (async () => {
  if (window.__echoNativeReviewStarted) return;
  window.__echoNativeReviewStarted = true;
  const invoke = window.__TAURI_INTERNALS__.invoke;
  const delay = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
  const report = { environment: "native WebView + production IPC", phase: "first-launch", checks: [], errors: [] };
  const reviewKey = "echoagent.native-review.v1";
  const requestText = "原生验收：请只回复中文“验收成功”。不要读取文件或调用工具。";
  const probeText = "模型格式验收：请只回复中文“验收成功”。不要读取文件或调用工具。";
  const restartText = "重启后继续任务验收：请只回复中文“验收成功”。不要读取文件或调用工具。";
  const cancelText = "原生停止验收：请直接撰写一篇五千字的中文桌面软件使用说明，逐段展开，不调用工具。";
  const cancelRecoveryText = "停止后恢复验收：请只回复中文“验收成功”。不要读取文件或调用工具。";
  const agentOfficeTitle = '原生Agent办公验收';
  const agentOfficeText = '请调用 office_create 办公工具，生成Word（docx），标题必须是“原生Agent办公验收”，正文必须包含原句“今天确认任务状态，明天完成复核。”。请只用办公工具，不要使用终端、浏览器或其他工具。工具成功后回复“验收成功”和文件保存路径。并在回复末尾原样输出公式 $x^{2}+1$，以及以下Mermaid代码块：\n```mermaid\nflowchart LR\n A[任务] --> B[完成]\n```';
  const targetModelId = "echoagent-ojlab/Echo Reed";
  const expectedTitle = "原生验收保留会话";
  const artifactName = "中文成果/原生验收 文件.txt";
  const artifactContent = "EchoAgent 原生文件验收\n中文、空格路径与重启读回。\n";
  const projectName = "原生验收项目";
  const assistantReplyText = () => {
    const lastUser = [...document.querySelectorAll('.msg--user')]
      .filter((item) => item.getClientRects().length > 0).at(-1);
    return [...document.querySelectorAll('.msg--assistant .msg__body > .markdown-body[data-md-theme="loose"]')]
      .filter((item) => item.getClientRects().length > 0
        && (!lastUser || (lastUser.compareDocumentPosition(item) & Node.DOCUMENT_POSITION_FOLLOWING)))
      .map((item) => item.innerText || "").join("\n");
  };
  const record = (id, result, evidence) => report.checks.push({ id, result, evidence });
  const verifyWorkspace = async (workspace, write) => {
    let path = `${workspace}/${artifactName}`;
    if (write) {
      path = await invoke("write_text_file", { path: artifactName, content: artifactContent, workspaceRoot: workspace });
      let rejected = false;
      let error;
      try { await invoke("write_text_file", { path: "../越界写入应拒绝.txt", content: "must not be written", workspaceRoot: workspace }); }
      catch (reason) { rejected = true; error = String(reason); }
      record("native-workspace-boundary", rejected ? "passed" : "failed", { rejected, error });
    }
    const content = await invoke("read_text_file", { path, cwd: workspace, maxBytes: 4096 });
    record("native-workspace-file", content === artifactContent ? "passed" : "failed", { path, content, restarted: !write });
  };
  const waitFor = async (check, timeout = 15000) => {
    const deadline = Date.now() + timeout;
    while (Date.now() < deadline) { const value = await check(); if (value) return value; await delay(150); }
    throw new Error("Timed out waiting for native UI condition");
  };
  const send = async (text) => {
    const input = await waitFor(() => document.querySelector(".echo-composer textarea"));
    Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value").set.call(input, text);
    input.dispatchEvent(new Event("input", { bubbles: true }));
    const button = await waitFor(() => { const b = document.querySelector('button[aria-label="发送"]'); return b && !b.disabled ? b : null; });
    button.click();
  };
  const chooseModel = async (modelId) => {
    const picker = await waitFor(() => {
      const button = document.querySelector('.echo-composer .model-selector__trigger');
      return button && !button.disabled ? button : null;
    });
    picker.click();
    const option = await waitFor(() => [...document.querySelectorAll('.model-selector__item')]
      .find((item) => item.dataset.modelId === modelId));
    const label = option.querySelector('.model-selector__item-label')?.textContent;
    option.click();
    await waitFor(() => {
      const button = document.querySelector('.echo-composer .model-selector__trigger');
      return button && !button.disabled && button.textContent?.includes(label);
    });
    return label;
  };
  try {
    const nativeContext = await invoke("desktop_review_context");
    report.phase = nativeContext.phase;
    report.nativeContext = nativeContext;
    const stored = localStorage.getItem(reviewKey);
    const reviewState = stored ? JSON.parse(stored) : null;
    if (reviewState && reviewState.version !== 1) throw new Error("Invalid native review restart marker");
    record("native-webview-state", (report.phase === "first-launch" ? !reviewState : !!reviewState?.session)
      ? "passed" : "failed", { fresh: !reviewState, retainedSessionId: reviewState?.session?.sessionId || null });
    // This marker records only acceptance expectations. It does not replace
    // session data, model configuration, IPC, or the application's stores.
    if (report.phase === "first-launch" && !reviewState) localStorage.setItem(reviewKey, JSON.stringify({ version: 1, session: null }));
    await waitFor(() => document.querySelector(".home__title"));
    record("native-shell", "passed", { heading: document.querySelector(".home__title").textContent, width: innerWidth, height: innerHeight });
    const settings = await waitFor(() => document.querySelector('button[aria-label="设置"]'));
    settings.click();
    await waitFor(() => document.querySelector('button[aria-label="关闭设置"]'));
    const dialog = !!document.querySelector('[role="dialog"]');
    record("settings-entry", dialog ? "passed" : "failed", { dialog });
    document.querySelector('button[aria-label="关闭设置"]').click();
    const auth = await invoke("agent_auth_status");
    record("runtime-model-ready", auth.ready ? "passed" : "blocked", { ready: auth.ready, runtimeReady: auth.runtimeReady, synchronized: auth.synchronized, runtimeModels: auth.runtimeModels, reason: auth.reason });
    const capabilities = await invoke("automation_capabilities");
    record("automation-capabilities", "passed", capabilities);
    const existing = await invoke("agent_list_all_sessions", { includeArchived: true });
    if (report.phase === "restart") {
      const expected = reviewState?.session;
      if (!expected) {
        record("restart-session-load", "blocked", { reason: "First launch did not create a native session" });
      } else {
        const session = existing.find((item) => item.sessionId === expected.sessionId);
        if (!session || session.title !== expectedTitle || session.cwd !== expected.cwd) throw new Error("Persisted native session does not match the first launch");
        const expectedProject = reviewState.project;
        if (!expectedProject) throw new Error("First launch did not persist the native project expectation");
        const project = (await invoke('projects_load')).find((item) => item.id === expectedProject.id);
        if (!project || project.name !== projectName || project.defaultModelId !== targetModelId
          || !project.conversations?.some((item) => item.sessionId === session.sessionId)) {
          throw new Error('Persisted project binding does not match the first launch');
        }
        await waitFor(() => [...document.querySelectorAll('.sidebar__node-name')].some((item) => item.textContent === projectName));
        const expand = document.querySelector(`button[aria-label="展开${projectName}对话"]`);
        if (expand) expand.click();
        record('native-project-restore', 'passed', { id: project.id, name: project.name,
          defaultModelId: project.defaultModelId, sessionId: session.sessionId, cwd: session.cwd, visibleInSidebar: true });
        const row = await waitFor(() => document.querySelector(`[data-session-id="${CSS.escape(session.sessionId)}"] .sidebar__conv-select`));
        row.click();
        const selected = await waitFor(() => document.querySelector(`[data-session-id="${CSS.escape(session.sessionId)}"] .sidebar__conv-select[aria-current="page"]`));
        await waitFor(() => [...document.querySelectorAll(".msg--user")].some((item) => item.textContent?.includes(requestText)));
        if (expected.modelResponsePassed) await waitFor(() => assistantReplyText().includes("验收成功"));
        record("restart-session-load", "passed", { sessionId: session.sessionId, title: session.title, cwd: session.cwd, selectedInUi: !!selected, userMessageVisible: true, assistantSuccessVisible: assistantReplyText().includes("验收成功") });
        // Replay appears before agent_load_session completes. Wait for the
        // production Composer readiness gate and execute a fresh turn so a
        // rendered history cannot masquerade as a usable restored session.
        await waitFor(() => {
          const input = document.querySelector('.echo-composer textarea');
          const picker = document.querySelector('.echo-composer .model-selector__trigger');
          return input && !input.disabled && picker && !picker.disabled
            && picker.textContent?.includes('Echo Reed');
        }, 60000);
        await send(restartText);
        await waitFor(() => [...document.querySelectorAll('.msg--user')].some((item) => item.textContent?.includes(restartText)));
        await waitFor(() => assistantReplyText().includes('验收成功'), 90000);
        const resumed = await waitFor(async () => {
          const item = (await invoke('agent_list_all_sessions', { includeArchived: true })).find((entry) => entry.sessionId === session.sessionId);
          return item && ['completed', 'failed', 'stopped'].includes(item.status) ? item : null;
        });
        record('native-restart-task', resumed.status === 'completed' && resumed.cwd === session.cwd ? 'passed' : 'failed', {
          sessionId: resumed.sessionId, cwd: resumed.cwd, status: resumed.status,
          modelLabel: document.querySelector('.echo-composer .model-selector__trigger')?.textContent,
          newUserMessageVisible: true, answer: assistantReplyText().slice(0, 1000),
        });
        await verifyWorkspace(session.cwd, false);
        if (nativeContext.mediaRequested) {
          const expectation = reviewState.meeting;
          if (!expectation) throw new Error('First launch did not persist the meeting expectation');
          const meeting = await invoke('meeting_get', { meetingId: expectation.id });
          const matches = meeting.audioPath === expectation.audioPath && meeting.minutes === expectation.minutes
            && meeting.transcriptText?.includes('复核') && meeting.durationSeconds > 5 && meeting.durationSeconds < 8;
          record('native-meeting-restore', matches ? 'passed' : 'failed', {
            id: meeting.id, status: meeting.status, audioPath: meeting.audioPath,
            durationSeconds: meeting.durationSeconds, minutes: meeting.minutes,
            transcriptText: meeting.transcriptText,
          });
          const office = JSON.parse(await invoke('read_text_file', {
            path: `${session.cwd}/native-office-done.json`, cwd: session.cwd, maxBytes: 131072,
          }));
          if (!office.passed || !reviewState.office
            || JSON.stringify(office.receipts) !== JSON.stringify(reviewState.office.receipts)) {
            throw new Error('Persisted native Office receipts differ from the first launch');
          }
          report.office = office;
          record('native-office-restore', 'passed', { receipts: office.receipts, restarted: true });
        }
      }
    } else {
      record("empty-session-catalog", existing.length === 0 ? "passed" : "failed", { count: existing.length });
      if (existing.length) throw new Error("First isolated launch already contains sessions");
      if (!auth.ready) {
        for (const id of ["composer-native-send", "real-model-response", "native-rename-persistence"]) record(id, "blocked", { reason: "Configured model is not ready" });
      } else {
        // Exercise the current default first. A malformed provider response
        // must end in an actionable error, rather than an endless tool loop.
        const initialPicker = await waitFor(() => {
          const picker = document.querySelector('.echo-composer .model-selector__trigger');
          return picker && !picker.disabled ? picker : null;
        });
        const initialModel = initialPicker.textContent;
        await send(probeText);
        const probeSession = await waitFor(async () => (await invoke("agent_list_all_sessions", { includeArchived: true }))[0]);
        await waitFor(() => [...document.querySelectorAll('.msg--user')].some((item) => item.textContent?.includes(probeText)));
        const probeResult = await waitFor(async () => {
          const current = (await invoke("agent_list_all_sessions", { includeArchived: true })).find((item) => item.sessionId === probeSession.sessionId);
          return current && ["completed", "failed"].includes(current.status) ? current : null;
        }, 45000);
        if (probeResult.status === "completed") await waitFor(() => assistantReplyText().includes("验收成功"));
        const probeError = document.querySelector('.chatview__error-text')?.textContent || '';
        const probePassed = probeResult.status === "completed"
          ? assistantReplyText().includes("验收成功")
          : /模型服务|工具名称|工具调用/.test(probeError) && /重试|切换模型/.test(probeError);
        record("native-initial-model-probe", probePassed ? "passed" : "failed", {
          initialModel, status: probeResult.status, error: probeError,
          answer: assistantReplyText().slice(0, 1000),
        });
        const label = await chooseModel(targetModelId);
        report.selectedModelId = targetModelId;
        record("native-model-selection", "passed", { modelId: targetModelId, label, previousModel: initialModel });
        await send(requestText);
        const session = await waitFor(async () => (await invoke("agent_list_all_sessions", { includeArchived: true }))[0]);
        await waitFor(() => [...document.querySelectorAll(".msg--user")].some((item) => item.textContent?.includes(requestText)));
        report.sessionId = session.sessionId;
        report.workspace = session.cwd;
        const expected = { sessionId: session.sessionId, cwd: session.cwd, modelResponsePassed: false };
        localStorage.setItem(reviewKey, JSON.stringify({ version: 1, session: expected }));
        record("composer-native-send", "passed", { sessionId: session.sessionId, cwd: session.cwd, userMessageVisible: true });
        await verifyWorkspace(session.cwd, true);
        let completed;
        try {
          completed = await waitFor(async () => {
            const current = (await invoke("agent_list_all_sessions", { includeArchived: true })).find((item) => item.sessionId === session.sessionId);
            return current && ["completed", "failed", "stopped", "paused"].includes(current.status) ? current : null;
          }, 90000);
        } catch (error) {
          const stop = document.querySelector('button[aria-label="停止生成"]');
          if (stop) { stop.click(); await delay(1500); record("timeout-stop-entry", "passed", { clicked: true }); }
          record("model-completion-timeout", "blocked", { error: String(error), stopClicked: !!stop });
        }
        if (completed?.status === "completed") {
          try { await waitFor(() => assistantReplyText().includes("验收成功"), 5000); } catch {}
        }
        const answer = assistantReplyText().slice(0, 1000);
        const modelResponsePassed = completed?.status === "completed" && answer.includes("验收成功");
        record("real-model-response", modelResponsePassed ? "passed" : "blocked", { status: completed?.status || "timeout", answer, error: document.querySelector(".chatview__error-text")?.textContent?.slice(0, 1000) });
        localStorage.setItem(reviewKey, JSON.stringify({ version: 1, session: { ...expected, modelResponsePassed } }));
        await send(cancelText);
        await waitFor(() => [...document.querySelectorAll('.msg--user')].some((item) => item.textContent?.includes(cancelText)));
        const stopButton = await waitFor(() => {
          const button = document.querySelector('button[aria-label="停止生成"]');
          return button && !button.disabled ? button : null;
        });
        stopButton.click();
        const stopped = await waitFor(async () => (await invoke('agent_list_all_sessions', { includeArchived: true }))
          .find((item) => item.sessionId === session.sessionId && item.status === 'stopped'), 30000);
        await delay(1000);
        const stillStopped = (await invoke('agent_list_all_sessions', { includeArchived: true }))
          .find((item) => item.sessionId === session.sessionId);
        record('native-stop-task', stillStopped?.status === 'stopped' ? 'passed' : 'failed', {
          sessionId: stopped.sessionId, status: stillStopped?.status, clicked: true, lateEventWaitMs: 1000,
        });
        await send(cancelRecoveryText);
        await waitFor(() => [...document.querySelectorAll('.msg--user')].some((item) => item.textContent?.includes(cancelRecoveryText)));
        await waitFor(() => assistantReplyText().includes('验收成功'), 90000);
        const recovered = await waitFor(async () => (await invoke('agent_list_all_sessions', { includeArchived: true }))
          .find((item) => item.sessionId === session.sessionId && ['completed', 'failed', 'stopped'].includes(item.status)));
        record('native-stop-recovery', recovered.status === 'completed' ? 'passed' : 'failed', {
          sessionId: recovered.sessionId, cwd: recovered.cwd, status: recovered.status,
          newUserMessageVisible: true, answer: assistantReplyText().slice(0, 1000),
        });
        const renameMenuButton = await waitFor(() => document.querySelector(`[data-session-id="${CSS.escape(session.sessionId)}"] .sidebar__conv-action`));
        renameMenuButton.click();
        const renameMenu = await waitFor(() => [...document.querySelectorAll('[role="menuitem"]')].find((item) => item.textContent?.trim() === '重命名'));
        renameMenu.click();
        const renameInput = await waitFor(() => document.querySelector('input[aria-label="会话名称"]'));
        Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set.call(renameInput, expectedTitle);
        renameInput.dispatchEvent(new Event('input', { bubbles: true }));
        await delay(150);
        const renameSave = await waitFor(() => document.querySelector('.context-menu__rename-save'));
        renameSave.click();
        const saved = await waitFor(async () => (await invoke("agent_list_all_sessions", { includeArchived: true }))
          .find((item) => item.sessionId === session.sessionId && item.title === expectedTitle));
        record("native-rename-persistence", saved?.title === expectedTitle ? "passed" : "failed", { title: saved?.title });
        if (nativeContext.mediaRequested) {
          const modelId = 'organization/MiniMax-M3.1-Flash-Preview';
          await chooseModel(modelId);
          await send(agentOfficeText);
          await waitFor(() => [...document.querySelectorAll('.msg--user')].some((item) => item.textContent?.includes(agentOfficeText)));
          let approved = false;
          const outcome = await waitFor(async () => {
            const permission = document.querySelector('.perm-inline');
            if (permission && !approved) {
              const title = permission.querySelector('.perm-inline__title')?.textContent || '';
              const raw = permission.querySelector('.perm-inline__raw')?.textContent || '';
              let input;
              try { input = JSON.parse(raw); } catch { /* An unknown request must not be approved. */ }
              const officeInput = input?.tool_name === 'echoagent-office__office_create'
                ? input.tool_input : /echoagent-office__office_create/.test(title) ? input : null;
              if (officeInput?.title !== agentOfficeTitle || officeInput?.format !== 'docx'
                || typeof officeInput.markdown !== 'string'
                || !officeInput.markdown.includes('今天确认任务状态，明天完成复核。')) {
                record('native-agent-office-permission', 'failed', { title, input });
                const reject = [...permission.querySelectorAll('button')].find((button) => button.textContent?.trim() === '拒绝');
                reject?.click();
                throw new Error('Native Agent requested an unexpected operation during the document task');
              }
              const allow = [...permission.querySelectorAll('button')].find((button) => button.textContent?.trim() === '允许一次' && !button.disabled);
              if (allow) { allow.click(); approved = true; }
            }
            const current = (await invoke('agent_list_all_sessions', { includeArchived: true })).find((item) => item.sessionId === session.sessionId);
            if (current?.status === 'failed' || current?.status === 'stopped') return current;
            return current?.status === 'completed' && assistantReplyText().includes('验收成功') ? current : null;
          }, 120000);
          // Completed execution groups collapse when the final answer starts.
          // Open the actual disclosure, as a user does, before locating a tool.
          let tool;
          if (outcome.status === 'completed') {
            const lastUser = [...document.querySelectorAll('.msg--user')].at(-1);
            const execution = [...document.querySelectorAll('.execution-process__header')]
              .filter((item) => !lastUser || (lastUser.compareDocumentPosition(item) & Node.DOCUMENT_POSITION_FOLLOWING))
              .at(-1);
            if (execution?.getAttribute('aria-expanded') === 'false') execution.click();
            tool = await waitFor(() => [...document.querySelectorAll('.toolcall--ok')]
              .find((item) => item.getClientRects().length > 0 && item.textContent?.includes(agentOfficeTitle)));
          }
          report.agentOffice = { title: agentOfficeTitle, sessionId: session.sessionId, cwd: session.cwd,
            modelId, status: outcome.status, toolVisible: !!tool, userMessageVisible: true,
            approvedOnce: approved, answer: assistantReplyText().slice(0, 2000) };
          record('native-agent-office-task', outcome.status === 'completed' && !!tool ? 'passed' : 'failed', report.agentOffice);
          if (outcome.status !== 'completed' || !tool) throw new Error('The native Agent did not complete the actual Office tool task');
          tool.click();
          const officePath = await waitFor(() => {
            const button = document.querySelector('.tool-side-panel .tool-detail .diff__path--clickable');
            const path = button?.textContent?.trim();
            return button && !button.disabled && button.getClientRects().length > 0
              && path?.startsWith(`${session.cwd}/EchoAgent成果/${agentOfficeTitle}-`)
              && path.endsWith('.docx') ? path : null;
          });
          report.agentOffice.receiptPath = officePath;
          report.agentOffice.receiptVisibleInDetails = true;
          const closeDetails = document.querySelector('.tool-side-panel button[aria-label="关闭面板"]');
          if (!closeDetails) throw new Error('Native Office detail panel has no close control');
          closeDetails.click();
          const math = await waitFor(() => document.querySelector('.msg--assistant .msg__body > .markdown-body[data-md-theme="loose"] .katex'), 30000);
          const diagram = await waitFor(() => {
            const image = document.querySelector('.msg--assistant .msg__body > .markdown-body[data-md-theme="loose"] .md-mermaid-diagram img');
            return image?.complete && image.naturalWidth > 0 ? image : null;
          }, 30000);
          diagram.scrollIntoView({ block: 'center' });
          await delay(150);
          const mathRect = math.getBoundingClientRect();
          const diagramRect = diagram.getBoundingClientRect();
          const officeExport = math.closest('.msg--assistant')?.querySelector('button[aria-label="导出回复为办公文件"]');
          if (!officeExport) throw new Error('Native answer has no Office export entry');
          officeExport.scrollIntoView({ block: 'center' });
          officeExport.click();
          const exportMenu = await waitFor(() => math.closest('.msg--assistant')?.querySelector('.msg__export-menu'));
          const exportItems = [...exportMenu.querySelectorAll('[role="menuitem"]')];
          const scopeNotes = [...exportMenu.querySelectorAll('.msg__export-note')];
          const menuRect = exportMenu.getBoundingClientRect();
          const exportScope = scopeNotes.length === 2 && scopeNotes.every((note) => note.getBoundingClientRect().height > 0)
            && scopeNotes[0].textContent.includes('原始文本') && scopeNotes[0].textContent.includes('SVG')
            && scopeNotes[1].textContent.includes('仅导出表格') && exportItems.length === 4
            && exportItems.every((item) => !item.disabled && item.getAttribute('aria-describedby'))
            && menuRect.left >= 0 && menuRect.right <= innerWidth && menuRect.top >= 0 && menuRect.bottom <= innerHeight;
          exportItems[0]?.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
          await waitFor(() => !math.closest('.msg--assistant')?.querySelector('.msg__export-menu'));
          record('native-markdown-render', mathRect.width > 0 && mathRect.height > 0
            && diagramRect.width > 0 && diagramRect.height > 0
            && diagramRect.top >= 0 && diagramRect.bottom <= innerHeight && exportScope ? 'passed' : 'failed', {
            math: { width: mathRect.width, height: mathRect.height },
            mermaid: { width: diagramRect.width, height: diagramRect.height,
              top: diagramRect.top, bottom: diagramRect.bottom,
              naturalWidth: diagram.naturalWidth, naturalHeight: diagram.naturalHeight },
            officeExportScope: { visible: exportScope, notes: scopeNotes.map((note) => note.textContent),
              formats: exportItems.map((item) => item.textContent), keyboardClosed: true },
          });
          await chooseModel(targetModelId);
        }
        const projectNav = await waitFor(() => [...document.querySelectorAll('.sidebar__nav-item')].find((item) => item.textContent?.trim() === '项目'));
        projectNav.click();
        const createProject = await waitFor(() => [...document.querySelectorAll('button')].find((item) => item.textContent?.trim() === '新建项目'));
        createProject.click();
        const projectInput = await waitFor(() => document.querySelector('#create-project-name'));
        Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set.call(projectInput, projectName);
        projectInput.dispatchEvent(new Event('input', { bubbles: true }));
        const projectModel = await waitFor(() => document.querySelector('#create-project-model'));
        Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, 'value').set.call(projectModel, targetModelId);
        projectModel.dispatchEvent(new Event('change', { bubbles: true }));
        await delay(150);
        const createConfirm = await waitFor(() => [...document.querySelectorAll('[role="dialog"][aria-label="新建项目"] button')]
          .find((item) => item.textContent?.trim() === '确定' && !item.disabled));
        createConfirm.click();
        const project = await waitFor(async () => (await invoke('projects_load')).find((item) => item.name === projectName));
        record('native-project-create', project.defaultModelId === targetModelId ? 'passed' : 'failed', {
          id: project.id, name: project.name, defaultModelId: project.defaultModelId, cwd: project.cwd,
        });
        const sessionMenu = await waitFor(() => document.querySelector(`[data-session-id="${CSS.escape(session.sessionId)}"] .sidebar__conv-action`));
        sessionMenu.click();
        const moveMenu = await waitFor(() => [...document.querySelectorAll('[role="menuitem"]')].find((item) => item.textContent?.includes('移入项目')));
        moveMenu.click();
        const projectRadio = await waitFor(() => [...document.querySelectorAll('input[name="target-project"]')].find((item) => item.value === project.id));
        projectRadio.click();
        const moveConfirm = await waitFor(() => [...document.querySelectorAll('.project-move-dialog button')].find((item) => item.textContent?.trim() === '移入项目' && !item.disabled));
        moveConfirm.click();
        const associated = await waitFor(async () => (await invoke('projects_load')).find((item) => item.id === project.id
          && item.conversations?.some((conversation) => conversation.sessionId === session.sessionId)));
        const unchangedSession = (await invoke('agent_list_all_sessions', { includeArchived: true })).find((item) => item.sessionId === session.sessionId);
        record('native-project-association', unchangedSession?.cwd === expected.cwd ? 'passed' : 'failed', {
          projectId: associated.id, sessionId: session.sessionId, cwd: unchangedSession?.cwd,
        });
        report.project = { id: associated.id, name: associated.name };
        localStorage.setItem(reviewKey, JSON.stringify({ version: 1, session: { ...expected, modelResponsePassed }, project: report.project }));
        if (nativeContext.mediaRequested) {
          const modelId = 'organization/MiniMax-M3.1-Flash-Preview';
          const providerId = 'echoagent-organization';
          await invoke('meeting_check_connection', { modelId, providerId });
          const meeting = await invoke('meeting_import_audio', {
            sourcePath: `${session.cwd}/中文会议验收.wav`, title: '原生中文会议验收', modelId, providerId,
          });
          record('native-meeting-import', meeting.durationSeconds > 5 && meeting.durationSeconds < 8 ? 'passed' : 'failed', {
            id: meeting.id, audioPath: meeting.audioPath, durationSeconds: meeting.durationSeconds, modelId: meeting.modelId, providerId: meeting.providerId,
          });
          const processed = await invoke('meeting_process', { meetingId: meeting.id, modelId, providerId });
          const processedOk = processed.status === 'completed' && processed.transcriptText?.includes('任务状态')
            && /任务|状态/.test(processed.minutes || '') && /明天|次日/.test(processed.minutes || '') && !processed.error;
          record('native-meeting-process', processedOk ? 'passed' : 'failed', {
            id: processed.id, status: processed.status, transcriptText: processed.transcriptText,
            minutes: processed.minutes, error: processed.error,
          });
          if (!processedOk) throw new Error('Native meeting processing did not complete with actual transcript and minutes');
          const transcript = processed.transcript.map((segment) => ({ ...segment, text: segment.text.replaceAll('复合', '复核') }));
          await invoke('meeting_update', { meetingId: meeting.id, transcript, title: null, minutes: null });
          const regenerated = await invoke('meeting_regenerate_minutes', { meetingId: meeting.id, modelId, providerId });
          if (regenerated.status !== 'completed' || !regenerated.minutes?.includes('复核') || regenerated.error) {
            throw new Error('Regenerated minutes did not reflect the corrected transcript');
          }
          const minutes = `${regenerated.minutes || ''}\n\n## 人工复核\n\n- 验收人工编辑：明天完成复核。\n`;
          const edited = await invoke('meeting_update', { meetingId: meeting.id, title: null, transcript: null, minutes });
          const savedMeeting = await invoke('meeting_get', { meetingId: meeting.id });
          record('native-meeting-edit', edited.minutes === minutes && savedMeeting.minutes === minutes
            && savedMeeting.transcriptText?.includes('复核') ? 'passed' : 'failed', {
            id: savedMeeting.id, audioPath: savedMeeting.audioPath, durationSeconds: savedMeeting.durationSeconds,
            minutes: savedMeeting.minutes, transcriptText: savedMeeting.transcriptText,
          });
          report.meeting = { id: savedMeeting.id, audioPath: savedMeeting.audioPath, minutes };
          // The parent test driver calls the real, authenticated local Office
          // MCP endpoint. This handshake never substitutes a native command or
          // bypasses the session workspace grant; it only keeps this test-owned
          // process alive until actual Worker files can be checked.
          await invoke('write_text_file', {
            path: 'native-office-ready.json', workspaceRoot: session.cwd,
            content: JSON.stringify({ sessionId: session.sessionId, cwd: session.cwd, phase: report.phase }),
          });
          const office = await waitFor(async () => {
            let content;
            try { content = await invoke('read_text_file', {
              path: `${session.cwd}/native-office-done.json`, cwd: session.cwd, maxBytes: 131072,
            }); } catch { return null; }
            return JSON.parse(content);
          }, 180000);
          if (!office.passed) throw new Error(`Native Office integration failed: ${office.error || 'no valid receipts'}`);
          report.office = office;
          record('native-office-files', 'passed', { receipts: office.receipts, negativeChecks: office.negativeChecks });
          localStorage.setItem(reviewKey, JSON.stringify({ version: 1, session: { ...expected, modelResponsePassed }, project: report.project, meeting: report.meeting, office }));
        }
      }
    }
    if (nativeContext.extendedRequested) {
      const state = JSON.parse(localStorage.getItem(reviewKey) || 'null');
      const session = report.phase === 'first-launch'
        ? { sessionId: report.sessionId, cwd: report.workspace } : state?.session;
      if (!session?.sessionId || !session.cwd) throw new Error('Extended native review requires the actual core session/workspace');
      const plan = await window.__echoNativePlanReview({
        invoke, home: nativeContext.home, cwd: session.cwd, phase: report.phase,
        expected: state?.plan, returnSessionId: session.sessionId,
      });
      report.plan = plan;
      record('native-plan-approval', plan.passed && plan.errors.length === 0 ? 'passed' : 'failed', plan);
      if (report.phase === 'first-launch') {
        state.plan = plan.expected;
        localStorage.setItem(reviewKey, JSON.stringify(state));
      }
      const knowledge = await window.__echoNativeKnowledgeReview({
        invoke, home: nativeContext.home, cwd: session.cwd, phase: report.phase, expected: state?.knowledge,
      });
      report.knowledge = knowledge;
      record('native-knowledge-memory', knowledge.passed && knowledge.errors.length === 0 ? 'passed' : 'failed', knowledge);
      if (report.phase === 'first-launch') {
        state.knowledge = knowledge.expected;
        localStorage.setItem(reviewKey, JSON.stringify(state));
      }
      const automation = await window.__echoNativeAutomationReview({
        invoke, home: nativeContext.home, cwd: session.cwd, phase: report.phase, expected: state?.automation,
      });
      report.automation = automation;
      record('native-scheduled-task', automation.passed && automation.errors.length === 0 ? 'passed' : 'failed', automation);
      if (report.phase === 'first-launch') {
        state.automation = automation.expected;
        localStorage.setItem(reviewKey, JSON.stringify(state));
      }
    }
  } catch (error) {
    report.errors.push(String(error));
    report.uiAtFailure = {
      selectedSessions: [...document.querySelectorAll('.sidebar__conv-select[aria-current="page"]')]
        .map((item) => item.closest('[data-session-id]')?.dataset.sessionId),
      userMessages: [...document.querySelectorAll('.msg--user')].map((item) => item.innerText?.slice(0, 500)),
      assistantText: assistantReplyText().slice(0, 1000),
      errorText: document.querySelector('.chatview__error-text')?.textContent?.slice(0, 1000),
      composerDisabled: document.querySelector('.echo-composer textarea')?.disabled,
    };
  }
  report.finishedAt = new Date().toISOString();
  await invoke("desktop_review_complete", { report });
})();
