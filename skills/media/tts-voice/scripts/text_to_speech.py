#!/usr/bin/env python3
"""Text-to-speech using edge-tts (Microsoft Edge TTS, free)."""

import argparse
import asyncio
import sys

try:
    from echo_agent.dependencies import require
    require("skill.tts-voice")
except ImportError:
    pass

try:
    import edge_tts
except ImportError:
    sys.exit("Install: pip install edge-tts")


async def synthesize(text, voice="zh-CN-XiaoxiaoNeural", output="output.mp3", rate="+0%"):
    communicate = edge_tts.Communicate(text, voice, rate=rate)
    await communicate.save(output)
    print(f"Audio saved: {output} (voice={voice})")


async def list_voices(language=None):
    voices = await edge_tts.list_voices()
    for v in voices:
        if language and not v["Locale"].startswith(language):
            continue
        print(f"  {v['ShortName']} — {v['Gender']} — {v['Locale']}")


def main():
    parser = argparse.ArgumentParser(description="Text-to-speech (edge-tts)")
    sub = parser.add_subparsers(dest="cmd")
    p = sub.add_parser("speak")
    p.add_argument("text", help="Text to synthesize")
    p.add_argument("--voice", default="zh-CN-XiaoxiaoNeural")
    p.add_argument("--output", "-o", default="output.mp3")
    p.add_argument("--rate", default="+0%", help="Speed: +50%, -20%, etc.")
    p = sub.add_parser("voices")
    p.add_argument("--lang", help="Filter by language (e.g., zh, en)")
    args = parser.parse_args()

    if args.cmd == "speak":
        asyncio.run(synthesize(args.text, args.voice, args.output, args.rate))
    elif args.cmd == "voices":
        asyncio.run(list_voices(args.lang))
    else:
        parser.print_help()


if __name__ == "__main__":
    main()
