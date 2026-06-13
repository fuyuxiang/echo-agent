---
name: text-tools
description: "Text processing toolkit: translate, rewrite, regex, encoding/decoding, and formatting."
version: 1.0.0
metadata:
  echo:
    tags: [Text, Translation, Regex, Encoding, Format, Utility]
---

# Text Tools

Text processing utilities powered by Python stdlib.

## Text Cleaning

```python
import re
# Strip HTML
clean = re.sub(r'<[^>]+>', '', html_text)
# Normalize whitespace
clean = ' '.join(text.split())
# Fix common encoding issues
text.encode('utf-8').decode('utf-8')
```

## Regex Helpers

| Pattern | Matches |
|---------|---------|
| `r'[\w.-]+@[\w.-]+'` | Email addresses |
| `r'https?://\S+'` | URLs |
| `r'1[3-9]\d{9}'` | Chinese phone numbers |
| `r'\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}'` | IPv4 |
| `r'\d{4}-\d{2}-\d{2}'` | Dates (YYYY-MM-DD) |
| `r'[一-鿿]+'` | Chinese characters |

## Encoding/Decoding

```python
from urllib.parse import quote, unquote
import html, base64

# URL encode/decode
quote("你好世界")         # '%E4%BD%A0%E5%A5%BD%E4%B8%96%E7%95%8C'
unquote('%E4%BD%A0')     # '你'

# HTML entities
html.escape('<script>')  # '&lt;script&gt;'
html.unescape('&amp;')  # '&'

# Base64
base64.b64encode(b"hello").decode()  # 'aGVsbG8='
base64.b64decode("aGVsbG8=")         # b'hello'
```

## Word/Character Count

```python
text = "Hello 你好世界"
chars = len(text)           # 8
chars_no_space = len(text.replace(' ', ''))  # 7
words = len(text.split())  # 2
chinese = len(re.findall(r'[一-鿿]', text))  # 3
```

## Text Diff

```python
import difflib
diff = difflib.unified_diff(old.splitlines(), new.splitlines(), lineterm='')
print('\n'.join(diff))
```

## Translation

For translation, use the agent's LLM capability directly — no external API needed. The agent can translate between any languages in-context.

## Script

```bash
python3 scripts/text_process.py clean "  messy   text  "
python3 scripts/text_process.py count "your text here"
python3 scripts/text_process.py regex-extract "email" "Contact: test@example.com"
python3 scripts/text_process.py encode url "你好"
python3 scripts/text_process.py decode base64 "aGVsbG8="
```
