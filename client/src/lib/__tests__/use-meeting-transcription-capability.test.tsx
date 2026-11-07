// @vitest-environment jsdom
import { renderHook, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: mocks.invoke }));

import { useMeetingTranscriptionCapability } from "../use-meeting-transcription-capability";
import { isMiniMaxMeetingModel } from "../meeting-minutes";

type TestModel = {
  id: string;
  remoteModelId: string;
  providerId: string;
  providerKind: string;
  source: "personal" | "organization" | "legacy";
};

describe("meeting transcription capability", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mocks.invoke.mockResolvedValue(undefined);
  });

  it("checks the actual upstream slug and endpoint for a personal or organization model", async () => {
    const personal: TestModel = { id: "work-model", remoteModelId: "MiniMax-M3", providerId: "personal", providerKind: "custom", source: "personal" };
    const organization: TestModel = { id: "organization/MiniMax-M3", remoteModelId: "MiniMax-M3", providerId: "echoagent-organization", providerKind: "custom", source: "organization" };
    const { result, rerender } = renderHook(({ model }: { model: TestModel }) => useMeetingTranscriptionCapability(model), { initialProps: { model: personal } });

    await waitFor(() => expect(result.current.available).toBe(true));
    expect(mocks.invoke).toHaveBeenCalledWith("meeting_check_connection", { modelId: personal.id, providerId: "personal" });

    rerender({ model: organization });
    expect(result.current.available).toBe(false);
    await waitFor(() => expect(result.current.available).toBe(true));
    expect(mocks.invoke).toHaveBeenCalledWith("meeting_check_connection", { modelId: organization.id, providerId: "echoagent-organization" });
  });

  it("hides unsupported and built-in models without probing", async () => {
    const nonMiniMax: TestModel = { id: "work-model", remoteModelId: "other-chat", providerId: "personal", providerKind: "custom", source: "personal" };
    const builtIn: TestModel = { id: "echoagent-ojlab/MiniMax-M3", remoteModelId: "MiniMax-M3", providerId: "echoagent-ojlab", providerKind: "minimax", source: "legacy" };
    const { result, rerender } = renderHook(({ model }: { model: TestModel }) => useMeetingTranscriptionCapability(model), { initialProps: { model: nonMiniMax } });

    expect(isMiniMaxMeetingModel(nonMiniMax)).toBe(false);
    expect(result.current.available).toBe(false);
    rerender({ model: builtIn });
    expect(result.current.available).toBe(false);
    expect(mocks.invoke).not.toHaveBeenCalled();
  });

  it("keeps the entry hidden when the speech endpoint rejects the connection", async () => {
    mocks.invoke.mockRejectedValue(new Error("转写接口拒绝当前 API Key"));
    const model = { id: "personal/MiniMax-M3", remoteModelId: "MiniMax-M3", providerId: "personal", providerKind: "custom" };
    const { result } = renderHook(() => useMeetingTranscriptionCapability(model));

    await waitFor(() => expect(result.current.state).toBe("unavailable"));
    expect(result.current.available).toBe(false);
    expect(result.current.error).toContain("API Key");
  });
});
