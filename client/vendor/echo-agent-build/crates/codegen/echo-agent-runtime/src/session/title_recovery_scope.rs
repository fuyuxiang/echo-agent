// Modified by EchoAgent for the embedded desktop runtime; see ECHOAGENT_VENDOR.md.

/// Earliest durable prompt that belongs to this session's own title.
/// This is independent of the inherited context boundary used by compaction.
pub(crate) fn next_title_prompt_index(
    selected: Option<usize>,
    inherited_prompts: impl IntoIterator<Item = Option<usize>>,
) -> std::io::Result<usize> {
    let last = selected.or_else(|| inherited_prompts.into_iter().flatten().max());
    last.map_or(Ok(0), |index| {
        index.checked_add(1).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "title prompt index overflow",
            )
        })
    })
}

pub(crate) fn initial_title_scope(
    title_source_prompt_index: Option<usize>,
    user_prompts: impl IntoIterator<Item = Option<usize>>,
) -> (bool, usize) {
    let minimum = title_source_prompt_index.unwrap_or(0);
    let had_local_history = user_prompts
        .into_iter()
        .any(|index| index.map_or(minimum == 0, |index| index >= minimum));
    (had_local_history, minimum)
}

pub(crate) fn is_title_source_prompt(index: Option<u64>, minimum: usize) -> bool {
    index.map_or(minimum == 0, |index| index >= minimum as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_resume_keeps_original_opening_even_after_compaction() {
        assert_eq!(initial_title_scope(None, [None, Some(12)]), (true, 0));
        assert_eq!(initial_title_scope(None, []), (false, 0));
    }

    #[test]
    fn inherited_only_fork_waits_for_its_first_local_prompt() {
        assert_eq!(
            initial_title_scope(Some(5), [None, Some(0), Some(4)]),
            (false, 5)
        );
    }

    #[test]
    fn resumed_fork_recovers_original_local_opening_after_compaction() {
        assert_eq!(
            initial_title_scope(Some(5), [None, Some(0), Some(4), Some(16)]),
            (true, 5)
        );
    }

    #[test]
    fn missing_legacy_indexes_cannot_name_a_fresh_continuation() {
        assert_eq!(initial_title_scope(Some(5), [None, Some(2)]), (false, 5));
    }

    #[test]
    fn title_boundary_is_the_selected_cut_and_rejects_overflow() {
        assert_eq!(
            next_title_prompt_index(Some(4), [Some(0), Some(12)]).unwrap(),
            5
        );
        assert_eq!(next_title_prompt_index(None, [None, Some(12)]).unwrap(), 13);
        assert_eq!(next_title_prompt_index(None, []).unwrap(), 0);
        assert!(next_title_prompt_index(Some(usize::MAX), []).is_err());
    }

    #[test]
    fn prompt_filter_keeps_legacy_openings_but_skips_inherited_and_unscoped_chunks() {
        assert!(is_title_source_prompt(None, 0));
        assert!(is_title_source_prompt(Some(0), 0));
        assert!(!is_title_source_prompt(Some(4), 5));
        assert!(!is_title_source_prompt(None, 5));
        assert!(is_title_source_prompt(Some(5), 5));
        assert!(is_title_source_prompt(Some(16), 5));
    }
}
