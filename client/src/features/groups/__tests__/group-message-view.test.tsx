import { fireEvent, render, screen } from "@testing-library/react";
import { expect, it, vi } from "vitest";
import { MessageItem } from "@/components/MessageItem";
import { ThemeProvider } from "@/components/ThemeProvider";
import { groupMessageView } from "../group-message-view";
import type { GroupMessage, GroupWork } from "../group-client";

const message: GroupMessage = { id: "live-group-answer", seq: 2, memberId: "expert", kind: "assistant", text: "已找到接口约束。", createdAt: 1, workId: "work", replyTo: null, attachments: [], toolCalls: [{ toolCallId: "read", title: "读取接口", kind: "read_file", status: "in_progress", content: [] }] };
const work: GroupWork = { id: "work", rootId: "question", memberId: "expert", parentId: null, continuationOf: null, prompt: "检查接口", status: "running", discussion: false, sessionId: "native", output: message.text, error: null, createdAt: 0, tokens: 0 };

it("真实消息组件保留专家结论、正确显示运行状态且默认折叠工具详情", () => {
  const onOpenTool = vi.fn();
  const view = groupMessageView(message, work);
  expect(view.complete).toBe(false);
  render(<ThemeProvider><MessageItem message={view} streaming authorName="接口专家" autoExpandProcess={false} onOpenTool={onOpenTool} /></ThemeProvider>);
  expect(screen.getByText("接口专家")).toBeInTheDocument();
  expect(screen.getByRole("status")).toHaveTextContent("已找到接口约束");
  expect(screen.queryByText(/有操作未收尾/)).toBeNull();
  expect(screen.queryByText(/深度思考/)).toBeNull();
  const disclosure = document.querySelector(".execution-process__header")!;
  expect(disclosure).toHaveAttribute("aria-expanded", "false");
  fireEvent.click(disclosure);
  fireEvent.click(document.querySelector("button.toolcall")!);
  expect(onOpenTool).toHaveBeenCalledWith(expect.objectContaining({ toolCallId: "read", status: "in_progress" }));
});

it.each(["completed", "failed", "cancelled", "waiting_children", "uncertain"])("从 Work %s 状态派生已结束的模型回合", status => {
  const view = groupMessageView(message, { ...work, status, error: status === "failed" ? "模型不可用" : null });
  expect(view.complete).toBe(true);
  if (status === "failed") expect(view.stopReason).toBe("error");
  if (status === "cancelled") expect(view.stopReason).toBe("cancelled");
});

it("工具用量变化复用消息视图，状态变化及时更新", () => {
  const running = groupMessageView(message, work);
  expect(groupMessageView(message, { ...work, tokens: 100 })).toBe(running);
  const completed = groupMessageView(message, { ...work, status: "completed" });
  expect(completed).not.toBe(running);
  expect(completed.complete).toBe(true);
});
