import { beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { groupList, groupSnapshot, groupStatus, groupRecipients, resolveGroupForSession, mentionedMembers, type Group, type GroupMember } from "../group-client";
import { useGroupStore } from "../group-store";
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("../group-client", async original => ({ ...await original<typeof import("../group-client")>(), groupList: vi.fn(), groupSnapshot: vi.fn() }));
const group = (id: string): Group => ({ id, name: id, goal: "交付", cwd: "/work", sourceSessionId: null, sourceTeamId: null, coordinatorId: "c", members: [], messages: [], works: [], decisions: [], archived: false, pinned: false, paused: false, revision: 1, updatedAt: 1 });
describe("专家团状态与路由", () => {
  it("未确认保存的消息重试沿用请求 ID，确认后可以重新提问", () => {
    const first = useGroupStore.getState().requestId("a", "same message");
    expect(useGroupStore.getState().requestId("a", "same message")).toBe(first);
    useGroupStore.getState().acknowledgeSend("a");
    expect(useGroupStore.getState().requestId("a", "same message")).not.toBe(first);
  });
  it("无效的目录响应不会破坏原来的专家团和普通任务导航", async () => {
    useGroupStore.setState({ groups: [group("previous")] });
    vi.mocked(groupList).mockResolvedValue(null as never);
    await useGroupStore.getState().refresh();
    expect(useGroupStore.getState().groups[0].id).toBe("previous");
    expect(useGroupStore.getState().error).toContain("有效数据");
  });
  beforeEach(() => { vi.resetAllMocks(); useGroupStore.setState({ groups: [], current: null, currentId: null, error: null, createFrom: null, createTemplate: null, drafts: {}, outbox: {} }); });
  it("流式快照沿用未变化的历史消息，避免重复渲染正文", async () => {
    const first = {...group("a"), messages: [{id:"m",seq:1,text:"已完成的历史消息",attachments:[],toolCalls:[],memberId:null,kind:"user",createdAt:0,workId:null,replyTo:null}] as Group["messages"]};
    vi.mocked(groupSnapshot).mockResolvedValueOnce(first).mockResolvedValueOnce(structuredClone(first));
    await useGroupStore.getState().select("a");
    const message = useGroupStore.getState().current!.messages[0];
    await useGroupStore.getState().select("a");
    expect(useGroupStore.getState().current!.messages[0]).toBe(message);
  });
  it("保存草稿时同时保留收件人、只读模式和引用，不覆盖正文", () => {
    useGroupStore.getState().setDraft("a", "未发送的正文");
    const options = {targets:["expert"],discussion:true,replyTo:"source-message",newTopic:false,requestId:"original-task"};
    useGroupStore.getState().setDraftOptions("a", options);
    expect(useGroupStore.getState().draftOptions("a")).toEqual(options);
    expect(useGroupStore.getState().drafts.a).toBe("未发送的正文");
  });
  it("快速切换专家团时忽略晚到的旧快照", async () => {
    const pending: ((g: Group) => void)[] = [];
    vi.mocked(groupSnapshot).mockImplementation(() => new Promise(resolve => pending.push(resolve)));
    const a = useGroupStore.getState().select("a"), b = useGroupStore.getState().select("b");
    pending[1](group("b")); await b; pending[0](group("a")); await a;
    expect(useGroupStore.getState().current?.id).toBe("b");
  });
  it("刷新失败保留上次成功的专家团记录", async () => {
    vi.mocked(groupSnapshot).mockResolvedValueOnce(group("a")).mockRejectedValueOnce(new Error("暂时离线"));
    await useGroupStore.getState().select("a"); await useGroupStore.getState().select("a");
    expect(useGroupStore.getState().current?.name).toBe("a"); expect(useGroupStore.getState().error).toContain("暂时离线");
  });
  it("目录刷新不会把用户带离当前专家团", async () => { useGroupStore.setState({ currentId: "a", current: group("a") }); vi.mocked(groupList).mockResolvedValue([group("b")]); await useGroupStore.getState().refresh(); expect(useGroupStore.getState().currentId).toBe("a"); });
  it("同名的不同来源团队不会串群", () => { const existing = { ...group("a"), sourceSessionId: "source-a", sourceTeamId: "交付组" }; useGroupStore.setState({ groups: [existing] }); useGroupStore.getState().fromTeam("source-b", { teamId: "交付组", members: ["专家"], createdAt: 0 }); expect(useGroupStore.getState().createFrom?.sessionId).toBe("source-b"); });
  it("来源编组已有归档专家团时打开原记录，不创建重复专家团", async () => {
    const existing={...group("archived-linked"),sourceSessionId:"source",sourceTeamId:"交付组",archived:true};
    useGroupStore.setState({groups:[existing],createFrom:null});
    vi.mocked(groupSnapshot).mockResolvedValue(existing);
    useGroupStore.getState().fromTeam("source",{teamId:"交付组",members:[],createdAt:0});
    expect(useGroupStore.getState().createFrom).toBeNull();
    expect(useGroupStore.getState().currentId).toBe(existing.id);
  });
  it("状态不能把等待确认的执行误报为完成", () => { const g = group("a"); g.works = [{ id: "w", status: "uncertain" }] as Group["works"]; expect(groupStatus(g)).toBe("需要检查"); });
  it("成员提及支持中文和前缀重名，忽略代码、邮箱和文件", () => {
    const members = [{ id: "a", name: "代码", expert: "代码" }, { id: "b", name: "代码 专家", expert: "代码 专家" }, { id: "c", name: "分析师", expert: "分析师" }] as GroupMember[];
    expect(mentionedMembers("@代码 专家 请检查， @分析师 请评审", members)).toEqual(["b", "c"]);
    expect(mentionedMembers("`@分析师` user@分析师.com @分析师.md", members)).toEqual([]);
    expect(mentionedMembers("@全体 请讨论", members)).toHaveLength(3);
  });
  it("收件人统一采用显式选择、提及、回复作者和协调员的优先级", () => {
    const g = group("a");
    g.members = [{id:"c",name:"协调员",expert:null},{id:"e",name:"代码专家",expert:"代码专家"},{id:"gone",name:"旧成员",removed:true}] as GroupMember[];
    g.messages = [{id:"answer",memberId:"e"}] as Group["messages"];
    expect(groupRecipients(g,"请继续",[],"answer")).toEqual(["e"]);
    expect(groupRecipients(g,"请继续",["c"],"answer")).toEqual(["c"]);
    expect(groupRecipients(g,"@代码专家 请继续",[],null)).toEqual(["e"]);
    expect(groupRecipients(g,"请继续",["gone"],null)).toEqual(["c"]);
  });
  it("群列表已不包含旧会话时，仍向持久化归属查询，不按普通任务处理", async () => {
    vi.mocked(invoke).mockResolvedValue("a");
    expect(await resolveGroupForSession("historical-native",[group("a")])).toBe("a");
    expect(invoke).toHaveBeenCalledWith("group_owner",{sessionId:"historical-native"});
    vi.mocked(invoke).mockRejectedValueOnce(new Error("暂不可读"));
    await expect(resolveGroupForSession("historical-native",[])).rejects.toThrow("暂不可读");
  });
});

it("删除后晚到的目录、快照和输入回调不能重新生成群聊数据", async () => {
  const id="late-deleted-group";
  const value=group(id);
  useGroupStore.setState({groups:[value],currentId:id,current:value,drafts:{[id]:"旧草稿"}});
  let listDone!: (groups: Group[])=>void, snapshotDone!: (group: Group)=>void;
  vi.mocked(groupList).mockImplementationOnce(()=>new Promise(resolve=>{listDone=resolve;}));
  vi.mocked(groupSnapshot).mockImplementationOnce(()=>new Promise(resolve=>{snapshotDone=resolve;}));
  const refreshing=useGroupStore.getState().refresh(), selecting=useGroupStore.getState().select(id);
  useGroupStore.getState().forget(id);
  listDone([value]); snapshotDone(value); await Promise.all([refreshing,selecting]);
  useGroupStore.getState().setDraft(id,"晚到草稿");
  useGroupStore.getState().setDraftOptions(id,{targets:[],discussion:false,replyTo:null});
  expect(useGroupStore.getState().groups.some(g=>g.id===id)).toBe(false);
  expect(useGroupStore.getState().currentId).toBeNull();
  expect(useGroupStore.getState().drafts[id]).toBeUndefined();
  expect(useGroupStore.getState().drafts[`options:${id}`]).toBeUndefined();
  expect(()=>useGroupStore.getState().requestId(id,"late send")).toThrow("已删除");
});

it("补充消息沿用原请求的失败状态", () => {
  const g=group("followup-failure");
  g.messages=[{id:"root",kind:"user"},{id:"supplement",kind:"user",requestId:"root"}] as Group["messages"];
  g.works=[{id:"failed",rootId:"root",status:"failed",continuationOf:null}] as Group["works"];
  expect(groupStatus(g)).toBe("执行失败");
});

it("停止尚未确认时不能用已暂停掩盖正在停止", () => {
  const g=group("stopping");g.paused=true;g.works=[{id:"w",status:"stopping"}] as Group["works"];
  expect(groupStatus(g)).toBe("正在停止");
  g.works[0].status="cancelled";expect(groupStatus(g)).toBe("已暂停");
});

it("轻量列表沿用后端任务归属和交付状态", () => {
  const preview=group("preview");
  preview.latestRequestId="request";
  preview.messages=[{id:"answer",seq:3,kind:"assistant",text:"结论",memberId:"c",workId:"work",replyTo:null,attachments:[],toolCalls:[],createdAt:1}];
  preview.requests=[{id:"request",ownerId:"c",workId:"work",inputRevision:1,state:"delivered",finalMessageId:"answer"}];
  expect(groupStatus(preview)).toBe("已回复");
  for (const [state,label] of [["missing_result","待整理结果"],["partial","有未完成事项"],["stopped","已停止"],["failed","执行失败"]] as const) {
    preview.requests[0].state=state;
    preview.works=[{id:"failed-child",rootId:"request",memberId:"e",parentId:"work",continuationOf:null,prompt:"",status:"failed",discussion:false,sessionId:null,output:"",error:"failed",createdAt:1,tokens:0}];
    expect(groupStatus(preview)).toBe(label);
  }
});

it("升级后重试旧格式待发送消息沿用原请求 ID，明确新话题使用新 ID", () => {
  const original={text:"请完成交付",attachments:[],recipients:["c"],discussion:false,reply:null,sources:[],scopes:[]};
  useGroupStore.setState({outbox:{legacy:JSON.stringify({id:"persisted-request",fingerprint:JSON.stringify(original)})}});
  expect(useGroupStore.getState().requestId("legacy",JSON.stringify({...original,newTopic:false,selectedRequest:null}))).toBe("persisted-request");
  expect(useGroupStore.getState().requestId("legacy",JSON.stringify({...original,newTopic:true,selectedRequest:null}))).not.toBe("persisted-request");
});
