import { act } from "@testing-library/react";
import { beforeEach, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { useOrgSessionStore } from "@/stores/org-session-store";
import { captureTeamProjectOperation, projectCollaborationCapabilities, teamProjectRequest, teamProjectUpload } from "../team-project-client";
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => ({})) }));
vi.mock("@/lib/org-client", () => ({ orgSession: vi.fn() }));
beforeEach(() => {
 vi.clearAllMocks();
 vi.mocked(invoke).mockReset().mockResolvedValue({});
 useOrgSessionStore.setState({ session: { loggedIn: true, accountGeneration: 7, serverUrl: "https://org.example", user: { id: "a", username: "a", displayName: "A", role: "member", clearance: 0 } }, boundaryRevision: 1 });
});
it("binds requests and uploads to the native account generation captured before an operation", async () => {
 const operation = captureTeamProjectOperation();
 await teamProjectRequest("snapshot", "project", undefined, undefined, operation);
 await teamProjectUpload("project", "/authorized/file.md", undefined, operation);
 expect(invoke).toHaveBeenCalledWith("org_project_request", expect.objectContaining({ expectedGeneration: 7 }));
 expect(invoke).toHaveBeenCalledWith("org_project_upload", expect.objectContaining({ expectedGeneration: 7 }));
});
it("rejects an old multi-step operation before sending any request under a new identity", () => {
 const operation = captureTeamProjectOperation();
 act(() => useOrgSessionStore.getState().clearSession());
 expect(() => teamProjectUpload("project", "/authorized/file.md", undefined, operation)).toThrow("组织身份或权限已变化");
 expect(invoke).not.toHaveBeenCalled();
});

it("falls back to a working original project API when an old router treats capabilities as a project ID", async () => {
 vi.mocked(invoke).mockRejectedValueOnce(new Error("项目不存在或已失去成员权限")).mockResolvedValueOnce([]);
 await expect(projectCollaborationCapabilities()).resolves.toEqual({version:1,memberSearch:false,memberRoles:false,idempotentCreate:false});
 expect(invoke).toHaveBeenLastCalledWith("org_project_request",expect.objectContaining({action:"list",expectedGeneration:7}));
});
it("reports an unsupported service in user terms and never attempts a write during capability detection", async () => {
 vi.mocked(invoke).mockRejectedValueOnce(new Error("接口不存在")).mockRejectedValueOnce(new Error("接口不存在"));
 await expect(projectCollaborationCapabilities()).rejects.toThrow("个人项目仍可正常使用");
 expect(vi.mocked(invoke).mock.calls.map(([,args])=>(args as {action:string}).action)).toEqual(["capabilities","list"]);
});
