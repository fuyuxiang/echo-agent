//! Window identity is distinct from optional provider keyboard focus.
use serde_json::{json, Value};

pub fn from_observation(value: &Value) -> Option<Value> {
    let value = if value["windowRef"].is_string() {
        value
    } else {
        &value["state"]
    };
    let application = value["application"].as_str().or_else(|| {
        value["nodes"]
            .as_array()?
            .iter()
            .find_map(|node| node["application"].as_str())
    });
    Some(json!({"windowRef":value["windowRef"].as_str()?,
        "application":application,"identifier":value["identifier"]}))
}

pub fn same_window(before: &Value, after: &Value) -> bool {
    before["windowRef"].is_string()
        && [
            "windowRef",
            "applicationIdentifier",
            "windowBounds",
            "documentUrl",
            "processIds",
        ]
        .iter()
        .all(|key| before[*key] == after[*key])
}

pub fn process_ids(value: &Value) -> Vec<u32> {
    value["processIds"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_u64().and_then(|n| u32::try_from(n).ok()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_focusless_window_stays_bound_but_window_changes_never_pass() {
        let before = json!({"windowRef":"w1","applicationIdentifier":"app","windowBounds":[0,0,300,200],"documentUrl":null,"processIds":[12],"focusRef":null});
        let mut current = before.clone();
        current["focusRef"] = json!("new_input");
        assert!(same_window(&before, &current));
        for (key, value) in [
            ("windowRef", json!("other")),
            ("applicationIdentifier", json!("other")),
            ("windowBounds", json!([1, 0, 300, 200])),
            ("documentUrl", json!("other")),
            ("processIds", json!([13])),
        ] {
            let mut changed = current.clone();
            changed[key] = value;
            assert!(!same_window(&before, &changed), "{key}");
        }
        assert!(!same_window(&json!({}), &json!({})));
    }
}
