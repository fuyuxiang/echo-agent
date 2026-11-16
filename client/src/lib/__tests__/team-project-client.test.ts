import { act } from "@testing-library/react";
import { beforeEach, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { useOrgSessionStore } from "@/stores/org-session-store";
import { captureTeamProjectOperation, teamProjectRequest, teamProjectUpload } from "../team-project-client";
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => ({})) }));
vi.mock("@/lib/org-client", () => ({ orgSession: vi.fn() }));
beforeEach(() => {
 vi.clearAllMocks();
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
