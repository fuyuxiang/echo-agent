// Production chat, composer and process with an in-memory host. No OS input.
import "./ui-surfaces";
import { useSessionStore } from "../../src/stores/session-store";
import type { AutomationStatus } from "../../src/lib/automation-client";
const sid = "review-conversation", promptId = "computer-review-turn";
const callbacks = new Map<number, (event: unknown) => void>();
const listeners = new Map<string, Set<number>>();
const transport = (window as any).__TAURI_INTERNALS__;
const originalInvoke = transport.invoke;
const originalCallback = transport.transformCallback;
const calls: {command:string;args:unknown}[] = [];
let phase = "running", revision = 0, reason: string|null = null;
const status = (): AutomationStatus => ({
  sessionId:sid,mode:"computer_use",paused:["paused","stopped","completed"].includes(phase),revision:++revision,
  allowPrivateNetwork:false,browserRunning:false,browserHasData:false,browser:{available:true},
  computer:{available:true,platform:"macOS",screenCapture:true,inputControl:true},
  task:{phase:phase as any,reason,scope:"在文稿应用整理并保存会议记录",authorized:phase==="running",generation:1,promptId,
    outcome:reason==="message_text_required"?"请告诉我需要发送的消息内容。":reason==="input_conflict"?"目标应用正在被使用，可以直接发送消息继续或调整要求。":phase==="completed"?"已核验指定文件的正文。":null},
  execution:{nativeIsolation:"process",backgroundInput:true,inputMode:"application_targeted_with_foreground_fallback",platform:"macos",fileDialog:true,windowClick:true},
});
transport.transformCallback = (fn: (event:unknown)=>void, once?:boolean) => {
  const id = originalCallback(fn,once); callbacks.set(id,fn); return id;
};
transport.invoke = async (command:string,args:any={}) => {
  calls.push({command,args});
  if (command === "automation_status" && args.sessionId === sid) return status();
  if (command === "plugin:event|listen" && args.event === "automation://status") {
    const set = listeners.get(args.event) ?? new Set(); set.add(args.handler); listeners.set(args.event,set); return args.handler;
  }
  return originalInvoke(command,args);
};
const setPhase = (value:string, blocker: string|null = null) => {
  if (phase === value && reason === blocker) return;
  phase=value; reason=blocker;
  const store=useSessionStore.getState();
  if (phase === "running") store.startStreaming(sid,promptId);
  else if (store.streaming) store.markComplete({sessionId:sid,promptId,stopReason:phase==="stopped"?"cancelled":"end_turn"});
  const payload=status();
  for (const handler of listeners.get("automation://status") ?? []) callbacks.get(handler)?.({payload});
};
const store=useSessionStore.getState();
store.pushUser("请整理文稿应用中的会议记录并保存。",[],sid);
store.startStreaming(sid,promptId);
store.applyUpdate({sessionUpdate:"tool_call",__sessionId:sid,toolCallId:"native-fixture",kind:"edit",title:"填写会议记录",status:"completed",
  rawInput:{tool:"computer_set_value"},content:[{type:"content",content:{type:"text",text:"正文已经回读，准备核验保存结果。"}}],_meta:{promptId}} as any);
useSessionStore.subscribe(state => { if (state.control?.phase === "stopped" && phase !== "stopped") setPhase("stopped"); });
(window as any).computerTaskReview={setPhase,calls};
