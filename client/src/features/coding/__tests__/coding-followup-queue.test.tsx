import { act, renderHook, waitFor } from "@testing-library/react";
import { beforeEach, expect, it, vi } from "vitest";
import { useCodingFollowupQueue } from "../hooks/useCodingFollowupQueue";
import { useMessageQueueStore } from "@/stores/message-queue-store";
import { useSessionStore, type ChatMessage } from "@/stores/session-store";
const options = () => ({workspace:"/repo",taskId:"A",sessionId:"sA",autoReady:false,manualReady:false,
  send:vi.fn(async (_text: string, _paths: string[], _promptId: string, _manual: boolean)=>true),onToast:vi.fn()});
beforeEach(()=>{localStorage.clear();useMessageQueueStore.setState({queues:{}});useSessionStore.setState({transcripts:{}});});
function finish(sessionId: string, promptId: string, stopReason = "end_turn") {
  const state = useSessionStore.getState();
  state.setSession(sessionId);
  state.setMessages([{id:promptId,role:"assistant",parts:[],complete:true,promptId,stopReason} as ChatMessage]);
}

it("retains admitted context until the correlated native completion, then advances only after delivery", async () => {
  const props=options();
  const hook=renderHook(value=>useCodingFollowupQueue(value),{initialProps:props});
  act(()=>{hook.result.current.enqueue("核验边界",["/repo/a.ts"]);hook.result.current.enqueue("第二条",[]);});
  hook.rerender({...props,manualReady:true});
  expect(props.send).not.toHaveBeenCalled();
  hook.rerender({...props,autoReady:true,manualReady:true});
  await waitFor(()=>expect(props.send).toHaveBeenCalledExactlyOnceWith("核验边界",["/repo/a.ts"],expect.any(String),false,undefined));
  expect(useMessageQueueStore.getState().getQueue(hook.result.current.queueId!)[0].status).toBe("sending");
  hook.rerender({...props,autoReady:false,manualReady:false});
  act(()=>finish("sA",props.send.mock.calls[0][2]));
  await waitFor(()=>expect(useMessageQueueStore.getState().getQueue(hook.result.current.queueId!)).toHaveLength(1));
  expect(props.send).toHaveBeenCalledOnce();
  hook.rerender({...props,autoReady:true,manualReady:true});
  await waitFor(()=>expect(props.send).toHaveBeenCalledTimes(2));
});

it("pauses a local rejection and does not silently advance later messages", async () => {
  const props={...options(),send:vi.fn(async()=>false)};
  const hook=renderHook(value=>useCodingFollowupQueue(value),{initialProps:props});
  act(()=>{hook.result.current.enqueue("补充测试",[]);hook.result.current.enqueue("不要提前发送",[]);});
  hook.rerender({...props,autoReady:true,manualReady:true});
  await waitFor(()=>expect(useMessageQueueStore.getState().getQueue(hook.result.current.queueId!)[0]).toMatchObject({status:"paused",sendError:expect.any(String)}));
  expect(props.send).toHaveBeenCalledOnce();
});

it("a native asynchronous failure keeps the exact row paused for review", async () => {
  const props={...options(),autoReady:true,manualReady:true};
  const hook=renderHook(()=>useCodingFollowupQueue(props));
  act(()=>{hook.result.current.enqueue("不能丢失的要求",["src/a.ts"]);});
  await waitFor(()=>expect(props.send).toHaveBeenCalledOnce());
  act(()=>finish("sA",props.send.mock.calls[0][2],"error"));
  await waitFor(()=>expect(useMessageQueueStore.getState().getQueue(hook.result.current.queueId!)[0]).toMatchObject({status:"paused",recovery:"uncertain",attachments:["src/a.ts"]}));
  expect(props.send).toHaveBeenCalledOnce();
});

it("late acknowledgements settle only their owner after switching tasks", async () => {
  let accept!: (value:boolean)=>void;
  const props={...options(),autoReady:true,manualReady:true,send:vi.fn((_text: string,_paths:string[],_promptId:string,_manual:boolean)=>new Promise<boolean>(resolve=>{accept=resolve;}))};
  const hook=renderHook(value=>useCodingFollowupQueue(value),{initialProps:props});
  const keyA=hook.result.current.queueId!;
  act(()=>{hook.result.current.enqueue("A 的要求",[]);});
  await waitFor(()=>expect(props.send).toHaveBeenCalledOnce());
  hook.rerender({...props,taskId:"B",sessionId:"sB",autoReady:false,manualReady:false});
  act(()=>{hook.result.current.enqueue("B 的要求",[]);});
  const keyB=hook.result.current.queueId!;
  await act(async()=>accept(true));
  expect(useMessageQueueStore.getState().getQueue(keyA)[0].status).toBe("sending");
  expect(useMessageQueueStore.getState().getQueue(keyB)[0].text).toBe("B 的要求");
  hook.rerender(props);
  act(()=>finish("sA",props.send.mock.calls[0][2]));
  await waitFor(()=>expect(useMessageQueueStore.getState().getQueue(keyA)).toEqual([]));
  expect(useMessageQueueStore.getState().getQueue(keyB)).toHaveLength(1);
});

it("coexisting or remounted workbenches cannot claim a second row while one is sending", async () => {
  const props={...options(),autoReady:true,manualReady:true};
  const first=renderHook(()=>useCodingFollowupQueue(props));
  act(()=>{first.result.current.enqueue("first",[]);first.result.current.enqueue("second",[]);});
  await waitFor(()=>expect(props.send).toHaveBeenCalledOnce());
  const second=renderHook(()=>useCodingFollowupQueue(props));
  first.unmount();second.unmount();
  const remounted=renderHook(()=>useCodingFollowupQueue(props));
  expect(props.send).toHaveBeenCalledOnce();
  remounted.unmount();
});
