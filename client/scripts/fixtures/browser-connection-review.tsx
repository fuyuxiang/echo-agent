// Production settings with a synthetic host; no user browser is paired.
import "./ui-surfaces";
import { useSessionStore } from "../../src/stores/session-store";
import { useSessionsStore } from "../../src/stores/sessions-store";
const transport = (window as any).__TAURI_INTERNALS__;
const original = transport.invoke, transform = transport.transformCallback;
const callbacks = new Map<number, (event:unknown)=>void>(), listeners = new Set<number>();
let source = "isolated", connected = false, revision = 0, nextExpires = 300;
const calls: {command:string;args:any}[] = [];
const status = (sessionId = "browser-a") => ({
  sessionId,mode:"computer_use",paused:true,allowPrivateNetwork:false,browserRunning:false,browserHasData:true,revision:++revision,
  extensionConnected:connected,browser:{available:true},computer:{available:true,platform:"macos",screenCapture:true,inputControl:true},
  task:{phase:"paused",authorized:false,generation:1,scope:"查询浏览器中的资料",reason:"user_paused"},
  execution:{nativeIsolation:"process",backgroundInput:true,inputMode:"application_targeted_with_foreground_fallback",platform:"macos",fileDialog:true,windowClick:true,browserSource:source},
});
const emit = () => {const payload=status();for(const id of listeners)callbacks.get(id)?.({payload});};
transport.transformCallback = (fn:(event:unknown)=>void,once?:boolean) => {const id=transform(fn,once);callbacks.set(id,fn);return id;};
transport.invoke = async (command:string,args:any={}) => {
  calls.push({command,args});
  if(command==="automation_status")return status(args.sessionId);
  if(command==="automation_pair_browser"){
    source="extension";connected=false;emit();
    const expiresInSeconds=nextExpires;nextExpires=300;
    return {endpoint:`ws://127.0.0.1:12345/${"a".repeat(64)}`,extensionPath:`/Applications/${"浏览器扩展目录/".repeat(12)}browser-extension`,expiresInSeconds};
  }
  if(command==="automation_disconnect_browser"){connected=false;emit();return;}
  if(command==="automation_use_isolated_browser"){source="isolated";connected=false;emit();return status(args.sessionId);}
  if(command==="plugin:event|listen"&&args.event==="automation://status"){listeners.add(args.handler);return args.handler;}
  return original(command,args);
};
useSessionStore.getState().setSession("browser-a");
useSessionsStore.getState().setIndependent([{sessionId:"browser-a",title:"资料整理",cwd:"/review/browser",status:"completed"}]);
(window as any).browserConnectionReview={
  calls,connect(){connected=true;emit();},disconnect(){connected=false;emit();},expireNext(){nextExpires=0;},
};
