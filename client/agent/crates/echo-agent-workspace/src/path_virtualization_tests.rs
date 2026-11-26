use super::*;
use echo_agent_tool_protocol::ToolId;
use echo_agent_tool_runtime::ContentBlock;
use serde_json::json;

fn mapping() -> PathVirtualization {
    PathVirtualization::try_from_session_root("/workspace/conv-abc").expect("valid session root")
}

#[test]
fn session_root_must_be_safe_absolute() {
    assert!(PathVirtualization::try_from_session_root("/workspace/conv-abc").is_some());
    assert!(
        PathVirtualization::try_from_session_root("").is_none(),
        "empty must not enable virtualization"
    );
    assert!(PathVirtualization::try_from_session_root("workspace/conv").is_none());
    assert!(PathVirtualization::try_from_session_root("/workspace/../etc").is_none());
    assert!(PathVirtualization::try_from_session_root("/workspace/.").is_none());
    assert!(PathVirtualization::try_from_session_root("/").is_none());
    let trailing = PathVirtualization::try_from_session_root("/workspace/conv-abc/")
        .expect("trailing slash is normalized, not rejected");
    assert_eq!(trailing.real_root(), "/workspace/conv-abc");
    assert_eq!(
        trailing.to_model_visible("/workspace/conv-abc/foo"),
        "/workspace/foo",
        "normalized root must still rewrite outbound"
    );
    let doubled = PathVirtualization::try_from_session_root("/workspace//conv-abc")
        .expect("empty segments are dropped");
    assert_eq!(doubled.real_root(), "/workspace/conv-abc");
}

#[test]
fn outbound_rewrites_real_root_prefix() {
    let v = mapping();
    assert_eq!(v.to_model_visible("/workspace/conv-abc"), "/workspace");
    assert_eq!(
        v.to_model_visible("/workspace/conv-abc/foo.txt"),
        "/workspace/foo.txt"
    );
    assert_eq!(
        v.to_model_visible("/workspace/conv-abc/nested/bar"),
        "/workspace/nested/bar"
    );
}

#[test]
fn inbound_workspace_maps_to_session_root() {
    let v = mapping();
    assert_eq!(v.to_guest("/workspace"), "/workspace/conv-abc");
    assert_eq!(v.to_guest("/workspace/"), "/workspace/conv-abc/");
    assert_eq!(
        v.to_guest("/workspace/foo.txt"),
        "/workspace/conv-abc/foo.txt"
    );
}

#[test]
fn inbound_artifacts_alias_maps_to_session_root() {
    let v = mapping();
    assert_eq!(v.to_guest("/workspace/artifacts"), "/workspace/conv-abc");
    assert_eq!(
        v.to_guest("/workspace/artifacts/foo.txt"),
        "/workspace/conv-abc/foo.txt"
    );
    assert_eq!(
        v.to_guest("/workspace/artifacts/nested/bar"),
        "/workspace/conv-abc/nested/bar"
    );
}

#[test]
fn paths_outside_session_root_are_unchanged() {
    let v = mapping();
    for path in [
        "/tmp/secret",
        "/home/user/file",
        "/workspace/other-conv/file",
        "/workspace-extra/x",
        "relative/path",
        "",
    ] {
        assert_eq!(
            v.to_model_visible(path),
            path,
            "outbound must leave {path:?} unchanged"
        );
    }
    for path in [
        "/tmp/secret",
        "/home/user/file",
        "/workspace-extra/x",
        "relative/path",
        "",
    ] {
        assert_eq!(
            v.to_guest(path),
            path,
            "inbound must leave {path:?} unchanged"
        );
    }
}

#[test]
fn already_guest_inbound_is_not_double_prefixed() {
    let v = mapping();
    assert_eq!(
        v.to_guest("/workspace/conv-abc/foo"),
        "/workspace/conv-abc/foo"
    );
}

