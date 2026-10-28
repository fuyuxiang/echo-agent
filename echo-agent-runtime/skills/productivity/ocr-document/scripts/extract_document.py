#!/usr/bin/env python3
"""Document text extraction: PDF, DOCX, images (OCR)."""

import argparse
import sys
from pathlib import Path

from echo_agent.dependencies.skill_require import require

# Optional packages are imported *inside* each extractor, after its require()
# call. At module scope they ran before require() could install anything, so on
# a machine missing any one of them the script died on import — the lazy-install
# handshake never got a turn. Importing per-format also means extracting a PDF
# no longer needs the OCR stack installed.


def extract_pdf(filepath):
    require("skill.ocr-document.pdf")
    import pymupdf

    doc = pymupdf.open(filepath)
    text = ""
    for page in doc:
        text += page.get_text() + "\n"
    doc.close()
    return text.strip()


def extract_docx(filepath):
    # Was skill.excel-author (openpyxl) — a spreadsheet dependency for a Word
    # document, so DOCX extraction installed the wrong package and then failed
    # on the python-docx import it actually needs.
    require("skill.ocr-document.docx")
    from docx import Document

    doc = Document(filepath)
    return "\n".join(p.text for p in doc.paragraphs)


def extract_image_ocr(filepath):
    require("skill.ocr-document.image")
    from PIL import Image
    import pytesseract

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
