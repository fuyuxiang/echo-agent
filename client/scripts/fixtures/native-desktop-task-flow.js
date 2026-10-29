// Real MiniMax acceptance through production Composer only. This fixture never
// grants consent, resumes/cancels through IPC, writes the target document, or
// resolves approvals. The ONLY controls are the user's normal text submissions.
// Accept complete native provider readbacks across valid observation tools.
// A pre-existing document must never count before this task actually writes.
window.__echoTaskFlowObserveBody = (report, entry) => {
  const o=entry.output;
  if(!o)return;
  if(entry.tool==='computer_set_value'&&o.executed===true&&o.after?.role==='AXTextArea')report.bodyWrite={id:entry.id,at:entry.at};
  if(report.bodyReadback||!report.bodyWrite)return;
  const record=via=>{report.bodyReadback={id:entry.id,at:entry.at,text:report.expected,via};};
  if(entry.tool==='computer_read'&&o.text===report.expected&&!o.truncated&&(o.offset??0)===0)record('computer_read');
  if(entry.tool==='computer_set_value'&&o.verified===true&&o.after?.role==='AXTextArea'&&o.after.value===report.expected&&!o.after.textTruncated)record('verified_native_provider_readback');
  if(['computer_snapshot','computer_find','computer_continue'].includes(entry.tool)&&[...(o.nodes||[]),...(o.candidates||[])].some(n=>n.role==='AXTextArea'&&n.value===report.expected&&!n.textTruncated))record('observed_native_provider_text');
};
window.__echoDesktopTaskFlow = async ({home,cwd,modelId,crossApp=false}) => {
  if(window.__echoDesktopTaskFlowStarted)return;
  window.__echoDesktopTaskFlowStarted=true;
  const invoke=(command,args={})=>window.__TAURI_INTERNALS__.invoke(command,args);
  const directory=`${cwd}/native-control-fixtures`,file=`${directory}/task-flow-output.txt`;
  const expected=`任务驱动电脑验收 ${home.split('-').at(-1)}\n中文输入、暂停与继续成功\n`;
  const report={version:2,kind:'desktop-task-flow',home,cwd,modelId,file,expected,
    tools:[],approvals:[],runtimePermissions:[],completions:[],controls:[],errors:[],desktopAcceptance:false,
    targetWrittenByFixture:false,approvalResolvedByFixture:false,startedAt:new Date().toISOString()};
  const save=()=>invoke('write_text_file',{path:`${directory}/task-flow-progress.json`,content:JSON.stringify(report,null,2),workspaceRoot:cwd});
  const delay=ms=>new Promise(r=>setTimeout(r,ms));
  const visible=e=>e&&e.getClientRects().length>0;
  const wait=async(fn,label,ms=120000)=>{const end=Date.now()+ms;while(Date.now()<end){const result=await fn();if(result)return result;await delay(200);}throw Error(`${label}: timeout`);};
  const listen=async(event,fn)=>{const handler=window.__TAURI_INTERNALS__.transformCallback(fn);await invoke('plugin:event|listen',{event,target:{kind:'Any'},handler});};
  const submit=async text=>{
    const area=await wait(()=>{const e=document.querySelector('.echo-composer textarea');return visible(e)&&!e.disabled?e:null;},`Composer available for ${text.slice(0,12)}`);
    Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype,'value').set.call(area,text);
    area.dispatchEvent(new Event('input',{bubbles:true}));
    await delay(100);
    area.dispatchEvent(new KeyboardEvent('keydown',{key:'Enter',code:'Enter',bubbles:true,cancelable:true}));
    report.controls.push({text,at:new Date().toISOString()});await save();
  };
  try {
    report.context=await invoke('desktop_review_context');
    if(report.context.home!==home)throw Error('private acceptance home mismatch');
    await wait(async()=>{const a=await invoke('agent_auth_status');return a.ready&&a.runtimeReady&&a.synchronized;},'Runtime ready');
    if((await invoke('agent_list_all_sessions',{includeArchived:true})).length)throw Error('Task-flow requires fresh acceptance data');
    await listen('automation://approval',({payload})=>{report.approvals.push(payload);void save();});
    await listen('agent://permission',({payload})=>{report.runtimePermissions.push(payload);void save();});
    await listen('agent://update',({payload})=>{
      if(report.sessionId&&payload.sessionId!==report.sessionId)return;
      const u=payload.update||payload,r=u.rawOutput;
      if(['completed','failed'].includes(u.status)&&r?.type==='MCP'&&r.server_name==='echoagent-automation'){
        let output;try{output=JSON.parse(r.output?.OkayOutput);}catch{}
        const entry={id:u.toolCallId,at:new Date().toISOString(),tool:r.tool_name,status:u.status,output,error:r.output?.Error};
        const index=report.tools.findIndex(t=>t.id===entry.id);if(index<0)report.tools.push(entry);else report.tools[index]=entry;
        window.__echoTaskFlowObserveBody(report,entry);
        void save();
      }
    });
    await listen('agent://complete',({payload})=>{if(!report.sessionId||payload.sessionId===report.sessionId){report.completions.push({...payload,at:new Date().toISOString()});void save();}});
    (await wait(()=>{const e=document.querySelector('.echo-composer .model-selector__trigger');return visible(e)&&!e.disabled?e:null;},'model chooser')).click();
    (await wait(()=>[...document.querySelectorAll('.model-selector__item')].find(e=>visible(e)&&e.dataset.modelId===modelId),'configured MiniMax model')).click();
    // No + menu, computer mode toggle, approval click, or dedicated resume button.
    const prompt=`请在 TextEdit（系统文本编辑）中先执行一次“新建”文稿命令，创建一个独立的纯文本文稿；不要把打开应用自动出现的空文稿当成本次新建。正文必须精确等于这个 JSON 字符串解码后的内容：${JSON.stringify(expected)}。其中有真正换行，末尾也有一个换行，共 ${[...expected].length} 个 Unicode 字符。请通过真实桌面控件输入并读回核对，然后用系统保存面板保存到 ${file}，文件名为 task-flow-output.txt。只操作这份新文稿，不修改已有文稿；已有文稿即使正文相似也不能复用；如果已有保存或前往面板阻挡，可以取消面板但不关闭或改动已有文稿。禁止 Shell、脚本和文件写工具代替真实桌面输入与保存；允许通过只读结果核验确认落盘。流程中我可能说“停一下”和“继续保存”，请保留现场并在同一任务续做。完成前核对真实文档位置和文件内容，不能只凭窗口标题或工具派发成功报告完成。`;
    await submit(prompt);
    report.sessionId=await wait(async()=>(await invoke('agent_list_all_sessions',{includeArchived:true})).find(s=>s.cwd===cwd)?.sessionId,'UI-created task');
    report.model=await wait(async()=>{const s=(await invoke('agent_list_all_sessions',{includeArchived:true})).find(s=>s.sessionId===report.sessionId);return s?.currentModelId===modelId||s?.currentModelId===modelId.split('/').at(-1)?s.currentModelId:null;},'actual configured model');
    report.initialStatus=await wait(async()=>{const s=await invoke('automation_status',{sessionId:report.sessionId});return s.mode==='computer_use'&&s.computerAuthorized&&!s.paused&&s.task?.authorized&&s.task?.scope===prompt?s:null;},'user-task automatic admission');await save();
    await wait(()=>report.bodyReadback,'real exact input and readback',25*60*1000);
    await submit('停一下');
    report.pausedStatus=await wait(async()=>{const s=await invoke('automation_status',{sessionId:report.sessionId});return s.paused&&!s.computerAuthorized&&s.task?.reason==='user_paused'?s:null;},'natural user pause');
    await wait(()=>report.completions.some(c=>c.cancelTrigger==='pause'&&c.at>=report.controls[1].at),'model cancellation acknowledged');
    await wait(()=>{const e=document.querySelector('.echo-composer textarea');return visible(e)&&!e.disabled;},'paused Composer accepts natural continuation');
    report.pauseSettledAt=new Date().toISOString();
    const physical=new Set(['computer_file_dialog','computer_open_app','computer_activate_window','computer_set_value','computer_invoke','computer_key_window','computer_key','computer_type','computer_click','computer_click_control','computer_drag','computer_scroll','computer_move']);
    const writes=()=>report.tools.filter(t=>physical.has(t.tool)&&t.status==='completed'&&!t.error).map(t=>t.id);
    const before=writes();await delay(5000);report.pauseQuiet={before,after:writes(),durationMs:5000};
    if(JSON.stringify(before)!==JSON.stringify(writes()))throw Error('physical write completed after pause acknowledgement');
    await submit('继续保存');
    report.resumedStatus=await wait(async()=>{const s=await invoke('automation_status',{sessionId:report.sessionId});return !s.paused&&s.computerAuthorized&&s.task?.generation>report.pausedStatus.task.generation?s:null;},'natural continuation same task');
    report.resumedAt=new Date().toISOString();await save();
    await wait(()=>report.completions.some(c=>c.at>report.resumedAt),'continued model turn completed',25*60*1000);
    report.finalStatus=await invoke('automation_status',{sessionId:report.sessionId});
    report.finishedAt=new Date().toISOString();await save();
    if(crossApp){
      const text=`通用桌面浏览器验收 ${home.split('-').at(-1)}`,pass=`ECHO-CROSS-PASS-${home.split('-').at(-1)}`;
      report.crossApp={startedAt:new Date().toISOString(),toolStart:report.tools.length,text,pass};
      await submit(`请用真实桌面操作在 Safari 新建一个独立标签页；如果上轮合成测试遗留的文件选择或前往面板阻挡，先取消面板、保留原标签页，再执行新建标签页命令。然后打开本机合成测试页 file://${directory}/browser.html。在“合成输入”多行文本框输入精确正文 ${JSON.stringify(text)}，点击“更新结果”。再通过网页的“授权测试文件”文件选择器，选择我刚才让你保存的 ${file}。这是本机合成页面，不向网络传文件。读回页面显示的输入结果和所选文件内容；页面会在两项都正确时显示 ${pass}，请在真实界面核对该标记后完成。只操作这个新标签页；禁止 Shell、脚本、浏览器自动化/CDP 或在地址栏执行代码，必须复用电脑原生控件/键盘与必要的截图能力。`);
      await wait(()=>report.completions.some(c=>c.at>report.crossApp.startedAt),'cross-app model turn completed',15*60*1000);
      report.crossApp.finishedAt=new Date().toISOString();await save();
    }
  }catch(error){report.errors.push(String(error?.message||error));report.failedAt=new Date().toISOString();await save();}
};
