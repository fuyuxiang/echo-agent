import { invoke } from "@tauri-apps/api/core";
import { ThemeProvider } from "@/components/ThemeProvider";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render as renderWithoutTheme, screen, waitFor, within } from "@testing-library/react";
import { GroupWorkspace } from "../GroupWorkspace";
import { useGroupStore } from "../group-store";
import { groupSend, groupMemberChange, groupControl, type Group } from "../group-client";
import { useKnowledgeStore } from "@/stores/knowledge-store";
import { saveDraftAttachments } from "@/lib/draft-attachments";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => []) }));
vi.mock("@/lib/agent-client", async original => ({ ...await original<typeof import("@/lib/agent-client")>(), agentsList: vi.fn(async () => []) }));
vi.mock("../group-client", async original => ({ ...await original<typeof import("../group-client")>(), groupSend: vi.fn(async () => {}), groupMemberChange: vi.fn(async () => {}), groupControl: vi.fn(async () => {}) }));
vi.mock("@/components/MessageItem", () => ({ MessageItem: () => <div>成员回答</div> }));

let measuredWidth: number | null = null;
vi.mock("@/lib/use-element-size", () => ({ useElementSize: (_ref: unknown, dimension: string) => dimension === "width" ? measuredWidth : null }));

const render = (ui: Parameters<typeof renderWithoutTheme>[0]) => renderWithoutTheme(ui, { wrapper: ThemeProvider });

const group: Group = { id:"reply-group",name:"需求协作组",goal:"验证",cwd:"/work",sourceSessionId:null,sourceTeamId:null,
  coordinatorId:"c",members:[
    {id:"c",name:"协调员",expert:null,role:"协调",modelId:"test",sessionId:"native-c",discussionSessionId:null,generation:0},
    {id:"e",name:"代码专家",expert:"代码专家",role:"编码",modelId:"test",sessionId:"native-e",discussionSessionId:null,generation:0},
  ], messages:[{id:"answer",seq:1,kind:"assistant",text:"请回复我",memberId:"e",createdAt:0,workId:null,replyTo:null,attachments:[],toolCalls:[]}],
  works:[],decisions:[],archived:false,pinned:false,paused:false,revision:1,updatedAt:1 };

beforeEach(() => {
  localStorage.clear(); vi.clearAllMocks(); vi.mocked(invoke).mockResolvedValue([]); measuredWidth = null;
  useKnowledgeStore.setState({defaultSources:[],sessionSources:{},defaultOrganizationScopeIds:[],sessionOrganizationScopeIds:{}});
  useGroupStore.setState({current:structuredClone(group),currentId:group.id,groups:[structuredClone(group)],drafts:{},outbox:{},error:null});
});
afterEach(cleanup);

it("停止请求未完成时返回列表，迟到结果不重新打开原专家团", async () => {
  let finish!: () => void;
  vi.mocked(groupControl).mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }));
  useGroupStore.setState({ current: { ...structuredClone(group), paused: true } });
  render(<GroupWorkspace cwd="/work" models={[{id:"test",label:"测试"}]} workspaces={[]} onToast={() => {}} onOpenSettings={() => {}} />);
  fireEvent.click(screen.getByRole("button", { name: "恢复派发" }));
  await waitFor(() => expect(groupControl).toHaveBeenCalled());
  fireEvent.click(screen.getByRole("button", { name: "返回专家团列表" }));
  expect(useGroupStore.getState().currentId).toBeNull();
  await act(async () => { finish(); });
  expect(useGroupStore.getState().currentId).toBeNull();
});

