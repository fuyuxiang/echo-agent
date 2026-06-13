#!/usr/bin/env python3
"""Document text extraction: PDF, DOCX, images (OCR)."""

import argparse
import sys
from pathlib import Path


def extract_pdf(filepath):
    try:
        import pymupdf
    except ImportError:
        sys.exit("Install: pip install pymupdf")
    doc = pymupdf.open(filepath)
    text = ""
    for page in doc:
        text += page.get_text() + "\n"
    doc.close()
    return text.strip()


def extract_docx(filepath):
    try:
        from docx import Document
    except ImportError:
        sys.exit("Install: pip install python-docx")
    doc = Document(filepath)
    return "\n".join(p.text for p in doc.paragraphs)


def extract_image_ocr(filepath):
    try:
        from PIL import Image
    except ImportError:
        sys.exit("Install: pip install Pillow")
    try:
        import pytesseract
    except ImportError:
        sys.exit("Install: pip install pytesseract (and Tesseract OCR binary)")
    img = Image.open(filepath)
    return pytesseract.image_to_string(img, lang="chi_sim+eng")


EXTRACTORS = {
    ".pdf": extract_pdf,
    ".docx": extract_docx,
    ".png": extract_image_ocr,
    ".jpg": extract_image_ocr,
    ".jpeg": extract_image_ocr,
    ".tiff": extract_image_ocr,
    ".bmp": extract_image_ocr,
}


def main():
    parser = argparse.ArgumentParser(description="Document text extractor")
    parser.add_argument("file", help="File to extract text from")
    parser.add_argument("--output", "-o", help="Save to file")
    args = parser.parse_args()

    ext = Path(args.file).suffix.lower()
    extractor = EXTRACTORS.get(ext)
    if not extractor:
        sys.exit(f"Unsupported format: {ext}\nSupported: {list(EXTRACTORS.keys())}")

    text = extractor(args.file)
    if args.output:
        Path(args.output).write_text(text)
        print(f"Extracted {len(text)} chars -> {args.output}")
    else:
        print(text)


if __name__ == "__main__":
    main()
