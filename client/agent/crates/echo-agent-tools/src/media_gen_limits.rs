//! Per-batch caps for media-generation tools (per tool name, not shared).
//! Hosts apply [`partition_media_gen_batch`] before prepare/dispatch.
//!
//! Modest over-cap: first K of that name keep model order; the tail rejects.
//! Egregious over-cap (`total >= 2 * max`) is a host resample signal.

use std::collections::HashMap;

use crate::types::tool::ToolKind;

pub const DEFAULT_MAX_PARALLEL_IMAGE_GEN: usize = 8;
pub const DEFAULT_MAX_PARALLEL_VIDEO_GEN: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaGenBatchLimits {
    pub max_image: usize,
    pub max_video: usize,
}

impl Default for MediaGenBatchLimits {
    fn default() -> Self {
        Self {
            max_image: DEFAULT_MAX_PARALLEL_IMAGE_GEN,
            max_video: DEFAULT_MAX_PARALLEL_VIDEO_GEN,
        }
    }
}

pub fn max_calls_per_batch(kind: ToolKind, limits: &MediaGenBatchLimits) -> Option<usize> {
    // Exhaustive: a new media-like kind must pick a budget (or explicit None).
    match kind {
        ToolKind::ImageGen => Some(limits.max_image),
        ToolKind::VideoGen | ToolKind::ImageToVideo | ToolKind::ReferenceToVideo => {
            Some(limits.max_video)
        }
        ToolKind::Read
        | ToolKind::Edit
        | ToolKind::Delete
        | ToolKind::ListDir
        | ToolKind::Write
        | ToolKind::Move
        | ToolKind::Search
        | ToolKind::Lsp
        | ToolKind::Execute
        | ToolKind::Plan
        | ToolKind::WebSearch
        | ToolKind::WebFetch
        | ToolKind::BackgroundTaskAction
        | ToolKind::WaitTasksAction
        | ToolKind::KillTaskAction
        | ToolKind::List
        | ToolKind::Skill
        | ToolKind::MemorySearch
        | ToolKind::MemoryGet
        | ToolKind::Task
        | ToolKind::EnterPlan
        | ToolKind::ExitPlan
        | ToolKind::AskUser
        | ToolKind::DeployApp
        | ToolKind::InitOrUpdateApp
        | ToolKind::SearchTool
        | ToolKind::UseTool
        | ToolKind::Monitor
        | ToolKind::GoalUpdate
        | ToolKind::Workflow
        | ToolKind::Other => None,
    }
}

/// A tool name whose count in one model step exceeds its per-name cap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaGenOverCap {
    pub name: String,
    pub total: usize,
    pub max: usize,
}

impl MediaGenOverCap {
    /// Spam-sized: at least twice the per-name cap (e.g. 16 `image_gen` when max is 8).
    pub fn is_egregious(&self) -> bool {
        self.max > 0 && self.total >= self.max.saturating_mul(2)
    }
}

/// Names (with totals) that exceed their per-name cap. Empty = admit the batch.
pub fn over_cap_by_name<'a>(
    calls: impl IntoIterator<Item = (&'a str, Option<ToolKind>)>,
    limits: &MediaGenBatchLimits,
) -> Vec<MediaGenOverCap> {
    let mut total_by_name: HashMap<String, (usize, usize)> = HashMap::new();
    for (name, kind) in calls {
        let Some(max) = kind.and_then(|k| max_calls_per_batch(k, limits)) else {
            continue;
        };
        let entry = total_by_name.entry(name.to_owned()).or_insert((0, max));
        entry.0 += 1;
    }
    let mut over: Vec<MediaGenOverCap> = total_by_name
        .into_iter()
        .filter_map(|(name, (total, max))| {
            (total > max).then_some(MediaGenOverCap { name, total, max })
        })
        .collect();
    over.sort_by(|a, b| a.name.cmp(&b.name));
    over
}

