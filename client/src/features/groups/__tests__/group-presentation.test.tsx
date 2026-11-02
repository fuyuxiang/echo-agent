import { afterEach, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { ThemeProvider } from "@/components/ThemeProvider";
import { GroupReply } from "../GroupReply";
import { GroupWorkList } from "../GroupWorkList";
import { groupActivity, groupPublicSeq, groupRequests, groupRequestTarget, publicGroupMessages, resultOnlyWorkIds } from "../group-presentation";
import type { Group, GroupMessage, GroupWork } from "../group-client";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => []) }));
afterEach(cleanup);
const member = {id:"member-identity",name:"自定义评审专家",expert:"review",role:"评审",modelId:"test",sessionId:null,discussionSessionId:null,generation:0};
const work = (overrides: Partial<GroupWork> = {}): GroupWork => ({id:"work",rootId:"request",memberId:member.id,parentId:null,continuationOf:null,prompt:"核对结果",status:"running",discussion:false,sessionId:null,output:"",error:null,createdAt:1,tokens:0,...overrides});
const message = (overrides: Partial<GroupMessage> = {}): GroupMessage => ({id:"reply",seq:2,memberId:member.id,kind:"assistant",text:"",createdAt:1,workId:"work",replyTo:null,attachments:[],toolCalls:[],...overrides});
const group = (overrides: Partial<Group> = {}): Group => ({id:"group",name:"用户自己的专家团",goal:"核对",cwd:"/work",sourceSessionId:null,sourceTeamId:null,coordinatorId:member.id,members:[member],messages:[],works:[],decisions:[],archived:false,pinned:false,paused:false,revision:1,updatedAt:1,...overrides});

it("工具活动和空正文不产生专家头像或历史发言", () => {
  const current=group({works:[work()],messages:[message({kind:"activity",toolCalls:[{toolCallId:"tool",status:"in_progress"}]}),message()]});
  expect(publicGroupMessages(current)).toEqual([]);
  expect(groupActivity(current)).toBe("自定义评审专家正在处理任务…");
});

it("多次续跑后缺少回复的任务只允许整理结果，避免重做原始操作", () => {
  const current=group({works:[work({status:"failed"}),work({id:"next",continuationOf:"work",status:"failed"}),work({id:"repair",continuationOf:"next",resultRepair:true,status:"failed"}),work({id:"unrelated"})]});
  expect([...resultOnlyWorkIds(current)].sort()).toEqual(["next","repair","work"]);
});

it("中断的只读结果整理先走核对重试，避免直接汇总被中断状态阻塞", () => {
  const onControl=vi.fn();
  render(<GroupWorkList group={group({works:[work({status:"uncertain",resultRepair:true})]})} busy={false} onControl={onControl} onOpenTool={vi.fn()} />);
  fireEvent.click(screen.getByRole("button",{name:"核对并整理结果"}));
  expect(onControl).toHaveBeenCalledWith("retry","work");
});

it("正文直接呈现，不会被单聊的过程分类折叠", () => {
  const reply=message({text:"已完成核对。结论：必须补充边界验收。"});
  render(<ThemeProvider><GroupReply message={reply} member={member} coordinator conclusion complete config={{}} /></ThemeProvider>);
  expect(screen.getByText(member.name)).toBeInTheDocument();
  expect(screen.getByText("已完成核对。结论：必须补充边界验收。")).toBeInTheDocument();
  expect(screen.getByText("本次结论")).toBeInTheDocument();
  expect(document.querySelector(".execution-process")).toBeNull();
  expect(screen.queryByText("本轮已完成")).toBeNull();
});

it("恢复历史保留的正文，纯工具记录仍然可以在详情中访问", () => {
  const current=group({works:[work({status:"completed",output:"已保存的真实结果"})],messages:[message({kind:"activity",toolCalls:[{toolCallId:"read",status:"completed"}]})]});
  expect(publicGroupMessages(current).map(message=>message.text)).toEqual(["已保存的真实结果"]);
  expect(current.messages[0].toolCalls).toHaveLength(1);
  expect(groupRequests(current)[0].state).toBe("delivered");
});

it("续跑回传的相同结果只显示一次，避免重复头像", () => {
  const current=group({works:[work({status:"completed",output:"最终结论"}),work({id:"continuation",continuationOf:"work",status:"completed",output:"最终结论"})],messages:[message({id:"answer",workId:"continuation",text:"最终结论"})]});
  expect(publicGroupMessages(current)).toHaveLength(1);
  expect(groupRequests(current)[0].finalMessageId).toBe("answer");
});

