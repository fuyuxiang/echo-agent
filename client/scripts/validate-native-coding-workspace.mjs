import assert from 'node:assert/strict';
import { spawn, spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { existsSync, lstatSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, realpathSync, writeFileSync } from 'node:fs';
import { dirname, isAbsolute, join, relative, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { setTimeout as delay } from 'node:timers/promises';
import vm from 'node:vm';
import { cleanNativeReviewEnvironment } from './lib/native-review-isolation.mjs';
import { captureReviewModelLogs } from './lib/native-review-model.mjs';
import { assertNativeReviewSessionUnlocked, activateNativeReviewWindow } from './lib/native-review-window.mjs';

const hash = bytes => createHash('sha256').update(bytes).digest('hex');
const completedActualTools = updates => {
  const tools = new Map();let promptOrdinal = 0;
  for (const envelope of updates) {
    const update = envelope.params?.update ?? envelope.params ?? envelope;
    if (update?.sessionUpdate === 'user_message_chunk') promptOrdinal++;
    if (!['tool_call','tool_call_update'].includes(update?.sessionUpdate) || typeof update.toolCallId !== 'string') continue;
    const key = `${promptOrdinal}:${update.toolCallId}`, previous = tools.get(key) ?? {};
    tools.set(key,{...previous,...update,rawInput:update.rawInput ?? previous.rawInput});
  }
  return [...tools.values()].filter(tool=>tool.status==='completed');
};
const safeSource = value => typeof value==='string'&&value.length<=4096&&value.includes('UNSAVED_A01')
  && /^\s*exports\s*\.\s*double\s*=\s*n\s*=>\s*n\s*\*\s*2\s*;\s*$/.test(value.replace(/\/\*[\s\S]*?\*\/|\/\/[^\r\n\u2028\u2029]*/g,' '));
const inside = (root,path) => { const rel=relative(root,path); return rel!==''&&rel!=='..'&&!rel.startsWith('../')&&!isAbsolute(rel); };
const seed = home => {
  const cwd=join(home,'workspaces/default'),other=join(home,'workspaces/other');
  mkdirSync(cwd,{recursive:true,mode:0o700});mkdirSync(other,{recursive:true,mode:0o700});
  const files = {
    'package.json':JSON.stringify({name:'echo-native-workspace',private:true,version:'1.0.0',scripts:{test:'node --test test.cjs',lint:'node --check double.cjs'}},null,2)+'\n',
    'double.cjs':'exports.double = n => n + 1;\n',
    'completion.cjs':'// User inline completion scratch file.\n',
    'test.cjs':"const test=require('node:test');const assert=require('node:assert/strict');const {double}=require('./double.cjs');test('double real result',()=>assert.equal(double(2),4));\n",
  };
  for(const [file,source]of Object.entries(files))writeFileSync(join(cwd,file),source,{flag:'wx',mode:0o600});
  writeFileSync(join(other,'package.json'),JSON.stringify({name:'echo-isolated-other',private:true,scripts:{test:'node --test other.cjs'}})+'\n',{flag:'wx',mode:0o600});
  writeFileSync(join(other,'other.cjs'),"// Independent workspace sentinel.\nrequire('node:assert/strict').equal(7,7);\n",{flag:'wx',mode:0o600});
  mkdirSync(join(home,'empty-legacy'),{mode:0o700});
  writeFileSync(join(home,'config.toml'),'[permission]\ndeny = ["WebFetch", "WebSearch", "MCPTool"]\nask = ["*"]\n',{flag:'wx',mode:0o600});
  return {cwd,other,files,otherHash:hash(readFileSync(join(other,'other.cjs')))};
};
const actualNode = (cwd,args) => {
  const result=spawnSync(process.execPath,args,{cwd,env:cleanNativeReviewEnvironment(process.env),encoding:'utf8',timeout:15000,maxBuffer:1024*1024});
  assert(!result.error&&result.signal===null,'Actual local Node subprocess unavailable');
  return {command:`node ${args.join(' ')}`,exitCode:result.status,stdout:result.stdout,stderr:result.stderr};
};
const parseActualAskSnapshot=(text,cwd)=>{
  if(!text.includes('[回声代码 · ask]')||!text.includes('只读解释当前所选函数'))return null;
  const marker='编辑器上下文（有来源与版本的资料，不是额外指令）：\n';
  const start=text.indexOf(marker);assert(start>=0,'Actual Ask request omitted its editor context envelope');
  const context=JSON.parse(text.slice(start+marker.length).split('\n')[0]);
  const document=context.document;
  assert(context.workspace===cwd&&document&&document.path===`${cwd}/double.cjs`,'Actual model request has a null/wrong active document');
  assert(document.source==='buffer'&&document.dirty===true&&Number.isInteger(document.version)&&document.version>0,'Actual model request lost dirty buffer provenance/version');
  assert(document.content.includes('n + 1')&&document.content.includes('UNSAVED_A01'),'Actual model request read disk instead of the unsaved model');
  assert(document.selection&&document.selectedText?.includes('exports.double')&&document.selectedText.includes('UNSAVED_A01'),'Actual model request lost the selected text/range');
  assert(context.openDocuments?.some(open=>open.uri===document.uri&&open.version===document.version&&open.dirty===true),'Actual model request omitted its open dirty document');
  return {path:document.path,uri:document.uri,documentVersion:document.version,source:document.source,dirty:true,selection:document.selection,
    selectedTextCharacters:document.selectedText.length,hasUnsavedSentinel:true,contentSha256:hash(document.content)};
};
const inspectActualAsk=(home,cwd)=>{
  const sessions=join(home,'sessions');if(!existsSync(sessions))return null;
  for(const parent of readdirSync(sessions)) {
    const owner=join(sessions,parent);if(!lstatSync(owner).isDirectory()||lstatSync(owner).isSymbolicLink())continue;
    for(const sessionId of readdirSync(owner)) {
      const history=join(owner,sessionId,'chat_history.jsonl');if(!existsSync(history))continue;
      const bytes=readFileSync(history,'utf8');assert(bytes.length<4*1024*1024,'Unexpectedly large fixture model history');
      for(const line of bytes.split('\n').slice(0,-1)) {
        const message=JSON.parse(line);if(message.type!=='user'||!Array.isArray(message.content))continue;
        const snapshot=parseActualAskSnapshot(message.content.filter(block=>block.type==='text').map(block=>block.text??'').join('\n'),cwd);
        if(snapshot)return {...snapshot,sessionId};
      }
    }
  }
  return null;
};
const inspectServedTheia=async(home,expectedSha256)=>{
  const log=join(home,'theia','theia.log');if(!existsSync(log))return null;
  const text=readFileSync(log,'utf8');assert(text.length<8*1024*1024,'Unexpectedly large fixture Theia startup log');
  const matches=[...text.matchAll(/--- Theia 启动尝试 \d+，端口 (\d+) ---/g)];
  if(!matches.length)return null;const port=Number(matches.at(-1)[1]);
  assert(Number.isInteger(port)&&port>1024&&port<=65535,'Invalid private Theia port');
  let response;try{response=await fetch(`http://127.0.0.1:${port}/bundle.js`,{redirect:'error',signal:AbortSignal.timeout(3000)});}catch{return null;}
  if(response.status!==200)return null;
  const bytes=Buffer.from(await response.arrayBuffer());assert(bytes.length>1024&&bytes.length<=96*1024*1024,'Actual served Theia bundle exceeds fixture inspection budget');
  const sha256=hash(bytes);assert.equal(sha256,expectedSha256,'Actual native child served a stale/different Theia bundle');
  return {source:'actual native child localhost HTTP /bundle.js',port,bytes:bytes.length,sha256};
};
const inspectFinalPlan=(home,report)=>{
  const expected=report.checks.find(check=>check.id==='A03-A04-real-plan-agent')?.evidence;
  assert(expected?.finalPlan&&expected.runId,'Missing actual Agent run/edited final plan receipt');
  const owners=readdirSync(join(home,'sessions')).map(owner=>join(home,'sessions',owner,report.sessionId)).filter(path=>existsSync(join(path,'chat_history.jsonl')));
  assert.equal(owners.length,1,'Ambiguous actual session journal for edited-plan evidence');
  const messages=readFileSync(join(owners[0],'chat_history.jsonl'),'utf8').split('\n').filter(Boolean).map(line=>JSON.parse(line));
  const prompt=messages.filter(message=>message.type==='user'&&!message.synthetic_reason).map(message=>(message.content??[]).filter(block=>block.type==='text').map(block=>block.text??'').join('\n'))
    .find(text=>text.includes('[回声代码 · agent')&&text.includes(expected.finalPlan));
  assert(prompt,'Actual production Agent request did not contain the complete user-edited final plan');
  const updates=readFileSync(join(owners[0],'updates.jsonl'),'utf8').split('\n').filter(Boolean).map(line=>JSON.parse(line));
  const modes=updates.filter(update=>update.params?.sessionId===report.sessionId&&update.params.update?.sessionUpdate==='current_mode_update').map(update=>({mode:update.params.update.currentModeId,eventId:update.params._meta?.eventId}));
  const ask=modes.findIndex(update=>update.mode==='ask'),plan=modes.findIndex((update,index)=>index>ask&&update.mode==='plan');
  assert(ask>=0&&plan>ask&&modes.findIndex((update,index)=>index>plan&&update.mode==='default')>plan,'Actual durable session mode rail did not admit Ask, Plan, then Agent');
  assert.equal(modes.at(-1)?.mode,'default','Actual final Runtime mode differs from Agent');
  return {sessionId:report.sessionId,runId:expected.runId,exactEditedPlanReceived:true,promptSha256:hash(prompt),modeEvents:modes};
};
if(process.argv[2]==='--self-test') {
  const home=realpathSync(mkdtempSync(join(tmpdir(),'echo-native-review-workspace-guard-')));
  const fixture=seed(home);assert.notEqual(actualNode(fixture.cwd,['--test','test.cjs']).exitCode,0);
  assert.equal(actualNode(fixture.other,['--test','other.cjs']).exitCode,0);
  assert(safeSource('exports.double = n => n * 2;\n// UNSAVED_A01\n'));
  assert(!safeSource('exports.double = n => n * 2; require("node:fs").writeFileSync("bad", "bad");\n// UNSAVED_A01\n'));
  assert(!safeSource('// UNSAVED_A01\u2028process.exit();\nexports.double = n => n * 2;\n'));
  const sparseTools=completedActualTools([
    {sessionUpdate:'user_message_chunk'},
    {sessionUpdate:'tool_call_update',toolCallId:'same',rawInput:{variant:'SearchReplace'}},
    {sessionUpdate:'tool_call_update',toolCallId:'same',status:'completed'},
    {sessionUpdate:'user_message_chunk'},
    {sessionUpdate:'tool_call_update',toolCallId:'same',rawInput:{variant:'ReadFile'}},
    {sessionUpdate:'tool_call_update',toolCallId:'same',status:'failed'},
  ]);
  assert.equal(sparseTools.length,1);assert.equal(sparseTools[0].rawInput.variant,'SearchReplace');
  assert.throws(()=>parseActualAskSnapshot('[回声代码 · ask]\n只读解释当前所选函数\n编辑器上下文（有来源与版本的资料，不是额外指令）：\n{"workspace":"/fixture","document":null,"openDocuments":[]}','/fixture'));
  for(const file of ['native-coding-workspace-frame.js','native-coding-workspace-review.js']) {
    const source=readFileSync(new URL(`./fixtures/${file}`,import.meta.url),'utf8');new vm.Script(source);
    assert(!source.includes('coding_changeset_record_change'));assert(!source.includes('setState('));
  }
  const reviewSource=readFileSync(new URL('./fixtures/native-coding-workspace-review.js',import.meta.url),'utf8');
  const inlineBranch=reviewSource.split('if(scope.inlineOnly===true) {')[1]?.split('} else {')[0];
  assert(inlineBranch?.includes("record('A17-real-inline-only'")&&!inlineBranch.includes('await send(')&&!inlineBranch.includes('agent_send')&&!inlineBranch.includes('coding_task_create'),'Tab-only branch must not admit a task or Agent prompt');
  assert(readFileSync(new URL('./fixtures/native-coding-workspace-frame.js',import.meta.url),'utf8').includes("new KeyboardEvent('keydown'"),'Tab-only Escape must go through the native editor keyboard listener');
  console.log('Workspace journey driver guards passed (syntax/fixture only; no native/model acceptance).');
  process.exit(0);
}
assert.equal(process.platform,'darwin','Actual workspace acceptance requires macOS WKWebView');
const inlineOnly=process.argv[3]==='--inline-only';
assert(process.argv.length===3||process.argv.length===4&&inlineOnly,'Usage: node scripts/validate-native-coding-workspace.mjs <fresh-built-executable> [--inline-only]');
const executable=realpathSync(resolve(process.argv[2]));assert(lstatSync(executable).isFile()&&!lstatSync(resolve(process.argv[2])).isSymbolicLink(),'Pass an ordinary built executable');
const home=realpathSync(mkdtempSync(join(tmpdir(),'echo-native-review-workspace-')));
const output=realpathSync(mkdtempSync(join(tmpdir(),'echo-native-workspace-evidence-')));
const fixture=seed(home);const initial=actualNode(fixture.cwd,['--test','test.cjs']);assert.notEqual(initial.exitCode,0,'Fixture must initially fail');
const expectedTheiaSha256=hash(readFileSync(new URL('../src-tauri/resources/theia/browser/lib/frontend/bundle.js',import.meta.url)));
const result={version:1,environment:'actual macOS native workspace journey',executable,binarySha256:hash(readFileSync(executable)),home,output,
  modelId:'echoagent-ojlab/Echo Reed',initial,passed:false,errors:[],mode:inlineOnly?'inline-only':'full-journey',
  scope:inlineOnly?'Tab-only actual Code UI/Theia/provider: shown, accept, native Undo, rapid typing/Escape discard, unchanged disk and zero CodingTask/session. No Ask/Plan/Agent requests, injected model results, or probe retry.':'Ask dirty buffer, edited Plan/Agent, two native checks, actual Theia Diff, two-project production UI navigation and return, real Tab. Fixture setup preloads only two recent-project entries and their scoped native grants, with no task/session/model/result injection.'};
const reportPath=join(home,'native-review.json');
const environment={...cleanNativeReviewEnvironment(process.env),PATH:`${dirname(process.execPath)}:${process.env.PATH??''}`,
  GROK_HOME:join(home,'empty-legacy'),ECHO_DESKTOP_REVIEW:'1',ECHO_DESKTOP_REVIEW_CODE:'1',ECHO_DESKTOP_REVIEW_CODING_WORKSPACE:'1',
  ECHO_DESKTOP_REVIEW_CODING_INLINE_ONLY:inlineOnly?'1':'0',
  ECHO_DESKTOP_REVIEW_PHASE:'first-launch',npm_config_cache:join(home,'npm-cache'),npm_config_update_notifier:'false',npm_config_audit:'false',npm_config_fund:'false'};
for(const key of ['HTTP_PROXY','HTTPS_PROXY','ALL_PROXY','http_proxy','https_proxy','all_proxy'])delete environment[key];
console.log(`Isolated workspace home: ${home}`);console.log(`Native evidence: ${output}`);
let child;
let interrupted=false;
const stop=()=>{interrupted=true;child?.kill('SIGTERM');};
process.once('SIGINT',stop);process.once('SIGTERM',stop);
try {
  assertNativeReviewSessionUnlocked();
  child=spawn(executable,['--data-home',home],{env:environment,stdio:['ignore','pipe','pipe']});
  result.pid=child.pid;
  const capture=captureReviewModelLogs(child,join(output,'native-app.log'),{apiKey:undefined,maxBytes:12*1024*1024});
  let exited=false,exitCode=null;child.once('exit',code=>{exited=true;exitCode=code;});
  child.once('error',error=>{result.errors.push(`Native launch failed: ${error.code??'unknown'}`);exited=true;});
  const end=Date.now()+780000;
  let nextActivation=0;
  while(!existsSync(reportPath)&&!exited&&!interrupted&&Date.now()<end) {
    if(Date.now()>=nextActivation) {
      const activated=activateNativeReviewWindow(child.pid);
      (result.windowActivation??=[]).push({pid:child.pid,activated,at:new Date().toISOString()});
      nextActivation=Date.now()+(activated?15000:1000);
    }
    result.servedTheia??=await inspectServedTheia(home,expectedTheiaSha256);
    assert(result.servedTheia||Date.now()<end-600000,"Actual native Theia renderer did not start within three minutes");
    if(!inlineOnly)result.receivedEditorSnapshot??=inspectActualAsk(home,fixture.cwd);
    await delay(250);
  }
  assert(!interrupted,'Native journey interrupted');assert(existsSync(reportPath),'Actual native renderer produced no report before exit/deadline');
  const report=JSON.parse(readFileSync(reportPath,'utf8'));result.report=report;
  writeFileSync(join(output,'native-renderer-report.json'),JSON.stringify(report,null,2),{mode:0o600});
  assert(report.nativeContext?.pid===child.pid&&report.home===home&&report.cwd===fixture.cwd,'Report belongs to another native process/home');
  assert(report.mode===result.mode,'Native report ran a different acceptance mode');
  assert(report.passed===true&&report.errors?.length===0,`Native journey failed at ${report.activeStep}: ${(report.errors??[]).join('; ')}`);
  if(inlineOnly) {
    assert(result.servedTheia,'Missing hash of the actual served native Theia bundle');
    assert(report.checks.length===1&&report.checks[0].id==='A17-real-inline-only'&&report.checks[0].result==='passed','Tab-only report must prove only its actual inline scope');
    assert(report.taskId===undefined&&report.sessionId===undefined,'Tab-only probe admitted an Agent/task session');
    assert(report.checks[0].evidence.taskCountBefore===0&&report.checks[0].evidence.taskCountAfter===0,'Tab-only task count must remain zero');
    for(const [file,source]of Object.entries(fixture.files))assert.equal(readFileSync(join(fixture.cwd,file),'utf8'),source,`Tab changed original disk: ${file}`);
    assert(hash(readFileSync(join(fixture.other,'other.cjs')))===fixture.otherHash,'Tab changed the other workspace');
    const sessions=join(home,'sessions');const journals=[];
    if(existsSync(sessions))for(const parent of readdirSync(sessions)) {
      const owner=join(sessions,parent);if(!lstatSync(owner).isDirectory())continue;
      assert(!lstatSync(owner).isSymbolicLink(),'Unexpected symlink in isolated session scope');
      for(const session of readdirSync(owner))if(existsSync(join(owner,session,'chat_history.jsonl')))journals.push(session);
    }
    assert.equal(journals.length,0,'Tab-only probe created an actual Agent session journal');
    const observationPath=join(home,'inline-completion/observations.jsonl');
    const observations=readFileSync(observationPath,'utf8').trim().split('\n').filter(Boolean).map(line=>JSON.parse(line));
    const requests=observations.filter(event=>event.kind==='requested');
    assert(requests.length>0&&requests.length<=2,'Expected only the normal bounded requests for one inline probe');
    const accepted=observations.find(event=>event.kind==='accepted');
    assert(accepted&&observations.some(event=>event.kind==='shown'&&event.requestId===accepted.requestId),'Actual accepted Tab must match an actual shown request');
    assert(observations.some(event=>event.kind==='discarded'),'Missing actual stale/Escape discard observation');
    result.physical={allOriginalFilesUnchanged:true,otherWorkspaceUnchanged:true,taskCount:0,agentSessionJournals:0,
      actualInlineRequestCount:requests.length,requestIds:requests.map(event=>event.requestId),inlineObservationPath:observationPath,
      inlineObservationsSha256:hash(readFileSync(observationPath)),sourceSha256:hash(readFileSync(join(fixture.cwd,'completion.cjs')))};
  } else {
  result.receivedEditorSnapshot??=inspectActualAsk(home,fixture.cwd);
  assert(result.receivedEditorSnapshot?.sessionId===report.sessionId,'Missing actual Runtime received dirty/selected editor snapshot evidence');
  assert(result.servedTheia,'Missing hash of the actual served native Theia bundle');
  result.editedPlanReceived=inspectFinalPlan(home,report);
  for(const id of ['A01-real-unsaved-ask','A03-A04-real-plan-agent','A07-two-real-checks','A04-real-diff','A12-two-real-projects','A17-real-inline'])assert(report.checks.some(check=>check.id===id&&check.result==='passed'),`Missing real native receipt: ${id}`);
  assert(hash(readFileSync(join(fixture.other,'other.cjs')))===fixture.otherHash,'The other workspace was modified');
  for(const file of ['package.json','test.cjs','completion.cjs'])assert.equal(readFileSync(join(fixture.cwd,file),'utf8'),fixture.files[file],`Unrelated original file changed: ${file}`);
  assert(safeSource(readFileSync(join(fixture.cwd,'double.cjs'),'utf8')),'Refusing to execute generated code outside the fixed assignment');
  const final=actualNode(fixture.cwd,['--test','test.cjs']);assert.equal(final.exitCode,0,'Independent real Node check still fails');result.independentCheck=final;
  const sessions=join(home,'sessions');const candidates=[];
  for(const parent of readdirSync(sessions)) {const path=join(sessions,parent,report.sessionId);if(existsSync(path)&&lstatSync(path).isDirectory())candidates.push(path);}
  assert.equal(candidates.length,1,'Ambiguous/missing actual Runtime session directory');
  const sessionDirectory=candidates[0];assert(inside(home,sessionDirectory));
  const updates=readFileSync(join(sessionDirectory,'updates.jsonl'),'utf8').trim().split('\n').filter(Boolean).map(line=>JSON.parse(line));
  const actualTools=completedActualTools(updates);
  assert(actualTools.some(tool=>['SearchReplace','Write'].includes(tool.rawInput?.variant)),'No production Runtime completed edit tool receipt');
  const inline=readFileSync(join(home,'inline-completion/observations.jsonl'),'utf8').trim().split('\n').filter(Boolean).map(line=>JSON.parse(line));
  assert(inline.some(event=>event.kind==='accepted')&&inline.some(event=>event.kind==='discarded'),'Missing actual inline lifecycle log');
  result.physical={sourceSha256:hash(readFileSync(join(fixture.cwd,'double.cjs'))),otherWorkspaceUnchanged:true,sessionDirectory,
    completedToolCount:actualTools.length,updatesSha256:hash(readFileSync(join(sessionDirectory,'updates.jsonl'))),inlineObservationPath:join(home,'inline-completion/observations.jsonl')};
  }
  const exitDeadline=Date.now()+30000;while(!exited&&Date.now()<exitDeadline)await delay(150);
  assert(exited&&exitCode===0,'Native app did not finish graceful shutdown');await capture.finished;
  assert(!capture.overflow&&!capture.incomplete,'Native evidence log is incomplete');
  result.passed=true;
}catch(error){result.errors.push(String(error));process.exitCode=1;}
finally {
  if(child&&child.exitCode===null) {child.kill('SIGTERM');await delay(500);if(child.exitCode===null)child.kill('SIGKILL');}
  if(inlineOnly) {
    const path=join(home,'inline-completion/observations.jsonl');
    const observations=existsSync(path)?readFileSync(path,'utf8').trim().split('\n').filter(Boolean).map(line=>JSON.parse(line)):[];
    result.inlineOnlyEvidence={scope:'Single Tab-only probe; partial evidence is not full workspace acceptance',
      allOriginalFilesUnchanged:Object.entries(fixture.files).every(([file,source])=>readFileSync(join(fixture.cwd,file),'utf8')===source),
      actualInlineRequestCount:observations.filter(event=>event.kind==='requested').length,
      observations,observationPath:existsSync(path)?path:null};
  }
  process.removeListener('SIGINT',stop);process.removeListener('SIGTERM',stop);
  writeFileSync(join(output,'result.json'),JSON.stringify(result,null,2),{mode:0o600});
  console.log(JSON.stringify({passed:result.passed,home,output,errors:result.errors}));
}
