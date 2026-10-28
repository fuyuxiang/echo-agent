#!/usr/bin/env python3
"""Safe math expression evaluator + unit converter + currency."""

import argparse
import ast
import math
import operator
import sys
from datetime import datetime, date

SAFE_OPS = {ast.Add: operator.add, ast.Sub: operator.sub, ast.Mult: operator.mul,
            ast.Div: operator.truediv, ast.Pow: operator.pow, ast.Mod: operator.mod,
            ast.FloorDiv: operator.floordiv, ast.USub: operator.neg}


def safe_eval(expr: str) -> float:
    expr = expr.replace("^", "**")
    node = ast.parse(expr, mode="eval").body
    return _eval_node(node)


def _eval_node(node):
    if isinstance(node, ast.Constant):
        return node.value
    if isinstance(node, ast.BinOp):
        return SAFE_OPS[type(node.op)](_eval_node(node.left), _eval_node(node.right))
    if isinstance(node, ast.UnaryOp):
        return SAFE_OPS[type(node.op)](_eval_node(node.operand))
    if isinstance(node, ast.Call) and isinstance(node.func, ast.Name):
        func = getattr(math, node.func.id, None)
        if func:
            args = [_eval_node(a) for a in node.args]
            return func(*args)
    raise ValueError(f"Unsupported: {ast.dump(node)}")


UNITS = {
    ("km", "mi"): 0.621371, ("mi", "km"): 1.60934,
    ("m", "ft"): 3.28084, ("ft", "m"): 0.3048,
    ("kg", "lb"): 2.20462, ("lb", "kg"): 0.453592,
    ("cm", "in"): 0.393701, ("in", "cm"): 2.54,
    ("l", "gal"): 0.264172, ("gal", "l"): 3.78541,
    ("c", "f"): None, ("f", "c"): None,
}


def convert_unit(value, from_u, to_u):
    from_u, to_u = from_u.lower(), to_u.lower()
    if (from_u, to_u) == ("c", "f"):
        return value * 9 / 5 + 32
    if (from_u, to_u) == ("f", "c"):
        return (value - 32) * 5 / 9
    factor = UNITS.get((from_u, to_u))
    if factor:
        return value * factor
    raise ValueError(f"Unknown conversion: {from_u} -> {to_u}")


def days_until(target_date: str) -> int:
    target = datetime.strptime(target_date, "%Y-%m-%d").date()
    return (target - date.today()).days


def main():
    parser = argparse.ArgumentParser(description="Calculator")
    sub = parser.add_subparsers(dest="cmd")
    sub.add_parser("eval").add_argument("expr")
    p = sub.add_parser("convert")
    p.add_argument("value", type=float)
    p.add_argument("from_unit")
    p.add_argument("to_unit")
    p = sub.add_parser("date")
    p.add_argument("target")
    p.add_argument("op", choices=["days-until"])
    p = sub.add_parser("currency")
    p.add_argument("amount", type=float)
    p.add_argument("from_cur")
    p.add_argument("to_cur")
    args = parser.parse_args()

    if args.cmd == "eval" or (args.cmd is None and len(sys.argv) > 1):
        expr = args.expr if args.cmd == "eval" else " ".join(sys.argv[1:])
        try:
            print(safe_eval(expr))
        except Exception as e:
            print(f"Error: {e}", file=sys.stderr)
    elif args.cmd == "convert":
        result = convert_unit(args.value, args.from_unit, args.to_unit)
        print(f"{args.value} {args.from_unit} = {result:.4f} {args.to_unit}")
    elif args.cmd == "date":
        print(f"{days_until(args.target)} days until {args.target}")
    elif args.cmd == "currency":
        import urllib.request
        import json
        data = json.loads(urllib.request.urlopen("https://open.er-api.com/v6/latest/" + args.from_cur.upper(), timeout=10).read())
        rate = data["rates"].get(args.to_cur.upper(), 0)
        print(f"{args.amount} {args.from_cur.upper()} = {args.amount * rate:.2f} {args.to_cur.upper()}")
    else:
        parser.print_help()


if __name__ == "__main__":
    main()
