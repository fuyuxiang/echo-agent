use crate::error::MermaidError;
use crate::theme::MermaidTheme;

// ── C4 config defaults (from Mermaid 11.12.2 defaultConfig.c4) ──────────────

const DIAGRAM_MARGIN_X: f64 = 50.0;
const DIAGRAM_MARGIN_Y: f64 = 10.0;
const C4_SHAPE_MARGIN: f64 = 50.0;
const C4_SHAPE_PADDING: f64 = 20.0;
const DEFAULT_WIDTH: f64 = 216.0;
const DEFAULT_HEIGHT: f64 = 60.0;
const C4_SHAPE_IN_ROW: usize = 4;
const FONT_SIZE: f64 = 14.0;
const FONT_FAMILY: &str = "'Open Sans', sans-serif";
const MESSAGE_FONT_SIZE: f64 = 12.0;

const PERSON_IMG: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAADAAAAAwCAIAAADYYG7QAAACD0lEQVR4Xu2YoU4EMRCGT+4j8Ai8AhaH4QHgAUjQuFMECUgMIUgwJAgMhgQsAYUiJCiQIBBY+EITsjfTdme6V24v4c8vyGbb+ZjOtN0bNcvjQXmkH83WvYBWto6PLm6v7p7uH1/w2fXD+PBycX1Pv2l3IdDm/vn7x+dXQiAubRzoURa7gRZWd0iGRIiJbOnhnfYBQZNJjNbuyY2eJG8fkDE3bbG4ep6MHUAsgYxmE3nVs6VsBWJSGccsOlFPmLIViMzLOB7pCVO2AtHJMohH7Fh6zqitQK7m0rJvAVYgGcEpe//PLdDz65sM4pF9N7ICcXDKIB5Nv6j7tD0NoSdM2QrU9Gg0ewE1LqBhHR3BBdvj2vapnidjHxD/q6vd7Pvhr31AwcY8eXMTXAKECZZJFXuEq27aLgQK5uLMohCenGGuGewOxSjBvYBqeG6B+Nqiblggdjnc+ZXDy+FNFpFzw76O3UBAROuXh6FoiAcf5g9eTvUgzy0nWg6I8cXHRUpg5bOVBCo+KDpFajOf23GgPme7RSQ+lacIENUgJ6gg1k6HjgOlqnLqip4tEuhv0hNEMXUD0clyXE3p6pZA0S2nnvTlXwLJEZWlb7cTQH1+USgTN4VhAenm/wea1OCAOmqo6fE1WCb9WSKBah+rbUWPWAmE2Rvk0ApiB45eOyNAzU8xcTvj8KvkKEoOaIYeHNA3ZuygAvFMUO0AAAAASUVORK5CYII=";

const EXTERNAL_PERSON_IMG: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAADAAAAAwCAIAAADYYG7QAAAB6ElEQVR4Xu2YLY+ЕМБCG9+dWr0aj0Wg0Go1Go0+j8Xdv2uTCvv1gpt0ebHKPuhDaeW4605Z9mJvx4AdXUyTUdd08z+u6flmWZRnHsWkafk9DptAwDPu+f0eAYtu2PEaGWuj5fCIZrBAC2eLBAnRCsEkkxmeaJp7iDJ2QMDdHsLg8SxKFEJaAo8lAXnmuOFIhTMpxxKATebo4UiFknuNo4OniSIXQyRxEA3YsnjGCVEjVXD7yLUAqxBGUyPv/Y4W2beMgGuS7kVQIBycH0fD+oi5pezQETxdHKmQKGk1eQEYldK+jw5GxPfZ9z7Mk0Qnhf1W1m3w//EUn5BDmSZsbR44QQLBEqrBHqOrmSKaQAxdnLArCrxZcM7A7ZKs4ioRq8LFC+NpC3WCBJsvpVw5edm9iEXFuyNfxXAgSwfrFQ1c0iNda8AdejvUgnktOtJQQxmcfFzGglc5WVCj7oDgFqU18boeFSs52CUh8LE8BIVQDT1ABrB0HtgSEYlX5doJnCwv9TXocKCaKbnwhdDKPq4lf3SwU3HLq4V/+WYhHVMa/3b4IlfyikAduCkcBc7mQ3/z/Qq/cTuikhkzB12Ae/mcJC9U+Vo8Ej1gWAtgbeGgFsAMHr50BIWOLCbezvhpBFUdY6EJuJ/QDW0XoMX60zZ0AAAAASUVORK5CYII=";

// ── Colors ──────────────────────────────────────────────────────────────────

fn bg_color_for(type_text: &str) -> &'static str {
    match type_text {
        "person" => "#08427B",
        "external_person" => "#686868",
        "system" => "#1168BD",
        "external_system" => "#999999",
        "system_db" => "#1168BD",
        "external_system_db" => "#999999",
        "system_queue" => "#1168BD",
        "external_system_queue" => "#999999",
        "container" => "#438DD5",
        "external_container" => "#B3B3B3",
        "container_db" => "#438DD5",
        "external_container_db" => "#B3B3B3",
        "container_queue" => "#438DD5",
        "external_container_queue" => "#B3B3B3",
        "component" => "#85BBF0",
        "external_component" => "#CCCCCC",
        "component_db" => "#85BBF0",
        "external_component_db" => "#CCCCCC",
        "component_queue" => "#85BBF0",
        "external_component_queue" => "#CCCCCC",
        _ => "#1168BD",
    }
}

fn border_color_for(type_text: &str) -> &'static str {
    match type_text {
        "person" => "#073B6F",
        "external_person" => "#8A8A8A",
        "system" => "#3C7FC0",
        "external_system" => "#8A8A8A",
        "system_db" => "#3C7FC0",
        "external_system_db" => "#8A8A8A",
        "system_queue" => "#3C7FC0",
        "external_system_queue" => "#8A8A8A",
        "container" => "#3C7FC0",
        "external_container" => "#A6A6A6",
        "container_db" => "#3C7FC0",
        "external_container_db" => "#A6A6A6",
        "container_queue" => "#3C7FC0",
        "external_container_queue" => "#A6A6A6",
        "component" => "#78A8D8",
        "external_component" => "#BFBFBF",
        "component_db" => "#78A8D8",
        "external_component_db" => "#BFBFBF",
        "component_queue" => "#78A8D8",
        "external_component_queue" => "#BFBFBF",
        _ => "#3C7FC0",
    }
}

// ── Data model ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct C4Shape {
    alias: String,
    type_c4: String, // person, system, container, component, external_* variants
    label: String,
    techn: String,
    descr: String,
    parent_boundary: String,
    // Computed layout
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

