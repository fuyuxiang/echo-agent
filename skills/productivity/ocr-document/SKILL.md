---
name: ocr-document
description: "Extract text from PDFs, images, and scanned documents. Uses pymupdf (local) or optional cloud OCR APIs."
version: 1.0.0
metadata:
  echo:
    tags: [OCR, PDF, Document, Extract, Text]
---

# OCR & Document Processing

Extract text from PDFs, scanned images, and documents.

## PDF Text Extraction (PyMuPDF)

Best choice for text-based PDFs:

```bash
pip install pymupdf
```

```python
import pymupdf

doc = pymupdf.open("file.pdf")
for page in doc:
    text = page.get_text()
    print(text)

# All pages at once
full_text = "\n".join(page.get_text() for page in doc)
```

## PDF → Markdown (marker-pdf)

High-quality conversion preserving structure:

```bash
pip install marker-pdf
marker_single file.pdf output_dir/ --output_format markdown
```

## Image OCR

### Surya OCR (Modern ML-based, best for Chinese)

```bash
pip install surya-ocr
surya_ocr image.png --langs zh,en
```

### Pytesseract (Traditional, widely available)

```bash
# Install Tesseract engine first
brew install tesseract tesseract-lang  # macOS
apt install tesseract-ocr tesseract-ocr-chi-sim  # Linux
pip install pytesseract Pillow
```

```python
import pytesseract
from PIL import Image

text = pytesseract.image_to_string(
    Image.open("scan.png"),
    lang="chi_sim+eng"
)
```

## Script

```bash
python3 scripts/extract_document.py document.pdf
python3 scripts/extract_document.py scan.png
python3 scripts/extract_document.py report.pdf --output extracted.txt
```

Auto-detects format by extension: PDF → pymupdf, DOCX → python-docx, Image → pytesseract.
OCR language is controlled by system Tesseract config (e.g., `chi_sim+eng` default).

## Tips

- For scanned PDFs, extract images first then OCR each page
- Preprocessing (deskew, contrast) improves OCR accuracy
- Chinese OCR: surya-ocr > pytesseract for accuracy