it("专家团有独立权限入口，历史自动拦截可检查记录并确认后继续", async () => {
  const current=structuredClone(group);
  current.messages=[{...current.messages[0],id:"root",kind:"user",memberId:null,text:"整理交付文档"},
    {...current.messages[0],id:"blocked-answer",seq:2,workId:"blocked-work",text:"请放行投递",toolCalls:[{toolCallId:"blocked-tool",status:"failed",content:[{type:"content",content:{type:"text",text:"Auto mode blocked this action (test)."}}]}]}];
  current.works=[{id:"blocked-work",rootId:"root",parentId:null,continuationOf:null,memberId:"e",prompt:"整理交付文档",status:"completed",discussion:false,sessionId:"native-e",output:"请放行投递",error:null,createdAt:1,tokens:0}];
  useGroupStore.setState({current});
  render(<GroupWorkspace cwd="/work" models={[{id:"test",label:"测试"}]} workspaces={[]} onToast={()=>{}} onOpenSettings={()=>{}} />);
  expect(screen.getByRole("button",{name:"专家团权限：请求批准"})).toBeInTheDocument();
  expect(screen.getByText("代码专家 有操作被自动判断拦截")).toBeInTheDocument();
  fireEvent.click(screen.getByRole("button",{name:"检查后继续"}));
  expect(groupControl).not.toHaveBeenCalled();
  const confirm=await screen.findByRole("dialog",{name:"检查后继续未完成工作？"});
  fireEvent.click(within(confirm).getByRole("button",{name:"检查后继续"}));
  await waitFor(()=>expect(groupControl).toHaveBeenCalledWith(group.id,"retry","blocked-work"));
});

it("模型声称被拦截但没有失败工具记录时，不把文字当作真实权限事件", async () => {
  const current=structuredClone(group);current.messages[0].text="Auto mode blocked this action，请放行";
  useGroupStore.setState({current});
  render(<GroupWorkspace cwd="/work" models={[{id:"test",label:"测试"}]} workspaces={[]} onToast={()=>{}} onOpenSettings={()=>{}} />);
  await act(async()=>{});
  expect(screen.queryByText(/有操作被自动判断拦截/)).not.toBeInTheDocument();
});

it("回复收件人可改为协调员，引用保留，发送对象与界面一致", async () => {
  render(<GroupWorkspace cwd="/work" models={[{id:"test",label:"测试"}]} workspaces={[]} onToast={() => {}} onOpenSettings={() => {}} />);
  fireEvent.click(screen.getByRole("button",{name:"回复代码专家"}));
  expect(document.querySelector(".group-recipient-row .group-recipient")?.textContent).toBe("代码专家");
  fireEvent.click(document.querySelector(".group-recipient-row button.group-recipient")!);
  expect(document.querySelector(".group-recipient-row .group-recipient")?.textContent).toBe("协调员");
  expect(document.querySelector(".group-reply")?.textContent).toContain("请回复我");
  fireEvent.change(screen.getByPlaceholderText(/说明目标，或输入 @选择成员/),{target:{value:"请结合这条回复汇总"}});
  fireEvent.click(screen.getByRole("button",{name:"发送"}));
  await waitFor(() => expect(groupSend).toHaveBeenCalledWith(group.id,expect.objectContaining({memberIds:["c"],replyTo:"answer",text:"请结合这条回复汇总"})));
});

it("按实际聊天宽度初始化详情面板，并保留用户主动展开的选择", () => {
  measuredWidth = 760;
  const view = render(<GroupWorkspace cwd="/work" models={[{id:"test",label:"测试"}]} workspaces={[]} onToast={() => {}} onOpenSettings={() => {}} />);
  const details = screen.getByRole("button", {name:"专家团详情"});
  expect(details).toHaveAttribute("aria-expanded", "false");
  fireEvent.click(details);
  expect(details).toHaveAttribute("aria-expanded", "true");
  measuredWidth = 600;
  view.rerender(<GroupWorkspace cwd="/work" models={[{id:"test",label:"测试"}]} workspaces={[]} onToast={() => {}} onOpenSettings={() => {}} />);
  expect(details).toHaveAttribute("aria-expanded", "true");
});

