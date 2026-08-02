#!/usr/bin/env python3
"""Document text extraction: PDF, DOCX, images (OCR)."""

import argparse
import sys
from echo_agent.dependencies.skill_require import require  # noqa: E402
from pathlib import Path

import pymupdf  # noqa: E402
from docx import Document  # noqa: E402
from PIL import Image  # noqa: E402
import pytesseract  # noqa: E402


def extract_pdf(filepath):
    require("skill.ocr-document")
    doc = pymupdf.open(filepath)
    text = ""
    for page in doc:
        text += page.get_text() + "\n"
    doc.close()
    return text.strip()


def extract_docx(filepath):
    require("skill.excel-author")
    doc = Document(filepath)
    return "\n".join(p.text for p in doc.paragraphs)


def extract_image_ocr(filepath):
    require("skill.ocr-document")
    require("skill.ocr-document")
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
