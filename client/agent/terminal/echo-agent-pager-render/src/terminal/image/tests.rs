use super::*;

#[test]
fn protocol_matrix_matches_supported_terminals() {
    for (brand, expected) in [
        (TerminalName::Kitty, GraphicsProtocol::Kitty),
        (TerminalName::Ghostty, GraphicsProtocol::Kitty),
        (TerminalName::WezTerm, GraphicsProtocol::Kitty),
        (TerminalName::WarpTerminal, GraphicsProtocol::Kitty),
        (TerminalName::Iterm2, GraphicsProtocol::None),
        (TerminalName::Unknown, GraphicsProtocol::None),
    ] {
        assert_eq!(protocol_for_brand(brand, false), expected);
        assert_eq!(protocol_for_brand(brand, true), GraphicsProtocol::None);
    }
}

#[test]
fn scrollback_overlay_excludes_warp() {
    assert!(scrollback_inline_overlay_active_for_brand(
        GraphicsProtocol::Kitty,
        TerminalName::Kitty,
    ));
    assert!(!scrollback_inline_overlay_active_for_brand(
        GraphicsProtocol::Kitty,
        TerminalName::WarpTerminal,
    ));
}

#[test]
#[serial_test::serial]
fn force_off_overrides_capability() {
    let _guard = set_protocol_for_test(GraphicsProtocol::Kitty);
    set_inline_overlay_force_off(false);
    assert!(scrollback_inline_overlay_active());
    set_inline_overlay_force_off(true);
    assert!(!scrollback_inline_overlay_active());
    set_inline_overlay_force_off(false);
}

#[test]
fn kitty_escape_chunks_and_preserves_cursor() {
    let small = render_kitty_image(&[0u8; 10], KittyImageFormat::Png, 40, 20);
    assert!(small.contains("a=T"));
    assert!(small.contains("f=100"));
    assert!(small.contains("q=2"));
    assert!(small.contains("C=1"));
    assert!(small.contains("c=40"));
    assert!(small.contains("r=20"));
    assert!(small.contains("m=0"));
    let large = render_kitty_image(&vec![0u8; 5000], KittyImageFormat::Png, 40, 20);
    assert!(large.matches("\x1b_G").count() > 1);
}

#[test]
fn kitty_format_and_conversion_produce_png() {
    use image::{ImageBuffer, Rgb};

    let png = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];
    assert_eq!(kitty_format_from_bytes(&png), Some(KittyImageFormat::Png));
    let buffer: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_pixel(4, 3, Rgb([128, 64, 32]));
    let mut jpeg = Vec::new();
    buffer
        .write_to(
            &mut std::io::Cursor::new(&mut jpeg),
            image::ImageFormat::Jpeg,
        )
        .unwrap();
    assert_eq!(kitty_format_from_bytes(&jpeg), None);
    let converted = prepare_kitty_overlay_image_bytes(&jpeg).unwrap();
    assert_eq!(
        kitty_format_from_bytes(&converted),
        Some(KittyImageFormat::Png)
    );
}

#[test]
fn iterm_escape_preserves_requested_geometry() {
    let escape = render_iterm2_image(&[0u8; 10], 30, 15);
    assert!(escape.starts_with("\x1b]1337;File="));
    assert!(escape.contains("width=30cells"));
    assert!(escape.contains("height=15cells"));
    assert!(escape.contains("preserveAspectRatio=1"));
}

#[test]
fn low_level_overlay_separates_transmit_from_placement() {
    let png = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];
    let protocol = GraphicsProtocol::Kitty;
    let first =
        build_overlay_image_escapes_for_protocol(protocol, &png, 20, 10, 0, 0, true).unwrap();
    let subsequent =
        build_overlay_image_escapes_for_protocol(protocol, &png, 20, 10, 0, 0, false).unwrap();
    assert!(first.contains("a=t") && first.contains("a=p"));
    assert!(!first.contains("a=T"));
    assert!(subsequent.contains("a=p"));
    assert!(!subsequent.contains("a=t"));
}

#[test]
fn iterm_place_can_skip_inline_data() {
    let _guard = set_protocol_for_test(GraphicsProtocol::ITerm2);
    let area = ratatui::layout::Rect::new(0, 0, 40, 20);
    let escape = place_inline_image(&[0u8; 10], 100, 50, area, 20, 0, 2, false).unwrap();
    assert!(escape.starts_with("\x1b["));
    assert!(!escape.contains("1337"));
}

#[test]
fn placement_only_steady_state_removes_payload_cost() {
    let mut png = vec![0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];
    png.extend(std::iter::repeat_n(0u8, 200_000));
    let protocol = GraphicsProtocol::Kitty;
    let first =
        build_overlay_image_escapes_for_protocol(protocol, &png, 40, 20, 0, 0, true).unwrap();
    let subsequent =
        build_overlay_image_escapes_for_protocol(protocol, &png, 40, 20, 0, 0, false).unwrap();
    assert!(first.len() > 200_000);
    assert!(subsequent.len() < 200);
    assert!(!subsequent.contains("a=t"));
}
