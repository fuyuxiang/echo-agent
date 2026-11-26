use std::collections::HashMap;

use crate::error::MermaidError;
use crate::text_wrap::line_width;
use crate::theme::MermaidTheme;

/// Mermaid 11.12.2 block default padding (from `getConfig2()?.block?.padding ?? 8`).
/// This is the layout padding between sibling blocks AND the node shape padding
/// (added to bbox.width / bbox.height in rect2()).
const BLOCK_PADDING: f64 = 8.0;

/// Approximate per-character width for 16px Trebuchet MS rendered in Chromium.
/// Derived from reference SVG foreignObject measurements: "A" → 10.953, "B" → 10.984.
/// This replaces DEFAULT_CHAR_WIDTH (8.0) which is too narrow for block-beta nodes.
const BLOCK_CHAR_WIDTH: f64 = 10.97;

/// Approximate text height for a single line of 16px Trebuchet MS in Chromium foreignObject.
/// Reference SVG shows foreignObject height = 19 for single-line labels.
const BLOCK_TEXT_HEIGHT: f64 = 19.0;

/// ViewBox margin around content bounds (from `bounds2.x - 5, bounds2.y - 5, …+10, …+10`).
const VB_MARGIN: f64 = 5.0;

/// Arrow point marker offset (from mermaid's `markerOffsets.arrow_point = 4`).
/// Applied via `getLineFunctionsWithOffset` to shift the last edge point backward
/// so the arrowhead marker tip lands near the target node's edge.
const ARROW_POINT_OFFSET: f64 = 4.0;

