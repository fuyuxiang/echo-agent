import {describe,it,expect} from "vitest";
import {computerOutcome} from "../computer-outcome";
import type {ToolCallView} from "@/stores/session-store";
const call=(value:unknown,name="computer_invoke"):ToolCallView=>({toolCallId:"a",title:name,kind:"other",status:"completed",rawInput:{tool_name:`echoagent-automation__${name}`},content:[{type:"text",text:JSON.stringify(value)}]});
describe("desktop physical outcome",()=>{
  it("ignores malformed wrapped tool names without crashing the conversation",()=>{
    expect(computerOutcome({...call({status:"observed"},"web_search"),rawInput:{tool_name:["computer_click"]}})).toBeNull();
  });
  it("keeps a failed helper transport distinct from a proven non-dispatch",()=>{
    const tool={...call(null),status:"failed" as const,content:[{type:"text" as const,text:"native_timeout: isolated helper terminated; action result unknown"}]};
    expect(computerOutcome(tool)).toMatchObject({unknown:true,label:"结果待核验",pendingVerification:true});
    expect(computerOutcome({...tool,rawInput:{tool_name:"echoagent-automation__computer_context"}})).toBeNull();
  });
  it("does not label an unknown physical result completed",()=>{expect(computerOutcome(call({status:"result_unknown",executed:null,verified:false}))).toMatchObject({unknown:true,label:"结果待核验"});});
  it("separates dispatch and verified readback",()=>{expect(computerOutcome(call({status:"dispatched_unverified",keyDispatched:true}))).toMatchObject({label:"已派发，待核验",verified:false});expect(computerOutcome(call({status:"verified",verified:true,receiptId:"proof"}))).toMatchObject({label:"已核验",receiptId:"proof"});});
  it("shows task completion only from the host's completed task outcome",()=>{expect(computerOutcome(call({task:{phase:"completed"}},"automation_complete"))).toMatchObject({verified:true,label:"成果已核验",pendingVerification:false});});
  it("does not interpret web text as a computer result",()=>{expect(computerOutcome(call({status:"verified",verified:true},"web_search"))).toBeNull();});
});
