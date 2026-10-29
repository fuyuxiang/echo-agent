import { act, renderHook, waitFor } from "@testing-library/react";
import { beforeEach, expect, it, vi } from "vitest";
const native = vi.hoisted(()=>({send:vi.fn(),state:vi.fn(),followup:vi.fn(),interrupt:vi.fn()}));
vi.mock("@/lib/agent-client",()=>({agentSend:native.send}));
vi.mock("../lib/tauri-api",()=>({codingApi:{orchestratorState:native.state,beginFollowup:native.followup,reportInterrupted:native.interrupt}}));
import { claimCodingAdmission, sendCodingQueuedFollowup, settleCodingQueueTurn } from "../lib/coding-followup";
import { useCodingFollowupQueue } from "../hooks/useCodingFollowupQueue";
import { useMessageQueueStore } from "@/stores/message-queue-store";
import { useSessionStore } from "@/stores/session-store";
import { usePermissionStore } from "@/stores/permission-store";
import { useQuestionStore } from "@/stores/question-store";

beforeEach(()=>{
  localStorage.clear();vi.clearAllMocks();
  useMessageQueueStore.setState({queues:{}});
  useSessionStore.setState({sessionId:null,transcripts:{},error:null});
  useSessionStore.getState().setSession("sA");
  usePermissionStore.setState({queues:{}});useQuestionStore.setState({queues:{}});
  native.state.mockResolvedValue({task:{id:"A",sessionId:"sA",phase:"delivered"}});
  native.followup.mockResolvedValue({id:"A",sessionId:"sA",phase:"discovering",updatedAt:"reopened-revision"});native.interrupt.mockResolvedValue(undefined);
  native.send.mockResolvedValue(undefined);
});
const admission = (promptId: string, prepare?:()=>Promise<void>, canDispatch=()=>true) => sendCodingQueuedFollowup({
  root:"/repo",taskId:"A",sessionId:"sA",promptId,text:"完善边界处理",contextPaths:["src/a.ts"],manual:false,prepare,canDispatch,
});

it("keeps a background outbox after local admission and a later native rejection", async()=>{
  let rejectNative!: (error:Error)=>void;
  native.send.mockImplementation(()=>new Promise<void>((_,reject)=>{rejectNative=reject;}));
  const options={workspace:"/repo",taskId:"A",sessionId:"sA",autoReady:true,manualReady:true,
    send:(_text:string,_paths:string[],promptId:string)=>admission(promptId)};
  useSessionStore.getState().setSession("other");
  const view=renderHook(()=>useCodingFollowupQueue(options));
  act(()=>{view.result.current.enqueue("完善边界处理",["src/a.ts"]);});
  await waitFor(()=>expect(native.send).toHaveBeenCalledOnce());
  const key=view.result.current.queueId!;
  const promptId=useMessageQueueStore.getState().getQueue(key)[0].promptId;
  expect(native.send).toHaveBeenCalledWith("sA",expect.stringContaining("src/a.ts"),[],"完善边界处理",promptId,{mode:"agent",documentToolBridge:true,workspace:"/repo",taskId:"A"});
  expect(useMessageQueueStore.getState().getQueue(key)[0].status).toBe("sending");
  await act(async()=>rejectNative(new Error("原生发送通道断开")));
  await waitFor(()=>expect(useMessageQueueStore.getState().getQueue(key)[0]).toMatchObject({status:"paused",recovery:"uncertain",text:"完善边界处理"}));
  expect(useSessionStore.getState().sessionId).toBe("other");
  expect(useSessionStore.getState().error).toBeNull();
  expect(native.send).toHaveBeenCalledOnce();
});

it("does not resurrect a successful row after a late transport rejection", async()=>{
  let rejectNative!: (error:Error)=>void;
  native.send.mockImplementation(()=>new Promise<void>((_,reject)=>{rejectNative=reject;}));
  const view=renderHook(()=>useCodingFollowupQueue({workspace:"/repo",taskId:"A",sessionId:"sA",autoReady:true,manualReady:true,
    send:(_text,_paths,promptId)=>admission(promptId)}));
  act(()=>{view.result.current.enqueue("完善边界处理",[]);});
  await waitFor(()=>expect(native.send).toHaveBeenCalledOnce());
  const key=view.result.current.queueId!;
  const promptId=useMessageQueueStore.getState().getQueue(key)[0].promptId!;
  await act(async()=>{
    const completion={sessionId:"sA",promptId,stopReason:"end_turn"};
    settleCodingQueueTurn(completion);
    useSessionStore.getState().markComplete(completion);
  });
  await waitFor(()=>expect(useMessageQueueStore.getState().getQueue(key)).toEqual([]));
  await act(async()=>rejectNative(new Error("迟到的断线")));
  expect(useMessageQueueStore.getState().getQueue(key)).toEqual([]);
  expect(useSessionStore.getState().error).toBeNull();
});