pub fn render_block_diagram_to_svg(
    mermaid_source: &str,
    theme: &MermaidTheme,
) -> Result<String, MermaidError> {
    let diagram = parse_block_beta(mermaid_source)?;

    let ordered_nodes = &diagram.node_order;

    // --- Phase 1: Calculate node sizes (like mermaid's calculateBlockSize) ---
    // Mermaid inserts the node into the DOM, calls getBBox(), then stores
    // { width: bbox.width, height: bbox.height }.  The rect2() shape adds
    // `node.padding` (= block.padding = 8) to both dimensions.
    // We approximate the label bbox using BLOCK_CHAR_WIDTH and BLOCK_TEXT_HEIGHT.
    let mut node_sizes: HashMap<String, (f64, f64)> = HashMap::new();
    for id in ordered_nodes {
        let label = diagram.nodes.get(id).map(String::as_str).unwrap_or(id);
        let text_w = line_width(label, BLOCK_CHAR_WIDTH);
        let w = text_w + BLOCK_PADDING;
        let h = BLOCK_TEXT_HEIGHT + BLOCK_PADDING;
        node_sizes.insert(id.clone(), (w, h));
    }

    // --- Phase 2: setBlockSizes (normalize children to max width/height) ---
    let max_w = node_sizes.values().map(|(w, _)| *w).fold(0.0_f64, f64::max);
    let max_h = node_sizes.values().map(|(_, h)| *h).fold(0.0_f64, f64::max);
    // All children get the same dimensions (mermaid normalizes to maxChildSize).
    for size in node_sizes.values_mut() {
        *size = (max_w, max_h);
    }

    // --- Phase 3: layoutBlocks (position each child) ---
    // Mermaid logic: columns determine how many nodes per row.
    // startingPosX = -padding (because root.size.x is 0, which is falsy in JS)
    // child.x = startingPosX + padding + halfWidth; startingPosX = child.x + halfWidth
    // child.y = parent.y - parent.height/2 + py*(height+padding) + height/2 + padding
    //
    // First compute the root block size so we can derive child y positions.
    let columns = diagram.columns;
    let num_items = ordered_nodes.len() as i32;
    let x_size = if columns > 0 && columns < num_items {
        columns
    } else {
        num_items
    };
    let y_size = if x_size > 0 {
        (num_items as f64 / x_size as f64).ceil() as i32
    } else {
        1
    };
    let _root_w = x_size as f64 * (max_w + BLOCK_PADDING) + BLOCK_PADDING;
    let root_h = y_size as f64 * (max_h + BLOCK_PADDING) + BLOCK_PADDING;

    let mut node_layout: HashMap<String, (f64, f64)> = HashMap::new();
    let half_w = max_w / 2.0;
    let mut starting_pos_x = -BLOCK_PADDING;
    let mut current_row: i32 = 0;

    for (col_pos, id) in ordered_nodes.iter().enumerate() {
        let (_, py) = calculate_block_position(columns, col_pos as i32);
        if py != current_row {
            current_row = py;
            starting_pos_x = -BLOCK_PADDING;
        }
        let cx = starting_pos_x + BLOCK_PADDING + half_w;
        // Mermaid: child.size.y = parent.y - parent.height/2 + py*(height+padding) + height/2 + padding
        // parent.y = 0, parent.height = root_h
        let cy = -root_h / 2.0 + py as f64 * (max_h + BLOCK_PADDING) + max_h / 2.0 + BLOCK_PADDING;
        node_layout.insert(id.clone(), (cx, cy));
        starting_pos_x = cx + half_w;
    }

    // --- Phase 4: findBounds ---
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    );
    for id in ordered_nodes {
        let (cx, cy) = node_layout[id];
        let (w, h) = node_sizes[id];
        min_x = min_x.min(cx - w / 2.0);
        min_y = min_y.min(cy - h / 2.0);
        max_x = max_x.max(cx + w / 2.0);
        max_y = max_y.max(cy + h / 2.0);
    }
    let bounds_w = max_x - min_x;
    let bounds_h = max_y - min_y;

    let vb_x = min_x - VB_MARGIN;
    let vb_y = min_y - VB_MARGIN;
    let vb_w = bounds_w + VB_MARGIN * 2.0;
    let vb_h = bounds_h + VB_MARGIN * 2.0;

    let background_color = if theme.background == "#ffffff" {
        "white"
    } else {
        theme.background.as_str()
    };
    let text_color = if theme.text_color == "#333333" {
        "#333"
    } else {
        theme.text_color.as_str()
    };

    let mut svg = String::new();
    svg.push_str(&format!(
        "<svg aria-roledescription=\"block\" role=\"graphics-document document\" viewBox=\"{vb_x} {vb_y} {vb_w} {vb_h}\" style=\"max-width: {vb_w}px; background-color: {background_color};\" xmlns:xlink=\"http://www.w3.org/1999/xlink\" xmlns=\"http://www.w3.org/2000/svg\" width=\"100%\" id=\"my-svg\">"
    ));

    svg.push_str(&format!(
        "<style>{}</style>",
        block_css(
            text_color,
            &theme.edge_color,
            &theme.node_fill,
            &theme.node_stroke
        )
    ));

    svg.push_str("<g/>");
    svg.push_str(
        "<marker orient=\"auto\" markerHeight=\"12\" markerWidth=\"12\" markerUnits=\"userSpaceOnUse\" refY=\"5\" refX=\"6\" viewBox=\"0 0 10 10\" class=\"marker block\" id=\"my-svg_block-pointEnd\"><path style=\"stroke-width: 1; stroke-dasharray: 1, 0;\" class=\"arrowMarkerPath\" d=\"M 0 0 L 10 5 L 0 10 z\"/></marker>"
    );
    svg.push_str(
        "<marker orient=\"auto\" markerHeight=\"12\" markerWidth=\"12\" markerUnits=\"userSpaceOnUse\" refY=\"5\" refX=\"4.5\" viewBox=\"0 0 10 10\" class=\"marker block\" id=\"my-svg_block-pointStart\"><path style=\"stroke-width: 1; stroke-dasharray: 1, 0;\" class=\"arrowMarkerPath\" d=\"M 0 5 L 10 10 L 10 0 z\"/></marker>"
    );
    svg.push_str(
        "<marker orient=\"auto\" markerHeight=\"11\" markerWidth=\"11\" markerUnits=\"userSpaceOnUse\" refY=\"5\" refX=\"11\" viewBox=\"0 0 10 10\" class=\"marker block\" id=\"my-svg_block-circleEnd\"><circle style=\"stroke-width: 1; stroke-dasharray: 1, 0;\" class=\"arrowMarkerPath\" r=\"5\" cy=\"5\" cx=\"5\"/></marker>"
    );
    svg.push_str(
        "<marker orient=\"auto\" markerHeight=\"11\" markerWidth=\"11\" markerUnits=\"userSpaceOnUse\" refY=\"5\" refX=\"-1\" viewBox=\"0 0 10 10\" class=\"marker block\" id=\"my-svg_block-circleStart\"><circle style=\"stroke-width: 1; stroke-dasharray: 1, 0;\" class=\"arrowMarkerPath\" r=\"5\" cy=\"5\" cx=\"5\"/></marker>"
    );
    svg.push_str(
        "<marker orient=\"auto\" markerHeight=\"11\" markerWidth=\"11\" markerUnits=\"userSpaceOnUse\" refY=\"5.2\" refX=\"12\" viewBox=\"0 0 11 11\" class=\"marker cross block\" id=\"my-svg_block-crossEnd\"><path style=\"stroke-width: 2; stroke-dasharray: 1, 0;\" class=\"arrowMarkerPath\" d=\"M 1,1 l 9,9 M 10,1 l -9,9\"/></marker>"
    );
    svg.push_str(
        "<marker orient=\"auto\" markerHeight=\"11\" markerWidth=\"11\" markerUnits=\"userSpaceOnUse\" refY=\"5.2\" refX=\"-1\" viewBox=\"0 0 11 11\" class=\"marker cross block\" id=\"my-svg_block-crossStart\"><path style=\"stroke-width: 2; stroke-dasharray: 1, 0;\" class=\"arrowMarkerPath\" d=\"M 1,1 l 9,9 M 10,1 l -9,9\"/></marker>"
    );

    svg.push_str("<g class=\"block\">");

    // --- Render nodes ---
    for id in ordered_nodes {
        let Some((cx, cy)) = node_layout.get(id).copied() else {
            continue;
        };
        let (w, h) = node_sizes.get(id).copied().unwrap_or((0.0, 0.0));
        let label = diagram.nodes.get(id).map(String::as_str).unwrap_or(id);

        svg.push_str(&format!(
            "<g class=\"node default default flowchart-label\" id=\"{id}\" transform=\"translate({cx}, {cy})\">",
            id = escape_xml(id)
        ));
        svg.push_str(&format!(
            "<rect class=\"basic label-container\" style=\"\" rx=\"0\" ry=\"0\" x=\"{x2}\" y=\"{y2}\" width=\"{w}\" height=\"{h}\"/>",
            x2 = -w / 2.0,
            y2 = -h / 2.0
        ));

        svg.push_str(&format!(
            "<g class=\"label\" style=\"\"><text text-anchor=\"middle\" dominant-baseline=\"central\" class=\"nodeLabel\" dy=\"0\">{}</text></g>",
            escape_xml(label),
        ));

        svg.push_str("</g>");
    }

    // --- Render edges ---
    // Mermaid 11.12.2: 3 points [start_center, midpoint, end_center],
    // clipped via node intersection, then curveBasis path.
    for (idx, (from, to)) in diagram.edges.iter().enumerate() {
        let Some((fx, fy)) = node_layout.get(from).copied() else {
            continue;
        };
        let Some((tx, ty)) = node_layout.get(to).copied() else {
            continue;
        };
        let (fw, fh) = node_sizes.get(from).copied().unwrap_or((0.0, 0.0));
        let (tw, th) = node_sizes.get(to).copied().unwrap_or((0.0, 0.0));

        let mid_x = fx + (tx - fx) / 2.0;
        let mid_y = fy + (ty - fy) / 2.0;

        let start = rect_intersect(fx, fy, fw, fh, mid_x, mid_y);
        let end = rect_intersect(tx, ty, tw, th, mid_x, mid_y);

        // Apply arrow_point marker offset to end point (mermaid's getLineFunctionsWithOffset).
        // This shifts the curve endpoint backward by ARROW_POINT_OFFSET in the edge direction
        // so the arrowhead marker tip lands at the correct position.
        let edge_dx = end.0 - start.0;
        let edge_dy = end.1 - start.1;
        let edge_len = (edge_dx * edge_dx + edge_dy * edge_dy).sqrt();
        let offset_end = if edge_len > 1e-9 {
            (
                end.0 - ARROW_POINT_OFFSET * edge_dx / edge_len,
                end.1 - ARROW_POINT_OFFSET * edge_dy / edge_len,
            )
        } else {
            end
        };

        let points = vec![start, (mid_x, mid_y), offset_end];
        let d = curve_basis_path(&points);

        let edge_no = idx + 1;
        let ls = format!("{}1", from.to_lowercase());
        let le = format!("{}1", to.to_lowercase());

        svg.push_str(&format!(
            "<path marker-end=\"url(#my-svg_block-pointEnd)\" class=\"edge-thickness-normal edge-pattern-solid flowchart-link LS-{ls} LE-{le}\" id=\"{edge_no}-{from}-{to}\" d=\"{d}\"/>",
            from = escape_xml(from),
            to = escape_xml(to)
        ));
    }

    svg.push_str("</g></svg>");

    Ok(svg)
}

