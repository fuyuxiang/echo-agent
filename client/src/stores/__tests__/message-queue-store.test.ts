import { describe, it, expect, beforeEach } from "vitest";
import {
  useMessageQueueStore,
  hasActiveItems,
  queueTerminalPolicy,
} from "../message-queue-store";

const resetStore = () => useMessageQueueStore.setState({ queues: {} });

describe("message-queue-store — 入队与读取", () => {
  beforeEach(resetStore);

  it("enqueue 追加到末尾并返回 id", () => {
    const id = useMessageQueueStore.getState().enqueue("s1", "first");
    expect(typeof id).toBe("string");
    const q = useMessageQueueStore.getState().getQueue("s1");
    expect(q).toHaveLength(1);
    expect(q[0].text).toBe("first");
    expect(q[0].status).toBe("queued");
  });

  it("enqueue 保留并去重附件路径", () => {
    useMessageQueueStore.getState().enqueue(
      "s1",
      "请优化",
      ["/tmp/方案.docx", "/tmp/方案.docx"],
    );
    expect(useMessageQueueStore.getState().getQueue("s1")[0].attachments).toEqual([
      "/tmp/方案.docx",
    ]);
  });

  it("多条按顺序排列", () => {
    const s = useMessageQueueStore.getState();
    s.enqueue("s1", "a");
    s.enqueue("s1", "b");
    s.enqueue("s1", "c");
    expect(s.getQueue("s1").map((i) => i.text)).toEqual(["a", "b", "c"]);
  });

  it("不同会话隔离", () => {
    const s = useMessageQueueStore.getState();
    s.enqueue("s1", "a");
    s.enqueue("s2", "b");
    expect(s.getQueue("s1").map((i) => i.text)).toEqual(["a"]);
    expect(s.getQueue("s2").map((i) => i.text)).toEqual(["b"]);
  });

  it("空会话返回空数组", () => {
    expect(useMessageQueueStore.getState().getQueue("nope")).toEqual([]);
  });
});

describe("message-queue-store — 编辑/删除/重排", () => {
  beforeEach(resetStore);

  it("update 修改文本", () => {
    const s = useMessageQueueStore.getState();
    const id = s.enqueue("s1", "old");
    s.update("s1", id, "new");
    expect(store().getQueue("s1")[0].text).toBe("new");
  });

  it("update 不存在的 id 无副作用(内容不变)", () => {
    const s = useMessageQueueStore.getState();
    s.enqueue("s1", "a");
    s.update("s1", "nope", "x");
    // 内容不变(用值断言,不依赖引用相等)。
    expect(store().getQueue("s1").map((i) => i.text)).toEqual(["a"]);
  });

  it("remove 删除指定项", () => {
    const s = useMessageQueueStore.getState();
    const id = s.enqueue("s1", "a");
    s.enqueue("s1", "b");
    s.remove("s1", id);
    expect(store().getQueue("s1").map((i) => i.text)).toEqual(["b"]);
  });

  it("remove 最后一条后会话键被清理", () => {
    const s = useMessageQueueStore.getState();
    const id = s.enqueue("s1", "a");
    s.remove("s1", id);
    expect(store().queues["s1"]).toBeUndefined();
  });

  it("reorder 把首条移到末尾", () => {
    const s = useMessageQueueStore.getState();
    s.enqueue("s1", "a");
    s.enqueue("s1", "b");
    s.enqueue("s1", "c");
    s.reorder("s1", 0, 2);
    expect(store().getQueue("s1").map((i) => i.text)).toEqual(["b", "c", "a"]);
  });

  it("reorder 越界 to 被 clamp", () => {
    const s = useMessageQueueStore.getState();
    s.enqueue("s1", "a");
    s.enqueue("s1", "b");
    s.reorder("s1", 0, 99);
    expect(store().getQueue("s1").map((i) => i.text)).toEqual(["b", "a"]);
  });

  it("reorder 相同位置无副作用(顺序不变)", () => {
    const s = useMessageQueueStore.getState();
    s.enqueue("s1", "a");
    s.enqueue("s1", "b");
    s.reorder("s1", 1, 1);
    expect(store().getQueue("s1").map((i) => i.text)).toEqual(["a", "b"]);
  });

  it("reorder 非法 from 无副作用", () => {
    const s = useMessageQueueStore.getState();
    s.enqueue("s1", "a");
    s.reorder("s1", 5, 0);
    expect(store().getQueue("s1").map((i) => i.text)).toEqual(["a"]);
  });
});

