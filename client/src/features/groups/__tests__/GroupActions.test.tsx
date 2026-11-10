import { beforeEach, afterEach, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { GroupWorkspace } from "../GroupWorkspace";
import { GroupSidebar } from "../GroupSidebar";
import { GroupActionsHost } from "../GroupActions";
import { useGroupStore } from "../group-store";
import { groupDelete, groupMetadata, groupList, groupSnapshot, type Group } from "../group-client";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => []) }));
vi.mock("@/lib/agent-client", async original => ({ ...await original<typeof import("@/lib/agent-client")>(), agentsList: vi.fn(async () => []) }));
vi.mock("../group-client", async original => ({ ...await original<typeof import("../group-client")>(), groupDelete: vi.fn(), groupMetadata: vi.fn(), groupList: vi.fn(), groupSnapshot: vi.fn() }));
const makeGroup = (id: string): Group => ({id,name:id,goal:"交付",cwd:"/work",sourceSessionId:null,sourceTeamId:null,coordinatorId:"c",members:[],messages:[],works:[],decisions:[],archived:false,pinned:false,paused:false,revision:1,updatedAt:1});
let records: Group[];
beforeEach(() => {
  vi.resetAllMocks(); localStorage.clear(); records=[makeGroup("生命周期验证组")];
  useGroupStore.setState({groups:structuredClone(records),current:null,currentId:null,loading:false,error:null,drafts:{},outbox:{},readRevisions:{}});
  vi.mocked(groupList).mockImplementation(async () => structuredClone(records));
  vi.mocked(groupSnapshot).mockImplementation(async id => structuredClone(records.find(g => g.id === id)!));
  vi.mocked(groupMetadata).mockImplementation(async (id, patch) => { Object.assign(records.find(g=>g.id===id)!,patch); });
  vi.mocked(groupDelete).mockResolvedValue({sessionIds:[]});
});
afterEach(cleanup);
function mount() {return render(<><GroupSidebar onOpen={() => {}} /><GroupWorkspace cwd="/work" models={[]} workspaces={[]} onToast={() => {}} onOpenSettings={() => {}} /><GroupActionsHost /></>);}

it("卡片与侧栏使用相同操作，重命名失败留在弹窗且不打开会话", async () => {
  mount();
  fireEvent.click(screen.getAllByRole("button",{name:"管理专家团 生命周期验证组"})[1]);
  fireEvent.click(screen.getByRole("menuitem",{name:"重命名"}));
  const dialog=screen.getByRole("dialog");
  fireEvent.change(within(dialog).getByRole("textbox",{name:"专家团名称"}),{target:{value:"新的交付组"}});
  vi.mocked(groupMetadata).mockRejectedValueOnce(new Error("写入失败"));
  fireEvent.click(within(dialog).getByRole("button",{name:"保存名称"}));
  expect(await within(dialog).findByRole("alert")).toHaveTextContent("写入失败");
  expect(useGroupStore.getState().currentId).toBeNull();
  fireEvent.click(within(dialog).getByRole("button",{name:"保存名称"}));
  await waitFor(()=>expect(screen.queryByRole("dialog")).toBeNull());
  expect(screen.getAllByRole("button",{name:"管理专家团 新的交付组"})).toHaveLength(2);
});

it("归档可撤销，归档筛选只显示归档专家团，恢复不恢复执行工作", async () => {
  mount();
  fireEvent.click(screen.getAllByRole("button",{name:"管理专家团 生命周期验证组"})[0]);
  fireEvent.click(screen.getByRole("menuitem",{name:"归档专家团"}));
  await screen.findByRole("button",{name:"撤销"});
  expect(screen.queryByRole("button",{name:"打开专家团 生命周期验证组"})).toBeNull();
  fireEvent.click(screen.getByRole("button",{name:"已归档"}));
  await screen.findByRole("button",{name:"打开专家团 生命周期验证组"});
  fireEvent.click(screen.getByRole("button",{name:"撤销"}));
  await waitFor(()=>expect(records[0].archived).toBe(false));
  expect(groupMetadata).toHaveBeenLastCalledWith("生命周期验证组",{archived:false});
});

