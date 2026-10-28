---
name: file-convert
description: "Convert between file formats: CSV↔JSON, Markdown↔HTML, YAML↔JSON, images, and more. No external service needed."
version: 1.0.0
metadata:
  echo:
    tags: [Convert, Files, CSV, JSON, Markdown, HTML, Utility]
---

# File Convert

Common format conversions using Python stdlib + minimal dependencies.

## Quick Conversions

### CSV → JSON
```bash
python3 -c "import csv,json,sys; print(json.dumps(list(csv.DictReader(open(sys.argv[1]))),ensure_ascii=False,indent=2))" data.csv
```

### JSON → CSV
```python
import json, csv, sys
data = json.load(open(sys.argv[1]))
w = csv.DictWriter(sys.stdout, fieldnames=data[0].keys())
w.writeheader(); w.writerows(data)
```

### YAML → JSON
```bash
python3 -c "import yaml,json,sys; print(json.dumps(yaml.safe_load(open(sys.argv[1])),ensure_ascii=False,indent=2))" file.yaml
```

### JSON → YAML
```bash
python3 -c "import yaml,json,sys; print(yaml.dump(json.load(open(sys.argv[1])),allow_unicode=True,default_flow_style=False))" file.json
```

### Markdown → HTML
```bash
pip install markdown
python3 -c "import markdown,sys; print(markdown.markdown(open(sys.argv[1]).read(),extensions=['tables','fenced_code']))" file.md
```

### Base64 encode/decode
```bash
base64 < file.bin > file.b64
base64 -d < file.b64 > file.bin
```

## Universal Converter Script

```bash
python3 scripts/convert.py input.csv output.json
python3 scripts/convert.py data.yaml data.json
python3 scripts/convert.py README.md README.html
```

Auto-detects format from file extension and performs the appropriate conversion.