#[derive(Debug, Clone)]
struct C4Boundary {
    alias: String,
    label: String,
    type_text: String,
    #[allow(dead_code)] // Used for nested boundary support in future iterations
    parent_boundary: String,
    // Computed layout
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

#[derive(Debug, Clone)]
struct C4Rel {
    #[allow(dead_code)]
    rel_type: String, // "rel", "birel", "rel_b"
    from: String,
    to: String,
    label: String,
    techn: String,
}

#[derive(Debug)]
struct C4Diagram {
    c4_type: String,
    title: String,
    shapes: Vec<C4Shape>,
    boundaries: Vec<C4Boundary>,
    rels: Vec<C4Rel>,
}

// ── Bounds (port of Mermaid's Bounds class in c4Renderer.js) ────────────────

#[derive(Debug, Clone)]
struct Bounds {
    startx: f64,
    stopx: f64,
    starty: f64,
    stopy: f64,
    width_limit: f64,
    next_startx: f64,
    next_stopx: f64,
    next_starty: f64,
    next_stopy: f64,
    next_cnt: usize,
}

impl Bounds {
    fn new() -> Self {
        Self {
            startx: 0.0,
            stopx: 0.0,
            starty: 0.0,
            stopy: 0.0,
            width_limit: f64::MAX,
            next_startx: 0.0,
            next_stopx: 0.0,
            next_starty: 0.0,
            next_stopy: 0.0,
            next_cnt: 0,
        }
    }

    fn set_data(&mut self, startx: f64, stopx: f64, starty: f64, stopy: f64) {
        self.startx = startx;
        self.stopx = stopx;
        self.starty = starty;
        self.stopy = stopy;
        self.next_startx = startx;
        self.next_stopx = stopx;
        self.next_starty = starty;
        self.next_stopy = stopy;
    }

    fn insert(&mut self, shape: &mut C4Shape) {
        self.next_cnt += 1;
        let margin = C4_SHAPE_MARGIN;

        let mut sx = if (self.next_startx - self.next_stopx).abs() < 0.001 {
            self.next_stopx + margin
        } else {
            self.next_stopx + margin * 2.0
        };
        let mut ex = sx + shape.width;
        let mut sy = self.next_starty + margin * 2.0;
        let mut ey = sy + shape.height;

        if sx >= self.width_limit || ex >= self.width_limit || self.next_cnt > C4_SHAPE_IN_ROW {
            sx = self.next_startx + margin;
            sy = self.next_stopy + margin * 2.0;
            self.next_stopx = sx + shape.width;
            ex = self.next_stopx;
            self.next_starty = self.next_stopy;
            self.next_stopy = sy + shape.height;
            ey = self.next_stopy;
            self.next_cnt = 1;
        }

        shape.x = sx;
        shape.y = sy;

        // updateVal: min for start, max for stop
        self.startx = self.startx.min(sx);
        self.starty = self.starty.min(sy);
        self.stopx = self.stopx.max(ex);
        self.stopy = self.stopy.max(ey);
        self.next_startx = self.next_startx.min(sx);
        self.next_starty = self.next_starty.min(sy);
        self.next_stopx = self.next_stopx.max(ex);
        self.next_stopy = self.next_stopy.max(ey);
    }

