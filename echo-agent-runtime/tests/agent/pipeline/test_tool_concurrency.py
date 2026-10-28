from echo_agent.agent.pipeline.tool_concurrency import (
    ToolPlan, extract_paths, paths_overlap, partition_concurrent,
)


def _plan(index, name, read_only, paths=None, approved=True):
    return ToolPlan(index=index, name=name, read_only=read_only,
                    paths=paths or [], approved=approved)


def test_extract_paths_reads_common_keys():
    assert extract_paths({"path": "a/b.txt"}) == ["a/b.txt"]
    assert extract_paths({"file": "x.py"}) == ["x.py"]
    assert extract_paths({"query": "hello"}) == []


def test_paths_overlap_parent_child():
    assert paths_overlap(["src/"], ["src/app.py"]) is True
    assert paths_overlap(["src/a.py"], ["src/a.py"]) is True
    assert paths_overlap(["src/a.py"], ["src/b.py"]) is False
    assert paths_overlap([], ["src/a.py"]) is False


def test_partition_read_only_go_concurrent():
    plans = [_plan(0, "read_file", True, ["a.txt"]),
             _plan(1, "search_files", True, [])]
    conc, serial = partition_concurrent(plans)
    assert [p.index for p in conc] == [0, 1]
    assert serial == []


def test_partition_side_effect_stays_serial():
    plans = [_plan(0, "read_file", True, ["a.txt"]),
             _plan(1, "write_file", False, ["b.txt"])]
    conc, serial = partition_concurrent(plans)
    assert [p.index for p in conc] == [0]
    assert [p.index for p in serial] == [1]


def test_partition_path_overlap_demotes_reader_to_serial():
    # read_file 与同批 write_file 路径重叠 → reader 退回串行
    plans = [_plan(0, "read_file", True, ["src/app.py"]),
             _plan(1, "write_file", False, ["src/app.py"])]
    conc, serial = partition_concurrent(plans)
    assert conc == []
    assert [p.index for p in serial] == [0, 1]


def test_partition_unapproved_stays_serial():
    plans = [_plan(0, "read_file", True, ["a.txt"], approved=False)]
    conc, serial = partition_concurrent(plans)
    assert conc == []
    assert [p.index for p in serial] == [0]
