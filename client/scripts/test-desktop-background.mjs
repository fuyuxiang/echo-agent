import { mkdtempSync, mkdirSync, readFileSync, writeFileSync, cpSync, existsSync, realpathSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { spawnSync } from 'node:child_process';
import assert from 'node:assert/strict';
assert.equal(process.platform,'darwin','This acceptance requires a real macOS graphical session');
const directory=realpathSync(mkdtempSync(join(tmpdir(),'echo-background-native-')));
const identity=directory.split('/').at(-1).replaceAll('-','').toLowerCase();
const binary=join(directory,'fixture');
const run=(command,args,options={})=>{const result=spawnSync(command,args,{stdio:'inherit',...options});assert.equal(result.status,0,`${command} failed (${result.status})`);};
console.log(`Isolated native background evidence: ${directory}`);
run('/usr/bin/swiftc',['scripts/fixtures/native-background-app.swift','-o',binary],{timeout:120000});
try {
  for (const role of ['target','other']) {
    const app=join(directory,`${role}.app`), contents=join(app,'Contents');
    mkdirSync(join(contents,'MacOS'),{recursive:true});cpSync(binary,join(contents,'MacOS','fixture'));
    writeFileSync(join(contents,'Info.plist'),`<?xml version="1.0"?><plist version="1.0"><dict><key>CFBundleExecutable</key><string>fixture</string><key>CFBundleIdentifier</key><string>com.echoagent.fixture.background.${role}.${identity}</string><key>CFBundleName</key><string>Echo background ${role}</string><key>CFBundlePackageType</key><string>APPL</string><key>NSHighResolutionCapable</key><true/></dict></plist>`);
  }
  run('cargo',['test','--locked','--offline','--manifest-path','scripts/desktop-native-check/Cargo.toml','actual_background_roundtrip','--','--ignored','--nocapture'],
    {env:{...process.env,ECHO_NATIVE_BACKGROUND_FIXTURES:directory},timeout:180000});
  console.log(JSON.stringify({passed:true,directory,kind:'real-native-isolated-apps'}));
} finally {
  for (const role of ['target','other']) {
    const file=join(directory,`${role}-pid`);if(!existsSync(file))continue;
    const pid=Number(readFileSync(file,'utf8'));
    if(!Number.isSafeInteger(pid)||pid<=1)continue;
    const command=spawnSync('/bin/ps',['-p',String(pid),'-o','comm='],{encoding:'utf8'}).stdout?.trim();
    if(command && existsSync(command) && realpathSync(command)===realpathSync(join(directory,`${role}.app`,'Contents','MacOS','fixture'))) {
      try{process.kill(pid,'SIGTERM');}catch{}
    }
  }
}
