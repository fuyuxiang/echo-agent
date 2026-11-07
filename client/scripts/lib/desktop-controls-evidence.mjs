import { createHash } from 'node:crypto';
import assert from 'node:assert/strict';
import { isReviewModelId } from './native-review-model.mjs';
export function verifyControlsModelBinding(reported, requested) {
  assert(isReviewModelId(requested), 'Desktop task requires the explicitly configured review model');
  assert([requested, requested.split('/').at(-1)].includes(reported), 'Desktop task used a different model');
}
export function verifyControlsJournal(bytes, sessionId) {
  const results=[],groups=new Map();let marker=false,terminalUsed=false,invalidNativeJson=false,assistantTail="";
  for(const line of String(bytes).split('\n').filter(Boolean)) {
    const event=JSON.parse(line),p=event.params;
    if(p?.sessionId!==sessionId)continue;
    const u=p.update;
    if(u?.sessionUpdate==='agent_message_chunk'&&u.content?.type==='text'){assistantTail=(assistantTail+u.content.text).slice(-1024);marker ||= assistantTail.includes('CONTROL-RUNTIME-READONLY');}
    const r=u?.rawOutput;
    if(r?.type==='Bash')terminalUsed=true;
    if(u?.sessionUpdate!=='tool_call_update'||u.status!=='completed'||r?.type!=='MCP'||r.server_name!=='echoagent-automation'||typeof r.output?.OkayOutput!=='string')continue;
    let output;try{output=JSON.parse(r.output.OkayOutput);}catch{invalidNativeJson=true;continue;}
    const result={toolCallId:u.toolCallId,tool:r.tool_name,server:r.server_name};
    // References are opaque identities. Both short production handles and
    // retained legacy handles are valid; length is not evidence of provenance.
    if((r.tool_name==='computer_list_apps'||r.tool_name==='computer_continue')&&Array.isArray(output.items)&&output.items.length>0&&output.probe==='succeeded'&&output.items.every(n=>n.kind==='app'&&typeof n.ref==='string'&&/^\S{1,256}$/u.test(n.ref))) {
      const id=output.resultId||u.toolCallId;
      const group=groups.get(id)||{refs:new Set(),count:0,total:output.totalItems??output.items.length,finished:false,started:false};
      for(const n of output.items)group.refs.add(n.ref);
      group.count+=output.items.length;group.finished ||= output.nextCursor==null;
      group.started ||= r.tool_name==='computer_list_apps';
      groups.set(id,group);result.nativeAppCount=output.items.length;
    }
    if(r.tool_name==='computer_capabilities'&&typeof output.available==='boolean')result.capabilityReturned=true;
    if(r.tool_name==='automation_pause'&&output.sessionId===sessionId&&output.paused===true)result.pauseVerified=true;
    results.push(result);
  }
  const completeApps=[...groups.values()].find(g=>g.started&&g.finished&&g.refs.size===g.total&&g.count===g.total);
  return {runtimeVerified:marker&&!!completeApps&&!terminalUsed&&!invalidNativeJson&&results.some(r=>r.capabilityReturned)&&results.some(r=>r.pauseVerified),
    nativeAppCount:completeApps?.total??null,terminalUsed,invalidNativeJson,
    desktopAcceptance:false,sessionId,assistantMarker:marker,journalSha256:createHash('sha256').update(bytes).digest('hex'),results};
}
