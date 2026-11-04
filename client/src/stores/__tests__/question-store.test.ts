import { describe, it, expect, beforeEach, vi } from "vitest";
import {
  useQuestionStore,
  selectQuestionForSession,
  hasPendingQuestionForSession,
  type QuestionRequest,
  questionScope,
} from "../question-store";

const resetStore = () => useQuestionStore.getState().clearAll();

function makeQuestion(requestId: string, sessionId: string): QuestionRequest {
  return {
    requestId,
    sessionId,
    toolCallId: `tc-${requestId}`,
    title: `Question ${requestId}`,
    questions: [
      {
        id: "q1",
        question: "选择方案",
        options: ["A", "B", "C"].map((label) => ({ label, description: "" })),
        multiSelect: false,
      },
    ],
    mode: "default",
  };
}

describe("question-store", () => {
  beforeEach(resetStore);

  it("request 入队到对应 sessionId", () => {
    useQuestionStore.getState().request(makeQuestion("r1", "s1"));
    expect(useQuestionStore.getState().queues["s1"]).toHaveLength(1);
    expect(useQuestionStore.getState().queues["s1"][0].requestId).toBe("r1");
  });

  it("同会话多个 question 按顺序排列", () => {
    const s = useQuestionStore.getState();
    s.request(makeQuestion("r1", "s1"));
    s.request(makeQuestion("r2", "s1"));
    expect(useQuestionStore.getState().queues["s1"].map((q) => q.requestId)).toEqual(["r1", "r2"]);
  });

  it("重放同一 requestId 时不重复入队", () => {
    const request = makeQuestion("r1", "s1");
    useQuestionStore.getState().request(request);
    useQuestionStore.getState().request(request);
    expect(useQuestionStore.getState().queues.s1).toHaveLength(1);
  });

  it("关闭通知先于请求或快照到达时不恢复已关闭提问", () => {
    const store = useQuestionStore.getState();
    store.dismiss("closed", "s1");
    store.request(makeQuestion("closed", "s1"));
    expect(hasPendingQuestionForSession("s1")).toBe(false);
    store.request(makeQuestion("new", "s1"));
    expect(useQuestionStore.getState().queues.s1.map((q) => q.requestId)).toEqual(["new"]);
  });

  it("不同会话隔离", () => {
    const s = useQuestionStore.getState();
    s.request(makeQuestion("r1", "s1"));
    s.request(makeQuestion("r2", "s2"));
    expect(useQuestionStore.getState().queues["s1"]).toHaveLength(1);
    expect(useQuestionStore.getState().queues["s2"]).toHaveLength(1);
  });

  it("sessionId 为空时兜底到 __global", () => {
    useQuestionStore.getState().request(makeQuestion("r1", ""));
    expect(useQuestionStore.getState().queues["__global"]).toHaveLength(1);
  });

  it("dismiss 指定 sessionId 只移除该会话中的", () => {
    const s = useQuestionStore.getState();
    s.request(makeQuestion("r1", "s1"));
    s.request(makeQuestion("r2", "s1"));
    s.request(makeQuestion("r3", "s2"));
    s.dismiss("r1", "s1");
    expect(useQuestionStore.getState().queues["s1"].map((q) => q.requestId)).toEqual(["r2"]);
    expect(useQuestionStore.getState().queues["s2"]).toHaveLength(1);
  });

  it("dismiss 不指定 sessionId 则从所有队列中移除", () => {
    const s = useQuestionStore.getState();
    s.request(makeQuestion("r1", "s1"));
    s.request(makeQuestion("r1", "s2"));
    s.dismiss("r1");
    expect(useQuestionStore.getState().queues["s1"]).toHaveLength(0);
    expect(useQuestionStore.getState().queues["s2"]).toHaveLength(0);
  });

  it("dismiss 不存在的 requestId 无副作用", () => {
    useQuestionStore.getState().request(makeQuestion("r1", "s1"));
    useQuestionStore.getState().dismiss("nope", "s1");
    expect(useQuestionStore.getState().queues["s1"]).toHaveLength(1);
  });

  it("dismiss 不存在的 sessionId 无副作用", () => {
    useQuestionStore.getState().request(makeQuestion("r1", "s1"));
    useQuestionStore.getState().dismiss("r1", "no-such");
    expect(useQuestionStore.getState().queues["s1"]).toHaveLength(1);
  });

  it("Agent 退出后清空所有失效提问", () => {
    const s = useQuestionStore.getState();
    s.request(makeQuestion("r1", "s1"));
    s.request(makeQuestion("r2", "s2"));
    s.clearAll();
    expect(useQuestionStore.getState().queues).toEqual({});
  });

  it("删除会话时只清理该会话的待回答问题", () => {
    const store = useQuestionStore.getState();
    store.request(makeQuestion("r1", "s1"));
    store.request(makeQuestion("r2", "s2"));
    store.clearSession("s1");
    expect(useQuestionStore.getState().queues.s1).toBeUndefined();
    expect(useQuestionStore.getState().queues.s2).toHaveLength(1);
  });

  it("排队和重复快照不延长提问期限，迟到回答不能重新打开请求", () => {
    const store = useQuestionStore.getState();
    const clock = vi.spyOn(Date, "now").mockReturnValue(1_000);
    try {
      store.request({ ...makeQuestion("r1", "s1"), timeout: 2 });
      clock.mockReturnValue(2_000);
      store.request({ ...makeQuestion("r1", "s1"), timeout: 2 });
      expect(useQuestionStore.getState().queues.s1[0].expiresAt).toBe(3_000);
      clock.mockReturnValue(3_000);
      expect(store.beginAnswer("s1", "r1")).toBe(false);
      store.dismiss("r1", "s1");
      store.finishAnswer("s1", "r1", "迟到失败");
      expect(useQuestionStore.getState().responses).toEqual({});
    } finally { clock.mockRestore(); }
  });

  it("清理一个任务的回答草稿不会影响另一个任务或待确认提交", () => {
    const store = useQuestionStore.getState();
    store.request(makeQuestion("r1", "s1"));
    store.request(makeQuestion("r2", "s2"));
    store.updateResponse("s1", "r1", response => ({ ...response, customInputs: { q1: "任务一" } }));
    store.updateResponse("s2", "r2", response => ({ ...response, customInputs: { q1: "任务二" } }));
    expect(store.beginAnswer("s2", "r2")).toBe(true);
    expect(store.beginAnswer("s2", "r2")).toBe(false);
    store.clearSession("s1");
    expect(useQuestionStore.getState().responses[questionScope("s1", "r1")]).toBeUndefined();
    expect(useQuestionStore.getState().responses[questionScope("s2", "r2")]).toMatchObject({ customInputs: { q1: "任务二" }, resolving: true });
    store.finishAnswer("s2", "r2", "连接未确认");
    expect(store.beginAnswer("s2", "r2")).toBe(true);
  });
});

describe("selectQuestionForSession", () => {
  beforeEach(resetStore);

  it("返回指定会话的第一个 pending question", () => {
    const s = useQuestionStore.getState();
    s.request(makeQuestion("r1", "s1"));
    s.request(makeQuestion("r2", "s1"));
    const selector = selectQuestionForSession("s1");
    expect(selector(useQuestionStore.getState())?.requestId).toBe("r1");
  });

  it("会话无 pending 返回 null", () => {
    expect(selectQuestionForSession("s1")(useQuestionStore.getState())).toBeNull();
  });

  it("sessionId 为 null 返回 null", () => {
    useQuestionStore.getState().request(makeQuestion("r1", "s1"));
    expect(selectQuestionForSession(null)(useQuestionStore.getState())).toBeNull();
  });

  it("事件处理器可同步检查当前任务是否仍有待回答问题", () => {
    expect(hasPendingQuestionForSession("s1")).toBe(false);
    useQuestionStore.getState().request(makeQuestion("r1", "s1"));
    expect(hasPendingQuestionForSession("s1")).toBe(true);
    expect(hasPendingQuestionForSession("s2")).toBe(false);
  });
});
