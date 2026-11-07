// Supplemental direct-child exit receipt for an already completed native soak.
import assert from 'node:assert/strict';
import {spawn,execFileSync} from 'node:child_process';
import {createHash} from 'node:crypto';
import {existsSync,lstatSync,readFileSync,realpathSync,writeFileSync,unlinkSync} from 'node:fs';
import {join,resolve,dirname} from 'node:path';
import {setTimeout as delay} from 'node:timers/promises';
import {lifecycleEnvironment,lifecycleWorkspace,verifyLifecycleReport} from './validate-native-lifecycle-review.mjs';
import {captureReviewModelLogs} from './lib/native-review-model.mjs';
import {activateNativeReviewWindow,assertNativeReviewSessionUnlocked} from './lib/native-review-window.mjs';
assert.equal(process.argv.length,4);
assertNativeReviewSessionUnlocked();
const resultPath=realpathSync(resolve(process.argv[2])),executable=realpathSync(resolve(process.argv[3]));
const prior=JSON.parse(readFileSync(resultPath));
assert.equal(prior.kind,'real-native-lifecycle-driver');assert(prior.passed&&prior.nativeVerified);
assert.equal(createHash('sha256').update(readFileSync(executable)).digest('hex'),prior.binarySha256);
const {home,cwd,nonce,expected,modelId,output}=prior;assert.equal(dirname(resultPath),output);
assert(/^\/private\/tmp\/echo-native-review-lifecycle-[A-Za-z0-9_-]+$/.test(home));
assert.equal(cwd,lifecycleWorkspace(nonce));
for(const p of [home,cwd]){const st=lstatSync(p);assert(st.isDirectory()&&!st.isSymbolicLink()&&st.uid===process.getuid()&&!(st.mode&0o077)&&realpathSync(p)===p);}
assert.equal(expected.home,home);assert.equal(expected.cwd,cwd);assert.equal(expected.nonce,nonce);
const scope={schema:1,home,cwd,nonce,modelId,phase:'restart',cycle:'final',expected};
const reportPath=join(home,'native-review.json');if(existsSync(reportPath))unlinkSync(reportPath);
writeFileSync(join(home,'native-lifecycle-manifest.private.json'),JSON.stringify(scope),{mode:0o600});
const artifact=join(cwd,'lifecycle-result.txt'),snapshot=()=>({sha256:createHash('sha256').update(readFileSync(artifact)).digest('hex'),mtimeMs:lstatSync(artifact).mtimeMs,size:lstatSync(artifact).size});
assert.deepEqual(snapshot(),prior.artifactAfterFinal);
const receipt={kind:'native-lifecycle-direct-exit',home,cwd,nonce,passed:false,startedAt:new Date().toISOString(),errors:[]};
let closed=false;
const child=spawn(executable,['--data-home',home],{cwd,env:lifecycleEnvironment(process.env,home,'restart'),stdio:['ignore','pipe','pipe']});
child.once('error',e=>{receipt.errors.push(String(e));closed=true;});
child.once('close',(exitCode,signal)=>{receipt.exitCode=exitCode;receipt.signal=signal;closed=true;});
receipt.pid=child.pid;
const logs=captureReviewModelLogs(child,join(output,'final-direct-exit-app.log'));
try{
 const end=Date.now()+120000;let nextActivate=0;
 while(!closed&&Date.now()<end){if(Date.now()>nextActivate){activateNativeReviewWindow(child.pid);nextActivate=Date.now()+15000;}await delay(200);}
 assert(closed,'Direct restart did not finish');await logs.finished;
 assert.equal(receipt.exitCode,0);assert.equal(receipt.signal,null);assert(!logs.overflow&&!logs.incomplete);
 const host=JSON.parse(readFileSync(join(home,'native-lifecycle-host.private.json')));assert.equal(host.pid,child.pid);assert.equal(host.nonce,nonce);assert.equal(host.restored,false);
 const report=JSON.parse(readFileSync(reportPath));verifyLifecycleReport(report,scope,child.pid);
 assert.deepEqual(snapshot(),prior.artifactAfterFinal);
 receipt.report=report;receipt.passed=true;
}catch(error){receipt.errors.push(String(error.stack??error));}
finally{
 if(!closed){writeFileSync(join(home,'native-lifecycle-stop.private.json'),JSON.stringify({nonce}),{mode:0o600});for(let n=0;n<50&&!closed;n++)await delay(200);if(!closed){try{const h=JSON.parse(readFileSync(join(home,'native-lifecycle-host.private.json')));if(h.pid===child.pid&&h.nonce===nonce&&Number.isSafeInteger(h.watcherPid)){const cmd=execFileSync('/bin/ps',['-p',String(h.watcherPid),'-o','command='],{encoding:'utf8',timeout:3000});if(cmd.includes(executable)&&cmd.includes('--echoagent-crash-watchdog'))process.kill(h.watcherPid,'SIGKILL');}}catch{}child.kill('SIGKILL');}}
 receipt.finishedAt=new Date().toISOString();writeFileSync(join(output,'final-direct-exit.json'),JSON.stringify(receipt,null,2),{mode:0o600});
}
console.log(JSON.stringify({passed:receipt.passed,pid:child.pid,exitCode:receipt.exitCode,signal:receipt.signal,errors:receipt.errors}));
if(!receipt.passed)process.exitCode=1;
