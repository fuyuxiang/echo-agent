import assert from 'node:assert/strict';
import { spawn, execFileSync } from 'node:child_process';
import { createHash, randomBytes } from 'node:crypto';
import { chmodSync, copyFileSync, existsSync, lstatSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, realpathSync, unlinkSync, writeFileSync } from 'node:fs';
import { join, resolve, basename } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { pathToFileURL } from 'node:url';
import { seedReviewModel, captureReviewModelLogs, redactReviewModelSecrets } from './lib/native-review-model.mjs';
import { cleanNativeReviewEnvironment } from './lib/native-review-isolation.mjs';
import { nativeReviewOutput } from './lib/native-review-output.mjs';
import { assertNativeReviewSessionUnlocked, activateNativeReviewWindow } from './lib/native-review-window.mjs';
import { confirmNativeReviewWorkspace } from './lib/native-core-workspace-picker.mjs';
import { readonlyCompatibilitySourceRevocations } from './lib/native-core-review-isolation.mjs';

export const PROJECT_MODEL_ID = 'organization/MiniMax-M3.1-Flash';
const sha = b => createHash('sha256').update(b).digest('hex');
export const PROJECT_CHECKS = Object.freeze({
  'first-launch': ['native-folder-selection','real-account-and-model','project-create-and-publish-ui','real-project-task-and-file',
    'output-preview-copy-and-new-task','share-generated-file-and-accept','scheduled-project-work-actual-due','no-duplicate-project-task-or-trigger'],
  restart: ['real-account-and-model','restart-project-sessions-and-files'],
});
export function verifyProjectReport(report, scope, pid, startedAt) {
  assert.equal(report?.kind, 'native-project'); assert.equal(report.version, 1);
  assert.equal(report.fakeIPC, false); assert.equal(report.passed, true, JSON.stringify(report.errors)); assert.deepEqual(report.errors, []);
  for (const key of ['home','nonce','phase']) assert.equal(report[key],scope[key]);
  assert.equal(report.nativeContext.pid,pid); assert.equal(report.nativeContext.phase,scope.phase); assert.equal(report.nativeContext.home,scope.home); assert.equal(report.nativeContext.evaluations,1);
  assert(Date.parse(report.finishedAt)>=startedAt && Date.parse(report.finishedAt)<=Date.now()+1000);
  assert.deepEqual(report.checks.map(c=>c.id),PROJECT_CHECKS[scope.phase]); assert(report.checks.every(c=>c.result==='passed'));
  const e=report.expected; assert(e.projectId && e.remoteProjectId && e.documentId && e.automationId && e.recordId);
  assert.equal(e.name,`端到端验收-${scope.nonce.slice(0,8)}`);
  assert.equal(e.sessionIds.length,3); assert.equal(new Set(e.sessionIds).size,3);
  assert(e.sessionIds.every(id=>/^[A-Za-z0-9-]{1,128}$/.test(id)));
  assert(e.copiedPath.startsWith(`${scope.home}/projects/${e.projectId}/assets/`) && !e.copiedPath.includes('..'));
  assert.equal(e.contextInitial,`CONTEXT-${scope.nonce}-INITIAL`);assert.equal(e.contextLatest,`CONTEXT-${scope.nonce}-LATEST`);
  for (const permission of report.permissions ?? []) {
    assert(e.sessionIds.includes(permission.sessionId));
    assert.equal(permission.via,'production allow-once UI');
    if(permission.name==='knowledge_context') {
      assert(e.projectScopeId && permission.taskId);
      assert.deepEqual(permission.scopeIds,[e.projectScopeId]);
      assert.equal(permission.path,null);
    } else {
      assert(['read_file','write_file','edit_file','create_file'].includes(permission.name));
      const allowed=['source.csv','project-result.md','project-reuse.md','project-scheduled.md'].map(n=>`${scope.home}/workspaces/default/${n}`);
      assert(allowed.includes(permission.path) || permission.path===e.copiedPath);
      if(permission.path===e.copiedPath || permission.path===allowed[0])assert.equal(permission.name,'read_file');
    }
  }
  const account=report.checks.find(c=>c.id==='real-account-and-model').evidence;
  assert.equal(account.modelId,scope.modelId); assert.equal(account.serverUrl,scope.serverUrl);assert.equal(account.userId,scope.expectedUserId);assert.equal(account.username,scope.expectedUsername);
  if(scope.phase==='restart')assert.deepEqual(e,scope.expected);
  else {
    const schedule=report.checks.find(c=>c.id==='scheduled-project-work-actual-due').evidence;
    assert.equal(schedule.manualRunInvoked,false); assert.equal(schedule.pausedAfterVerification,true);
    assert.equal(Date.parse(schedule.scheduledFor),e.dueEpoch); assert(Date.parse(schedule.startedAt)>=e.dueEpoch);
    assert.equal(schedule.status,'success');assert.equal(schedule.modelId,scope.modelId);
    assert.equal(schedule.sessionId,e.sessionIds[2]);
    assert(report.checks.filter(c=>['real-project-task-and-file','output-preview-copy-and-new-task'].includes(c.id)).every(c=>c.evidence.completion.stopReason==='end_turn'));
  }
  return e;
}

