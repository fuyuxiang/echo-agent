use std::sync::Arc;

use super::super::*;
use super::delta;

#[test]
fn edit_plan_exposes_removed_text_before_apply_and_delta_matches() {
    let mut buffer = EditBuffer::from_text("say hello-world");
    let plan = buffer.plan_command(
        EditCommand::DeleteWordBackward(WordStyle::WhitespaceDelimited),
        &[],
    );
    let replaced = "say ".len()..buffer.text().len();
    assert_eq!(plan.replaced_byte_range(), replaced);
    assert_eq!(plan.replacement(), "");
    assert_eq!(plan.removed_text(), "hello-world");
    assert_eq!(plan.cursor_byte(), replaced.start);
    assert_eq!(plan.cursor_affinity(), PostEditCursorAffinity::Right);
    assert_eq!(buffer.text(), "say hello-world");

    let expected_delta = delta(replaced.clone(), replaced.start..replaced.start);
    let outcome = buffer.apply_plan(&plan);
    assert_eq!(outcome, Ok(EditOutcome::TextAndCursor(expected_delta)));
    assert_eq!(buffer.text(), "say ");
    assert_eq!(buffer.cursor_byte(), replaced.start);
    assert_eq!(buffer.apply_plan(&plan), Err(ApplyEditPlanError::StalePlan));
    assert_eq!(plan.into_removed_text(), "hello-world");
}

#[test]
fn stale_edit_plan_is_rejected_without_mutation() {
    let mut buffer = EditBuffer::from_text("abc");
    let plan = buffer.plan_command(EditCommand::DeleteGraphemeBackward, &[]);
    let _ = buffer.set_cursor_byte(0);
    assert_eq!(buffer.apply_plan(&plan), Err(ApplyEditPlanError::StalePlan));
    assert_eq!(buffer.text(), "abc");
    assert_eq!(buffer.cursor_byte(), 0);
}

#[test]
fn edit_plans_are_bound_to_buffer_identity_and_generation() {
    let source = EditBuffer::from_text("x");
    let plan = source.plan_command(EditCommand::DeleteGraphemeBackward, &[]);

    let mut same_value = EditBuffer::from_text("x");
    assert_eq!(same_value, source);
    assert_eq!(
        same_value.apply_plan(&plan),
        Err(ApplyEditPlanError::StalePlan)
    );

    let mut other = EditBuffer::from_text("x\u{301}");
    assert_eq!(other.apply_plan(&plan), Err(ApplyEditPlanError::StalePlan));
    assert_eq!(other.text(), "x\u{301}");

    let mut cloned = source.clone();
    assert_eq!(cloned, source);
    assert_eq!(cloned.apply_plan(&plan), Err(ApplyEditPlanError::StalePlan));
    assert_eq!(cloned, source);

    let mut changed_text = EditBuffer::from_text("x");
    let stale = changed_text.plan_command(EditCommand::DeleteGraphemeBackward, &[]);
    let _ = changed_text.insert_str("y");
    assert_eq!(
        changed_text.apply_plan(&stale),
        Err(ApplyEditPlanError::StalePlan)
    );
    assert_eq!(changed_text.text(), "xy");
}

#[test]
fn normal_state_changes_advance_generation_without_rotating_identity() {
    let mut buffer = EditBuffer::from_text("ab");
    let identity = Arc::clone(&buffer.identity);
    assert_eq!(buffer.generation, 0);

    let _ = buffer.apply(EditCommand::MoveGraphemeLeft);
    assert_eq!(buffer.generation, 1);
    assert!(Arc::ptr_eq(&identity, &buffer.identity));

    let _ = buffer.insert_str("x");
    assert_eq!(buffer.generation, 2);
    assert!(Arc::ptr_eq(&identity, &buffer.identity));
}

#[test]
fn generation_overflow_rotates_identity_and_invalidates_old_plans() {
    let mut buffer = EditBuffer::from_text("ab");
    buffer.generation = u64::MAX;
    let identity = Arc::clone(&buffer.identity);
    let plan = buffer.plan_command(EditCommand::DeleteGraphemeBackward, &[]);

    assert_eq!(
        buffer.apply_plan(&plan),
        Ok(EditOutcome::TextAndCursor(delta(1..2, 1..1)))
    );
    assert_eq!(buffer.generation, 0);
    assert!(!Arc::ptr_eq(&identity, &buffer.identity));
    assert_eq!(buffer.apply_plan(&plan), Err(ApplyEditPlanError::StalePlan));
}

