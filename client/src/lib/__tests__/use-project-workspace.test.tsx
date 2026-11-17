import { act, renderHook, waitFor } from "@testing-library/react";
import { beforeEach, expect, it, vi } from "vitest";
import { useProjectWorkspace } from "../use-project-workspace";
import { useOrgSessionStore } from "@/stores/org-session-store";
const request=vi.hoisted(()=>vi.fn());
vi.mock("../team-project-client",async original=>({...await original<object>(),teamProjectRequest:request}));
const snapshot={id:"shared",revision:1,tasks:[],assets:[],members:[]};
beforeEach(()=>{
 request.mockReset().mockResolvedValue(snapshot);
 useOrgSessionStore.setState({boundaryRevision:1,session:{loggedIn:true,accountId:"a",accountGeneration:1,serverUrl:"https://test",user:{id:"a",username:"a",displayName:"A",role:"member",clearance:0}}});
});
it("discards a pending snapshot at an account boundary and rejects old mutation closures",async()=>{
 let finish!:(value:unknown)=>void;request.mockImplementationOnce(()=>new Promise(resolve=>{finish=resolve;}));
 const {result}=renderHook(()=>useProjectWorkspace("shared"));
 await waitFor(()=>expect(request).toHaveBeenCalledOnce());const old=result.current;
 act(()=>useOrgSessionStore.setState({boundaryRevision:2,session:null}));
 await act(async()=>finish({...snapshot,goal:"上一账号的内容"}));expect(result.current.snapshot).toBeNull();
 const previousCalls=request.mock.calls.length;await expect(old.mutate("createTask",{title:"旧请求"})).rejects.toThrow("身份已变化");await old.refresh();expect(request).toHaveBeenCalledTimes(previousCalls);
});
it("does not replace the real mutation result with a later refresh failure",async()=>{
 const {result}=renderHook(()=>useProjectWorkspace("shared"));await waitFor(()=>expect(result.current.snapshot?.id).toBe("shared"));
 request.mockImplementation(async(action:string)=>{if(action==="createTask")return {id:"saved-task"};throw new Error("连接中断");});
 let response:unknown;await act(async()=>{response=await result.current.mutate("createTask",{title:"已保存"});});expect(response).toEqual({id:"saved-task"});await waitFor(()=>expect(result.current.error).toContain("连接中断"));expect(result.current.snapshot).toBeNull();
});