it("删除失败保留群聊和草稿，重试成功后清理当前选择及持久化草稿", async () => {
  const id="删除失败可重试组"; records=[makeGroup(id)];
  useGroupStore.setState({groups:structuredClone(records),drafts:{[id]:"正文",[`options:${id}`]:"{}"},outbox:{[id]:"待发送"},readRevisions:{[id]:2}});
  vi.mocked(groupDelete).mockRejectedValueOnce(new Error("尚未确认停止"));
  mount();
  fireEvent.click(screen.getAllByRole("button",{name:`管理专家团 ${id}`})[0]);
  fireEvent.click(screen.getByRole("menuitem",{name:"删除专家团…"}));
  const dialog=screen.getByRole("alertdialog");
  fireEvent.click(within(dialog).getByRole("button",{name:"永久删除"}));
  expect(await within(dialog).findByRole("alert")).toHaveTextContent("尚未确认停止");
  expect(useGroupStore.getState().drafts[id]).toBe("正文");
  expect(screen.getByRole("button",{name:`打开专家团 ${id}`})).toBeInTheDocument();
  fireEvent.click(within(dialog).getByRole("button",{name:"永久删除"}));
  await waitFor(()=>expect(screen.queryByRole("alertdialog")).toBeNull());
  expect(useGroupStore.getState().groups).toHaveLength(0);
  expect(useGroupStore.getState().drafts[id]).toBeUndefined();
  expect(useGroupStore.getState().drafts[`options:${id}`]).toBeUndefined();
  expect(useGroupStore.getState().outbox[id]).toBeUndefined();
  expect(JSON.parse(localStorage.getItem("echoagent.group-drafts.v1")!).data[id]).toBeUndefined();
});

it("超过首屏数量的专家团仍可全量搜索，并提供继续加载", async () => {
  records=Array.from({length:55},(_,i)=>makeGroup(`专家团${i}`));
  useGroupStore.setState({groups:structuredClone(records)}); mount();
  expect(screen.queryByRole("button",{name:"打开专家团 专家团54"})).toBeNull();
  fireEvent.change(screen.getByRole("textbox",{name:"搜索专家团"}),{target:{value:"专家团54"}});
  expect(screen.getByRole("button",{name:"打开专家团 专家团54"})).toBeInTheDocument();
  fireEvent.change(screen.getByRole("textbox",{name:"搜索专家团"}),{target:{value:""}});
  fireEvent.click(screen.getByRole("button",{name:/加载更多专家团/}));
  expect(screen.getByRole("button",{name:"打开专家团 专家团40"})).toBeInTheDocument();
});

it("菜单支持键盘导航，Escape 返回入口且不会触发全局快捷键", () => {
  mount();
  const trigger=screen.getAllByRole("button",{name:"管理专家团 生命周期验证组"})[0];
  const globalEscape=vi.fn();document.addEventListener("keydown",globalEscape);
  try {
    fireEvent.click(trigger);
    const menu=screen.getByRole("menu");
    fireEvent.keyDown(menu,{key:"End"});
    expect(screen.getByRole("menuitem",{name:"删除专家团…"})).toHaveFocus();
    fireEvent.keyDown(menu,{key:"Escape"});
    expect(screen.queryByRole("menu")).toBeNull();
    expect(trigger).toHaveFocus();
    expect(globalEscape).not.toHaveBeenCalled();
  } finally {document.removeEventListener("keydown",globalEscape);}
});

it("同一个专家团的菜单只展开实际触发按钮，再次点击可关闭", () => {
  mount();
  const triggers = screen.getAllByRole("button", { name: "管理专家团 生命周期验证组" });
  for (const trigger of triggers) {
    fireEvent.click(trigger);
    const menu = screen.getByRole("menu");
    expect(trigger).toHaveAttribute("aria-expanded", "true");
    expect(trigger).toHaveAttribute("aria-controls", menu.id);
    const other = triggers.find(button => button !== trigger);
    expect(other).toHaveAttribute("aria-expanded", "false");
    expect(other).not.toHaveAttribute("aria-controls");

    fireEvent.pointerDown(trigger);
    fireEvent.click(trigger);
    expect(screen.queryByRole("menu")).toBeNull();
    expect(trigger).toHaveAttribute("aria-expanded", "false");
    expect(trigger).toHaveFocus();
  }
});

it("在侧栏条目上打开右键菜单时，只展开侧栏入口并将焦点还给原控件", () => {
  mount();
  const sidebar = screen.getByRole("region", { name: "最近专家团" });
  const item = within(sidebar).getByRole("button", { name: /^生命周期验证组/ });
  const [sidebarTrigger, cardTrigger] = screen.getAllByRole("button", { name: "管理专家团 生命周期验证组" });
  item.focus();
  fireEvent.contextMenu(item, { clientX: 32, clientY: 48 });

  expect(sidebarTrigger).toHaveAttribute("aria-expanded", "true");
  expect(cardTrigger).toHaveAttribute("aria-expanded", "false");
  expect(screen.getByRole("menuitem", { name: "重命名" })).toHaveFocus();
  fireEvent.keyDown(screen.getByRole("menu"), { key: "Escape" });
  expect(screen.queryByRole("menu")).toBeNull();
  expect(item).toHaveFocus();
});
