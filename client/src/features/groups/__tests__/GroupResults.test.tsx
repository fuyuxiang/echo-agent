import { afterEach, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { GroupResults } from "../GroupResults";
import { groupRequests } from "../group-presentation";
import type { Group, GroupMessage, GroupWork } from "../group-client";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => []) }));
afterEach(cleanup);
const message = (id: string, text: string, workId: string | null, toolCalls: GroupMessage["toolCalls"] = []): GroupMessage => ({id,seq:1,kind:workId?"assistant":"user",text,memberId:workId?"e":null,createdAt:1,workId,replyTo:null,attachments:[],toolCalls});
const work = (id: string, rootId: string): GroupWork => ({id,rootId,memberId:"e",parentId:null,continuationOf:null,prompt:"交付",status:"completed",discussion:false,sessionId:null,output:"已完成",error:null,createdAt:1,tokens:0});
const file = (id: string, path: string) => ({toolCallId:id,title:"写入文件",kind:"edit",status:"completed",rawInput:{path},content:[{type:"diff",path,oldText:"",newText:"交付内容"}]});
const group = (): Group => ({id:"g",name:"组",goal:"",cwd:"/work",sourceSessionId:null,sourceTeamId:null,coordinatorId:"e",members:[],works:[work("first-work","first"),work("second-work","second"),work("other-work","other")],messages:[message("first","原始任务",null),message("first-result","已完成第一轮","first-work",[file("write-first","/work/first.md")]),{...message("second","继续完善",null),sourceRequestId:"first"},message("second-result","已完成第二轮","second-work",[file("write-second","/work/second.md"),{toolCallId:"read",title:"读取配置",kind:"read",status:"completed",rawInput:{path:"/work/secrets.json"},content:[]}]),message("other","独立任务",null),message("other-result","独立成果","other-work",[file("write-other","/work/other.md")])],decisions:[],archived:false,pinned:false,paused:false,revision:1,updatedAt:1});

it("成果只归集当前任务及关联前轮的真实输出，读取输入和独立任务不会混入", () => {
  const current = group();
  render(<GroupResults group={current} requests={groupRequests(current)} requestId="second" onSelect={vi.fn()} onConclusion={vi.fn()} onError={vi.fn()} />);
  expect(screen.getByRole("button",{name:/first.md/})).toBeInTheDocument();
  expect(screen.getByRole("button",{name:/second.md/})).toBeInTheDocument();
  expect(screen.queryByRole("button",{name:/other.md|secrets.json/})).toBeNull();
  expect(screen.getByText("已完成第二轮")).toBeInTheDocument();
});

it("后来删除的文件不会作为可用成果继续展示", () => {
  const current = group();
  current.messages.push(message("delete","删除旧交付文件","other-work",[{toolCallId:"delete",title:"Delete /work/first.md",kind:"delete",status:"completed",rawInput:{path:"/work/first.md"},content:[]} ]));
  render(<GroupResults group={current} requests={groupRequests(current)} requestId="second" onSelect={vi.fn()} onConclusion={vi.fn()} onError={vi.fn()} />);
  expect(screen.queryByRole("button",{name:/first.md/})).toBeNull();
  expect(screen.getByRole("button",{name:/second.md/})).toBeInTheDocument();
});

it("打开成果失败会在成果面板显示原因并允许重试", async () => {
  const current = group();
  const onError = vi.fn();
  render(<GroupResults group={current} requests={groupRequests(current)} requestId="second" onSelect={vi.fn()} onConclusion={vi.fn()} onError={onError} />);
  vi.mocked(invoke).mockRejectedValueOnce(new Error("文件暂不可访问"));
  fireEvent.click(screen.getByRole("button",{name:/second.md/}));
  expect(await screen.findByRole("alert")).toHaveTextContent("文件暂不可访问");
  expect(onError).toHaveBeenCalledWith("文件暂不可访问");
  fireEvent.click(screen.getByRole("button",{name:/second.md/}));
  expect(screen.queryByRole("alert")).toBeNull();
});

it("不同成员复用工具编号时，独立任务的文件仍不会混入", () => {
  const current = group();
  current.messages.find(message=>message.id==="other-result")!.toolCalls[0].toolCallId = "write-second";
  render(<GroupResults group={current} requests={groupRequests(current)} requestId="second" onSelect={vi.fn()} onConclusion={vi.fn()} onError={vi.fn()} />);
  expect(screen.getByRole("button",{name:/second.md/})).toBeInTheDocument();
  expect(screen.queryByRole("button",{name:/other.md/})).toBeNull();
});