    fn bump_last_margin(&mut self) {
        self.stopx += C4_SHAPE_MARGIN;
        self.stopy += C4_SHAPE_MARGIN;
    }
}

// ── Text measurement (simplified) ──────────────────────────────────────────

fn estimate_text_width(text: &str, font_size: f64) -> f64 {
    // Rough heuristic: ~0.6 * font_size per character (Open Sans)
    text.len() as f64 * font_size * 0.6
}

fn estimate_text_height(font_size: f64) -> f64 {
    font_size + 2.0
}

// ── Layout computation ─────────────────────────────────────────────────────

fn compute_shape_dimensions(shape: &mut C4Shape) {
    let type_font_size = FONT_SIZE - 2.0;

    let mut y = C4_SHAPE_PADDING;
    // typeC4Shape line
    y += type_font_size + 2.0 - 4.0;

    // Person image
    if shape.type_c4 == "person" || shape.type_c4 == "external_person" {
        y += 48.0; // image height
    }

    // Label
    let label_font_size = FONT_SIZE + 2.0;
    let label_width = estimate_text_width(&shape.label, label_font_size);
    y += 8.0; // padding before label
    y += estimate_text_height(label_font_size);

    let mut rect_width = label_width;

    // Techn or type text
    if !shape.techn.is_empty() {
        let techn_display = format!("[{}]", shape.techn);
        let techn_width = estimate_text_width(&techn_display, FONT_SIZE);
        rect_width = rect_width.max(techn_width);
        y += 5.0;
        y += estimate_text_height(FONT_SIZE);
    }

    // Description — mermaid.js includes description in the visible rect height
    // and uses the description width to size the shape.
    if !shape.descr.is_empty() {
        let descr_width = estimate_text_width(&shape.descr, FONT_SIZE);
        rect_width = rect_width.max(descr_width);
        y += 20.0;
        let descr_height = estimate_text_height(FONT_SIZE);
        y += descr_height;
    }

    // The final rect_height includes all text lines
    let rect_height = y;

    rect_width += C4_SHAPE_PADDING;

    shape.width = shape.width.max(rect_width).max(DEFAULT_WIDTH);
    shape.height = shape.height.max(rect_height).max(DEFAULT_HEIGHT);
}

fn layout_shapes_in_bounds(bounds: &mut Bounds, shapes: &mut [C4Shape]) {
    for shape in shapes.iter_mut() {
        compute_shape_dimensions(shape);
        bounds.insert(shape);
    }
    bounds.bump_last_margin();
}

fn get_intersect_point(
    from_x: f64,
    from_y: f64,
    from_w: f64,
    from_h: f64,
    end_x: f64,
    end_y: f64,
) -> (f64, f64) {
    let cx = from_x + from_w / 2.0;
    let cy = from_y + from_h / 2.0;
    let dx = (from_x - end_x).abs();
    let dy = (from_y - end_y).abs();

    if dx < 0.001 && dy < 0.001 {
        return (cx, cy);
    }

    let from_dyx = from_h / from_w;

    if (from_y - end_y).abs() < 0.001 {
        if from_x < end_x {
            return (from_x + from_w, cy);
        } else {
            return (from_x, cy);
        }
    }
    if (from_x - end_x).abs() < 0.001 {
        if from_y < end_y {
            return (cx, from_y + from_h);
        } else {
            return (cx, from_y);
        }
    }

    let tan_dyx = dy / dx;

    if from_x > end_x && from_y < end_y {
        if from_dyx >= tan_dyx {
            (from_x, cy + tan_dyx * from_w / 2.0)
        } else {
            (cx - dx / dy * from_h / 2.0, from_y + from_h)
        }
    } else if from_x < end_x && from_y < end_y {
        if from_dyx >= tan_dyx {
            (from_x + from_w, cy + tan_dyx * from_w / 2.0)
        } else {
            (cx + dx / dy * from_h / 2.0, from_y + from_h)
        }
    } else if from_x < end_x && from_y > end_y {
        if from_dyx >= tan_dyx {
            (from_x + from_w, cy - tan_dyx * from_w / 2.0)
        } else {
            (cx + from_h / 2.0 * dx / dy, from_y)
        }
    } else {
        // from_x > end_x && from_y > end_y
        if from_dyx >= tan_dyx {
            (from_x, cy - from_w / 2.0 * tan_dyx)
        } else {
            (cx - from_h / 2.0 * dx / dy, from_y)
        }
    }
}

fn get_intersect_points(from: &C4Shape, to: &C4Shape) -> ((f64, f64), (f64, f64)) {
    let end_center = (to.x + to.width / 2.0, to.y + to.height / 2.0);
    let start_point = get_intersect_point(
        from.x,
        from.y,
        from.width,
        from.height,
        end_center.0,
        end_center.1,
    );

    let from_center = (from.x + from.width / 2.0, from.y + from.height / 2.0);
    let end_point = get_intersect_point(
        to.x,
        to.y,
        to.width,
        to.height,
        from_center.0,
        from_center.1,
    );

    (start_point, end_point)
}

// ── Parser ──────────────────────────────────────────────────────────────────

fn parse_c4_diagram(input: &str) -> Result<C4Diagram, MermaidError> {
    let mut c4_type = String::new();
    let mut title = String::new();
    let mut shapes: Vec<C4Shape> = Vec::new();
    let mut boundaries: Vec<C4Boundary> = Vec::new();
    let mut rels: Vec<C4Rel> = Vec::new();
    let mut current_boundary = "global".to_string();
    let mut boundary_stack: Vec<String> = vec!["global".to_string()];

    // global boundary
    boundaries.push(C4Boundary {
        alias: "global".to_string(),
        label: "global".to_string(),
        type_text: "global".to_string(),
        parent_boundary: String::new(),
        x: 0.0,
        y: 0.0,
        width: 0.0,
        height: 0.0,
    });

    let mut found_header = false;

    for (idx, raw_line) in input.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with("%%") {
            continue;
        }

        if !found_header {
            let token = line.split_whitespace().next().unwrap_or("");
            match token {
                "C4Context" | "C4Container" | "C4Component" | "C4Dynamic" | "C4Deployment" => {
                    c4_type = token.to_string();
                    found_header = true;
                    continue;
                }
                _ => {
                    return Err(MermaidError::ParseError {
                        line: idx + 1,
                        message: format!("Expected C4 diagram type, got '{token}'"),
                    });
                }
            }
        }

        // Title
        if let Some(rest) = line.strip_prefix("title ") {
            title = rest.trim().to_string();
            continue;
        }
        if line == "title" {
            continue;
        }

        // Boundary end
        if line == "}" || line == "end" {
            if boundary_stack.len() > 1 {
                boundary_stack.pop();
                current_boundary = boundary_stack.last().cloned().unwrap_or_default();
            }
            continue;
        }

        // Parse function-call-style statements
        if let Some(parsed) = parse_c4_statement(line) {
            match parsed.func.as_str() {
                "Person" | "Person_Ext" => {
                    let type_c4 = if parsed.func == "Person_Ext" {
                        "external_person"
                    } else {
                        "person"
                    };
                    shapes.push(C4Shape {
                        alias: parsed.args.first().cloned().unwrap_or_default(),
                        type_c4: type_c4.to_string(),
                        label: parsed.args.get(1).cloned().unwrap_or_default(),
                        techn: String::new(),
                        descr: parsed.args.get(2).cloned().unwrap_or_default(),
                        parent_boundary: current_boundary.clone(),
                        x: 0.0,
                        y: 0.0,
                        width: 0.0,
                        height: 0.0,
                    });
                }
                "System" | "System_Ext" => {
                    let type_c4 = if parsed.func == "System_Ext" {
                        "external_system"
                    } else {
                        "system"
                    };
                    shapes.push(C4Shape {
                        alias: parsed.args.first().cloned().unwrap_or_default(),
                        type_c4: type_c4.to_string(),
                        label: parsed.args.get(1).cloned().unwrap_or_default(),
                        techn: String::new(),
                        descr: parsed.args.get(2).cloned().unwrap_or_default(),
                        parent_boundary: current_boundary.clone(),
                        x: 0.0,
                        y: 0.0,
                        width: 0.0,
                        height: 0.0,
                    });
                }
                "SystemDb" | "SystemDb_Ext" => {
                    let type_c4 = if parsed.func.contains("Ext") {
                        "external_system_db"
                    } else {
                        "system_db"
                    };
                    shapes.push(C4Shape {
                        alias: parsed.args.first().cloned().unwrap_or_default(),
                        type_c4: type_c4.to_string(),
                        label: parsed.args.get(1).cloned().unwrap_or_default(),
                        techn: String::new(),
                        descr: parsed.args.get(2).cloned().unwrap_or_default(),
                        parent_boundary: current_boundary.clone(),
                        x: 0.0,
                        y: 0.0,
                        width: 0.0,
                        height: 0.0,
                    });
                }
                "SystemQueue" | "SystemQueue_Ext" => {
                    let type_c4 = if parsed.func.contains("Ext") {
                        "external_system_queue"
                    } else {
                        "system_queue"
                    };
                    shapes.push(C4Shape {
                        alias: parsed.args.first().cloned().unwrap_or_default(),
                        type_c4: type_c4.to_string(),
                        label: parsed.args.get(1).cloned().unwrap_or_default(),
                        techn: String::new(),
                        descr: parsed.args.get(2).cloned().unwrap_or_default(),
                        parent_boundary: current_boundary.clone(),
                        x: 0.0,
                        y: 0.0,
                        width: 0.0,
                        height: 0.0,
                    });
                }
                "Container" | "Container_Ext" => {
                    let type_c4 = if parsed.func.contains("Ext") {
                        "external_container"
                    } else {
                        "container"
                    };
                    shapes.push(C4Shape {
                        alias: parsed.args.first().cloned().unwrap_or_default(),
                        type_c4: type_c4.to_string(),
                        label: parsed.args.get(1).cloned().unwrap_or_default(),
                        techn: parsed.args.get(2).cloned().unwrap_or_default(),
                        descr: parsed.args.get(3).cloned().unwrap_or_default(),
                        parent_boundary: current_boundary.clone(),
                        x: 0.0,
                        y: 0.0,
                        width: 0.0,
                        height: 0.0,
                    });
                }
                "ContainerDb" | "ContainerDb_Ext" => {
                    let type_c4 = if parsed.func.contains("Ext") {
                        "external_container_db"
                    } else {
                        "container_db"
                    };
                    shapes.push(C4Shape {
                        alias: parsed.args.first().cloned().unwrap_or_default(),
                        type_c4: type_c4.to_string(),
                        label: parsed.args.get(1).cloned().unwrap_or_default(),
                        techn: parsed.args.get(2).cloned().unwrap_or_default(),
                        descr: parsed.args.get(3).cloned().unwrap_or_default(),
                        parent_boundary: current_boundary.clone(),
                        x: 0.0,
                        y: 0.0,
                        width: 0.0,
                        height: 0.0,
                    });
                }
                "ContainerQueue" | "ContainerQueue_Ext" => {
                    let type_c4 = if parsed.func.contains("Ext") {
                        "external_container_queue"
                    } else {
                        "container_queue"
                    };
                    shapes.push(C4Shape {
                        alias: parsed.args.first().cloned().unwrap_or_default(),
                        type_c4: type_c4.to_string(),
                        label: parsed.args.get(1).cloned().unwrap_or_default(),
                        techn: parsed.args.get(2).cloned().unwrap_or_default(),
                        descr: parsed.args.get(3).cloned().unwrap_or_default(),
                        parent_boundary: current_boundary.clone(),
                        x: 0.0,
                        y: 0.0,
                        width: 0.0,
                        height: 0.0,
                    });
                }
                "Component" | "Component_Ext" => {
                    let type_c4 = if parsed.func.contains("Ext") {
                        "external_component"
                    } else {
                        "component"
                    };
                    shapes.push(C4Shape {
                        alias: parsed.args.first().cloned().unwrap_or_default(),
                        type_c4: type_c4.to_string(),
                        label: parsed.args.get(1).cloned().unwrap_or_default(),
                        techn: parsed.args.get(2).cloned().unwrap_or_default(),
                        descr: parsed.args.get(3).cloned().unwrap_or_default(),
                        parent_boundary: current_boundary.clone(),
                        x: 0.0,
                        y: 0.0,
                        width: 0.0,
                        height: 0.0,
                    });
                }
                "ComponentDb" | "ComponentDb_Ext" => {
                    let type_c4 = if parsed.func.contains("Ext") {
                        "external_component_db"
                    } else {
                        "component_db"
                    };
                    shapes.push(C4Shape {
                        alias: parsed.args.first().cloned().unwrap_or_default(),
                        type_c4: type_c4.to_string(),
                        label: parsed.args.get(1).cloned().unwrap_or_default(),
                        techn: parsed.args.get(2).cloned().unwrap_or_default(),
                        descr: parsed.args.get(3).cloned().unwrap_or_default(),
                        parent_boundary: current_boundary.clone(),
                        x: 0.0,
                        y: 0.0,
                        width: 0.0,
                        height: 0.0,
                    });
                }
                "ComponentQueue" | "ComponentQueue_Ext" => {
                    let type_c4 = if parsed.func.contains("Ext") {
                        "external_component_queue"
                    } else {
                        "component_queue"
                    };
                    shapes.push(C4Shape {
                        alias: parsed.args.first().cloned().unwrap_or_default(),
                        type_c4: type_c4.to_string(),
                        label: parsed.args.get(1).cloned().unwrap_or_default(),
                        techn: parsed.args.get(2).cloned().unwrap_or_default(),
                        descr: parsed.args.get(3).cloned().unwrap_or_default(),
                        parent_boundary: current_boundary.clone(),
                        x: 0.0,
                        y: 0.0,
                        width: 0.0,
                        height: 0.0,
                    });
                }
                "Rel" | "Rel_U" | "Rel_D" | "Rel_L" | "Rel_R" | "Rel_Back" | "Rel_Neighbor" => {
                    rels.push(C4Rel {
                        rel_type: "rel".to_string(),
                        from: parsed.args.first().cloned().unwrap_or_default(),
                        to: parsed.args.get(1).cloned().unwrap_or_default(),
                        label: parsed.args.get(2).cloned().unwrap_or_default(),
                        techn: parsed.args.get(3).cloned().unwrap_or_default(),
                    });
                }
                "BiRel" | "BiRel_U" | "BiRel_D" | "BiRel_L" | "BiRel_R" | "BiRel_Neighbor" => {
                    rels.push(C4Rel {
                        rel_type: "birel".to_string(),
                        from: parsed.args.first().cloned().unwrap_or_default(),
                        to: parsed.args.get(1).cloned().unwrap_or_default(),
                        label: parsed.args.get(2).cloned().unwrap_or_default(),
                        techn: parsed.args.get(3).cloned().unwrap_or_default(),
                    });
                }
                "Boundary"
                | "Enterprise_Boundary"
                | "System_Boundary"
                | "Container_Boundary"
                | "Deployment_Node"
                | "Deployment_Node_L"
                | "Deployment_Node_R" => {
                    let alias = parsed.args.first().cloned().unwrap_or_default();
                    let label = parsed.args.get(1).cloned().unwrap_or_default();
                    let type_text = parsed.args.get(2).cloned().unwrap_or_default();
                    boundaries.push(C4Boundary {
                        alias: alias.clone(),
                        label,
                        type_text,
                        parent_boundary: current_boundary.clone(),
                        x: 0.0,
                        y: 0.0,
                        width: 0.0,
                        height: 0.0,
                    });
                    boundary_stack.push(alias.clone());
                    current_boundary = alias;
                }
                "UpdateElementStyle" | "UpdateRelStyle" | "UpdateLayoutConfig" => {
                    // Styling updates – ignored for now
                }
                _ => {
                    // Unknown statement – skip
                }
            }
        }
    }

