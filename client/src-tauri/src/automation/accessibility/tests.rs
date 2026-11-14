use super::*;
use std::cell::RefCell;

#[derive(Default)]
struct Fake {
    bundles: RefCell<HashMap<u32, std::path::PathBuf>>,
    nodes: RefCell<HashMap<u32, Node>>,
    alive: RefCell<bool>,
    calls: RefCell<Vec<String>>,
    failure: RefCell<Option<String>>,
    inspect_failures: RefCell<HashMap<u32, String>>,
    children_failures: RefCell<HashMap<u32, String>>,
    child_overrides: RefCell<HashMap<u32, Vec<u32>>>,
    listing_issues: RefCell<Vec<(String, String)>>,
    focus_ok: RefCell<bool>,
    cancel_on_activate: RefCell<Option<CancellationToken>>,
    protected_focus: RefCell<bool>,
    session_error: RefCell<Option<String>>,
    cancel_after_text: RefCell<Option<CancellationToken>>,
    protect_after_text: RefCell<bool>,
    deferred_text: RefCell<Option<String>>,
    document_location: RefCell<Option<String>>,
    modal_root: RefCell<Option<u32>>,
    transient_app_reads: RefCell<u32>,
    file_panel_kind: RefCell<Option<&'static str>>,
    file_goto: RefCell<bool>,
    text_focus: RefCell<Option<u32>>,
    hit: RefCell<Option<u32>>,
    background_input: RefCell<bool>,
    missing_focus: RefCell<bool>,
    foreground: RefCell<Option<u32>>,
}
impl Native for Fake {
    type Element = u32;
    fn application_bundle_path(&self, e: &u32) -> Option<std::path::PathBuf> {
        self.bundles.borrow().get(e).cloned()
    }
    fn foreground_window(&self) -> Result<u32, String> {
        Ok(self.foreground.borrow().unwrap_or(2))
    }
    fn keyboard_focus(&self, _: &u32) -> Result<Option<u32>, String> {
        Ok((!*self.missing_focus.borrow()).then_some(3))
    }
    fn focused_target(&self) -> Result<(u32, u32), String> {
        Ok((2, 3))
    }
    fn hit_target(&self, _: &u32, _: f64, _: f64) -> Result<u32, String> {
        Ok(self.hit.borrow().unwrap_or(3))
    }
    fn same_element(&self, a: &u32, b: &u32) -> bool {
        a == b
    }
    fn application_element(&self, _: &u32) -> Result<Option<u32>, String> {
        Ok(Some(1))
    }
    fn new() -> Result<Self, String> {
        Ok(Self::default())
    }
    fn check_session(&self) -> Result<(), String> {
        self.session_error.borrow().clone().map_or(Ok(()), Err)
    }
    fn apps(&self) -> Result<Listing<u32>, String> {
        let mut ids: Vec<_> = self
            .nodes
            .borrow()
            .iter()
            .filter(|(_, n)| n.role == "application")
            .map(|(id, _)| *id)
            .collect();
        ids.sort();
        Ok(Listing {
            items: ids
                .into_iter()
                .map(|id| self.inspect(&id).map(|node| (id, node)))
                .collect::<Result<_, _>>()?,
            issues: self.listing_issues.borrow().clone(),
        })
    }
    fn windows(&self, _: Option<&u32>) -> Result<Listing<u32>, String> {
        Ok(Listing {
            items: vec![(2, self.inspect(&2)?)],
            issues: self.listing_issues.borrow().clone(),
        })
    }
    fn document_url(&self, _e: &u32) -> Result<Option<String>, String> {
        Ok(self.document_location.borrow().clone())
    }
    fn observation_root(&self, window: &u32) -> Result<u32, String> {
        Ok(self.modal_root.borrow().unwrap_or(*window))
    }
    fn file_dialog(&self, _: &u32) -> Result<Option<file_dialog::Dialog<u32>>, String> {
        Ok(self
            .file_panel_kind
            .borrow()
            .map(|kind| file_dialog::Dialog {
                kind: if *self.file_goto.borrow() {
                    "goto"
                } else {
                    kind
                },
                root: if *self.file_goto.borrow() { 21 } else { 20 },
                path: Some(22),
                filename: Some(23),
            }))
    }
    fn inspect(&self, e: &u32) -> Result<Node, String> {
        self.validate(e, None)?;
        if *e == 1 && *self.transient_app_reads.borrow() > 0 {
            *self.transient_app_reads.borrow_mut() -= 1;
            return Err("native_timeout: transitioning UI".into());
        }
        if let Some(err) = self.failure.borrow().clone() {
            return Err(err);
        }
        if let Some(error) = self.inspect_failures.borrow().get(e) {
            return Err(error.clone());
        }
        let mut node = self
            .nodes
            .borrow()
            .get(e)
            .cloned()
            .ok_or("element_not_found: removed")?;
        if let Some(value) = node.value.as_mut() {
            if value.chars().count() > 4096 {
                *value = value.chars().take(4096).collect();
                node.text_truncated = true;
            }
        }
        Ok(node)
    }
    fn children(&self, e: &u32, limit: usize) -> Result<(Vec<u32>, bool), String> {
        if let Some(error) = self.children_failures.borrow().get(e) {
            return Err(error.clone());
        }
        let children = self
            .child_overrides
            .borrow()
            .get(e)
            .cloned()
            .unwrap_or_else(|| if *e == 2 { vec![3, 4, 5] } else { vec![] });
        let more = children.len() > limit;
        Ok((children.into_iter().take(limit).collect(), more))
    }
    fn validate(&self, e: &u32, w: Option<&u32>) -> Result<(), String> {
        if !*self.alive.borrow() {
            return Err("application_exited".into());
        }
        if w.is_some_and(|w| *w != 2) {
            return Err("stale_reference: window changed".into());
        }
        if !self.nodes.borrow().contains_key(e) {
            return Err("element_not_found".into());
        }
        Ok(())
    }
    fn prepare_input_window(&self, e: &u32) -> Result<bool, String> {
        if *self.background_input.borrow() {
            self.calls
                .borrow_mut()
                .push(format!("application-focus:{e}"));
            Ok(*self.focus_ok.borrow())
        } else {
            self.activate(e)
        }
    }
    fn process_id(&self, _: &u32) -> Option<u32> {
        Some(10001)
    }
    fn activate(&self, e: &u32) -> Result<bool, String> {
        self.calls.borrow_mut().push(format!("activate:{e}"));
        if let Some(token) = self.cancel_on_activate.borrow().as_ref() {
            token.cancel();
        }
        Ok(*self.focus_ok.borrow())
    }
    fn key(&self, _: &u32, key: &str, modifiers: &[String]) -> Result<(), String> {
        if *self.protected_focus.borrow() {
            return Err("protected: keyboard focus requires user takeover".into());
        }
        self.calls
            .borrow_mut()
            .push(format!("key:{key}:{modifiers:?}"));
        if key == "backspace" {
            self.nodes
                .borrow_mut()
                .get_mut(&self.text_focus.borrow().unwrap_or(3))
                .unwrap()
                .value = Some(String::new());
        }
        if self.file_panel_kind.borrow().is_some() {
            if key == "g" {
                *self.file_goto.borrow_mut() = true;
            }
            if key == "return" {
                *self.file_goto.borrow_mut() = false;
            }
        }
        Ok(())
    }
    fn verify_text_focus(&self, e: &u32, window: &u32) -> Result<(), String> {
        self.validate(e, Some(window))?;
        if *self.protected_focus.borrow() {
            return Err("protected: unexpected password focus".into());
        }
        if !*self.focus_ok.borrow() {
            return Err("focus_changed: different input".into());
        }
        Ok(())
    }
    fn emit_text(&self, e: &u32, text: &str) -> Result<(), String> {
        self.calls.borrow_mut().push("unicode".into());
        if self.deferred_text.borrow().is_none() {
            self.nodes
                .borrow_mut()
                .get_mut(e)
                .unwrap()
                .value
                .as_mut()
                .unwrap()
                .push_str(text);
        }
        if let Some(token) = self.cancel_after_text.borrow().as_ref() {
            token.cancel();
        }
        if *self.protect_after_text.borrow() {
            *self.protected_focus.borrow_mut() = true;
        }
        Ok(())
    }
    fn invoke(&self, e: &u32, action: &str) -> Result<(), String> {
        self.calls.borrow_mut().push(action.into());
        self.nodes.borrow_mut().get_mut(e).unwrap().focused = true;
        if action == "focus" {
            *self.text_focus.borrow_mut() = Some(*e);
        }
        Ok(())
    }
    fn click_control(
        &self,
        _element: &u32,
        window: &u32,
        count: u32,
        token: &CancellationToken,
    ) -> Result<(), String> {
        if !self.activate(window)? || token.is_cancelled() {
            return Err("cancelled_or_unfocused".into());
        }
        self.calls
            .borrow_mut()
            .push(format!("control_click:{count}"));
        Ok(())
    }
    fn set_value(&self, e: &u32, value: &str) -> Result<(), String> {
        self.calls.borrow_mut().push("set".into());
        self.nodes.borrow_mut().get_mut(e).unwrap().value = Some(value.into());
        Ok(())
    }
    fn read(&self, e: &u32, offset: usize, limit: usize) -> Result<(String, bool), String> {
        self.validate(e, None)?;
        if let Some(value) = self.deferred_text.borrow_mut().take() {
            self.nodes.borrow_mut().get_mut(e).unwrap().value = Some(value);
        }
        let value = self
            .nodes
            .borrow()
            .get(e)
            .unwrap()
            .value
            .clone()
            .unwrap_or_default();
        let more = value.chars().count() > offset + limit;
        Ok((value.chars().skip(offset).take(limit).collect(), more))
    }
}

#[test]
fn custom_drawn_windows_can_be_observed_and_hit_tested_without_keyboard_focus() {
    let mut c = context();
    let token = CancellationToken::new();
    *c.backend.missing_focus.borrow_mut() = true;
    *c.backend.hit.borrow_mut() = Some(2);
    let window = c.run("_guard_window", &json!({}), &token).unwrap();
    let input = c
        .run("_guard_input", &json!({"ref":window["windowRef"]}), &token)
        .unwrap();
    assert_eq!(input["focusAvailable"], false);
    assert!(super::super::visual_target::same_window(&window, &input));
    assert!(c
        .run(
            "_guard_point",
            &json!({"windowRef":window["windowRef"],"x":0,"y":10}),
            &token
        )
        .is_ok());
    *c.backend.foreground.borrow_mut() = Some(4);
    assert!(c
        .run("_guard_window", &json!({"ref":window["windowRef"]}), &token)
        .unwrap_err()
        .starts_with("foreground_required"));
}

