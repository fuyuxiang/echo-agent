from echo_agent.cli.tui.completion import (
    COMMANDS, SlashCommand, completion_insert, filter_commands,
)


def test_catalog_has_exactly_five_commands():
    names = {c.name for c in COMMANDS}
    assert names == {"/approve", "/deny", "/approvals", "/clear", "/quit"}


def test_server_scope_only_the_three_real_ones():
    server = {c.name for c in COMMANDS if c.scope == "server"}
    assert server == {"/approve", "/deny", "/approvals"}


def test_local_scope_is_clear_and_quit():
    local = {c.name for c in COMMANDS if c.scope == "local"}
    assert local == {"/clear", "/quit"}


def test_filter_by_prefix():
    got = {c.name for c in filter_commands("/ap")}
    assert got == {"/approve", "/approvals"}


def test_filter_empty_slash_returns_all():
    assert len(filter_commands("/")) == 5


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