#[test]
fn inbound_does_not_create_dotdot_walk_out_of_real_root() {
    let v = mapping();
    assert_eq!(
        v.to_guest("/workspace/../other-conv/secret"),
        "/workspace/conv-abc",
        "visible-root .. walk-out must clip to real_root"
    );
    assert_eq!(
        v.to_guest("/workspace/artifacts/../other-conv/secret"),
        "/workspace/conv-abc",
        "artifacts .. walk-out must clip to real_root"
    );
    assert_eq!(
        v.to_guest("/workspace/conv-abc/../other"),
        "/workspace/conv-abc",
        "already-guest .. that leaves real_root must clip to real_root"
    );
    assert_eq!(
        v.to_guest("/workspace/foo/../bar"),
        "/workspace/conv-abc/bar",
        "in-root .. after the prefix map is resolved"
    );
    assert_eq!(
        v.to_guest("/tmp/../etc/passwd"),
        "/tmp/../etc/passwd",
        "absolute /tmp escapes are not blocked or rewritten"
    );
    assert_eq!(
        v.to_guest("/home/user/../other"),
        "/home/user/../other",
        "absolute /home escapes are not blocked or rewritten"
    );
}

#[test]
fn inbound_other_conv_under_visible_root_maps_into_session() {
    let v = mapping();
    assert_eq!(
        v.to_guest("/workspace/other-conv/file"),
        "/workspace/conv-abc/other-conv/file"
    );
}

#[test]
fn prefix_is_not_a_sibling_path() {
    let v = mapping();
    assert_eq!(
        v.to_model_visible("/workspace/conv-abcdef/x"),
        "/workspace/conv-abcdef/x"
    );
    assert_eq!(v.to_guest("/workspace2/foo"), "/workspace2/foo");
    assert_eq!(
        v.rewrite_text_outbound("/workspace/conv-abcdef/x"),
        "/workspace/conv-abcdef/x",
        "text scanner must not treat a sibling prefix as the session root"
    );
    assert_eq!(
        v.rewrite_text_inbound("/workspace2/foo"),
        "/workspace2/foo",
        "text scanner must not rewrite /workspace2"
    );
}

#[test]
fn outbound_text_rewrites_embedded_guest_paths() {
    let v = mapping();
    assert_eq!(
        v.rewrite_text_outbound("cwd: /workspace/conv-abc read /workspace/conv-abc/a.rs"),
        "cwd: /workspace read /workspace/a.rs"
    );
    assert_eq!(
        v.rewrite_text_outbound("no paths here"),
        "no paths here",
        "unchanged text must not allocate a new string"
    );
}

#[test]
fn inbound_text_rewrites_workspace_and_artifacts_without_double_apply() {
    let v = mapping();
    assert_eq!(
        v.rewrite_text_inbound("open /workspace/foo and /workspace/artifacts/bar"),
        "open /workspace/conv-abc/foo and /workspace/conv-abc/bar"
    );
    assert_eq!(
        v.rewrite_text_inbound("already /workspace/conv-abc/foo"),
        "already /workspace/conv-abc/foo"
    );
}

#[test]
fn inbound_text_clips_workspace_walk_outs_and_leaves_tmp() {
    let v = mapping();
    assert_eq!(
        v.rewrite_text_inbound("open /workspace/artifacts/../other-conv/secret"),
        "open /workspace/conv-abc",
        "artifacts .. walk-out in prose must clip to real_root"
    );
    assert_eq!(
        v.rewrite_text_inbound("read /workspace/conv-abc/../other then /tmp/../etc/passwd"),
        "read /workspace/conv-abc then /tmp/../etc/passwd",
        "already-guest walk-out clips; /tmp identity stays"
    );
    assert_eq!(
        v.rewrite_text_inbound("cd /workspace/../other-conv/secret and /home/user/../other"),
        "cd /workspace/conv-abc and /home/user/../other",
        "visible-root walk-out clips; /home identity stays"
    );
}

#[test]
fn json_outbound_rewrites_strings_only() {
    let v = mapping();
    let input = json!({
        "path": "/workspace/conv-abc/out.txt",
        "n": 3,
        "ok": true,
        "nested": ["/workspace/conv-abc", "/tmp/x"],
    });
    assert_eq!(
        v.rewrite_json_outbound(input),
        json!({
            "path": "/workspace/out.txt",
            "n": 3,
            "ok": true,
            "nested": ["/workspace", "/tmp/x"],
        })
    );
}

