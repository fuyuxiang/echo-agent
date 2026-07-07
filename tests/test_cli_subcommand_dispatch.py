import sys
from unittest import mock

import echo_agent.__main__ as m


def test_cli_subcommand_routes_to_run_cli_attach():
    argv = ["echo-agent", "cli", "--port", "9001", "--user", "alice", "--token", "t"]
    with mock.patch.object(sys, "argv", argv), \
         mock.patch("echo_agent.cli.attach_client.run_cli_attach", return_value=0) as run, \
         mock.patch("echo_agent.cli.attach_client.resolve_defaults",
                    return_value=("127.0.0.1", 9000, "/ws", "")) as rd, \
         mock.patch("sys.exit") as ex:
        m._dispatch()
    rd.assert_called_once()
    run.assert_called_once()
    # --port 覆盖默认；host 恒为本机
    kwargs = run.call_args.kwargs
    assert kwargs["host"] == "127.0.0.1"
    assert kwargs["port"] == 9001
    assert kwargs["user_id"] == "alice"
    assert kwargs["token"] == "t"
    ex.assert_called_once_with(0)


def test_existing_commands_unaffected():
    # 裸 echo-agent 仍走 run 分支（不抛 SystemExit 到 cli 分支）
    parser = m._build_parser()
    ns = parser.parse_args(["cli", "--port", "9001"])
    assert ns.command == "cli"
    assert ns.port == 9001


def test_resolve_defaults_falls_back_on_load_failure(monkeypatch):
    from echo_agent.cli import attach_client

    def _boom(*a, **k):
        raise RuntimeError("no config")

    monkeypatch.setattr("echo_agent.config.loader.load_config", _boom)
    host, port, ws_path, token = attach_client.resolve_defaults(None, None)
    assert host == "127.0.0.1"
    assert port == 58123
    assert ws_path == "/ws"
    assert token == ""


def test_resolve_defaults_host_pinned_to_loopback(monkeypatch):
    from echo_agent.cli import attach_client

    class _Auth:
        api_tokens = ["secret-token"]

    class _Gw:
        port = 8123
        ws_path = "/socket"
        auth = _Auth()

    class _Cfg:
        gateway = _Gw()

    monkeypatch.setattr("echo_agent.config.loader.load_config", lambda **k: _Cfg())
    host, port, ws_path, token = attach_client.resolve_defaults("/tmp/x.yaml", None)
    assert host == "127.0.0.1"
    assert port == 8123
    assert ws_path == "/socket"
    assert token == "secret-token"
