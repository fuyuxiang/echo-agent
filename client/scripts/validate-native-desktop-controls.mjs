import { seedReviewModel } from './lib/native-review-model.mjs';
// Build first with --features tauri/custom-protocol; a dev-URL binary cannot run standalone.
// Actual app identity and Runtime evidence. --task-flow requires the independent
// verifier before claiming a completed desktop task.
import assert from 'node:assert/strict';
import { spawn, execFileSync } from 'node:child_process';
import { createReadStream, copyFileSync, mkdirSync, mkdtempSync, chmodSync, readFileSync, writeFileSync, existsSync, realpathSync, openSync, closeSync, lstatSync } from 'node:fs';
import { join, resolve, dirname } from 'node:path';
import { createHash, randomUUID } from 'node:crypto';
import { setTimeout as delay } from 'node:timers/promises';
import { cleanNativeReviewEnvironment } from './lib/native-review-isolation.mjs';
import { assertNativeReviewSessionUnlocked } from './lib/native-review-window.mjs';
import { verifyControlsJournal } from './lib/desktop-controls-evidence.mjs';
import { confirmNativeReviewWorkspace } from './lib/native-core-workspace-picker.mjs';
import { activateNativeReviewWindow } from './lib/native-review-window.mjs';

assert.equal(process.platform,'darwin','This runner qualifies an actual macOS app only');
assert([3,4].includes(process.argv.length),'Pass the executable built from current source and optional --journey, --smoke or --task-flow');
const opaque=process.argv[3]==='--opaque-flow';
const wechat=process.argv[3]==='--wechat-flow';
if(wechat)assert.equal(process.env.ECHO_DESKTOP_REVIEW_WECHAT_SEND,'1','Real WeChat flow sends a message: explicit user authorization is required');
const crossApp=process.argv[3]==='--universal-flow';
const taskFlow=crossApp||process.argv[3]==='--task-flow';
const smoke=process.argv[3]==='--smoke';
const journey=wechat||opaque||taskFlow||smoke||process.argv[3]==='--journey';
assert(process.argv.length===3||journey,'Only --journey, --smoke, --task-flow, --universal-flow, --opaque-flow or --wechat-flow is supported');
const source=realpathSync(resolve(process.argv[2]));
const home=realpathSync(mkdtempSync('/private/tmp/echo-native-review-controls-'));chmodSync(home,0o700);
const cwd=join(home,'workspaces','default'),fixtures=join(cwd,'native-control-fixtures');
mkdirSync(fixtures,{recursive:true,mode:0o700});
const marker=`CONTROL-SYNTHETIC-${randomUUID()}`;
const text=`${marker}\n中文与多行 😀\n特殊字符 <>&\"'\\ $()\n`;
writeFileSync(join(fixtures,'source.txt'),text,{mode:0o600});
const suffix=home.split('-').at(-1);
const taskText=`任务驱动电脑验收 ${suffix}\n中文输入、暂停与继续成功\n`;
const browserText=`通用桌面浏览器验收 ${suffix}`;
const browserPass=`ECHO-CROSS-PASS-${suffix}`;
writeFileSync(join(fixtures,'browser.html'),`<!doctype html><meta charset="utf-8"><title>Echo synthetic controls</title><h1>${marker}</h1><label>合成输入<textarea id="entry"></textarea></label><button id="copy">更新结果</button><pre id="result"></pre><label>授权测试文件<input id="file" type="file"></label><pre id="received"></pre><p id="status">等待真实输入和选取合成文件</p><script>
let received='';
const verify=()=>{if(document.querySelector('#result').textContent===${JSON.stringify(browserText)}&&received===${JSON.stringify(taskText)}){document.title=${JSON.stringify(browserPass)};document.querySelector('#status').textContent=${JSON.stringify(browserPass)};}};
document.querySelector('#copy').onclick=()=>{document.querySelector('#result').textContent=document.querySelector('#entry').value;verify();};
document.querySelector('#file').onchange=async e=>{const f=e.target.files[0];received=f?await f.text():'';document.querySelector('#received').textContent=f?f.name+'\\n'+received:'';verify();};
</script>`,{mode:0o600});
let opaquePid,opaqueApp;
if(opaque) {
  const binary=join(fixtures,'canvas-fixture');
  execFileSync('/usr/bin/swiftc',['scripts/fixtures/native-canvas-app.swift','-o',binary],{stdio:'pipe',timeout:120000});
  const app=join(fixtures,'Echo Opaque Canvas.app'),contents=join(app,'Contents');mkdirSync(join(contents,'MacOS'),{recursive:true});
  copyFileSync(binary,join(contents,'MacOS','fixture'));
  writeFileSync(join(contents,'Info.plist'),`<?xml version="1.0"?><plist version="1.0"><dict><key>CFBundleIdentifier</key><string>com.echoagent.fixture.real-model.${home.split('-').at(-1).toLowerCase()}</string><key>CFBundleExecutable</key><string>fixture</string><key>CFBundleName</key><string>Echo opaque canvas</string><key>CFBundlePackageType</key><string>APPL</string><key>NSHighResolutionCapable</key><true/></dict></plist>`);
  opaqueApp=app;
}
writeFileSync(join(home,'config.toml'),'[marketplace]\nofficial_marketplace_auto_installed = true\ndisabled_sources = []\n[claude_compat]\nimported = true\n',{mode:0o600});
let requestedModelId='echoagent-review/claude-opus-5-5';
const resource=process.env.ECHO_DESKTOP_REVIEW_CONTROLS_MODEL_RESOURCE;
if(resource){
  const file=lstatSync(resource),parent=lstatSync(dirname(resource));
  assert(file.isFile()&&!file.isSymbolicLink()&&file.size<16384&&realpathSync(resource)===resource&&!(file.mode&0o077)&&parent.isDirectory()&&!(parent.mode&0o077),'Model resource must be an ordinary owner-only file in a private directory');
  const m=JSON.parse(readFileSync(resource,'utf8'));
  assert(typeof m.modelId==='string'&&/^[A-Za-z0-9._/-]+$/.test(m.modelId));
  assert(typeof m.providerId==='string'&&/^[A-Za-z0-9._-]+$/.test(m.providerId));
  assert.deepEqual(Object.keys(m.model).sort(),['model','model_provider']);
  assert.equal(m.model.model_provider,m.providerId);
  assert.deepEqual(Object.keys(m.provider).sort(),['api_backend','api_key','auth_scheme','base_url']);
  assert(Object.values(m.model).every(v=>typeof v==='string')&&Object.values(m.provider).every(v=>typeof v==='string'));
  assert.equal(m.provider.api_backend,'chat_completions');assert.equal(m.provider.auth_scheme,'bearer');
  const url=new URL(m.provider.base_url);assert(url.protocol==='https:'&&!url.username&&!url.password&&!url.search&&!url.hash);
  const quote=JSON.stringify;
  const table=(name,values)=>'\n['+name+']\n'+Object.entries(values).map(([k,v])=>k+' = '+quote(v)+'\n').join('');
  const config=readFileSync(join(home,'config.toml'),'utf8')+
    table('models',{default:m.modelId})+table('model_providers.'+quote(m.providerId),m.provider)+table('model.'+quote(m.modelId),m.model);
  writeFileSync(join(home,'config.toml'),config,{mode:0o600});
  requestedModelId=m.modelId;
} else {
  requestedModelId=seedReviewModel(home);
}
const legacy=join(home,'empty-legacy');mkdirSync(legacy,{mode:0o700});
const bundle=join(home,'EchoAgent Controls.app'),contents=join(bundle,'Contents');
mkdirSync(join(contents,'MacOS'),{recursive:true});
const executable=join(contents,'MacOS','echoagent');copyFileSync(source,executable);chmodSync(executable,0o755);
writeFileSync(join(contents,'Info.plist'),`<?xml version="1.0"?><!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd"><plist version="1.0"><dict><key>CFBundleIdentifier</key><string>com.echoagent.desktop.controls-review</string><key>CFBundleExecutable</key><string>echoagent</string><key>CFBundleName</key><string>EchoAgent Controls</string><key>CFBundlePackageType</key><string>APPL</string><key>NSHighResolutionCapable</key><true/><key>LSMinimumSystemVersion</key><string>14.0</string></dict></plist>`);
const binaryHash=createHash("sha256");
for await(const chunk of createReadStream(source))binaryHash.update(chunk);
const report={version:1,kind:'native-controls-driver',home,cwd,fixtures,bundle,executable,marker,
  sourceBinarySha256:binaryHash.digest('hex'),requestedModelId,desktopAcceptance:false,runtimeVerified:false,errors:[]};
