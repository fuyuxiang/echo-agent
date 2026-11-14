//! One bounded observation for app discovery, windows, commands and active UI.
//! Actions attach this state; it never grants execution or replays an action.
use super::*;

impl<B: Native> Context<B> {
    pub(super) fn desktop_context(
        &mut self,
        args: &Value,
        token: &CancellationToken,
    ) -> Result<Value, String> {
        let mut excluded_helpers = vec![];
        if let Some(id) = args.get("windowRef").and_then(Value::as_str) {
            let window = self.resolve(id)?;
            if window.kind != "window" {
                return Err("invalid_argument: windowRef must be a window".into());
            }
            let app = self
                .backend
                .application_element(&window.element)?
                .ok_or("unsupported: app identity unavailable")?;
            return self.context_for_app(app, Some(id), token, true);
        }
        let app = if let Some(id) = args.get("appRef").and_then(Value::as_str) {
            let r = self.resolve(id)?;
            if r.kind != "app" {
                return Err("invalid_argument: appRef must be an application".into());
            }
            r.element
        } else {
            let query = string(args, "app")?;
            let mut apps = self.backend.apps()?;
            apps.items
                .retain(|(_, n)| matches(n, &json!({"name":query})) || n.identifier == query);
            if apps.items.len() > 1 {
                let bundles: Vec<_> = apps
                    .items
                    .iter()
                    .map(|(e, _)| self.backend.application_bundle_path(e))
                    .collect();
                let excluded: Vec<_> = bundles
                    .iter()
                    .enumerate()
                    .map(|(i, child)| {
                        child.as_ref().is_some_and(|child| {
                            bundles.iter().enumerate().any(|(j, host)| {
                                i != j
                                    && host.as_ref().is_some_and(|host| {
                                        host.extension().is_some_and(|e| e == "app")
                                            && child.starts_with(host.join("Contents"))
                                    })
                            })
                        })
                    })
                    .collect();
                let mut index = 0;
                apps.items.retain(|(_, node)| {
                    let keep = !excluded[index];
                    index += 1;
                    if !keep {
                        excluded_helpers.push(node.identifier.clone());
                    }
                    keep
                });
            }
            if apps.items.len() != 1 {
                return Ok(
                    json!({"status":if apps.items.is_empty(){if apps.issues.is_empty(){"app_not_found"}else{"app_discovery_incomplete"}}else{"ambiguous_app"},"appDiscoveryIssueCount":apps.issues.len(),"appDiscoveryIssues":apps.issues.iter().take(4).map(|(app,error)|json!({"application":app.chars().take(64).collect::<String>(),"error":error.chars().take(160).collect::<String>()})).collect::<Vec<_>>(),"nodes":[],"candidates":apps.items.iter().map(|(_,n)|json!({"name":n.name,"identifier":n.identifier,"aliases":n.aliases})).collect::<Vec<_>>(),"hint":if apps.items.is_empty()&&!apps.issues.is_empty(){"Application discovery was incomplete; absence is not proven. Inspect computer_list_apps with the exact name/identifier or an already observed appRef before launching another instance."}else if apps.items.is_empty(){"No running application matches this name, executable alias or identifier. Do not repeat global discovery. Open the requested installed app with computer_open_app, then call computer_context once. App activation does not create a document."}else{"Use an exact observed app name, bundle/executable alias or identifier; never choose the first ambiguous match."}}),
                );
            }
            apps.items.remove(0).0
        };
        let mut result = self.context_for_app(app, None, token, true)?;
        if !excluded_helpers.is_empty() {
            result["appResolution"] = json!({"basis":"matching_main_bundle_contains_helpers","excludedHelperIdentifiers":excluded_helpers});
        }
        if args.get("app").is_some() {
            result["textEntryGuidance"]=json!("Prefer computer_set_value(inputMethod=keyboard) for user text in focusable form, filename and directory fields. Native value readback alone does not prove application change handlers ran. If dependent UI stays unchanged, enter using keyboard before confirming; do not repeat an ineffective confirm/open action.");
            result["documentCreationGuidance"]=json!("An empty/untitled window is not proof of creating a new document. If the user requests a new document, execute the observed New command once in this task before input, even when app launch shows an empty document. Do not reuse a restored or existing document. After that command, use its returned state directly. Before the first Save, compare the requested file format with the observed document/menu state and set the required format using actual controls/commands. Do not open Save just to discover a format mismatch and cancel/reopen it.");
        }
        Ok(result)
    }

