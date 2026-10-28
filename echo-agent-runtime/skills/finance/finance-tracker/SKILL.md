---
name: finance-tracker
description: "Track expenses, analyze spending patterns, categorize transactions. Works with bank CSV exports or manual input."
version: 1.0.0
metadata:
  echo:
    tags: [Finance, Expense, Budget, Tracking, Analysis]
---

# Finance Tracker

Personal finance tracking with SQLite backend. Supports Chinese bank CSV imports.

## Storage

SQLite database at `~/.echo-agent/finance.db`.

## Script

```bash
# Add expense
python3 scripts/finance_manager.py add 35.5 餐饮 --note "午餐"

# Import bank CSV
python3 scripts/finance_manager.py import ~/Downloads/alipay_record.csv

# Monthly summary
python3 scripts/finance_manager.py summary --month 2026-06

# Trend (month-over-month)
python3 scripts/finance_manager.py trend --months 6

# Budget check
python3 scripts/finance_manager.py budget 餐饮 3000
```

## Categories (默认)

餐饮, 交通, 购物, 住房, 娱乐, 医疗, 教育, 通讯, 投资, 其他

## CSV Import

Auto-detects format from common Chinese sources:
- 支付宝 (Alipay) — `alipay_record_*.csv`
- 微信支付 (WeChat Pay) — `微信支付账单*.csv`
- 招商银行 — 信用卡账单 CSV
- 工商银行 — 明细导出 CSV

## Database Schema

```sql
CREATE TABLE expenses (
    id INTEGER PRIMARY KEY,
    amount REAL NOT NULL,
    category TEXT NOT NULL,
    note TEXT,
    date TEXT DEFAULT (date('now')),
    source TEXT DEFAULT 'manual',
    created_at TEXT DEFAULT CURRENT_TIMESTAMP
);
```

## Reports

Monthly summary shows total spending by category with percentage breakdown.
Trend analysis compares month-over-month changes per category.
