use std::time::Duration;

use ratatui::style::Modifier;
use ratatui::text::{Line, Span, Text};

use crate::appearance::AppearanceConfig;
use crate::render::color::blend_color;
use crate::scrollback::block::BlockContent;
use crate::scrollback::types::{AccentStyle, BlockContext, BlockOutput, DisplayMode};
use crate::theme::Theme;
use crate::util::format_duration;

#[derive(Debug, Clone, PartialEq)]
pub enum WorkflowBlockStatus {
    Running,
    Done { elapsed: Duration },
    Failed { elapsed: Duration },
    Cancelled { elapsed: Duration },
    Paused { elapsed: Duration },
}

#[derive(Debug, Clone)]
pub struct WorkflowBlockPhase {
    pub title: String,
    pub state: String,
}

#[derive(Debug, Clone)]
pub struct WorkflowBlock {
    pub run_id: String,
    pub name: String,
    pub objective: String,
    pub status: WorkflowBlockStatus,
    pub phases: Vec<WorkflowBlockPhase>,
    pub current_phase: Option<String>,
    pub active_agents: u32,
    pub elapsed: Duration,
}

impl WorkflowBlock {
    pub fn started(
        run_id: impl Into<String>,
        name: impl Into<String>,
        objective: impl Into<String>,
    ) -> Self {
        Self {
            run_id: run_id.into(),
            name: name.into(),
            objective: objective.into(),
            status: WorkflowBlockStatus::Running,
            phases: Vec::new(),
            current_phase: None,
            active_agents: 0,
            elapsed: Duration::ZERO,
        }
    }

    fn phase_trail(&self) -> Option<String> {
        if self.phases.is_empty() {
            return self.current_phase.clone();
        }
        Some(
            self.phases
                .iter()
                .map(|p| {
                    let mark = match p.state.as_str() {
                        "done" => "✓",
                        "active" => "●",
                        _ => "○",
                    };
                    format!("{} {mark}", p.title)
                })
                .collect::<Vec<_>>()
                .join(" · "),
        )
    }
}

impl BlockContent for WorkflowBlock {
    fn output(&self, ctx: &BlockContext) -> BlockOutput {
        let theme = Theme::current();
        let bold = if ctx.is_selected {
            theme.primary().add_modifier(Modifier::BOLD)
        } else {
            theme.muted().add_modifier(Modifier::BOLD)
        };
        let muted = theme.muted();

        let mut spans = vec![Span::styled("Workflow ", bold)];
        let verb = match &self.status {
            WorkflowBlockStatus::Running => format!("{}: ", self.name),
            WorkflowBlockStatus::Done { elapsed } => {
                format!("{} done in {}: ", self.name, format_duration(*elapsed))
            }
            WorkflowBlockStatus::Failed { elapsed } => {
                format!("{} failed in {}: ", self.name, format_duration(*elapsed))
            }
            WorkflowBlockStatus::Cancelled { elapsed } => {
                format!(
                    "{} ◌ cancelled after {}: ",
                    self.name,
                    format_duration(*elapsed)
                )
            }
            WorkflowBlockStatus::Paused { elapsed } => {
                format!("{} paused at {}: ", self.name, format_duration(*elapsed))
            }
        };
        let text_style = if matches!(self.status, WorkflowBlockStatus::Cancelled { .. }) {
            theme.dim()
        } else {
            muted
        };
        spans.push(Span::styled(verb, text_style));
        spans.push(Span::styled(self.objective.replace('\n', " "), text_style));
        if let Some(trail) = self.phase_trail()
            && !trail.is_empty()
        {
            spans.push(Span::styled(format!("  [{trail}]"), text_style));
        }
        if matches!(self.status, WorkflowBlockStatus::Running) && self.active_agents > 0 {
            spans.push(Span::styled(
                format!("  ({} agents)", self.active_agents),
                muted,
            ));
        }

        BlockOutput {
            lines: vec![Line::from(spans).into()],
        }
    }

    fn accent(&self, ctx: &BlockContext) -> Option<AccentStyle> {
        let theme = Theme::current();
        match &self.status {
            WorkflowBlockStatus::Running if ctx.is_running => {
                Some(AccentStyle::static_color(theme.accent_running))
            }
            _ => None,
        }
    }

