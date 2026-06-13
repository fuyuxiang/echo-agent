#!/usr/bin/env python3
"""Make meme images with Pillow text overlay."""

import argparse
import sys
import tempfile
import urllib.request
from pathlib import Path

try:
    from PIL import Image, ImageDraw, ImageFont
except ImportError:
    sys.exit("Install: pip install Pillow")

TEMPLATES = {
    "drake": "https://i.imgflip.com/30b1gx.jpg",
    "distracted": "https://i.imgflip.com/1ur9b0.jpg",
    "changemymind": "https://i.imgflip.com/24y43o.jpg",
    "thisisfine": "https://i.imgflip.com/wxica.jpg",
    "twobuttons": "https://i.imgflip.com/1g8my4.jpg",
}

FONT_PATHS = [
    "/System/Library/Fonts/PingFang.ttc",
    "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.otf",
]


def find_font(size=40):
    for path in FONT_PATHS:
        if Path(path).exists():
            return ImageFont.truetype(path, size)
    try:
        return ImageFont.truetype("DejaVuSans.ttf", size)
    except OSError:
        return ImageFont.load_default()


def wrap_text(text, font, max_width, draw):
    words = list(text)
    lines = []
    current = ""
    for char in words:
        test = current + char
        bbox = draw.textbbox((0, 0), test, font=font)
        if bbox[2] - bbox[0] <= max_width:
            current = test
        else:
            if current:
                lines.append(current)
            current = char
    if current:
        lines.append(current)
    return lines


def draw_text(draw, text, position, font, max_width):
    lines = wrap_text(text, font, max_width, draw)
    x, y = position
    for line in lines:
        bbox = draw.textbbox((0, 0), line, font=font)
        text_w = bbox[2] - bbox[0]
        text_x = x + (max_width - text_w) // 2
        for dx in (-2, 2):
            for dy in (-2, 2):
                draw.text((text_x + dx, y + dy), line, font=font, fill="black")
        draw.text((text_x, y), line, font=font, fill="white")
        y += bbox[3] - bbox[1] + 5


def make_meme(image_source, top_text="", bottom_text="", output=None):
    if image_source.startswith("http"):
        tmp = tempfile.NamedTemporaryFile(suffix=".jpg", delete=False)
        urllib.request.urlretrieve(image_source, tmp.name)
        img = Image.open(tmp.name)
    else:
        img = Image.open(image_source)

    draw = ImageDraw.Draw(img)
    w, h = img.size
    font_size = max(20, w // 15)
    font = find_font(font_size)
    margin = 10

    if top_text:
        draw_text(draw, top_text, (margin, margin), font, w - 2 * margin)
    if bottom_text:
        lines = wrap_text(bottom_text, font, w - 2 * margin, draw)
        line_h = font_size + 5
        y = h - len(lines) * line_h - margin
        draw_text(draw, bottom_text, (margin, y), font, w - 2 * margin)

    output = output or tempfile.mktemp(suffix=".png")
    img.save(output)
    print(f"Meme saved: {output}")
    return output


def main():
    parser = argparse.ArgumentParser(description="Meme generator")
    parser.add_argument("--template", "-t", choices=list(TEMPLATES.keys()))
    parser.add_argument("--image", "-i", help="Custom image URL or path")
    parser.add_argument("--top", default="", help="Top text")
    parser.add_argument("--bottom", default="", help="Bottom text")
    parser.add_argument("--output", "-o", help="Output path")
    args = parser.parse_args()

    if args.template:
        source = TEMPLATES[args.template]
    elif args.image:
        source = args.image
    else:
        parser.error("Provide --template or --image")

    make_meme(source, args.top, args.bottom, args.output)


if __name__ == "__main__":
    main()
