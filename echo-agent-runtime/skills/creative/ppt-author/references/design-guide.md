# PPT 设计规范(ppt-author)

本文件供 LLM 生成演示文稿前参考。生成专业 PPT 前请通读本规范,严格执行。

## 1. 配色规范
- 主题色由用户指定(十六进制)。围绕主题色构建协调三色:主色(标题/强调)、辅色(主色降低饱和度或取邻近色,用于副标题/装饰)、中性色(正文用深灰 #333333,背景留白用白或极浅灰)。
- 对比:正文与背景对比度不低于 4.5:1(可读性)。标题可用主色,正文不用高饱和色。
- 一份 PPT 全程只用这一套配色,不要每页换色。

## 2. 版式规范(按页类型)
- 封面页:大标题(主题)+ 副标题(日期/作者)+ 主题色色块或底纹。
- 目录页:3-6 个章节条目,编号 + 标题。
- 章节分隔页:大号章节标题 + 主题色背景。
- 内容页:页标题 + 3-5 个要点(bullet),每个要点一行,避免整段文字。
- 数据页:用表格或简单图形呈现指标,不堆文字。
- 总结页:3 条以内关键结论 + 行动项。

## 3. 排版规范
- 字号层级:封面主标题 40-44pt,页标题 28-32pt,正文 18-22pt,注释 14pt。
- 行距 1.2-1.5;每页留白充足,内容不顶到边缘(页边距至少 0.5 英寸)。
- 中文字体统一设为"微软雅黑"(见下方代码),英文/数字可用 Arial。

## 4. 页数节奏
- N 页分配:封面 1 + 目录 1 +(可选章节页)+ 正文 (N-3~N-4) + 总结 1。
- 正文每页信息密度控制在 3-5 个要点,内容多则拆页,不要塞满。

## 5. 中文字体设置(python-pptx)
```python
from pptx.util import Pt
from pptx.dml.color import RGBColor

def style_runs(text_frame, size_pt, hex_color="333333"):
    for paragraph in text_frame.paragraphs:
        for run in paragraph.runs:
            run.font.name = "微软雅黑"
            run.font.size = Pt(size_pt)
            run.font.color.rgb = RGBColor.from_string(hex_color)
```

## 6. 完整代码样例(可直接仿写)
以下样例生成一份带主题色封面、目录、内容页、总结页的 PPT。生成时把 THEME_COLOR、标题、要点替换为实际内容。

```python
from pptx import Presentation
from pptx.util import Inches, Pt
from pptx.dml.color import RGBColor

THEME_COLOR = "1F6FEB"  # 用户主题色(十六进制,无 #)
prs = Presentation()

# 封面
s = prs.slides.add_slide(prs.slide_layouts[0])
s.shapes.title.text = "演示文稿标题"
s.placeholders[1].text = "2026-06"
for p in s.shapes.title.text_frame.paragraphs:
    for r in p.runs:
        r.font.name = "微软雅黑"; r.font.size = Pt(40)
        r.font.color.rgb = RGBColor.from_string(THEME_COLOR)

# 目录
s = prs.slides.add_slide(prs.slide_layouts[1])
s.shapes.title.text = "目录"
tf = s.placeholders[1].text_frame
tf.text = "1. 背景"
for item in ["2. 现状", "3. 方案", "4. 总结"]:
    p = tf.add_paragraph(); p.text = item

# 内容页
s = prs.slides.add_slide(prs.slide_layouts[1])
s.shapes.title.text = "核心要点"
tf = s.placeholders[1].text_frame
tf.text = "要点一"
for item in ["要点二", "要点三"]:
    p = tf.add_paragraph(); p.text = item
for p in tf.paragraphs:
    for r in p.runs:
        r.font.name = "微软雅黑"; r.font.size = Pt(20)
        r.font.color.rgb = RGBColor.from_string("333333")

# 总结
s = prs.slides.add_slide(prs.slide_layouts[1])
s.shapes.title.text = "总结"
s.placeholders[1].text_frame.text = "关键结论与行动项"

prs.save("output.pptx")
print("saved: output.pptx")
```

## 7. 不做什么
- 不依赖外部图片(除非用户开启配图);默认用形状/色块。
- 不使用 python-pptx 不稳定的高级特性(复杂图表动画等)。
- 不一页堆超过 5 个要点。