    if !found_header {
        return Err(MermaidError::ParseError {
            line: 1,
            message: "Expected C4 diagram type declaration".to_string(),
        });
    }

    Ok(C4Diagram {
        c4_type,
        title,
        shapes,
        boundaries,
        rels,
    })
}

struct ParsedStatement {
    func: String,
    args: Vec<String>,
}

/// Parse a line like `Person(alias, "Label", "Description")`
/// Also handles boundary openers: `Boundary(alias, "Label") {`
fn parse_c4_statement(line: &str) -> Option<ParsedStatement> {
    let line = line.trim();

    // Find the function name (everything before the first '(')
    let paren_pos = line.find('(')?;
    let func = line[..paren_pos].trim().to_string();
    if func.is_empty() {
        return None;
    }

    // Find the matching closing paren
    let rest = &line[paren_pos + 1..];
    let close_paren = find_matching_paren(rest)?;
    let args_str = &rest[..close_paren];

    let args = split_c4_args(args_str);

    Some(ParsedStatement { func, args })
}

fn find_matching_paren(s: &str) -> Option<usize> {
    let mut depth = 0i32;
    let mut in_quote = false;
    for (i, ch) in s.char_indices() {
        match ch {
            '"' => in_quote = !in_quote,
            '(' if !in_quote => depth += 1,
            ')' if !in_quote => {
                if depth == 0 {
                    return Some(i);
                }
                depth -= 1;
            }
            _ => {}
        }
    }
    None
}