describe("message-queue-store — 暂停/恢复", () => {
  beforeEach(resetStore);

  it("停止只暂停该任务的待发送条目，保留顺序、附件和正在发送的身份", () => {
    const s = useMessageQueueStore.getState();
    const active = s.enqueue("s1", "当前执行");
    s.claimById("s1", active, "active-prompt");
    const waiting = s.enqueue("s1", "稍后处理", ["/tmp/方案.docx"]);
    s.enqueue("s2", "其他任务");
    s.pauseQueued("s1");
    expect(s.getQueue("s1")).toMatchObject([
      { id: active, status: "sending", promptId: "active-prompt" },
      { id: waiting, status: "paused", attachments: ["/tmp/方案.docx"] },
    ]);
    expect(s.getQueue("s2")[0].status).toBe("queued");
    s.settleSending("s1", "consume", "active-prompt");
    expect(s.claimNext("s1", "unrelated-followup")).toBeNull();
    s.setStatus("s1", waiting, "queued");
    expect(s.claimById("s1", waiting, "manual-send")?.id).toBe(waiting);
  });

  it("setStatus 切换 paused/queued", () => {
    const s = useMessageQueueStore.getState();
    const id = s.enqueue("s1", "a");
    s.setStatus("s1", id, "paused");
    expect(store().getQueue("s1")[0].status).toBe("paused");
    s.setStatus("s1", id, "queued");
    expect(store().getQueue("s1")[0].status).toBe("queued");
  });

  it("setStatus 不存在的 id 无副作用(状态不变)", () => {
    const s = useMessageQueueStore.getState();
    s.enqueue("s1", "a");
    s.setStatus("s1", "nope", "paused");
    expect(store().getQueue("s1")[0].status).toBe("queued");
  });
});

describe("message-queue-store — shiftNext", () => {
  beforeEach(resetStore);

  it("取第一条 active 并从队列移除", () => {
    const s = useMessageQueueStore.getState();
    s.enqueue("s1", "a");
    s.enqueue("s1", "b");
    const item = s.shiftNext("s1");
    expect(item?.text).toBe("a");
    expect(store().getQueue("s1").map((i) => i.text)).toEqual(["b"]);
  });

  it("跳过 paused 项取下一条 active", () => {
    const s = useMessageQueueStore.getState();
    const id1 = s.enqueue("s1", "a");
    s.enqueue("s1", "b");
    s.setStatus("s1", id1, "paused");
    const item = s.shiftNext("s1");
    expect(item?.text).toBe("b");
    // paused 项保留
    expect(store().getQueue("s1").map((i) => i.text)).toEqual(["a"]);
  });

  it("无 active 项返回 null", () => {
    const s = useMessageQueueStore.getState();
    const id = s.enqueue("s1", "a");
    s.setStatus("s1", id, "paused");
    expect(s.shiftNext("s1")).toBeNull();
    expect(store().getQueue("s1")).toHaveLength(1);
  });

  it("空队列返回 null", () => {
    expect(useMessageQueueStore.getState().shiftNext("s1")).toBeNull();
  });

  it("取走最后一条 active 后会话键被清理", () => {
    const s = useMessageQueueStore.getState();
    s.enqueue("s1", "a");
    s.shiftNext("s1");
    expect(store().queues["s1"]).toBeUndefined();
  });
});

describe("message-queue-store — claimNext", () => {
  beforeEach(resetStore);

  it("原子标记发送中，同一条不会被重复取出", () => {
    const s = useMessageQueueStore.getState();
    s.enqueue("s1", "a");
    s.enqueue("s1", "b");

    expect(s.claimNext("s1")?.text).toBe("a");
    expect(store().getQueue("s1").map((item) => item.status)).toEqual([
      "sending", "queued",
    ]);
    expect(s.claimNext("s1")?.text).toBe("b");
    expect(s.claimNext("s1")).toBeNull();
  });

  it("拒绝后可将 sending 恢复为 queued 重试", () => {
    const s = useMessageQueueStore.getState();
    const id = s.enqueue("s1", "a");
    s.claimNext("s1");
    s.setStatus("s1", id, "queued");

    expect(s.claimNext("s1")?.id).toBe(id);
  });

  it("按 id 认领用户选择的条目并绑定 promptId", () => {
    const s = useMessageQueueStore.getState();
    s.enqueue("s1", "a");
    const second = s.enqueue("s1", "b");

    expect(s.claimById("s1", second, "prompt-b")).toMatchObject({
      id: second,
      text: "b",
      status: "sending",
      promptId: "prompt-b",
    });
    expect(store().getQueue("s1").map((item) => item.status)).toEqual([
      "queued",
      "sending",
    ]);
    expect(s.claimById("s1", second, "duplicate")).toBeNull();
  });
});