it("rechecks cancellation and decisions after the editor acknowledgment", async()=>{
  let finish!: ()=>void;let allowed=true;
  const pending=admission("p",()=>new Promise<void>(resolve=>{finish=resolve;}),()=>allowed);
  allowed=false;finish();
  await expect(pending).resolves.toBe(false);
  expect(native.followup).not.toHaveBeenCalled();expect(native.send).not.toHaveBeenCalled();
  const beforeDecision=admission("p2",async()=>{useSessionStore.setState(state=>({transcripts:{...state.transcripts,sA:{...state.transcripts.sA,planApprovals:[{requestId:"plan",toolCallId:"p",sessionId:"sA"}]}}}));});
  await expect(beforeDecision).resolves.toBe(false);
  expect(native.send).not.toHaveBeenCalled();
});

it("rechecks the native phase instead of dispatching through a stale delivered view", async()=>{
  native.state.mockResolvedValue({task:{id:"A",sessionId:"sA",phase:"verifying"}});
  await expect(admission("p")).resolves.toBe("deferred");
  expect(native.followup).not.toHaveBeenCalled();expect(native.send).not.toHaveBeenCalled();
});

it("serializes a direct composer send and a queued admission for the same task", async()=>{
  const release=claimCodingAdmission("/repo","A")!;
  await expect(admission("queued")).resolves.toBe("deferred");
  expect(native.followup).not.toHaveBeenCalled();expect(native.send).not.toHaveBeenCalled();
  release();
  await expect(admission("after-release")).resolves.toBe(true);
  expect(native.send).toHaveBeenCalledOnce();
});

it.each([true, false])("settles its original reopened task when dispatch becomes unavailable (manual=%s)", async manual => {
  let current = true;
  let finish!: (task: object) => void;
  native.followup.mockImplementation(() => new Promise(resolve => { finish = resolve; }));
  const refresh = vi.fn(async () => undefined);
  const pending = sendCodingQueuedFollowup({root:"/repo",taskId:"A",sessionId:"sA",promptId:"navigation-race",
    text:"补充要求",contextPaths:[],manual,prepare:async()=>{},canDispatch:()=>current,onChanged:refresh});
  await waitFor(() => expect(native.followup).toHaveBeenCalledOnce());
  current = false;
  useSessionStore.getState().setSession("sB");
  finish({id:"A",sessionId:"sA",phase:"discovering",updatedAt:"owned-revision"});
  await expect(pending).resolves.toBe(false);
  expect(native.interrupt).toHaveBeenCalledExactlyOnceWith("/repo","A","paused","owned-revision");
  expect(native.send).not.toHaveBeenCalled();
  expect(useSessionStore.getState().sessionId).toBe("sB");
  expect(refresh).toHaveBeenCalledOnce();
});

it("preserves a newer native stop when the old admission tries to settle", async () => {
  let current = true;
  native.followup.mockImplementation(async () => {
    current = false;
    native.state.mockResolvedValue({task:{id:"A",sessionId:"sA",phase:"stopped",updatedAt:"newer-stop"}});
    return {id:"A",sessionId:"sA",phase:"discovering",updatedAt:"owned-revision"};
  });
  native.interrupt.mockRejectedValue(new Error("任务状态已变化"));
  await expect(admission("stopped",async()=>{},()=>current)).resolves.toBe(false);
  expect(native.interrupt).toHaveBeenCalledExactlyOnceWith("/repo","A","paused","owned-revision");
  expect(native.send).not.toHaveBeenCalled();
});

it("reports a failed settlement instead of claiming the original task was paused", async () => {
  let current = true;
  native.followup.mockImplementation(async () => {
    current = false;
    native.state.mockResolvedValue({task:{id:"A",sessionId:"sA",phase:"discovering",updatedAt:"owned-revision"}});
    return {id:"A",sessionId:"sA",phase:"discovering",updatedAt:"owned-revision"};
  });
  native.interrupt.mockRejectedValue(new Error("暂停状态未能保存"));
  await expect(admission("failed-settlement",async()=>{},()=>current)).rejects.toThrow("暂停状态未能保存");
  expect(native.send).not.toHaveBeenCalled();
  const release = claimCodingAdmission("/repo","A");
  expect(release).not.toBeNull();
  release?.();
});

it("keeps the explicit read-only intent and snapshot when dispatching a queued turn",async()=>{
  await sendCodingQueuedFollowup({root:"/repo",taskId:"A",sessionId:"sA",promptId:"readonly",text:"解释选区",contextPaths:[],manual:false,canDispatch:()=>true,
    coding:{mode:"ask",editorContext:'{"dirty":true,"version":9}',imagePaths:["/data/screenshot.png"]}});
  expect(native.send).toHaveBeenCalledWith("sA",expect.stringContaining('"version":9'),["/data/screenshot.png"],"解释选区","readonly",
    {mode:"ask",documentToolBridge:true,workspace:"/repo",taskId:"A"});
});