/// Compute the (px, py) grid position for a given column position.
/// Mirrors mermaid's `calculateBlockPosition(columns, position)`.
fn calculate_block_position(columns: i32, position: i32) -> (i32, i32) {
    if columns < 0 {
        return (position, 0);
    }
    if columns == 1 {
        return (0, position);
    }
    let px = position % columns;
    let py = position / columns;
    (px, py)
}

/// Compute the intersection of a ray from inside (cx, cy) toward outside (ox, oy)
/// with the boundary of a rect centered at (cx, cy) with given width and height.
fn rect_intersect(cx: f64, cy: f64, w: f64, h: f64, ox: f64, oy: f64) -> (f64, f64) {
    let hw = w / 2.0;
    let hh = h / 2.0;
    let dx = ox - cx;
    let dy = oy - cy;

    if dx.abs() < 1e-9 && dy.abs() < 1e-9 {
        return (cx + hw, cy);
    }

    if dx.abs() > 1e-9 {
        let t_x = if dx > 0.0 { hw / dx } else { -hw / dx };
        let y_at_edge = cy + dy * t_x;
        if (y_at_edge - cy).abs() <= hh + 1e-9 {
            if dx > 0.0 {
                return (cx + hw, y_at_edge);
            } else {
                return (cx - hw, y_at_edge);
            }
        }
    }

    if dy.abs() > 1e-9 {
        let t_y = if dy > 0.0 { hh / dy } else { -hh / dy };
        let x_at_edge = cx + dx * t_y;
        if dy > 0.0 {
            return (x_at_edge, cy + hh);
        } else {
            return (x_at_edge, cy - hh);
        }
    }

    (cx + hw, cy)
}