it("从成果查看完整结论时收起窄屏详情，让聊天内容可见", () => {
  measuredWidth = 600;
  const current = structuredClone(group);
  current.messages = [
    {...current.messages[0], id:"original", kind:"user", memberId:null, text:"完成原型"},
    {...current.messages[0], id:"delivered", seq:2, workId:"finished", text:"原型已交付"},
  ];
  current.works = [{id:"finished", rootId:"original", memberId:"e", parentId:null, continuationOf:null, prompt:"完成原型", status:"completed", discussion:false, sessionId:null, output:"原型已交付", error:null, createdAt:1, tokens:0}];
  useGroupStore.setState({current});
  render(<GroupWorkspace cwd="/work" models={[{id:"test",label:"测试"}]} workspaces={[]} onToast={() => {}} onOpenSettings={() => {}} />);
  fireEvent.click(screen.getByRole("button", {name:"查看成果"}));
  const details = screen.getByRole("dialog", {name:"专家团详情"});
  fireEvent.click(within(details).getByRole("button", {name:"在聊天中查看完整结论"}));
  expect(screen.queryByRole("dialog", {name:"专家团详情"})).not.toBeInTheDocument();
  expect(document.getElementById("group-message-delivered")).toBeInTheDocument();
});

it("补充后启动失败仍展示原因、重试入口和原请求", () => {
  const current=structuredClone(group);
  current.messages=[{...current.messages[0],id:"root",kind:"user",memberId:null,text:"原始交付目标"},
    {...current.messages[0],id:"supplement",seq:2,kind:"user",memberId:null,text:"补充约束",requestId:"root"}];
  current.works=[{id:"failure",rootId:"root",parentId:null,continuationOf:null,memberId:"e",prompt:"交付",status:"failed",discussion:false,sessionId:null,output:"",error:"模型连接失败",createdAt:1,tokens:0}];
  useGroupStore.setState({current});
  render(<GroupWorkspace cwd="/work" models={[{id:"test",label:"测试"}]} workspaces={[]} onToast={() => {}} onOpenSettings={() => {}} />);
  expect(screen.getByText("模型连接失败")).toBeInTheDocument();
  expect(screen.getAllByRole("button",{name:"检查后继续"}).length).toBeGreaterThan(0);
  expect(screen.getByText("补充原请求：原始交付目标")).toBeInTheDocument();
  expect(document.querySelector(".group-chat__header .group-status")?.textContent).toBe("执行失败");
});

it("清除成员记忆失败保留确认框，允许原地重试", async () => {
  vi.mocked(groupMemberChange).mockRejectedValueOnce(new Error("记忆目录暂时不可写"));
  render(<GroupWorkspace cwd="/work" models={[{id:"test",label:"测试"}]} workspaces={[]} onToast={() => {}} onOpenSettings={() => {}} />);
  const details=screen.getByRole("button",{name:"专家团详情"});
  if(details.getAttribute("aria-expanded") === "false") fireEvent.click(details);
  fireEvent.click(screen.getByText("管理成员"));
  fireEvent.click(screen.getAllByRole("button",{name:"清除成员记忆"})[0]);
  const dialog=screen.getByRole("alertdialog");
  fireEvent.click(within(dialog).getByRole("button",{name:"清除成员记忆"}));
  await waitFor(() => expect(within(dialog).getByRole("alert")).toHaveTextContent("记忆目录暂时不可写"));
  expect(dialog).toBeInTheDocument();
  fireEvent.click(within(dialog).getByRole("button",{name:"清除成员记忆"}));
  await waitFor(() => expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument());
  expect(groupMemberChange).toHaveBeenCalledTimes(2);
});

it("同名成员必须明确选择，反复发送不会静默选择第一个成员", async () => {
  const current=structuredClone(group);
  current.members.forEach(member => { member.name="同名专家"; });
  useGroupStore.setState({current,drafts:{},outbox:{}});
  render(<GroupWorkspace cwd="/work" models={[{id:"test",label:"测试"}]} workspaces={[]} onToast={() => {}} onOpenSettings={() => {}} />);
  fireEvent.change(screen.getByPlaceholderText(/说明目标，或输入 @选择成员/),{target:{value:"@同名专家 请核对"}});
  fireEvent.click(screen.getByRole("button",{name:"发送"}));
  await screen.findByText("成员名称重复，请通过 @菜单选择具体成员后发送");
  fireEvent.click(screen.getByRole("button",{name:"发送"}));
  expect(groupSend).not.toHaveBeenCalled();
});

