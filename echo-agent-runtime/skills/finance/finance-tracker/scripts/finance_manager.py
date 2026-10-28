#!/usr/bin/env python3
"""Finance tracker with SQLite backend and CSV import."""

import argparse
import csv
import re
import sqlite3
from datetime import date
from pathlib import Path

DB_PATH = Path.home() / ".echo-agent" / "finance.db"

CATEGORIES = ["餐饮", "交通", "购物", "住房", "娱乐", "医疗", "教育", "通讯", "投资", "其他"]


def get_db():
    DB_PATH.parent.mkdir(parents=True, exist_ok=True)
    conn = sqlite3.connect(str(DB_PATH))
    conn.execute("""CREATE TABLE IF NOT EXISTS expenses (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        amount REAL NOT NULL,
        category TEXT NOT NULL,
        note TEXT DEFAULT '',
        date TEXT DEFAULT (date('now')),
        source TEXT DEFAULT 'manual',
        created_at TEXT DEFAULT CURRENT_TIMESTAMP
    )""")
    return conn


def add_expense(amount, category, note="", dt=None):
    conn = get_db()
    dt = dt or date.today().isoformat()
    conn.execute("INSERT INTO expenses (amount, category, note, date) VALUES (?, ?, ?, ?)",
                 (amount, category, note, dt))
    conn.commit()
    print(f"Added: ¥{amount:.2f} [{category}] {note} ({dt})")


def monthly_summary(month=None):
    conn = get_db()
    if not month:
        month = date.today().strftime("%Y-%m")
    rows = conn.execute(
        "SELECT category, SUM(amount) FROM expenses WHERE date LIKE ? GROUP BY category ORDER BY SUM(amount) DESC",
        (f"{month}%",)).fetchall()
    total = sum(r[1] for r in rows)
    print(f"\n📊 {month} 月度支出报表\n{'='*30}")
    for cat, amt in rows:
        pct = amt / total * 100 if total else 0
        bar = "█" * int(pct / 5)
        print(f"  {cat:6s} ¥{amt:>8.2f} ({pct:4.1f}%) {bar}")
    print(f"{'='*30}\n  合计    ¥{total:>8.2f}")


def import_csv(file_path):
    conn = get_db()
    count = 0
    with open(file_path, newline="", encoding="utf-8-sig") as f:
        content = f.read()
    lines = content.strip().split("\n")
    reader = csv.DictReader(lines)
    for row in reader:
        amount_str = ""
        note = ""
        dt = date.today().isoformat()
        for key in row:
            if "金额" in key or "amount" in key.lower():
                amount_str = re.sub(r"[¥￥,]", "", row[key])
            elif "备注" in key or "商品" in key or "描述" in key or "note" in key.lower():
                note = row[key]
            elif "时间" in key or "日期" in key or "date" in key.lower():
                dt_raw = row[key][:10]
                if re.match(r"\d{4}[-/]\d{2}[-/]\d{2}", dt_raw):
                    dt = dt_raw.replace("/", "-")
        try:
            amount = abs(float(amount_str))
            if amount > 0:
                conn.execute("INSERT INTO expenses (amount, category, note, date, source) VALUES (?, ?, ?, ?, ?)",
                             (amount, "其他", note[:50], dt, "csv_import"))
                count += 1
        except (ValueError, TypeError):
            continue
    conn.commit()
    print(f"Imported {count} records from {file_path}")


def trend(months=6):
    conn = get_db()
    today = date.today()
    print(f"\n📈 最近{months}个月趋势\n{'='*30}")
    for i in range(months - 1, -1, -1):
        m = (today.month - i - 1) % 12 + 1
        y = today.year - (1 if today.month - i <= 0 else 0)
        prefix = f"{y}-{m:02d}"
        total = conn.execute("SELECT COALESCE(SUM(amount),0) FROM expenses WHERE date LIKE ?", (f"{prefix}%",)).fetchone()[0]
        bar = "█" * int(total / 500)
        print(f"  {prefix}  ¥{total:>8.2f}  {bar}")


def main():
    parser = argparse.ArgumentParser(description="Finance Tracker")
    sub = parser.add_subparsers(dest="cmd")
    p = sub.add_parser("add")
    p.add_argument("amount", type=float)
    p.add_argument("category", choices=CATEGORIES)
    p.add_argument("--note", default="")
    p.add_argument("--date")
    p = sub.add_parser("summary")
    p.add_argument("--month")
    p = sub.add_parser("import")
    p.add_argument("file")
    p = sub.add_parser("trend")
    p.add_argument("--months", type=int, default=6)
    p = sub.add_parser("budget")
    p.add_argument("category")
    p.add_argument("limit", type=float)
    args = parser.parse_args()

    if args.cmd == "add":
        add_expense(args.amount, args.category, args.note, args.date)
    elif args.cmd == "summary":
        monthly_summary(args.month)
    elif args.cmd == "import":
        import_csv(args.file)
    elif args.cmd == "trend":
        trend(args.months)
    elif args.cmd == "budget":
        conn = get_db()
        month = date.today().strftime("%Y-%m")
        spent = conn.execute("SELECT COALESCE(SUM(amount),0) FROM expenses WHERE category=? AND date LIKE ?",
                             (args.category, f"{month}%")).fetchone()[0]
        pct = spent / args.limit * 100
        status = "⚠️ 超支!" if pct > 100 else "✓ 正常" if pct < 80 else "⚠️ 接近预算"
        print(f"{args.category}: ¥{spent:.2f} / ¥{args.limit:.2f} ({pct:.0f}%) {status}")
    else:
        parser.print_help()


if __name__ == "__main__":
    main()
