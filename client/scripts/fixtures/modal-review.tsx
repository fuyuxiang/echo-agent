import { useEffect, useState } from "react";
import { useAppDialog } from "../../src/components/AppDialog";

/** Isolated real-component fixtures; no service or native commands are called. */
export default function ModalReview({ variant = "long" }: { variant?: "short" | "long" | "error" }) {
  const { requestInput, requestConfirmation, dialog } = useAppDialog();
  const [result, setResult] = useState("");
  useEffect(() => {
    if (variant === "short") {
      requestConfirmation({
        title: "移除这条记录？",
        description: "记录将从当前列表移除。",
        confirmLabel: "移除",
        danger: true,
        action: () => setResult("记录已移除"),
      });
      return;
    }
    requestInput({
      title: "整理项目说明",
      description: "将项目背景、约束和交付要求分组整理。长内容可以在正文中滚动，标题与操作始终保持可见。",
      fields: Array.from({ length: 7 }, (_, index) => ({
        name: `section-${index}`,
        label: ["项目背景", "用户与使用场景", "操作流程", "数据与约束", "异常与恢复", "验证要求", "交付说明"][index],
        multiline: true,
        rows: 4,
        defaultValue: `第 ${index + 1} 部分：明确内容、操作和最终结果。\n`.repeat(6),
      })),
      confirmLabel: "保存说明",
      action: () => {
        if (variant === "error") throw new Error("暂时无法保存，请保留输入并重试。");
        setResult("项目说明已保存");
      },
    });
  }, [requestConfirmation, requestInput, variant]);
  return <main style={{ padding: 32 }}><h1>项目说明</h1><p>公共弹窗布局与交互验证</p><p role="status">{result}</p>{dialog}</main>;
}