function physicalProof(home,scope,e,sourceMarker) {
  const projects=JSON.parse(readFileSync(join(home,'echoagent-projects.json')));
  const list=Array.isArray(projects)?projects:projects.projects;
  assert.equal(list.length,1);const p=list.find(p=>p.id===e.projectId);assert(p);
  assert.deepEqual(JSON.parse(readFileSync(join(home,'echoagent-picked-workspaces.json'))),[scope.cwd]);
  assert.equal(p.cwd,scope.cwd);assert.equal(p.instructions.includes(e.contextLatest),true);
  assert(e.sessionIds.every(id=>p.conversations.some(c=>c.sessionId===id)));
  const copy=p.assets.find(a=>a.path===e.copiedPath);assert(copy && copy.sourceSessionId===e.sessionIds[0] && copy.purpose==='output');
  assert.equal(realpathSync(e.copiedPath),e.copiedPath); assert(lstatSync(e.copiedPath).isFile() && !lstatSync(e.copiedPath).isSymbolicLink());
  assert.equal(readFileSync(join(scope.cwd,'source.csv'),'utf8'),`item,amount\nalpha,7\nbeta,11\nsource_marker,${sourceMarker}\n`);
  const original=readFileSync(join(scope.cwd,'project-result.md'));assert.equal(sha(readFileSync(e.copiedPath)),sha(original));
  const files={};
  for(const [name,context]of [['project-result.md',e.contextInitial],['project-reuse.md',e.contextInitial],['project-scheduled.md',e.contextLatest]]){
    const bytes=readFileSync(join(scope.cwd,name));const text=bytes.toString('utf8');
    // Preserve all 128 random bits independently of whether the model keeps
    // SOURCE- as a value prefix or renders SOURCE=<value> as a labelled field.
    assert(text.includes('TOTAL=18') && text.includes(sourceMarker.slice('SOURCE-'.length)) && text.includes(context),`${name} does not contain independently verified source/computation/context`);
    if(name==='project-result.md')assert(text.includes(sourceMarker),'The initial source value must be preserved verbatim');
    files[name]={bytes:bytes.length,sha256:sha(bytes)};
  }
  const schedule=JSON.parse(readFileSync(join(home,'echoagent-automations.json'))).automations.find(a=>a.id===e.automationId);
  const records=JSON.parse(readFileSync(join(home,'echoagent-automation-records.json'))).records.filter(r=>r.automationId===e.automationId);
  assert.equal(schedule.projectId,e.projectId);assert.equal(schedule.status,'PAUSED');assert.equal(records.length,1);
  const r=records[0];assert.equal(r.id,e.recordId);assert.equal(r.status,'success');assert.equal(r.sessionId,e.sessionIds[2]);
  assert.equal(Date.parse(r.scheduledFor),e.dueEpoch);assert(Date.parse(r.startedAt)>=e.dueEpoch);
  assert.equal(r.modelId,scope.modelId);assert.equal(r.cwd,scope.cwd);
  // Independent runtime files, rather than renderer text, must retain actual
  // model work and exact attachment source. Only inspect this owned fresh home.
  const paths=[];
  const walk=dir=>{if(!existsSync(dir))return;for(const ent of readdirSync(dir,{withFileTypes:true})){const path=join(dir,ent.name);if(ent.isDirectory())walk(path);else if(ent.isFile()&&/\.jsonl$/.test(ent.name))paths.push(path);}};
  walk(join(home,'sessions'));
  assert(paths.length>0,'Runtime session evidence missing');
  const transcripts=paths.filter(p=>lstatSync(p).size<64*1024*1024).map(p=>({path:p,text:readFileSync(p,'utf8')}));
  assert(transcripts.some(t=>t.text.includes(sourceMarker) && t.text.includes('project-result.md')),'Runtime did not retain the source read/output');
  assert(transcripts.some(t=>t.text.includes(e.copiedPath) && t.text.includes('project-reuse.md')),'Actual second task attachment reference missing from runtime history');
  return {projectId:p.id,remoteProjectId:e.remoteProjectId,files,copySha256:sha(readFileSync(e.copiedPath)),sessionIds:e.sessionIds,
    runtimeFiles:transcripts.map(t=>({name:basename(t.path),sha256:sha(t.text)})),automation:{id:schedule.id,status:schedule.status,recordId:r.id,scheduledFor:r.scheduledFor,startedAt:r.startedAt,sessionId:r.sessionId}};
}

