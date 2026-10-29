import { modelDisplayName } from "./model-presentation";
/**
 * 消息时间线分组纯函数 —— 对齐 EchoAgent `echo-chat-ui/message-timeline`
 * (grouped messages / model-switch dividers / continue-cards / date separators)。
 *
 * EchoAgent 的 ChatMessage 当前不带 model/timestamp,这里用可选字段做前向兼容:
 * 消息可携带 `modelId` / `createdAt`。无这些字段时退化为「无分隔符」(不影响渲染)。
 * 纯函数、无副作用,便于单测。
 */
import type { ChatMessage } from "@/stores/session-store";
import { isUpstreamBrandedModelId } from "@/lib/model-branding";

/** 带可选元数据的消息(前向兼容:缺省字段视为不存在)。 */
export type TimelineMessage = ChatMessage & {
  modelId?: string;
  createdAt?: string | number;
};

/** 时间线节点:分隔符或消息引用。 */
export type TimelineNode =
  | { kind: "date-divider"; label: string; key: string }
  | { kind: "model-divider"; label: string; key: string }
  | { kind: "message"; message: TimelineMessage; index: number };

/**
 * 把消息序列展开成「分隔符 + 消息」的时间线节点序列。
 *
 *  - 日期分隔:相邻消息跨「天」(按 createdAt)时插入。
 *  - 模型分隔:相邻消息的 modelId 变化时插入(显示「已切换到 X」)。
 *  - 无 createdAt/modelId 时不插入对应分隔符。
 */
export function buildTimeline(messages: TimelineMessage[]): TimelineNode[] {
  const nodes: TimelineNode[] = [];
  let prevDay: string | null = null;
  let prevModel: string | undefined;
  messages.forEach((m, i) => {
    // 日期分隔。
    if (m.createdAt != null) {
      const day = dayLabel(m.createdAt);
      if (day && day !== prevDay) {
        nodes.push({ kind: "date-divider", label: day, key: `date-${day}-${i}` });
        prevDay = day;
      }
    }
    // 模型切换分隔。带上游品牌词的 modelId 不生成分隔条 —— 既不显示品牌名,也不用
    // 假标签冒充一次切换;但仍推进 prevModel,后续切回真实模型时能正常生成分隔条。
    if (m.modelId && m.modelId !== prevModel) {
      if (!isUpstreamBrandedModelId(m.modelId)) {
        nodes.push({
          kind: "model-divider",
          label: `已切换到 ${modelDisplayName(m.modelId)}`,
          key: `model-${m.modelId}-${i}`,
        });
      }
      prevModel = m.modelId;
    }
    nodes.push({ kind: "message", message: m, index: i });
  });
  return nodes;
}

/** 把时间戳(ISO 字符串或毫秒)归一为「年月日」中文标签;无效返回 null。 */
export function dayLabel(ts: string | number): string | null {
  const d = new Date(ts);
  if (Number.isNaN(d.getTime())) return null;
  const y = d.getFullYear();
  const m = String(d.getMonth() + 1).padStart(2, "0");
  const day = String(d.getDate()).padStart(2, "0");
  return `${y}-${m}-${day}`;
}

/** 判断两条相邻消息是否构成「模型切换」。 */
export function isModelSwitch(a: TimelineMessage, b: TimelineMessage): boolean {
  return !!a.modelId && !!b.modelId && a.modelId !== b.modelId;
}

/** 统计时间线里的模型切换次数。 */
export function countModelSwitches(messages: TimelineMessage[]): number {
  let n = 0;
  let prev: string | undefined;
  for (const m of messages) {
    if (m.modelId && m.modelId !== prev) {
      if (prev !== undefined) n++;
      prev = m.modelId;
    }
  }
  return n;
}