it("选择任务 A 后回复任务 B，同步切换归属并保留指定收件人", async () => {
  const current = structuredClone(group);
  const work = (id: string, rootId: string, memberId: string) => ({id, rootId, memberId, parentId:null, continuationOf:null, prompt:rootId, status:"running", discussion:false, sessionId:null, output:"", error:null, createdAt:1, tokens:0});
  current.works = [work("work-a", "request-a", "c"), work("work-b", "request-b", "e")];
  current.messages = [
    {...current.messages[0], id:"request-a", kind:"user", memberId:null, text:"任务 A", workId:null},
    {...current.messages[0], id:"request-b", seq:2, kind:"user", memberId:null, text:"任务 B", workId:null},
    {...current.messages[0], id:"answer-b", seq:3, text:"任务 B 的进度", workId:"work-b"},
  ];
  useGroupStore.setState({current, drafts:{}, outbox:{}});
  render(<GroupWorkspace cwd="/work" models={[{id:"test",label:"测试"}]} workspaces={[]} onToast={() => {}} onOpenSettings={() => {}} />);
  fireEvent.change(screen.getByRole("combobox", {name:"消息归属"}), {target:{value:"request-a"}});
  fireEvent.click(screen.getByRole("button", {name:"回复代码专家"}));
  expect(screen.getByRole("combobox", {name:"消息归属"})).toHaveValue("request-b");
  fireEvent.change(screen.getByRole("combobox", {name:"消息归属"}), {target:{value:"request-a"}});
  expect(document.querySelector(".group-reply")).toBeNull();
  expect(screen.getByRole("combobox", {name:"消息归属"})).toHaveValue("request-a");
  fireEvent.change(screen.getByRole("combobox", {name:"消息归属"}), {target:{value:"new"}});
  fireEvent.click(screen.getByRole("button", {name:"回复代码专家"}));
  expect(screen.getByRole("combobox", {name:"消息归属"})).toHaveValue("request-b");
  fireEvent.change(screen.getByPlaceholderText(/说明目标，或输入 @选择成员/), {target:{value:"请修正任务 B"}});
  fireEvent.click(screen.getByRole("button", {name:"发送"}));
  await waitFor(() => expect(groupSend).toHaveBeenCalledWith(group.id, expect.objectContaining({requestId:"request-b", newRequest:false, memberIds:["e"], replyTo:"answer-b"})));
});

it("同名成员仍可通过 @全体 明确发送给全部专家", async () => {
  const current=structuredClone(group);
  current.members.forEach(member => { member.name="同名专家"; });
  useGroupStore.setState({current,drafts:{},outbox:{}});
  render(<GroupWorkspace cwd="/work" models={[{id:"test",label:"测试"}]} workspaces={[]} onToast={() => {}} onOpenSettings={() => {}} />);
  fireEvent.change(screen.getByPlaceholderText(/说明目标，或输入 @选择成员/),{target:{value:"@全体 请共同核对"}});
  fireEvent.click(screen.getByRole("button",{name:"发送"}));
  await waitFor(() => expect(groupSend).toHaveBeenCalledWith(group.id,expect.objectContaining({memberIds:["e"]})));
});

it("已结束任务的追问保留关联，另起任务明确脱离旧任务", async () => {
  const current = structuredClone(group);
  current.messages = [
    {...current.messages[0],id:"original",kind:"user",memberId:null,text:"完成教务原型",requestOwnerId:"e"},
    {...current.messages[0],id:"delivered",seq:2,workId:"finished",text:"原型已交付"},
  ];
  current.works = [{id:"finished",rootId:"original",memberId:"e",parentId:null,continuationOf:null,prompt:"完成原型",status:"completed",discussion:false,sessionId:null,output:"原型已交付",error:null,createdAt:1,tokens:0}];
  useGroupStore.setState({current,drafts:{},outbox:{}});
  render(<GroupWorkspace cwd="/work" models={[{id:"test",label:"测试"}]} workspaces={[]} onToast={()=>{}} onOpenSettings={()=>{}} />);
  expect(screen.getByText("继续：完成教务原型")).toBeInTheDocument();
  fireEvent.change(screen.getByPlaceholderText(/说明目标/),{target:{value:"增加教师角色"}});
  fireEvent.click(screen.getByRole("button",{name:"发送"}));
  await waitFor(()=>expect(groupSend).toHaveBeenCalledWith(group.id,expect.objectContaining({sourceRequestId:"original",requestId:null,newRequest:true,text:"增加教师角色"})));
  fireEvent.click(screen.getByRole("button",{name:"另起任务"}));
  fireEvent.change(screen.getByPlaceholderText(/说明目标/),{target:{value:"另做一个报表"}});
  fireEvent.click(screen.getByRole("button",{name:"发送"}));
  await waitFor(()=>expect(groupSend).toHaveBeenLastCalledWith(group.id,expect.objectContaining({sourceRequestId:null,requestId:null,newRequest:true,text:"另做一个报表"})));
});