/// First over-cap at 2× (or more) resamples; later over-caps use first-K.
pub fn should_resample_egregious(
    over: &[MediaGenOverCap],
    resamples_used: u32,
    max_resamples: u32,
) -> bool {
    resamples_used < max_resamples && over.iter().any(MediaGenOverCap::is_egregious)
}

/// Reminder after a discarded 2× burst: re-read the user ask; stay under the cap.
pub fn resample_reminder(egregious: &[MediaGenOverCap]) -> String {
    debug_assert!(!egregious.is_empty());
    let names = join_backticked_names(egregious.iter().map(|o| o.name.as_str()));
    let limit = match egregious {
        [one] => format!("the max limit for {names} is {}", one.max),
        many if many.iter().all(|o| o.max == many[0].max) => {
            format!("the max limit for {names} is {}", many[0].max)
        }
        many => {
            let parts = many
                .iter()
                .map(|o| format!("`{}` is {}", o.name, o.max))
                .collect::<Vec<_>>()
                .join(" and the max limit for ");
            format!("the max limit for {parts}")
        }
    };
    format!(
        "Consider the user message carefully for your future responses and tool calls. \
         If you are considering making {names} calls, {limit} in one parallel set of calls. \
         Larger requests will be rejected."
    )
}

fn join_backticked_names<'a>(names: impl IntoIterator<Item = &'a str>) -> String {
    let names: Vec<&str> = names.into_iter().collect();
    match names.as_slice() {
        [] => String::new(),
        [one] => format!("`{one}`"),
        [a, b] => format!("`{a}` and `{b}`"),
        names => match names.split_last() {
            Some((last, rest)) if !rest.is_empty() => format!(
                "{}, and `{last}`",
                rest.iter()
                    .map(|n| format!("`{n}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            _ => String::new(),
        },
    }
}

/// Per-name first-K. At or under max: all allow. Over max: the first `max`
/// of that name (model order) allow; the tail rejects. Relative order
/// preserved within each side for tool_use/tool_result pairing.
pub fn partition_media_gen_batch<T>(
    calls: impl IntoIterator<Item = T>,
    tool_name: impl Fn(&T) -> &str,
    tool_kind: impl Fn(&T) -> Option<ToolKind>,
    limits: &MediaGenBatchLimits,
) -> (Vec<T>, Vec<(T, String)>) {
    let calls: Vec<T> = calls.into_iter().collect();

    let mut total_by_name: HashMap<String, usize> = HashMap::new();
    for call in &calls {
        if tool_kind(call)
            .and_then(|k| max_calls_per_batch(k, limits))
            .is_none()
        {
            continue;
        }
        *total_by_name.entry(tool_name(call).to_owned()).or_default() += 1;
    }

    let mut seen_by_name: HashMap<String, usize> = HashMap::new();
    let mut allowed = Vec::with_capacity(calls.len());
    let mut rejected = Vec::new();

    for call in calls {
        let Some(max) = tool_kind(&call).and_then(|k| max_calls_per_batch(k, limits)) else {
            allowed.push(call);
            continue;
        };
        let name = tool_name(&call).to_owned();
        let total = total_by_name.get(&name).copied().unwrap_or(0);
        let seen = seen_by_name.entry(name.clone()).or_default();
        *seen += 1;
        if *seen <= max {
            allowed.push(call);
        } else {
            rejected.push((
                call,
                format!(
                    "Rejected: at most {max} `{name}` tool calls are allowed in a single model step \
                     (this batch had {total}). This extra call was skipped. \
                     If you still need more, make a new step with at most {max} `{name}` call(s)."
                ),
            ));
        }
    }

    (allowed, rejected)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct Call {
        name: &'static str,
        id: u32,
    }

    fn kind_for(name: &str) -> Option<ToolKind> {
        match name {
            "image_gen" | "image_edit" => Some(ToolKind::ImageGen),
            "image_to_video" => Some(ToolKind::ImageToVideo),
            "reference_to_video" => Some(ToolKind::ReferenceToVideo),
            "video_gen" => Some(ToolKind::VideoGen),
            "read_file" => Some(ToolKind::Read),
            _ => None,
        }
    }

    fn partition(
        calls: Vec<Call>,
        limits: &MediaGenBatchLimits,
    ) -> (Vec<Call>, Vec<(Call, String)>) {
        partition_media_gen_batch(calls, |c| c.name, |c| kind_for(c.name), limits)
    }

    #[test]
    fn uncapped_tools_always_pass() {
        let limits = MediaGenBatchLimits::default();
        let calls: Vec<Call> = (0..20)
            .map(|id| Call {
                name: "read_file",
                id,
            })
            .collect();
        let (allowed, rejected) = partition(calls.clone(), &limits);
        assert_eq!(allowed, calls);
        assert!(rejected.is_empty());
    }

    #[test]
    fn under_limit_all_allowed() {
        let limits = MediaGenBatchLimits::default();
        let calls: Vec<Call> = (0..limits.max_image as u32)
            .map(|id| Call {
                name: "image_gen",
                id,
            })
            .collect();
        let (allowed, rejected) = partition(calls.clone(), &limits);
        assert_eq!(allowed, calls);
        assert!(rejected.is_empty());
    }

    #[test]
    fn over_limit_keeps_first_k_rejects_tail() {
        let limits = MediaGenBatchLimits::default();
        let total = limits.max_image + 2;
        let calls: Vec<Call> = (0..total as u32)
            .map(|id| Call {
                name: "image_gen",
                id,
            })
            .collect();
        let (allowed, rejected) = partition(calls, &limits);
        assert_eq!(
            allowed.iter().map(|c| c.id).collect::<Vec<_>>(),
            (0..limits.max_image as u32).collect::<Vec<_>>()
        );
        assert_eq!(rejected.len(), 2);
        assert!(rejected[0].1.contains(&format!(
            "at most {} `image_gen` tool calls are allowed",
            limits.max_image
        )));
        assert!(rejected[0].1.contains(&format!("this batch had {total}")));
        assert!(rejected[0].1.contains("This extra call was skipped"));
    }

    #[test]
    fn image_gen_and_image_edit_have_independent_budgets() {
        let limits = MediaGenBatchLimits::default();
        let mut calls = Vec::new();
        for id in 0..limits.max_image as u32 {
            calls.push(Call {
                name: "image_gen",
                id,
            });
        }
        for id in 100..(100 + limits.max_image as u32) {
            calls.push(Call {
                name: "image_edit",
                id,
            });
        }
        let (allowed, rejected) = partition(calls, &limits);
        assert_eq!(allowed.len(), limits.max_image * 2);
        assert!(rejected.is_empty());
    }

    #[test]
    fn over_limit_media_keeps_first_k_and_siblings() {
        let limits = MediaGenBatchLimits {
            max_image: 8,
            max_video: 4,
        };
        let mut calls = vec![Call {
            name: "read_file",
            id: 0,
        }];
        for id in 1..=9 {
            calls.push(Call {
                name: "image_gen",
                id,
            });
        }
        calls.push(Call {
            name: "grep",
            id: 10,
        });
        let (allowed, rejected) = partition(calls, &limits);
        assert_eq!(
            allowed.iter().map(|c| c.id).collect::<Vec<_>>(),
            vec![0, 1, 2, 3, 4, 5, 6, 7, 8, 10]
        );
        assert_eq!(rejected.len(), 1);
        assert_eq!(rejected[0].0.id, 9);
        assert_eq!(rejected[0].0.name, "image_gen");
    }

    #[test]
    fn video_over_limit_keeps_first_k() {
        let limits = MediaGenBatchLimits::default();
        let total = limits.max_video + 2;
        let calls: Vec<Call> = (0..total as u32)
            .map(|id| Call {
                name: "image_to_video",
                id,
            })
            .collect();
        let (allowed, rejected) = partition(calls, &limits);
        assert_eq!(allowed.len(), limits.max_video);
        assert_eq!(rejected.len(), 2);
    }

    #[test]
    fn egregious_is_double_cap_or_more() {
        assert!(
            MediaGenOverCap {
                name: "image_gen".into(),
                total: 16,
                max: 8,
            }
            .is_egregious()
        );
        assert!(
            !MediaGenOverCap {
                name: "image_gen".into(),
                total: 9,
                max: 8,
            }
            .is_egregious()
        );
        assert!(
            !MediaGenOverCap {
                name: "image_gen".into(),
                total: 15,
                max: 8,
            }
            .is_egregious()
        );
        assert!(
            !MediaGenOverCap {
                name: "image_gen".into(),
                total: 5,
                max: 0,
            }
            .is_egregious()
        );
    }

    #[test]
    fn resample_once_only_when_egregious() {
        let modest = [MediaGenOverCap {
            name: "image_gen".into(),
            total: 9,
            max: 8,
        }];
        let spam = [MediaGenOverCap {
            name: "image_gen".into(),
            total: 16,
            max: 8,
        }];
        assert!(!should_resample_egregious(&modest, 0, 1));
        assert!(should_resample_egregious(&spam, 0, 1));
        assert!(!should_resample_egregious(&spam, 1, 1));
        let text = resample_reminder(&spam);
        assert_eq!(
            text,
            "Consider the user message carefully for your future responses and tool calls. \
             If you are considering making `image_gen` calls, the max limit for `image_gen` is 8 \
             in one parallel set of calls. Larger requests will be rejected."
        );
        let mixed = [
            MediaGenOverCap {
                name: "image_gen".into(),
                total: 16,
                max: 8,
            },
            MediaGenOverCap {
                name: "video_gen".into(),
                total: 8,
                max: 4,
            },
        ];
        let mixed_text = resample_reminder(&mixed);
        assert!(mixed_text.contains("making `image_gen` and `video_gen` calls"));
        assert!(mixed_text.contains("the max limit for `image_gen` is 8"));
        assert!(mixed_text.contains("the max limit for `video_gen` is 4"));
    }

    #[test]
    fn over_cap_by_name_lists_only_over_names() {
        let limits = MediaGenBatchLimits {
            max_image: 2,
            max_video: 4,
        };
        let calls = [
            ("image_gen", Some(ToolKind::ImageGen)),
            ("image_gen", Some(ToolKind::ImageGen)),
            ("image_gen", Some(ToolKind::ImageGen)),
            ("read_file", Some(ToolKind::Read)),
            ("image_edit", Some(ToolKind::ImageGen)),
        ];
        let over = over_cap_by_name(calls, &limits);
        assert_eq!(over.len(), 1);
        assert_eq!(over[0].name, "image_gen");
        assert_eq!(over[0].total, 3);
        assert_eq!(over[0].max, 2);
    }

    #[test]
    fn image_edit_kind_is_image_gen_for_cap() {
        // Cap wiring depends on image_edit reporting ToolKind::ImageGen.
        use crate::implementations::echo_agent_build::image_edit::ImageEditTool;
        use crate::types::tool_metadata::ToolMetadata;
        assert_eq!(ToolMetadata::kind(&ImageEditTool), ToolKind::ImageGen);
    }

    #[test]
    fn every_tool_kind_has_an_explicit_budget_arm() {
        // Pins VARIANT_COUNT so a new ToolKind fails this test until
        // max_calls_per_batch is updated (exhaustive match also fails compile).
        let media_kinds = [
            ToolKind::ImageGen,
            ToolKind::VideoGen,
            ToolKind::ImageToVideo,
            ToolKind::ReferenceToVideo,
        ];
        let limits = MediaGenBatchLimits::default();
        assert!(
            media_kinds
                .iter()
                .all(|k| max_calls_per_batch(*k, &limits).is_some())
        );
        assert_eq!(
            ToolKind::VARIANT_COUNT,
            media_kinds.len() + 31,
            "ToolKind grew/shrank; update max_calls_per_batch arms and this count"
        );
    }
}
