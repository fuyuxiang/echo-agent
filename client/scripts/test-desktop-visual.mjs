import { mkdtempSync, mkdirSync, readFileSync, writeFileSync, cpSync, existsSync, realpathSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { spawnSync } from 'node:child_process';
import assert from 'node:assert/strict';
assert.equal(process.platform,'darwin');
const directory=realpathSync(mkdtempSync(join(tmpdir(),'echo-visual-native-')));
const identity=directory.split('/').at(-1).replaceAll('-','').toLowerCase();
const run=(command,args,options={})=>{const r=spawnSync(command,args,{stdio:'inherit',...options});assert.equal(r.status,0,`${command} failed (${r.status})`);};
console.log(`Opaque native GUI evidence: ${directory}`);
run('/usr/bin/swiftc',['scripts/fixtures/native-canvas-app.swift','-o',join(directory,'fixture')],{timeout:120000});
try {
  for(const role of ['canvas','other']) {
    const contents=join(directory,`${role}.app`,'Contents');mkdirSync(join(contents,'MacOS'),{recursive:true});
    cpSync(join(directory,'fixture'),join(contents,'MacOS','fixture'));
    writeFileSync(join(contents,'Info.plist'),`<?xml version="1.0"?><plist version="1.0"><dict><key>CFBundleExecutable</key><string>fixture</string><key>CFBundleIdentifier</key><string>com.echoagent.fixture.canvas.${role}.${identity}</string><key>CFBundleName</key><string>Echo opaque ${role}</string><key>CFBundlePackageType</key><string>APPL</string><key>NSHighResolutionCapable</key><true/></dict></plist>`);
  }
  run('cargo',['test','--locked','--offline','--manifest-path','scripts/desktop-native-check/Cargo.toml','actual_opaque_canvas_flow','--','--ignored','--nocapture'],{env:{...process.env,ECHO_NATIVE_VISUAL_FIXTURES:directory},timeout:180000});
  console.log(JSON.stringify({passed:true,directory,kind:'real-opaque-native-gui'}));
} finally {
  for(const role of ['canvas','other']) {
    const file=join(directory,`${role}-pid`);if(!existsSync(file))continue;
    const pid=Number(readFileSync(file,'utf8'));if(!Number.isSafeInteger(pid)||pid<=1)continue;
    const comm=spawnSync('/bin/ps',['-p',String(pid),'-o','comm='],{encoding:'utf8'}).stdout?.trim();
    if(comm&&existsSync(comm)&&realpathSync(comm)===realpathSync(join(directory,`${role}.app`,'Contents','MacOS','fixture')))try{process.kill(pid,'SIGTERM');}catch{}
  }
}
