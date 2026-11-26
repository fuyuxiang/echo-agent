# EchoAgent CPAL patch

This is CPAL 0.18.2 with one macOS loopback selection fix. Its original
license files are included in this directory.

CPAL normally uses an output device as an input device when the hardware
exposes both directions. That records a headset microphone instead of the
computer audio for duplex devices. EchoAgent calls
`force_loopback_for_next_input_stream()` immediately before building the
system-output stream. The flag is local to that thread and consumed by the
next Core Audio input stream build. Microphone streams retain CPAL's regular
input path.

The runtime changes are limited to `src/lib.rs` and
`src/host/coreaudio/macos/device.rs`.