describe("message-queue-store — settleSending", () => {
  beforeEach(resetStore);

  it("成功或取消后消费最早的 sending 项", () => {
    const s = useMessageQueueStore.getState();
    const first = s.enqueue("s1", "a");
    s.enqueue("s1", "b");
    s.claimNext("s1");

    expect(s.settleSending("s1", "consume")?.id).toBe(first);
    expect(store().getQueue("s1").map((item) => item.text)).toEqual(["b"]);
  });

  it("可重试失败把 sending 退回 queued", () => {
    const s = useMessageQueueStore.getState();
    const id = s.enqueue("s1", "a");
    s.claimNext("s1");

    expect(s.settleSending("s1", "retry")?.id).toBe(id);
    expect(store().getQueue("s1")[0].status).toBe("queued");
  });

  it("没有 sending 项时不会消费普通队列消息", () => {
    const s = useMessageQueueStore.getState();
    s.enqueue("s1", "a");

    expect(s.settleSending("s1", "consume")).toBeNull();
    expect(store().getQueue("s1").map((item) => item.text)).toEqual(["a"]);
  });

  it("只结算匹配 promptId 的发送项，迟到完成不会删除新轮次", () => {
    const s = useMessageQueueStore.getState();
    const first = s.enqueue("s1", "a");
    const second = s.enqueue("s1", "b");
    s.claimById("s1", first, "old-prompt");
    s.claimById("s1", second, "new-prompt");

    expect(s.settleSending("s1", "consume", "unknown-prompt")).toBeNull();
    expect(store().getQueue("s1")).toHaveLength(2);
    expect(s.settleSending("s1", "consume", "old-prompt")?.id).toBe(first);
    expect(store().getQueue("s1")[0]).toMatchObject({
      id: second,
      status: "sending",
      promptId: "new-prompt",
    });
  });

  it("失败原位恢复同一条目并清除旧 promptId", () => {
    const s = useMessageQueueStore.getState();
    const first = s.enqueue("s1", "a");
    const second = s.enqueue("s1", "b");
    s.claimById("s1", second, "prompt-b");

    expect(s.settleSending("s1", "retry", "prompt-b")?.id).toBe(second);
    expect(store().getQueue("s1").map((item) => item.id)).toEqual([first, second]);
    expect(store().getQueue("s1")[1]).toMatchObject({ status: "queued" });
    expect(store().getQueue("s1")[1].promptId).toBeUndefined();
  });
});

describe("message-queue-store — clear", () => {
  beforeEach(resetStore);

  it("clear 清空整个会话队列", () => {
    const s = useMessageQueueStore.getState();
    s.enqueue("s1", "a");
    s.enqueue("s1", "b");
    s.clear("s1");
    expect(store().getQueue("s1")).toEqual([]);
    expect(store().queues["s1"]).toBeUndefined();
  });

  it("clear 不存在的会话无副作用(s1 队列仍在)", () => {
    const s = useMessageQueueStore.getState();
    s.enqueue("s1", "a");
    s.clear("nope");
    expect(store().getQueue("s1").map((i) => i.text)).toEqual(["a"]);
  });
});

describe("hasActiveItems", () => {
  it("存在 queued 项返回 true", () => {
    expect(hasActiveItems([{ id: "1", text: "a", status: "queued", createdAt: 1 }])).toBe(true);
  });

  it("仅 paused 项返回 false", () => {
    expect(hasActiveItems([{ id: "1", text: "a", status: "paused", createdAt: 1 }])).toBe(false);
  });

  it("仅 sending 项返回 false，避免完成事件重复发送", () => {
    expect(hasActiveItems([{ id: "1", text: "a", status: "sending", createdAt: 1 }])).toBe(false);
  });

  it("空数组返回 false", () => {
    expect(hasActiveItems([])).toBe(false);
  });
});

describe("queueTerminalPolicy", () => {
  it("完整 end_turn 允许自动续发", () => {
    expect(queueTerminalPolicy("end_turn")).toEqual({
      settlement: "consume",
      autoAdvance: true,
      failed: false,
    });
  });

  it("用户取消消费在途项但不继续队列", () => {
    expect(queueTerminalPolicy("cancelled")).toEqual({
      settlement: "consume",
      autoAdvance: false,
      failed: false,
    });
  });

  it("所有异常终态保留队列项供重试且不自动续发", () => {
    for (const reason of [
      "error",
      "rate_limit",
      "rate_limited",
      "refusal",
      "content_filter",
      "max_tokens",
      "max_turns",
    ]) {
      expect(queueTerminalPolicy(reason)).toEqual({
        settlement: "retry",
        autoAdvance: false,
        failed: true,
      });
    }
  });

  it("取消类别可进一步识别安全规则失败", () => {
    expect(queueTerminalPolicy("cancelled", "HookDenied")).toEqual({
      settlement: "retry",
      autoAdvance: false,
      failed: true,
    });
    expect(queueTerminalPolicy("cancelled", "PermissionRejected")).toEqual({
      settlement: "consume",
      autoAdvance: false,
      failed: false,
    });
  });
});

describe("message-queue-store — 进程级故障恢复", () => {
  beforeEach(resetStore);

  it("把所有会话的 sending 项恢复为待确认队列，保留其它状态", () => {
    const s = useMessageQueueStore.getState();
    s.enqueue("s1", "one");
    const pausedId = s.enqueue("s1", "paused");
    s.setStatus("s1", pausedId, "paused");
    s.enqueue("s2", "two");
    s.claimNext("s1");
    s.claimNext("s2");

    s.retryAllSending();

    expect(store().getQueue("s1").map((item) => item.status)).toEqual([
      "paused",
      "paused",
    ]);
    expect(store().getQueue("s2")[0].status).toBe("paused");
  });
});

/** 便捷:重新读取最新 store 快照(避免测试里持过期引用)。 */
function store() {
  return useMessageQueueStore.getState();
}
