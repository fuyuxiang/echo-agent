---
name: meme-gen
description: "Generate meme images with text overlays using Pillow. Pick templates or create custom image macros."
version: 1.0.0
metadata:
  echo:
    tags: [Meme, Image, Fun, Creative, Social]
    requires:
      pip: [Pillow]
---

# Meme Generator

Create meme images with text overlays.

## Install

```bash
pip install Pillow httpx
```

## Script

```bash
python3 scripts/make_meme.py --template drake --top "写文档" --bottom "写 meme"
python3 scripts/make_meme.py --image url_or_path --top "TOP TEXT" --bottom "BOTTOM TEXT"
python3 scripts/make_meme.py --template distracted --top "新框架" --bottom "当前项目"
```

## Popular Templates

| Template | ID | Use |
|----------|-----|-----|
| Drake | drake | Yes/No preference |
| Distracted Boyfriend | distracted | Attraction comparison |
| Change My Mind | changemymind | Strong opinion |
| This is Fine | thisisfine | Chaos acceptance |
| Two Buttons | twobuttons | Hard choice |

## How It Works

1. Download template image (from imgflip or local cache)
2. Add text with white fill + black outline stroke
3. Auto-wrap long text and auto-size font
4. Save to temp file for channel delivery

## Chinese Text Support

Font resolution order:
1. `/System/Library/Fonts/PingFang.ttc` (macOS)
2. `/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc` (Linux)
3. Bundled fallback font

## Custom Images

```bash
# From URL
python3 scripts/make_meme.py --image "https://example.com/photo.jpg" --top "顶部文字"

# From local file
python3 scripts/make_meme.py --image /tmp/photo.png --bottom "底部文字"
```

## Template API

Browse templates: `curl -s "https://api.imgflip.com/get_memes" | python3 -c "import sys,json; [print(f\"{m['id']}: {m['name']}\") for m in json.load(sys.stdin)['data']['memes'][:20]]"`
