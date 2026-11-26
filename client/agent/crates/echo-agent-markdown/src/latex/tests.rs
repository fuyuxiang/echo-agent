use super::*;

fn inline(src: &str) -> String {
    latex_to_unicode_inline(src).expect("within size limit")
}

fn display(src: &str) -> Vec<String> {
    latex_to_unicode_display(src).expect("within size limit")
}

#[test]
fn plain_expression_passes_through() {
    assert_eq!(inline("E = mc"), "E = mc");
}

#[test]
fn superscripts_map_to_unicode() {
    assert_eq!(inline("E = mc^2"), "E = mc²");
    assert_eq!(inline("x^{10}"), "x¹⁰");
    assert_eq!(inline("e^{-x}"), "e⁻ˣ");
    assert_eq!(inline("x^T"), "xᵀ");
}

#[test]
fn subscripts_map_to_unicode() {
    assert_eq!(inline("a_1 + a_2"), "a₁ + a₂");
    assert_eq!(inline("x_{ij}"), "xᵢⱼ");
}

#[test]
fn script_fallback_uses_parens() {
    // φ has no superscript form → fall back to ^(...)
    assert_eq!(inline("x^{\\alpha\\beta}"), "x^(αβ)");
    assert_eq!(inline("x^\\alpha"), "x^α");
    // Single unmappable subscript char.
    assert_eq!(inline("a_q"), "a_q");
}

#[test]
fn wordlike_scripts_fall_back_to_parens() {
    // Text-family commands mark the atom as a word → no modifier-letter runs
    // (`pₜₒᵣₛₒ` is unreadable and gappy in many terminal fonts).
    assert_eq!(inline("p_{\\text{torso}}"), "p_(torso)");
    assert_eq!(inline("z_{\\mathrm{draft}}"), "z_(draft)");
    assert_eq!(inline("x^{\\text{opt}}"), "x^(opt)");
    // 3+ letter runs read as words even without \text.
    assert_eq!(inline("x_{max}"), "x_(max)");
    assert_eq!(inline("z_{torso}"), "z_(torso)");
}

#[test]
fn indexlike_scripts_keep_unicode_forms() {
    // 1–2 letter runs are index juxtapositions, not words.
    assert_eq!(inline("x_{ij}"), "xᵢⱼ");
    assert_eq!(inline("T_{i+1}"), "Tᵢ₊₁");
    assert_eq!(inline("n^{th}"), "nᵗʰ");
    assert_eq!(inline("\\sum_{i=0}^{2} \\gamma^{i}"), "∑ᵢ₌₀² γⁱ");
}

#[test]
fn boxed_renders_content_without_frame() {
    assert_eq!(inline("\\boxed{x = 1}"), "x = 1");
    assert_eq!(inline("\\boxed{\\mathcal{L}}"), "ℒ");
    assert_eq!(inline("\\fbox{done}"), "done");
    // Math typography applies inside \boxed (math mode) …
    assert_eq!(inline("\\boxed{a - b}"), "a − b");
    // … but not inside \fbox (text mode).
    assert_eq!(inline("\\fbox{a-b}"), "a-b");
}

#[test]
fn mtp_loss_equation_converts_fully() {
    // A complex real-world equation: every command must
    // convert — no literal command names in the output.
    let src = "\\boxed{\n\\mathcal{L}_{\\text{MTP}}\n=\n\\sum_{i=0}^{2}\n\\gamma^{i}\\,\n\\mathbb{E}_{\\text{positions, mask}}\n\\Big[\n\\mathrm{KL}\\big(\n  \\mathrm{softmax}(z_{\\text{torso}}^{(s_i)})\n  \\;\\big\\|\\;\n  \\mathrm{softmax}(z_{\\text{draft}}^{(i)})\n\\big)\n\\Big]\n}";
    let joined = inline(src);
    assert!(joined.contains("ℒ_(MTP)"), "got: {joined}");
    assert!(joined.contains("∑ᵢ₌₀²"), "got: {joined}");
    assert!(joined.contains("𝔼_(positions, mask)"), "got: {joined}");
    assert!(joined.contains("softmax(z_(torso)"), "got: {joined}");
    assert!(joined.contains("‖"), "got: {joined}");
    assert!(!joined.contains("boxed"), "got: {joined}");
    assert!(!joined.contains('\\'), "got: {joined}");
}