fn split_c4_args(s: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_quote = false;
    let mut depth = 0i32;

    for ch in s.chars() {
        match ch {
            '"' => {
                in_quote = !in_quote;
                // Don't include the quotes in the output
            }
            '(' if !in_quote => {
                depth += 1;
                current.push(ch);
            }
            ')' if !in_quote => {
                depth -= 1;
                current.push(ch);
            }
            ',' if !in_quote && depth == 0 => {
                args.push(current.trim().to_string());
                current.clear();
            }
            _ => {
                current.push(ch);
            }
        }
    }
    let last = current.trim().to_string();
    if !last.is_empty() {
        args.push(last);
    }
    args
}

// ── SVG Rendering ──────────────────────────────────────────────────────────

fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

pub fn render_c4_diagram_to_svg(
    mermaid_source: &str,
    theme: &MermaidTheme,
) -> Result<String, MermaidError> {
    let mut diagram = parse_c4_diagram(mermaid_source)?;

    // Layout: place shapes in global boundary using Bounds
    let mut screen_bounds = Bounds::new();
    screen_bounds.set_data(
        DIAGRAM_MARGIN_X,
        DIAGRAM_MARGIN_X,
        DIAGRAM_MARGIN_Y,
        DIAGRAM_MARGIN_Y,
    );
    screen_bounds.width_limit = 800.0; // Puppeteer default viewport width (matches mermaid-cli)

    // Collect shapes for the global boundary
    let mut global_shapes: Vec<usize> = Vec::new();
    let mut boundary_shapes: std::collections::HashMap<String, Vec<usize>> =
        std::collections::HashMap::new();

    for (i, shape) in diagram.shapes.iter().enumerate() {
        if shape.parent_boundary == "global" {
            global_shapes.push(i);
        } else {
            boundary_shapes
                .entry(shape.parent_boundary.clone())
                .or_default()
                .push(i);
        }
    }

    // Layout global shapes
    {
        let mut shapes_to_layout: Vec<C4Shape> = global_shapes
            .iter()
            .map(|&i| diagram.shapes[i].clone())
            .collect();
        layout_shapes_in_bounds(&mut screen_bounds, &mut shapes_to_layout);
        for (j, &orig_idx) in global_shapes.iter().enumerate() {
            diagram.shapes[orig_idx] = shapes_to_layout[j].clone();
        }
    }

    // Layout shapes inside boundaries
    for boundary in &mut diagram.boundaries {
        if boundary.alias == "global" {
            continue;
        }
        if let Some(indices) = boundary_shapes.get(&boundary.alias) {
            let mut inner_bounds = Bounds::new();
            let parent_y = screen_bounds.stopy;
            inner_bounds.set_data(
                screen_bounds.startx + DIAGRAM_MARGIN_X,
                screen_bounds.startx + DIAGRAM_MARGIN_X,
                parent_y + DIAGRAM_MARGIN_Y + 30.0, // leave room for boundary header
                parent_y + DIAGRAM_MARGIN_Y + 30.0,
            );
            inner_bounds.width_limit = screen_bounds.width_limit;

            let mut shapes_to_layout: Vec<C4Shape> =
                indices.iter().map(|&i| diagram.shapes[i].clone()).collect();
            layout_shapes_in_bounds(&mut inner_bounds, &mut shapes_to_layout);
            for (j, &orig_idx) in indices.iter().enumerate() {
                diagram.shapes[orig_idx] = shapes_to_layout[j].clone();
            }

            boundary.x = inner_bounds.startx;
            boundary.y = inner_bounds.starty - 30.0;
            boundary.width = inner_bounds.stopx - inner_bounds.startx;
            boundary.height = inner_bounds.stopy - inner_bounds.starty + 30.0;

            screen_bounds.stopy = screen_bounds
                .stopy
                .max(inner_bounds.stopy + C4_SHAPE_MARGIN);
            screen_bounds.stopx = screen_bounds
                .stopx
                .max(inner_bounds.stopx + C4_SHAPE_MARGIN);
        }
    }

    let mut global_max_x = screen_bounds.stopx;
    let mut global_max_y = screen_bounds.stopy;
    for shape in &diagram.shapes {
        global_max_x = global_max_x.max(shape.x + shape.width + C4_SHAPE_MARGIN);
        global_max_y = global_max_y.max(shape.y + shape.height + C4_SHAPE_MARGIN);
    }

    let box_width = global_max_x - screen_bounds.startx;
    let box_height = global_max_y - screen_bounds.starty;
    let svg_width = box_width + 2.0 * DIAGRAM_MARGIN_X;
    let svg_height = box_height + 2.0 * DIAGRAM_MARGIN_Y;

    let extra_vert_for_title = if !diagram.title.is_empty() { 60.0 } else { 0.0 };

    let vb_x = screen_bounds.startx - DIAGRAM_MARGIN_X;
    let vb_y = -(DIAGRAM_MARGIN_Y + extra_vert_for_title);
    let vb_w = svg_width;
    let vb_h = svg_height + extra_vert_for_title;

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

    // SVG header
    svg.push_str(&format!(
        "<svg id=\"my-svg\" width=\"100%\" xmlns=\"http://www.w3.org/2000/svg\" xmlns:xlink=\"http://www.w3.org/1999/xlink\" style=\"max-width: {max_w}px; background-color: {background_color};\" viewBox=\"{vb_x} {vb_y} {vb_w} {vb_h}\" role=\"graphics-document document\" aria-roledescription=\"c4\">",
        max_w = svg_width.ceil() as i64,
    ));

    // Style block
    svg.push_str(&format!(
        "<style>#my-svg{{font-family:\"trebuchet ms\",verdana,arial,sans-serif;font-size:16px;fill:{text_color};}}@keyframes edge-animation-frame{{from{{stroke-dashoffset:0;}}}}@keyframes dash{{to{{stroke-dashoffset:0;}}}}#my-svg .edge-animation-slow{{stroke-dasharray:9,5!important;stroke-dashoffset:900;animation:dash 50s linear infinite;stroke-linecap:round;}}#my-svg .edge-animation-fast{{stroke-dasharray:9,5!important;stroke-dashoffset:900;animation:dash 20s linear infinite;stroke-linecap:round;}}#my-svg .error-icon{{fill:#552222;}}#my-svg .error-text{{fill:#552222;stroke:#552222;}}#my-svg .edge-thickness-normal{{stroke-width:1px;}}#my-svg .edge-thickness-thick{{stroke-width:3.5px;}}#my-svg .edge-pattern-solid{{stroke-dasharray:0;}}#my-svg .edge-thickness-invisible{{stroke-width:0;fill:none;}}#my-svg .edge-pattern-dashed{{stroke-dasharray:3;}}#my-svg .edge-pattern-dotted{{stroke-dasharray:2;}}#my-svg .marker{{fill:{edge};stroke:{edge};}}#my-svg .marker.cross{{stroke:{edge};}}#my-svg svg{{font-family:\"trebuchet ms\",verdana,arial,sans-serif;font-size:16px;}}#my-svg p{{margin:0;}}#my-svg .person{{stroke:hsl(240, 60%, 86.2745098039%);fill:#ECECFF;}}#my-svg :root{{--mermaid-font-family:\"trebuchet ms\",verdana,arial,sans-serif;}}</style>",
        edge = theme.edge_color
    ));

    svg.push_str("<g/>");

    // Defs: icons (computer, database, clock)
    render_icon_defs(&mut svg);

    // Render boundaries (non-global)
    for boundary in &diagram.boundaries {
        if boundary.alias == "global" || boundary.width < 1.0 {
            continue;
        }
        render_boundary(&mut svg, boundary);
    }

    // Render shapes
    for shape in &diagram.shapes {
        render_c4_shape(&mut svg, shape);
    }

    // Arrow marker defs
    render_arrow_defs(&mut svg);

    // Render relationships
    render_rels(&mut svg, &diagram.rels, &diagram.shapes, &diagram.c4_type);

    // Title
    if !diagram.title.is_empty() {
        let title_x = (global_max_x - screen_bounds.startx) / 2.0 - 4.0 * DIAGRAM_MARGIN_X;
        let title_y = screen_bounds.starty + DIAGRAM_MARGIN_Y;
        svg.push_str(&format!(
            "<text x=\"{title_x}\" y=\"{title_y}\">{}</text>",
            escape_xml(&diagram.title)
        ));
    }

    svg.push_str("</svg>");

    Ok(svg)
}