#[test]
fn edit_plan_validation_rejects_grapheme_splitting_ranges() {
    let mut buffer = EditBuffer::from_text("x\u{301}");
    let mut plan = buffer.plan_command(EditCommand::DeleteGraphemeBackward, &[]);
    plan.replaced_byte_range = 0..1;
    plan.removed_text = "x".to_owned();
    plan.cursor_byte = 0;

    assert_eq!(
        buffer.apply_plan(&plan),
        Err(ApplyEditPlanError::InvalidRange)
    );
    assert_eq!(buffer.text(), "x\u{301}");
    assert_eq!(buffer.cursor_byte(), "x\u{301}".len());
}

#[test]
fn atomic_ranges_are_indivisible_for_grapheme_plans() {
    let atomic = 1..6;

    let mut backward = EditBuffer::from_parts("aTOKENb", atomic.end);
    let plan = backward.plan_command(
        EditCommand::DeleteGraphemeBackward,
        std::slice::from_ref(&atomic),
    );
    assert_eq!(plan.replaced_byte_range(), atomic);
    assert_eq!(plan.removed_text(), "TOKEN");
    assert_eq!(
        backward.apply_plan(&plan),
        Ok(EditOutcome::TextAndCursor(delta(1..6, 1..1)))
    );
    assert_eq!(backward.text(), "ab");
    assert_eq!(backward.cursor_byte(), 1);

    let mut forward = EditBuffer::from_parts("aTOKENb", atomic.start);
    let plan = forward.plan_command(
        EditCommand::DeleteGraphemeForward,
        std::slice::from_ref(&atomic),
    );
    assert_eq!(plan.replaced_byte_range(), atomic);
    assert_eq!(
        forward.apply_plan(&plan),
        Ok(EditOutcome::TextOnly(delta(1..6, 1..1)))
    );
    assert_eq!(forward.text(), "ab");
    assert_eq!(forward.cursor_byte(), 1);

    let mut motion = EditBuffer::from_parts("aTOKENb", atomic.end);
    let left = motion.plan_command(EditCommand::MoveGraphemeLeft, std::slice::from_ref(&atomic));
    assert_eq!(left.cursor_byte(), atomic.start);
    assert_eq!(motion.apply_plan(&left), Ok(EditOutcome::CursorOnly));
    let right = motion.plan_command(EditCommand::MoveGraphemeRight, &[atomic]);
    assert_eq!(right.cursor_byte(), 6);
    assert_eq!(motion.apply_plan(&right), Ok(EditOutcome::CursorOnly));

    let mut replacement = EditBuffer::from_parts("aTOKENb", 3);
    let plan = replacement.plan_replace_byte_range(3..4, "x", &[1..6]);
    assert_eq!(plan.replaced_byte_range(), 1..6);
    assert_eq!(plan.replacement(), "x");
    assert_eq!(plan.removed_text(), "TOKEN");
    assert_eq!(
        replacement.apply_plan(&plan),
        Ok(EditOutcome::TextAndCursor(delta(1..6, 1..2)))
    );
    assert_eq!(replacement.text(), "axb");
}

