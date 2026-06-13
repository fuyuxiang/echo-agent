---
name: stocks
description: "Query stock prices, fund NAV, crypto rates via free APIs. Supports A-shares, US stocks, HK stocks, and crypto."
version: 1.0.0
metadata:
  echo:
    tags: [Stocks, Finance, Market, Crypto, Fund]
---

# Stocks & Market Data

Query real-time market data from free APIs. No API key required.

## A-Shares (中国A股)

```bash
# Sina Finance (real-time during trading hours)
curl -s "https://hq.sinajs.cn/list=sh600519" | iconv -f gbk -t utf8
# sh=上海, sz=深圳, e.g. sz000001 (平安银行)

# East Money JSON API
curl -s "https://push2.eastmoney.com/api/qt/stock/get?secid=1.600519&fields=f43,f44,f45,f46,f47,f57,f58"
```

## US Stocks

```bash
# Yahoo Finance (free, no key)
curl -s "https://query1.finance.yahoo.com/v8/finance/chart/AAPL?interval=1d&range=5d" | python3 -m json.tool
```

## Crypto

```bash
# CoinGecko (free, no key)
curl -s "https://api.coingecko.com/api/v3/simple/price?ids=bitcoin,ethereum&vs_currencies=usd,cny"
```

## Fund NAV (基金净值)

```bash
# 天天基金 (e.g. 001496)
curl -s "https://fundgz.1234567.com.cn/js/001496.js" | python3 -c "
import sys,json,re
data = re.search(r'jsonpgz\((.*)\)', sys.stdin.read())
if data: d=json.loads(data.group(1)); print(f\"{d['name']}: 净值{d['dwjz']} 估算{d['gsz']} ({d['gszzl']}%)\")
"
```

## Script

```bash
python3 scripts/market_query.py stock 600519          # A-share
python3 scripts/market_query.py stock AAPL            # US stock
python3 scripts/market_query.py crypto bitcoin        # Crypto
python3 scripts/market_query.py fund 001496           # Fund
```

## Watchlist

Configure `~/.echo-agent/watchlist.yaml`:

```yaml
stocks:
  - symbol: sh600519
    name: 贵州茅台
  - symbol: AAPL
    name: Apple
crypto:
  - bitcoin
  - ethereum
funds:
  - "001496"
alert_threshold: 3  # alert if change > 3%
```
