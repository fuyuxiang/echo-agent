//! No-caller canonical attempt records.

#[allow(dead_code, reason = "accounting is consumed by the next storage slice")]
mod accounting;
#[allow(
    dead_code,
    reason = "codec foundation is consumed by the next storage slice"
)]
mod codec;
#[allow(dead_code, reason = "consumed by the next storage slice")]
mod completion;
#[allow(
    dead_code,
    reason = "decoder foundation is consumed by the next storage slice"
)]
mod decoder;
#[allow(dead_code, reason = "consumed by the next storage slice")]
mod intent;
#[allow(dead_code, reason = "consumed by the next storage slice")]
mod recovery;
#[allow(dead_code, reason = "consumed by the next storage slice")]
mod rewind;

#[cfg(test)]
#[path = "accounting_tests.rs"]
mod accounting_tests;
#[cfg(test)]
#[path = "codec_tests.rs"]
mod codec_tests;
#[cfg(test)]
#[path = "completion_tests.rs"]
mod completion_tests;
#[cfg(test)]
#[path = "decoder_tests.rs"]
mod decoder_tests;
#[cfg(test)]
#[path = "intent_tests.rs"]
mod intent_tests;
#[cfg(test)]
#[path = "recovery_tests.rs"]
mod recovery_tests;
#[cfg(test)]
#[path = "rewind_tests.rs"]
mod rewind_tests;