#[test]
fn json_inbound_rewrites_workspace_and_artifacts() {
    let v = mapping();
    let input = json!({
        "target_file": "/workspace/foo.rs",
        "legacy": "/workspace/artifacts/bar.rs",
        "outside": "/tmp/x",
    });
    assert_eq!(
        v.rewrite_json_inbound(input),
        json!({
            "target_file": "/workspace/conv-abc/foo.rs",
            "legacy": "/workspace/conv-abc/bar.rs",
            "outside": "/tmp/x",
        })
    );
}

#[test]
fn json_inbound_leaves_write_edit_bodies_unchanged() {
    let v = mapping();
    let input = json!({
        "target_file": "/workspace/Dockerfile",
        "contents": "WORKDIR /workspace\nCOPY /workspace/app /app\n",
        "old_string": "cd /workspace && make",
        "new_string": "cd /workspace && make test",
        "patch": "--- a/x\n+++ b/x\n@@ -1 +1 @@\n-# /workspace\n+# /workspace\n",
        "command": "sed -n 1p /workspace/Dockerfile",
        "pattern": "/workspace",
        "notify_on_output": { "pattern": "/workspace", "reason": "watch root" },
    });
    assert_eq!(
        v.rewrite_json_inbound(input),
        json!({
            "target_file": "/workspace/conv-abc/Dockerfile",
            "contents": "WORKDIR /workspace\nCOPY /workspace/app /app\n",
            "old_string": "cd /workspace && make",
            "new_string": "cd /workspace && make test",
            "patch": "--- a/x\n+++ b/x\n@@ -1 +1 @@\n-# /workspace\n+# /workspace\n",
            "command": "sed -n 1p /workspace/conv-abc/Dockerfile",
            "pattern": "/workspace",
            "notify_on_output": { "pattern": "/workspace", "reason": "watch root" },
        })
    );
}

#[test]
fn typed_output_and_error_rewrite_model_facing_fields() {
    let v = mapping();
    let output = TypedToolOutput {
        tool_id: ToolId::new("read_file").unwrap(),
        value: json!({"path": "/workspace/conv-abc/a.txt", "prompt_text": "read /workspace/conv-abc/a.txt"}),
        model_output: vec![ContentBlock::Text {
            text: "read /workspace/conv-abc/a.txt".into(),
        }],
        chat_completion_output: None,
    };
    let rewritten = v.rewrite_typed_output(output);
    assert_eq!(rewritten.value["path"], "/workspace/a.txt");
    match &rewritten.model_output[0] {
        ContentBlock::Text { text } => assert_eq!(text, "read /workspace/a.txt"),
        other => panic!("expected text block, got {other:?}"),
    }

    let err = ToolError::new(
        echo_agent_tool_runtime::ToolErrorKind::Execution,
        "missing /workspace/conv-abc/gone.txt",
    )
    .with_details(json!({"path": "/workspace/conv-abc/gone.txt"}));
    let err = v.rewrite_error(err);
    assert_eq!(err.detail, "missing /workspace/gone.txt");
    assert_eq!(err.details.unwrap()["path"], "/workspace/gone.txt");

    let cco = echo_agent_tool_runtime::ToolChatCompletionResponse {
        result: Some(echo_agent_tool_runtime::ToolChatCompletion {
            message: "cwd /workspace/conv-abc".into(),
            code_execution_result: Some(echo_agent_tool_runtime::ToolCodeExecutionResult {
                stdout: "/workspace/conv-abc/out.txt\n".into(),
                stderr: "warn /workspace/conv-abc/e.txt".into(),
                ..Default::default()
            }),
            card_attachment: Some("{\"path\":\"/workspace/conv-abc/card.txt\"}".into()),
            extra: serde_json::Map::from_iter([(
                "output_file".into(),
                json!("/workspace/conv-abc/trunc.txt"),
            )]),
            ..Default::default()
        }),
        stream_error: Some(echo_agent_tool_runtime::ToolStreamError {
            message: "failed /workspace/conv-abc/x".into(),
            typed_error: Some(json!({"path": "/workspace/conv-abc/typed.txt"})),
        }),
    };
    let rewritten = v.rewrite_typed_output(TypedToolOutput {
        tool_id: ToolId::new("bash").unwrap(),
        value: json!({}),
        model_output: vec![],
        chat_completion_output: Some(cco),
    });
    let cco = rewritten.chat_completion_output.expect("cco");
    let result = cco.result.expect("result");
    assert_eq!(result.message, "cwd /workspace");
    let cer = result.code_execution_result.expect("cer");
    assert_eq!(cer.stdout, "/workspace/out.txt\n");
    assert_eq!(cer.stderr, "warn /workspace/e.txt");
    assert_eq!(
        result.card_attachment.as_deref(),
        Some("{\"path\":\"/workspace/card.txt\"}")
    );
    assert_eq!(
        result.extra.get("output_file"),
        Some(&json!("/workspace/trunc.txt"))
    );
    let stream_error = cco.stream_error.expect("stream_error");
    assert_eq!(stream_error.message, "failed /workspace/x");
    assert_eq!(
        stream_error.typed_error,
        Some(json!({"path": "/workspace/typed.txt"}))
    );
}

