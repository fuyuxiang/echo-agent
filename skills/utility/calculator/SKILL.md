---
name: calculator
description: "Math calculations, unit conversions, date/time arithmetic, and currency rates. Python-powered, no API needed for math."
version: 1.0.0
metadata:
  echo:
    tags: [Math, Calculator, Units, Date, Currency, Utility]
---

# Calculator

Math, units, dates, and currency conversion.

## Math Expressions

Safe evaluation via Python:

```python
import ast
result = eval(compile(ast.parse("2**10 + 3.14 * 2", mode='eval'), '', 'eval'))
```

Complex math:
```python
import math
math.sqrt(144)      # 12.0
math.log2(1024)     # 10.0
math.factorial(10)  # 3628800
math.pi             # 3.14159...
```

## Unit Conversions

| Category | Conversions |
|----------|------------|
| Temperature | `C↔F: F = C*9/5+32` |
| Length | km↔mi (×0.621371), m↔ft (×3.28084), cm↔in (×0.393701) |
| Weight | kg↔lb (×2.20462), g↔oz (×0.035274) |
| Volume | L↔gal (×0.264172) |
| Data | KB↔MB↔GB↔TB (÷1024) |

## Date Calculations

```python
from datetime import datetime, timedelta
from zoneinfo import ZoneInfo

# Days between dates
(datetime(2026, 12, 31) - datetime.now()).days

# Add/subtract time
datetime.now() + timedelta(days=30, hours=2)

# Timezone conversion
dt = datetime.now(ZoneInfo("Asia/Shanghai"))
dt.astimezone(ZoneInfo("America/New_York"))
```

## Currency Exchange

Free API (no key required):
```bash
curl -s "https://open.er-api.com/v6/latest/USD" | python3 -c "
import sys,json; d=json.load(sys.stdin)['rates']
print(f\"1 USD = {d['CNY']:.4f} CNY\")
print(f\"1 USD = {d['EUR']:.4f} EUR\")
print(f\"1 USD = {d['JPY']:.2f} JPY\")
"
```

## Common Formulas

```python
# BMI
bmi = weight_kg / (height_m ** 2)

# Compound interest
A = P * (1 + r/n) ** (n*t)

# Percentage change
change = (new - old) / old * 100
```

## Script

```bash
python3 scripts/calc.py "2**32 - 1"
python3 scripts/calc.py convert 100 km mi
python3 scripts/calc.py date "2026-12-31" days-until
python3 scripts/calc.py currency 100 USD CNY
```