#[test]
fn window_observation_never_requires_password_focus_but_input_still_rejects_it() {
    let mut c = context();
    let token = CancellationToken::new();
    c.backend.nodes.borrow_mut().get_mut(&3).unwrap().protected = true;
    assert!(c.run("_guard_window", &json!({}), &token).is_ok());
    assert!(c
        .run("_guard_input", &json!({}), &token)
        .unwrap_err()
        .starts_with("protected"));
}
fn context() -> Context<Fake> {
    let backend = Fake::default();
    *backend.alive.borrow_mut() = true;
    *backend.focus_ok.borrow_mut() = true;
    for (id, role, name) in [
        (1, "application", "app"),
        (2, "window", "test"),
        (3, "edit", "duplicate"),
        (4, "button", "duplicate"),
        (5, "edit", "password"),
    ] {
        backend.nodes.borrow_mut().insert(
            id,
            Node {
                role: role.into(),
                name: name.into(),
                enabled: true,
                writable: true,
                protected: id == 5,
                value: Some(if id == 5 { "secret" } else { "before" }.into()),
                actions: vec!["focus".into()],
                bounds: Some([-300.0, 0.0, 250.0, 100.0]),
                ..Node::default()
            },
        );
    }
    Context {
        backend,
        refs: HashMap::new(),
        approvals: HashMap::new(),
        pages: HashMap::new(),
    }
}

#[test]
fn visual_click_rejects_changed_hit_identity_window_and_document_before_dispatch() {
    for mutation in ["hit", "window", "document", "name"] {
        let mut c = context();
        let token = CancellationToken::new();
        let target = c
            .run("_guard_point", &json!({"x":0,"y":10}), &token)
            .unwrap();
        match mutation {
            "hit" => *c.backend.hit.borrow_mut() = Some(4),
            "window" => c.backend.nodes.borrow_mut().get_mut(&2).unwrap().name = "different".into(),
            "document" => {
                *c.backend.document_location.borrow_mut() = Some("file:///different".into())
            }
            _ => c.backend.nodes.borrow_mut().get_mut(&3).unwrap().name = "different".into(),
        }
        let error = c
            .run(
                "_click_point",
                &json!({"ref":target["ref"],"x":0,"y":10}),
                &token,
            )
            .unwrap_err();
        assert!(error.starts_with("target_changed"), "{mutation}: {error}");
        assert!(c.backend.calls.borrow().is_empty());
    }
}