it("明确先回答的消息使用只读新一轮，原执行任务保持运行", async () => {
  const current = structuredClone(group);
  current.messages = [{...current.messages[0],id:"original",kind:"user",memberId:null,text:"交付原型"}];
  current.works = [{id:"working",rootId:"original",memberId:"c",parentId:null,continuationOf:null,prompt:"交付原型",status:"running",discussion:false,sessionId:null,output:"",error:null,createdAt:1,tokens:0}];
  useGroupStore.setState({current,drafts:{},outbox:{}});
  render(<GroupWorkspace cwd="/work" models={[{id:"test",label:"测试"}]} workspaces={[]} onToast={()=>{}} onOpenSettings={()=>{}} />);
  fireEvent.change(screen.getByPlaceholderText(/说明目标/),{target:{value:"先回答，不要修改代码，这个方案有什么风险？"}});
  expect(screen.getByText("已识别只读要求")).toBeInTheDocument();
  fireEvent.click(screen.getByRole("button",{name:"发送"}));
  await waitFor(()=>expect(groupSend).toHaveBeenCalledWith(group.id,expect.objectContaining({discussion:true,newRequest:true,sourceRequestId:"original"})));
  expect(current.works[0].status).toBe("running");
  expect(groupControl).not.toHaveBeenCalled();
});

it("补充回执等待原生记录，不以模型声称已收到作为处理证据", () => {
  const current = structuredClone(group);
  current.messages = [
    {...current.messages[0],id:"original",seq:1,kind:"user",memberId:null,text:"交付原型"},
    {...current.messages[0],id:"followup",seq:2,kind:"user",memberId:null,text:"改用 PostgreSQL",requestId:"original",memberIds:["c"]},
    {...current.messages[0],id:"claim",seq:3,text:"已收到并采用",workId:"working"},
  ];
  current.works = [{id:"working",rootId:"original",memberId:"c",parentId:null,continuationOf:null,prompt:"交付",status:"running",discussion:false,sessionId:null,output:"已收到并采用",error:null,createdAt:1,tokens:0,contextSeq:1}];
  useGroupStore.setState({current,drafts:{},outbox:{}});
  render(<GroupWorkspace cwd="/work" models={[{id:"test",label:"测试"}]} workspaces={[]} onToast={()=>{}} onOpenSettings={()=>{}} />);
  expect(screen.getByText("补充已保存，等待处理")).toBeInTheDocument();
  act(()=>useGroupStore.setState({current:{...current,revision:2,works:[{...current.works[0],consumedFollowups:["followup"]}]}}));
  expect(screen.getByText("已纳入处理")).toBeInTheDocument();
});

it("切换当前任务后可以恢复历史失败，恢复提示只在所属任务显示一次", () => {
  const current = structuredClone(group);
  current.messages = [
    {...current.messages[0],id:"failed-root",kind:"user",memberId:null,text:"失败任务"},
    {...current.messages[0],id:"new-root",seq:2,kind:"user",memberId:null,text:"进行中任务"},
  ];
  const base = {parentId:null,continuationOf:null,prompt:"交付",discussion:false,sessionId:null,output:"",createdAt:1,tokens:0};
  current.works = [{...base,id:"failed",rootId:"failed-root",memberId:"e",status:"failed",error:"历史连接失败"},{...base,id:"running",rootId:"new-root",memberId:"c",status:"running",error:null}];
  useGroupStore.setState({current,drafts:{},outbox:{}});
  render(<GroupWorkspace cwd="/work" models={[{id:"test",label:"测试"}]} workspaces={[]} onToast={()=>{}} onOpenSettings={()=>{}} />);
  expect(screen.queryByText("历史连接失败")).toBeNull();
  fireEvent.change(screen.getByRole("combobox",{name:"当前任务"}),{target:{value:"failed-root"}});
  expect(screen.getAllByText("历史连接失败")).toHaveLength(1);
  fireEvent.click(screen.getByRole("button",{name:"检查后继续"}));
  expect(groupControl).toHaveBeenCalledWith(group.id,"retry","failed");
});

