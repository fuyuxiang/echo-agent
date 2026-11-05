import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { ExpertsTab } from "../experts-panel/experts/ExpertsTab";
import { usePendingExpertStore } from "@/stores/pending-expert-store";

const api = vi.hoisted(() => ({ agentsList: vi.fn(), agentsTemplate: vi.fn(), agentsSave: vi.fn() }));
vi.mock("@/lib/agent-client", () => api);

describe("本地专家直接使用与失败恢复", () => {
  beforeEach(() => {
    vi.resetAllMocks();
    usePendingExpertStore.getState().clear();
    api.agentsList.mockResolvedValue([{
      name: "评审专家", description: "检查代码", scope: "user", path: "/agents/review.md",
      raw: "---\nname: 评审专家\n---\n检查正确性和风险。",
    }]);
  });

  it("直接加载本地专家，详情开始对话时保留真实提示词和本地来源", async () => {
    const onGoHome = vi.fn();
    render(<ExpertsTab pills={null} onGoHome={onGoHome} />);
    fireEvent.click(await screen.findByRole("button", { name: "查看专家 评审专家 详情" }));
    fireEvent.click(screen.getByRole("button", { name: "开始对话" }));
    expect(onGoHome).toHaveBeenCalledOnce();
    expect(usePendingExpertStore.getState().expert).toMatchObject({
      name: "评审专家", prompt: "检查正确性和风险。", expertId: "评审专家", source: "local",
    });
    expect(screen.queryByRole("tab", { name: "专家团" })).toBeNull();
  });

  it("列表读取失败显示重试，恢复后仍可使用本地专家", async () => {
    api.agentsList.mockRejectedValueOnce(new Error("目录不可用"));
    const onGoHome = vi.fn();
    render(<ExpertsTab pills={null} onGoHome={onGoHome} />);
    expect(await screen.findByRole("alert")).toHaveTextContent("目录不可用");
    expect(usePendingExpertStore.getState().expert).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "重试" }));
    fireEvent.click(await screen.findByRole("button", { name: "与评审专家对话" }));
    await waitFor(() => expect(onGoHome).toHaveBeenCalledOnce());
    expect(usePendingExpertStore.getState().expert?.source).toBe("local");
  });
});