#[test]
fn visual_click_rejects_protected_targets_and_cancelled_work() {
    let mut c = context();
    let token = CancellationToken::new();
    *c.backend.hit.borrow_mut() = Some(5);
    assert!(c
        .run("_guard_point", &json!({"x":0,"y":10}), &token)
        .unwrap_err()
        .starts_with("protected"));
    assert!(c.refs.is_empty());
    *c.backend.hit.borrow_mut() = None;
    let target = c
        .run("_guard_point", &json!({"x":0,"y":10}), &token)
        .unwrap();
    token.cancel();
    assert!(c
        .run(
            "_click_point",
            &json!({"ref":target["ref"],"x":0,"y":10}),
            &token
        )
        .unwrap_err()
        .starts_with("cancelled"));
    assert!(c.backend.calls.borrow().is_empty());
}
fn large_page(c: &mut Context<Fake>, w: Option<&str>) -> Value {
    let items: Vec<_> = (0..80)
        .map(|n| json!({"ref":format!("opaque-{n}"),"name":"中文\\\"\n".repeat(200)}))
        .collect();
    paging::start(
        if w.is_some() {
            "computer_snapshot"
        } else {
            "computer_list_apps"
        },
        &json!({"windowRef":w}),
        if w.is_some() {
            json!({"nodes":items,"snapshotId":"one-generation"})
        } else {
            json!({"items":items})
        },
        &mut c.pages,
    )
    .unwrap()
}
#[test]
fn all_pages_are_valid_bounded_json_and_preserve_order_once() {
    let mut c = context();
    let mut page = large_page(&mut c, None);
    let mut refs = Vec::new();
    loop {
        let bytes = serde_json::to_vec(&page).unwrap();
        assert!(bytes.len() <= paging::MAX_OUTPUT_BYTES);
        assert!(serde_json::from_slice::<Value>(&bytes).is_ok());
        refs.extend(
            page["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|n| n["ref"].as_str().unwrap().to_owned()),
        );
        if page["nextCursor"].is_null() {
            break;
        }
        page = call(
            &mut c,
            "computer_continue",
            json!({"cursor":page["nextCursor"]}),
        )
        .unwrap();
    }
    assert_eq!(
        refs,
        (0..80).map(|n| format!("opaque-{n}")).collect::<Vec<_>>()
    );
    assert!(c.pages.is_empty());
}
#[test]
fn oversized_context_metadata_cannot_escape_output_limit_even_with_no_nodes() {
    let mut c = context();
    assert!(paging::start(
        "computer_context",
        &json!({}),
        json!({"nodes":[],"documentUrl":"a".repeat(20_000)}),
        &mut c.pages
    )
    .unwrap_err()
    .contains("output_limit"));
    assert!(c.pages.is_empty());
}
#[test]
fn cursors_are_session_bound_single_use_and_expiring() {
    let mut c = context();
    let cursor = large_page(&mut c, None)["nextCursor"].clone();
    assert!(call(
        &mut context(),
        "computer_continue",
        json!({"cursor":cursor})
    )
    .is_err());
    call(&mut c, "computer_continue", json!({"cursor":cursor})).unwrap();
    assert!(call(&mut c, "computer_continue", json!({"cursor":cursor})).is_err());
    let cursor = large_page(&mut c, None)["nextCursor"].clone();
    c.pages.get_mut(cursor.as_str().unwrap()).unwrap().created = Instant::now() - REF_AGE;
    assert!(call(&mut c, "computer_continue", json!({"cursor":cursor})).is_err());
}
#[test]
fn observation_cursor_does_not_renew_retained_window_identity_or_grant_execution() {
    let mut c = context();
    let w = window(&mut c);
    let cursor = large_page(&mut c, Some(&w))["nextCursor"].clone();
    c.refs.get_mut(&w).unwrap().created = Instant::now() - REF_AGE;
    let page = call(&mut c, "computer_continue", json!({"cursor":cursor})).unwrap();
    assert_eq!(page["snapshotId"], "one-generation");
    assert!(c.refs[&w].created.elapsed() >= REF_AGE);
    assert!(c.resolve(&w).is_ok());
    assert!(call(
        &mut c,
        "computer_key_window",
        json!({"ref":w,"shortcut":"cmd+s"})
    )
    .is_err());
    assert!(c.backend.calls.borrow().is_empty());
}
#[test]
fn observation_cursor_still_validates_retained_window_and_native_liveness() {
    let mut c = context();
    let w = window(&mut c);
    let cursor = large_page(&mut c, Some(&w))["nextCursor"].clone();
    *c.backend.alive.borrow_mut() = false;
    assert!(call(&mut c, "computer_continue", json!({"cursor":cursor}))
        .unwrap_err()
        .contains("application_exited"));
    *c.backend.alive.borrow_mut() = true;
    let cursor = large_page(&mut c, Some(&w))["nextCursor"].clone();
    c.refs.remove(&w);
    assert!(call(&mut c, "computer_continue", json!({"cursor":cursor}))
        .unwrap_err()
        .contains("original window was released"));
}
#[test]
fn changed_snapshot_and_native_write_invalidate_pending_control_pages() {
    let mut c = context();
    let w = window(&mut c);
    let cursor = large_page(&mut c, Some(&w))["nextCursor"].clone();
    call(&mut c, "computer_snapshot", json!({"windowRef":w})).unwrap();
    let id = edit(&mut c, &w);
    assert!(call(&mut c, "computer_continue", json!({"cursor":cursor})).is_err());
    let cursor = large_page(&mut c, Some(&w))["nextCursor"].clone();
    let proof = prepare(&mut c, &id);
    call(
        &mut c,
        "computer_set_value",
        json!({"ref":id,"_proof":proof,"value":"new"}),
    )
    .unwrap();
    assert!(call(&mut c, "computer_continue", json!({"cursor":cursor})).is_err());
}
#[test]
fn escaped_long_text_is_delivered_completely_with_actual_offsets() {
    let mut c = context();
    let w = window(&mut c);
    let text = "中文😀\n\u{0001}\"\\".repeat(6000);
    c.backend.nodes.borrow_mut().get_mut(&3).unwrap().value = Some(text.clone());
    let id = edit(&mut c, &w);
    let mut actual = String::new();
    let mut offset = 0;
    loop {
        let result = call(
            &mut c,
            "computer_read",
            json!({"ref":id,"offset":offset,"limit":64000}),
        )
        .unwrap();
        assert!(serde_json::to_vec(&result).unwrap().len() <= paging::MAX_OUTPUT_BYTES);
        actual.push_str(result["text"].as_str().unwrap());
        let next = result["nextOffset"].as_u64().unwrap();
        assert!(next > offset);
        offset = next;
        if result["truncated"] == false {
            break;
        }
    }
    assert_eq!(actual, text);
    assert_eq!(
        call(
            &mut c,
            "_read_matches",
            json!({"ref":id,"text":text,"limit":64000})
        )
        .unwrap()["matches"],
        true
    );
}
#[test]
fn find_status_does_not_become_unique_when_candidates_are_paged() {
    let mut c = context();
    let values: Vec<_> = (0..40)
        .map(|n| json!({"ref":n,"name":"x".repeat(800)}))
        .collect();
    let result = paging::start(
        "computer_find",
        &json!({}),
        json!({"status":"ambiguous","candidates":values}),
        &mut c.pages,
    )
    .unwrap();
    assert_eq!(result["status"], "ambiguous");
    assert_eq!(result["totalItems"], 40);
    assert!(result["pageTruncated"] == true);
}
fn call(c: &mut Context<Fake>, method: &str, args: Value) -> Result<Value, String> {
    c.run(method, &args, &CancellationToken::new())
}
fn window(c: &mut Context<Fake>) -> String {
    call(c, "computer_list_windows", json!({})).unwrap()["items"][0]["ref"]
        .as_str()
        .unwrap()
        .into()
}
fn edit(c: &mut Context<Fake>, w: &str) -> String {
    call(
        c,
        "computer_find",
        json!({"windowRef":w,"role":"edit","name":"duplicate"}),
    )
    .unwrap()["candidates"][0]["ref"]
        .as_str()
        .unwrap()
        .into()
}
fn prepare(c: &mut Context<Fake>, id: &str) -> String {
    call(c, "_prepare", json!({"ref":id})).unwrap()["proof"]
        .as_str()
        .unwrap()
        .into()
}

#[test]
fn app_alias_context_combines_discovery_and_fresh_controls_without_actions() {
    let mut c = context();
    c.backend.nodes.borrow_mut().get_mut(&1).unwrap().name = "本地应用".into();
    c.backend.nodes.borrow_mut().get_mut(&1).unwrap().aliases = vec!["LocalEditor".into()];
    let first = call(&mut c, "computer_context", json!({"app":"localeditor"})).unwrap();
    let app = first["appRef"].as_str().unwrap();
    let window = first["windowRef"].as_str().unwrap();
    assert!(first["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|n| n["role"] == "edit"));
    assert_eq!(first["commandsSupported"], false);
    let second = call(&mut c, "computer_context", json!({"appRef":app})).unwrap();
    assert_eq!(second["appRef"], app);
    assert_eq!(second["windowRef"], window);
    assert!(c.backend.calls.borrow().is_empty());
}

#[test]
fn compact_context_preserves_empty_values_disabled_and_protected_states() {
    let mut c = context();
    c.backend.nodes.borrow_mut().get_mut(&3).unwrap().value = Some(String::new());
    c.backend.nodes.borrow_mut().get_mut(&4).unwrap().enabled = false;
    let state = call(&mut c, "computer_context", json!({"app":"app"})).unwrap();
    let nodes = state["nodes"].as_array().unwrap();
    assert!(nodes
        .iter()
        .any(|n| n["role"] == "edit" && n["value"] == "" && n["writable"] == true));
    assert!(nodes
        .iter()
        .any(|n| n["role"] == "button" && n["enabled"] == false));
    assert!(nodes
        .iter()
        .any(|n| n["protected"] == true && n.get("value").is_none()));
    assert_eq!(state["nodeDefaults"]["focused"], false);
    assert!(nodes
        .iter()
        .all(|n| n.get("focused") != Some(&json!(false))));
}
#[test]
fn provider_modifier_array_wrapper_is_validated_without_guessing_other_shapes() {
    assert_eq!(
        window_key_input(&json!({"key":"g","modifiers":{"item":["cmd","shift"]}}))
            .unwrap()
            .1,
        vec!["cmd", "shift"]
    );
    for modifiers in [
        json!({"item":["cmd"],"extra":true}),
        json!({"item":"shell"}),
        json!({"item":["invalid"]}),
    ] {
        assert!(window_key_input(&json!({"key":"g","modifiers":modifiers})).is_err());
    }
}

#[test]
fn native_action_returns_observed_post_action_ui_without_redispatch() {
    let mut c = context();
    let w = window(&mut c);
    let id = edit(&mut c, &w);
    let proof = prepare(&mut c, &id);
    let value = "通用控件结果";
    let output = call(
        &mut c,
        "computer_set_value",
        json!({"ref":id,"_proof":proof,"value":value}),
    )
    .unwrap();
    assert_eq!(output["verified"], true);
    assert_eq!(output["state"]["windowRef"], w);
    assert!(output["state"]["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|n| n["value"] == value));
    assert!(c.resolve(&id).is_err());
    assert_eq!(
        c.backend
            .calls
            .borrow()
            .iter()
            .filter(|s| s.as_str() == "set")
            .count(),
        1
    );
}

#[test]
fn post_action_observation_error_preserves_verified_write_and_never_replays() {
    let mut c = context();
    let w = window(&mut c);
    let id = edit(&mut c, &w);
    let proof = prepare(&mut c, &id);
    // The write target is available; only the subsequent application context fails.
    c.backend
        .inspect_failures
        .borrow_mut()
        .insert(1, "native_timeout".into());
    let output = call(
        &mut c,
        "computer_set_value",
        json!({"ref":id,"_proof":proof,"value":"once"}),
    )
    .unwrap();
    assert_eq!(output["verified"], true);
    assert_eq!(output["stateError"], "native_timeout");
    assert_eq!(
        c.backend
            .calls
            .borrow()
            .iter()
            .filter(|s| s.as_str() == "set")
            .count(),
        1
    );
}

#[test]
fn transient_post_action_timeout_recovers_state_without_repeating_input() {
    let mut c = context();
    let w = window(&mut c);
    let id = edit(&mut c, &w);
    let proof = prepare(&mut c, &id);
    *c.backend.transient_app_reads.borrow_mut() = 1;
    let output = call(
        &mut c,
        "computer_set_value",
        json!({"ref":id,"_proof":proof,"value":"once"}),
    )
    .unwrap();
    assert_eq!(output["verified"], true);
    assert!(output.get("stateError").is_none());
    assert_eq!(output["state"]["windowRef"], w);
    assert_eq!(
        c.backend
            .calls
            .borrow()
            .iter()
            .filter(|s| s.as_str() == "set")
            .count(),
        1
    );
}

#[test]
fn observation_retry_stops_on_takeover_and_does_not_retry_identity_errors() {
    let token = CancellationToken::new();
    let mut calls = 0;
    let result: Result<(), String> = retry_observation(&token, || {
        calls += 1;
        token.cancel();
        Err("native_timeout".into())
    });
    assert!(result.is_err());
    assert_eq!(calls, 1);
    let token = CancellationToken::new();
    let mut calls = 0;
    let result: Result<(), String> = retry_observation(&token, || {
        calls += 1;
        Err("stale_reference: application_exited".into())
    });
    assert!(result.is_err());
    assert_eq!(calls, 1);
}

#[test]
fn pause_invalidates_controls_receipts_and_pages_but_keeps_read_only_identities() {
    let mut c = context();
    let w = window(&mut c);
    let id = edit(&mut c, &w);
    let proof = prepare(&mut c, &id);
    let cursor = large_page(&mut c, Some(&w))["nextCursor"].clone();
    call(&mut c, "_invalidate_elements", json!({})).unwrap();
    assert!(c.resolve(&w).is_ok());
    assert!(c.resolve(&id).is_err());
    assert!(call(&mut c, "computer_continue", json!({"cursor":cursor})).is_err());
    assert!(call(
        &mut c,
        "computer_set_value",
        json!({"ref":id,"_proof":proof,"value":"blocked"})
    )
    .is_err());
    assert!(c.backend.calls.borrow().is_empty());
    let resumed = call(&mut c, "computer_context", json!({"windowRef":w})).unwrap();
    assert_eq!(resumed["windowRef"], w);
    assert!(!resumed["nodes"].as_array().unwrap().is_empty());
}
#[test]
fn refs_are_session_isolated_and_never_index_paths() {
    let mut a = context();
    let mut b = context();
    let w = window(&mut a);
    assert!(b.resolve(&w).unwrap_err().contains("stale_reference"));
    assert!(!w.contains('/'));
    let id = edit(&mut a, &w);
    assert!(call(&mut b, "computer_read", json!({"ref":id})).is_err());
}
#[test]
fn elements_expire_but_retained_app_window_still_validate_native_liveness() {
    let mut c = context();
    let w = window(&mut c);
    let id = edit(&mut c, &w);
    let a = call(&mut c, "computer_list_apps", json!({})).unwrap()["items"][0]["ref"]
        .as_str()
        .unwrap()
        .to_string();
    for id in [&w, &id, &a] {
        c.refs.get_mut(id).unwrap().created = Instant::now() - REF_AGE - Duration::from_secs(1);
    }
    assert!(c.resolve(&id).is_err());
    assert!(c.resolve(&w).is_ok());
    assert!(c.resolve(&a).is_ok());
    *c.backend.alive.borrow_mut() = false;
    assert!(c.resolve(&w).unwrap_err().contains("application_exited"));
    assert!(c.resolve(&a).unwrap_err().contains("application_exited"));
}
#[test]
fn duplicate_names_are_ambiguous() {
    let mut c = context();
    let w = window(&mut c);
    let r = call(
        &mut c,
        "computer_find",
        json!({"windowRef":w,"name":"duplicate"}),
    )
    .unwrap();
    assert_eq!(r["status"], "ambiguous");
    assert_eq!(r["candidates"].as_array().unwrap().len(), 2);
    assert!(c.backend.calls.borrow().is_empty());
}
#[test]
fn vanished_control_and_exited_app_cannot_be_used() {
    let mut c = context();
    let w = window(&mut c);
    let id = edit(&mut c, &w);
    c.backend.nodes.borrow_mut().remove(&3);
    assert!(call(&mut c, "_prepare", json!({"ref":id})).is_err());
    *c.backend.alive.borrow_mut() = false;
    assert!(call(&mut c, "computer_snapshot", json!({"windowRef":w}))
        .unwrap_err()
        .contains("application_exited"));
}
#[test]
fn changed_window_membership_is_stale() {
    let mut c = context();
    let w = window(&mut c);
    let id = edit(&mut c, &w);
    c.refs.get_mut(&id).unwrap().window = Some(42);
    assert!(c.resolve(&id).unwrap_err().contains("window changed"));
}
#[test]
fn protected_fields_are_redacted_and_never_written() {
    let mut c = context();
    let w = window(&mut c);
    let r = call(&mut c, "computer_snapshot", json!({"windowRef":w})).unwrap();
    let protected = r["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["protected"] == true)
        .unwrap();
    assert!(protected["value"].is_null());
    assert!(!protected.to_string().contains("secret"));
    assert!(call(&mut c, "_prepare", json!({"ref":protected["ref"]}))
        .unwrap_err()
        .contains("protected"));
    assert!(c.backend.calls.borrow().is_empty());
}
#[test]
fn disabled_and_readonly_never_dispatch() {
    for readonly in [true, false] {
        let mut c = context();
        let w = window(&mut c);
        if readonly {
            c.backend.nodes.borrow_mut().get_mut(&3).unwrap().writable = false;
        } else {
            c.backend.nodes.borrow_mut().get_mut(&3).unwrap().enabled = false;
        }
        let id = edit(&mut c, &w);
        if readonly {
            let proof = prepare(&mut c, &id);
            assert!(call(
                &mut c,
                "computer_set_value",
                json!({"ref":id,"_proof":proof,"value":"blocked"})
            )
            .unwrap_err()
            .contains("readonly"));
        } else {
            assert!(call(&mut c, "_prepare", json!({"ref":id}))
                .unwrap_err()
                .contains("disabled"));
        }
        assert!(c.backend.calls.borrow().is_empty());
    }
}
#[test]
fn unicode_multiline_and_special_text_is_read_back_exactly() {
    let mut c = context();
    let w = window(&mut c);
    let id = edit(&mut c, &w);
    let proof = prepare(&mut c, &id);
    let text = "中文😀\n第二行 <>&\"'\\ $()";
    let r = call(
        &mut c,
        "computer_set_value",
        json!({"ref":id,"_proof":proof,"value":text}),
    )
    .unwrap();
    assert_eq!(r["verified"], true);
    let id = edit(&mut c, &w);
    assert_eq!(
        call(&mut c, "computer_read", json!({"ref":id})).unwrap()["text"],
        text
    );
}
#[test]
fn snapshots_truncate_and_reads_continue_without_claiming_history() {
    let mut c = context();
    let w = window(&mut c);
    let snapshot = call(
        &mut c,
        "computer_snapshot",
        json!({"windowRef":w,"maxNodes":2}),
    )
    .unwrap();
    assert_eq!(snapshot["truncated"], true);
    assert_eq!(snapshot["completeHistory"], false);
    let id = edit(&mut c, &w);
    c.backend.nodes.borrow_mut().get_mut(&3).unwrap().value = Some("中文😀多行\n特殊字符".into());
    let mut text = String::new();
    let mut offset = 0;
    loop {
        let r = call(
            &mut c,
            "computer_read",
            json!({"ref":id,"offset":offset,"limit":3}),
        )
        .unwrap();
        text.push_str(r["text"].as_str().unwrap());
        offset = r["nextOffset"].as_u64().unwrap();
        if r["truncated"] == false {
            break;
        }
    }
    assert_eq!(text, "中文😀多行\n特殊字符");
}
#[test]
fn errors_and_timeouts_are_preserved() {
    for failure in ["native_timeout", "permission_denied", "native_failure"] {
        let mut c = context();
        let w = window(&mut c);
        *c.backend.failure.borrow_mut() = Some(failure.into());
        assert!(call(&mut c, "computer_snapshot", json!({"windowRef":w}))
            .unwrap_err()
            .contains(failure));
    }
}
#[test]
fn approval_does_not_transfer_to_changed_target_or_replay() {
    let mut c = context();
    let w = window(&mut c);
    let id = edit(&mut c, &w);
    let proof = prepare(&mut c, &id);
    c.backend.nodes.borrow_mut().get_mut(&3).unwrap().name = "new target".into();
    let args = json!({"ref":id,"_proof":proof,"value":"do not type"});
    assert!(call(&mut c, "computer_set_value", args.clone())
        .unwrap_err()
        .contains("target_changed"));
    assert!(call(&mut c, "computer_set_value", args)
        .unwrap_err()
        .contains("stale_approval"));
    assert!(c.backend.calls.borrow().is_empty());
}
#[test]
fn approval_revalidates_identity_after_observation_expires() {
    let mut c = context();
    let w = window(&mut c);
    let id = edit(&mut c, &w);
    let proof = prepare(&mut c, &id);
    c.refs.get_mut(&id).unwrap().created = Instant::now() - REF_AGE;
    let r = call(
        &mut c,
        "computer_set_value",
        json!({"ref":id,"_proof":proof,"value":"approved original"}),
    )
    .unwrap();
    assert_eq!(r["verified"], true);
}
#[test]
fn cancellation_after_approval_dispatches_nothing() {
    let mut c = context();
    let w = window(&mut c);
    let id = edit(&mut c, &w);
    let proof = prepare(&mut c, &id);
    let token = CancellationToken::new();
    token.cancel();
    assert!(c
        .run(
            "computer_set_value",
            &json!({"ref":id,"_proof":proof,"value":"cancelled"}),
            &token
        )
        .is_err());
    assert!(c.backend.calls.borrow().is_empty());
}
#[test]
fn refresh_keeps_unchanged_native_element_and_window_identities() {
    let mut c = context();
    let w = window(&mut c);
    let old = edit(&mut c, &w);
    call(&mut c, "computer_snapshot", json!({"windowRef":w})).unwrap();
    assert!(c.resolve(&old).is_ok());
    assert!(c.resolve(&w).is_ok());
}
#[test]
fn post_action_observation_reuses_only_unchanged_native_targets() {
    let mut c = context();
    let w = window(&mut c);
    let first = edit(&mut c, &w);
    let other = call(
        &mut c,
        "computer_find",
        json!({"windowRef":w,"role":"button"}),
    )
    .unwrap()["candidates"][0]["ref"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(c.resolve(&first).is_ok());
    let proof = prepare(&mut c, &first);
    call(
        &mut c,
        "computer_set_value",
        json!({"ref":first,"_proof":proof,"value":"new"}),
    )
    .unwrap();
    assert!(c.resolve(&other).is_ok());
    assert!(c.resolve(&first).is_err());
}
#[test]
fn repeated_not_found_queries_do_not_accumulate_native_references() {
    let mut c = context();
    let w = window(&mut c);
    for _ in 0..150 {
        let result = call(
            &mut c,
            "computer_find",
            json!({"windowRef":w,"role":"missing"}),
        )
        .unwrap();
        assert_eq!(result["status"], "not_found");
    }
    assert_eq!(c.refs.len(), 1);
}
#[test]
fn negative_bounds_are_preserved_in_native_units() {
    let mut c = context();
    let w = window(&mut c);
    let r = call(&mut c, "computer_snapshot", json!({"windowRef":w})).unwrap();
    assert_eq!(r["nodes"][0]["bounds"][0], -300.0);
    assert_eq!(
        r["coordinateUnit"],
        if cfg!(target_os = "windows") {
            "physical_desktop_pixels"
        } else {
            "desktop_logical_points"
        }
    );
}
#[test]
fn long_input_uses_full_readback_instead_of_snapshot_preview() {
    let mut c = context();
    let w = window(&mut c);
    let id = edit(&mut c, &w);
    let proof = prepare(&mut c, &id);
    let text = "中文😀\n<>".repeat(1500);
    let result = call(
        &mut c,
        "computer_set_value",
        json!({"ref":id,"_proof":proof,"value":text}),
    )
    .unwrap();
    assert_eq!(result["verified"], true);
}
#[test]
fn partial_search_never_claims_unique_or_absent() {
    let mut c = context();
    let w = window(&mut c);
    let partial = call(
        &mut c,
        "computer_find",
        json!({"windowRef":w,"name":"duplicate","maxNodes":2}),
    )
    .unwrap();
    assert_eq!(partial["status"], "incomplete");
    assert_eq!(partial["searchComplete"], false);
    let full = call(
        &mut c,
        "computer_find",
        json!({"windowRef":w,"name":"duplicate"}),
    )
    .unwrap();
    assert_eq!(full["status"], "ambiguous");
}
#[test]
fn clearing_a_value_reads_back_empty() {
    let mut c = context();
    let w = window(&mut c);
    let id = edit(&mut c, &w);
    let proof = prepare(&mut c, &id);
    assert_eq!(
        call(
            &mut c,
            "computer_set_value",
            json!({"ref":id,"_proof":proof,"value":""})
        )
        .unwrap()["verified"],
        true
    );
}
#[test]
fn writes_require_a_private_single_use_approval_receipt() {
    let mut c = context();
    let w = window(&mut c);
    let id = edit(&mut c, &w);
    assert!(call(
        &mut c,
        "computer_set_value",
        json!({"ref":id,"value":"unauthorized"})
    )
    .is_err());
    assert!(c.backend.calls.borrow().is_empty());
    let proof = prepare(&mut c, &id);
    let args = json!({"ref":id,"value":"authorized","_proof":proof});
    assert_eq!(
        call(&mut c, "computer_set_value", args.clone()).unwrap()["verified"],
        true
    );
    assert!(call(&mut c, "computer_set_value", args).is_err());
    assert_eq!(c.backend.calls.borrow().len(), 1);
}
#[test]
fn unsupported_actions_do_not_become_coordinate_clicks() {
    let mut c = context();
    let w = window(&mut c);
    let id = edit(&mut c, &w);
    let proof = prepare(&mut c, &id);
    assert!(call(
        &mut c,
        "computer_invoke",
        json!({"ref":id,"action":"click","_proof":proof})
    )
    .unwrap_err()
    .contains("unsupported_action"));
    assert!(c.backend.calls.borrow().is_empty());
}
#[test]
fn approval_does_not_follow_a_save_button_into_a_different_document() {
    let mut c = context();
    let w = window(&mut c);
    let id = edit(&mut c, &w);
    let proof = prepare(&mut c, &id);
    c.backend.nodes.borrow_mut().get_mut(&2).unwrap().name = "different document".into();
    assert!(call(
        &mut c,
        "computer_set_value",
        json!({"ref":id,"_proof":proof,"value":"do not write"})
    )
    .unwrap_err()
    .contains("window context changed"));
    assert!(c.backend.calls.borrow().is_empty());
}
#[test]
fn a_reused_native_object_is_rejected_when_label_changes_before_approval() {
    let mut c = context();
    let w = window(&mut c);
    let id = edit(&mut c, &w);
    c.backend.nodes.borrow_mut().get_mut(&3).unwrap().name = "different action".into();
    assert!(call(&mut c, "_prepare", json!({"ref":id}))
        .unwrap_err()
        .contains("stale_reference"));
    assert!(c.approvals.is_empty());
    assert!(c.backend.calls.borrow().is_empty());
}
#[tokio::test]
async fn dropped_http_caller_cannot_release_desktop_during_native_dispatch() {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    let desktop = Arc::new(tokio::sync::Mutex::new(()));
    let gate = Arc::new(tokio::sync::RwLock::new(()));
    let lease = ExecutionLease {
        _desktop: Some(super::super::desktop_coordinator::DesktopLease::for_test(
            desktop.clone().lock_owned().await,
        )),
        _input: vec![],
        _session: gate.clone().read_owned().await,
    };
    let (sender, receiver) = mpsc::sync_channel::<Request>(1);
    let controller = Controller {
        sender,
        alive: Arc::new(AtomicBool::new(true)),
    };
    let (started, observed) = oneshot::channel();
    let (finish, finished) = mpsc::channel();
    let completed = Arc::new(AtomicBool::new(false));
    let completed_by_worker = completed.clone();
    let worker = std::thread::spawn(move || {
        let request = receiver.recv().unwrap();
        started.send(()).unwrap();
        // Represents a non-retractable OS call already in progress.
        finished.recv().unwrap();
        completed_by_worker.store(true, Ordering::SeqCst);
        let _ = request.reply.send(Ok(json!({"executed":true})));
        // Remaining Request fields, including the owned lease, drop here.
    });
    let caller = tokio::spawn(async move {
        controller
            .call("native-test", json!({}), &CancellationToken::new(), lease)
            .await
    });
    observed.await.unwrap();
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    assert!(
        desktop.try_lock().is_err(),
        "another session must not enter while the OS call runs"
    );
    assert!(
        gate.try_write().is_err(),
        "pause must not acknowledge before the OS call returns"
    );
    finish.send(()).unwrap();
    worker.join().unwrap();
    assert!(completed.load(Ordering::SeqCst));
    assert!(desktop.try_lock().is_ok());
    assert!(gate.try_write().is_ok());
}
#[test]
fn clipped_selector_metadata_cannot_prove_an_exact_unique_match() {
    let mut c = context();
    let w = window(&mut c);
    c.backend
        .nodes
        .borrow_mut()
        .get_mut(&3)
        .unwrap()
        .metadata_truncated = true;
    let result = call(
        &mut c,
        "computer_find",
        json!({"windowRef":w,"role":"edit","name":"duplicate"}),
    )
    .unwrap();
    assert_eq!(result["status"], "incomplete");
    assert_eq!(result["searchComplete"], false);
}

#[test]
fn inaccessible_application_preserves_usable_windows_and_marks_discovery_incomplete() {
    let mut c = context();
    *c.backend.listing_issues.borrow_mut() = vec![(
        "other app".into(),
        "native_timeout: provider did not respond".into(),
    )];
    let listing = call(&mut c, "computer_list_windows", json!({})).unwrap();
    assert_eq!(listing["status"], "incomplete");
    assert_eq!(listing["probe"], "partial");
    assert_eq!(listing["truncated"], true);
    assert_eq!(listing["issueCount"], 1);
    let reference = listing["items"][0]["ref"].as_str().unwrap();
    assert!(c.resolve(reference).is_ok());
    assert_eq!(
        call(&mut c, "computer_snapshot", json!({"windowRef":reference})).unwrap()["nodes"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
}
#[test]
fn many_discovery_errors_stay_bounded_without_hiding_incompleteness() {
    let mut c = context();
    *c.backend.listing_issues.borrow_mut() = (0..100)
        .map(|_| ("\u{1}".repeat(1024), "\u{1}".repeat(1024)))
        .collect();
    let listing = call(&mut c, "computer_list_windows", json!({})).unwrap();
    assert_eq!(listing["issueCount"], 100);
    assert_eq!(listing["issuesTruncated"], true);
    assert!(serde_json::to_vec(&listing).unwrap().len() <= paging::MAX_OUTPUT_BYTES);
}
#[test]
fn window_shortcut_revalidates_approved_window_and_never_claims_task_verified() {
    let mut c = context();
    let w = window(&mut c);
    let proof = prepare(&mut c, &w);
    c.refs.get_mut(&w).unwrap().created = Instant::now() - REF_AGE;
    let result = call(
        &mut c,
        "computer_key_window",
        json!({"ref":w,"_proof":proof,"key":"s","modifiers":["cmd"]}),
    )
    .unwrap();
    assert_eq!(result["focusVerified"], true);
    assert_eq!(result["keyDispatched"], true);
    assert_eq!(result["verified"], false);
    assert_eq!(result["status"], "dispatched_unverified");
    assert_eq!(result["dispatched"], true);
    assert!(result["executed"].is_null());
    assert_eq!(
        *c.backend.calls.borrow(),
        vec!["activate:2", "key:s:[\"cmd\"]"]
    );
}

#[test]
fn combined_shortcut_strings_are_rejected_before_window_activation() {
    let mut c = context();
    let w = window(&mut c);
    let proof = prepare(&mut c, &w);
    let error = call(
        &mut c,
        "computer_key_window",
        json!({"ref":w,"_proof":proof,"key":"command+n","modifiers":[]}),
    )
    .unwrap_err();
    assert!(error.contains("modifiers"));
    assert!(c.backend.calls.borrow().is_empty());
}

#[test]
fn explicit_shortcut_strings_use_the_same_approved_window_dispatch() {
    let mut c = context();
    let w = window(&mut c);
    let proof = prepare(&mut c, &w);
    let result = call(
        &mut c,
        "computer_key_window",
        json!({"ref":w,"_proof":proof,"shortcut":"cmd+n"}),
    )
    .unwrap();
    assert_eq!(result["keyDispatched"], true);
    assert_eq!(
        *c.backend.calls.borrow(),
        vec!["activate:2", "key:n:[\"cmd\"]"]
    );
    assert!(window_key_input(&json!({"shortcut":"cmd+n","key":"n"})).is_err());
    assert!(window_key_input(&json!({"shortcut":"cmd++n"})).is_err());
    assert!(window_key_input(&json!({"shortcut":"unknown+n"})).is_err());
    let args = json!({"shortcut":"command+meta+shift+s"});
    assert_eq!(
        window_key_input(&args).unwrap(),
        ("s", vec!["cmd".into(), "shift".into()])
    );
}
#[test]
fn changed_window_focus_failure_protected_focus_and_cancel_never_send_shortcut() {
    for scenario in ["changed", "focus", "protected", "cancel"] {
        let mut c = context();
        let w = window(&mut c);
        let proof = prepare(&mut c, &w);
        let token = CancellationToken::new();
        match scenario {
            "changed" => {
                c.backend.nodes.borrow_mut().get_mut(&2).unwrap().name = "different document".into()
            }
            "focus" => *c.backend.focus_ok.borrow_mut() = false,
            "protected" => *c.backend.protected_focus.borrow_mut() = true,
            "cancel" => *c.backend.cancel_on_activate.borrow_mut() = Some(token.clone()),
            _ => unreachable!(),
        }
        let result = c.run(
            "computer_key_window",
            &json!({"ref":w,"_proof":proof,"key":"s","modifiers":["cmd"]}),
            &token,
        );
        if scenario == "changed" {
            assert!(result.unwrap_err().contains("target_changed"));
        } else {
            assert_ne!(result.unwrap()["keyDispatched"], true);
        }
        assert!(
            !c.backend
                .calls
                .borrow()
                .iter()
                .any(|s| s.starts_with("key:")),
            "{scenario}"
        );
    }
}

#[test]
fn exact_application_discovery_filters_before_refs_and_does_not_guess_absence() {
    let mut c = context();
    let found = call(&mut c, "computer_list_apps", json!({"name":"app"})).unwrap();
    assert_eq!(found["matchStatus"], "unique");
    assert_eq!(found["items"].as_array().unwrap().len(), 1);
    assert!(c
        .resolve(found["items"][0]["ref"].as_str().unwrap())
        .is_ok());
    let absent = call(&mut c, "computer_list_apps", json!({"name":"another app"})).unwrap();
    assert_eq!(absent["matchStatus"], "not_found");
    assert!(absent["items"].as_array().unwrap().is_empty());
    assert!(call(&mut c, "computer_list_apps", json!({"name":""})).is_err());
}

#[test]
fn a_failed_child_provider_keeps_readable_siblings_and_search_incomplete() {
    let mut c = context();
    let w = window(&mut c);
    c.backend
        .inspect_failures
        .borrow_mut()
        .insert(3, "native_failure: provider; read(AXValue)".into());
    let snapshot = call(&mut c, "computer_snapshot", json!({"windowRef":w})).unwrap();
    assert_eq!(snapshot["treeTruncated"], true);
    assert_eq!(snapshot["status"], "incomplete");
    assert_eq!(snapshot["issueCount"], 1);
    let button = snapshot["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["role"] == "button")
        .unwrap();
    assert_eq!(
        call(&mut c, "computer_read", json!({"ref":button["ref"]})).unwrap()["text"],
        "before"
    );
    let found = call(
        &mut c,
        "computer_find",
        json!({"windowRef":w,"name":"duplicate"}),
    )
    .unwrap();
    assert_eq!(found["candidates"].as_array().unwrap().len(), 1);
    assert_eq!(found["status"], "incomplete");
    assert_eq!(found["searchComplete"], false);
    assert_eq!(found["issueCount"], 1);
}

#[test]
fn failed_child_expansion_keeps_parent_but_permission_loss_aborts_snapshot() {
    let mut c = context();
    let w = window(&mut c);
    c.backend
        .children_failures
        .borrow_mut()
        .insert(3, "native_failure: count(AXChildren)".into());
    let snapshot = call(&mut c, "computer_snapshot", json!({"windowRef":w})).unwrap();
    assert_eq!(snapshot["nodes"].as_array().unwrap().len(), 4);
    let edit = snapshot["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["name"] == "duplicate" && n["role"] == "edit")
        .unwrap();
    assert_eq!(edit["childrenTruncated"], true);
    assert_eq!(snapshot["treeTruncated"], true);
    c.backend
        .children_failures
        .borrow_mut()
        .insert(4, "permission_denied: revoked".into());
    assert!(call(&mut c, "computer_snapshot", json!({"windowRef":w}))
        .unwrap_err()
        .starts_with("permission_denied"));
}

#[test]
fn unknown_labels_do_not_hide_known_roles_but_name_search_stays_incomplete() {
    let mut c = context();
    let w = window(&mut c);
    c.backend
        .nodes
        .borrow_mut()
        .get_mut(&4)
        .unwrap()
        .metadata_truncated = true;
    let by_role = call(
        &mut c,
        "computer_find",
        json!({"windowRef":w,"role":"button"}),
    )
    .unwrap();
    assert_eq!(by_role["status"], "unique");
    assert_eq!(by_role["searchComplete"], true);
    let by_name = call(
        &mut c,
        "computer_find",
        json!({"windowRef":w,"name":"unobserved label"}),
    )
    .unwrap();
    assert_eq!(by_name["status"], "incomplete");
    assert_eq!(by_name["searchComplete"], false);
}

#[test]
fn explicit_keyboard_replacement_verifies_unicode_without_native_value_support() {
    let mut c = context();
    c.backend.nodes.borrow_mut().get_mut(&3).unwrap().writable = false;
    let w = window(&mut c);
    let e = edit(&mut c, &w);
    let proof = prepare(&mut c, &e);
    let value = "中文\n多行 😀 <>&\"'\\ $()\n".repeat(4);
    let result = call(
        &mut c,
        "computer_set_value",
        json!({"_proof":proof,"value":value,"inputMethod":"keyboard"}),
    )
    .unwrap();
    assert_eq!(result["verified"], true);
    assert_eq!(
        c.backend.nodes.borrow()[&3].value.as_deref(),
        Some(value.as_str())
    );
    assert!(!c.backend.calls.borrow().contains(&"set".into()));
    assert!(c.backend.calls.borrow().contains(&"unicode".into()));
    assert!(call(&mut c, "computer_read", json!({"ref":e})).is_err());
}
#[test]
fn keyboard_replacement_stops_for_cancelled_or_protected_focus_and_never_types_into_buttons() {
    for scenario in ["cancel", "protected", "button"] {
        let mut c = context();
        if scenario == "button" {
            c.backend.nodes.borrow_mut().get_mut(&3).unwrap().role = "button".into();
        }
        let w = window(&mut c);
        let observed = call(&mut c,"computer_find",json!({"windowRef":w,"name":"duplicate","role":if scenario=="button" {"button"} else {"edit"}})).unwrap();
        let e = observed["candidates"][0]["ref"].as_str().unwrap();
        let proof = prepare(&mut c, e);
        let token = CancellationToken::new();
        if scenario == "cancel" {
            *c.backend.cancel_on_activate.borrow_mut() = Some(token.clone());
        }
        if scenario == "protected" {
            *c.backend.protected_focus.borrow_mut() = true;
        }
        let _ = c.run(
            "computer_set_value",
            &json!({"_proof":proof,"value":"text","inputMethod":"keyboard"}),
            &token,
        );
        assert!(!c
            .backend
            .calls
            .borrow()
            .iter()
            .any(|call| call == "unicode" || call.starts_with("key:")));
    }
}
#[test]
fn desktop_disconnection_invalidates_references_and_pending_receipts() {
    let mut c = context();
    let w = window(&mut c);
    let e = edit(&mut c, &w);
    let proof = prepare(&mut c, &e);
    *c.backend.session_error.borrow_mut() = Some("desktop_unavailable: sleeping".into());
    assert!(call(
        &mut c,
        "computer_set_value",
        json!({"_proof":proof,"value":"text"})
    )
    .unwrap_err()
    .starts_with("desktop_unavailable"));
    assert!(c.refs.is_empty() && c.approvals.is_empty() && c.pages.is_empty());
    *c.backend.session_error.borrow_mut() = None;
    assert!(call(&mut c, "computer_read", json!({"ref":e})).is_err());
    assert!(c.backend.calls.borrow().is_empty());
}
#[test]
fn text_input_methods_are_explicit_bounded_and_do_not_unescape_content() {
    let literal = "literal \\n 中文 😀";
    assert_eq!(
        text_input(&json!({"value":literal})).unwrap(),
        (literal, false)
    );
    for args in [
        json!({"value":"x","inputMethod":true}),
        json!({"value":"x","inputMethod":"auto"}),
        json!({"value":42}),
        json!({"value":"x".repeat(8001),"inputMethod":"keyboard"}),
    ] {
        assert!(text_input(&args).is_err());
    }
    let value = "中😀\n\\\"'".repeat(30);
    let chunks = unicode_input_chunks(&value);
    assert_eq!(chunks.concat(), value);
    assert!(chunks
        .iter()
        .all(|chunk| !chunk.is_empty() && chunk.encode_utf16().count() <= 20));
}

#[test]
fn unicode_replacement_never_continues_after_mid_input_takeover_or_focus_change() {
    for cancelled in [false, true] {
        let mut c = context();
        let w = window(&mut c);
        let e = edit(&mut c, &w);
        let proof = prepare(&mut c, &e);
        let token = CancellationToken::new();
        if cancelled {
            *c.backend.cancel_after_text.borrow_mut() = Some(token.clone());
        } else {
            *c.backend.protect_after_text.borrow_mut() = true;
        }
        let text = "中文😀".repeat(20);
        let result = c
            .run(
                "computer_set_value",
                &json!({"_proof":proof,"value":text,"inputMethod":"keyboard"}),
                &token,
            )
            .unwrap();
        assert_eq!(result["status"], "result_unknown");
        assert_eq!(result["replaySafe"], false);
        assert_eq!(
            c.backend
                .calls
                .borrow()
                .iter()
                .filter(|v| *v == "unicode")
                .count(),
            1
        );
        assert_ne!(
            c.backend.nodes.borrow()[&3].value.as_deref(),
            Some(text.as_str())
        );
        assert!(call(&mut c, "computer_read", json!({"ref":e})).is_err());
    }
}

#[test]
fn keyboard_response_observes_after_asynchronous_text_delivery() {
    let mut c = context();
    let w = window(&mut c);
    let e = edit(&mut c, &w);
    let proof = prepare(&mut c, &e);
    let value = "delivered after initial inspection";
    *c.backend.deferred_text.borrow_mut() = Some(value.into());
    let result = call(
        &mut c,
        "computer_set_value",
        json!({"_proof":proof,"value":value,"inputMethod":"keyboard"}),
    )
    .unwrap();
    assert_eq!(result["verified"], true);
    assert_eq!(result["after"]["value"], value);
}

#[test]
fn real_document_url_is_exposed_independently_of_a_path_like_title() {
    let mut c = context();
    c.backend.nodes.borrow_mut().get_mut(&2).unwrap().name = "/requested/output.txt".into();
    *c.backend.document_location.borrow_mut() = Some("file:///actual/other-name.txt".into());
    let token = CancellationToken::new();
    let windows = c.run("computer_list_windows", &json!({}), &token).unwrap();
    assert_eq!(windows["items"][0]["name"], "/requested/output.txt");
    assert_eq!(
        windows["items"][0]["documentUrl"],
        "file:///actual/other-name.txt"
    );
    let snapshot = c
        .run(
            "computer_snapshot",
            &json!({"windowRef":windows["items"][0]["ref"]}),
            &token,
        )
        .unwrap();
    assert_eq!(snapshot["documentUrl"], "file:///actual/other-name.txt");
}

#[test]
fn reference_click_uses_retained_control_and_same_cancellation_guard() {
    let mut c = context();
    let token = CancellationToken::new();
    let windows = c.run("computer_list_windows", &json!({}), &token).unwrap();
    let snapshot = c
        .run(
            "computer_snapshot",
            &json!({"windowRef":windows["items"][0]["ref"]}),
            &token,
        )
        .unwrap();
    let target = snapshot["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["role"] == "button")
        .unwrap()["ref"]
        .clone();
    let proof = c.run("_prepare", &json!({"ref":target}), &token).unwrap()["proof"].clone();
    c.run(
        "computer_click_control",
        &json!({"ref":target,"_proof":proof,"clickCount":2}),
        &token,
    )
    .unwrap();
    assert!(c
        .backend
        .calls
        .borrow()
        .iter()
        .any(|call| call == "control_click:2"));
    token.cancel();
    assert!(c
        .run(
            "computer_click_control",
            &json!({"ref":target,"clickCount":2}),
            &token
        )
        .is_err());
}

#[test]
fn reference_click_cancels_during_activation_without_dispatch() {
    let mut c = context();
    let token = CancellationToken::new();
    let windows = c.run("computer_list_windows", &json!({}), &token).unwrap();
    let snapshot = c
        .run(
            "computer_snapshot",
            &json!({"windowRef":windows["items"][0]["ref"]}),
            &token,
        )
        .unwrap();
    let target = snapshot["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["role"] == "button")
        .unwrap()["ref"]
        .clone();
    let proof = c.run("_prepare", &json!({"ref":target}), &token).unwrap()["proof"].clone();
    *c.backend.cancel_on_activate.borrow_mut() = Some(token.clone());
    let result = c
        .run(
            "computer_click_control",
            &json!({"ref":target,"_proof":proof,"clickCount":2}),
            &token,
        )
        .unwrap();
    assert_eq!(result["verified"], false);
    assert!(!c
        .backend
        .calls
        .borrow()
        .iter()
        .any(|call| call.starts_with("control_click:")));
}

#[test]
fn click_target_modes_cannot_be_mixed_or_forged() {
    assert_eq!(
        native_click_ref(&json!({"ref":"node-1","clickCount":2})).unwrap(),
        Some("node-1")
    );
    assert_eq!(
        native_click_ref(&json!({"frameId":"frame-1","x":1,"y":2})).unwrap(),
        None
    );
    for args in [
        json!({"ref":""}),
        json!({"ref":2}),
        json!({"ref":"node-1","x":2}),
        json!({"ref":"node-1","frameId":"frame-1"}),
        json!({"ref":"node-1","button":"right"}),
    ] {
        assert!(native_click_ref(&args).is_err());
    }
}

#[test]
fn bounded_observation_keeps_shallow_dialog_buttons_before_large_list_text() {
    let mut c = context();
    c.backend.child_overrides.borrow_mut().insert(2, vec![3, 4]);
    c.backend.child_overrides.borrow_mut().insert(3, vec![10]);
    c.backend.nodes.borrow_mut().get_mut(&3).unwrap().value = None;
    c.backend.nodes.borrow_mut().get_mut(&4).unwrap().name = "取消".into();
    let mut content = c.backend.nodes.borrow()[&3].clone();
    content.name = "file list content".into();
    content.value = Some("文件".repeat(300));
    c.backend.nodes.borrow_mut().insert(10, content);
    let token = CancellationToken::new();
    let windows = c.run("computer_list_windows", &json!({}), &token).unwrap();
    let window = windows["items"][0]["ref"].as_str().unwrap();
    let snapshot = c
        .run(
            "computer_snapshot",
            &json!({"windowRef":window,"maxText":64}),
            &token,
        )
        .unwrap();
    assert!(snapshot["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|n| n["name"] == "取消"));
    assert_eq!(snapshot["truncated"], true);
    assert_eq!(snapshot["status"], "incomplete");
}

#[test]
fn provider_decimal_integer_options_are_normalized_without_other_coercion() {
    let args =
        json!({"ref":"node-1","clickCount":"2","x":"10.0","value":"001\n","_proof":"opaque"});
    let result = normalize_integer_options("computer_click", &args).unwrap();
    assert_eq!(result["clickCount"], 2);
    for key in ["ref", "x", "value", "_proof"] {
        assert_eq!(result[key], args[key]);
    }
    assert_eq!(
        normalize_integer_options("computer_find", &json!({"maxDepth":"20","maxNodes":"300"}))
            .unwrap(),
        json!({"maxDepth":20,"maxNodes":300})
    );
    assert_eq!(
        normalize_integer_options(
            "computer_set_value",
            &json!({"trailingNewlines":"1","value":"text"})
        )
        .unwrap()["trailingNewlines"],
        1
    );
    assert_eq!(normalize_integer_options("unknown", &args).unwrap(), args);
    for invalid in [
        "",
        " 2",
        "+2",
        "-1",
        "2.0",
        "2;anything",
        "18446744073709551616",
    ] {
        assert!(
            normalize_integer_options("computer_click", &json!({"clickCount":invalid})).is_err()
        );
    }
}

#[test]
fn focused_modal_observation_preserves_host_identity_and_avoids_background_tree() {
    let mut c = context();
    let mut sheet = c.backend.nodes.borrow()[&2].clone();
    sheet.role = "AXSheet".into();
    sheet.name = "Go to Folder".into();
    sheet.value = None;
    c.backend.nodes.borrow_mut().insert(10, sheet);
    let mut path = c.backend.nodes.borrow()[&3].clone();
    path.identifier = "PathTextField".into();
    path.name = String::new();
    c.backend.nodes.borrow_mut().insert(11, path);
    c.backend.child_overrides.borrow_mut().insert(10, vec![11]);
    *c.backend.modal_root.borrow_mut() = Some(10);
    let token = CancellationToken::new();
    let windows = c.run("computer_list_windows", &json!({}), &token).unwrap();
    let window = windows["items"][0]["ref"].as_str().unwrap();
    let snapshot = c
        .run("computer_snapshot", &json!({"windowRef":window}), &token)
        .unwrap();
    assert_eq!(snapshot["windowRef"], window);
    assert_eq!(snapshot["focusedDialog"], true);
    assert_eq!(snapshot["nodes"].as_array().unwrap().len(), 2);
    assert_eq!(snapshot["nodes"][1]["identifier"], "PathTextField");
    assert_eq!(snapshot["nodes"][1]["windowRef"], window);
    let id = snapshot["nodes"][1]["ref"].as_str().unwrap();
    assert_eq!(c.resolve(id).unwrap().window, Some(2));
    *c.backend.modal_root.borrow_mut() = None;
    let document = c
        .run("computer_snapshot", &json!({"windowRef":window}), &token)
        .unwrap();
    assert_eq!(document["focusedDialog"], false);
    assert!(document["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|n| n["role"] == "edit"));
}

#[test]
fn changed_removed_and_expired_controls_are_never_rebound_to_old_handles() {
    let mut c = context();
    let w = window(&mut c);
    let old = edit(&mut c, &w);
    c.backend.nodes.borrow_mut().get_mut(&3).unwrap().value = Some("changed".into());
    call(&mut c, "computer_snapshot", json!({"windowRef":w})).unwrap();
    assert!(c.resolve(&old).is_err());
    let current = edit(&mut c, &w);
    c.refs.get_mut(&current).unwrap().created = Instant::now() - REF_AGE;
    call(&mut c, "computer_snapshot", json!({"windowRef":w})).unwrap();
    assert!(c.resolve(&current).is_err());
    let current = edit(&mut c, &w);
    c.backend.child_overrides.borrow_mut().insert(2, vec![4, 5]);
    call(&mut c, "computer_snapshot", json!({"windowRef":w})).unwrap();
    assert!(c.resolve(&current).is_err());
}
#[test]
fn write_without_followup_observation_does_not_restore_any_old_control() {
    let mut c = context();
    let w = window(&mut c);
    let id = edit(&mut c, &w);
    let proof = prepare(&mut c, &id);
    call(
        &mut c,
        "computer_set_value",
        json!({"ref":id,"_proof":proof,"value":"new","observe":false}),
    )
    .unwrap();
    assert!(c.refs.values().all(|r| r.kind != "element"));
}
#[test]
fn nested_provider_modifier_arrays_are_bounded_and_still_enum_checked() {
    assert_eq!(
        window_key_input(&json!({"key":"g","modifiers":{"item":{"item":["cmd","shift"]}}}))
            .unwrap()
            .1,
        vec!["cmd", "shift"]
    );
    for value in [
        json!({"item":{"item":{"item":["cmd"]}}}),
        json!({"item":{"item":["shell"]}}),
        json!({"item":{"item":["cmd"],"extra":1}}),
    ] {
        assert!(window_key_input(&json!({"key":"g","modifiers":value})).is_err());
    }
}
fn with_file_panel(kind: &'static str) -> Context<Fake> {
    let c = context();
    *c.backend.file_panel_kind.borrow_mut() = Some(kind);
    for id in 20..=23 {
        c.backend.nodes.borrow_mut().insert(
            id,
            Node {
                enabled: true,
                value: Some(String::new()),
                ..Node::default()
            },
        );
    }
    c
}
#[test]
fn file_panel_prepares_open_and_save_with_no_final_submit_or_reopen() {
    let directory = std::env::temp_dir().join("目录");
    let file = directory.join("文件.txt");
    for kind in ["save", "open"] {
        let c = with_file_panel(kind);
        let r = file_dialog::prepare(
            &c.backend,
            &2,
            file.to_str().unwrap(),
            &CancellationToken::new(),
        )
        .unwrap();
        assert_eq!(r["prepared"], true);
        assert_eq!(r["verified"], false);
        assert_eq!(r["finalActionDispatched"], false);
        let expected = if kind == "save" {
            directory.join("")
        } else {
            file.clone()
        };
        assert_eq!(
            c.backend.nodes.borrow()[&22].value.as_deref(),
            expected.to_str()
        );
        if kind == "save" {
            assert_eq!(
                c.backend.nodes.borrow()[&23].value.as_deref(),
                Some("文件.txt")
            );
        }
        let calls = c.backend.calls.borrow();
        assert_eq!(calls.iter().filter(|v| v.starts_with("key:g:")).count(), 1);
        assert_eq!(
            calls
                .iter()
                .filter(|v| v.starts_with("key:return:"))
                .count(),
            1
        );
        assert!(calls
            .iter()
            .all(|v| !v.starts_with("control_click") && v != "AXPress"));
    }
}
#[test]
fn file_panel_invalid_path_missing_panel_and_pause_never_continue_input() {
    let directory = std::env::temp_dir();
    let file = directory.join("test.txt");
    let path = file.to_str().unwrap();
    for invalid in [
        std::path::PathBuf::from("relative.txt"),
        directory.join("..").join("other.txt"),
        directory.join("a\n.txt"),
    ] {
        let c = with_file_panel("save");
        assert!(file_dialog::prepare(
            &c.backend,
            &2,
            invalid.to_str().unwrap(),
            &CancellationToken::new()
        )
        .unwrap_err()
        .starts_with("invalid_argument:"));
        assert!(c.backend.calls.borrow().is_empty());
    }
    let c = context();
    assert!(
        file_dialog::prepare(&c.backend, &2, path, &CancellationToken::new())
            .unwrap_err()
            .starts_with("unsupported_panel:")
    );
    assert!(c.backend.calls.borrow().is_empty());
    let c = with_file_panel("save");
    let token = CancellationToken::new();
    *c.backend.cancel_after_text.borrow_mut() = Some(token.clone());
    assert!(file_dialog::prepare(&c.backend, &2, path, &token)
        .unwrap_err()
        .contains("cancelled"));
    assert!(!c
        .backend
        .calls
        .borrow()
        .iter()
        .any(|v| v.starts_with("key:return:")));
}
#[test]
fn document_location_or_window_context_change_retires_control_handles() {
    for change_url in [true, false] {
        let mut c = context();
        let w = window(&mut c);
        let old = edit(&mut c, &w);
        if change_url {
            *c.backend.document_location.borrow_mut() = Some("file:///tmp/other.txt".into());
        } else {
            c.backend.nodes.borrow_mut().get_mut(&2).unwrap().name = "different document".into();
        }
        call(&mut c, "computer_snapshot", json!({"windowRef":w})).unwrap();
        assert!(c.resolve(&old).is_err());
    }
}
#[test]
fn single_modifier_transport_and_key_chord_share_the_same_validated_parser() {
    assert_eq!(
        window_key_input(&json!({"key":"down","modifiers":{"item":"cmd"}}))
            .unwrap()
            .1,
        vec!["cmd"]
    );
    assert_eq!(
        window_key_input(&json!({"key":"cmd+down"})).unwrap(),
        ("down", vec!["cmd".into()])
    );
    assert!(window_key_input(&json!({"key":"shell+down"})).is_err());
    assert!(window_key_input(&json!({"key":"cmd+down","modifiers":["cmd"]})).is_err());
}

#[test]
fn action_envelope_and_paged_state_together_fit_the_runtime_budget() {
    let mut c = context();
    {
        let mut nodes = c.backend.nodes.borrow_mut();
        let node = nodes.get_mut(&3).unwrap();
        node.name = "😀".repeat(256);
        node.identifier = "😀".repeat(256);
        node.application = "测试应用".into();
        node.value = Some("😀".repeat(512));
        for id in 10..90 {
            nodes.insert(
                id,
                Node {
                    role: "button".into(),
                    name: format!("target{id}-{}", "中".repeat(250)),
                    enabled: true,
                    actions: vec!["focus".into()],
                    ..Node::default()
                },
            );
        }
    }
    c.backend
        .child_overrides
        .borrow_mut()
        .insert(2, std::iter::once(3).chain(10..90).collect());
    let w = window(&mut c);
    let snapshot = call(&mut c, "computer_snapshot", json!({"windowRef":w})).unwrap();
    let id = snapshot["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["role"] == "edit")
        .unwrap()["ref"]
        .as_str()
        .unwrap()
        .to_string();
    let proof = prepare(&mut c, &id);
    let result = call(
        &mut c,
        "computer_set_value",
        json!({"ref":id,"_proof":proof,"value":"😀".repeat(512)}),
    )
    .unwrap();
    assert!(serde_json::to_vec(&result).unwrap().len() <= paging::MAX_OUTPUT_BYTES);
    assert_eq!(result["executed"], true);
    assert_eq!(result["verified"], true);
    assert!(result.get("stateError").is_none());
    let mut state = result["state"].clone();
    let total = state["totalItems"].as_u64().unwrap() as usize;
    let mut observed = state["nodes"].as_array().unwrap().len();
    while let Some(cursor) = state["nextCursor"].as_str() {
        state = call(&mut c, "computer_continue", json!({"cursor":cursor})).unwrap();
        assert!(serde_json::to_vec(&state).unwrap().len() <= paging::MAX_OUTPUT_BYTES);
        observed += state["nodes"].as_array().unwrap().len();
    }
    assert_eq!(observed, total);
    assert_eq!(
        c.backend
            .calls
            .borrow()
            .iter()
            .filter(|s| s.as_str() == "set")
            .count(),
        1
    );
}

#[test]
fn localized_and_executable_app_aliases_use_the_same_ambiguity_checks() {
    let mut c = context();
    let (name, aliases) = app_names::windows_names(r"C:\Windows\notepad.exe", &["记事本".into()]);
    {
        let mut nodes = c.backend.nodes.borrow_mut();
        let app = nodes.get_mut(&1).unwrap();
        app.name = name;
        app.aliases = aliases;
    }
    for name in ["记事本", "Notepad", "notepad.exe"] {
        let result = call(&mut c, "computer_list_apps", json!({"name":name})).unwrap();
        assert_eq!(result["matchStatus"], "unique", "{name}");
    }
    let missing = call(&mut c, "computer_list_apps", json!({"name":"notepad-plus"})).unwrap();
    assert_eq!(missing["matchStatus"], "not_found");
}

#[test]
fn incomplete_application_discovery_never_claims_absence_or_launches_another_app() {
    let mut c = context();
    c.backend
        .listing_issues
        .borrow_mut()
        .push(("protected provider".into(), "permission_denied".into()));
    let state = call(&mut c, "computer_context", json!({"app":"missing"})).unwrap();
    assert_eq!(state["status"], "app_discovery_incomplete");
    assert_eq!(state["appDiscoveryIssueCount"], 1);
    assert!(c.backend.calls.borrow().is_empty());
}

#[test]
fn application_alias_ambiguity_never_selects_the_first_native_process() {
    let mut c = context();
    let mut app = c.backend.nodes.borrow().get(&1).unwrap().clone();
    app.aliases = vec!["Notepad".into()];
    c.backend.nodes.borrow_mut().insert(1, app.clone());
    c.backend.nodes.borrow_mut().insert(6, app);
    let state = call(&mut c, "computer_context", json!({"app":"notepad"})).unwrap();
    assert_eq!(state["status"], "ambiguous_app");
    assert_eq!(state["candidates"].as_array().unwrap().len(), 2);
    assert!(c.backend.calls.borrow().is_empty());
}

#[test]
fn matching_helpers_inside_the_main_app_bundle_do_not_create_false_ambiguity() {
    for (helper_path, excluded) in [
        ("/Applications/Target.app/Contents/MacOS/TargetEx.app", true),
        ("/Applications/Other.app", false),
        (
            "/Applications/Target.app-neighbor/Contents/TargetEx.app",
            false,
        ),
    ] {
        let mut c = context();
        let mut main = c.backend.nodes.borrow().get(&1).unwrap().clone();
        main.name = "Target UI".into();
        main.identifier = "observed.main".into();
        let mut helper = main.clone();
        helper.identifier = "observed.helper".into();
        c.backend.nodes.borrow_mut().insert(1, main);
        c.backend.nodes.borrow_mut().insert(6, helper);
        c.backend
            .bundles
            .borrow_mut()
            .insert(1, "/Applications/Target.app".into());
        c.backend.bundles.borrow_mut().insert(6, helper_path.into());
        let state = call(&mut c, "computer_context", json!({"app":"Target UI"})).unwrap();
        if excluded {
            assert_ne!(state["status"], "ambiguous_app");
            assert_eq!(
                state["appResolution"]["excludedHelperIdentifiers"],
                json!(["observed.helper"])
            );
        } else {
            assert_eq!(state["status"], "ambiguous_app");
        }
        let explicit = call(&mut c, "computer_context", json!({"app":"observed.helper"})).unwrap();
        assert!(
            explicit.get("appResolution").is_none(),
            "an explicit helper identity remains addressable"
        );
        assert_eq!(
            c.refs[explicit["appRef"].as_str().unwrap()]
                .observed
                .identifier,
            "observed.helper"
        );
    }
}

#[test]
fn application_targeted_text_and_shortcuts_never_activate_a_global_window() {
    let mut c = context();
    *c.backend.background_input.borrow_mut() = true;
    let w = window(&mut c);
    let id = edit(&mut c, &w);
    let proof = prepare(&mut c, &id);
    let output = call(
        &mut c,
        "computer_set_value",
        json!({"ref":id,"_proof":proof,"value":"后台输入\n完整正文","inputMethod":"keyboard"}),
    )
    .unwrap();
    assert_eq!(output["verified"], true);
    let proof = prepare(&mut c, &w);
    let output = call(
        &mut c,
        "computer_key_window",
        json!({"ref":w,"_proof":proof,"shortcut":"cmd+s"}),
    )
    .unwrap();
    assert_eq!(output["keyDispatched"], true);
    assert!(c
        .backend
        .calls
        .borrow()
        .iter()
        .any(|s| s.starts_with("application-focus:")));
    assert!(!c
        .backend
        .calls
        .borrow()
        .iter()
        .any(|s| s.starts_with("activate:")));
}
#[test]
fn unsupported_window_capture_does_not_fall_back_to_display_capture_or_activate() {
    let mut c = context();
    let w = window(&mut c);
    let result = call(&mut c, "computer_capture_window", json!({"ref":w}));
    assert!(result.unwrap_err().starts_with("unsupported_action"));
    assert!(c.backend.calls.borrow().is_empty());
}
#[test]
fn input_lane_is_chosen_per_operation_not_for_the_whole_application() {
    assert!(!foreground_required("computer_snapshot", &json!({})));
    assert!(!foreground_required(
        "computer_set_value",
        &json!({"inputMethod":"value"})
    ));
    assert!(!foreground_required(
        "computer_invoke",
        &json!({"action":"AXPress"})
    ));
    assert!(foreground_required("computer_activate_window", &json!({})));
    assert_eq!(
        foreground_required("computer_key_window", &json!({})),
        !cfg!(target_os = "macos")
    );
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "Creates only disposable native fixture apps; requires an unlocked macOS graphical session and AX/screen-recording consent"]
fn actual_background_roundtrip() {
    use core_graphics::{
        event::CGEvent,
        event_source::{CGEventSource, CGEventSourceStateID},
    };
    let directory = std::env::var("ECHO_NATIVE_BACKGROUND_FIXTURES")
        .expect("Use scripts/test-desktop-background.mjs");
    let directory = std::fs::canonicalize(directory).unwrap();
    assert!(directory
        .file_name()
        .unwrap()
        .to_string_lossy()
        .starts_with("echo-background-native-"));
    let original = macos::foreground_pid().unwrap();
    struct Restore(Option<i32>);
    impl Drop for Restore {
        fn drop(&mut self) {
            if let Some(pid) = self.0 {
                let _ = macos_restore_foreground(pid);
            }
        }
    }
    let _restore = Restore(original);
    let token = CancellationToken::new();
    let wait_file = |name: &str| {
        let end = Instant::now() + Duration::from_secs(8);
        loop {
            if let Ok(value) = std::fs::read_to_string(directory.join(name)) {
                return value;
            }
            assert!(Instant::now() < end, "fixture output missing: {name}");
            std::thread::sleep(Duration::from_millis(80));
        }
    };
    for role in ["target", "other"] {
        let mut launch = std::process::Command::new("/usr/bin/open");
        if role == "target" {
            launch.arg("-g");
        }
        assert!(launch
            .arg("-a")
            .arg(directory.join(format!("{role}.app")))
            .arg("--args")
            .arg(&directory)
            .arg(role)
            .status()
            .unwrap()
            .success());
        wait_file(&format!("{role}-pid"));
    }
    let other_pid = wait_file("other-pid").parse::<i32>().unwrap();
    let end = Instant::now() + Duration::from_secs(3);
    while macos::foreground_pid().unwrap() != Some(other_pid) {
        assert!(Instant::now() < end, "foreground fixture did not activate");
        std::thread::sleep(Duration::from_millis(50));
    }
    let pointer = || {
        CGEvent::new(CGEventSource::new(CGEventSourceStateID::CombinedSessionState).unwrap())
            .unwrap()
            .location()
    };
    super::super::emergency_stop::start(|_| {});
    let monitor_deadline = Instant::now() + Duration::from_secs(1);
    while !super::super::emergency_stop::monitor_available() && Instant::now() < monitor_deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    let monitor_available = super::super::emergency_stop::monitor_available();
    println!("Native input conflict monitor available: {monitor_available}");
    let before_pointer = pointer();
    let mut c = Context {
        backend: Platform::new().unwrap(),
        refs: HashMap::new(),
        approvals: HashMap::new(),
        pages: HashMap::new(),
    };
    let app_id = format!(
        "com.echoagent.fixture.background.target.{}",
        directory
            .file_name()
            .unwrap()
            .to_string_lossy()
            .replace('-', "")
            .to_lowercase()
    );
    let deadline = Instant::now() + Duration::from_secs(6);
    let state = loop {
        let state = c
            .run("computer_context", &json!({"app":app_id}), &token)
            .unwrap();
        if state["windowRef"].is_string() {
            break state;
        }
        assert!(Instant::now() < deadline, "fixture window ref: {state}");
        std::thread::sleep(Duration::from_millis(100));
    };
    let window = state["windowRef"].as_str().unwrap().to_owned();
    let find = |c: &mut Context<Platform>, identifier: &str| {
        let output = c
            .run(
                "computer_find",
                &json!({"windowRef":window,"identifier":identifier}),
                &token,
            )
            .unwrap();
        assert_eq!(output["status"], "unique", "{output}");
        output["candidates"][0]["ref"].as_str().unwrap().to_owned()
    };
    for (method, value) in [
        ("value", "AX-DIRECT"),
        (
            "keyboard",
            "后台输入 Echo🧪 Unicode 0123456789 abcdefghijklmnopqrstuvwxyz 中文完成",
        ),
    ] {
        let field = find(&mut c, "echo-background-input");
        let proof = c.run("_prepare", &json!({"ref":field}), &token).unwrap();
        let result = c
            .run(
                "computer_set_value",
                &json!({"ref":field,"_proof":proof["proof"],"value":value,"inputMethod":method}),
                &token,
            )
            .unwrap();
        assert_eq!(result["verified"], true, "{method}: {result}");
        assert_eq!(
            macos::foreground_pid().unwrap(),
            Some(other_pid),
            "background text stole focus"
        );
    }
    let proof = c.run("_prepare", &json!({"ref":window}), &token).unwrap();
    let result = c
        .run(
            "computer_key_window",
            &json!({"ref":window,"_proof":proof["proof"],"shortcut":"cmd+shift+k"}),
            &token,
        )
        .unwrap();
    assert_eq!(result["keyDispatched"], true, "{result}");
    assert_eq!(wait_file("target-key"), "KEY-RECEIVED");
    let field = find(&mut c, "echo-background-input");
    let proof = c.run("_prepare", &json!({"ref":field}), &token).unwrap();
    let result = c
        .run(
            "computer_click_control",
            &json!({"ref":field,"_proof":proof["proof"],"clickCount":1}),
            &token,
        )
        .unwrap();
    assert!(
        result["error"]
            .as_str()
            .is_some_and(|error| error.contains("background_pointer_unavailable")),
        "{result}"
    );
    assert_ne!(result["executed"], true);
    assert!(
        !directory.join("target-click").exists(),
        "unsupported pointer input was dispatched"
    );
    assert!(
        !directory.join("target-mouse-event").exists(),
        "unsupported mouse event reached the app"
    );
    let button = find(&mut c, "echo-background-commit");
    let proof = c.run("_prepare", &json!({"ref":button}), &token).unwrap();
    let result = c
        .run(
            "computer_click_control",
            &json!({"ref":button,"_proof":proof["proof"],"clickCount":1}),
            &token,
        )
        .unwrap();
    assert_eq!(result["executed"], true, "{result}");
    assert_eq!(
        wait_file("target-commit"),
        "后台输入 Echo🧪 Unicode 0123456789 abcdefghijklmnopqrstuvwxyz 中文完成"
    );
    assert!(
        !directory.join("other-change").exists(),
        "foreground text was changed"
    );
    assert_eq!(
        macos::foreground_pid().unwrap(),
        Some(other_pid),
        "background action stole focus"
    );
    let after_pointer = pointer();
    assert_eq!(
        (after_pointer.x, after_pointer.y),
        (before_pointer.x, before_pointer.y),
        "background action moved system pointer"
    );
    let capture = c
        .run("computer_capture_window", &json!({"ref":window}), &token)
        .unwrap();
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(capture["data"].as_str().unwrap())
        .unwrap();
    let image = image::load_from_memory(&bytes).unwrap().to_rgba8();
    let red = image
        .pixels()
        .filter(|p| p[0] > 130 && p[1] < 100 && p[2] < 100)
        .count();
    assert!(
        red as f64 / f64::from(image.width() * image.height()) > 0.3,
        "capture did not contain the occluded target's red background"
    );
    std::fs::write(directory.join("target-window.png"), bytes).unwrap();
    std::fs::write(directory.join("evidence.json"),json!({"backgroundValue":true,"backgroundKeyboard":true,"backgroundShortcut":true,"backgroundPress":true,"backgroundPointer":false,"unsupportedPointerRejected":true,"inputMonitorAvailable":monitor_available,"foregroundUnchanged":true,"pointerUnchanged":true,"occludedWindowCapture":true}).to_string()).unwrap();
    println!(
        "Real isolated native background checks passed: {}",
        directory.display()
    );
}
