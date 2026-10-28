#!/usr/bin/env python3
"""Market data query: A-shares, US stocks, crypto, funds."""

import argparse
import json
import re
import urllib.request


def get_stock_cn(symbol: str):
    """Query A-share stock via Sina Finance."""
    prefix = "sh" if symbol.startswith("6") else "sz"
    code = f"{prefix}{symbol}"
    url = f"https://hq.sinajs.cn/list={code}"
    req = urllib.request.Request(url, headers={"Referer": "https://finance.sina.com.cn"})
    resp = urllib.request.urlopen(req, timeout=10).read().decode("gbk")
    parts = resp.split('"')[1].split(",")
    if len(parts) < 30:
        return {"error": f"No data for {symbol}"}
    return {"name": parts[0], "open": parts[1], "close_prev": parts[2],
            "price": parts[3], "high": parts[4], "low": parts[5],
            "volume": int(float(parts[8])), "amount": float(parts[9]),
            "change_pct": f"{(float(parts[3]) - float(parts[2])) / float(parts[2]) * 100:.2f}%"}


def get_stock_us(symbol: str):
    """Query US stock via Yahoo Finance."""
    url = f"https://query1.finance.yahoo.com/v8/finance/chart/{symbol}?interval=1d&range=1d"
    req = urllib.request.Request(url, headers={"User-Agent": "EchoAgent/1.0"})
    data = json.loads(urllib.request.urlopen(req, timeout=10).read())
    meta = data["chart"]["result"][0]["meta"]
    return {"symbol": symbol, "price": meta["regularMarketPrice"],
            "prev_close": meta["previousClose"], "currency": meta["currency"],
            "change_pct": f"{(meta['regularMarketPrice'] - meta['previousClose']) / meta['previousClose'] * 100:.2f}%"}


def get_crypto(coin_id: str):
    """Query crypto via CoinGecko."""
    url = f"https://api.coingecko.com/api/v3/simple/price?ids={coin_id}&vs_currencies=usd,cny&include_24hr_change=true"
    data = json.loads(urllib.request.urlopen(url, timeout=10).read())
    if coin_id in data:
        d = data[coin_id]
        return {"coin": coin_id, "usd": d.get("usd"), "cny": d.get("cny"),
                "change_24h": f"{d.get('usd_24h_change', 0):.2f}%"}
    return {"error": f"Coin {coin_id} not found"}


def get_fund(fund_code: str):
    """Query fund NAV via 天天基金."""
    url = f"https://fundgz.1234567.com.cn/js/{fund_code}.js"
    resp = urllib.request.urlopen(url, timeout=10).read().decode()
    match = re.search(r"jsonpgz\((.*)\)", resp)
    if match:
        d = json.loads(match.group(1))
        return {"code": fund_code, "name": d.get("name"), "nav": d.get("dwjz"),
                "estimate": d.get("gsz"), "change_pct": d.get("gszzl", "0") + "%"}
    return {"error": f"Fund {fund_code} not found"}


def main():
    parser = argparse.ArgumentParser(description="Market data query")
    sub = parser.add_subparsers(dest="cmd")
    p = sub.add_parser("stock")
    p.add_argument("symbol")
    p = sub.add_parser("crypto")
    p.add_argument("coin")
    p = sub.add_parser("fund")
    p.add_argument("code")
    args = parser.parse_args()

    if args.cmd == "stock":
        sym = args.symbol.upper()
        if sym.isdigit() and len(sym) == 6:
            result = get_stock_cn(sym)
        else:
            result = get_stock_us(sym)
        if "error" in result:
            print(result["error"])
        else:
            name = result.get("name", result.get("symbol", ""))
            print(f"{name}: ¥{result['price']} ({result['change_pct']})")
    elif args.cmd == "crypto":
        result = get_crypto(args.coin.lower())
        if "error" in result:
            print(result["error"])
        else:
            print(f"{result['coin']}: ${result['usd']} / ¥{result['cny']} ({result['change_24h']})")
    elif args.cmd == "fund":
        result = get_fund(args.code)
        if "error" in result:
            print(result["error"])
        else:
            print(f"{result['name']} ({result['code']}): 净值 {result['nav']} 估算 {result['estimate']} ({result['change_pct']})")
    else:
        parser.print_help()


if __name__ == "__main__":
    main()
