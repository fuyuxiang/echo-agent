---
name: image-gen
description: "Generate images via DALL-E, Stable Diffusion, or free alternatives. Supports multi-channel delivery."
version: 1.0.0
metadata:
  echo:
    tags: [Image, Generation, DALL-E, StableDiffusion, Creative]
    requires:
      anyEnv: [OPENAI_API_KEY, STABILITY_API_KEY]
---

# Image Generation

Generate images from text prompts. Multiple provider options.

## Option A: OpenAI DALL-E 3 (Best quality)

Requires `OPENAI_API_KEY`.

```python
from openai import OpenAI
client = OpenAI()
response = client.images.generate(
    model="dall-e-3",
    prompt="A serene mountain lake at sunset, photorealistic",
    size="1024x1024",
    quality="standard",  # or "hd"
    n=1,
)
image_url = response.data[0].url
```

Sizes: `1024x1024`, `1792x1024`, `1024x1792`

## Option B: Pollinations.ai (Free, no key)

```bash
# Simple GET request - returns PNG
curl "https://image.pollinations.ai/prompt/A%20cat%20wearing%20sunglasses" -o output.png

# With parameters
curl "https://image.pollinations.ai/prompt/YOUR_PROMPT?width=1024&height=1024&seed=42" -o output.png
```

## Option C: Stability AI

Requires `STABILITY_API_KEY`.

```bash
curl -X POST "https://api.stability.ai/v2beta/stable-image/generate/sd3" \
  -H "authorization: Bearer $STABILITY_API_KEY" \
  -H "accept: image/*" \
  -F "prompt=lighthouse on a cliff" \
  -F "output_format=png" \
  -o output.png
```

## Script

```bash
python3 scripts/generate_image.py "A cat in space" --provider dalle --output /tmp/gen.png
python3 scripts/generate_image.py "Logo design" --provider dalle --size 1024x1024
python3 scripts/generate_image.py "Mountains" --provider stability --output landscape.png
```

Requires API key environment variable:
- DALL-E: `export OPENAI_API_KEY=sk-xxx`
- Stability AI: `export STABILITY_API_KEY=sk-xxx`

## Prompt Tips

- Be specific: "a red fox sitting in snow, photorealistic, 4K" > "fox"
- Style keywords: photorealistic, watercolor, oil painting, pixel art, anime
- Negative: avoid ambiguous words, be descriptive
