// Fixed native journey: real product input, configured model, Runtime tools,
// Theia documents and native checks. Initial recent-project metadata and two
// directory grants are fixed native fixture setup; no task/session/result state
// or model response is injected.
((scope) => {
  if (window.__echoNativeWorkspaceJourney) return;
  window.__echoNativeWorkspaceJourney = true;
  const invoke = window.__TAURI_INTERNALS__?.invoke;
  const home = scope?.home;
  const cwd = `${home}/workspaces/default`;
  const modelId = 'echoagent-ojlab/Echo Reed';
  const original = 'exports.double = n => n + 1;\n';
  const safeSource = value => typeof value === 'string' && value.length <= 4096
    && value.includes('UNSAVED_A01')
    && /^\s*exports\s*\.\s*double\s*=\s*n\s*=>\s*n\s*\*\s*2\s*;\s*$/.test(
      value.replace(/\/\*[\s\S]*?\*\/|\/\/[^\r\n\u2028\u2029]*/g,' '));
  const report = { version: 1, environment: 'macOS WKWebView + production React/Runtime/Theia + real configured model',
    checks: [], errors: [], steps: [], passed: false, home, cwd, modelId,
    mode:scope.inlineOnly===true?'inline-only':'full-journey' };
  const deadline = Date.now() + 660000;
  const delay = ms => new Promise(resolve => setTimeout(resolve, ms));
  const assert = (condition, message) => { if (!condition) throw new Error(message); };
  const visible = element => !!element && !element.closest('[hidden]') && element.getClientRects().length > 0;
  const button = (root, text) => [...(root?.querySelectorAll('button') ?? [])].find(element => visible(element) && !element.disabled && element.textContent.trim() === text);
  const record = (id, evidence) => report.checks.push({ id, evidence, result: 'passed' });
  const call = async (command, args = {}) => invoke(command, args);
  const wait = async (check, label, timeout = 30000) => {
    report.activeStep = label; report.steps.push({ label, at: new Date().toISOString() });
    const end = Math.min(deadline, Date.now() + timeout);
    while (Date.now() < end) { const result = await check(); if (result) return result; await delay(150); }
    throw new Error(`Timed out: ${label}`);
  };
  const text = (element, value) => {
    element.focus();
    Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value').set.call(element, value);
    element.dispatchEvent(new Event('input', { bubbles: true }));
  };
  const inline = [];
  const inlineRequests = [];
  const documentRequests = [];
  const completions = [];
  const pending = new Map();
  window.addEventListener('message', event => {
    const frame = document.querySelector('.echo-theia__frame');
    if (!frame || event.source !== frame.contentWindow || event.origin !== new URL(frame.src).origin) return;
    if (event.data?.type?.startsWith('echo/inline-completion-')) {
      inline.push({ type:event.data.type, requestId:event.data.requestId, uri:event.data.uri, version:event.data.version,
        latencyMs:event.data.latencyMs, characters:event.data.characters, reason:event.data.reason, error:event.data.error });
    }
    if(event.data?.type==='echo/inline-completion') {
      inlineRequests.push({requestId:event.data.requestId,uri:event.data.uri,version:event.data.version,maxTokens:event.data.maxTokens});
    }
    if (event.data?.type === 'echo/native-workspace-result' && event.data.nonce === scope.nonce) {
      const deferred = pending.get(event.data.id); if (!deferred) return;
      pending.delete(event.data.id); event.data.ok ? deferred.resolve(event.data.value) : deferred.reject(new Error(event.data.error));
    }
  });
  const frameAction = action => new Promise((resolve,reject) => {
    const frame = document.querySelector('.echo-theia__frame'); assert(frame, 'Actual Theia frame missing');
    const id = crypto.randomUUID();
    const timer = setTimeout(() => {pending.delete(id);reject(new Error(`Native editor ${action} timed out`));},45000);
    pending.set(id,{resolve:value=>{clearTimeout(timer);resolve(value);},reject:error=>{clearTimeout(timer);reject(error);}});
    frame.contentWindow.postMessage({type:'echo/native-workspace-operation',nonce:scope.nonce,id,action},new URL(frame.src).origin);
  });
  const hostFocusDiagnostics = () => {
    const frame=document.querySelector('.echo-theia__frame');
    const describe=element=>element?{tag:element.tagName,id:element.id,className:typeof element.className==='string'?element.className.slice(0,250):'',
      role:element.getAttribute('role'),ariaLabel:element.getAttribute('aria-label')?.slice(0,200)??null,inert:element.inert===true}:null;
    const active=document.activeElement;
    const inertAncestors=[];
    for(let element=active; element&&inertAncestors.length<10; element=element.parentElement)if(element.inert)inertAncestors.push(describe(element));
    const style=frame?getComputedStyle(frame):null,rect=frame?.getBoundingClientRect();
    const dialogs=[...new Set(document.querySelectorAll('[role="dialog"],[role="alertdialog"],dialog,[aria-modal="true"]'))]
      .filter(visible).slice(0,10).map(element=>({...describe(element),ariaModal:element.getAttribute('aria-modal'),
        text:element.textContent?.replace(/\s+/g,' ').trim().slice(0,300)??''}));
    return {source:'read-only parent DOM observation',documentHasFocus:document.hasFocus(),activeElement:describe(active),
      activeElementIsFrame:active===frame,activeElementDialog:describe(active?.closest('[role="dialog"],[role="alertdialog"],dialog,[aria-modal="true"]')),
      inertAncestors,visibleDialogs:dialogs,
      frame:frame?{...describe(frame),hidden:frame.hidden,display:style.display,visibility:style.visibility,opacity:style.opacity,
        rect:{x:rect.x,y:rect.y,width:rect.width,height:rect.height},inertAncestor:describe(frame.parentElement?.closest('[inert]'))}:null};
  };
  const focusEditorFrame=async()=>{
    const frame=document.querySelector('.echo-theia__frame');
    assert(visible(frame)&&!frame.inert,'Actual editor iframe is not focusable');
    frame.focus({preventScroll:true});
    await wait(()=>document.activeElement===frame,'Actual parent focus entered the editor iframe');
  };
  const focusInlineInput=async()=>{
    await focusEditorFrame();
    const target=await frameAction('tab_focus_target');
    assert(target.path===`${cwd}/completion.cjs`,'Native focus target is outside the fixed completion file');
    report.inlineFocusTargetBefore=target;
    if(!target.focused) {
      const frame=document.querySelector('.echo-theia__frame'),rect=frame.getBoundingClientRect(),style=getComputedStyle(frame);
      assert(visible(frame)&&!frame.inert&&!frame.closest('[inert]')&&style.visibility==='visible'&&style.display!=='none','Native pointer target frame is not visible/unlocked');
      const context=await call('desktop_review_context',{inlineFocusClick:{nonce:scope.nonce,path:target.path,input:target.input,
        childViewport:target.childViewport,frame:{x:rect.x,y:rect.y,width:rect.width,height:rect.height},viewport:{width:innerWidth,height:innerHeight}}});
      report.nativeInlinePointer=context.inlinePointer;
      assert(report.nativeInlinePointer?.downAndUpDispatched===true,'Native own-window focus gesture missing');
    }
    report.inlineFocusAfterPointer=await frameAction('inline_diagnostics');
  };
  const chooseMode = async value => {
    const select = await wait(() => document.querySelector('select[aria-label="开发意图"]'),'Actual intent selector');
    Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype,'value').set.call(select,value);
    select.dispatchEvent(new Event('change',{bubbles:true})); await delay(100);
    assert(select.value === value,'Actual mode selection failed');
  };
  const nextAdmittedRun = async (task, previous) => wait(async()=>{
    const entries=await call('coding_checkpoint_list',{root:cwd,taskId:task.id});
    const fresh=entries.filter(entry=>!previous.includes(entry.runId));
    assert(fresh.length<=1,'One UI admission created multiple native runs');
    return fresh[0]?.runId??null;
  },'Actual native admitted prompt identity',30000);
  const successfulCompletion = (sessionId,runId) => {
    const complete=completions.find(event=>event.sessionId===sessionId&&event.promptId===runId);
    if(complete)assert(complete.stopReason==='end_turn'&&!complete.cancellationCategory,`Actual run ended ${complete.stopReason}/${complete.cancellationCategory??''}`);
    return complete;
  };
  const send = async (value,mode) => {
    const previous=report.taskId?(await call('coding_checkpoint_list',{root:cwd,taskId:report.taskId})).map(entry=>entry.runId):[];
    await chooseMode(mode);
    const input = await wait(() => {const el=document.querySelector('.echo-theia-agent__composer textarea[aria-label="任务描述"], .echo-theia-agent__composer textarea[aria-label="补充开发要求"]');return visible(el)&&!el.disabled?el:null;},'Actual Coding input');
    text(input,value);
    const liveBuffer=await frameAction('state');
    if(liveBuffer.path===`${cwd}/double.cjs`&&liveBuffer.dirty) {
      assert(liveBuffer.selection&&liveBuffer.text.includes('UNSAVED_A01'),'Send lost the real selected dirty buffer');
      await wait(()=>document.querySelector('.echo-theia-agent__context > span')?.title===`${cwd}/double.cjs`,'Production remembers the actual editor while composer has focus');
    }
    (await wait(()=>{const el=document.querySelector('.echo-theia-agent__send');return visible(el)&&!el.disabled?el:null;},'Actual Coding send')).click();
    const task = await wait(async()=>{const tasks=await call('coding_task_list',{root:cwd});assert(tasks.length<=1,'One send created duplicate tasks');if(!tasks.length)return null;const task=await call('coding_task_get',{root:cwd,taskId:tasks[0].id});return task.sessionId?task:null;},'Production task/session binding',60000);
    report.taskId=task.id;report.sessionId=task.sessionId;
    assert(task.modelId===modelId,'Actual Coding task selected a different configured model');
    const runId=await nextAdmittedRun(task,previous);
    await wait(async()=>{await permissions(task.sessionId,mode);return successfulCompletion(task.sessionId,runId)&&!document.querySelector('.echo-theia-agent__stop');},`Real ${mode} model run complete`,180000);
    return task;
  };
  const permissions = async (sessionId,mode) => {
    const interactions=await call('agent_list_pending_interactions',{sessionId});
    assert(!interactions.questions?.length&&!interactions.planApprovals?.length&&!interactions.folderTrustRequests?.length,'Unexpected model question/plan/trust outside fixed journey');
    for(const request of interactions.permissions??[]) {
      let raw=request.rawInput;if(typeof raw==='string'){try{raw=JSON.parse(raw);}catch{raw=null;}}
      assert(request.sessionId===sessionId,'Permission belongs to another session');
      const names={read_file:'ReadFile',grep:'Grep',glob:'Glob',list_dir:'ListDir',search_replace:'SearchReplace',write:'Write',bash:'Bash',run_terminal_command:'Bash',todo_write:'TodoWrite'};
      if(typeof raw?.tool_name==='string')raw={...raw.tool_input,variant:names[raw.tool_name.split(/:|__/).pop().toLowerCase()]};
      const path=raw?.file_path??raw?.target_file??raw?.path;
      const owned=['double.cjs',`${cwd}/double.cjs`].includes(path);
      const harmlessRead=raw?.variant==='ReadFile'&&typeof path==='string'&&(owned||['package.json','test.cjs',`${cwd}/package.json`,`${cwd}/test.cjs`].includes(path));
      const scopedPath=value=>value==null||['.','./',cwd,`${cwd}/`,'double.cjs','test.cjs','package.json','completion.cjs',`${cwd}/double.cjs`,`${cwd}/test.cjs`,`${cwd}/package.json`,`${cwd}/completion.cjs`].includes(value);
      const safePattern=value=>typeof value==='string'&&value.length>0&&value.length<=512&&!/[\x00\r\n]/.test(value);
      const safeGlob=value=>value==null||typeof value==='string'&&value.length<=128&&!value.startsWith('/')&&!value.includes('..')&&!/[\x00\r\n]/.test(value);
      const harmlessSearch=raw?.variant==='Grep'&&scopedPath(raw.path)&&safePattern(raw.pattern)&&safeGlob(raw.glob)
        ||raw?.variant==='Glob'&&scopedPath(raw.path)&&safeGlob(raw.pattern??raw.glob_pattern)&&typeof(raw.pattern??raw.glob_pattern)==='string'
        ||raw?.variant==='ListDir'&&scopedPath(raw.target_directory??raw.path);
      const harmlessTodos=mode==='agent'&&raw?.variant==='TodoWrite'&&Array.isArray(raw.todos)&&raw.todos.length<=8&&raw.todos.every(item=>item&&typeof item.id==='string'&&(item.content==null||typeof item.content==='string')&&(item.status==null||['pending','in_progress','completed','cancelled'].includes(item.status)));
      let edit=false;
      if(mode==='agent'&&owned&&['SearchReplace','Write'].includes(raw?.variant)) {
        const before=(await frameAction('state')).text;
        const old=raw.old_string??raw.old_str,newText=raw.new_string??raw.new_str;
        const candidate=raw.variant==='Write'?raw.content:typeof old==='string'&&old.length>0&&typeof newText==='string'&&before.split(old).length===2?before.replace(old,newText):null;
        edit=safeSource(candidate);
      }
      const command=mode==='agent'&&raw?.variant==='Bash'&&['npm test','npm run test','npm run lint','node --test test.cjs','node --check double.cjs'].includes(raw.command)&&!raw.is_background&&(raw.cwd==null||raw.cwd===cwd);
      if(!harmlessRead&&!harmlessSearch&&!harmlessTodos&&!edit&&!command) {await call('agent_resolve_permission',{requestId:request.requestId,optionId:null,cancelled:true});throw new Error(`Unexpected model permission denied: ${raw?.variant??'unknown'}`);}
      const allow=button(document.getElementById(`permission-${request.requestId}`),'允许一次');if(allow)allow.click();
    }
  };
  void (async()=>{
    try {
      assert(invoke&&typeof home==='string'&&home.includes('/echo-native-review-')&&scope.workspaceJourney===true&&scope.nonce?.length>=24,'Isolated native workspace scope required');
      const context=await call('desktop_review_context');assert(context.home===home,'Native scope differs');report.nativeContext=context;
      const documentCallback=window.__TAURI_INTERNALS__.transformCallback(event=>{
        const value=event.payload;
        if(value?.workspace===cwd&&value.phase==='before'&&['edit','write'].includes(value.toolKind)) {
          documentRequests.push({sessionId:value.sessionId,runId:value.runId,operationId:value.operationId,toolKind:value.toolKind});
        }
      });
      await call('plugin:event|listen',{event:'agent://document-tool',target:{kind:'Any'},handler:documentCallback});
      const completeCallback=window.__TAURI_INTERNALS__.transformCallback(event=>completions.push(event.payload));
      await call('plugin:event|listen',{event:'agent://complete',target:{kind:'Any'},handler:completeCallback});
      await wait(async()=>{const status=await call('agent_auth_status');return status.ready&&status.runtimeReady&&status.synchronized&&status.runtimeModels?.includes(modelId);},'Existing configured model ready',60000);
      if(document.querySelector('.app__body--collapsed')) (await wait(()=>document.querySelector('button[aria-label="展开侧边栏"]'),'Actual sidebar expansion')).click();
      const nav=await wait(()=>button(document.querySelector('.sidebar__nav'),'代码开发'),'Actual Code navigation');nav.click();
      await wait(()=>visible(document.querySelector('.echo-theia__frame'))&&!document.querySelector('.echo-theia__cover'),'Actual production Theia startup',90000);
      if(scope.inlineOnly===true) {
        assert((await call('coding_task_list',{root:cwd})).length===0,'Tab-only fixture already has a CodingTask');
        await wait(()=>document.querySelector('button[aria-label^="开发任务模型："]')?.getAttribute('aria-label').includes('Echo Reed'),'Tab-only existing configured Echo Reed selection');
        const baseline=await call('read_text_file',{path:`${cwd}/completion.cjs`,cwd,maxBytes:4096});
        const inlineStart=inline.length;
        report.hostFocusBefore=hostFocusDiagnostics();
        report.nativeFocusBefore=(await call('desktop_review_context')).inlineFocus;
        await focusInlineInput();
        report.hostFocusAfterFrameFocus=hostFocusDiagnostics();
        await frameAction('tab_prepare');
        report.inlineDiagnosticsBefore=await frameAction('inline_diagnostics');
        await frameAction('tab_trigger');
        const shown=await wait(()=>{
          const error=inline.slice(inlineStart).find(event=>event.type==='echo/inline-completion-error');
          if(error)throw new Error(`Real Tab model failed: ${String(error.error).slice(0,250)}`);
          return inline.slice(inlineStart).find(event=>event.type==='echo/inline-completion-shown');
        },'Tab-only actual model ghost text shown',25000);
        const beforeAccept=await frameAction('state');
        const accepted=await frameAction('tab_accept');
        assert(accepted.text!==beforeAccept.text,'Native inline accept inserted nothing');
        const acceptedEvent=await wait(()=>inline.slice(inlineStart).find(event=>event.type==='echo/inline-completion-accepted'),'Tab-only production accepted event');
        assert(acceptedEvent.requestId===shown.requestId&&shown.version===beforeAccept.version,'Tab-only accept did not match the actual shown version/request');
        const undone=await frameAction('undo');
        assert(undone.text===beforeAccept.text,'Native Undo did not reverse the real accepted inline text');
        const discardStart=inline.length;
        const typedDuring=await frameAction('type_during_completion');
        await frameAction('tab_escape');
        await wait(()=>{
          const error=inline.slice(discardStart).find(event=>event.type==='echo/inline-completion-error');
          if(error)throw new Error(`Real Tab cancellation request failed: ${String(error.error).slice(0,250)}`);
          return inline.slice(discardStart).some(event=>event.type==='echo/inline-completion-discarded');
        },'Tab-only fast typing and real Escape discard',20000);
        await delay(700);
        assert((await frameAction('state')).text===typedDuring.text,'A stale/cancelled Tab suggestion changed newer user input');
        assert((await call('coding_task_list',{root:cwd})).length===0,'Tab created a CodingTask');
        assert(completions.length===0&&documentRequests.length===0,'Tab invoked an Agent or document tool run');
        assert(inlineRequests.length>0&&inlineRequests.length<=2&&inlineRequests.every(request=>request.maxTokens===256),'Tab-only must use only normal bounded provider requests');
        assert(await call('read_text_file',{path:`${cwd}/completion.cjs`,cwd,maxBytes:4096})===baseline,'Tab saved or changed disk');
        record('A17-real-inline-only',{source:'actual production Code UI + Theia native inline provider',requestId:shown.requestId,
          firstSuggestionMs:shown.latencyMs,accepted:true,tabKeyboard:true,nativeUndo:true,fastTyping:true,escapeKeyboard:true,
          diskUnchanged:true,taskCountBefore:0,taskCountAfter:0,events:inline.slice(inlineStart),
          requests:inlineRequests,
          scope:'Tab-only; no Ask/Plan/Agent admission, task creation, model result injection or completion retry'});
      } else {
      const opened=await frameAction('open');assert(opened.text===original&&opened.visible,'Theia fixture differs');
      const dirty=await frameAction('dirty');assert(dirty.dirty&&dirty.text.includes('UNSAVED_A01'),'Actual unsaved editor input missing');
      await send('只读解释当前所选函数的行为，并明确提到编辑器未保存上下文中的 UNSAVED_A01。不要保存或修改任何文件，不运行命令，不委派。','ask');
      assert(await call('read_text_file',{path:`${cwd}/double.cjs`,cwd,maxBytes:4096})===original,'Ask changed or saved disk');
      const afterAsk=await frameAction('state');assert(afterAsk.dirty&&afterAsk.text===dirty.text,'Ask lost the unsaved buffer');
      record('A01-real-unsaved-ask',{sessionId:report.sessionId,taskId:report.taskId,version:dirty.version,selection:dirty.selection,diskUnchanged:true,bufferPreserved:true});
      await send('只读规划把 double.cjs 中 double(n) 修成 n 的两倍，保留未保存注释，仅给两步方案，不写工程、不运行命令。','plan');
      assert(await call('read_text_file',{path:`${cwd}/double.cjs`,cwd,maxBytes:4096})===original,'Plan modified disk');
      const editable=await wait(()=>document.querySelector('textarea[aria-label="编辑开发计划"]'),'Actual editable Plan');
      const finalPlan='仅修改 double.cjs：先读取当前编辑器内容，再用 search_replace 将 n + 1 改成 n * 2，保留 UNSAVED_A01 注释。不要改其他文件、安装依赖、使用 Git、联网或委派；不要切换模式或调用 todo_write；本轮不执行命令，检查由用户随后触发。完成后说明修改。';
      text(editable,finalPlan);
      const agentPriorRuns=(await call('coding_checkpoint_list',{root:cwd,taskId:report.taskId})).map(entry=>entry.runId);
      (await wait(()=>button(document.querySelector('.coding-editable-plan'),'执行最终计划'),'Actual final-plan execution')).click();
      const agentRunId=await nextAdmittedRun({id:report.taskId},agentPriorRuns);
      await wait(async()=>{await permissions(report.sessionId,'agent');const complete=successfulCompletion(report.sessionId,agentRunId);const content=await call('read_text_file',{path:`${cwd}/double.cjs`,cwd,maxBytes:4096});return complete&&/n\s*\*\s*2/.test(content)&&content.includes('UNSAVED_A01')&&!document.querySelector('.echo-theia-agent__stop')?content:null;},'Real model edit through versioned document bridge',180000);
      const changed=await frameAction('state');assert(/n\s*\*\s*2/.test(changed.text)&&changed.text.includes('UNSAVED_A01'),'Theia did not show the real model edit');
      assert(safeSource(changed.text),'Actual generated code contains executable content outside the fixed task');
      const bridgeRequest=await wait(()=>documentRequests.find(request=>request.sessionId===report.sessionId&&request.runId===agentRunId),'Actual native document operation identity');
      const checkpoint=await wait(async()=>{
        const checkpoints=await call('coding_checkpoint_list',{root:cwd,taskId:report.taskId});
        return checkpoints.find(entry=>entry.runId===bridgeRequest.runId&&entry.finishedAt&&entry.confirmedPaths.includes('double.cjs'));
      },'Actual finished run checkpoint confirms the editor operation',30000);
      assert(!checkpoint.externalPaths.includes('double.cjs'),'The real editor write was classified as an unattributed external change');
      const agentUndo=await frameAction('undo');
      assert(agentUndo.text===dirty.text&&agentUndo.text.includes('n + 1')&&agentUndo.text.includes('UNSAVED_A01'),'Native undo did not reverse precisely the Agent edit while preserving the user buffer');
      const agentRedo=await frameAction('redo');
      const savedAfterRedo=await call('read_text_file',{path:`${cwd}/double.cjs`,cwd,maxBytes:4096});
      assert(agentRedo.text===changed.text&&agentRedo.text===savedAfterRedo,'Native redo did not restore the Agent edit consistently with saved disk');
      record('A03-A04-real-plan-agent',{sessionId:report.sessionId,taskId:report.taskId,runId:bridgeRequest.runId,operationId:bridgeRequest.operationId,finalPlan,editorVersion:changed.version,checkpoint,nativeAgentUndo:true,nativeAgentRedo:true,savedBufferMatches:true,source:'actual native document operation + confirmed checkpoint + Theia undo/redo'});
      (await wait(()=>button(document.querySelector('.coding-session-actions'),'检查'),'Actual check entry')).click();
      const records=()=>call('coding_verification_list',{root:cwd,taskId:report.taskId});
      const firstButton=await wait(()=>[...document.querySelectorAll('button[title]')].find(el=>visible(el)&&!el.disabled&&el.title==='npm run test'),'Actual test command');firstButton.click();
      const first=await wait(async()=>{const all=await records();return all.find(record=>record.command==='npm run test'&&record.status==='passed'&&record.exitCode===0);},'Actual first native check',60000);
      const secondButton=await wait(()=>[...document.querySelectorAll('button[title]')].find(el=>visible(el)&&!el.disabled&&el.title==='npm run lint'),'Actual lint command');secondButton.click();
      const second=await wait(async()=>{const all=await records();assert(all.some(record=>record.id===first.id),'Second check erased first receipt');return all.find(record=>record.command==='npm run lint'&&record.status==='passed'&&record.exitCode===0);},'Actual independent second native check',60000);
      record('A07-two-real-checks',{first,second});
      (await wait(()=>[...document.querySelectorAll('.coding-session-actions button')].find(el=>visible(el)&&!el.disabled&&el.textContent.trim().startsWith('变更')),'Actual changes entry')).click();
      const diffButton=await wait(()=>[...document.querySelectorAll('.coding-changeset__row button[title="double.cjs"]')].find(el=>visible(el)&&!el.disabled),'Actual diff file');diffButton.click();
      const diff=await frameAction('diff');
      assert(diff.visible&&diff.original.includes('n + 1')&&diff.modified.includes('n * 2')&&diff.modified.includes('UNSAVED_A01'),'Actual Theia diff did not show baseline and current shared document');
      assert(diff.originalReadOnly&&diff.modifiedUri.includes('double.cjs'),'Actual Diff left side must be read-only and right side the workspace URI');
      record('A04-real-diff',{file:'double.cjs',source:'actual Theia MonacoDiffEditor',...diff});
      document.querySelector('button[aria-label="返回编辑器"]')?.click();
      const beforeSwitch=await frameAction('open');
      const originalTask=await call('coding_task_get',{root:cwd,taskId:report.taskId});
      const switchProject=async target=>{
        (await wait(()=>document.querySelector('button[aria-label="切换项目"]'),'Actual project switcher')).click();
        const select=await wait(()=>[...document.querySelectorAll('.coding-project-switcher__select')].find(element=>visible(element)&&element.querySelector('small')?.textContent===target),'Actual target project entry');
        select.click();
        await wait(()=>document.querySelector('button[aria-label="切换项目"]')?.title.includes(target)&&!document.querySelector('.echo-theia__cover'),'Actual project startup',90000);
      };
      await switchProject(`${home}/workspaces/other`);
      const other=await frameAction('open_other');
      assert(other.path===`${home}/workspaces/other/other.cjs`&&other.text.includes('Independent workspace sentinel'),'Second actual project editor did not load');
      assert((await call('coding_task_list',{root:`${home}/workspaces/other`})).length===0,'Project navigation synthesized an unrelated task');
      await switchProject(cwd);
      const restored=await frameAction('restored');
      assert(restored.path===beforeSwitch.path&&restored.text===beforeSwitch.text,'Returning to the first project lost its actual active document');
      const taskAfter=await call('coding_task_get',{root:cwd,taskId:report.taskId});
      assert(taskAfter.sessionId===report.sessionId&&taskAfter.id===originalTask.id,'Project return changed task/session ownership');
      await wait(()=>document.querySelector('.coding-task-switcher')?.textContent.includes(originalTask.name),'Actual original task selected again');
      assert((await call('coding_task_list',{root:cwd})).length===1,'Project return replayed task creation');
      record('A12-two-real-projects',{from:cwd,to:`${home}/workspaces/other`,returned:cwd,sessionId:report.sessionId,taskId:report.taskId,activeFile:restored.path,bufferPreserved:true,replayedRequests:false,
        setup:'Only initial recent-project metadata and scoped native directory grants; both switches used production ProjectSwitcher UI'});
      const beforeTasks=(await call('coding_task_list',{root:cwd})).length;
      const inlineStart=inline.length;
      await focusInlineInput();
      await frameAction('tab_setup');
      await wait(()=>{const error=inline.slice(inlineStart).find(event=>event.type==='echo/inline-completion-error');if(error)throw new Error(`Real Tab model failed: ${String(error.error).slice(0,150)}`);return inline.slice(inlineStart).find(event=>event.type==='echo/inline-completion-shown');},'Real model ghost text shown',25000);
      const beforeAccept=await frameAction('state');const accepted=await frameAction('tab_accept');
      assert(accepted.text!==beforeAccept.text,'Native inline accept inserted nothing');
      const acceptedEvent=await wait(()=>inline.slice(inlineStart).find(event=>event.type==='echo/inline-completion-accepted'),'Production inline accepted event');
      const shown=inline.slice(inlineStart).find(event=>event.type==='echo/inline-completion-shown'&&event.requestId===acceptedEvent.requestId);
      assert(shown&&shown.version===beforeAccept.version,'Accepted inline request did not match the actual shown document version');
      const undone=await frameAction('undo');assert(undone.text===beforeAccept.text,'Native undo did not reverse accepted inline text');
      const discardStart=inline.length;
      const typedDuring=await frameAction('type_during_completion');await frameAction('tab_cancel');
      await wait(()=>inline.slice(discardStart).some(event=>event.type==='echo/inline-completion-discarded'),'Real stale/cancelled inline discard',20000);
      await delay(500);
      assert((await frameAction('state')).text===typedDuring.text,'A cancelled/stale suggestion changed newer user input');
      assert((await call('coding_task_list',{root:cwd})).length===beforeTasks,'Tab created a CodingTask');
      record('A17-real-inline',{requestId:shown.requestId,firstSuggestionMs:shown.latencyMs,accepted:true,tabKeyboard:true,nativeUndo:true,taskCountUnchanged:true,events:inline});
      }
      report.passed=true;
    }catch(error){
      report.errors.push(String(error));report.passed=false;
      if(scope.inlineOnly===true) {
        report.hostFocusAfter=hostFocusDiagnostics();
        try {report.nativeFocusAfter=(await call('desktop_review_context')).inlineFocus;}
        catch(diagnosticError){report.errors.push(`Native focus diagnostics: ${String(diagnosticError)}`);}
        try {
          const before=await frameAction('state');
          report.inlineDiagnosticsAfter=await frameAction('inline_diagnostics');
          const after=await frameAction('type_after_inline_error');
          assert(after.text!==before.text&&after.text.includes('A17_STILL_TYPING'),'Model error blocked actual editor typing');
          report.typingAfterError={source:'real native editor keyboard input; Escape before automatic debounce; no model retry',
            editable:true,beforeVersion:before.version,afterVersion:after.version,
            taskCount:(await call('coding_task_list',{root:cwd})).length,
            diskUnchanged:await call('read_text_file',{path:`${cwd}/completion.cjs`,cwd,maxBytes:4096})==='// User inline completion scratch file.\n'};
        }catch(typingError){report.errors.push(`Post-error typing check: ${String(typingError)}`);}
      }
    }
    if(scope.inlineOnly===true){report.inlineEvents=inline;report.inlineRequests=inlineRequests;}
    report.finishedAt=new Date().toISOString();
    await call('desktop_review_complete',{report});
  })();
})
