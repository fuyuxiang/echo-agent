//! Compile and exercise the exact production native modules independently of
//! Tauri's platform webview/linker dependencies. This is not Runtime acceptance.
#![allow(dead_code)]
#[path = "../../src-tauri/src/automation/accessibility/mod.rs"]
mod accessibility;
#[path = "../../src-tauri/src/automation/computer.rs"]
mod computer;
#[path = "../../src-tauri/src/automation/desktop_coordinator.rs"]
mod desktop_coordinator;
#[path = "../../src-tauri/src/session_creation.rs"]
mod session_creation;
#[cfg(all(test, target_os = "macos"))]
mod host_probe {
    use super::accessibility;
    #[test]
    #[ignore = "Read-only graphical seat diagnostic; requires an unlocked real console"]
    fn actual_desktop_seat() {
        let capability = super::computer::ComputerController::capability();
        println!("desktopAvailable={:?}", capability.desktop_available);
        assert_eq!(capability.desktop_available, Some(true));
        assert!(!super::computer::ComputerController::displays()
            .unwrap()
            .is_empty());
    }
    #[test]
    #[ignore = "Requires real Accessibility consent for this executable; diagnostic only, not Runtime acceptance"]
    fn actual_native_permission_and_windows() {
        println!(
            "native capability: {}",
            serde_json::to_string(&accessibility::capability()).unwrap()
        );
        let backend = accessibility::macos_probe();
        assert!(backend.is_ok(), "{}", backend.err().unwrap_or_default());
    }
}

#[path = "../../src-tauri/src/automation/task.rs"]
mod task;

#[path = "../../vendor/echo-agent-build/crates/codegen/echo-agent-runtime/src/session/helpers/tool_input_parsing.rs"]
mod tool_input_parsing;

#[path = "../../src-tauri/src/automation/arguments.rs"]
mod arguments;
#[path = "../../src-tauri/src/automation/artifact.rs"]
mod artifact;

#[path = "../../src-tauri/src/automation/browser_bridge.rs"]
mod browser_bridge;
#[path = "../../src-tauri/src/automation/document_verification.rs"]
mod document_verification;
#[path = "../../src-tauri/src/knowledge_documents.rs"]
mod knowledge_documents;
#[path = "../../src-tauri/src/automation/policy.rs"]
mod policy;
#[path = "../../src-tauri/src/automation/targets.rs"]
mod targets;

#[path = "../../src-tauri/src/automation/emergency_stop.rs"]
mod emergency_stop;

pub fn run_helper() -> Result<(), String> {
    accessibility::run_helper()
}

/// Real self-spawned production helper, with no OS UI access or input.
pub async fn probe_helper_process() -> Result<(), String> {
    let controller = accessibility::Controller::new();
    let token = tokio_util::sync::CancellationToken::new();
    let gate = std::sync::Arc::new(tokio::sync::RwLock::new(()));
    let lease = accessibility::ExecutionLease {
        _desktop: Some(desktop_coordinator::acquire(&token).await?),
        _input: vec![],
        _session: gate.read_owned().await,
    };
    let response = controller
        .call("_helper_ping", serde_json::json!({}), &token, lease)
        .await?;
    if response["protocol"] != 1 || response["processId"] == std::process::id() {
        return Err("helper did not run in a separate authenticated process".into());
    }
    println!(
        "authenticated helper process: {} (host {})",
        response["processId"],
        std::process::id()
    );
    Ok(())
}

#[path = "../../src-tauri/src/automation/visual_target.rs"]
mod visual_target;

#[path = "../../src-tauri/src/automation/recovery.rs"]
mod recovery;
