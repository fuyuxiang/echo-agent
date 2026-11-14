//! Host-enforced convergence. Read-only retries cannot erase a blocked write.
use serde_json::Value;
use std::collections::HashMap;

#[derive(Clone, Debug)]
pub struct Blocker {
    pub reason: &'static str,
    pub message: &'static str,
}
#[derive(Default)]
pub struct Recovery {
    failures: u8,
    counts: HashMap<&'static str, u8>,
    yielded: bool,
}
impl Recovery {
    pub fn observe(
        &mut self,
        tool: &str,
        value: &Value,
        error: Option<&str>,
        write: bool,
    ) -> Option<Blocker> {
        if self.yielded {
            return None;
        }
        let error =
            error
                .or_else(|| value["error"].as_str())
                .or_else(|| match value["status"].as_str() {
                    Some("timeout") => Some("condition_timeout: expected UI state did not appear"),
                    Some("visual_mismatch") => Some("visual_verification_failed: expected text not observed; inspect the result, never resend blindly"),
                    Some("no_focused_window") => {
                        Some("window_unavailable: reopen the requested app with foreground=true")
                    }
                    Some("ambiguous_app") => {
                        Some("target_ambiguous: ask the user to identify the intended application")
                    }
                    _ => None,
                });
        let Some(error) = error else {
            if write
                && !matches!(tool, "computer_open_app" | "computer_activate_window")
                && (value["executed"] == true
                    || value["keyDispatched"] == true
                    || value["typed"] == true
                    || value["clicked"] == true
                    || value["verified"] == true
                    || value["status"] == "dispatched_unverified")
            {
                self.failures = 0;
                self.counts.clear();
            } else if tool == "computer_context" && value["windowRef"].is_string() {
                self.counts.remove("stale_reference");
            }
            return None;
        };
        if error.starts_with("cancelled") || error.starts_with("task_inactive") {
            return None;
        }
        self.failures = self.failures.saturating_add(1);
        let (reason, limit, message) = if error.starts_with("input_required") {
            ("message_text_required", 1, "请告诉我需要发送的消息内容。")
        } else if error.starts_with("input_conflict") {
            ("input_conflict",2,"检测到目标应用正在被使用，已停止重复操作并保留进度。可以直接发送消息继续或调整要求。")
        } else if error.starts_with("desktop_busy") {
            (
                "desktop_busy",
                2,
                "这一步需要使用前台，目前桌面正在被使用。已保留进度；方便时直接发送消息继续。",
            )
        } else if error.starts_with("foreground_required") {
            ("foreground_required",3,"目标应用尚未进入前台，这一步暂时无法执行。已保留目标和进度，可以直接发送消息继续。")
        } else if error.starts_with("capture_unavailable") {
            ("capture_unavailable",2,"目标窗口没有返回可用画面，已停止重复截图并保留进度。请确认目标窗口可见且允许屏幕捕获后，通过普通消息继续。")
        } else if error.starts_with("native_timeout") {
            (
                "native_unresponsive",
                2,
                "目标应用连续未响应，已结束重复尝试。请确认应用可用后，通过普通消息继续。",
            )
        } else if error.starts_with("visual_verification_failed") {
            ("verification_required",3,"尚未在目标窗口确认预期文字，已保留操作结果并停止重复尝试。请先核对现有结果，避免重复发送。")
        } else if error.starts_with("condition_timeout") || error.starts_with("window_unavailable")
        {
            ("target_unavailable",3,"连续等待仍未看到预期窗口或结果，已保留进度并停止重复操作。请确认目标应用可用后直接发送消息继续。")
        } else if error.starts_with("target_ambiguous") {
            (
                "target_ambiguous",
                3,
                "发现多个可能的目标应用，请说明需要操作哪一个。",
            )
        } else if error.starts_with("permission_denied")
            || error.starts_with("input_monitor_unavailable")
        {
            ("permission_required",1,"系统权限或输入监测不可用，操作尚未执行。请检查应用的系统授权，随后直接发送消息继续。")
        } else if error.starts_with("stale_reference")
            || error.starts_with("target_refresh_required")
        {
            (
                "stale_reference",
                3,
                "目标引用连续失效，已停止重复操作。需要重新读取目标应用，可以直接发送消息继续。",
            )
        } else if error.starts_with("stale_frame")
            || error.starts_with("target_changed")
            || error.starts_with("focus_changed")
        {
            ("visual_state_changed",3,"目标界面连续变化或截图已过期，已保留进度并停止重复操作。方便操作时可以直接发送消息继续。")
        } else if error.starts_with("protected") {
            (
                "manual_input_required",
                1,
                "当前操作涉及受保护的输入区域，需要你亲自完成该步骤，之后可直接发送消息继续。",
            )
        } else if tool == "automation_complete" {
            ("verification_required",3,"尚未取得有效的结果核验凭证，已保留操作结果并停止重复提交。可以直接发送消息继续核验。")
        } else {
            ("execution_blocked",8,"多次尝试仍未取得进展，已保留当前结果并结束重复操作。请核对目标或通过普通消息调整要求。")
        };
        let count = self.counts.entry(reason).or_default();
        *count = count.saturating_add(1);
        if *count >= limit || self.failures >= 8 {
            self.yielded = true;
            Some(Blocker { reason, message })
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn unavailable_capture_stops_instead_of_issuing_inputs_against_an_empty_image() {
        let mut r = Recovery::default();
        assert!(r
            .observe(
                "computer_screenshot",
                &json!({}),
                Some("capture_unavailable: no pixels"),
                false
            )
            .is_none());
        assert_eq!(
            r.observe(
                "computer_screenshot",
                &json!({}),
                Some("capture_unavailable: no pixels"),
                false
            )
            .unwrap()
            .reason,
            "capture_unavailable"
        );
    }
    #[test]
    fn failed_visual_proofs_converge_without_a_resend_or_read_loop() {
        let mut r = Recovery::default();
        for i in 0..3 {
            let blocked = r.observe(
                "computer_verify_visual",
                &json!({"status":"visual_mismatch","verified":false}),
                None,
                false,
            );
            assert_eq!(blocked.is_some(), i == 2);
            r.observe(
                "computer_screenshot",
                &json!({"frameId":"new"}),
                None,
                false,
            );
        }
    }
    #[test]
    fn opening_the_app_cannot_erase_repeated_missing_window_or_result_timeouts() {
        for status in ["timeout", "no_focused_window"] {
            let mut r = Recovery::default();
            for i in 0..3 {
                let blocked =
                    r.observe("computer_wait_for", &json!({"status":status}), None, false);
                assert_eq!(blocked.is_some(), i == 2, "{status}");
                r.observe(
                    "computer_open_app",
                    &json!({"executed":true,"verified":true}),
                    None,
                    true,
                );
            }
        }
    }
    #[test]
    fn invalid_completion_proofs_have_a_bounded_correction_budget() {
        let mut r = Recovery::default();
        for i in 0..3 {
            let blocked = r.observe(
                "automation_complete",
                &json!({}),
                Some("evidence_required: stale proof"),
                false,
            );
            assert_eq!(blocked.is_some(), i == 2);
            if let Some(blocked) = blocked {
                assert_eq!(blocked.reason, "verification_required");
            }
        }
    }
    #[test]
    fn readonly_discovery_does_not_hide_a_repeated_input_conflict() {
        let mut r = Recovery::default();
        assert!(r
            .observe(
                "computer_activate_window",
                &json!({}),
                Some("input_conflict: target"),
                true
            )
            .is_none());
        for _ in 0..5 {
            assert!(r
                .observe("computer_context", &json!({"windowRef":"w"}), None, false)
                .is_none());
        }
        assert_eq!(
            r.observe(
                "computer_key",
                &json!({}),
                Some("input_conflict: target"),
                true
            )
            .unwrap()
            .reason,
            "input_conflict"
        );
        assert!(r
            .observe(
                "computer_key",
                &json!({}),
                Some("input_conflict: target"),
                true
            )
            .is_none());
    }
    #[test]
    fn a_successful_real_action_resets_recovery_but_unverified_failures_do_not() {
        let mut r = Recovery::default();
        r.observe(
            "computer_key_window",
            &json!({"error":"visual_fallback_required: no focus","executed":null}),
            None,
            true,
        );
        r.observe("computer_click", &json!({"clicked":true}), None, true);
        assert!(r
            .observe(
                "computer_key",
                &json!({}),
                Some("desktop_busy: another app"),
                true
            )
            .is_none());
        assert_eq!(
            r.observe(
                "computer_key",
                &json!({}),
                Some("desktop_busy: another app"),
                true
            )
            .unwrap()
            .reason,
            "desktop_busy"
        );
    }
    #[test]
    fn helper_failures_stop_even_when_observation_between_them_succeeds() {
        let mut r = Recovery::default();
        r.observe(
            "computer_context",
            &json!({}),
            Some("native_timeout: terminated"),
            false,
        );
        r.observe(
            "computer_context",
            &json!({"windowRef":"fresh"}),
            None,
            false,
        );
        assert_eq!(
            r.observe(
                "computer_capture_window",
                &json!({}),
                Some("native_timeout: terminated"),
                false
            )
            .unwrap()
            .reason,
            "native_unresponsive"
        );
    }
    #[test]
    fn a_new_context_recovers_stale_refs_and_a_new_user_run_has_fresh_recovery() {
        let mut r = Recovery::default();
        r.observe(
            "computer_find",
            &json!({}),
            Some("stale_reference: old"),
            false,
        );
        r.observe("computer_context", &json!({"windowRef":"new"}), None, false);
        assert!(r
            .observe(
                "computer_find",
                &json!({}),
                Some("stale_reference: old"),
                false
            )
            .is_none());
        let mut next = Recovery::default();
        assert!(next
            .observe(
                "computer_key",
                &json!({}),
                Some("input_conflict: old"),
                true
            )
            .is_none());
    }
}