#[test]
fn bind_mount_hook_noop_never_fails() {
    let hook = BindMountHook::noop();
    let root = Path::new("/workspace/conv-abc");
    hook.on_bind(BindLifecycleCtx {
        session_id: "conv-abc",
        real_root: root,
    })
    .expect("noop bind must succeed");
    hook.on_unbind(BindLifecycleCtx {
        session_id: "conv-abc",
        real_root: root,
    });
}

#[test]
fn probe_hit_skips_mount() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let mounts = Arc::new(AtomicUsize::new(0));
    let mounts_c = mounts.clone();
    let hook = BindMountHook::probe_then_mount(
        |_| true,
        move |_| {
            mounts_c.fetch_add(1, Ordering::SeqCst);
            Ok(())
        },
    );
    hook.on_bind(BindLifecycleCtx {
        session_id: "s",
        real_root: Path::new("/workspace/s"),
    })
    .unwrap();
    assert_eq!(
        mounts.load(Ordering::SeqCst),
        0,
        "live mount must skip mount"
    );
}

#[test]
fn probe_miss_runs_mount() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let mounts = Arc::new(AtomicUsize::new(0));
    let mounts_c = mounts.clone();
    let hook = BindMountHook::probe_then_mount(
        |_| false,
        move |root| {
            assert_eq!(root, Path::new("/workspace/s"));
            mounts_c.fetch_add(1, Ordering::SeqCst);
            Ok(())
        },
    );
    hook.on_bind(BindLifecycleCtx {
        session_id: "s",
        real_root: Path::new("/workspace/s"),
    })
    .unwrap();
    assert_eq!(mounts.load(Ordering::SeqCst), 1);
}

#[test]
fn unbind_does_not_unmount() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let mounts = Arc::new(AtomicUsize::new(0));
    let unbinds = Arc::new(AtomicUsize::new(0));
    let mounts_c = mounts.clone();
    let unbinds_c = unbinds.clone();
    let hook = BindMountHook::probe_then_mount(
        |_| false,
        move |_| {
            mounts_c.fetch_add(1, Ordering::SeqCst);
            Ok(())
        },
    )
    .with_on_unbind(move |_, _| {
        unbinds_c.fetch_add(1, Ordering::SeqCst);
    });
    let ctx = BindLifecycleCtx {
        session_id: "s",
        real_root: Path::new("/workspace/s"),
    };
    hook.on_bind(ctx).unwrap();
    hook.on_unbind(ctx);
    hook.on_unbind(ctx);
    assert_eq!(mounts.load(Ordering::SeqCst), 1, "unbind must not remount");
    assert_eq!(unbinds.load(Ordering::SeqCst), 2);
}

#[test]
fn mount_error_is_returned() {
    let hook =
        BindMountHook::probe_then_mount(|_| false, |_| Err(BindMountError("fuse down".into())));
    let err = hook
        .on_bind(BindLifecycleCtx {
            session_id: "s",
            real_root: Path::new("/workspace/s"),
        })
        .expect_err("mount failure must surface");
    assert!(err.0.contains("fuse down"));
}