/// Generate an SVG path string using D3's curveBasis (uniform cubic B-spline).
fn curve_basis_path(points: &[(f64, f64)]) -> String {
    if points.is_empty() {
        return String::new();
    }
    if points.len() == 1 {
        return format!("M{},{}", fmt_num(points[0].0), fmt_num(points[0].1));
    }
    if points.len() == 2 {
        let (x0, y0) = points[0];
        let (x1, y1) = points[1];
        return format!(
            "M{},{}L{},{}",
            fmt_num(x0),
            fmt_num(y0),
            fmt_num(x1),
            fmt_num(y1)
        );
    }

    let mut path = String::new();
    let n = points.len();

    let (x0, y0) = points[0];
    path.push_str(&format!("M{},{}", fmt_num(x0), fmt_num(y0)));

    let (x1, y1) = points[1];
    let lx = (2.0 * x0 + x1) / 3.0;
    let ly = (2.0 * y0 + y1) / 3.0;
    path.push_str(&format!("L{},{}", fmt_num(lx), fmt_num(ly)));

    for i in 1..n - 1 {
        let (px, py) = points[i - 1];
        let (cx, cy) = points[i];
        let (nx, ny) = points[i + 1];

        let cp1x = (2.0 * cx + px) / 3.0;
        let cp1y = (2.0 * cy + py) / 3.0;
        let cp2x = (2.0 * cx + nx) / 3.0;
        let cp2y = (2.0 * cy + ny) / 3.0;

        if i == n - 2 {
            let end_x = (2.0 * cx + nx) / 3.0;
            let end_y = (2.0 * cy + ny) / 3.0;
            path.push_str(&format!(
                "C{},{},{},{},{},{}",
                fmt_num(cp1x),
                fmt_num(cp1y),
                fmt_num(cp2x),
                fmt_num(cp2y),
                fmt_num(end_x),
                fmt_num(end_y)
            ));
        } else {
            let epx = (cx + nx) / 2.0;
            let epy = (cy + ny) / 2.0;
            path.push_str(&format!(
                "C{},{},{},{},{},{}",
                fmt_num(cp1x),
                fmt_num(cp1y),
                fmt_num(cp2x),
                fmt_num(cp2y),
                fmt_num(epx),
                fmt_num(epy)
            ));
        }
    }

    let (xn, yn) = points[n - 1];
    path.push_str(&format!("L{},{}", fmt_num(xn), fmt_num(yn)));

    path
}

