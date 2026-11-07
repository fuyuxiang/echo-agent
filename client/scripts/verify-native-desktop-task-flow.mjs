import assert from 'node:assert/strict';
import {nativeShortcut,nativeBrowserMarkerObserved} from './lib/native-tool-inputs.mjs';
import {verifyControlsModelBinding} from './lib/desktop-controls-evidence.mjs';
import {readFileSync,writeFileSync,realpathSync,lstatSync,statSync} from 'node:fs';
import {join,resolve} from 'node:path';
import {createHash} from 'node:crypto';
import {fileURLToPath} from 'node:url';
const home=realpathSync(resolve(process.argv[2]||''));
assert(home.startsWith('/private/tmp/echo-native-review-controls-'),'dedicated acceptance home');
const driver=JSON.parse(readFileSync(join(home,'driver.json'),'utf8'));
const reportPath=join(home,'workspaces/default/native-control-fixtures/task-flow-progress.json');
const r=JSON.parse(readFileSync(reportPath,'utf8'));
assert.equal(r.home,home);assert.equal(driver.home,home);assert.equal(driver.pid,r.context.pid);
assert.equal(r.version,2);assert.equal(r.expected,`任务驱动电脑验收 ${home.split('-').at(-1)}\n中文输入、暂停与继续成功\n`,'each run must prove its own distinct input');
verifyControlsModelBinding(r.model,driver.requestedModelId);
assert.equal(driver.osPickerVerified,true);assert.equal(r.workspaceSelection?.cwd,driver.cwd);
assert.deepEqual(JSON.parse(readFileSync(join(home,'echoagent-picked-workspaces.json'),'utf8')),[driver.cwd]);
assert.deepEqual(r.errors,[]);assert(r.finishedAt,'continued model turn must finish');
assert(r.completions.some(c=>c.at>r.resumedAt&&c.at<=r.finishedAt&&c.stopReason==='end_turn'&&!c.cancelTrigger),'the continued model must end naturally, without a test cancellation');
assert.equal(r.targetWrittenByFixture,false);assert.equal(r.approvalResolvedByFixture,false);
assert.deepEqual(r.approvals,[],'ordinary native task requires zero repeated confirmations');
assert.deepEqual(r.runtimePermissions,[],'no duplicate or malformed-call Runtime approval');
assert.equal(r.controls.length,driver.crossApp?4:3);assert.equal(r.controls[1].text,'停一下');assert.equal(r.controls[2].text,'继续保存');
assert.equal(r.initialStatus.mode,'computer_use');assert.equal(r.initialStatus.computerAuthorized,true);
assert.equal(r.pausedStatus.paused,true);assert.equal(r.pausedStatus.computerAuthorized,false);assert.equal(r.pausedStatus.task.reason,'user_paused');
assert.deepEqual(r.pauseQuiet.before,r.pauseQuiet.after);assert(r.pauseQuiet.durationMs>=5000);
assert.equal(r.resumedStatus.sessionId,r.sessionId);assert.equal(r.resumedStatus.paused,false);assert.equal(r.resumedStatus.computerAuthorized,true);
assert(r.resumedStatus.task.generation>r.pausedStatus.task.generation);
assert.equal(r.initialStatus.task.scope,r.resumedStatus.task.scope);
assert.equal(r.bodyReadback.text,r.expected);
assert.equal(r.file,join(home,'workspaces/default/native-control-fixtures/task-flow-output.txt'));
assert(!lstatSync(r.file).isSymbolicLink());assert.equal(realpathSync(r.file),r.file);
assert(statSync(r.file).mtimeMs>=Date.parse(r.resumedAt)-1000,'the actual save must occur after natural continuation');
const bytes=readFileSync(r.file);assert.equal(bytes.toString('utf8'),r.expected,'actual saved file must match exact original Unicode text and line endings');
const journal=join(home,'sessions',encodeURIComponent(r.cwd),r.sessionId,'updates.jsonl');
const events=readFileSync(journal,'utf8').trim().split('\n').map(l=>JSON.parse(l));
const updates=events.map(e=>e.params?.update).filter(Boolean);
const completed=updates.filter(u=>u.status==='completed'&&u.rawOutput?.type==='MCP'&&u.rawOutput?.server_name==='echoagent-automation');
const tools=[...new Map(completed.map(u=>[u.toolCallId,u])).values()];
assert(tools.some(u=>u.rawOutput.tool_name==='computer_set_value'),'actual Runtime native input');
assert(tools.some(u=>u.rawOutput.tool_name==='computer_key_window'),'actual Runtime native file-dialog keys');
const newDocumentCall=updates.find(u=>u.rawInput?.tool_name==='echoagent-automation__computer_key_window'&&
 nativeShortcut(u.rawInput.tool_input)==='cmd+n'&&
 tools.some(done=>done.toolCallId===u.toolCallId&&(()=>{try{return JSON.parse(done.rawOutput.output?.OkayOutput).keyDispatched===true;}catch{return false;}})()));