fn render_icon_defs(svg: &mut String) {
    // Computer icon
    svg.push_str("<defs><symbol id=\"computer\" width=\"24\" height=\"24\"><path transform=\"scale(.5)\" d=\"M2 2v13h20v-13h-20zm18 11h-16v-9h16v9zm-10.228 6l.466-1h3.524l.467 1h-4.457zm14.228 3h-24l2-6h2.104l-1.33 4h18.45l-1.297-4h2.073l2 6zm-5-10h-14v-7h14v7z\"/></symbol></defs>");

    // Database icon (long path omitted for brevity — using same as reference)
    svg.push_str("<defs><symbol id=\"database\" fill-rule=\"evenodd\" clip-rule=\"evenodd\"><path transform=\"scale(.5)\" d=\"M12.258.001l.256.004.255.005.253.008.251.01.249.012.247.015.246.016.242.019.241.02.239.023.236.024.233.027.231.028.229.031.225.032.223.034.22.036.217.038.214.04.211.041.208.043.205.045.201.046.198.048.194.05.191.051.187.053.183.054.18.056.175.057.172.059.168.06.163.061.16.063.155.064.15.066.074.033.073.033.071.034.07.034.069.035.068.035.067.035.066.035.064.036.064.036.062.036.06.036.06.037.058.037.058.037.055.038.055.038.053.038.052.038.051.039.05.039.048.039.047.039.045.04.044.04.043.04.041.04.04.041.039.041.037.041.036.041.034.041.033.042.032.042.03.042.029.042.027.042.026.043.024.043.023.043.021.043.02.043.018.044.017.043.015.044.013.044.012.044.011.045.009.044.007.045.006.045.004.045.002.045.001.045v17l-.001.045-.002.045-.004.045-.006.045-.007.045-.009.044-.011.045-.012.044-.013.044-.015.044-.017.043-.018.044-.02.043-.021.043-.023.043-.024.043-.026.043-.027.042-.029.042-.03.042-.032.042-.033.042-.034.041-.036.041-.037.041-.039.041-.04.041-.041.04-.043.04-.044.04-.045.04-.047.039-.048.039-.05.039-.051.039-.052.038-.053.038-.055.038-.055.038-.058.037-.058.037-.06.037-.06.036-.062.036-.064.036-.064.036-.066.035-.067.035-.068.035-.069.035-.07.034-.071.034-.073.033-.074.033-.15.066-.155.064-.16.063-.163.061-.168.06-.172.059-.175.057-.18.056-.183.054-.187.053-.191.051-.194.05-.198.048-.201.046-.205.045-.208.043-.211.041-.214.04-.217.038-.22.036-.223.034-.225.032-.229.031-.231.028-.233.027-.236.024-.239.023-.241.02-.242.019-.246.016-.247.015-.249.012-.251.01-.253.008-.255.005-.256.004-.258.001-.258-.001-.256-.004-.255-.005-.253-.008-.251-.01-.249-.012-.247-.015-.245-.016-.243-.019-.241-.02-.238-.023-.236-.024-.234-.027-.231-.028-.228-.031-.226-.032-.223-.034-.22-.036-.217-.038-.214-.04-.211-.041-.208-.043-.204-.045-.201-.046-.198-.048-.195-.05-.19-.051-.187-.053-.184-.054-.179-.056-.176-.057-.172-.059-.167-.06-.164-.061-.159-.063-.155-.064-.151-.066-.074-.033-.072-.033-.072-.034-.07-.034-.069-.035-.068-.035-.067-.035-.066-.035-.064-.036-.063-.036-.062-.036-.061-.036-.06-.037-.058-.037-.057-.037-.056-.038-.055-.038-.053-.038-.052-.038-.051-.039-.049-.039-.049-.039-.046-.039-.046-.04-.044-.04-.043-.04-.041-.04-.04-.041-.039-.041-.037-.041-.036-.041-.034-.041-.033-.042-.032-.042-.03-.042-.029-.042-.027-.042-.026-.043-.024-.043-.023-.043-.021-.043-.02-.043-.018-.044-.017-.043-.015-.044-.013-.044-.012-.044-.011-.045-.009-.044-.007-.045-.006-.045-.004-.045-.002-.045-.001-.045v-17l.001-.045.002-.045.004-.045.006-.045.007-.045.009-.044.011-.045.012-.044.013-.044.015-.044.017-.043.018-.044.02-.043.021-.043.023-.043.024-.043.026-.043.027-.042.029-.042.03-.042.032-.042.033-.042.034-.041.036-.041.037-.041.039-.041.04-.041.041-.04.043-.04.044-.04.046-.04.046-.039.049-.039.049-.039.051-.039.052-.038.053-.038.055-.038.056-.038.057-.037.058-.037.06-.037.061-.036.062-.036.063-.036.064-.036.066-.035.067-.035.068-.035.069-.035.07-.034.072-.034.072-.033.074-.033.151-.066.155-.064.159-.063.164-.061.167-.06.172-.059.176-.057.179-.056.184-.054.187-.053.19-.051.195-.05.198-.048.201-.046.204-.045.208-.043.211-.041.214-.04.217-.038.22-.036.223-.034.226-.032.228-.031.231-.028.234-.027.236-.024.238-.023.241-.02.243-.019.245-.016.247-.015.249-.012.251-.01.253-.008.255-.005.256-.004.258-.001.258.001z\"/></symbol></defs>");

    // Clock icon
    svg.push_str("<defs><symbol id=\"clock\" width=\"24\" height=\"24\"><path transform=\"scale(.5)\" d=\"M12 2c5.514 0 10 4.486 10 10s-4.486 10-10 10-10-4.486-10-10 4.486-10 10-10zm0-2c-6.627 0-12 5.373-12 12s5.373 12 12 12 12-5.373 12-12-5.373-12-12-12zm5.848 12.459c.202.038.202.333.001.372-1.907.361-6.045 1.111-6.547 1.111-.719 0-1.301-.582-1.301-1.301 0-.512.77-5.447 1.125-7.445.034-.192.312-.181.343.014l.985 6.238 5.394 1.011z\"/></symbol></defs>");
}