export async function runNativeProjectReview(executable,accountProfile) {
  assert.equal(process.platform,'darwin'); assertNativeReviewSessionUnlocked();
  executable=realpathSync(resolve(executable));accountProfile=realpathSync(resolve(accountProfile));
  const original=JSON.parse(readFileSync(accountProfile));
  assert(original.serverUrl?.startsWith('https://') && original.userId && original.username && original.deviceId,'Provide the explicitly selected real account profile');
  const fixture=readFileSync(new URL('./fixtures/native-project-review.js',import.meta.url));const binary=readFileSync(executable);
  assert(binary.includes(fixture),'Rebuild native app with the current fixed project journey');
  const output=nativeReviewOutput('native-project','NATIVE_PROJECT_REVIEW_OUTPUT');
  const home=realpathSync(mkdtempSync('/private/tmp/echo-native-review-project-'));chmodSync(home,0o700);
  const cwd=join(home,'workspaces/default'),legacy=join(home,'empty-legacy');mkdirSync(cwd,{recursive:true,mode:0o700});mkdirSync(legacy,{mode:0o700});
  chmodSync(join(home,'workspaces'),0o700);chmodSync(cwd,0o700);
  writeFileSync(join(home,'config.toml'),`[marketplace]\nofficial_marketplace_auto_installed = true\ndisabled_sources = [${readonlyCompatibilitySourceRevocations().map(JSON.stringify).join(', ')}]\n[claude_compat]\nimported = true\n`,{mode:0o600});
  seedReviewModel(home);
  copyFileSync(accountProfile,join(home,'organization-profile.json'));chmodSync(join(home,'organization-profile.json'),0o600);
  // Reuse only this selected account through production Keychain + refresh;
  // no token is fabricated, injected into the renderer, or written to evidence.
  const nonce=randomBytes(16).toString('hex'),sourceMarker=`SOURCE-${randomBytes(16).toString('hex')}`;
  writeFileSync(join(cwd,'source.csv'),`item,amount\nalpha,7\nbeta,11\nsource_marker,${sourceMarker}\n`,{mode:0o600});
  const result={version:1,kind:'real-native-project-driver',home,cwd,output,passed:false,nativeVerified:false,
    binarySha256:sha(binary),fixtureSha256:sha(fixture),modelId:PROJECT_MODEL_ID,serverUrl:original.serverUrl,username:original.username,phases:[],errors:[]};
  let active,logs,expected,interrupted=false;
  const interrupt=()=>{interrupted=true;active?.kill('SIGTERM');};process.once('SIGINT',interrupt);process.once('SIGTERM',interrupt);
  try {
    for(const phase of ['first-launch','restart']){
      assert(!interrupted,'Project acceptance interrupted');
      const scope={schema:1,home,cwd,nonce,phase,modelId:PROJECT_MODEL_ID,serverUrl:original.serverUrl,expectedUserId:original.userId,expectedUsername:original.username,expected:expected??null};
      writeFileSync(join(home,'native-project-manifest.private.json'),JSON.stringify(scope),{mode:0o600});
      const reportPath=join(home,'native-review.json');if(existsSync(reportPath))unlinkSync(reportPath);
      const clean=cleanNativeReviewEnvironment(process.env);const bypass=[clean.NO_PROXY,clean.no_proxy,'127.0.0.1','localhost','::1',new URL(original.serverUrl).hostname].filter(Boolean).join(',');
      const env={...clean,GROK_HOME:legacy,NO_PROXY:bypass,no_proxy:bypass,ECHO_DESKTOP_REVIEW:'1',ECHO_DESKTOP_REVIEW_PROJECT:'1',ECHO_DESKTOP_REVIEW_PHASE:phase};
      const start=Date.now();active=spawn(executable,['--data-home',home],{cwd,env,stdio:['ignore','pipe','pipe']});
      const child=active;let ended=false,exitCode,signal,launchError;
      child.once('error',e=>{launchError=e;ended=true;});const closed=new Promise(resolve=>child.once('close',(code,sig)=>{exitCode=code;signal=sig;ended=true;resolve();}));
      logs=captureReviewModelLogs(child,join(output,`${phase}-app.log`));
      writeFileSync(join(output,`${phase}-launch.json`),JSON.stringify({pid:child.pid,...scope}),{mode:0o600});
      console.log(JSON.stringify({phase,pid:child.pid,home,output,serverUrl:original.serverUrl,username:original.username}));
      let picked=false,nextActivate=0,lastStep='',firstPickerAttempt=0;
      while(!ended && Date.now()-start<(phase==='first-launch'?1200000:180000)){
        if(Date.now()>nextActivate){activateNativeReviewWindow(child.pid);nextActivate=Date.now()+15000;}
        const ready=join(cwd,'project-workspace-picker-ready.json');
        if(phase==='first-launch'&&!picked&&existsSync(ready)){
          assert.deepEqual(JSON.parse(readFileSync(ready)),{nonce,cwd});
          firstPickerAttempt ||= Date.now();
          try { picked=confirmNativeReviewWorkspace({pid:child.pid,home,cwd,kind:'project'}); }
          catch(error) {
            if(!String(error).includes('Owned picker is not at the proposed workspace') || Date.now()-firstPickerAttempt>20000)throw error;
            console.log(JSON.stringify({phase,pickerWaitingForFocusOrFolder:true,detail:String(error.stderr??error.message).trim().slice(-900)}));
            await delay(500);
          }
        }
        const progress=join(cwd,'project-progress.json');
        if(existsSync(progress)){try{const p=JSON.parse(readFileSync(progress));if(p.step!==lastStep){lastStep=p.step;console.log(JSON.stringify({phase,step:lastStep,passedChecks:p.checks.length}));}}catch{}}
        await Promise.race([closed,delay(250)]);
      }
      assert(ended,'Native project journey exceeded its bounded deadline');await closed;await logs.finished;
      if(launchError)throw launchError;assert(!logs.overflow && !logs.incomplete);
      assert.equal(exitCode,0);assert.equal(signal,null);
      const report=JSON.parse(readFileSync(reportPath));writeFileSync(join(output,`${phase}.json`),redactReviewModelSecrets(JSON.stringify(report,null,2)),{mode:0o600});
      expected=verifyProjectReport(report,scope,child.pid,start);if(phase==='first-launch')assert(picked,'Real native folder picker not verified');
      result.phases.push({phase,pid:child.pid,exitCode,seconds:(Date.now()-start)/1000,physicalProof:physicalProof(home,scope,expected,sourceMarker)});
      active=undefined;
    }
    result.expected=expected;result.passed=result.nativeVerified=true;
  }catch(error){result.errors.push(String(error?.stack??error).slice(0,7000));}
  finally{
    if(active&&active.exitCode===null&&active.signalCode===null){active.kill('SIGTERM');await delay(1200);if(active.exitCode===null&&active.signalCode===null)active.kill('SIGKILL');}
    await logs?.finished;process.removeListener('SIGINT',interrupt);process.removeListener('SIGTERM',interrupt);
    result.finishedAt=new Date().toISOString();writeFileSync(join(output,'result.json'),redactReviewModelSecrets(JSON.stringify(result,null,2)),{mode:0o600});
  }
  console.log(JSON.stringify({passed:result.passed,nativeVerified:result.nativeVerified,home,output,errors:result.errors}));return result;
}
if(process.argv[1] && import.meta.url===pathToFileURL(resolve(process.argv[1])).href){
  assert.equal(process.argv.length,4,'Pass the freshly built binary and the explicitly selected real organization profile; set NATIVE_REVIEW_MODEL_CONFIG');
  if(!(await runNativeProjectReview(process.argv[2],process.argv[3])).passed)process.exitCode=1;
}