assert(newDocumentCall,'a new document must actually be created in this task, not inferred from app activation');
const newDocumentDone=updates.findIndex(u=>u.toolCallId===newDocumentCall.toolCallId&&u.status==='completed');
const exactInputIndex=updates.findIndex(u=>{if(u.status!=='completed'||u.rawOutput?.tool_name!=='computer_set_value')return false;try{const o=JSON.parse(u.rawOutput.output?.OkayOutput);return o.before?.role==='AXTextArea'&&o.before.value===''&&o.after?.value===r.expected&&o.verified===true;}catch{return false;}});
assert(newDocumentDone>=0&&exactInputIndex>newDocumentDone,'explicit new-document dispatch must precede the exact input');
assert(tools.some(u=>{if(u.rawOutput.tool_name!=='computer_set_value')return false;try{const o=JSON.parse(u.rawOutput.output?.OkayOutput);return o.before?.role==='AXTextArea'&&o.before.value===''&&o.after?.value===r.expected&&o.verified===true;}catch{return false;}}),'exact input must be verified in the newly empty editor');
assert(!updates.some(u=>['Shell','WriteFile','StrReplaceFile'].includes(u.rawOutput?.type)),'no shell or file tool substitutes for desktop task');
const hasSavedWindow=value=>{
 if(!value||typeof value!=='object')return false;
 if((value.kind==='window'||value.role==='AXWindow')&&[value.name,value.title].some(name=>typeof name==='string'&&name.includes('task-flow-output.txt')))return true;
 return Object.values(value).some(child=>Array.isArray(child)?child.some(hasSavedWindow):hasSavedWindow(child));
};
assert(tools.some(u=>{try{return hasSavedWindow(JSON.parse(u.rawOutput.output?.OkayOutput));}catch{return false;}}),'saved document window title observed through Runtime');
const documentUrls=[];
const collectUrls=value=>{if(!value||typeof value!=='object')return;if(typeof value.documentUrl==='string')documentUrls.push(value.documentUrl);Object.values(value).forEach(child=>Array.isArray(child)?child.forEach(collectUrls):collectUrls(child));};
for(const u of tools){try{collectUrls(JSON.parse(u.rawOutput.output?.OkayOutput));}catch{}}
assert(documentUrls.some(url=>{try{return fileURLToPath(url)===r.file;}catch{return false;}}),'actual native document URL must match the requested save location');

let crossAppVerification;
if(driver.crossApp){
 assert(r.crossApp?.finishedAt,'second real app must finish');
 assert(r.completions.some(c=>c.at>r.crossApp.startedAt&&c.stopReason==='end_turn'&&!c.cancelTrigger),'second app must end naturally');
 const secondTools=r.tools.slice(r.crossApp.toolStart);
 assert(nativeBrowserMarkerObserved(secondTools,r.crossApp.pass),'native Safari window/page must expose the real success marker, not a typed search/input value');
 assert(secondTools.some(t=>t.tool==='computer_set_value'&&t.output?.executed===true),'real browser native input');
 assert(!secondTools.some(t=>t.tool.startsWith('browser_')),'native desktop tools only');
 crossAppVerification={application:'Safari',inputAndFileSelectionVerified:true,marker:r.crossApp.pass,toolCalls:secondTools.length,elapsedSeconds:(Date.parse(r.crossApp.finishedAt)-Date.parse(r.crossApp.startedAt))/1000};
}
const evidence={version:1,kind:'verified-desktop-task-flow',home,sessionId:r.sessionId,model:r.model,pid:driver.pid,
 sourceBinarySha256:driver.sourceBinarySha256,signedBinarySha256:driver.binarySha256,
 nativeToolCalls:tools.length,crossAppVerification,ordinaryApprovalCount:r.approvals.length,runtimePermissionCount:r.runtimePermissions.length,modelEndedNaturally:true,
 newDocumentCreated:true,inputReadbackExact:true,actualFileExact:true,nativeDocumentUrlMatched:true,file:r.file,fileBytes:bytes.length,fileSha256:createHash('sha256').update(bytes).digest('hex'),
 pauseRevokedConsent:true,pauseQuietMilliseconds:r.pauseQuiet.durationMs,naturalContinuationSameTask:true,
 targetWrittenByFixture:false,approvalResolvedByFixture:false,desktopAcceptance:true,verifiedAt:new Date().toISOString()};
writeFileSync(join(home,'task-flow-verification.json'),JSON.stringify(evidence,null,2),{mode:0o600});
console.log(JSON.stringify(evidence,null,2));
