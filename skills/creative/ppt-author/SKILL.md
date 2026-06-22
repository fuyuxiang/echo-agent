---
name: ppt-author
description: "Create and edit PowerPoint (.pptx) presentations programmatically. Requires python-pptx."
version: 1.0.0
metadata:
  echo:
    tags: [PowerPoint, Presentation, Slides, Office, Creative]
    requires:
      pip: [python-pptx]
---

# PPT Author

Generate PowerPoint presentations from structured content.

## 生成专业演示文稿前必读

生成正式 PPT 前,**必须**先用 `skill_view` 读取本技能的 `references/design-guide.md`,
并严格按其中的配色、版式、排版、页数节奏规范执行。该文件含可直接仿写的完整代码样例。

## Install

```bash
pip install python-pptx
```

## Quick Start

```python
from pptx import Presentation
from pptx.util import Inches, Pt

prs = Presentation()

# Title slide
slide = prs.slides.add_slide(prs.slide_layouts[0])
slide.shapes.title.text = "项目汇报"
slide.placeholders[1].text = "2026年6月"

# Content slide
slide = prs.slides.add_slide(prs.slide_layouts[1])
slide.shapes.title.text = "核心指标"
body = slide.placeholders[1]
body.text = "月活用户: 100万\n收入增长: 25%\n客户满意度: 4.8/5"

prs.save("report.pptx")
```

## Markdown → Slides

Input format:
```markdown
# Presentation Title
subtitle: 2026-06-13

## Slide Title
- Bullet point 1
- Bullet point 2
- Bullet point 3

## Data Slide
| Metric | Value |
|--------|-------|
| Users  | 100K  |
| Revenue| $1M   |
```

## Script

```bash
python3 scripts/create_pptx.py from-md outline.md --output presentation.pptx
python3 scripts/create_pptx.py quick "Project Update" --slides "Background" "Progress" "Next Steps" --output deck.pptx
```

## Slide Types

- **Title** (layout 0): Title + subtitle
- **Content** (layout 1): Title + bullet points
- **Two Column** (layout 3): Side-by-side comparison
- **Blank** (layout 6): For custom images/charts

## Chinese Font Support

```python
from pptx.util import Pt
from pptx.dml.color import RGBColor

for paragraph in body.paragraphs:
    for run in paragraph.runs:
        run.font.name = '微软雅黑'
        run.font.size = Pt(18)
```