    fn bullet(&self, ctx: &BlockContext) -> Option<AccentStyle> {
        let theme = Theme::current();
        match &self.status {
            WorkflowBlockStatus::Running => {
                if ctx.is_running {
                    let dim = ctx.appearance.scrollback.display.dim_accent;
                    let dimmed = blend_color(theme.bg_base, theme.accent_running, dim)
                        .unwrap_or(theme.accent_running);
                    Some(AccentStyle::animated(dimmed))
                } else {
                    None
                }
            }
            WorkflowBlockStatus::Done { .. } => {
                Some(AccentStyle::static_color(theme.accent_success))
            }
            WorkflowBlockStatus::Failed { .. } => {
                Some(AccentStyle::static_color(theme.accent_error))
            }
            WorkflowBlockStatus::Cancelled { .. } => {
                Some(AccentStyle::static_color(theme.gray_dim))
            }
            WorkflowBlockStatus::Paused { .. } => Some(AccentStyle::static_color(theme.warning)),
        }
    }

    fn has_vpad_for(&self, _appearance: &AppearanceConfig) -> bool {
        false
    }

    fn has_raw_mode(&self) -> bool {
        false
    }

    fn is_foldable(&self) -> bool {
        false
    }

    fn default_display_mode(&self) -> DisplayMode {
        DisplayMode::Collapsed
    }

    fn is_selectable(&self) -> bool {
        true
    }

    fn has_bullet(&self, _ctx: &BlockContext) -> bool {
        true
    }

    fn is_groupable(&self) -> bool {
        true
    }

    fn preamble(&self, _ctx: &BlockContext) -> Option<Text<'static>> {
        let theme = Theme::current();
        let mut lines = vec![
            Line::from(Span::styled(self.objective.clone(), theme.primary())),
            Line::from(""),
        ];
        for p in &self.phases {
            let mark = match p.state.as_str() {
                "done" => "✓",
                "active" => "●",
                _ => "○",
            };
            lines.push(Line::from(Span::styled(
                format!("  {mark} {}", p.title),
                theme.muted(),
            )));
        }
        Some(Text::from(lines))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appearance::AppearanceConfig;

    fn test_ctx() -> BlockContext {
        BlockContext {
            mode: DisplayMode::Collapsed,
            is_running: false,
            width: 120,
            raw: false,
            max_lines: None,
            appearance: AppearanceConfig::default(),
            is_selected: false,
            cwd: None,
        }
    }

    fn line_text(block: &WorkflowBlock) -> String {
        block.output(&test_ctx()).lines[0]
            .content
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect()
    }

    #[test]
    fn running_line_shows_phase_trail_and_agents() {
        let mut block = WorkflowBlock::started("wf_1", "deep-research", "compare A and B");
        block.phases = vec![
            WorkflowBlockPhase {
                title: "Plan".into(),
                state: "done".into(),
            },
            WorkflowBlockPhase {
                title: "Research".into(),
                state: "active".into(),
            },
        ];
        block.active_agents = 3;
        let text = line_text(&block);
        assert!(text.contains("deep-research"), "got: {text}");
        assert!(text.contains("Plan ✓"), "got: {text}");
        assert!(text.contains("Research ●"), "got: {text}");
        assert!(text.contains("(3 agents)"), "got: {text}");
    }

    #[test]
    fn done_line_shows_duration_and_drops_agents() {
        let mut block = WorkflowBlock::started("wf_1", "deep-research", "q");
        block.active_agents = 3;
        block.status = WorkflowBlockStatus::Done {
            elapsed: Duration::from_secs(90),
        };
        let text = line_text(&block);
        assert!(text.contains("done in"), "got: {text}");
        assert!(!text.contains("agents"), "got: {text}");
    }

    #[test]
    fn cancelled_line_shows_dim_glyph_not_failure() {
        let mut block = WorkflowBlock::started("wf_1", "deep-research", "q");
        block.active_agents = 2;
        block.status = WorkflowBlockStatus::Cancelled {
            elapsed: Duration::from_secs(45),
        };
        let text = line_text(&block);
        assert!(text.contains("◌ cancelled after"), "got: {text}");
        assert!(!text.contains("failed"), "got: {text}");
        assert!(!text.contains("agents"), "got: {text}");
    }

    #[test]
    fn cancelled_bullet_is_static_gray() {
        let mut block = WorkflowBlock::started("wf_1", "deep-research", "q");
        block.status = WorkflowBlockStatus::Cancelled {
            elapsed: Duration::from_secs(1),
        };
        let theme = Theme::current();
        let bullet = block.bullet(&test_ctx()).expect("terminal bullet");
        assert_eq!(bullet, AccentStyle::static_color(theme.gray_dim));
        assert!(block.accent(&test_ctx()).is_none());
    }

    #[test]
    fn multiline_objective_collapses() {
        let block = WorkflowBlock::started("wf_1", "x", "line1\nline2");
        let text = line_text(&block);
        assert!(!text.contains('\n'));
        assert!(text.contains("line1 line2"));
    }
}