    pub(super) fn context_for_app(
        &mut self,
        app: B::Element,
        preferred: Option<&str>,
        token: &CancellationToken,
        commands: bool,
    ) -> Result<Value, String> {
        if token.is_cancelled() {
            return Err("cancelled: observation stopped".into());
        }
        self.backend.validate(&app, None)?;
        let mut node = self.backend.inspect(&app)?;
        node.role = "application".into();
        let app_info = self.insert(Reference {
            element: app.clone(),
            window: None,
            window_ref: None,
            kind: "app",
            observed: redact(node),
            observed_document: None,
            approved_window: None,
            created: Instant::now(),
        })?;
        let app_id = app_info["ref"].as_str().unwrap().to_owned();
        let listing = self.backend.windows(Some(&app))?;
        // An app's own focused window remains meaningful while another app is
        // foreground. Do not mislabel it as global foreground or guess by order.
        let app_window = self.backend.application_window(&app).ok().flatten();
        let mut app_window_ref = None;
        let mut windows = Vec::new();
        for (element, node) in listing.items.into_iter().take(64) {
            let app_selected = app_window
                .as_ref()
                .is_some_and(|w| self.backend.same_element(w, &element));
            let info = self.insert(Reference {
                element,
                window: None,
                window_ref: None,
                kind: "window",
                observed: redact(node),
                observed_document: None,
                approved_window: None,
                created: Instant::now(),
            })?;
            if app_selected {
                app_window_ref = info["ref"].as_str().map(str::to_owned);
            }
            windows.push(json!({"ref":info["ref"],"name":info["name"].as_str().unwrap_or("").chars().take(100).collect::<String>(),"focused":info["focused"]}));
        }
        let focused: Vec<_> = windows.iter().filter(|w| w["focused"] == true).collect();
        let selected = preferred
            .and_then(|id| windows.iter().find(|w| w["ref"] == id))
            .or_else(|| (focused.len() == 1).then(|| focused[0]))
            .or_else(|| {
                app_window_ref
                    .as_ref()
                    .and_then(|id| windows.iter().find(|w| w["ref"] == *id))
            })
            .or_else(|| (windows.len() == 1).then(|| &windows[0]))
            .cloned();
        let window_count = windows.len();
        // Keep the selected window visible even when an app owns many windows.
        if let Some(window) = &selected {
            windows.retain(|w| w["ref"] != window["ref"]);
            windows.insert(0, window.clone());
        }
        windows.truncate(8);
        let issue_count = listing.issues.len();
        let mut result = json!({"appRef":app_id,"application":app_info["name"],"identifier":self.backend.application_identifier(&app),"windows":windows,"windowCount":window_count,"windowsTruncated":window_count>8,"nodes":[],"status":"no_focused_window","windowDiscoveryIssues":listing.issues.into_iter().take(4).collect::<Vec<_>>(),"windowDiscoveryIssueCount":issue_count});
        if commands {
            match self.backend.commands(&app) {
                Ok(Some(mut items)) => {
                    // Prefer direct menu commands before deeply nested menus.
                    items.sort_by_key(|c| c["label"].as_str().unwrap_or("").matches(" > ").count());
                    let count = items.len();
                    let mut bytes = 0;
                    items.retain(|c| {
                        let size = serde_json::to_vec(c).unwrap().len();
                        if bytes + size > 4500 {
                            return false;
                        }
                        bytes += size;
                        true
                    });
                    result["commandsTruncated"] = json!(items.len() < count);
                    result["commandsCoverage"] =
                        json!("observed_supported_keyboard_shortcuts_only");
                    result["commands"] = json!(items);
                    result["commandsWindowRef"] = selected
                        .as_ref()
                        .map(|w| w["ref"].clone())
                        .unwrap_or(Value::Null);
                    result["commandsForegroundVerified"] =
                        json!(selected.as_ref().is_some_and(|w| w["focused"] == true));
                    result["commandsSupported"] = json!(true);
                }
                Ok(None) => result["commandsSupported"] = json!(false),
                Err(e) => result["commandsError"] = json!(e),
            }
        }
        if let Some(window) = selected {
            let id = window["ref"].as_str().unwrap();
            let snapshot = self.observe(
                id,
                &json!({"maxDepth":12,"maxNodes":100,"maxText":10000}),
                token,
                true,
            )?;
            for key in [
                "windowRef",
                "documentUrl",
                "documentUrlError",
                "focusedDialog",
                "observationRootRole",
                "snapshotId",
                "status",
                "truncated",
                "treeTruncated",
                "metadataIncomplete",
                "issues",
                "issueCount",
            ] {
                result[key] = snapshot[key].clone();
            }
            let mut nodes = snapshot["nodes"]
                .as_array()
                .unwrap()
                .iter()
                .map(|n| {
                    let mut item = serde_json::Map::new();
                    for key in [
                        "ref",
                        "parentRef",
                        "role",
                        "name",
                        "identifier",
                        "value",
                        "actions",
                        "enabled",
                        "focused",
                        "selected",
                        "protected",
                        "writable",
                        "textTruncated",
                        "childrenTruncated",
                        "observationErrors",
                    ] {
                        if let Some(v) = n.get(key) {
                            let default_false = matches!(
                                key,
                                "focused"
                                    | "protected"
                                    | "writable"
                                    | "textTruncated"
                                    | "childrenTruncated"
                            ) && v == &json!(false);
                            if !v.is_null()
                                && (key == "value" || v != &json!(""))
                                && v != &json!([])
                                && !default_false
                            {
                                item.insert(key.into(), v.clone());
                            }
                        }
                    }
                    Value::Object(item)
                })
                .collect::<Vec<_>>();
            let observed_count = nodes.len();
            nodes.retain(|n| {
                n["protected"] == true
                    || n["writable"] == true
                    || n["focused"] == true
                    || n["selected"] == true
                    || matches!(
                        n["role"].as_str().unwrap_or(""),
                        "AXWindow"
                            | "AXSheet"
                            | "AXWebArea"
                            | "AXTextField"
                            | "AXTextArea"
                            | "AXComboBox"
                            | "AXButton"
                            | "AXPopUpButton"
                            | "AXCheckBox"
                            | "AXRadioButton"
                            | "AXSlider"
                            | "AXMenuItem"
                            | "AXLink"
                            | "edit"
                            | "document"
                            | "button"
                            | "combo_box"
                            | "check_box"
                            | "radio_button"
                            | "hyperlink"
                            | "menu_item"
                            | "window"
                    )
                    || n["value"].as_str().is_some_and(|s| !s.is_empty())
                    || (matches!(n["role"].as_str(), Some("AXStaticText" | "text"))
                        && n["name"].as_str().is_some_and(|s| !s.is_empty()))
                    || n["actions"].as_array().is_some_and(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .any(|a| !["focus", "AXShowMenu", "AXScrollToVisible"].contains(&a))
                    })
            });
            // Inputs and actionable targets precede decorative containers. Keep
            // parentRef relationships, explicit coverage and every observed node.
            nodes.sort_by_key(|n| {
                if n["protected"] == true {
                    0
                } else if matches!(
                    n["role"].as_str().unwrap_or(""),
                    "AXTextField" | "AXTextArea" | "AXComboBox" | "edit" | "document" | "combo_box"
                ) {
                    1
                } else if n["focused"] == true || n["selected"] == true {
                    2
                } else if n["actions"].as_array().is_some_and(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .any(|a| !["focus", "AXShowMenu", "AXScrollToVisible"].contains(&a))
                }) {
                    3
                } else if n["value"].as_str().is_some_and(|s| !s.is_empty())
                    || matches!(
                        n["role"].as_str(),
                        Some("AXWebArea" | "AXWindow" | "AXSheet")
                    )
                {
                    4
                } else {
                    5
                }
            });
            result["nodeOrder"] = json!(
                "inputs_then_actions_then_content_then_structure; parentRef defines hierarchy"
            );
            result["structuralNodesOmitted"] = json!(observed_count - nodes.len());
            result["coverage"] = json!("compact_provider_controls; empty structural containers omitted, parentRef may be outside this view; use snapshot only if structural context is needed");
            result["nodes"] = json!(nodes);
            let inputs = nodes
                .iter()
                .filter(|n| {
                    n["writable"] == true
                        || n["actions"]
                            .as_array()
                            .is_some_and(|a| a.iter().any(|a| a == "focus"))
                })
                .count();
            let named_actions = nodes
                .iter()
                .filter(|n| {
                    n["name"].as_str().is_some_and(|n| !n.is_empty())
                        && n["actions"]
                            .as_array()
                            .is_some_and(|a| a.iter().any(|a| a == "AXPress" || a == "invoke"))
                })
                .count();
            result["interaction"] = json!({"nativeTextInputs":inputs,"namedNativeActions":named_actions,"visualFallbackRecommended":inputs==0 && named_actions==0,"guidance":if inputs==0 && named_actions==0 {"Only window chrome is exposed. Use computer_capture_window for visual observation, then explicitly activate this window and computer_screenshot(windowRef) for protected foreground input. Do not repeat native control searches."}else{"Prefer the exposed native controls and observed menu commands."}});

            result["nodeDefaults"] = json!({"focused":false,"protected":false,"writable":false,"textTruncated":false,"childrenTruncated":false});
        }
        Ok(result)
    }
}
