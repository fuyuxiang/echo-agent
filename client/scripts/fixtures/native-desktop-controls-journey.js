// Launch a synthetic task in the production Composer. Desktop actions and
// approvals remain entirely in Runtime/MCP and the normal human approval UI.
window.__echoDesktopControlsActiveBudget = (limitMs, initialNow) => {
  let last=initialNow,wasWaiting=true,activeMs=0;
  return (now,waitingForUser) => {
    // Conservatively exclude the interval spanning either edge of a human wait.
    if(!wasWaiting&&!waitingForUser)activeMs+=Math.max(0,now-last);
    last=Math.max(last,now);wasWaiting=waitingForUser;
    return {activeMs,limitMs,waitingForUser,expired:activeMs>=limitMs};
  };
};
window.__echoDesktopControlsJourney = async ({home,cwd,modelId='echoagent-ojlab/Echo Reed',smoke=false}) => {
  if(window.__echoDesktopControlsJourneyStarted)return;
  window.__echoDesktopControlsJourneyStarted=true;
  const invoke=(command,args={})=>window.__TAURI_INTERNALS__.invoke(command,args);
  const fixtures=`${cwd}/native-control-fixtures`,progress=`${fixtures}/journey-progress.json`;
  const editorText=`桌面合成验收\n中文与多行 😀\n特殊字符 <>&"'\\ $()\n`;
  const report={version:1,kind:smoke?'native-controls-smoke':'native-controls-journey',home,cwd,desktopAcceptance:false,
    approvalsAutomaticallyResolved:false,requestedModelId:modelId,tools:[],approvals:[],errors:[],startedAt:new Date().toISOString()};
  const visible=e=>!!e&&e.getClientRects().length>0;
  const wait=async(fn,label,ms=90000)=>{const end=Date.now()+ms;while(Date.now()<end){const v=await fn();if(v)return v;await new Promise(r=>setTimeout(r,250));}throw Error(`${label}: timeout`);};
  const save=()=>invoke('write_text_file',{path:progress,content:JSON.stringify(report,null,2),workspaceRoot:cwd});
  const listen=async(event,fn)=>{const handler=window.__TAURI_INTERNALS__.transformCallback(fn);await invoke('plugin:event|listen',{event,target:{kind:'Any'},handler});};
  try {
    report.context=await invoke('desktop_review_context');
    if(report.context.home!==home)throw Error('isolated home mismatch');
    await wait(async()=>{const s=await invoke('agent_auth_status');return s.ready&&s.runtimeReady&&s.synchronized;},'Runtime ready');
    const existing=(await invoke('agent_list_all_sessions',{includeArchived:true})).filter(s=>!s.hidden);
    if(existing.length>1||existing.some(s=>s.cwd!==cwd))throw Error('Refuse unrelated existing session data');
    if(existing.length){
      report.sessionId=existing[0].sessionId;
      report.restoredExistingSession=true;
      report.awaitingUserResume=true;
    }
    await listen('automation://approval',({payload})=>{
      report.approvals.push({...payload,observedAt:new Date().toISOString()});
      report.pendingApproval=payload.requestId;void save();
    });
    await listen('automation://approval-closed',({payload})=>{if(report.pendingApproval===payload.requestId)delete report.pendingApproval;void save();});
    await listen('agent://update',({payload})=>{
      if(report.sessionId&&payload.sessionId!==report.sessionId)return;
      report.sessionId=payload.sessionId;
      const u=payload.update||payload,r=u?.rawOutput;
      if(u?.sessionUpdate==='user_message_chunk')delete report.completion;
      if(['completed','failed'].includes(u?.status)&&r?.type==='MCP'&&r.server_name==='echoagent-automation') {
        let output;try{output=JSON.parse(r.output?.OkayOutput);}catch{}
        const entry={id:u.toolCallId,tool:r.tool_name,status:u.status,validJson:!!output,
          executed:output?.executed,verified:output?.verified,resultStatus:output?.status,
          snapshotId:output?.snapshotId,nextCursor:output?.nextCursor,truncated:output?.truncated,
          error:r.output?.Error};
        const index=report.tools.findIndex(t=>t.id===entry.id);
        if(index<0)report.tools.push(entry);else report.tools[index]=entry;
        void save();
      }
    });
    await listen('agent://complete',({payload})=>{if(payload.sessionId===report.sessionId){report.completion=payload;void save();}});
    if(existing.length){
      const selector=`[data-session-id="${CSS.escape(report.sessionId)}"] .sidebar__conv-select`;
      (await wait(()=>{const e=document.querySelector(selector);return visible(e)?e:null;},'existing acceptance task')).click();
      await wait(()=>document.querySelector(selector)?.getAttribute('aria-current')==='page','existing task selected');
    }
    (await wait(()=>{const e=document.querySelector('.echo-composer .model-selector__trigger');return visible(e)&&!e.disabled?e:null;},'model chooser')).click();
    (await wait(()=>[...document.querySelectorAll('.model-selector__item')].find(e=>visible(e)&&e.dataset.modelId===modelId),'exact configured model')).click();
    if(!existing.length){
    (await wait(()=>document.querySelector('.echo-composer button[aria-label="添加"]'),'add menu')).click();
    (await wait(()=>[...document.querySelectorAll('[role="menuitemradio"]')].find(e=>visible(e)&&e.querySelector('.iam-item__label')?.textContent==='操作电脑'&&e.getAttribute('aria-disabled')!=='true'),'Computer Use entry')).click();
    const prompt=`请操作电脑，完成通用桌面控件能力的合成验收，授权范围仅 ${fixtures}。所有桌面动作只能通过 echoagent-automation 的 computer_* / automation_* 工具并遵守明确任务范围及随时接管机制，任务内连续执行、不逐步确认；不得使用Shell、脚本、HTTP、文件写工具或操作业务文档。必须先通过 search_tool 发现完整参数；use_tool 的 tool_name 必须带服务器前缀，例如 {"tool_name":"echoagent-automation__computer_list_apps","tool_input":{}}，禁止用不带前缀的 computer_list_apps 作为 tool_name。先用 computer_list_apps 的 identifier 精确过滤 com.apple.TextEdit；打开应用后必须重新过滤取得新 appRef；computer_list_windows 必须传该 appRef，不要遍历全桌面再误判 TextEdit 不支持控件。使用真实 windowRef 激活、取快照。若要新建文稿或保存且没有原生菜单动作，可用 computer_key_window 绑定已观察窗口优先分别传 shortcut="cmd+n" 和 shortcut="cmd+s"；不要再提供 key/modifiers；审批后由后端恢复并核验焦点，随后重新观察对话框。禁止拿缺失的 windowRef 调工具或在审批窗口里坐标点击。若枚举所有应用，则 nextCursor 必须取全。第一阶段：用 computer_open_app 打开 /System/Applications/TextEdit.app；如已运行，先观察，不能修改任何已有文档。激活其窗口，新建独立文档，使用原生控件输入以下文本（保留真正的换行，含末尾换行）：
--- 合成文本开始 ---
${editorText}--- 合成文本结束 ---
然后读回核验全文。通过真实系统保存对话框将纯文本保存到 ${fixtures}/editor-output.txt（如果需新截图及键盘后备，遵循现有审批，禁止静默降级；结果未知先观察，禁止盲目重放保存）。保存后重新读回或检查窗口验证，再调用 automation_pause 并结束本轮，等待用户按现有恢复流程继续。最后明确报告真实工具结果及仍待验证项目，标记 CONTROL-JOURNEY-STAGE1。本轮不要关闭用户其他窗口，不要发送、发布或登录。后续阶段再进行 Finder、浏览器页面 ${fixtures}/browser.html、文件选择框、独立 Electron 应用和跨应用暂停恢复。`;
    const taskPrompt=smoke?`请操作电脑，完成修复后的短流程实测，仅操作 ${fixtures} 的合成资料。通过 search_tool 按需发现参数（每次最多3个结果），再通过 use_tool 调用 echoagent-automation__computer_*；禁止Shell、脚本或文件写工具代替桌面操作。此明确请求委托执行本任务，任务内连续操作；不要索求逐次确认，也不能扩大任务范围。先按 identifier=com.apple.TextEdit 发现应用，按 appRef 取窗口；如果上轮合成文稿“未命名2”或“未命名3”的保存面板仍打开，先观察并用真实取消按钮关闭该面板，不能修改其他已有文档。新建独立文稿，使用 computer_key_window 的 shortcut="cmd+n"，并在空文稿使用 shortcut="cmd+shift+t" 切换为纯文本，随后重新观察。重新 find 得到正文文本控件 ref，使用 computer_set_value 的 inputMethod="keyboard" 对该控件写入以下两行（保留末尾换行，不额外转义）：
桌面控件验收
中文输入与读回成功
然后 computer_read 核验全文，通过真实保存面板保存到 ${fixtures}/smoke-output.txt。使用 shortcut="cmd+s" 打开保存面板；通过真实 computer_list_windows 项的 kind=window 引用使用 shortcut="cmd+shift+g" 打开前往文件夹面板；find 得到 PathTextField 后用 computer_set_value 的 inputMethod="keyboard" 替换为目录，通过该窗口引用派发 return 并观察目录是否真正切换，再填写文件名并存储。computer_key_window 只能传列表中的真实窗口 ref，不能传 snapshot/find 的控件或 AXSheet ref。每次动作后观察实际状态；不要把工具 verified 当作用户目标完成。连续两次相同失败且状态未变化时停止重试并报告具体阻塞，不要循环截图或反复申请授权。完成或遇到明确阻塞后调用 automation_pause，并报告 CONTROL-SMOKE。此短流程不代表特殊字符、其他应用及全场景验收通过。`:prompt;
    const area=await wait(()=>{const e=document.querySelector('.echo-composer textarea');return visible(e)&&!e.disabled?e:null;},'Composer');
    Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype,'value').set.call(area,taskPrompt);
    area.dispatchEvent(new Event('input',{bubbles:true}));
    (await wait(()=>{const e=document.querySelector('.echo-composer button[aria-label="发送"]');return visible(e)&&!e.disabled?e:null;},'send')).click();
    report.sessionId=await wait(async()=>{const s=(await invoke('agent_list_all_sessions',{includeArchived:true})).find(s=>s.cwd===cwd&&!s.hidden);return s?.sessionId;},'actual UI task');
    }
    report.reportedModelId=await wait(async()=>{
      const s=(await invoke('agent_list_all_sessions',{includeArchived:true})).find(s=>s.sessionId===report.sessionId);
      const id=s?.currentModelId;
      return id===modelId||(modelId.includes('/')&&id===modelId.split('/').at(-1))?id:null;
    },'authoritative session model selection');
    report.selectedModelId=modelId;
    report.modelSelectionVerified=true;
    report.status=await wait(async()=>{const s=await invoke('automation_status',{sessionId:report.sessionId});return s.mode==='computer_use'?s:null;},'authoritative Computer Use mode');
    // Observe the actual production cards. Deliberately never resolve them.
    const budget=window.__echoDesktopControlsActiveBudget((smoke?5:30)*60*1000,performance.now());
    let observing=false;
    const observe=async()=>{
      if(observing)return;
      observing=true;
      try {
        const pending=await invoke('agent_list_pending_interactions',{sessionId:report.sessionId});
        report.runtimePermissions=(pending.permissions||[]).map(p=>({requestId:p.requestId,title:p.title,
          toolCallId:p.toolCallId,visible:visible(document.getElementById(`permission-${p.requestId}`))}));
        report.desktopApprovalVisible=visible(document.querySelector('.perm-inline[data-approval-source="automation"]'));
        report.status=await invoke('automation_status',{sessionId:report.sessionId});
        report.awaitingUserResume=report.restoredExistingSession&&report.status.paused;
        const waiting=!!report.completion||report.status.paused||!!report.pendingApproval||report.desktopApprovalVisible||
          ['permissions','questions','planApprovals','folderTrustRequests'].some(k=>pending[k]?.length>0);
        report.activeBudget=budget(performance.now(),waiting);
        if(report.activeBudget.expired){
          clearInterval(timer);
          report.status=await invoke('automation_pause',{sessionId:report.sessionId,reason:'validation_watchdog'});
          report.deadlinePause={reason:'active_execution_budget_exhausted',at:new Date().toISOString()};
        }
        report.observedAt=new Date().toISOString();await save();
      } finally {observing=false;}
    };
    const timer=setInterval(()=>void observe().catch(e=>{report.errors.push(String(e));}),5000);
    await observe();
    await save();
  } catch(error){report.errors.push(String(error?.message||error));await save();}
};
