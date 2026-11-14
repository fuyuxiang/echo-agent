// Explicit real-send acceptance. Only normal user messages are submitted here;
// the actual model and production Runtime/MCP must operate the real WeChat UI.
window.__echoDesktopWechatFlow = async ({home,cwd,modelId}) => {
  if(window.__echoDesktopWechatStarted)return;window.__echoDesktopWechatStarted=true;
  const invoke=(command,args={})=>window.__TAURI_INTERNALS__.invoke(command,args);
  const directory=`${cwd}/native-control-fixtures`,recipient='文件传输助手',text='EchoAgent 微信端到端测试，请忽略。';
  const report={version:1,kind:'real-model-wechat-send',home,cwd,modelId,recipient,text,tools:[],completions:[],errors:[],desktopAcceptance:false};
  const save=()=>invoke('write_text_file',{path:`${directory}/wechat-flow-progress.json`,workspaceRoot:cwd,content:JSON.stringify(report,null,2)});
  const delay=ms=>new Promise(r=>setTimeout(r,ms));
  const visible=e=>e&&e.getClientRects().length>0;
  const wait=async(fn,label,ms=120000)=>{const until=Date.now()+ms;while(Date.now()<until){const v=await fn();if(v)return v;await delay(200);}throw Error(`${label}: timeout`)};
  const listen=async(event,fn)=>{const handler=window.__TAURI_INTERNALS__.transformCallback(fn);await invoke('plugin:event|listen',{event,target:{kind:'Any'},handler});};
  try {
    report.context=await invoke('desktop_review_context');if(report.context.home!==home)throw Error('private home mismatch');
    await wait(async()=>{const a=await invoke('agent_auth_status');return a.ready&&a.runtimeReady&&a.synchronized},'Runtime ready');
    if((await invoke('agent_list_all_sessions',{includeArchived:true})).length)throw Error('fresh chat required; never auto-replay a send');
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
    (await wait(()=>[...document.querySelectorAll('.model-selector__item')].find(e=>visible(e)&&e.dataset.modelId===modelId),'configured model')).click();
    (await wait(()=>document.querySelector('.workspace-picker__trigger'),'workspace selector')).click();
    await invoke('write_text_file',{path:'controls-workspace-picker-ready.json',workspaceRoot:cwd,content:JSON.stringify({home,cwd,pid:report.context.pid})});
    (await wait(()=>[...document.querySelectorAll('button')].find(e=>visible(e)&&e.textContent.trim()==='选择文件夹…'),'folder picker')).click();
    await wait(()=>document.querySelector('.workspace-picker__trigger')?.getAttribute('aria-label')===`工作目录：${cwd}。点击切换`,'native workspace');
    const prompt=`请打开微信应用，给“${recipient}”发送且只发送一次消息，正文必须精确为 ${JSON.stringify(text)}。用户已明确授权这条测试消息。只操作微信中的这个会话，不给其他联系人发送消息，不修改其他聊天内容，不使用 Shell、脚本、数据库、AppleScript 或文件写入代替桌面界面操作。请先确认接收方标题，再填写正文；发送前检查输入正确。发送后检查正文位于聊天记录的已发送气泡中，输入框已清空，且没有发送失败标记。使用 computer_verify_visual 分别核验会话标题区域和消息记录区域的正文；消息区域必须排除输入框和联系人列表，不能用草稿作为已发送证据。用发送之后的宿主核验凭证完成任务。若发送结果不明，先观察，不要重复发送；只核验已有结果。`;
    const area=await wait(()=>{const e=document.querySelector('.echo-composer textarea');return visible(e)&&!e.disabled?e:null},'Composer ready');
    Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype,'value').set.call(area,prompt);area.dispatchEvent(new Event('input',{bubbles:true}));await delay(100);
    area.dispatchEvent(new KeyboardEvent('keydown',{key:'Enter',code:'Enter',bubbles:true,cancelable:true}));report.prompt=prompt;report.submittedAt=new Date().toISOString();await save();
    report.sessionId=await wait(async()=>(await invoke('agent_list_all_sessions',{includeArchived:true})).find(s=>s.cwd===cwd)?.sessionId,'normal UI chat');
    await wait(()=>report.completions.length,'WeChat task finishes',10*60*1000);
    report.finalStatus=await invoke('automation_status',{sessionId:report.sessionId});
    report.desktopAcceptance=report.finalStatus.task?.phase==='completed'&&report.tools.some(t=>t.tool==='computer_verify_visual'&&t.output?.verified===true&&t.output?.checks?.some(c=>c.text===text&&c.matched));
    if(!report.desktopAcceptance)throw Error('real WeChat task did not complete with visual host evidence');
  } catch(e){report.errors.push(String(e?.message||e));}
  report.finishedAt=new Date().toISOString();await save();
};
