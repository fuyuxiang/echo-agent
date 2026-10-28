from echo_agent.cli.tui.completion import (
    COMMANDS, completion_insert, filter_commands,
)


def test_catalog_has_all_commands():
    names = {c.name for c in COMMANDS}
    assert names == {
        "/approve", "/deny", "/approvals", "/clarify",
        "/help", "/clear", "/copy", "/save", "/theme", "/details",
        "/reconnect", "/status", "/quit",
    }


def test_server_scope_matches_gateway_intercepted_commands():
    # loop.py intercepts exactly these before the session lock: the approval
    # trio (_is_approval_command) plus /clarify (_is_clarify_command).
    server = {c.name for c in COMMANDS if c.scope == "server"}
    assert server == {"/approve", "/deny", "/approvals", "/clarify"}


def test_local_scope_commands():
    local = {c.name for c in COMMANDS if c.scope == "local"}
    assert local == {
        "/help", "/clear", "/copy", "/save", "/theme", "/details",
        "/reconnect", "/status", "/quit",
    }


def test_filter_by_prefix():
    got = {c.name for c in filter_commands("/ap")}
    assert got == {"/approve", "/approvals"}


def test_filter_empty_slash_returns_all():
    assert len(filter_commands("/")) == len(COMMANDS)


def test_filter_non_slash_returns_empty():
    assert filter_commands("hello") == []


def test_filter_case_insensitive():
    assert {c.name for c in filter_commands("/AP")} == {"/approve", "/approvals"}


def test_insert_arg_command_keeps_trailing_space():
    cmd = next(c for c in COMMANDS if c.name == "/approve")
    assert completion_insert(cmd) == "/approve "


def test_insert_noarg_command_no_trailing_space():
    cmd = next(c for c in COMMANDS if c.name == "/approvals")
    assert completion_insert(cmd) == "/approvals"


def test_filter_stops_once_name_finalized_by_space():
    # Once the user is typing arguments the name-completion list must be empty,
    # so the panel does not reopen over "/approve <id>".
    assert filter_commands("/approve ") == []
    assert filter_commands("/approve 5") == []
