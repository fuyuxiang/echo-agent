use std::ffi::OsStr;

use echo_agent_pager_pty_harness::{EnvOp, oauth_credential_ops};

#[test]
fn set_and_remove_operations_have_one_typed_surface() {
    let key = OsStr::new("FEATURE_FLAG");
    let value = OsStr::new("enabled");
    let operations: [EnvOp<'_>; 4] = [
        EnvOp::set("FEATURE_FLAG", "enabled"),
        EnvOp::remove("ECHO_AGENT_API_KEY"),
        EnvOp::set_os(key, value),
        EnvOp::remove_os(key),
    ];

    assert!(matches!(operations[0], EnvOp::Set(_, _)));
    assert!(matches!(operations[1], EnvOp::Remove(_)));
    assert!(matches!(operations[2], EnvOp::Set(_, _)));
    assert!(matches!(operations[3], EnvOp::Remove(_)));
}

#[test]
fn oauth_credential_operations_remove_the_api_key() {
    assert_eq!(
        oauth_credential_ops(),
        [EnvOp::remove("ECHO_AGENT_API_KEY")],
    );
}
