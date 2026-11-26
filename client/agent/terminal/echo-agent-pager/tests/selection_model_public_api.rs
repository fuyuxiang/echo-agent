use echo_agent_pager::scrollback::text_selection::ResolvedSelectionModel;
use ratatui::layout::Rect;

#[test]
fn resolved_selection_model_supports_exhaustive_external_literals() {
    let model = ResolvedSelectionModel {
        ranges: Vec::new(),
        visible_blocks: Vec::new(),
        content_area: Rect::default(),
    };

    assert!(model.ranges.is_empty());
}

#[test]
fn resolved_selection_model_supports_external_struct_update() {
    let model = ResolvedSelectionModel {
        content_area: Rect::new(1, 2, 3, 4),
        ..ResolvedSelectionModel::default()
    };

    assert_eq!(model.content_area, Rect::new(1, 2, 3, 4));
}
