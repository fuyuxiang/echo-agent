#!/usr/bin/env python3
"""Image generation via OpenAI DALL-E 3 or Stability AI."""

import argparse
import json
import os
import sys
import urllib.request
from pathlib import Path
import base64


def generate_dalle(prompt, size="1024x1024", output="generated.png"):
    api_key = os.environ.get("OPENAI_API_KEY")
    if not api_key:
        sys.exit("Set OPENAI_API_KEY environment variable")
    data = json.dumps({
        "model": "dall-e-3",
        "prompt": prompt,
        "n": 1,
        "size": size,
        "response_format": "b64_json",
    }).encode()
    req = urllib.request.Request(
        "https://api.openai.com/v1/images/generations",
        data=data,
        headers={"Authorization": f"Bearer {api_key}", "Content-Type": "application/json"},
    )
    resp = json.loads(urllib.request.urlopen(req, timeout=60).read())
    img_data = base64.b64decode(resp["data"][0]["b64_json"])
    Path(output).write_bytes(img_data)
    print(f"Image saved: {output}")
    if resp["data"][0].get("revised_prompt"):
        print(f"Revised prompt: {resp['data'][0]['revised_prompt']}")


def generate_stability(prompt, output="generated.png"):
    api_key = os.environ.get("STABILITY_API_KEY")
    if not api_key:
        sys.exit("Set STABILITY_API_KEY environment variable")
    data = json.dumps({
        "text_prompts": [{"text": prompt}],
        "cfg_scale": 7,
        "steps": 30,
        "samples": 1,
    }).encode()
    req = urllib.request.Request(
        "https://api.stability.ai/v1/generation/stable-diffusion-xl-1024-v1-0/text-to-image",
        data=data,
        headers={"Authorization": f"Bearer {api_key}", "Content-Type": "application/json", "Accept": "application/json"},
    )
    resp = json.loads(urllib.request.urlopen(req, timeout=60).read())
    img_data = base64.b64decode(resp["artifacts"][0]["base64"])
    Path(output).write_bytes(img_data)
    print(f"Image saved: {output}")


def main():
    parser = argparse.ArgumentParser(description="AI image generation")
    parser.add_argument("prompt", help="Image description")
    parser.add_argument("--provider", choices=["dalle", "stability"], default="dalle")
    parser.add_argument("--size", default="1024x1024")
    parser.add_argument("--output", "-o", default="generated.png")
    args = parser.parse_args()

    if args.provider == "dalle":
        generate_dalle(args.prompt, args.size, args.output)
    else:
        generate_stability(args.prompt, args.output)


if __name__ == "__main__":
    main()