writeFileSync(join(home,'driver.json'),JSON.stringify(report,null,2),{mode:0o600});
console.log(`Isolated controls acceptance artifacts: ${home}`);
let child;
try {
  assertNativeReviewSessionUnlocked();
  if(opaque) {
    execFileSync('/usr/bin/open',['-a',opaqueApp,'--args',fixtures,'canvas']);
    const until=Date.now()+10000;while(!existsSync(join(fixtures,'canvas-pid'))&&Date.now()<until)await delay(100);
    opaquePid=Number(readFileSync(join(fixtures,'canvas-pid'),'utf8'));
    assert(Number.isSafeInteger(opaquePid)&&opaquePid>1);
  }
  execFileSync('/usr/bin/codesign',['--force','--sign','-',bundle],{stdio:'pipe'});
  execFileSync('/usr/bin/codesign',['--verify',bundle],{stdio:'pipe'});
  const signedHash=createHash('sha256');for await(const chunk of createReadStream(executable))signedHash.update(chunk);
  report.binarySha256=signedHash.digest('hex');
  const environment={...cleanNativeReviewEnvironment(process.env),GROK_HOME:legacy,ECHO_DESKTOP_REVIEW:'1',ECHO_DESKTOP_REVIEW_CONTROLS:'1',ECHO_DESKTOP_REVIEW_CONTROLS_MODEL_ID:requestedModelId};
  delete environment.ECHO_AGENT_HOME;
  delete environment.ECHO_DESKTOP_REVIEW_CONTROLS_MODEL_RESOURCE;
  if(journey) {
    environment.ECHO_DESKTOP_REVIEW_CONTROLS_JOURNEY='1';
    if(smoke)environment.ECHO_DESKTOP_REVIEW_CONTROLS_SMOKE='1';
    if(taskFlow)environment.ECHO_DESKTOP_REVIEW_CONTROLS_TASK_FLOW='1';
    if(opaque)environment.ECHO_DESKTOP_REVIEW_CONTROLS_OPAQUE='1';
    if(wechat){environment.ECHO_DESKTOP_REVIEW_CONTROLS_WECHAT='1';environment.ECHO_DESKTOP_REVIEW_WECHAT_SEND='1';}
    if(crossApp)environment.ECHO_DESKTOP_REVIEW_CONTROLS_CROSS_APP='1';
    const fd=openSync(join(home,'app.log'),'a',0o600);
    child=spawn(executable,['--data-home',home],{env:environment,detached:true,stdio:['ignore',fd,fd]});
    closeSync(fd);child.unref();
    Object.assign(report,{pid:child.pid,journey:true,smoke,crossApp,wechat,approvalPolicy:taskFlow||opaque||wechat?'direct-user-task':'human-selected-task-or-action',progress:join(fixtures,wechat?'wechat-flow-progress.json':opaque?'visual-flow-progress.json':taskFlow?'task-flow-progress.json':'journey-progress.json')});
    writeFileSync(join(home,'driver.json'),JSON.stringify(report,null,2),{mode:0o600});
    if(taskFlow||opaque||wechat) {
      const ready=join(cwd,'controls-workspace-picker-ready.json'),deadline=Date.now()+120000;
      let picked=false;
      while(!picked&&child.exitCode===null&&child.signalCode===null&&Date.now()<deadline) {
        activateNativeReviewWindow(child.pid);
        if(existsSync(ready)) {
          const value=JSON.parse(readFileSync(ready,'utf8'));
          assert.deepEqual(value,{home,cwd,pid:child.pid});
          picked=confirmNativeReviewWorkspace({pid:child.pid,home,cwd,kind:'controls'});
        }
        if(!picked)await delay(500);
      }
      assert(picked,'The owned native Computer Use workspace picker did not finish');
      report.osPickerVerified=true;
      writeFileSync(join(home,'driver.json'),JSON.stringify(report,null,2),{mode:0o600});
    }
    if(wechat) {
      const until=Date.now()+12*60*1000;let actual;
      while(Date.now()<until&&child.exitCode===null&&child.signalCode===null){
        if(existsSync(report.progress)){try{actual=JSON.parse(readFileSync(report.progress,'utf8'));}catch{}if(actual?.finishedAt)break;}
        await delay(500);
      }
      assert(actual?.finishedAt,'Real WeChat flow did not finish; inspect before any repeat');report.actual=actual;
      const journal=join(home,'sessions',encodeURIComponent(cwd),actual.sessionId,'updates.jsonl');
      const entries=readFileSync(journal,'utf8').trim().split('\n').map(line=>JSON.parse(line));
      const decoded=entries.map(r=>r.params?.update?.rawOutput).filter(r=>r?.type==='MCP'&&r.server_name==='echoagent-automation').flatMap(r=>{try{return [{tool:r.tool_name,value:JSON.parse(r.output?.OkayOutput?.split('\n[image content')[0])}];}catch{return [];}});
      const wechatProofs=decoded.filter(r=>r.tool==='computer_verify_visual'&&r.value.verified===true&&r.value.window?.applicationIdentifier==='com.tencent.xinWeChat');
      report.runtimeVerified=wechatProofs.some(r=>r.value.checks?.some(c=>c.text===actual.text&&c.matched))&&wechatProofs.some(r=>r.value.checks?.some(c=>c.text===actual.recipient&&c.matched));
      report.desktopAcceptance=actual.desktopAcceptance===true&&report.runtimeVerified;
      report.finishedAt=new Date().toISOString();writeFileSync(join(home,'driver.json'),JSON.stringify(report,null,2),{mode:0o600});
      assert.deepEqual(actual.errors,[]);assert(report.desktopAcceptance,'WeChat send is not verified');
      console.log(JSON.stringify({passed:true,home,modelId:requestedModelId,kind:'real-model-wechat-send',desktopAcceptance:true}));
      child.kill('SIGTERM');process.exit(0);
    }
    if(opaque) {
      const until=Date.now()+10*60*1000;let actual;
      while(Date.now()<until&&child.exitCode===null&&child.signalCode===null) {
        if(existsSync(report.progress)){try{actual=JSON.parse(readFileSync(report.progress,'utf8'));}catch{}if(actual?.finishedAt)break;}
        await delay(500);
      }
      assert(actual?.finishedAt,'Opaque real-model flow did not finish');
      report.actual=actual;
      assert.deepEqual(actual.errors,[]);assert.equal(actual.desktopAcceptance,true);
      assert.equal(readFileSync(actual.file,'utf8'),actual.expected,'GUI output differs from requested message');
      assert.equal(actual.targetWrittenByFixture,false);assert.equal(actual.context.pid,child.pid);
      const journal=join(home,'sessions',encodeURIComponent(cwd),actual.sessionId,'updates.jsonl');
      const entries=readFileSync(journal,'utf8').trim().split('\n').map(line=>JSON.parse(line));
      const outputs=entries.map(r=>r.params?.update?.rawOutput).filter(r=>r?.type==='MCP'&&r.server_name==='echoagent-automation');
      const decoded=outputs.flatMap(r=>{try{return [{tool:r.tool_name,value:JSON.parse(r.output?.OkayOutput?.split('\n[image content')[0])}];}catch{return [];}});
      assert(decoded.some(r=>r.tool==='computer_type'&&r.value.typed===true),'production Runtime did not execute native text input');
      assert(decoded.some(r=>r.tool==='automation_complete'&&r.value.task?.phase==='completed'),'production Runtime did not complete with host evidence');
      assert.equal(actual.bareContinueStatus.task.reason,'message_text_required');
      assert.equal(actual.bareContinueStatus.computerAuthorized,false);
      assert.equal(actual.missingTextStatus.task.taskId,actual.finalStatus.task.taskId);
      report.desktopAcceptance=true;report.runtimeVerified=true;report.finishedAt=new Date().toISOString();
      writeFileSync(join(home,'driver.json'),JSON.stringify(report,null,2),{mode:0o600});
      child.kill('SIGTERM');
      const comm=execFileSync('/bin/ps',['-p',String(opaquePid),'-o','comm='],{encoding:'utf8'}).trim();
      assert.equal(realpathSync(comm),realpathSync(join(fixtures,'Echo Opaque Canvas.app','Contents','MacOS','fixture')));
      process.kill(opaquePid,'SIGTERM');
      console.log(JSON.stringify({passed:true,home,modelId:requestedModelId,kind:'real-model-production-opaque-gui',desktopAcceptance:true}));
      process.exit(0);
    }
    console.log(JSON.stringify({home,bundle,pid:child.pid,progress:report.progress,approvalPolicy:report.approvalPolicy}));
    process.exit(0);
  }
  child=spawn(executable,['--data-home',home],{env:environment,stdio:['ignore','pipe','pipe']});
  let logs='';const capture=bytes=>{if(logs.length<2*1024*1024)logs+=String(bytes);};child.stdout.on('data',capture);child.stderr.on('data',capture);
  const deadline=Date.now()+270000;
  while(!existsSync(join(home,'native-review.json'))&&child.exitCode===null&&Date.now()<deadline)await delay(500);
  writeFileSync(join(home,'app.log'),logs,{mode:0o600});
  if(!existsSync(join(home,'native-review.json')))throw new Error(`No real app report; exit=${child.exitCode}, signal=${child.signalCode}`);
  const actual=JSON.parse(readFileSync(join(home,'native-review.json'),'utf8'));
  assert.equal(actual.context?.pid,child.pid,'Report must originate from this app process');
  assert.equal(actual.home,home);report.actual=actual;
  for(let i=0;i<240&&child.exitCode===null;i++)await delay(250);
  const journal=join(home,'sessions',encodeURIComponent(cwd),actual.sessionId,'updates.jsonl');
  report.journalVerification=verifyControlsJournal(readFileSync(journal),actual.sessionId);
  report.runtimeVerified=actual.runtimeVerified===true&&report.journalVerification.runtimeVerified;
  report.exitCode=child.exitCode;
} catch(error){report.errors.push(String(error?.message||error));}
finally {
  if(opaquePid) {
    try{const comm=execFileSync('/bin/ps',['-p',String(opaquePid),'-o','comm='],{encoding:'utf8'}).trim();if(realpathSync(comm)===realpathSync(join(fixtures,'Echo Opaque Canvas.app','Contents','MacOS','fixture')))process.kill(opaquePid,'SIGTERM');}catch{}
  }
  if(child&&child.exitCode===null){child.kill('SIGTERM');report.interrupted=true;}
  report.finishedAt=new Date().toISOString();
  writeFileSync(join(home,'driver.json'),JSON.stringify(report,null,2),{mode:0o600});
}
console.log(JSON.stringify({home,bundle,runtimeVerified:report.runtimeVerified,desktopAcceptance:false,errors:report.errors}));
process.exitCode=report.runtimeVerified?0:1;