#[test]
fn greek_letters() {
    assert_eq!(inline("\\alpha + \\beta = \\Gamma"), "α + β = Γ");
    assert_eq!(inline("\\varepsilon \\varphi"), "ε φ");
}

#[test]
fn relations_and_operators() {
    assert_eq!(inline("a \\le b \\ne c \\times d"), "a ≤ b ≠ c × d");
    assert_eq!(inline("x \\in A \\cup B"), "x ∈ A ∪ B");
    assert_eq!(inline("p \\implies q"), "p ⟹ q");
    assert_eq!(inline("f: A \\to B"), "f: A → B");
}

#[test]
fn vulgar_and_general_fractions() {
    assert_eq!(inline("\\frac{1}{2}"), "½");
    assert_eq!(inline("\\frac{3}{4}"), "¾");
    assert_eq!(inline("\\frac{dy}{dx}"), "dy/dx");
    assert_eq!(inline("\\frac{a+b}{c}"), "(a+b)/c");
    assert_eq!(inline("\\frac{x}{y - z}"), "x/(y − z)");
}

#[test]
fn roots() {
    assert_eq!(inline("\\sqrt{x}"), "√x");
    assert_eq!(inline("\\sqrt{a + b}"), "√(a + b)");
    assert_eq!(inline("\\sqrt[3]{x}"), "∛x");
    assert_eq!(inline("\\sqrt[4]{x}"), "∜x");
    assert_eq!(inline("\\sqrt[n]{x}"), "ⁿ√x");
}

#[test]
fn text_commands_pass_content_through() {
    assert_eq!(inline("\\text{if } x > 0"), "if x > 0");
    assert_eq!(inline("\\mathrm{d}x"), "dx");
    assert_eq!(inline("\\operatorname{softmax}(z)"), "softmax(z)");
    // Text mode must not map `-` to minus.
    assert_eq!(inline("\\text{x-ray}"), "x-ray");
}

#[test]
fn alphabets() {
    assert_eq!(inline("\\mathbb{R}^n"), "ℝⁿ");
    assert_eq!(inline("\\mathbb{N} \\mathbb{Z} \\mathbb{Q}"), "ℕ ℤ ℚ");
    assert_eq!(inline("\\mathcal{L}"), "ℒ");
    assert_eq!(inline("\\mathcal{O}(n)"), "𝒪(n)");
    assert_eq!(inline("\\mathfrak{g}"), "𝔤");
    assert_eq!(inline("\\mathbf{v}"), "𝐯");
}

#[test]
fn accents_use_combining_marks() {
    assert_eq!(inline("\\hat{x}"), "x\u{0302}");
    assert_eq!(inline("\\bar{y}"), "y\u{0304}");
    assert_eq!(inline("\\vec{v}"), "v\u{20D7}");
    assert_eq!(inline("\\dot{q}"), "q\u{0307}");
    assert_eq!(inline("\\tilde\\theta"), "θ\u{0303}");
}

#[test]
fn left_right_and_spacing() {
    assert_eq!(inline("\\left( \\frac{1}{2} \\right)"), "( ½ )".to_string());
    assert_eq!(inline("\\left. x \\right|_0^1"), "x |₀¹");
    assert_eq!(inline("\\int f(x)\\,dx"), "∫ f(x) dx");
    assert_eq!(inline("a\\!b"), "ab");
    assert_eq!(inline("a \\quad b"), "a   b");
}

#[test]
fn named_function_operators() {
    assert_eq!(inline("\\sin(x) + \\cos(y)"), "sin(x) + cos(y)");
    assert_eq!(inline("\\lim_{x \\to 0} f(x)"), "lim_(x → 0) f(x)");
    assert_eq!(inline("\\log n"), "log n");
}

