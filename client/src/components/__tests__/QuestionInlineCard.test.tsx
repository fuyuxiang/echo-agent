import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const { clientMocks } = vi.hoisted(() => ({
  clientMocks: {
    resolve: vi.fn(),
    list: vi.fn(),
    listen: vi.fn(),
    closed: undefined as undefined | ((event: { requestId: string; sessionId: string }) => void),
  },
}));

vi.mock("@/lib/agent-client", () => ({
  agentResolveQuestion: clientMocks.resolve,
  agentListPendingInteractions: clientMocks.list,
  onQuestionClosedEvent: clientMocks.listen,
}));

import { QuestionInlineCard } from "../QuestionInlineCard";
import { useQuestionStore, type QuestionRequest } from "@/stores/question-store";

const request: QuestionRequest = {
  requestId: "question-1",
  sessionId: "session-1",
  toolCallId: "tool-1",
  title: "Choose",
  mode: "plan",
  timeout: 2,
  questions: [
    {
      id: "q-1",
      question: "Framework?",
      multiSelect: true,
      options: [
        { id: "react", label: "React", description: "UI", preview: "preview" },
        { id: "vue", label: "Vue", description: "Progressive" },
      ],
    },
  ],
};

const pending = () => ({
  permissions: [],
  questions: [request],
  planApprovals: [],
  folderTrustRequests: [],
});

describe("QuestionInlineCard", () => {
  beforeEach(() => {
    vi.useRealTimers();
    clientMocks.resolve.mockReset();
    clientMocks.list.mockReset().mockResolvedValue(pending());
    clientMocks.listen.mockReset().mockImplementation(async (callback) => {
      clientMocks.closed = callback;
      return vi.fn();
    });
    clientMocks.closed = undefined;
    useQuestionStore.setState({ queues: {} });
    useQuestionStore.getState().request(request);
  });

  it("keeps a multi-select request retryable until the backend ACK is true", async () => {
    const consoleError = vi.spyOn(console, "error").mockImplementation(() => undefined);
    try {
      clientMocks.resolve.mockResolvedValueOnce(false).mockResolvedValueOnce(true);
      render(<QuestionInlineCard sessionId="session-1" />);

      fireEvent.click(screen.getByRole("button", { name: /React/ }));
      fireEvent.click(screen.getByRole("button", { name: /Vue/ }));
      fireEvent.click(screen.getByRole("button", { name: "提交" }));

      expect(await screen.findByRole("alert")).toHaveTextContent("后端未找到该提问请求");
      expect(useQuestionStore.getState().queues["session-1"]).toHaveLength(1);
      expect(clientMocks.resolve).toHaveBeenLastCalledWith("question-1", {
        outcome: "accepted",
        answers: { "Framework?": ["React", "Vue"] },
        annotations: undefined,
      });
      expect(consoleError).toHaveBeenCalledWith("resolve question failed", expect.any(Error));

      fireEvent.click(screen.getByRole("button", { name: "提交" }));
      await waitFor(() => {
        expect(useQuestionStore.getState().queues["session-1"]).toHaveLength(0);
      });
    } finally {
      consoleError.mockRestore();
    }
  });

  it("disables every response when the propagated timeout expires", async () => {
    vi.useFakeTimers();
    render(<QuestionInlineCard sessionId="session-1" />);
    expect(screen.getByText("剩余 2 秒")).toBeInTheDocument();

    await act(async () => {
      vi.advanceTimersByTime(2_100);
    });

    expect(screen.getByText(/已超时/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "提交" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "取消" })).toBeDisabled();
  });

  it("removes the card when the backend closes the reverse request", async () => {
    render(<QuestionInlineCard sessionId="session-1" />);
    await waitFor(() => expect(clientMocks.closed).toBeTypeOf("function"));

    act(() => clientMocks.closed?.({ requestId: "question-1", sessionId: "session-1" }));
    expect(screen.queryByText("Framework?")).not.toBeInTheDocument();
  });

  it("keeps every item in a multi-question request reachable from a keyboard-scrollable region", () => {
    useQuestionStore.setState({ queues: {} });
    useQuestionStore.getState().request({
      ...request,
      questions: [
        request.questions[0],
        {
          id: "q-2",
          question: "MVP scope?",
          multiSelect: false,
          options: [
            { id: "contacts", label: "Contacts", description: "Customer records" },
            { id: "pipeline", label: "Sales pipeline", description: "Opportunity stages" },
          ],
        },
      ],
    });

    render(<QuestionInlineCard sessionId="session-1" />);

    expect(screen.getByText("2 个问题")).toBeInTheDocument();
    expect(screen.getByText("Framework?")).toBeInTheDocument();
    expect(screen.getByText("MVP scope?")).toBeInTheDocument();
    expect(screen.getByRole("region", { name: "Agent 问题列表" })).toHaveAttribute("tabindex", "0");
  });

  it("does not submit while the user confirms a Chinese IME candidate", async () => {
    clientMocks.resolve.mockResolvedValue(true);
    render(<QuestionInlineCard sessionId="session-1" />);
    const input = screen.getByPlaceholderText("输入补充回答…");
    fireEvent.change(input, { target: { value: "中文回答" } });
    fireEvent.keyDown(input, { key: "Enter", isComposing: true });
    fireEvent.keyDown(input, { key: "Enter", keyCode: 229 });
    expect(clientMocks.resolve).not.toHaveBeenCalled();
    fireEvent.keyDown(input, { key: "Enter" });
    await waitFor(() => expect(clientMocks.resolve).toHaveBeenCalledTimes(1));
  });

  it("does not put a late answer failure into another session's question", async () => {
    const next = { ...request, requestId: "question-2", sessionId: "session-2", title: "Second question" };
    clientMocks.list.mockResolvedValue({ ...pending(), questions: [request, next] });
    let rejectAnswer!: (cause: Error) => void;
    clientMocks.resolve.mockImplementation(() => new Promise((_resolve, reject) => { rejectAnswer = reject; }));
    const consoleError = vi.spyOn(console, "error").mockImplementation(() => undefined);
    try {
      const { rerender } = render(<QuestionInlineCard sessionId="session-1" />);
      fireEvent.click(screen.getByRole("button", { name: /React/ }));
      fireEvent.click(screen.getByRole("button", { name: "提交" }));
      act(() => useQuestionStore.getState().request(next));
      rerender(<QuestionInlineCard sessionId="session-2" />);
      await act(async () => rejectAnswer(new Error("旧请求失败")));
      expect(screen.getByText("Second question")).toBeInTheDocument();
      expect(screen.queryByRole("alert")).not.toBeInTheDocument();
      expect(screen.getByRole("button", { name: /React/ })).toBeEnabled();
    } finally {
      consoleError.mockRestore();
    }
  });
});
