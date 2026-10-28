//! Guards the async contract of the coding workspace commands: a long scan must
//! not block the tokio runtime, and the retired command names must be gone.

// The real current-thread scan responsiveness regression lives beside the
// production command in coding_workspace::tests. This integration target keeps
// the independently useful retired-command and direct-IO structural guards.

/// The verification engine must be the only place that spawns a command for a
/// coding task. The legacy `coding_run_command` / `coding_cancel_command` entry
/// points were retired together with the pre-workbench UI.
#[test]
fn command_execution_has_a_single_implementation() {
    let lib_source = include_str!("../src/lib.rs");
    assert!(
        lib_source.contains("coding::verification::coding_verification_run"),
        "验证引擎命令必须已注册"
    );
    assert!(
        !lib_source.contains("coding_workspace::coding_run_command"),
        "旧的命令执行入口应已随旧界面下线"
    );
    assert!(
        !lib_source.contains("coding_workspace::coding_cancel_command"),
        "旧的命令取消入口应已随旧界面下线"
    );

    let workspace_source = include_str!("../src/coding_workspace.rs");
    assert!(
        !workspace_source.contains("pub async fn coding_run_command"),
        "旧的命令执行实现不应再存在"
    );
    // The terminal is a separate concern and legitimately spawns its own shell;
    // what must not come back is a second *verification* execution path.
    assert!(
        !workspace_source.contains("CodingRunCommandRequest"),
        "旧的命令请求结构应已移除"
    );
}

/// Walk each `pub async fn` body and assert it does not reach for `std::fs`
/// directly; those calls belong inside `spawn_blocking` or a sync helper.
#[test]
fn async_commands_do_not_call_sync_fs_directly() {
    let source = include_str!("../src/coding_workspace.rs");
    let mut offenders = Vec::new();
    let mut in_async_command = false;
    let mut brace_depth = 0i32;
    let mut saw_spawn_blocking = false;

    for (index, line) in source.lines().enumerate() {
        if !in_async_command && line.starts_with("pub async fn") {
            in_async_command = true;
            brace_depth = 0;
            saw_spawn_blocking = false;
        }
        if !in_async_command {
            continue;
        }
        if line.contains("spawn_blocking") {
            saw_spawn_blocking = true;
        }
        if line.contains("std::fs::") && !saw_spawn_blocking {
            offenders.push(format!("{}: {}", index + 1, line.trim()));
        }
        brace_depth += line.matches('{').count() as i32;
        brace_depth -= line.matches('}').count() as i32;
        if brace_depth <= 0 && line.starts_with('}') {
            in_async_command = false;
        }
    }

    assert!(
        offenders.is_empty(),
        "以下 async 命令内仍有同步文件调用：\n{}",
        offenders.join("\n")
    );
}
