//! Runtime DA2 (Secondary Device Attributes) probe: `CSI > 0 c` →
//! `CSI > Pp ; Pv ; Pc c`, where `Pv` is a version packed as
//! `major * 10000 + minor * 100 + patch`.
//!
//! Alacritty is the reason this exists: it exports no version environment
//! variable and refuses XTVERSION on principle. What it answers with is the
//! `alacritty_terminal` library version — see [`unpack_version`].
//!
//! Unlike [`super::xtversion`] the reply is read at the fd rather than
//! recognized by an event-loop filter, because no filter could see it:
//! crossterm has no `CSI >` arm, so it errors and clears its buffer, dropping
//! the intro and leaving digits indistinguishable from typing.
//!
//! The read owns stdin, so it must run after `enable_raw_mode()` and before
//! crossterm's `EventStream` exists. A late reply that arrives partially is
//! drained to quiet by [`super::probe`]; one of which *no* byte arrives before
//! the deadline is left for crossterm, which types it into the composer —
//! `REPLY_TIMEOUT` is sized to keep that out of reach.

use std::sync::OnceLock;
#[cfg(unix)]
use std::time::Duration;

static DA2_VERSION: OnceLock<Option<Da2Version>> = OnceLock::new();

/// Both forms of one reply. The packed integer is kept rather than recovered
/// from `text`, so version gates compare what the terminal sent instead of
/// re-parsing what this module formatted.
#[derive(Debug, Eq, PartialEq)]
struct Da2Version {
    packed: u32,
    text: String,
}

#[cfg(unix)]
const QUERY: &[u8] = b"\x1b[>0c";

/// Sized for a slow link, not for a silent terminal: a reply that misses the
/// deadline entirely is typed into the composer, not merely lost.
#[cfg(unix)]
const REPLY_TIMEOUT: Duration = Duration::from_millis(500);

/// Rejects a packed value that cannot be a real release (major ≥ 100) instead
/// of folding it into a plausible-looking version.
#[cfg(any(unix, test))]
const MAX_PACKED_VERSION: u32 = 999_999;

/// Returns the version the terminal reported over DA2, if it answered.
pub fn detected() -> Option<&'static str> {
    Some(DA2_VERSION.get()?.as_ref()?.text.as_str())
}

/// [`detected`]'s reply as the packed integer the terminal sent
/// (`major * 10000 + minor * 100 + patch`).
pub fn detected_packed() -> Option<u32> {
    Some(DA2_VERSION.get()?.as_ref()?.packed)
}

/// Query DA2 once at startup and read the reply under a bounded deadline;
/// no-ops when the gate rejects the brand/multiplexer or stdin is not a TTY.
pub fn probe_at_startup() {
    use std::io::IsTerminal;

    if DA2_VERSION.get().is_some() {
        return;
    }
    let ctx = super::terminal_context();
    if !gate_allows_probe(ctx) || !std::io::stdin().is_terminal() {
        let _ = DA2_VERSION.set(None);
        return;
    }
    query_and_read();
}

/// Deliberately narrow: Alacritty is the only brand whose version is otherwise
/// unreachable, and it is excluded from [`super::xtversion`]'s allowlist, which
/// the synchronous read depends on. CSI-intercepting multiplexers skip — tmux
/// answers DA2 as itself, and passthrough still returns the reply through it.
fn gate_allows_probe(ctx: &super::TerminalContext) -> bool {
    ctx.brand == super::TerminalName::Alacritty && !ctx.multiplexer.intercepts_csi_queries()
}

#[cfg(unix)]
fn query_and_read() {
    if !super::probe::write_query(QUERY) {
        tracing::debug!("DA2 probe skipped: query write failed or output is not a TTY");
        let _ = DA2_VERSION.set(None);
        return;
    }
    // Only the DA2 intro ends the read: startup typeahead can already hold a
    // `>` and a `c` (`ls > out.c`), and a late DA1 reply has the escape but a
    // `?`. Both are consumed instead, and the read continues to the reply.
    let reply = super::probe::read_tty_reply(REPLY_TIMEOUT, |buf, byte| {
        byte == b'c' && buf.windows(3).any(|w| w == b"\x1b[>")
    });
    let version = reply.as_deref().and_then(parse_version);
    if let Some(bytes) = reply.as_deref()
        && version.is_none()
    {
        // A bare `None` cannot distinguish a rejected reply from silence.
        let text = String::from_utf8_lossy(bytes);
        tracing::debug!(reply = %text.escape_debug(), "DA2 reply rejected");
    }
    tracing::info!(version = ?version.as_ref().map(|v| &v.text), "DA2 probe");
    let _ = DA2_VERSION.set(version);
}

#[cfg(not(unix))]
fn query_and_read() {
    // The timed read is Unix-only, and ConPTY does not answer DA2.
    let _ = DA2_VERSION.set(None);
}

