from pathlib import Path

from echo_agent.agent.context import ContextBuilder


def test_transcript_rendered_into_user_message(tmp_path: Path):
    cb = ContextBuilder(tmp_path)
    media = [{"type": "voice", "url": str(tmp_path / "a.ogg"), "name": "语音",
              "transcribed_text": "你好我是转写"}]
    msgs = cb.build_messages(history=[], current_message="（语音）", media=media)
    user = msgs[-1]
    text_block = user["content"][0]["text"] if isinstance(user["content"], list) else user["content"]
    assert "[语音转写: 语音]" in text_block
    assert "你好我是转写" in text_block


def test_audio_without_transcript_falls_back_to_reference(tmp_path: Path):
    cb = ContextBuilder(tmp_path)
    media = [{"type": "voice", "url": str(tmp_path / "a.ogg"), "name": "语音", "transcribed_text": ""}]
    msgs = cb.build_messages(history=[], current_message="（语音）", media=media)
    user = msgs[-1]
    text_block = user["content"][0]["text"] if isinstance(user["content"], list) else user["content"]
    assert "[附件]" in text_block and "语音" in text_block
