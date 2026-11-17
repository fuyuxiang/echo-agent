import { describe, expect, it } from "vitest";
import { projectTaskRows, projectAssetRows, unifyProjectTasks } from "../project-tasks";
import type { ProjectMeta } from "@/stores/projects-store";
import type { TeamProjectSnapshot } from "../team-project-client";
const project: ProjectMeta = { id:"p",name:"历史项目",createdAt:"2026-01-01",tasks:[{id:"t",title:"旧任务",source:"manual",scope:"personal",status:"paused",sessionId:"s"}],conversations:[{sessionId:"s",title:"原会话",createdAt:"2026-01-01"},{sessionId:"old",title:"独立会话",createdAt:"2026-01-02",archived:true}],assets:[],plans:[],experts:[],skills:[],connectors:[],members:[] };
const remote = { id:"r",title:"服务器当前标题",status:"review",description:"当前说明",acceptance:"验收",assigneeId:"a",assigneeName:"A",revision:3,updatedAt:100 } as const;
const snapshot: TeamProjectSnapshot = {id:"shared",name:"项目",goal:"",scopeId:"scope",ownerId:"a",role:"admin",revision:2,updatedAt:100,tasks:[remote],assets:[],runs:[],events:[],members:[],decisions:[]};
describe("one task per project session", () => {
 it("preserves the original history and all legacy task fields with idempotent session migration", () => {
  const tasks=unifyProjectTasks(project.tasks,project.conversations);
  expect(tasks).toHaveLength(2);expect(tasks[0]).toEqual(project.tasks[0]);
  expect(tasks[1]).toMatchObject({sessionId:"old",source:"conversation",sessionArchived:true,title:"独立会话"});
  expect(unifyProjectTasks(tasks,project.conversations)).toEqual(tasks);
  expect(project.conversations).toHaveLength(2);
 });
 it("preserves colliding task ids instead of losing the history entry",()=>{
  const tasks=unifyProjectTasks([{...project.tasks[0],id:"session:old"}],project.conversations);
  expect(new Set(tasks.map(t=>t.id)).size).toBe(tasks.length);expect(tasks.some(t=>t.sessionId==="old")).toBe(true);
 });
 it("shows the remote task exactly once and uses its authoritative title and state",()=>{
  const linked={...project,tasks:[{...project.tasks[0],sharedTasks:{account:{projectId:"shared",taskId:"r"}}}]};
  const rows=projectTaskRows(linked,snapshot,"account");
  expect(rows).toHaveLength(2);expect(rows[0]).toMatchObject({title:remote.title,remote,sessionId:"s",private:false});
  expect(rows[1].private).toBe(true);
  expect(projectTaskRows(linked,null,"account",{},"a","shared")[0]).toMatchObject({remoteId:"r",private:false});
 });
 it("merges the current member's run with a historical local session without revealing another member's session",()=>{
  const state={...snapshot,runs:[{id:"run",taskId:"r",actorId:"a",sessionId:"s",status:"completed" as const,summary:"report",modelId:null,expertName:null,expertVersion:null,updatedAt:100}]};
  expect(projectTaskRows(project,state,"account",{},"a")).toHaveLength(2);
  const foreign=projectTaskRows(project,state,"other",{},"b");expect(foreign).toHaveLength(3);expect(foreign[2].sessionId).toBeUndefined();
 });
 it("does not unlock a pending shared creation or reuse another account's task link",()=>{
  const linked={...project,tasks:[{...project.tasks[0],sharedTasks:{account:{projectId:"shared",requestId:"key"}}}]};
  expect(projectTaskRows(linked,null,"account",{},"a","shared")[0].private).toBe(false);
  expect(projectTaskRows(linked,null,"other",{},"b","shared")[0].private).toBe(true);
 });
 it("merges a local document with the latest shared version without guessing by filename",()=>{
  const asset={id:"doc2",familyId:"family",title:"需求.md",version:2,status:"ready",ftsStatus:"ready",byteSize:12,currentDocumentId:"doc2",updatedAt:100};
  const local={...project,assets:[{id:"file",name:"需求.md",kind:"file" as const,sharedAssets:{account:{projectId:"shared",documentId:"doc1",familyId:"family"}}},{id:"private",name:"需求.md",kind:"file" as const}]};
  const rows=projectAssetRows(local,{...snapshot,assets:[asset]},"account");expect(rows).toHaveLength(2);expect(rows[0].remote).toEqual(asset);expect(rows[1].remote).toBeUndefined();
 });
});
it("keeps earlier local file versions under the same shared material",()=>{
 const link={projectId:"shared",documentId:"doc1",familyId:"family"};
 const local={...project,assets:[{id:"old",name:"old.md",path:"/old.md",kind:"file" as const,sharedAssets:{account:link}},{id:"new",name:"new.md",path:"/new.md",kind:"file" as const,sharedAssets:{account:{...link,documentId:"doc2"}}}]};
 const asset={id:"doc2",familyId:"family",title:"需求.md",version:2,status:"ready",ftsStatus:"ready",byteSize:12,currentDocumentId:"doc2",updatedAt:100};
 const rows=projectAssetRows(local,{...snapshot,assets:[asset]},"account");expect(rows).toHaveLength(1);expect(rows[0].local?.path).toBe("/new.md");expect(rows[0].previousLocals?.[0].path).toBe("/old.md");expect(local.assets).toHaveLength(2);
});