it("检查后继续会聚焦原生创建的新一轮，保留失败任务的记录", async () => {
  const current = structuredClone(group);
  current.messages = [{...current.messages[0],id:"original",kind:"user",memberId:null,text:"交付原型"}];
  current.works = [{id:"failed",rootId:"original",memberId:"e",parentId:null,continuationOf:null,prompt:"交付原型",status:"failed",discussion:false,sessionId:null,output:"",error:"连接失败",createdAt:1,tokens:0}];
  const next = structuredClone(current);
  next.messages.push({...current.messages[0],id:"retry-round",seq:2,text:"检查后继续：交付原型",sourceRequestId:"original",requestOwnerId:"e"});
  next.works.push({...current.works[0],id:"retry-work",rootId:"retry-round",status:"running",error:null});
  vi.mocked(invoke).mockImplementation(async command => command === "group_snapshot" ? next : []);
  useGroupStore.setState({current,drafts:{},outbox:{}});
  render(<GroupWorkspace cwd="/work" models={[{id:"test",label:"测试"}]} workspaces={[]} onToast={()=>{}} onOpenSettings={()=>{}} />);
  fireEvent.click(screen.getByRole("button",{name:"检查后继续"}));
  await waitFor(()=>expect(screen.getByRole("combobox",{name:"当前任务"})).toHaveValue("retry-round"));
  expect(useGroupStore.getState().current?.works.find(work=>work.id==="failed")?.status).toBe("failed");
});

it("窄屏详情可用 Escape 关闭并恢复打开按钮的焦点", () => {
  measuredWidth = 600;
  render(<GroupWorkspace cwd="/work" models={[{id:"test",label:"测试"}]} workspaces={[]} onToast={()=>{}} onOpenSettings={()=>{}} />);
  const trigger = screen.getByRole("button",{name:"专家团详情"});
  trigger.focus(); fireEvent.click(trigger);
  expect(screen.getByRole("dialog",{name:"专家团详情"})).toBeInTheDocument();
  fireEvent.keyDown(document,{key:"Escape"});
  expect(screen.queryByRole("dialog",{name:"专家团详情"})).toBeNull();
  expect(trigger).toHaveFocus();
});

it("重启后暂停的未确认执行仍可直接核对，不必先恢复其他派发", async () => {
  const current = structuredClone(group);
  current.paused = true;
  current.messages = [{...current.messages[0],id:"original",kind:"user",memberId:null,text:"交付原型"}];
  current.works = [{id:"unknown",rootId:"original",memberId:"e",parentId:null,continuationOf:null,prompt:"交付原型",status:"uncertain",discussion:false,sessionId:null,output:"",error:null,createdAt:1,tokens:0}];
  useGroupStore.setState({current,drafts:{},outbox:{}});
  render(<GroupWorkspace cwd="/work" models={[{id:"test",label:"测试"}]} workspaces={[]} onToast={()=>{}} onOpenSettings={()=>{}} />);
  expect(screen.getByRole("button",{name:"检查后继续"})).toBeEnabled();
  fireEvent.click(screen.getByRole("button",{name:"检查后继续"}));
  const confirm = await screen.findByRole("dialog",{name:"检查后继续工作"});
  fireEvent.click(within(confirm).getByRole("button",{name:"检查后继续"}));
  await waitFor(()=>expect(groupControl).toHaveBeenCalledWith(group.id,"retry","unknown"));
});