#[test]
fn integrals_and_sums_with_bounds() {
    assert_eq!(inline("\\int_0^\\infty e^{-x} dx"), "∫₀^∞ e⁻ˣ dx");
    assert_eq!(inline("\\sum_{i=1}^{n} a_i"), "∑ᵢ₌₁ⁿ aᵢ");
}

#[test]
fn minus_and_prime_typography() {
    assert_eq!(inline("a - b"), "a − b");
    assert_eq!(inline("f'(x)"), "f′(x)");
}

#[test]
fn not_negates_known_relations() {
    assert_eq!(inline("a \\not= b"), "a ≠ b");
    assert_eq!(inline("x \\not\\in S"), "x ∉ S");
    assert_eq!(inline("a \\not\\sim b"), "a ∼\u{0338} b");
}

#[test]
fn binomials_and_mod() {
    assert_eq!(inline("\\binom{n}{k}"), "C(n, k)");
    assert_eq!(inline("a \\equiv b \\pmod{m}"), "a ≡ b (mod m)");
    assert_eq!(inline("a \\bmod b"), "a mod b");
}

#[test]
fn row_breaks_join_inline_and_split_display() {
    assert_eq!(inline("a \\\\ b"), "a; b");
    assert_eq!(display("a \\\\ b"), vec!["a", "b"]);
}

#[test]
fn aligned_environment_strips_markers() {
    let lines = display("\\begin{aligned} x &= y + 1 \\\\ y &= 2 \\end{aligned}");
    assert_eq!(lines, vec!["x = y + 1", "y = 2"]);
}

#[test]
fn cases_environment_renders_brace_column() {
    let lines = display("f(x) = \\begin{cases} x & x > 0 \\\\ 0 & \\text{otherwise} \\end{cases}");
    assert_eq!(lines.len(), 2);
    assert!(lines[0].starts_with("f(x) = ⎧ x"), "got {lines:?}");
    assert!(lines[1].trim_start().starts_with("⎩ 0"), "got {lines:?}");
}

#[test]
fn pmatrix_pads_columns() {
    let lines = display("\\begin{pmatrix} 1 & 22 \\\\ 333 & 4 \\end{pmatrix}");
    assert_eq!(lines, vec!["⎛1    22⎞", "⎝333  4⎠"]);
}

#[test]
fn bmatrix_single_row_uses_flat_brackets() {
    assert_eq!(
        display("\\begin{bmatrix} a & b \\end{bmatrix}"),
        vec!["[a  b]"]
    );
}

#[test]
fn vmatrix_uses_bars() {
    let lines = display("\\begin{vmatrix} a & b \\\\ c & d \\end{vmatrix}");
    assert_eq!(lines, vec!["│a  b│", "│c  d│"]);
}

#[test]
fn matrix_with_prefix_aligns_as_box() {
    // The prefix must stay on the anchor row with the matrix body
    // aligned beneath — not glued to the first row only.
    let lines = display("A = \\begin{pmatrix} 1 & 2 \\\\ 3 & 4 \\end{pmatrix}");
    assert_eq!(lines, vec!["A = ⎛1  2⎞", "    ⎝3  4⎠"]);
}

#[test]
fn matrix_with_prefix_and_suffix_flows_on_anchor_row() {
    let lines =
        display("A = \\begin{pmatrix} 1 & 2 \\\\ 3 & 4 \\end{pmatrix}, \\quad \\det(A) = -2");
    assert_eq!(lines, vec!["A = ⎛1  2⎞,   det(A) = −2", "    ⎝3  4⎠"]);
}

#[test]
fn three_row_matrix_anchors_on_middle_row() {
    let lines = display("v = \\begin{pmatrix} 1 \\\\ 2 \\\\ 3 \\end{pmatrix} x");
    assert_eq!(lines, vec!["    ⎛1⎞", "v = ⎜2⎟ x", "    ⎝3⎠"]);
}

