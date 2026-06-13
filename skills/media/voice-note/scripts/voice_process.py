#!/usr/bin/env python3
"""Voice note processor: transcribe audio using faster-whisper."""

import argparse
import sys
from pathlib import Path

try:
    from echo_agent.dependencies import require
    require("skill.voice-note")
except ImportError:
    pass


def transcribe(audio_file, model_size="base", language=None, output=None):
    try:
        from faster_whisper import WhisperModel
    except ImportError:
        sys.exit("Install: pip install faster-whisper")

    model = WhisperModel(model_size, device="cpu", compute_type="int8")
    segments, info = model.transcribe(audio_file, language=language)

    print(f"Detected language: {info.language} (prob={info.language_probability:.2f})")
    text_parts = []
    for segment in segments:
        line = f"[{segment.start:.1f}s -> {segment.end:.1f}s] {segment.text}"
        print(line)
        text_parts.append(segment.text)

    full_text = " ".join(text_parts)
    if output:
        Path(output).write_text(full_text)
        print(f"\nTranscription saved: {output}")
    return full_text


def summarize(audio_file, model_size="base"):
    text = transcribe(audio_file, model_size)
    word_count = len(text.split())
    print("\n--- Summary ---")
    print(f"Total words: {word_count}")
    print(f"Full text: {text[:500]}{'...' if len(text) > 500 else ''}")


def main():
    parser = argparse.ArgumentParser(description="Voice note processor")
    sub = parser.add_subparsers(dest="cmd")
    p = sub.add_parser("transcribe")
    p.add_argument("file", help="Audio file path")
    p.add_argument("--model", default="base", choices=["tiny", "base", "small", "medium", "large-v3"])
    p.add_argument("--language", help="Language code (e.g., zh, en)")
    p.add_argument("--output", "-o", help="Save transcription to file")
    p = sub.add_parser("summarize")
    p.add_argument("file")
    p.add_argument("--model", default="base")
    args = parser.parse_args()

    if args.cmd == "transcribe":
        transcribe(args.file, args.model, args.language, args.output)
    elif args.cmd == "summarize":
        summarize(args.file, args.model)
    else:
        parser.print_help()


if __name__ == "__main__":
    main()