it("新旧附件超限时保留输入，调整历史附件后可发送，选择在离开后恢复", async () => {
  const current = structuredClone(group);
  const paths = Array.from({ length: 20 }, (_, index) => `/work/资料-${index}.txt`);
  current.messages = [{ ...current.messages[0], id: "original", seq: 1, kind: "user", memberId: null, text: "检查这些资料", attachments: paths }];
  current.works = [{ id: "finished", rootId: "original", memberId: "c", parentId: null, continuationOf: null, prompt: "检查", status: "completed", discussion: false, sessionId: null, output: "检查完成", error: null, createdAt: 1, tokens: 0 }];
  useGroupStore.setState({ current, drafts: { [current.id]: "结合新资料继续" }, outbox: {} });
  saveDraftAttachments(`group:${current.id}`, ["/work/新增资料.txt"]);
  let view = render(<GroupWorkspace cwd="/work" models={[{ id: "test", label: "测试" }]} workspaces={[]} onToast={() => {}} onOpenSettings={() => {}} />);
  fireEvent.click(screen.getByRole("button", { name: "发送" }));
  await screen.findByText(/新附件与沿用的历史附件合计最多 20 个/);
  expect(groupSend).not.toHaveBeenCalled();
  expect(screen.getByPlaceholderText(/说明目标/)).toHaveValue("结合新资料继续");
  fireEvent.click(screen.getByText("沿用此前任务附件（已选 20/20）"));
  fireEvent.click(screen.getByRole("checkbox", { name: "资料-0.txt" }));
  view.unmount();
  view = render(<GroupWorkspace cwd="/work" models={[{ id: "test", label: "测试" }]} workspaces={[]} onToast={() => {}} onOpenSettings={() => {}} />);
  expect(screen.getByText("沿用此前任务附件（已选 19/20）")).toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "发送" }));
  await waitFor(() => expect(groupSend).toHaveBeenCalledWith(current.id, expect.objectContaining({ sourceRequestId: "original", sourceAttachmentPaths: paths.slice(1), attachments: ["/work/新增资料.txt"] })));
});

it("执行中补充附件超限可转为关联新一轮，保留原任务和输入", async () => {
  const current = structuredClone(group);
  const paths = Array.from({ length: 20 }, (_, index) => `/work/执行资料-${index}.txt`);
  current.messages = [{ ...current.messages[0], id: "running-request", seq: 1, kind: "user", memberId: null, text: "处理原任务", attachments: paths }];
  current.works = [{ id: "running-work", rootId: "running-request", memberId: "c", parentId: null, continuationOf: null, prompt: "处理", status: "running", discussion: false, sessionId: "native-c", output: "", error: null, createdAt: 1, tokens: 0 }];
  useGroupStore.setState({ current, drafts: { [current.id]: "结合追加资料检查结果" }, outbox: {} });
  saveDraftAttachments(`group:${current.id}`, ["/work/追加资料.txt"]);
  let view = render(<GroupWorkspace cwd="/work" models={[{ id: "test", label: "测试" }]} workspaces={[]} onToast={() => {}} onOpenSettings={() => {}} />);
  fireEvent.click(screen.getByRole("button", { name: "发送" }));
  fireEvent.click(await screen.findByRole("button", { name: "改为关联新一轮，选择需要沿用的附件" }));
  expect(screen.getByPlaceholderText(/说明目标/)).toHaveValue("结合追加资料检查结果");
  fireEvent.click(screen.getByText("沿用此前任务附件（已选 20/20）"));
  fireEvent.click(screen.getByRole("button", { name: "不沿用附件" }));
  view.unmount();
  view = render(<GroupWorkspace cwd="/work" models={[{ id: "test", label: "测试" }]} workspaces={[]} onToast={() => {}} onOpenSettings={() => {}} />);
  expect(screen.getByText("沿用此前任务附件（已选 0/20）")).toBeInTheDocument();
  expect(useGroupStore.getState().current?.works[0].status).toBe("running");
  fireEvent.click(screen.getByRole("button", { name: "发送" }));
  await waitFor(() => expect(groupSend).toHaveBeenCalledWith(current.id, expect.objectContaining({ newRequest: true, requestId: null, sourceRequestId: "running-request", sourceAttachmentPaths: [], attachments: ["/work/追加资料.txt"] })));
  expect(groupControl).not.toHaveBeenCalled();
});