#[test]
fn cases_with_prefix_aligns_as_box() {
    let lines = display("f(x) = \\begin{cases} x & x > 0 \\\\ 0 & e \\end{cases}");
    assert_eq!(lines, vec!["f(x) = ⎧ x  x > 0", "       ⎩ 0  e"]);
}

#[test]
fn inline_matrix_renders_flat() {
    assert_eq!(
        inline("\\begin{pmatrix} 1 & 2 \\\\ 3 & 4 \\end{pmatrix}"),
        "(1  2; 3  4)"
    );
    assert_eq!(inline("\\begin{bmatrix} a \\\\ b \\end{bmatrix}"), "[a; b]");
}

#[test]
fn inline_cases_renders_flat() {
    assert_eq!(
        inline("\\begin{cases} x & x > 0 \\\\ 0 & e \\end{cases}"),
        "{x  x > 0; 0  e}"
    );
}

#[test]
fn two_matrices_on_one_line_share_rows() {
    let lines = display(
        "\\begin{pmatrix} 1 \\\\ 2 \\end{pmatrix} + \\begin{pmatrix} 3 \\\\ 4 \\end{pmatrix}",
    );
    assert_eq!(lines, vec!["⎛1⎞ + ⎛3⎞", "⎝2⎠   ⎝4⎠"]);
}

#[test]
fn row_break_then_matrix_does_not_disturb_previous_line() {
    let lines = display("a \\\\ B = \\begin{pmatrix} 1 \\\\ 2 \\end{pmatrix}");
    assert_eq!(lines, vec!["a", "B = ⎛1⎞", "    ⎝2⎠"]);
}

#[test]
fn unknown_environment_renders_rows() {
    let lines = display("\\begin{foo} a \\\\ b \\end{foo}");
    assert_eq!(lines, vec!["a", "b"]);
}

#[test]
fn nested_environment_resolves_matching_end() {
    let lines = display(
        "\\begin{aligned} A &= \\begin{pmatrix} 1 \\end{pmatrix} \\\\ B &= 2 \\end{aligned}",
    );
    assert_eq!(lines, vec!["A = (1)", "B = 2"]);
}

#[test]
fn unknown_commands_keep_their_name() {
    assert_eq!(inline("\\foobar x"), "foobar x");
}

#[test]
fn overset_and_stackrel() {
    assert_eq!(inline("a \\overset{!}{=} b"), "a = b");
    assert_eq!(inline("a \\overset{n}{=} b"), "a =ⁿ b");
}

#[test]
fn malformed_input_does_not_panic() {
    for src in [
        "",
        "{",
        "}",
        "\\",
        "\\frac{a}",
        "\\frac",
        "\\sqrt[",
        "\\begin{aligned} x",
        "\\begin",
        "\\end{x}",
        "^",
        "_",
        "^{",
        "a^",
        "{{{{{{",
        "\\left",
        "\\not",
        "$$$",
        "\\\\\\",
        "&&&&",
    ] {
        let _ = latex_to_unicode_inline(src);
        let _ = latex_to_unicode_display(src);
    }
}

#[test]
fn deeply_nested_input_is_bounded() {
    let mut src = String::new();
    for _ in 0..200 {
        src.push('{');
    }
    src.push('x');
    for _ in 0..200 {
        src.push('}');
    }
    let _ = latex_to_unicode_inline(&src);
}

#[test]
fn oversized_input_is_rejected() {
    let big = "x".repeat(MAX_MATH_SOURCE_LEN + 1);
    assert!(latex_to_unicode_inline(&big).is_none());
    assert!(latex_to_unicode_display(&big).is_none());
}

#[test]
fn whitespace_only_display_is_empty() {
    assert!(display("  \n  ").is_empty());
}

#[test]
fn escaped_literals() {
    assert_eq!(inline("100\\%"), "100%");
    assert_eq!(inline("\\{a, b\\}"), "{a, b}");
    assert_eq!(inline("\\$5"), "$5");
}
