import { test } from 'node:test';
import assert from 'node:assert/strict';
import { verifyControlsJournal, verifyControlsModelBinding } from './lib/desktop-controls-evidence.mjs';
test('task evidence binds the explicitly selected model and its persisted alias',()=>{
  const selected='echoagent-review/claude-opus-5-5';
  for(const value of [selected,'claude-opus-5-5'])verifyControlsModelBinding(value,selected);
  assert.throws(()=>verifyControlsModelBinding('MiniMax-M3.1-Flash',selected));
  assert.throws(()=>verifyControlsModelBinding('another/claude-opus-5-5',selected));
  assert.throws(()=>verifyControlsModelBinding('foreign','foreign'));
});
const session='synthetic-session';
const event=(tool,output,extra={})=>({params:{sessionId:session,update:{sessionUpdate:'tool_call_update',status:'completed',toolCallId:tool,rawOutput:{type:'MCP',server_name:'echoagent-automation',tool_name:tool,output:{OkayOutput:JSON.stringify(output)}},...extra}}});
const mark={params:{sessionId:session,update:{sessionUpdate:'agent_message_chunk',content:{type:'text',text:'CONTROL-RUNTIME-READONLY'}}}};
const valid=[event('computer_capabilities',{available:true}),event('computer_list_apps',{probe:'succeeded',items:[{kind:'app',ref:'synthetic-opaque-reference-12345'}]}),event('automation_pause',{paused:true,sessionId:session}),mark];
const check=events=>verifyControlsJournal(events.map(e=>JSON.stringify(e)).join('\n'),session);
test('uses real MCP rawOutput rather than assuming text content',()=>assert.equal(check(valid).runtimeVerified,true));
test('search results mentioning tool names are not native results',()=>{const v=structuredClone(valid);v[1].params.update.rawOutput.type='SearchTool';assert.equal(check(v).runtimeVerified,false);});
test('another session cannot supply proof',()=>{const v=structuredClone(valid);v[1].params.sessionId='foreign';assert.equal(check(v).runtimeVerified,false);});
test('failed native output cannot pass',()=>{const v=structuredClone(valid);v[1].params.update.rawOutput.output={Error:'native failure'};assert.equal(check(v).runtimeVerified,false);});
test('a marker in the user prompt cannot pass',()=>{const v=structuredClone(valid);v[3].params.update.sessionUpdate='user_message_chunk';assert.equal(check(v).runtimeVerified,false);});
test('incomplete calls cannot pass',()=>{const v=structuredClone(valid);v[1].params.update.status='in_progress';assert.equal(check(v).runtimeVerified,false);});
test('requires every page with unique native references',()=>{
  const first=event('computer_list_apps',{resultId:'pages',probe:'succeeded',totalItems:2,nextCursor:'next',items:[{kind:'app',ref:'synthetic-opaque-reference-12345'}]});
  const last=event('computer_continue',{sourceTool:'computer_list_apps',resultId:'pages',probe:'succeeded',totalItems:2,nextCursor:null,items:[{kind:'app',ref:'synthetic-opaque-reference-67890'}]});
  assert.equal(check([valid[0],first,last,...valid.slice(2)]).runtimeVerified,true);
  assert.equal(check([valid[0],first,...valid.slice(2)]).runtimeVerified,false);
  assert.equal(check([valid[0],last,...valid.slice(2)]).runtimeVerified,false);
  last.params.update.rawOutput.output.OkayOutput=first.params.update.rawOutput.output.OkayOutput.replace('"next"','null');
  assert.equal(check([valid[0],first,last,...valid.slice(2)]).runtimeVerified,false);
});
test('terminal fallback and malformed Runtime truncation fail qualification',()=>{
  const bash={params:{sessionId:session,update:{rawOutput:{type:'Bash'}}}};
  assert.equal(check([...valid,bash]).runtimeVerified,false);
  const broken=structuredClone(valid[1]);broken.params.update.rawOutput.output.OkayOutput='{"items":[... [MCP output truncated]';
  assert.equal(check([...valid,broken]).runtimeVerified,false);
});
test('short production handles and legacy handles both qualify without weakening provenance',()=>{
  for(const ref of ['a_0123456789ab','019abcdef-legacy-opaque-reference']) {
    const v=structuredClone(valid);
    v[1].params.update.rawOutput.output.OkayOutput=JSON.stringify({sourceTool:'computer_list_apps',probe:'succeeded',resultId:'page',totalItems:1,nextCursor:null,items:[{kind:'app',ref}]});
    assert.equal(check(v).runtimeVerified,true);
    v[1].params.update.rawOutput.server_name='another-server';
    assert.equal(check(v).runtimeVerified,false);
  }
});
test('empty, whitespace and unbounded references never qualify',()=>{
  for(const ref of ['', ' ', 'a_123\n', 'x'.repeat(257), null, 14]) {
    const v=structuredClone(valid);
    v[1].params.update.rawOutput.output.OkayOutput=JSON.stringify({probe:'succeeded',items:[{kind:'app',ref}]});
    assert.equal(check(v).runtimeVerified,false);
  }
});
