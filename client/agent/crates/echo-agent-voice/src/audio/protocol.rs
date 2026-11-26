//! Wire protocol between the `__mic-capture` helper child and its parent.
//!
//! One status header line on stdout, then (in capture mode) raw PCM:
//! - `READY <device>` — capture stream open, PCM follows;
//! - `INFO <name>\t<detail>` — device lookup result (no stream);
//! - `ERR <message>` — failure, non-zero exit.
//!
//! The child builds lines with the helpers here and the parent parses with
//! the same tags, so the two sides cannot drift.

/// Capture stream is open; raw PCM follows this line.
pub(super) const READY: &str = "READY";
/// Device lookup result; fields separated by [`INFO_FIELD_SEPARATOR`].
pub(super) const INFO: &str = "INFO";
/// Failure; the payload is the error message.
pub(super) const ERR: &str = "ERR";
/// Separates the device name from its detail in an `INFO` payload.
pub(super) const INFO_FIELD_SEPARATOR: char = '\t';

pub(super) fn ready_line(device: &str) -> String {
    format!("{READY} {}", sanitize(device))
}

pub(super) fn info_line(name: &str, detail: &str) -> String {
    format!(
        "{INFO} {}{INFO_FIELD_SEPARATOR}{}",
        sanitize(name),
        sanitize(detail)
    )
}

pub(super) fn err_line(message: &str) -> String {
    format!("{ERR} {}", sanitize(message))
}

/// Header payloads must stay single-line for the parent's line-oriented
/// handshake, and must not contain the `INFO` field separator (a device name
/// with a tab would otherwise bleed into the detail field).
fn sanitize(s: &str) -> String {
    s.replace(['\n', '\r', INFO_FIELD_SEPARATOR], " ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_carry_tag_and_sanitized_payload() {
        assert_eq!(ready_line("Mic\nName"), "READY Mic Name");
        assert_eq!(err_line("boom\r"), "ERR boom ");
        assert_eq!(info_line("USB Mic", "44100 Hz"), "INFO USB Mic\t44100 Hz");
    }

    #[test]
    fn sanitize_strips_the_info_field_separator() {
        // A tab inside a device name must not create a phantom third field.
        assert_eq!(
            info_line("Evil\tMic", "48000 Hz"),
            "INFO Evil Mic\t48000 Hz"
        );
    }
}
