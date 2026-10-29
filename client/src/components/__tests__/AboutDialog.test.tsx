import { render, screen } from "@testing-library/react";
import { expect, it, vi } from "vitest";
import { AboutDialog } from "../AboutDialog";

vi.mock("@/lib/agent-client", () => ({ agentAuthStatus: vi.fn(async () => ({
  ready: true,
  providers: ["echoagent-ojlab/Echo Reed", "organization/Echo Reed"],
})) }));

it("关于页面隐藏默认模型和目录的管理前缀，保留来源区分", async () => {
  const { container } = render(<AboutDialog open onClose={vi.fn()} init={{ defaultModelId: "echoagent-ojlab/Echo Reed" } as never} />);
  expect(await screen.findByText("Echo Reed · 内置, Echo Reed · 组织")).toBeInTheDocument();
  expect(screen.getByText("默认模型").nextElementSibling).toHaveTextContent("Echo Reed");
  expect(container.textContent).not.toMatch(/echoagent-ojlab\/|organization\//);
});
