"""A cold-cache model download must not keep a stopped server process alive."""

import subprocess
import sys
import textwrap


def test_process_exits_after_close_while_embedding_model_is_still_loading():
    script = textwrap.dedent("""
        import asyncio
        import threading
        from echo_agent.memory.local_embed import LocalEmbedder

        async def main():
            embedder = LocalEmbedder(load_timeout_seconds=0.01)
            embedder._load_model_sync = lambda: threading.Event().wait(30)
            await embedder._ensure_model()
            embedder.close()
            print("stopped", flush=True)

        asyncio.run(main())
    """)
    result = subprocess.run([sys.executable, "-c", script], capture_output=True, text=True, timeout=3)
    assert result.returncode == 0, result.stderr
    assert result.stdout.strip() == "stopped"