fn fmt_num(v: f64) -> String {
    let rounded = (v * 1000.0).round() / 1000.0;
    if (rounded - rounded.round()).abs() < 1e-9 {
        format!("{:.0}", rounded)
    } else {
        let s = format!("{rounded:.3}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

#[derive(Debug, Clone)]
struct BlockDiagram {
    nodes: HashMap<String, String>,
    /// Insertion-ordered list of node IDs (preserves declaration order).
    node_order: Vec<String>,
    edges: Vec<(String, String)>,
    /// Number of columns (from `columns N`). -1 means auto (single row).
    columns: i32,
}

fn parse_block_beta(input: &str) -> Result<BlockDiagram, MermaidError> {
    let mut found_header = false;
    let mut nodes: HashMap<String, String> = HashMap::new();
    let mut node_order: Vec<String> = Vec::new();
    let mut edges: Vec<(String, String)> = Vec::new();
    let mut columns: i32 = -1;

    let insert_node = |id: String,
                       label: String,
                       nodes: &mut HashMap<String, String>,
                       order: &mut Vec<String>| {
        if let std::collections::hash_map::Entry::Vacant(e) = nodes.entry(id.clone()) {
            order.push(id.clone());
            e.insert(label);
        } else if label != id {
            // Only update the label if the new label is explicit (not just the bare ID).
            nodes.insert(id, label);
        }
    };

    for (idx, raw) in input.lines().enumerate() {
        let line_no = idx + 1;
        let line = raw.trim();

        if line.is_empty() || line.starts_with("%%") {
            continue;
        }

        if !found_header {
            if line.split_whitespace().next() != Some("block-beta") {
                return Err(MermaidError::ParseError {
                    line: line_no,
                    message: "Expected 'block-beta' declaration".to_string(),
                });
            }
            found_header = true;
            continue;
        }

        // columns directive
        if let Some(rest) = line.strip_prefix("columns") {
            let rest = rest.trim();
            if rest == "auto" {
                columns = -1;
            } else if let Ok(n) = rest.parse::<i32>() {
                columns = n;
            }
            continue;
        }

        // Skip block:/end group markers (we don't support nested groups yet
        // but should not error on them)
        if line == "end" || line.starts_with("block:") || line.starts_with("block ") {
            continue;
        }

        // Skip style/classDef/class/linkStyle/space directives
        if line.starts_with("style ")
            || line.starts_with("classDef ")
            || line.starts_with("class ")
            || line.starts_with("linkStyle ")
        {
            continue;
        }

        // Space directive: `space` or `space:N`
        if line == "space" || line.starts_with("space:") {
            // Space nodes are invisible placeholders; skip for now
            continue;
        }

        // Edge line: contains `-->`
        if let Some((lhs, rhs)) = line.split_once("-->") {
            let (from_id, from_label) = parse_block_node(lhs.trim(), line_no)?;
            let (to_id, to_label) = parse_block_node(rhs.trim(), line_no)?;
            insert_node(from_id.clone(), from_label, &mut nodes, &mut node_order);
            insert_node(to_id.clone(), to_label, &mut nodes, &mut node_order);
            edges.push((from_id, to_id));
            continue;
        }

        // Standalone node declaration
        if let Ok((id, label)) = parse_block_node(line, line_no) {
            insert_node(id, label, &mut nodes, &mut node_order);
            continue;
        }
    }

    if !found_header {
        return Err(MermaidError::ParseError {
            line: 1,
            message: "Expected 'block-beta' declaration".to_string(),
        });
    }

    Ok(BlockDiagram {
        nodes,
        node_order,
        edges,
        columns,
    })
}

fn parse_block_node(s: &str, line: usize) -> Result<(String, String), MermaidError> {
    let s = s.trim();
    if s.is_empty() {
        return Err(MermaidError::ParseError {
            line,
            message: "Empty node".to_string(),
        });
    }

    if let Some(bracket_start) = s.find('[') {
        let id = s[..bracket_start].trim().to_string();
        let inner = s[bracket_start + 1..].trim();
        let inner = inner.strip_suffix(']').unwrap_or(inner).trim();
        let label = strip_quotes(inner);
        if id.is_empty() {
            return Err(MermaidError::ParseError {
                line,
                message: format!("Missing node id in '{s}'"),
            });
        }
        return Ok((id, label));
    }

    Ok((s.to_string(), s.to_string()))
}

fn strip_quotes(s: &str) -> String {
    let s = s.trim();
    if let Some(inner) = s.strip_prefix('"').and_then(|t| t.strip_suffix('"')) {
        return inner.to_string();
    }
    if let Some(inner) = s.strip_prefix('\'').and_then(|t| t.strip_suffix('\'')) {
        return inner.to_string();
    }
    s.to_string()
}

fn block_css(text_color: &str, edge_color: &str, node_fill: &str, node_stroke: &str) -> String {
    format!(
        "#my-svg{{font-family:\"trebuchet ms\",verdana,arial,sans-serif;font-size:16px;fill:{text_color};}}@keyframes edge-animation-frame{{from{{stroke-dashoffset:0;}}}}@keyframes dash{{to{{stroke-dashoffset:0;}}}}#my-svg .edge-animation-slow{{stroke-dasharray:9,5!important;stroke-dashoffset:900;animation:dash 50s linear infinite;stroke-linecap:round;}}#my-svg .edge-animation-fast{{stroke-dasharray:9,5!important;stroke-dashoffset:900;animation:dash 20s linear infinite;stroke-linecap:round;}}#my-svg .error-icon{{fill:#552222;}}#my-svg .error-text{{fill:#552222;stroke:#552222;}}#my-svg .edge-thickness-normal{{stroke-width:1px;}}#my-svg .edge-thickness-thick{{stroke-width:3.5px;}}#my-svg .edge-pattern-solid{{stroke-dasharray:0;}}#my-svg .edge-thickness-invisible{{stroke-width:0;fill:none;}}#my-svg .edge-pattern-dashed{{stroke-dasharray:3;}}#my-svg .edge-pattern-dotted{{stroke-dasharray:2;}}#my-svg .marker{{fill:{edge_color};stroke:{edge_color};}}#my-svg .marker.cross{{stroke:{edge_color};}}#my-svg svg{{font-family:\"trebuchet ms\",verdana,arial,sans-serif;font-size:16px;}}#my-svg p{{margin:0;}}#my-svg .label{{font-family:\"trebuchet ms\",verdana,arial,sans-serif;color:{text_color};}}#my-svg .cluster-label text{{fill:{text_color};}}#my-svg .cluster-label span,#my-svg p{{color:{text_color};}}#my-svg .label text,#my-svg span,#my-svg p{{fill:{text_color};color:{text_color};}}#my-svg .node rect,#my-svg .node circle,#my-svg .node ellipse,#my-svg .node polygon,#my-svg .node path{{fill:{node_fill};stroke:{node_stroke};stroke-width:1px;}}#my-svg .flowchart-label text{{text-anchor:middle;}}#my-svg .node .label{{text-align:center;}}#my-svg .node.clickable{{cursor:pointer;}}#my-svg .arrowheadPath{{fill:{edge_color};}}#my-svg .edgePath .path{{stroke:{edge_color};stroke-width:2.0px;}}#my-svg .flowchart-link{{stroke:{edge_color};fill:none;}}#my-svg .edgeLabel{{background-color:rgba(232,232,232, 0.8);text-align:center;}}#my-svg .edgeLabel rect{{opacity:0.5;background-color:rgba(232,232,232, 0.8);fill:rgba(232,232,232, 0.8);}}#my-svg .labelBkg{{background-color:rgba(232, 232, 232, 0.5);}}#my-svg .node .cluster{{fill:rgba(255, 255, 222, 0.5);stroke:rgba(170, 170, 51, 0.2);box-shadow:rgba(50, 50, 93, 0.25) 0px 13px 27px -5px,rgba(0, 0, 0, 0.3) 0px 8px 16px -8px;stroke-width:1px;}}#my-svg .cluster text{{fill:{text_color};}}#my-svg .cluster span,#my-svg p{{color:{text_color};}}#my-svg div.mermaidTooltip{{position:absolute;text-align:center;max-width:200px;padding:2px;font-family:\"trebuchet ms\",verdana,arial,sans-serif;font-size:12px;background:hsl(80, 100%, 96.2745098039%);border:1px solid #aaaa33;border-radius:2px;pointer-events:none;z-index:100;}}#my-svg .flowchartTitleText{{text-anchor:middle;font-size:18px;fill:{text_color};}}#my-svg :root{{--mermaid-font-family:\"trebuchet ms\",verdana,arial,sans-serif;}}",
    )
}

fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