it("执行结束而没有正文时明确缺少结果，不能显示为已交付", () => {
  const current=group({works:[work({status:"completed"})],messages:[message({kind:"activity"})]});
  expect(groupRequests(current)[0]).toMatchObject({state:"missing_result",finalMessageId:null});
  expect(groupActivity(current)).toBe("");
});

it("后续汇总失败时，不把协调员早先的观点误标为最终结论", () => {
  const current=group({works:[work({status:"completed",output:"初步观点"}),work({id:"summary",summary:true,status:"failed"})],messages:[message({text:"初步观点"})]});
  expect(groupRequests(current)[0]).toMatchObject({state:"failed",finalMessageId:null});
});

it("纯状态和工具更新不增加内容未读序号", () => {
  const current=group({messages:[message({id:"user",seq:1,kind:"user",text:"用户问题"}),message({seq:20,kind:"activity"})]});
  expect(groupPublicSeq(current)).toBe(1);
  expect(groupPublicSeq({...current,revision:50})).toBe(1);
  expect(groupPublicSeq({...current,messages:[...current.messages,message({seq:21,text:"成员回复"})]})).toBe(21);
});

it("协调员总结和结果整理显示真实处理状态，不持续增加消息", () => {
  const current=group({works:[work({summary:true})]});
  expect(groupActivity(current)).toBe("自定义评审专家正在整理结果…");
  expect(groupRequests(current)[0].state).toBe("summarizing");
  expect(publicGroupMessages(current)).toHaveLength(0);
});

 it("回复已结束任务或所选任务失效时，不串入另一个进行中的任务", () => {
  const current=group({works:[work({status:"completed",output:"旧结果"}),work({id:"other-work",rootId:"other-request"})],messages:[message({text:"旧结果"})]});
  const requests=groupRequests(current);
  expect(groupRequestTarget(current,requests,null,false,"reply")).toBeNull();
  expect(groupRequestTarget(current,requests,"missing",false,null)).toBeNull();
  expect(groupRequestTarget(current,requests,null,false,null)).toBe("other-request");
  expect(groupRequestTarget(current,requests,null,true,null)).toBeNull();
});

it("讨论与执行的任务归属互相隔离，切换模式不会升级既有任务权限", () => {
  const current=group({works:[work({discussion:true})]});
  const requests=groupRequests(current);
  expect(groupRequestTarget(current,requests,null,false,null,true)).toBe("request");
  expect(groupRequestTarget(current,requests,null,false,null,false)).toBeNull();
  expect(groupRequestTarget(current,requests,"request",false,null,false)).toBeNull();
});

it("流式正文更新使用内容版本，工具进度不改变未读", () => {
  const current = group({messages:[message({text:"第一段",publicRevision:8})]});
  const read = groupPublicSeq(current);
  const updated = {...current,messages:[{...current.messages[0],text:"第一段及最终结论",publicRevision:9}]};
  expect(groupPublicSeq(updated)).toBeGreaterThan(read);
  expect(groupPublicSeq({...updated,revision:100})).toBe(9);
  expect(groupPublicSeq({...updated,messages:[...updated.messages,message({kind:"activity",seq:500,toolCalls:[{status:"completed"}]})]})).toBe(9);
  expect(groupPublicSeq({...updated,messages:[],publicSeq:9})).toBe(9);
});

it("历史多成员任务更换协调员后仍定位原来的汇总结论", () => {
  const current = group({coordinatorId:"new-owner", works:[work({status:"completed",output:"个人观点"}),work({id:"other",memberId:"new-owner",status:"completed",output:"其他观点"}),work({id:"summary",summary:true,status:"completed",output:"最终结论"})], messages:[message({id:"summary-answer",workId:"summary",text:"最终结论"})]});
  expect(groupRequests(current)[0]).toMatchObject({ownerId:member.id,finalMessageId:"summary-answer",state:"delivered"});
});

it("恢复旧草稿时引用优先于过时的任务选择，新话题仍保持独立", () => {
  const current=group({works:[work(),work({id:"other-work",rootId:"other-request"})],messages:[message({text:"当前任务的进度"})]});
  const requests=groupRequests(current);
  expect(groupRequestTarget(current,requests,"other-request",false,"reply")).toBe("request");
  expect(groupRequestTarget(current,requests,"other-request",true,"reply")).toBeNull();
});
