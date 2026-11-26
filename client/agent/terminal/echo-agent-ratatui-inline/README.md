# ratatui-inline

A Rust library for building terminal applications with inline viewports - dynamic UI elements that stay at the bottom of the terminal while preserving scrollback history above them. Perfect for building chat-like interfaces, command prompts, and interactive terminal tools.

## What is this?

This crate provides tools for creating terminal applications where:
- A viewport (UI element) is pinned to the bottom of the terminal
- Content above the viewport becomes part of the terminal's native scrollback
- Users can scroll through history using their terminal's built-in scroll functionality
- Long lines wrap naturally without truncation
- The viewport remains visible and interactive while history accumulates above

Think of applications like:
- Chat interfaces with an input box at the bottom
- Interactive REPLs with command history
- Log viewers with controls at the bottom
- Any TUI that needs to preserve output history

## Key Features

- **Inline viewport** - UI stays at bottom while content flows above into scrollback
- **Natural text flow** - Content is printed normally, leveraging terminal's native behavior
- **Zero-copy text processing** - Efficient ANSI-aware text segmentation without allocations
- **Proper line wrapping** - Handles terminal width boundaries correctly with ANSI sequences
- **Unicode support** - Correct handling of emoji, CJK characters, combining characters
- **Terminal resize handling** - Robust resize support using RIS (Reset to Initial State)
- **Synchronized output** - Flicker-free rendering using DCS protocol
- **Cross-platform** - Works in all terminals and multiplexers (tmux, screen, etc.)

## Usage

See `examples/inline.rs` for a complete working example.

## Architecture

### Text Processing

The library uses a zero-copy approach for ANSI-aware text segmentation:
- **anstyle-parse** - ANSI/SGR-aware segmentation for zero-copy line splitting
- **Zero allocations** - Returns string slices without copying or allocating
- **Single-pass parsing** - Processes input once with proper escape sequence tracking
- **Unicode support** - Correct width calculation for emoji, CJK, combining characters

### Scrollback Implementation

The library uses a "natural flow" approach for scrollback:
1. Position cursor at viewport top
2. Print content, letting terminal handle wrapping naturally
3. Add viewport-height newlines to reserve space
4. Clear and render the viewport

This single implementation works universally across all terminals and multiplexers without special modes or workarounds.

### Line Ending Handling

- **LF (`\n`)** - Standard line ending, moves to next line
- **CRLF (`\r\n`)** - Windows-style line ending, treated as single line break
- **CR (`\r`)** - Carriage return only, resets cursor to line start (overwrites)

## Design Decisions

### Why Fork ratatui's Terminal?

The standard ratatui Terminal API doesn't expose internals needed for inline viewport manipulation:
- **Viewport area access** - Need to know current position and dimensions
- **Direct viewport positioning** - Must be able to set viewport location
- **Buffer management** - Need back buffer reset and previous buffer access
- **Resize calculations** - Require access to buffer state during resize

Our forked Terminal provides these capabilities while maintaining compatibility with ratatui's API.

### Synchronized Output

Flicker-free rendering using the DCS synchronized output protocol:
- All operations between begin/end markers are atomic
- Terminal only updates display once per batch
- Eliminates partial render states

## Performance

- **Colored JSON**: ~186μs per operation
- **Plain text**: ~75μs per operation  
- **Zero allocations** in hot path
- **Single-pass parsing** for all text processing

## Testing

Comprehensive test coverage including:
- Text segmentation with ANSI sequences
- Line wrapping and Unicode handling
- All line ending types (LF, CRLF, CR)
- Viewport positioning and resizing
- Terminal resize with history re-rendering
- Mock terminal infrastructure for unit testing

### Terminal Resize Strategy

#### The Problem

When using inline viewports on the main screen (not alternate screen), terminal resize causes issues:
- Terminal reflows content automatically BEFORE the app receives SIGWINCH
- Old viewport borders get reflowed as garbage text
- Built-in `autoresize()` corrupts scrollback history
- Cursor position queries (DSR) have race conditions during rapid resize
- Different terminals handle reflow unpredictably

#### The Solution: RIS (Reset to Initial State)

We use the "nuclear option" - completely reset and re-render:
1. Send RIS (`ESC c`) to clear everything
2. Re-output entire scrollback history
3. Position viewport based on content amount

This approach:
- **Works consistently** across all terminals
- **Preserves scrollback** by re-outputting history
- **Avoids artifacts** from unpredictable reflow
- **No race conditions** from cursor queries
- **Handles all resize types** (horizontal and vertical)

## Dependencies

- `ratatui` - Terminal UI framework (forked Terminal class)
- `crossterm` - Cross-platform terminal manipulation
- `anstyle-parse` - ANSI/SGR-aware line segmentation (production)
- `unicode-width` - Unicode character width calculation
- `termwiz` - **dev-dependency only**; reference splitter for
  `tests/segment_differential.rs` (not linked into shipped binaries)

## References

- [anstyle-parse](https://crates.io/crates/anstyle-parse)
- [Ratatui wrapping discussion](https://github.com/ratatui/ratatui/issues/1426)


## License / attribution

This crate includes a forked `Terminal` implementation derived from [ratatui](https://github.com/ratatui/ratatui)
(MIT / Apache-2.0). See `NOTICE` in this directory and the repository root `THIRD-PARTY-NOTICES`.
