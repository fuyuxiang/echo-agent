import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { PROJECT_CHECKS, PROJECT_MODEL_ID, readPreparedProjectReviewProfile, verifyProjectReport } from './validate-native-project-review.mjs';
function fixture() {
  const now=Date.now(),nonce='0123456789abcdef0123456789abcdef',home='/private/tmp/echo-native-review-project-test';
  const scope={home,nonce,phase:'first-launch',modelId:PROJECT_MODEL_ID,serverUrl:'https://org.example:8787',expectedUserId:'user',expectedUsername:'reviewer'};
  const e={projectId:'project_123',remoteProjectId:'remote',documentId:'doc',automationId:'auto',recordId:'record',name:'端到端验收-01234567',
    sessionIds:['session-1','session-2','session-3'],copiedPath:home+'/projects/project_123/assets/project-result.md',dueEpoch:now-10000,contextInitial:`CONTEXT-${nonce}-INITIAL`,contextLatest:`CONTEXT-${nonce}-LATEST`};
  const report={kind:'native-project',version:1,fakeIPC:false,passed:true,errors:[],home,nonce,phase:scope.phase,nativeContext:{pid:1234,home,phase:scope.phase,evaluations:1},finishedAt:new Date(now).toISOString(),expected:e,
    checks:PROJECT_CHECKS['first-launch'].map(id=>({id,result:'passed',evidence:{}}))};
  report.checks.find(c=>c.id==='real-account-and-model').evidence={serverUrl:scope.serverUrl,userId:'user',username:'reviewer',modelId:PROJECT_MODEL_ID};
  for(const id of ['real-project-task-and-file','output-preview-copy-and-new-task'])report.checks.find(c=>c.id===id).evidence={completion:{stopReason:'end_turn'}};
  report.checks.find(c=>c.id==='scheduled-project-work-actual-due').evidence={manualRunInvoked:false,pausedAfterVerification:true,scheduledFor:new Date(e.dueEpoch).toISOString(),startedAt:new Date(e.dueEpoch+1).toISOString(),status:'success',modelId:PROJECT_MODEL_ID,sessionId:'session-3'};
  return {report,scope,now};
}
test('acceptance requires a bound real account/model and native launch, not just a green UI report',()=>{
 const {report,scope,now}=fixture();assert.equal(verifyProjectReport(report,scope,1234,now-1000).projectId,'project_123');
 for(const alter of [r=>{r.fakeIPC=true;},r=>{r.checks.find(c=>c.id==='real-account-and-model').evidence.userId='foreign';},r=>{r.checks.find(c=>c.id==='real-account-and-model').evidence.modelId='unrelated';},r=>{r.nativeContext.pid=999;},r=>{r.nativeContext.phase='restart';},r=>{r.nonce='foreign';}]){
  const bad=structuredClone(report);alter(bad);assert.throws(()=>verifyProjectReport(bad,scope,1234,now-1000));
 }
});
test('manual runs, early triggers and missing model completion cannot qualify scheduled acceptance',()=>{
 const {report,scope,now}=fixture();
 for(const alter of [s=>{s.manualRunInvoked=true;},s=>{s.startedAt=new Date(report.expected.dueEpoch-1).toISOString();},s=>{s.scheduledFor=new Date(report.expected.dueEpoch+60000).toISOString();},s=>{s.status='running';},s=>{s.pausedAfterVerification=false;}]){
  const bad=structuredClone(report);alter(bad.checks.find(c=>c.id==='scheduled-project-work-actual-due').evidence);assert.throws(()=>verifyProjectReport(bad,scope,1234,now-1000));
 }
 const bad=structuredClone(report);bad.checks.find(c=>c.id==='real-project-task-and-file').evidence.completion.stopReason='cancelled';assert.throws(()=>verifyProjectReport(bad,scope,1234,now-1000));
});
test('foreign copy paths, duplicate sessions and skipped stages are rejected',()=>{
 const {report,scope,now}=fixture();
 for(const alter of [r=>{r.expected.copiedPath+='/../../foreign';},r=>{r.expected.projectId='../other';},r=>{r.expected.sessionIds[2]='session-1';},r=>{r.checks.pop();},r=>{r.checks[2].result='skipped';}]){
  const bad=structuredClone(report);alter(bad);assert.throws(()=>verifyProjectReport(bad,scope,1234,now-1000));
 }
});
test('restart must preserve the exact prior project and execution identities',()=>{
 const {report,scope,now}=fixture();scope.phase='restart';scope.expected=structuredClone(report.expected);report.phase='restart';report.nativeContext.phase='restart';report.checks=[report.checks.find(c=>c.id==='real-account-and-model'),{id:'restart-project-sessions-and-files',result:'passed',evidence:{}}];
 assert.equal(verifyProjectReport(report,scope,1234,now-1000).recordId,'record');report.expected.recordId='foreign-record';assert.throws(()=>verifyProjectReport(report,scope,1234,now-1000));
});
test('project knowledge reads cannot authorize another scope or write outside the owned workspace',()=>{
 const {report,scope,now}=fixture();report.expected.projectScopeId='project-scope';
 report.permissions=[{sessionId:'session-1',via:'production allow-once UI',name:'knowledge_context',path:null,taskId:'task',scopeIds:['project-scope']}];
 verifyProjectReport(report,scope,1234,now-1000);
 for(const permission of [
  {...report.permissions[0],scopeIds:['other-scope']},
  {...report.permissions[0],scopeIds:['project-scope','other-scope']},
  {...report.permissions[0],sessionId:'foreign-session'},
  {sessionId:'session-1',via:'production allow-once UI',name:'write_file',path:report.expected.copiedPath},
  {sessionId:'session-1',via:'production allow-once UI',name:'write_file',path:'/tmp/foreign.md'},
 ]) {const bad=structuredClone(report);bad.permissions=[permission];assert.throws(()=>verifyProjectReport(bad,scope,1234,now-1000));}
});

test('the selected project review profile stays in its own private login home', { skip: process.platform !== 'darwin' }, () => {
 const home=mkdtempSync('/private/tmp/echo-native-review-project-');
 const other=mkdtempSync('/private/tmp/echo-native-review-project-');
 try {
  const profile=join(home,'organization-profile.json');
  const identity={serverUrl:'https://org.example:8787',userId:'user',username:'reviewer',deviceId:'qa-device'};
  writeFileSync(profile,JSON.stringify(identity),{mode:0o600});
  assert.deepEqual(readPreparedProjectReviewProfile(profile),{home,original:identity});
  const alias=join(other,'organization-profile.json');
  symlinkSync(profile,alias);
  assert.throws(()=>readPreparedProjectReviewProfile(alias));
  writeFileSync(join(home,'echoagent-projects.json'),'[]',{mode:0o600});
  assert.throws(()=>readPreparedProjectReviewProfile(profile),/fresh dedicated data home/);
 } finally {
  rmSync(home,{recursive:true,force:true});
  rmSync(other,{recursive:true,force:true});
 }
});
