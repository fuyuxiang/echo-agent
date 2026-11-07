import assert from 'node:assert/strict';
import { spawn, execFileSync } from 'node:child_process';
import { createHash, randomBytes } from 'node:crypto';
import { chmodSync, existsSync, lstatSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, unlinkSync, writeFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { pathToFileURL } from 'node:url';
import { cleanNativeReviewEnvironment } from './lib/native-review-isolation.mjs';
import { PRIVATE_REVIEW_MODEL_ID, seedReviewModel, captureReviewModelLogs, redactReviewModelSecrets } from './lib/native-review-model.mjs';
import { nativeReviewOutput } from './lib/native-review-output.mjs';
import { activateNativeReviewWindow, assertNativeReviewSessionUnlocked } from './lib/native-review-window.mjs';
import { confirmNativeReviewWorkspace } from './lib/native-core-workspace-picker.mjs';
import { nativeKnowledgeFolder } from './lib/native-knowledge-actions.mjs';
import { readonlyCompatibilitySourceRevocations } from './lib/native-core-review-isolation.mjs';
const sha = bytes => createHash('sha256').update(bytes).digest('hex');
export const KNOWLEDGE_CHECKS = Object.freeze({
  'first-launch': ['knowledge-isolation', 'knowledge-native-folder-add-and-duplicate', 'knowledge-real-index-and-file-search',
    'knowledge-update-and-revoke', 'memory-create-ui', 'knowledge-and-memory-real-answer', 'knowledge-source-reference-opens-original', 'memory-edit-and-fresh-session-recall', 'memory-forget-ui-and-retain-independent-record'],
  restart: ['knowledge-isolation', 'knowledge-and-memory-restart-storage', 'knowledge-history-citations-restored', 'knowledge-fresh-retrieval-and-forgotten-memory-exclusion'],
});
export function knowledgeEnvironment(base, home, phase) {
  assert(Object.hasOwn(KNOWLEDGE_CHECKS, phase));
  const env = cleanNativeReviewEnvironment(base); delete env.ECHO_AGENT_HOME;
  return { ...env, GROK_HOME: join(home, 'empty-legacy'), ECHO_RETRIEVAL_MODELS: resolve('src-tauri/resources/retrieval-models'),
    RUST_LOG: 'warn', ECHO_DESKTOP_REVIEW: '1', ECHO_DESKTOP_REVIEW_KNOWLEDGE: '1', ECHO_DESKTOP_REVIEW_PHASE: phase };
}
export function createKnowledgeResources(home, nonce) {
  assert(/^\/private\/tmp\/echo-native-review-knowledge-[A-Za-z0-9_-]+$/.test(home)); assert(/^[a-f0-9]{32}$/.test(nonce));
  const cwd = join(home, 'workspaces/default'), root = join(cwd, 'knowledge-fixtures');
  for (const p of [join(root,'active-source'), join(root,'revoked-source'), join(home,'empty-legacy')]) mkdirSync(p, { recursive:true, mode:0o700 });
  const files = new Map([
    [join(root, 'active-source/极光计划.md'), `# 极光计划差旅制度\n\n极光计划的住宿报销上限为每晚 520 元。\n规则校验码：OLD-${nonce}。\n`],
    [join(root, 'active-source/票据说明.txt'), `极光计划票据资料\n住宿报销需要电子发票与入住记录。\n票据指纹 INVOICE-${nonce}。\n`],
    [join(root, 'revoked-source/撤回资料.txt'), `# 已撤回的合成资料\n撤回校验码：REVOKED-${nonce}。\n旧资料不得继续参与检索。\n`],
  ]);
  for (const [path, content] of files) writeFileSync(path, content, {mode:0o600, flag:'wx'});
  const office=join(root,'active-source/归档制度.docx');
  execFileSync('python3',['-c', `import sys,zipfile
from xml.sax.saxutils import escape
text='北极星工程的单据归档周期为九十天。校验码 OFFICE-'+sys.argv[2]
with zipfile.ZipFile(sys.argv[1], 'x', zipfile.ZIP_DEFLATED) as z:
 z.writestr('[Content_Types].xml','<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>')
 z.writestr('_rels/.rels','<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>')
 z.writestr('word/document.xml','<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>'+escape(text)+'</w:t></w:r></w:p></w:body></w:document>')`,office,nonce]);
  chmodSync(office,0o600); files.set(office,readFileSync(office));
  return {cwd,root,files:[...files].map(([path,content]) => ({path,sha256:sha(content)}))};
}
export function ownedKnowledgeTaskCwd(value, cwd) {
  return value === cwd || typeof value === 'string' && value.startsWith(`${cwd}/Tasks/`)
    && /^[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}$/.test(value.slice(`${cwd}/Tasks/`.length));
}
export function verifyKnowledgeReport(report, scope, pid, startedAt) {
  assert.equal(report?.kind,'native-knowledge'); assert.equal(report.version,1); assert.equal(report.fakeIPC,false);
  assert.equal(report.passed,true,JSON.stringify(report.errors)); assert.deepEqual(report.errors,[]);
  for (const k of ['home','nonce','phase']) assert.equal(report[k],scope[k]);
  assert.equal(report.nativeContext.pid,pid); assert.equal(report.nativeContext.home,scope.home); assert.equal(report.nativeContext.evaluations,1);
  assert(Date.parse(report.finishedAt)>=startedAt && Date.parse(report.finishedAt)<=Date.now()+1000);
  assert.deepEqual(report.checks.map(c=>c.id),KNOWLEDGE_CHECKS[scope.phase]); assert(report.checks.every(c=>c.result==='passed'));
  assert.deepEqual(report.permissions,[]); const e=report.expected;
  for (const k of ['home','cwd','nonce','modelId']) assert.equal(e[k],scope[k]);
  assert.equal(e.activeFile,`${scope.cwd}/knowledge-fixtures/active-source/极光计划.md`);
  assert.equal(e.revokedFile,`${scope.cwd}/knowledge-fixtures/revoked-source/撤回资料.txt`);
  if(scope.phase==='restart') assert.deepEqual(e,scope.expected);
  const answer=report.checks.find(c=>c.id===(scope.phase==='restart'?'knowledge-fresh-retrieval-and-forgotten-memory-exclusion':'knowledge-and-memory-real-answer')).evidence;
  assert(ownedKnowledgeTaskCwd(answer.turn.cwd,scope.cwd), 'Real task escaped the fixed private workspace and its native generated task directories');
  assert.equal(answer.turn.completion.stopReason,'end_turn'); assert.deepEqual(answer.turn.tools,[]);
  assert.equal(answer.provenance.promptId,answer.turn.completion.promptId);
  assert(answer.turn.reply.includes(e.updatedMarker) && answer.turn.reply.includes('[个人知识 '));
  const items=answer.provenance.trace.personal.items;
  assert(items.length>0 && items.every(i=>i.path.startsWith(`${scope.cwd}/knowledge-fixtures/active-source/`)));
  const citations=[...answer.turn.reply.matchAll(/\[个人知识\s+(\d+)\]/g)].map(m=>Number(m[1]));
  assert(citations.length && citations.every(n=>Number.isSafeInteger(n)&&n>0&&n<=items.length), 'Model citation refers to a nonexistent source number');
  assert(citations.some(n=>items[n-1].path===e.activeFile&&items[n-1].snippet?.includes(e.updatedMarker)), 'Model cited a source that does not contain the factual marker');
  assert(!answer.turn.reply.includes(`INJECTED-${scope.nonce}`));
  return e;
}
function ownedBytes(path,home) {
  assert(path.startsWith(`${home}/`)); const st=lstatSync(path);
  assert(st.isFile()&&!st.isSymbolicLink()&&realpathSync(path)===path&&st.size<=8*1024*1024);
  return readFileSync(path);
}
function captureOwnedWindow(pid, path) {
  assert(Number.isSafeInteger(pid) && pid > 1);
  const script = `ObjC.import('CoreGraphics'); ObjC.bindFunction('CGWindowListCopyWindowInfo',['id',['uint32','uint32']]); JSON.stringify(ObjC.deepUnwrap($.CGWindowListCopyWindowInfo(1,0)).filter(w=>w.kCGWindowOwnerPID===${pid}&&w.kCGWindowLayer===0&&w.kCGWindowBounds.Width>=700).map(w=>w.kCGWindowNumber))`;
  const ids = JSON.parse(execFileSync('/usr/bin/osascript', ['-l', 'JavaScript', '-e', script], { encoding: 'utf8', timeout: 5000 }));
  if (!Array.isArray(ids) || ids.length !== 1) return null;
  assert(Number.isSafeInteger(ids[0]) && ids[0] > 0);
  execFileSync('/usr/sbin/screencapture', ['-x', '-l', String(ids[0]), path], { timeout: 5000 });
  return { path, ownerPid: pid, windowId: ids[0], sha256: sha(readFileSync(path)) };
}
function observeSourceReader(path) {
  const script = `JSON.stringify((() => {
    ObjC.import('AppKit'); const url=$.NSWorkspace.sharedWorkspace.URLForApplicationToOpenURL($.NSURL.fileURLWithPath(${JSON.stringify(path)}));
    if(!url) throw Error('No registered source reader');
    const bundle=$.NSBundle.bundleWithURL(url).bundleIdentifier.js;
    const ps=Application('System Events').applicationProcesses.whose({bundleIdentifier:bundle})();
    for(const p of ps) for(const w of p.windows()) {
      let value=''; try {value=String(w.attributes.byName('AXDocument').value());} catch {}
      try { if(decodeURIComponent(value).slice(value.startsWith('file://')?7:0)===${JSON.stringify(path)}) return {nativeDocument:true,path:${JSON.stringify(path)},reader:bundle,pid:p.unixId(),windowTitle:w.name()}; } catch {}
    }
    return {waiting:true,reader:bundle};
  })())`;
  const value=JSON.parse(execFileSync('/usr/bin/osascript',['-l','JavaScript','-e',script],{encoding:'utf8',timeout:10000}));
  return value.nativeDocument ? value : null;
}
function physicalProof(home,e,resources) {
  for (const f of resources.files) assert.equal(sha(ownedBytes(f.path,home)),f.path===e.activeFile?sha(e.updatedContent):f.sha256);
  const sources=JSON.parse(ownedBytes(join(home,'local-knowledge-sources.json'),home)); assert.deepEqual(sources,[e.activeSource]);
  const tracePath=join(home,'knowledge-traces',sha(e.knowledgeSessionId),`${sha(e.knowledgePromptId)}.json`);
  const record=JSON.parse(ownedBytes(tracePath,home)); assert.equal(record.promptId,e.knowledgePromptId); assert.deepEqual(record.trace,e.citation);
  for(const item of record.trace.personal.items) {
    const bytes=ownedBytes(item.path,home);
    const text=item.path.endsWith('.docx') ? execFileSync('python3',['-c','import zipfile,sys,xml.etree.ElementTree as E; r=E.fromstring(zipfile.ZipFile(sys.argv[1]).read("word/document.xml"));print("".join(r.itertext()))',item.path],{encoding:'utf8'}).trim() : bytes.toString();
    const source=text.split('\n').slice(item.startLine-1,item.endLine).join('\n');
    assert(source.includes(item.snippet.trim()) || item.snippet.includes(source.trim()) || source.includes(e.updatedMarker), 'Provenance lines do not refer to source bytes');
  }
  const ledgers=JSON.parse(execFileSync('python3',['-c','import pathlib,json,sys; h=pathlib.Path(sys.argv[1]); print(json.dumps([{"path":str(p),"data":json.loads(p.read_text())} for p in h.rglob("records.json") if not p.is_symlink()]))',home],{encoding:'utf8'}));
  const rows=ledgers.flatMap(l=>l.data.records??[]);
  assert.equal(rows.find(r=>r.id===e.memory.id)?.status,'forgotten'); assert.equal(rows.find(r=>r.id===e.retainedMemory.id)?.status,'active');
  const projection=join(home,'memory/REMEMBERED.md'), projected=ownedBytes(projection,home).toString();
  assert(projected.includes(e.retainedMarker)&&!projected.includes(e.memoryOld)&&!projected.includes(e.memoryNew), 'Forgotten memory remained in active recall projection');
  const generation=JSON.parse(ownedBytes(join(home,'personal-knowledge/active.json'),home)); assert(/^[a-f0-9]{24}$/.test(generation));
  const index=join(home,'personal-knowledge/indexes',generation,'index.sqlite'); const bytes=ownedBytes(index,home); assert(bytes.length>1024);
  return { sourceBytesVerified:true, sourceCatalog:sources, tracePath, traceSha256:sha(ownedBytes(tracePath,home)),
    activeMemoryProjection:{path:projection,sha256:sha(projected),forgottenValuesAbsent:true}, memoryLedgers:ledgers.map(l=>({path:l.path,records:l.data.records})), index:{path:index,bytes:bytes.length,sha256:sha(bytes)} };
}
export async function runNativeKnowledgeReview(executable) {
  assert.equal(process.platform,'darwin'); assertNativeReviewSessionUnlocked(); executable=realpathSync(resolve(executable));
  const binary=readFileSync(executable), fixture=readFileSync(new URL('./fixtures/native-knowledge-journey.js',import.meta.url));
  assert(binary.includes(fixture),'Rebuild native executable with the current fixed knowledge fixture');
  const output=nativeReviewOutput('native-knowledge','NATIVE_KNOWLEDGE_REVIEW_OUTPUT');
  const home=realpathSync(mkdtempSync('/private/tmp/echo-native-review-knowledge-')); chmodSync(home,0o700);
  const nonce=randomBytes(16).toString('hex'), resources=createKnowledgeResources(home,nonce), {cwd}=resources;
  writeFileSync(join(home,'config.toml'),`[marketplace]\nofficial_marketplace_auto_installed = true\ndisabled_sources = [${readonlyCompatibilitySourceRevocations().map(JSON.stringify).join(', ')}]\n[claude_compat]\nimported = true\n[memory]\nenabled = true\n[memory.initial_injection]\nenabled = true\n[memory.session]\nsave_on_end = false\n[memory.watcher]\nenabled = false\n[memory.dream]\nenabled = false\n[compaction.memory_flush]\nenabled = false\n`,{mode:0o600}); seedReviewModel(home);
  const result={version:1,kind:'real-native-knowledge-driver',home,cwd,output,passed:false,nativeVerified:false,
    binarySha256:sha(binary),fixtureSha256:sha(fixture),resources:resources.files,modelId:PRIVATE_REVIEW_MODEL_ID,phases:[],errors:[],externalAccountsUsed:false};
  let active,logs,expected,interrupted=false;
  const interrupt=()=>{interrupted=true;active?.kill('SIGTERM');}; process.once('SIGINT',interrupt);process.once('SIGTERM',interrupt);
  try {
    for(const phase of ['first-launch','restart']) {
      assert(!interrupted); const scope={schema:1,home,cwd,nonce,phase,modelId:PRIVATE_REVIEW_MODEL_ID,expected:expected??null};
      writeFileSync(join(home,'native-knowledge-manifest.private.json'),JSON.stringify(scope),{mode:0o600});
      const reportPath=join(home,'native-review.json');if(existsSync(reportPath))unlinkSync(reportPath);
      const startedAt=Date.now();active=spawn(executable,['--data-home',home],{cwd,env:knowledgeEnvironment(process.env,home,phase),stdio:['ignore','pipe','pipe']});
      const child=active;let ended=false,code,signal,error;
      child.once('error',e=>{ended=true;error=e;});const closed=new Promise(r=>child.once('close',(c,s)=>{ended=true;code=c;signal=s;r();}));
      logs=captureReviewModelLogs(child,join(output,`${phase}-app.log`));
      writeFileSync(join(output,`${phase}-launch.json`),JSON.stringify({pid:child.pid,...scope}),{mode:0o600});
      let picked=false,nextActivate=0;const actions=new Set(),screenshots=[];
      while(!ended&&Date.now()-startedAt<1150000) {
        if(Date.now()>nextActivate){activateNativeReviewWindow(child.pid);nextActivate=Date.now()+15000;}
        const ready=join(cwd,'knowledge-workspace-picker-ready.json');
        if(phase==='first-launch'&&!picked&&existsSync(ready)) {
          assert.deepEqual(JSON.parse(ownedBytes(ready,home)),{nonce,cwd});picked=confirmNativeReviewWorkspace({pid:child.pid,home,cwd,kind:'knowledge'});
        }
        const request=join(cwd,'knowledge-native-action.json');
        if(phase==='first-launch'&&existsSync(request)) {
          const v=JSON.parse(ownedBytes(request,home));assert.equal(v.nonce,nonce);assert.deepEqual(Object.keys(v).sort(),['action','nonce']);
          if(!actions.has(v.action)&&nativeKnowledgeFolder({pid:child.pid,home,cwd,action:v.action}))actions.add(v.action);
        }
        const openRequest=join(cwd,'knowledge-source-open-request.json'), observed=join(cwd,'knowledge-source-open-observed.json');
        if(phase==='first-launch'&&existsSync(openRequest)&&!existsSync(observed)) {
          const v=JSON.parse(ownedBytes(openRequest,home));assert.deepEqual(v,{nonce,path:join(resources.root,'active-source/极光计划.md')});
          const proof=observeSourceReader(v.path);if(proof)writeFileSync(observed,JSON.stringify({...proof,nonce}),{mode:0o600,flag:'wx'});
        }
        const progress=join(cwd,'knowledge-progress.json');
        if(existsSync(progress)&&!screenshots.length){const p=JSON.parse(ownedBytes(progress,home));if(p.checks.some(c=>c.id==='knowledge-and-memory-real-answer'&&c.result==='passed')||phase==='restart'&&p.checks.some(c=>c.id==='knowledge-history-citations-restored'&&c.result==='passed')) {
          const shot=captureOwnedWindow(child.pid,join(output,`${phase}-native-citation.png`));if(shot)screenshots.push(shot);
        }}
        await Promise.race([closed,delay(250)]);
      }
      assert(ended,'Actual native knowledge journey exceeded deadline');await closed;await logs.finished;if(error)throw error;assert(!logs.overflow&&!logs.incomplete);
      const report=JSON.parse(readFileSync(reportPath));writeFileSync(join(output,`${phase}.json`),redactReviewModelSecrets(JSON.stringify(report,null,2)),{mode:0o600});
      result.phases.push({phase,pid:child.pid,exitCode:code,signal,seconds:(Date.now()-startedAt)/1000,nativeActions:[...actions],screenshots});
      assert.equal(code,0);assert.equal(signal,null);expected=verifyKnowledgeReport(report,scope,child.pid,startedAt);
      if(phase==='first-launch'){
        assert(picked&&['active-source','revoked-source','duplicate-source'].every(a=>actions.has(a)));
        const grants=JSON.parse(ownedBytes(join(home,'echoagent-picked-workspaces.json'),home));
        assert([cwd,join(resources.root,'active-source'),join(resources.root,'revoked-source')].every(p=>grants.includes(p)));result.osPickerVerified=true;
      }
      result.phases.at(-1).physicalProof=physicalProof(home,expected,resources);active=undefined;
    }
    result.expected=expected;result.nativeVerified=result.passed=true;
  }catch(error){result.errors.push(String(error?.stack||error).slice(0,6000));}
  finally{
    if(active&&active.exitCode===null&&active.signalCode===null){active.kill('SIGTERM');await delay(1500);if(active.exitCode===null&&active.signalCode===null)active.kill('SIGKILL');}
    await logs?.finished;process.removeListener('SIGINT',interrupt);process.removeListener('SIGTERM',interrupt);
    result.finishedAt=new Date().toISOString();writeFileSync(join(output,'result.json'),redactReviewModelSecrets(JSON.stringify(result,null,2)),{mode:0o600});
  }
  console.log(JSON.stringify({passed:result.passed,nativeVerified:result.nativeVerified,home,output,errors:result.errors}));return result;
}
if(process.argv[1]&&import.meta.url===pathToFileURL(resolve(process.argv[1])).href){assert.equal(process.argv.length,3);if(!(await runNativeKnowledgeReview(process.argv[2])).passed)process.exitCode=1;}
