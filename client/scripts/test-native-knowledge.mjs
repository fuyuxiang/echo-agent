import assert from 'node:assert/strict';
import { test } from 'node:test';
import { mkdtempSync, realpathSync, chmodSync, readFileSync, rmSync } from 'node:fs';
import { createKnowledgeResources, knowledgeEnvironment, verifyKnowledgeReport, KNOWLEDGE_CHECKS, ownedKnowledgeTaskCwd } from './validate-native-knowledge-review.mjs';
import { nativeKnowledgeFolder } from './lib/native-knowledge-actions.mjs';
const nonce='0123456789abcdef0123456789abcdef';
function profile(){const home=realpathSync(mkdtempSync('/private/tmp/echo-native-review-knowledge-'));chmodSync(home,0o700);return home;}
test('knowledge review clears ambient provider keys, user profiles and competing native suites',()=>{
 const env=knowledgeEnvironment({HOME:'/real/home',OPENAI_API_KEY:'secret',ANTHROPIC_API_KEY:'secret',ECHO_AGENT_HOME:'/real/profile',ECHO_DESKTOP_REVIEW_CORE:'1',NODE_OPTIONS:'--require=foreign'},'/private/tmp/owned','restart');
 assert.equal(env.HOME,'/real/home');for(const k of ['OPENAI_API_KEY','ANTHROPIC_API_KEY','ECHO_AGENT_HOME','ECHO_DESKTOP_REVIEW_CORE','NODE_OPTIONS'])assert(!Object.hasOwn(env,k));
 assert.equal(env.ECHO_DESKTOP_REVIEW_KNOWLEDGE,'1');assert.equal(env.ECHO_DESKTOP_REVIEW_PHASE,'restart');assert.equal(env.GROK_HOME,'/private/tmp/owned/empty-legacy');
});
test('knowledge resource generation rejects ordinary user directory and invalid nonce',()=>{
 assert.throws(()=>createKnowledgeResources('/Users/example',nonce));assert.throws(()=>createKnowledgeResources('/private/tmp/echo-native-review-knowledge-test','bad'));
});
test('synthetic documents contain hidden independent old and revoked markers',()=>{const h=profile();try{
 const r=createKnowledgeResources(h,nonce);assert.equal(r.files.length,4);assert(readFileSync(r.files[0].path,'utf8').includes(`OLD-${nonce}`));assert(readFileSync(r.files[2].path,'utf8').includes(`REVOKED-${nonce}`));
 assert.throws(()=>createKnowledgeResources(h,nonce),/EEXIST/);
}finally{rmSync(h,{recursive:true});}});
test('folder automation rejects other application PID, user home and arbitrary target before AppleScript',()=>{
 let runs=0;const run=()=>{runs++;throw Error('must not run');};
 for(const value of [{pid:1,home:'/private/tmp/echo-native-review-knowledge-x',action:'active-source'}, {pid:234,home:'/Users/x',action:'active-source'}, {pid:234,home:'/private/tmp/echo-native-review-knowledge-x',action:'../other'}])assert.throws(()=>nativeKnowledgeFolder({...value,cwd:`${value.home}/workspaces/default`},run));
 assert.equal(runs,0);
});
test('folder automation always navigates the entire owned path and binds returned PID / target',()=>{
 const h=profile();try{const r=createKnowledgeResources(h,nonce);let called=0;
 const run=(cmd,args)=>{called++;assert.equal(cmd,'/usr/bin/osascript');const s=args.at(-1);assert(s.includes(`${r.root}/active-source`));assert(s.includes('unixId:234'));assert(s.includes("se.keystroke('g'"));return JSON.stringify({performed:true,pid:234,action:'duplicate-source',path:`${r.root}/active-source`});};
 assert(nativeKnowledgeFolder({pid:234,home:h,cwd:r.cwd,action:'duplicate-source'},run));assert.equal(called,1);
 assert.throws(()=>nativeKnowledgeFolder({pid:234,home:h,cwd:r.cwd,action:'active-source'},()=>JSON.stringify({performed:true,pid:999,action:'active-source',path:`${r.root}/active-source`})));
 }finally{rmSync(h,{recursive:true});}
});
function valid(){const home='/private/tmp/echo-native-review-knowledge-x',cwd=`${home}/workspaces/default`,phase='restart',modelId='echoagent-review/claude-opus-5-5',pid=234;
 const e={home,cwd,nonce,modelId,activeFile:`${cwd}/knowledge-fixtures/active-source/极光计划.md`,revokedFile:`${cwd}/knowledge-fixtures/revoked-source/撤回资料.txt`,updatedMarker:'updated'};
 const turn={cwd,completion:{sessionId:'s',promptId:'p',stopReason:'end_turn'},tools:[],reply:'updated [个人知识 1]'};
 const provenance={promptId:'p',trace:{personal:{items:[{path:e.activeFile,snippet:'updated'}]}}};
 const report={version:1,kind:'native-knowledge',home,nonce,phase,fakeIPC:false,passed:true,errors:[],permissions:[],nativeContext:{pid,home,evaluations:1},finishedAt:new Date().toISOString(),expected:e,
 checks:KNOWLEDGE_CHECKS.restart.map(id=>({id,result:'passed',evidence:id==='knowledge-fresh-retrieval-and-forgotten-memory-exclusion'?{turn,provenance}:{}}))};
 return {scope:{home,cwd,nonce,phase,modelId,expected:e},report,pid};}
for(const [name,mutate] of [
 ['fake IPC',r=>{r.fakeIPC=true;}],['foreign PID',r=>{r.nativeContext.pid=999;}],['missing phase checks',r=>{r.checks.pop();}],['another prompt citation',r=>{r.checks.at(-1).evidence.provenance.promptId='other';}],['revoked source citation',r=>{r.checks.at(-1).evidence.provenance.trace.personal.items[0].path=r.expected.revokedFile;}],['nonexistent numbered citation',r=>{r.checks.at(-1).evidence.turn.reply='updated [个人知识 9]';}],['wrong numbered source',r=>{r.checks.at(-1).evidence.provenance.trace.personal.items[0].snippet='unrelated';}],['unexpected Runtime tool',r=>{r.checks.at(-1).evidence.turn.tools=[{toolCallId:'foreign'}];}],
])test(`knowledge acceptance rejects ${name}`,()=>{const {scope,report,pid}=valid();mutate(report);assert.throws(()=>verifyKnowledgeReport(report,scope,pid,Date.now()-1000));});
test('knowledge acceptance qualifies a complete bound provenance report',()=>{const {scope,report,pid}=valid();assert.equal(verifyKnowledgeReport(report,scope,pid,Date.now()-1000),scope.expected);});

test('generated task scopes accept only a direct UUID task below the private workspace',()=>{
 const cwd='/private/tmp/echo-native-review-knowledge-owned/workspaces/default', id='01234567-89ab-4cde-8f01-23456789abcd';
 assert(ownedKnowledgeTaskCwd(cwd,cwd)); assert(ownedKnowledgeTaskCwd(`${cwd}/Tasks/${id}`,cwd));
 for(const target of [`/Users/example/Tasks/${id}`,`${cwd}/Tasks/../other`,`${cwd}/Tasks/${id}/child`,`${cwd}/Tasks/not-a-uuid`,`${cwd}-other/Tasks/${id}`])assert(!ownedKnowledgeTaskCwd(target,cwd));
});
