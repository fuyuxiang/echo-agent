//! Pure merge rules for `[ui].combine_queued_prompts`.
//!
//! Pager and shell keep separate call sites (local drain vs promote) but share
//! eligibility, join, and content-meta stamping so stop conditions cannot drift.

use crate::COMBINED_DISPLAY_TEXTS_META;

/// Separator between original follow-ups in the joined model body.
pub const TEXT_SEPARATOR: &str = "\n\n";

/// Adapter-filled gate for one queue row.
#[derive(Debug, Clone, Copy)]
pub struct CombineGate<'a> {
    pub id: &'a str,
    /// Plain user prompt (`kind == "prompt"`), not bash/command/cron.
    pub is_plain_prompt: bool,
    /// Synthetic / auto-wake origins never combine.
    pub is_synthetic: bool,
    /// Client-expanded skill payload (`displayText` meta).
    pub is_expanded_skill: bool,
    /// Bash command (meta or kind).
    pub is_bash: bool,
    /// Followers must have no images; front may keep its own.
    pub has_images: bool,
    /// Non-empty display / body text required to participate.
    pub text: &'a str,
}

/// Front of a combine run: plain user prompt; may carry images.
pub fn can_merge_front(g: &CombineGate<'_>) -> bool {
    g.is_plain_prompt && !g.is_synthetic && !g.is_expanded_skill && !g.is_bash && !g.text.is_empty()
}

/// Follower: same as front, no images, and not under edit hold.
pub fn can_merge_follower(g: &CombineGate<'_>, skip_ids: &[&str]) -> bool {
    can_merge_front(g) && !g.has_images && !skip_ids.contains(&g.id)
}

/// Length of the mergeable prefix (including front). `1` means take front only.
/// `0` if `items` is empty.
pub fn combine_prefix_len<'a>(
    items: impl IntoIterator<Item = CombineGate<'a>>,
    skip_ids: &[&str],
) -> usize {
    let mut iter = items.into_iter();
    let Some(front) = iter.next() else {
        return 0;
    };
    if !can_merge_front(&front) {
        return 1;
    }
    let mut n = 1;
    for next in iter {
        if !can_merge_follower(&next, skip_ids) {
            break;
        }
        n += 1;
    }
    n
}

pub fn join_texts<'a>(texts: impl IntoIterator<Item = &'a str>) -> String {
    texts
        .into_iter()
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join(TEXT_SEPARATOR)
}

/// Multi-bubble UI when at least two original prompts were merged.
#[inline]
pub fn is_combined(segs: &[String]) -> bool {
    segs.len() >= 2
}

/// Stamp [`COMBINED_DISPLAY_TEXTS_META`] when `segs.len() >= 2`.
pub fn stamp_combined_display_texts(
    meta: &mut serde_json::Map<String, serde_json::Value>,
    segs: &[String],
) {
    if !is_combined(segs) {
        return;
    }
    meta.insert(
        COMBINED_DISPLAY_TEXTS_META.to_string(),
        serde_json::Value::Array(
            segs.iter()
                .cloned()
                .map(serde_json::Value::String)
                .collect(),
        ),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain<'a>(id: &'a str, text: &'a str) -> CombineGate<'a> {
        CombineGate {
            id,
            is_plain_prompt: true,
            is_synthetic: false,
            is_expanded_skill: false,
            is_bash: false,
            has_images: false,
            text,
        }
    }

    #[test]
    fn three_plain_prompts_merge() {
        let items = [plain("a", "one"), plain("b", "two"), plain("c", "three")];
        assert_eq!(combine_prefix_len(items, &[]), 3);
        assert_eq!(join_texts(["one", "two", "three"]), "one\n\ntwo\n\nthree");
    }

    #[test]
    fn stops_at_bash() {
        let bash = CombineGate {
            id: "bash",
            is_plain_prompt: true,
            is_synthetic: false,
            is_expanded_skill: false,
            is_bash: true,
            has_images: false,
            text: "ls",
        };
        let items = [
            plain("a", "one"),
            plain("b", "two"),
            bash,
            plain("c", "three"),
        ];
        assert_eq!(combine_prefix_len(items, &[]), 2);
    }

    #[test]
    fn stops_at_non_prompt_kind() {
        let cmd = CombineGate {
            id: "cmd",
            is_plain_prompt: false,
            is_synthetic: false,
            is_expanded_skill: false,
            is_bash: false,
            has_images: false,
            text: "/compact",
        };
        assert_eq!(
            combine_prefix_len([plain("a", "one"), plain("b", "two"), cmd], &[]),
            2
        );
    }

    #[test]
    fn stops_at_expanded_skill() {
        let skill = CombineGate {
            id: "sk",
            is_plain_prompt: true,
            is_synthetic: false,
            is_expanded_skill: true,
            is_bash: false,
            has_images: false,
            text: "/commit",
        };
        assert_eq!(
            combine_prefix_len([plain("a", "one"), skill, plain("b", "two")], &[]),
            1
        );
    }

    #[test]
    fn stops_at_image_follower() {
        let img = CombineGate {
            id: "img",
            is_plain_prompt: true,
            is_synthetic: false,
            is_expanded_skill: false,
            is_bash: false,
            has_images: true,
            text: "see",
        };
        assert_eq!(
            combine_prefix_len([plain("a", "one"), plain("b", "two"), img], &[]),
            2
        );
        // Front may keep images.
        let front_img = CombineGate {
            id: "f",
            is_plain_prompt: true,
            is_synthetic: false,
            is_expanded_skill: false,
            is_bash: false,
            has_images: true,
            text: "with image",
        };
        assert_eq!(combine_prefix_len([front_img, plain("b", "two")], &[]), 2);
    }

    #[test]
    fn skips_row_under_edit() {
        assert_eq!(
            combine_prefix_len(
                [
                    plain("a", "one"),
                    plain("edit", "draft"),
                    plain("c", "three")
                ],
                &["edit"],
            ),
            1
        );
        assert_eq!(
            combine_prefix_len(
                [plain("a", "one"), plain("b", "two"), plain("edit", "draft")],
                &["edit"],
            ),
            2
        );
    }

    #[test]
    fn stamp_only_when_multi() {
        let mut meta = serde_json::Map::new();
        stamp_combined_display_texts(&mut meta, &["only".into()]);
        assert!(meta.is_empty());
        stamp_combined_display_texts(&mut meta, &["a".into(), "b".into()]);
        assert_eq!(
            meta.get(COMBINED_DISPLAY_TEXTS_META),
            Some(&serde_json::json!(["a", "b"]))
        );
    }

    #[test]
    fn ineligible_front_is_taken_alone() {
        let bash = CombineGate {
            id: "b",
            is_plain_prompt: true,
            is_synthetic: false,
            is_expanded_skill: false,
            is_bash: true,
            has_images: false,
            text: "pwd",
        };
        assert_eq!(combine_prefix_len([bash, plain("a", "x")], &[]), 1);
    }
}
