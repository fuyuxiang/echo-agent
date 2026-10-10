import { useState } from "react";
import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { ExpertsTab } from "../experts-panel/experts/ExpertsTab";
import { agentsList, agentsSave, agentsTemplate } from "@/lib/agent-client";

vi.mock("@/lib/agent-client", () => ({
  expertsDefaultRoot: vi.fn().mockResolvedValue(null),
  agentsList: vi.fn(),
  agentsTemplate: vi.fn(),
  agentsSave: vi.fn(),
}));

describe("从对话创建专家", () => {
  it("从已有专家雇佣后将新草稿填入当前编辑器并回到名称字段", async () => {
    vi.mocked(agentsList).mockResolvedValue([{
      name: "模板专家", path: "/agents/template.md", scope: "user",
      description: "模板说明", raw: "模板提示词", modelTags: ["default"],
    }]);
    function Harness() {
      const [requested, setRequested] = useState(true);
      return <ExpertsTab pills={null} createExpertRequested={requested} onCreateExpertRequestHandled={() => setRequested(false)} />;
    }
    render(<Harness />);
    await screen.findByRole("dialog", { name: "创建专家" });
    fireEvent.click(await screen.findByRole("button", { name: "从专家雇佣" }));
    fireEvent.click(screen.getByRole("button", { name: /模板专家.*模板说明/ }));
    await waitFor(() => expect(screen.getByRole("textbox", { name: "专家名称" })).toHaveValue("模板专家（副本）"));
    expect(screen.getByRole("textbox", { name: "专家 System Prompt" })).toHaveValue("模板提示词");
    expect(screen.getByRole("textbox", { name: "专家名称" })).toHaveFocus();
  });

  it("进入我的专家后直接打开编辑器，保存后显示新专家", async () => {
    let saved = false;
    vi.mocked(agentsList).mockImplementation(async () => saved ? [{
      name: "评审专家", path: "/agents/review.md", scope: "user",
      description: "评审方案", raw: "专家提示词", modelTags: ["default"],
    }] : []);
    vi.mocked(agentsTemplate).mockResolvedValue("专家提示词");
    vi.mocked(agentsSave).mockImplementation(async () => {
      saved = true;
      return { name: "评审专家", path: "/agents/review.md", scope: "user", raw: "专家提示词" };
    });

    function Harness() {
      const [requested, setRequested] = useState(true);
      return <>
        <button type="button" onClick={() => setRequested(true)}>再次创建专家</button>
        <ExpertsTab pills={null} createExpertRequested={requested} onCreateExpertRequestHandled={() => setRequested(false)} />
      </>;
    }

    render(<Harness />);
    const dialog = await screen.findByRole("dialog", { name: "创建专家" });
    expect(screen.getByRole("heading", { name: "我的专家" })).toBeInTheDocument();
    fireEvent.change(screen.getByRole("textbox", { name: "专家名称" }), { target: { value: "评审专家" } });
    fireEvent.change(screen.getByRole("textbox", { name: "专家 System Prompt" }), { target: { value: "专家提示词" } });
    fireEvent.click(dialog.querySelector("button.btn--primary") as HTMLButtonElement);

    await waitFor(() => expect(agentsSave).toHaveBeenCalledWith("评审专家", "专家提示词"));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "创建专家" })).toBeNull());
    const expertCard = await screen.findByRole("button", { name: "查看专家 评审专家 详情" });
    fireEvent.click(expertCard);
    fireEvent.click(screen.getByRole("button", { name: "再次创建专家" }));
    expect(await screen.findByRole("dialog", { name: "创建专家" })).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "我的专家" })).toBeInTheDocument();
  });
});