/// Decode `CSI > Pp ; Pv ; Pc c`, rejecting anything that is not Alacritty's
/// exact reply shape.
///
/// `Pv` means whatever its emulator decided — xterm puts a patch level there,
/// so `> 0 ; 388 ; 0 c` would decode to a confident, wrong `0.3.88`. The brand
/// evidence here is only `TERM=alacritty`, so the shape upstream hardcodes
/// (`Pp == 0`, `Pc == 1`) is what makes the number trustworthy.
#[cfg(any(unix, test))]
fn parse_version(reply: &[u8]) -> Option<Da2Version> {
    let text = String::from_utf8_lossy(reply);
    // Split at the last `>` so a keystroke racing the reply cannot shift the
    // parameter list.
    let (_, params) = text.rsplit_once('>')?;
    let mut fields = params.trim_end().trim_end_matches('c').split(';');
    if fields.next()?.trim() != "0" {
        return None;
    }
    let packed: u32 = fields.next()?.trim().parse().ok()?;
    if fields.next()?.trim() != "1" {
        return None;
    }
    unpack_version(packed)
}

/// For Alacritty the decoded value is the `alacritty_terminal` **library**
/// version, not the application release: upstream packs the library crate's own
/// `CARGO_PKG_VERSION`, and the two diverged after 0.5 — release 0.15.1 answers
/// `2500`. Reported as-is. Pre-release suffixes are stripped upstream, so a
/// `-dev` build is indistinguishable from the matching release.
#[cfg(any(unix, test))]
fn unpack_version(packed: u32) -> Option<Da2Version> {
    if packed == 0 || packed > MAX_PACKED_VERSION {
        return None;
    }
    let major = packed / 10_000;
    let minor = (packed / 100) % 100;
    let patch = packed % 100;
    Some(Da2Version {
        packed,
        text: format!("{major}.{minor}.{patch}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::{MultiplexerKind, TerminalContext, TerminalName};

    fn parsed(reply: &[u8]) -> Option<(u32, String)> {
        parse_version(reply).map(|v| (v.packed, v.text))
    }

    #[test]
    fn packed_version_round_trips() {
        // Real `alacritty_terminal` versions, not the releases they ship in:
        // 0.21 is Alacritty 0.13.x; 0.25 is 0.15.1+ (0.15.0 still shipped 0.24.2).
        assert_eq!(
            parsed(b"\x1b[>0;2100;1c"),
            Some((2100, "0.21.0".to_owned()))
        );
        assert_eq!(
            parsed(b"\x1b[>0;2500;1c"),
            Some((2500, "0.25.0".to_owned()))
        );
        assert_eq!(
            parsed(b"\x1b[>0;2601;1c"),
            Some((2601, "0.26.1".to_owned()))
        );
        // Typeahead consumed ahead of the reply: the last `>` is still the
        // reply's, so its parameters are what get parsed.
        assert_eq!(
            parsed(b"ls > out.c\x1b[>0;2500;1c"),
            Some((2500, "0.25.0".to_owned()))
        );
    }

    #[test]
    fn another_emulators_da2_is_not_a_version() {
        // xterm's `Pv` is a patch level and VTE's is its own numbering; both
        // would otherwise decode cleanly.
        assert_eq!(parse_version(b"\x1b[>41;389;0c"), None);
        assert_eq!(parse_version(b"\x1b[>0;388;0c"), None);
        assert_eq!(parse_version(b"\x1b[>65;6003;1c"), None);
    }

    #[test]
    fn undecodable_payloads_are_none() {
        // Absent, empty, or truncated parameter lists.
        assert_eq!(parse_version(b""), None);
        assert_eq!(parse_version(b"c"), None);
        assert_eq!(parse_version(b"\x1b[>0c"), None);
        assert_eq!(parse_version(b"\x1b[>0;;1c"), None);
        assert_eq!(parse_version(b"\x1b[?62;1;6c"), None);
        // Non-numeric, signed, and absurd values must not wrap or panic.
        assert_eq!(parse_version(b"\x1b[>0;abc;1c"), None);
        assert_eq!(parse_version(b"\x1b[>0;-1;1c"), None);
        assert_eq!(parse_version(b"\x1b[>0;0;1c"), None);
        assert_eq!(parse_version(b"\x1b[>0;4294967295;1c"), None);
        assert_eq!(parse_version(b"\x1b[>0;99999999999999999999;1c"), None);
        assert_eq!(parse_version(b"\x1b[>0;1000000;1c"), None);
        assert_eq!(parse_version(b"\x1b[>0;\xff\xfe;1c"), None);
    }

    /// The DA2 read would eat an XTVERSION reply in flight for the same brand.
    /// Widening this gate onto a brand XTVERSION already probes is the edit
    /// that would break it, so the complement is what gets asserted.
    #[test]
    fn no_brand_is_probed_by_both_xtversion_and_da2() {
        use crate::terminal::xtversion;

        let ctx = |brand| TerminalContext {
            brand,
            multiplexer: MultiplexerKind::Undetected,
            ..Default::default()
        };
        assert!(gate_allows_probe(&ctx(TerminalName::Alacritty)));
        assert!(!xtversion::gate_allows_probe(&ctx(TerminalName::Alacritty)));

        for brand in [
            TerminalName::Unknown,
            TerminalName::Kitty,
            TerminalName::WezTerm,
            TerminalName::Ghostty,
            TerminalName::Iterm2,
            TerminalName::Rio,
        ] {
            // Keeps this hardcoded copy of the allowlist from going vacuous.
            assert!(
                xtversion::gate_allows_probe(&ctx(brand)),
                "{brand:?} left the XTVERSION allowlist this asserts against"
            );
            assert!(
                !gate_allows_probe(&ctx(brand)),
                "{brand:?} would be probed by both"
            );
        }
    }
}
