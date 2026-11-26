use std::path::Path;

use ratatui::layout::Rect;

use crate::prompt_images::PastedImage;
use crate::terminal::image::{self as terminal_image, GraphicsProtocol};

pub(super) const MIN_BOX_WIDTH: u16 = 28;
pub(super) const MIN_PIXEL_BOX_HEIGHT: u16 = 8;
pub(super) const MIN_META_BOX_HEIGHT: u16 = 6;

const META_PREVIEW_WIDTH_RATIO: f32 = 0.75;
const META_CONTENT_LINES: u16 = 4;
const META_BOX_CHROME_ROWS: u16 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ImagePreviewPlan<'a> {
    pub(super) show_pixels: bool,
    pub(super) display_path: Option<&'a Path>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ImageOverlayGeometry {
    pub(super) overlay_rect: Rect,
    pub(super) image_placement: Option<ImagePlacement>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ImagePlacement {
    pub(super) cols: u16,
    pub(super) rows: u16,
    pub(super) x: u16,
    pub(super) y: u16,
}

pub(super) fn plan_image_preview(
    image: &PastedImage,
    protocol: GraphicsProtocol,
) -> ImagePreviewPlan<'_> {
    ImagePreviewPlan {
        show_pixels: protocol.supports_images() && image.preview.prepared().is_some(),
        display_path: image.source_path.as_deref(),
    }
}

pub(super) fn overlay_geometry(
    area: Rect,
    show_pixels: bool,
    has_path: bool,
    dimensions: (u32, u32),
) -> Option<ImageOverlayGeometry> {
    let min_height = if show_pixels {
        MIN_PIXEL_BOX_HEIGHT
    } else {
        MIN_META_BOX_HEIGHT
    };
    if area.width < MIN_BOX_WIDTH || area.height < min_height {
        return None;
    }

    if show_pixels {
        let footer_rows = u16::from(has_path);
        let max_cols = area.width.saturating_sub(2).max(4);
        let max_rows = area
            .height
            .saturating_sub(2)
            .saturating_sub(footer_rows)
            .max(2);
        let (cols, rows) =
            terminal_image::fit_image_to_cells(dimensions.0, dimensions.1, max_cols, max_rows);
        let width = (cols.saturating_add(2)).clamp(MIN_BOX_WIDTH, area.width);
        let height = (rows.saturating_add(2).saturating_add(footer_rows))
            .clamp(MIN_PIXEL_BOX_HEIGHT, area.height);
        let x = area.x + area.width.saturating_sub(width) / 2;
        let y = area.y + area.height.saturating_sub(height) / 2;
        let inner_width = width.saturating_sub(2);
        let inner_height = height.saturating_sub(2).saturating_sub(footer_rows);
        return Some(ImageOverlayGeometry {
            overlay_rect: Rect::new(x, y, width, height),
            image_placement: Some(ImagePlacement {
                cols,
                rows,
                x: x + 1 + inner_width.saturating_sub(cols) / 2,
                y: y + 1 + inner_height.saturating_sub(rows) / 2,
            }),
        });
    }

    let width = ((area.width as f32) * META_PREVIEW_WIDTH_RATIO) as u16;
    let width = width.clamp(MIN_BOX_WIDTH, area.width);
    let height = (META_CONTENT_LINES + META_BOX_CHROME_ROWS)
        .min(area.height)
        .max(MIN_META_BOX_HEIGHT)
        .min(area.height);
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height);
    Some(ImageOverlayGeometry {
        overlay_rect: Rect::new(x, y, width, height),
        image_placement: None,
    })
}
