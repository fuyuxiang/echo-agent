import { beforeEach, expect, it, vi } from "vitest";
import { waitFor } from "@testing-library/react";
const mocks = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: mocks.invoke }));
vi.mock("../tauri-kb-reader", () => ({ isTauriAvailable: () => true }));
import { indexTaskArtifacts, registerDocumentArtifact, loadTaskArtifacts, loadSessionDocumentArtifacts, authorizeDocumentArtifact } from "../artifact-catalog";
import { exportOfficeDocument, officeReceiptFromRawOutput, officeInputFromToolCall } from "../document-export";
import { mergeSessionArtifacts } from "../session-artifacts";
import type { ChatMessage } from "@/stores/session-store";
const receipt = { path: "/synthetic/export.docx", format: "docx" as const, byteSize: 123, sha256: "a".repeat(64) };
beforeEach(() => { mocks.invoke.mockReset(); localStorage.clear(); });

it("Agent updates use validated native receipts while reads never register outputs", () => {
  const output = {type:"MCP",server_name:"echoagent-office",tool_name:"office_document_update",is_error:false,output:{OkayOutput:JSON.stringify({...receipt,revision:4})}};
  expect(officeReceiptFromRawOutput(output)).toMatchObject(receipt);
  expect(officeReceiptFromRawOutput({...output,tool_name:"office_document_read"})).toBeNull();
  expect(officeReceiptFromRawOutput({...output,is_error:true})).toBeNull();
  expect(officeInputFromToolCall({toolCallId:"u",kind:"mcp_tool",title:"use_tool",status:"completed",content:[],rawInput:{tool_name:"echoagent-office__office_document_update",tool_input:{path:receipt.path,expectedRevision:3}}})).toEqual({path:receipt.path,expectedRevision:3});
});

it("a delayed canonicalization cannot erase a newer manually exported artifact", async () => {
  let resolve!: (value: { canonicalPath: string }) => void;
  mocks.invoke.mockImplementation(() => new Promise(accept => { resolve = accept; }));
  const messages = [{ id: "m", role: "assistant", parts: [{ kind: "tool_call", toolCall: {
    toolCallId: "write", kind: "edit", title: "Write result", status: "completed",
    content: [{ type: "diff", diff: { path: "/synthetic/result.md", old: "", new: "result" } }],
  } }] }] as ChatMessage[];
  indexTaskArtifacts("s", "任务", "/synthetic", messages);
  registerDocumentArtifact(receipt, "s", "任务", "/synthetic");
  resolve({ canonicalPath: "/synthetic/result.md" });
  await waitFor(() => expect(loadTaskArtifacts().find(item => item.path === "/synthetic/result.md")?.canonicalPath).toBe("/synthetic/result.md"));
  expect(loadTaskArtifacts().map(item => item.path).sort()).toEqual(["/synthetic/export.docx", "/synthetic/result.md"]);
});

it("manual exports bind the native receipt to the session and reload it from durable history", async () => {
  mocks.invoke.mockResolvedValueOnce(receipt);
  await expect(exportOfficeDocument("报告", "# 报告", "docx", "s")).resolves.toEqual(receipt);
  expect(mocks.invoke).toHaveBeenCalledWith("document_export", { request: { title: "报告", markdown: "# 报告", format: "docx" }, sessionId: "s" });
  localStorage.clear();
  mocks.invoke.mockResolvedValueOnce([{ receipt, title: "报告", createdAt: 1 }]);
  const entries = await loadSessionDocumentArtifacts("s");
  expect(mocks.invoke).toHaveBeenLastCalledWith("session_exports_list", { sessionId: "s" });
  expect(entries[0]).toMatchObject({ path: receipt.path, verifiedOutput: true, status: "completed" });
});

it("restores only the clicked native export capability and propagates invalid receipts", async () => {
  mocks.invoke.mockResolvedValueOnce([{ receipt, title: "报告", createdAt: 1 }]);
  const [artifact] = await loadSessionDocumentArtifacts("s");
  mocks.invoke.mockResolvedValueOnce(undefined);
  await authorizeDocumentArtifact("s", artifact);
  expect(mocks.invoke).toHaveBeenLastCalledWith("session_export_authorize", {sessionId: "s", path: receipt.path, sha256: receipt.sha256});
  mocks.invoke.mockRejectedValueOnce(new Error("导出文件已移动、删除或内容已变化"));
  await expect(authorizeDocumentArtifact("s", artifact)).rejects.toThrow("内容已变化");
  mocks.invoke.mockClear();
  await authorizeDocumentArtifact("s", {...artifact, kind: "edit", toolCallId: "ordinary-tool"});
  expect(mocks.invoke).not.toHaveBeenCalled();
});

it("canonical display keys preserve the exact native receipt path", async () => {
  const rawPath = "c:\\outside\\report.docx";
  const [artifact] = mergeSessionArtifacts([{id:"legacy",path:rawPath,kind:"document_export",title:"报告",toolCallId:`document-export:${receipt.sha256}`,status:"completed"}]);
  expect(artifact.path).toBe(rawPath);
  expect(artifact.id).toBe("C:/outside/report.docx");
  mocks.invoke.mockResolvedValueOnce(undefined);
  await authorizeDocumentArtifact("s", artifact);
  expect(mocks.invoke).toHaveBeenLastCalledWith("session_export_authorize", {sessionId:"s",path:rawPath,sha256:receipt.sha256});
});

it("reindexing old writes or deletes preserves the newer native receipt and its authorization", () => {
  registerDocumentArtifact(receipt,"s","报告","/synthetic");
  for (const kind of ["write","delete"]) {
    indexTaskArtifacts("s","报告","/synthetic",[{id:kind,role:"assistant",complete:true,completedAt:1,parts:[{kind:"tool_call",toolCall:{toolCallId:kind,kind,title:kind,status:"completed",rawInput:{path:receipt.path},content:[]}}]}]);
    expect(loadTaskArtifacts().find(item=>item.path===receipt.path)?.toolCallId).toBe(`document-export:${receipt.sha256}`);
  }
});
