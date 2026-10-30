import type { QuestionRequest } from "../../src/stores/question-store";

export function questionReviewRequest(variant: string | null, plan = false): QuestionRequest {
  const questions: QuestionRequest["questions"] = [
    {
      id: "style", question: "「opus5 风格」是指 Anthropic Claude Opus 5 的品牌视觉吗？", multiSelect: false,
      options: [
        { label: "Claude Opus 5 品牌视觉（推荐）", description: "暖白 / 奶油底色，珊瑚橙强调色与深灰文字；衬线大标题、无衬线正文，搭配留白与细线条。" },
        { label: "深色沉稳科技风", description: "深墨底色与暖橙高光，更接近发布会物料的暗色版调。" },
        { label: "暖白与大留白杂志排版", description: "更接近纸质杂志，以留白和细横线分隔内容，减少装饰。" },
      ],
    },
    {
      id: "tone", question: "内容调性更想要哪一种？", multiSelect: false,
      options: [
        { label: "扎心吐槽 + 实用方案（推荐）", description: "前半段用真实职场困境引起共鸣，后半段给出可执行的养生清单。" },
        { label: "纯扎心吐槽", description: "以幽默自嘲和职场黑话为主，笑中带痛，方案点到为止。" },
        { label: "纯实用养生方案", description: "直接给出作息、运动、饮食与体检指标清单，风格严肃务实。" },
      ],
    },
    {
      id: "length", question: "篇幅和交付形式？", multiSelect: false,
      options: [
        { label: "约 12 页 PPTX（推荐）", description: "完整结构：封面、共鸣、指标、方案、清单与结尾。" },
        { label: "约 8 页精简版", description: "只保留核心内容，适合快速浏览和分享。" },
        { label: "20 页以上完整版", description: "展开每个主题，配上图表与数据页。" },
      ],
    },
  ];
  if (variant === "multi") questions[0].multiSelect = true;
  if (variant === "freeform") questions[0] = { ...questions[0], question: "还有哪些需要在方案中考虑的要求？", options: [] };
  if (variant === "long") {
    questions[0].question += "请结合受众、演讲时间、投影环境和团队既有品牌规范选择最合适的方向。".repeat(3);
    questions[0].options = questions[0].options.map(option => ({ ...option, description: option.description.repeat(12) }));
  }
  return {
    requestId: "review-question", sessionId: "review-conversation", toolCallId: "review-question-tool",
    title: questions[0].question, mode: plan ? "plan" : "default",
    timeout: variant === "expired" ? 0 : variant === "ending" ? 59 : 1800,
    questions: variant === "single" || variant === "freeform" ? questions.slice(0, 1) : questions,
  };
}
