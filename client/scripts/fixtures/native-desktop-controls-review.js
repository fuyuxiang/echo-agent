// Read-only qualification through the real app IPC, Runtime, and MCP service.
// Does not approve desktop writes, synthesize tools, or claim task acceptance.
window.__echoDesktopControlsReview = async ({ home, cwd }) => {
  if (window.__echoDesktopControlsStarted) return;
  window.__echoDesktopControlsStarted = true;
  const invoke = (command,args={}) => window.__TAURI_INTERNALS__.invoke(command,args);
  const report = {version:1,kind:'native-desktop-controls',passed:false,runtimeVerified:false,
    desktopAcceptance:false,home,cwd,checks:[],errors:[],toolTitles:[]};
  const completions=[];const listeners=[];const tools=new Map();let markerSeen=false;let replyTail="";let sessionId;
  const wait = async (fn,label,ms=60000) => {
    const deadline=Date.now()+ms;
    while(Date.now()<deadline){const value=await fn();if(value)return value;await new Promise(resolve=>setTimeout(resolve,250));}
    throw new Error(`${label}: timeout`);
  };
  const listen=async(event,fn)=>{
    const handler=window.__TAURI_INTERNALS__.transformCallback(fn);
    const eventId=await invoke('plugin:event|listen',{event,target:{kind:'Any'},handler});
    listeners.push({event,eventId});
  };
  try {
    report.context=await invoke('desktop_review_context');
    if(report.context.home!==home)throw new Error('isolated home mismatch');
    report.capabilities=await invoke('automation_capabilities');
    report.checks.push({id:'actual-app-permissions',result:report.capabilities.computer.inputControl&&report.capabilities.computer.screenCapture?'passed':'blocked',evidence:report.capabilities.computer});
    await wait(async()=>{const auth=await invoke('agent_auth_status');return auth.ready&&auth.runtimeReady&&auth.synchronized;},'real Runtime readiness',90000);
    await listen('agent://complete',({payload})=>completions.push(payload));
    await listen('agent://update',({payload})=>{
      if(payload.sessionId!==sessionId)return;
      const u=payload.update||payload;
      if(u.sessionUpdate==='agent_message_chunk') {
        replyTail=(replyTail+(u.content?.text||'')).slice(-512);
        if(replyTail.includes('CONTROL-RUNTIME-READONLY'))markerSeen=true;
      }
      if(!['tool_call','tool_call_update'].includes(u.sessionUpdate))return;
      const id=u.toolCallId||u.id;if(!id)return;
      const previous=tools.get(id)||{id};
      const raw=u.rawOutput;
      if(raw?.type==='MCP'&&raw.server_name==='echoagent-automation')previous.name=raw.tool_name;
      if(u.status==='completed')previous.completed=true;
      if(raw?.type==='MCP'&&raw.server_name==='echoagent-automation'&&typeof raw.output?.OkayOutput==='string') {
        try {const output=JSON.parse(raw.output.OkayOutput);
          if((raw.tool_name==='computer_list_apps'||raw.tool_name==='computer_continue')&&output.sourceTool==='computer_list_apps') {
            previous.nativePage={resultId:output.resultId,totalItems:output.totalItems,refs:output.items.map(n=>n.ref),finished:output.nextCursor===null};
          }
        } catch {previous.invalidJson=true;}
      }
      tools.set(id,previous);
    });
    sessionId=await invoke('agent_new_session',{cwd,modelId:'echoagent-ojlab/Echo Reed',permissionMode:'ask'});
    report.sessionId=sessionId;
    await invoke('set_coding_mode',{sessionId,mode:'computer_use'});
    const promptId=`controls-${Date.now()}`;
    await invoke('agent_send',{sessionId,promptId,text:'本次是只读工具接入验收。请通过 echoagent-automation MCP 实际调用 computer_capabilities，然后 computer_list_apps，若 nextCursor 非空必须用 computer_continue 取完所有页（禁止使用终端命令），根据实际 totalItems 报告应用总数，然后调用 automation_pause。只允许为这些工具进行工具发现。不要输入、点击、打开应用、保存文件或访问其他数据。最后用中文报告实际结果，回复结尾标记 CONTROL-RUNTIME-READONLY。',attachments:[]});
    const completed=await wait(()=>completions.find(e=>e.sessionId===sessionId&&e.promptId===promptId),'actual model task completion',180000);
    report.completion=completed;
    // Verify real completed tool/result events; prompt text alone is no evidence.
    report.tools=[...tools.values()];report.replyMarkerSeen=markerSeen;
    const pages=report.tools.filter(t=>t.completed&&t.nativePage).map(t=>t.nativePage);
    const first=pages[0];const refs=new Set(pages.filter(p=>p.resultId===first?.resultId).flatMap(p=>p.refs));
    report.runtimeVerified=!!first&&pages.some(p=>p.resultId===first.resultId&&p.finished)&&refs.size===first.totalItems&&markerSeen&&completed.stopReason==='end_turn';
    report.checks.push({id:'real-runtime-new-tool-roundtrip',result:report.runtimeVerified?'passed':'failed'});
    report.passed=report.runtimeVerified;
  } catch(error){report.errors.push(String(error?.message||error));}
  finally {
    if(sessionId){try{await invoke('automation_pause',{sessionId});}catch{}}
    for(const args of listeners){try{await invoke('plugin:event|unlisten',args);}catch{}}
    report.finishedAt=new Date().toISOString();
    await invoke('desktop_review_complete',{report});
  }
};