#[test]
fn atomic_word_classes_follow_the_selected_word_style() {
    let text = "fooTOKENbar";
    let atomic = 3..8;

    let small_forward = EditBuffer::from_parts(text, 0).plan_command(
        EditCommand::MoveWordRight(WordStyle::Small),
        std::slice::from_ref(&atomic),
    );
    assert_eq!(small_forward.cursor_byte(), atomic.start);
    let small_backward = EditBuffer::from_text(text).plan_command(
        EditCommand::MoveWordLeft(WordStyle::Small),
        std::slice::from_ref(&atomic),
    );
    assert_eq!(small_backward.cursor_byte(), atomic.end);

    let mut small_delete_forward = EditBuffer::from_parts(text, atomic.start);
    let plan = small_delete_forward.plan_command(
        EditCommand::DeleteWordForward(WordStyle::Small),
        std::slice::from_ref(&atomic),
    );
    assert_eq!(plan.replaced_byte_range(), atomic);
    assert_eq!(
        small_delete_forward.apply_plan(&plan),
        Ok(EditOutcome::TextOnly(delta(3..8, 3..3)))
    );
    assert_eq!(small_delete_forward.text(), "foobar");

    let mut small_delete_backward = EditBuffer::from_parts(text, atomic.end);
    let plan = small_delete_backward.plan_command(
        EditCommand::DeleteWordBackward(WordStyle::Small),
        std::slice::from_ref(&atomic),
    );
    assert_eq!(plan.replaced_byte_range(), atomic);
    assert_eq!(
        small_delete_backward.apply_plan(&plan),
        Ok(EditOutcome::TextAndCursor(delta(3..8, 3..3)))
    );
    assert_eq!(small_delete_backward.text(), "foobar");

    let word_forward = EditBuffer::from_parts(text, 0).plan_command(
        EditCommand::MoveWordRight(WordStyle::WhitespaceDelimited),
        std::slice::from_ref(&atomic),
    );
    assert_eq!(word_forward.cursor_byte(), text.len());
    let word_backward = EditBuffer::from_text(text).plan_command(
        EditCommand::MoveWordLeft(WordStyle::WhitespaceDelimited),
        std::slice::from_ref(&atomic),
    );
    assert_eq!(word_backward.cursor_byte(), 0);

    let mut word_delete_forward = EditBuffer::from_parts(text, 0);
    let plan = word_delete_forward.plan_command(
        EditCommand::DeleteWordForward(WordStyle::WhitespaceDelimited),
        std::slice::from_ref(&atomic),
    );
    assert_eq!(plan.replaced_byte_range(), 0..text.len());
    assert_eq!(
        word_delete_forward.apply_plan(&plan),
        Ok(EditOutcome::TextOnly(delta(0..text.len(), 0..0)))
    );
    assert_eq!(word_delete_forward.text(), "");

    let mut word_delete_backward = EditBuffer::from_text(text);
    let plan = word_delete_backward.plan_command(
        EditCommand::DeleteWordBackward(WordStyle::WhitespaceDelimited),
        &[atomic],
    );
    assert_eq!(plan.replaced_byte_range(), 0..text.len());
    assert_eq!(
        word_delete_backward.apply_plan(&plan),
        Ok(EditOutcome::TextAndCursor(delta(0..text.len(), 0..0)))
    );
    assert_eq!(word_delete_backward.text(), "");
}

#[test]
fn logical_line_plans_ignore_newlines_inside_atomic_ranges() {
    let atomic = 1..4;
    let text = "aX\nYb\nc";

    let motion = EditBuffer::from_parts(text, 0);
    let to_end = motion.plan_command(
        EditCommand::MoveLogicalLineEnd,
        std::slice::from_ref(&atomic),
    );
    assert_eq!(to_end.cursor_byte(), 5);

    let mut deletion = EditBuffer::from_parts(text, 0);
    let to_end = deletion.plan_command(EditCommand::DeleteToLineEnd, std::slice::from_ref(&atomic));
    assert_eq!(to_end.replaced_byte_range(), 0..5);
    assert_eq!(to_end.removed_text(), "aX\nYb");
    assert_eq!(
        deletion.apply_plan(&to_end),
        Ok(EditOutcome::TextOnly(delta(0..5, 0..0)))
    );
    assert_eq!(deletion.text(), "\nc");

    let from_bol = EditBuffer::from_parts(text, 6);
    let to_start = from_bol.plan_command(
        EditCommand::MoveLogicalLineStart,
        std::slice::from_ref(&atomic),
    );
    assert_eq!(to_start.cursor_byte(), 0);

    let deletion = EditBuffer::from_parts(text, atomic.end);
    let to_start = deletion.plan_command(EditCommand::DeleteToLineStart, &[atomic]);
    assert_eq!(to_start.replaced_byte_range(), 0..4);
    assert_eq!(to_start.removed_text(), "aX\nY");
}