fn render_arrow_defs(svg: &mut String) {
    svg.push_str("<defs><marker id=\"arrowhead\" refX=\"9\" refY=\"5\" markerUnits=\"userSpaceOnUse\" markerWidth=\"12\" markerHeight=\"12\" orient=\"auto\"><path d=\"M 0 0 L 10 5 L 0 10 z\"/></marker></defs>");
    svg.push_str("<defs><marker id=\"arrowend\" refX=\"1\" refY=\"5\" markerUnits=\"userSpaceOnUse\" markerWidth=\"12\" markerHeight=\"12\" orient=\"auto\"><path d=\"M 10 0 L 0 5 L 10 10 z\"/></marker></defs>");
    svg.push_str("<defs><marker id=\"crosshead\" markerWidth=\"15\" markerHeight=\"8\" orient=\"auto\" refX=\"16\" refY=\"4\"><path fill=\"black\" stroke=\"#000000\" stroke-width=\"1px\" d=\"M 9,2 V 6 L16,4 Z\" style=\"stroke-dasharray: 0, 0;\"/><path fill=\"none\" stroke=\"#000000\" stroke-width=\"1px\" d=\"M 0,1 L 6,7 M 6,1 L 0,7\" style=\"stroke-dasharray: 0, 0;\"/></marker></defs>");
    svg.push_str("<defs><marker id=\"filled-head\" refX=\"18\" refY=\"7\" markerWidth=\"20\" markerHeight=\"28\" orient=\"auto\"><path d=\"M 18,7 L9,13 L14,7 L9,1 Z\"/></marker></defs>");
}

fn render_boundary(svg: &mut String, boundary: &C4Boundary) {
    let stroke_color = "#444444";
    let font_color = "black";

    svg.push_str(&format!(
        "<g><rect x=\"{x}\" y=\"{y}\" fill=\"none\" stroke=\"{stroke_color}\" width=\"{w}\" height=\"{h}\" rx=\"2.5\" ry=\"2.5\" stroke-width=\"1\" stroke-dasharray=\"7.0,7.0\"/>",
        x = boundary.x,
        y = boundary.y,
        w = boundary.width,
        h = boundary.height,
    ));

    // Boundary label (bold, +2 font size)
    let label_y = boundary.y + C4_SHAPE_MARGIN - 35.0;
    svg.push_str(&format!(
        "<text x=\"{x}\" y=\"{y}\" dominant-baseline=\"middle\" fill=\"{font_color}\" style=\"text-anchor: middle; font-size: {fs}px; font-weight: bold; font-family: {FONT_FAMILY};\">{label}</text>",
        x = boundary.x + boundary.width / 2.0,
        y = label_y,
        fs = FONT_SIZE + 2.0,
        label = escape_xml(&boundary.label),
    ));

    // Boundary type
    if !boundary.type_text.is_empty() {
        let type_y = label_y + FONT_SIZE + 5.0;
        svg.push_str(&format!(
            "<text x=\"{x}\" y=\"{y}\" dominant-baseline=\"middle\" fill=\"{font_color}\" style=\"text-anchor: middle; font-size: {fs}px; font-weight: normal; font-family: {FONT_FAMILY};\">[{type_text}]</text>",
            x = boundary.x + boundary.width / 2.0,
            y = type_y,
            fs = FONT_SIZE,
            type_text = escape_xml(&boundary.type_text),
        ));
    }

    svg.push_str("</g>");
}

