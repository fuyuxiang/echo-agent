// Real model + production Composer/Runtime/MCP/native input. The fixture only
// submits normal user messages, observes receipts, and writes the QA report.
window.__echoDesktopVisualFlow = async ({home,cwd,modelId}) => {
  if(window.__echoDesktopVisualStarted)return;window.__echoDesktopVisualStarted=true;
  const invoke=(command,args={})=>window.__TAURI_INTERNALS__.invoke(command,args);
  const directory=`${cwd}/native-control-fixtures`,file=`${directory}/canvas-sent`;
  const expected=`ECHO CANVAS ${home.split('-').at(-1)} 中文消息 🧪`;
  const report={version:1,kind:'real-model-opaque-gui',modelId,home,cwd,file,expected,tools:[],completions:[],errors:[],controls:[],desktopAcceptance:false,targetWrittenByFixture:false};
  const save=()=>invoke('write_text_file',{path:`${directory}/visual-flow-progress.json`,workspaceRoot:cwd,content:JSON.stringify(report,null,2)});
  const delay=ms=>new Promise(r=>setTimeout(r,ms));
  const visible=e=>e&&e.getClientRects().length>0;
  const wait=async(fn,label,ms=120000)=>{const until=Date.now()+ms;while(Date.now()<until){const v=await fn();if(v)return v;await delay(200);}throw Error(`${label}: timeout`)};
  const listen=async(event,fn)=>{const handler=window.__TAURI_INTERNALS__.transformCallback(fn);await invoke('plugin:event|listen',{event,target:{kind:'Any'},handler});};
  const submit=async text=>{
    const area=await wait(()=>{const e=document.querySelector('.echo-composer textarea');return visible(e)&&!e.disabled?e:null},'Composer ready');
    Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype,'value').set.call(area,text);area.dispatchEvent(new Event('input',{bubbles:true}));await delay(100);
    area.dispatchEvent(new KeyboardEvent('keydown',{key:'Enter',code:'Enter',bubbles:true,cancelable:true}));report.controls.push({text,at:new Date().toISOString()});await save();
  };
  try {
    report.context=await invoke('desktop_review_context');if(report.context.home!==home)throw Error('private home mismatch');
    await wait(async()=>{const a=await invoke('agent_auth_status');return a.ready&&a.runtimeReady&&a.synchronized},'Runtime ready');
    if((await invoke('agent_list_all_sessions',{includeArchived:true})).length)throw Error('fresh data required');
    await listen('agent://update',({payload})=>{
      if(report.sessionId&&payload.sessionId!==report.sessionId)return;
      const u=payload.update||payload,r=u.rawOutput;
      if(['completed','failed'].includes(u.status)&&r?.type==='MCP'&&r.server_name==='echoagent-automation') {
        let output;try{output=JSON.parse(r.output?.OkayOutput?.split('\n[image content')[0]);}catch{}
        const t={id:u.toolCallId,at:new Date().toISOString(),tool:r.tool_name,status:u.status,error:r.output?.Error,output};
        const i=report.tools.findIndex(x=>x.id===t.id);if(i<0)report.tools.push(t);else report.tools[i]=t;void save();
      }
    });
    await listen('agent://complete',({payload})=>{if(!report.sessionId||payload.sessionId===report.sessionId){report.completions.push({...payload,at:new Date().toISOString()});void save();}});
    (await wait(()=>{const e=document.querySelector('.echo-composer .model-selector__trigger');return visible(e)&&!e.disabled?e:null},'model selector')).click();
    (await wait(()=>[...document.querySelectorAll('.model-selector__item')].find(e=>visible(e)&&e.dataset.modelId===modelId),'requested model')).click();
    (await wait(()=>document.querySelector('.workspace-picker__trigger'),'workspace selector')).click();
    await invoke('write_text_file',{path:'controls-workspace-picker-ready.json',workspaceRoot:cwd,content:JSON.stringify({home,cwd,pid:report.context.pid})});
    (await wait(()=>[...document.querySelectorAll('button')].find(e=>visible(e)&&e.textContent.trim()==='选择文件夹…'),'folder picker')).click();
    await wait(()=>document.querySelector('.workspace-picker__trigger')?.getAttribute('aria-label')===`工作目录：${cwd}。点击切换`,'private native workspace');
    await submit('请在 Echo opaque canvas 测试应用里发条消息');
    report.sessionId=await wait(async()=>(await invoke('agent_list_all_sessions',{includeArchived:true})).find(s=>s.cwd===cwd)?.sessionId,'UI-created chat');
    await wait(()=>report.completions.length,'missing-text turn ends');
    report.missingTextStatus=await invoke('automation_status',{sessionId:report.sessionId});
    if(report.missingTextStatus.task?.reason!=='message_text_required'||report.missingTextStatus.computerAuthorized)throw Error('missing body was not gated');
    if(report.tools.some(t=>/^computer_/.test(t.tool)&&!t.error))throw Error('desktop accessed before message body');
    report.firstTurnToolCount=report.tools.length;
    const beforeContinue=report.completions.length;
    await submit('继续');
    await wait(()=>report.completions.length>beforeContinue,'bare continue remains gated');
    report.bareContinueStatus=await invoke('automation_status',{sessionId:report.sessionId});
    if(report.bareContinueStatus.task?.reason!=='message_text_required'||report.bareContinueStatus.computerAuthorized)throw Error('bare continue bypassed missing message body');
    if(report.tools.some(t=>/^computer_/.test(t.tool)&&!t.error))throw Error('desktop accessed before actual message body');
    if(report.bareContinueStatus.task?.taskId!==report.missingTextStatus.task?.taskId)throw Error('ordinary continue created a new task');
    const before=report.completions.length;
    await submit(`内容必须精确为 ${JSON.stringify(expected)}。只在刚才的 Echo opaque canvas 验收应用里操作，不要操作真实聊天软件。这个应用是已运行的自绘界面，只有一个白色输入框和 Send to fixture file 按钮，没有可用的原生文字控件。请通过真实桌面界面填写该内容并点击发送。这个按钮只会把内容存到本任务指定的临时文件 ${file}，不使用网络。不要使用 Shell、脚本或文件写入工具代替界面操作；允许只读核验 ${file} 的精确正文。请核验后结束任务。`);
    await wait(()=>report.completions.length>before,'actual GUI message task finishes',8*60*1000);
    report.finalStatus=await invoke('automation_status',{sessionId:report.sessionId});
    report.desktopAcceptance=report.finalStatus.task?.phase==='completed'&&report.tools.some(t=>t.tool==='computer_type'&&t.output?.typed===true)&&report.tools.filter(t=>t.tool==='computer_click'&&t.output?.clicked===true).length>=2&&report.tools.some(t=>t.tool==='computer_verify_file'&&t.output?.status==='verified_exact_text');
    if(!report.desktopAcceptance)throw Error('full GUI input/send/verification not completed');
  } catch(e){report.errors.push(String(e?.message||e));}
  report.finishedAt=new Date().toISOString();await save();
};
