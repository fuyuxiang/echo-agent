---
name: excel-author
description: "Create and edit Excel (.xlsx) workbooks with openpyxl. Supports formulas, charts, formatting, and data analysis."
version: 1.0.0
metadata:
  echo:
    tags: [Excel, Spreadsheet, Data, Charts, Office, Creative]
    requires:
      pip: [openpyxl]
---

# Excel Author

Create professional Excel workbooks programmatically.

## Install

```bash
pip install openpyxl
```

## Quick Start

```python
from openpyxl import Workbook
from openpyxl.styles import Font, PatternFill, Alignment, Border, Side

wb = Workbook()
ws = wb.active
ws.title = "月度报表"

# Headers
headers = ["日期", "类别", "金额", "备注"]
ws.append(headers)
for cell in ws[1]:
    cell.font = Font(bold=True, size=12)
    cell.fill = PatternFill("solid", fgColor="4472C4")
    cell.font = Font(bold=True, color="FFFFFF")

# Data
ws.append(["2026-06-01", "餐饮", 85.5, "午餐"])
ws.append(["2026-06-01", "交通", 15.0, "地铁"])
ws.append(["2026-06-02", "购物", 299.0, ""])

# Formula
ws.append(["", "合计", "=SUM(C2:C4)", ""])

# Auto-width
for col in ws.columns:
    max_len = max(len(str(cell.value or "")) for cell in col)
    ws.column_dimensions[col[0].column_letter].width = max_len + 4

wb.save("report.xlsx")
```

## Script

```bash
python3 scripts/create_xlsx.py from-csv data.csv --output report.xlsx
python3 scripts/create_xlsx.py from-json data.json --output report.xlsx
python3 scripts/create_xlsx.py quick "Report Title" --headers Name Age City --output output.xlsx
```

## Templates

- **Expense Report**: Category breakdown with totals
- **Project Tracker**: Status, assignee, due date columns
- **Data Analysis**: Summary statistics + charts

## Charts

```python
from openpyxl.chart import BarChart, Reference

chart = BarChart()
chart.title = "月度支出"
data = Reference(ws, min_col=3, min_row=1, max_row=5)
cats = Reference(ws, min_col=2, min_row=2, max_row=5)
chart.add_data(data, titles_from_data=True)
chart.set_categories(cats)
ws.add_chart(chart, "E2")
```

## Conventions

- Blue cells: input values
- Black cells: calculated/formula
- Green cells: final output/results