fn render_c4_shape(svg: &mut String, shape: &C4Shape) {
    let fill = bg_color_for(&shape.type_c4);
    let stroke = border_color_for(&shape.type_c4);
    let font_color = "#FFFFFF";

    svg.push_str("<g class=\"person-man\">");

    // Shape background
    match shape.type_c4.as_str() {
        "system_db"
        | "external_system_db"
        | "container_db"
        | "external_container_db"
        | "component_db"
        | "external_component_db" => {
            // Database cylinder shape
            let half = shape.width / 2.0;
            svg.push_str(&format!(
                "<path fill=\"{fill}\" stroke-width=\"0.5\" stroke=\"{stroke}\" d=\"M{x},{y}c0,-10 {half},-10 {half},-10c0,0 {half},0 {half},10l0,{h}c0,10 -{half},10 -{half},10c0,0 -{half},0 -{half},-10l0,-{h}\"/>",
                x = shape.x,
                y = shape.y,
                h = shape.height,
            ));
            svg.push_str(&format!(
                "<path fill=\"none\" stroke-width=\"0.5\" stroke=\"{stroke}\" d=\"M{x},{y}c0,10 {half},10 {half},10c0,0 {half},0 {half},-10\"/>",
                x = shape.x,
                y = shape.y,
            ));
        }
        "system_queue"
        | "external_system_queue"
        | "container_queue"
        | "external_container_queue"
        | "component_queue"
        | "external_component_queue" => {
            // Queue shape
            let half = shape.height / 2.0;
            svg.push_str(&format!(
                "<path fill=\"{fill}\" stroke-width=\"0.5\" stroke=\"{stroke}\" d=\"M{x},{y}l{w},0c5,0 5,{half} 5,{half}c0,0 0,{half} -5,{half}l-{w},0c-5,0 -5,-{half} -5,-{half}c0,0 0,-{half} 5,-{half}\"/>",
                x = shape.x,
                y = shape.y,
                w = shape.width,
            ));
            svg.push_str(&format!(
                "<path fill=\"none\" stroke-width=\"0.5\" stroke=\"{stroke}\" d=\"M{x},{y}c-5,0 -5,{half} -5,{half}c0,{half} 5,{half} 5,{half}\"/>",
                x = shape.x + shape.width,
                y = shape.y,
            ));
        }
        _ => {
            // Normal rectangle
            svg.push_str(&format!(
                "<rect x=\"{x}\" y=\"{y}\" fill=\"{fill}\" stroke=\"{stroke}\" width=\"{w}\" height=\"{h}\" rx=\"2.5\" ry=\"2.5\" stroke-width=\"0.5\"/>",
                x = shape.x,
                y = shape.y,
                w = shape.width,
                h = shape.height,
            ));
        }
    }

    // Type label (e.g., <<person>>)
    let type_font_size = FONT_SIZE - 2.0;
    let type_text = format!("<<{}>>", shape.type_c4);
    let type_text_width = estimate_text_width(&type_text, type_font_size);
    let type_y = shape.y + C4_SHAPE_PADDING;
    svg.push_str(&format!(
        "<text fill=\"{font_color}\" font-family=\"{FONT_FAMILY}\" font-size=\"{type_font_size}\" font-style=\"italic\" lengthAdjust=\"spacing\" textLength=\"{type_text_width}\" x=\"{x}\" y=\"{y}\">{text}</text>",
        x = shape.x + shape.width / 2.0 - type_text_width / 2.0,
        y = type_y,
        text = escape_xml(&type_text),
    ));

    let mut current_y = type_y + type_font_size + 2.0 - 4.0;

    // Person image
    if shape.type_c4 == "person" || shape.type_c4 == "external_person" {
        let img_src = if shape.type_c4 == "external_person" {
            EXTERNAL_PERSON_IMG
        } else {
            PERSON_IMG
        };
        let img_x = shape.x + shape.width / 2.0 - 24.0;
        svg.push_str(&format!(
            "<image width=\"48\" height=\"48\" x=\"{img_x}\" y=\"{current_y}\" xlink:href=\"{img_src}\"/>"
        ));
        current_y += 48.0;
    }

    // Label (bold, +2 font size)
    current_y += 8.0;
    let label_font_size = FONT_SIZE + 2.0;
    svg.push_str(&format!(
        "<text x=\"{x}\" y=\"{y}\" dominant-baseline=\"middle\" fill=\"{font_color}\" style=\"text-anchor: middle; font-size: {label_font_size}px; font-weight: bold; font-family: {FONT_FAMILY};\"><tspan dy=\"0\" alignment-baseline=\"mathematical\">{text}</tspan></text>",
        x = shape.x + shape.width / 2.0,
        y = current_y,
        text = escape_xml(&shape.label),
    ));
    current_y += estimate_text_height(label_font_size);

    // Techn (italic)
    if !shape.techn.is_empty() {
        current_y += 5.0;
        let techn_display = format!("[{}]", shape.techn);
        svg.push_str(&format!(
            "<text x=\"{x}\" y=\"{y}\" dominant-baseline=\"middle\" fill=\"{font_color}\" style=\"text-anchor: middle; font-size: {fs}px; font-weight: normal; font-style: italic; font-family: {FONT_FAMILY};\"><tspan dy=\"0\" alignment-baseline=\"mathematical\">{text}</tspan></text>",
            x = shape.x + shape.width / 2.0,
            y = current_y,
            fs = FONT_SIZE,
            text = escape_xml(&techn_display),
        ));
        current_y += estimate_text_height(FONT_SIZE);
    }

    // Description
    if !shape.descr.is_empty() {
        current_y += 20.0;
        svg.push_str(&format!(
            "<text x=\"{x}\" y=\"{y}\" dominant-baseline=\"middle\" fill=\"{font_color}\" style=\"text-anchor: middle; font-size: {fs}px; font-weight: normal; font-family: {FONT_FAMILY};\"><tspan dy=\"0\" alignment-baseline=\"mathematical\">{text}</tspan></text>",
            x = shape.x + shape.width / 2.0,
            y = current_y,
            fs = FONT_SIZE,
            text = escape_xml(&shape.descr),
        ));
    }

    svg.push_str("</g>");
}

fn render_rels(svg: &mut String, rels: &[C4Rel], shapes: &[C4Shape], c4_type: &str) {
    if rels.is_empty() {
        return;
    }

    svg.push_str("<g>");
    for (i, rel) in rels.iter().enumerate() {
        let from_shape = shapes.iter().find(|s| s.alias == rel.from);
        let to_shape = shapes.iter().find(|s| s.alias == rel.to);

        let (from_shape, to_shape) = match (from_shape, to_shape) {
            (Some(f), Some(t)) => (f, t),
            _ => continue,
        };

        let (start, end) = get_intersect_points(from_shape, to_shape);
        let text_color = "#444444";
        let stroke_color = "#444444";

        // Display label
        let label = if c4_type == "C4Dynamic" {
            format!("{}: {}", i + 1, rel.label)
        } else {
            rel.label.clone()
        };

        // First rel gets a line, subsequent ones get a curved path
        if i == 0 {
            svg.push_str(&format!(
                "<line x1=\"{sx}\" y1=\"{sy}\" x2=\"{ex}\" y2=\"{ey}\" stroke-width=\"1\" stroke=\"{stroke_color}\" marker-end=\"url(#arrowhead)\" style=\"fill: none;\"/>",
                sx = start.0,
                sy = start.1,
                ex = end.0,
                ey = end.1,
            ));
        } else {
            let ctrl_x = start.0 + (end.0 - start.0) / 2.0 - (end.0 - start.0) / 4.0;
            let ctrl_y = start.1 + (end.1 - start.1) / 2.0;
            svg.push_str(&format!(
                "<path fill=\"none\" stroke-width=\"1\" stroke=\"{stroke_color}\" d=\"M{sx},{sy} Q{ctrl_x},{ctrl_y} {ex},{ey}\" marker-end=\"url(#arrowhead)\"/>",
                sx = start.0,
                sy = start.1,
                ex = end.0,
                ey = end.1,
            ));
        }

        // Rel label text
        let mid_x = start.0.min(end.0) + (end.0 - start.0).abs() / 2.0;
        let mid_y = start.1.min(end.1) + (end.1 - start.1).abs() / 2.0;

        svg.push_str(&format!(
            "<text x=\"{mid_x}\" y=\"{mid_y}\" dominant-baseline=\"middle\" fill=\"{text_color}\" style=\"text-anchor: middle; font-size: {MESSAGE_FONT_SIZE}px; font-weight: normal; font-family: {FONT_FAMILY};\"><tspan dy=\"0\" alignment-baseline=\"mathematical\">{text}</tspan></text>",
            text = escape_xml(&label),
        ));

        // Techn text (italic, below label)
        if !rel.techn.is_empty() {
            let techn_display = format!("[{}]", rel.techn);
            let techn_y = mid_y + MESSAGE_FONT_SIZE + 5.0;
            svg.push_str(&format!(
                "<text x=\"{mid_x}\" y=\"{techn_y}\" dominant-baseline=\"middle\" fill=\"{text_color}\" style=\"text-anchor: middle; font-size: {MESSAGE_FONT_SIZE}px; font-weight: normal; font-style: italic; font-family: {FONT_FAMILY};\"><tspan dy=\"0\" alignment-baseline=\"mathematical\">{text}</tspan></text>",
                text = escape_xml(&techn_display),
            ));
        }
    }
    svg.push_str("</g>");
}
