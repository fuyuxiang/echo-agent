/// Events emitted by [`crate::pipeline::run_voice_pipeline`] to the pager event loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VoiceEvent {
    /// Partial transcript while the user is speaking (`interim_results` / non-final chunks).
    InterimTranscript { text: String },

    /// Utterance complete (`speech_final` on streaming STT, or batch result).
    UtteranceFinal { text: String },

    /// Non-fatal or fatal error from capture or STT.
    Error {
        /// Short description for a one-line toast.
        message: String,
        /// Optional longer fix steps, for surfaces that fit more than one line.
        hint: Option<String>,
    },
}
