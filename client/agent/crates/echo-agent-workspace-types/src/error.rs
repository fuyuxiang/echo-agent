//! Workspace-wide error type.
//!
//! # Implementation notes
//!
//! - **`Io`**: a natural definition would be `Io(#[from] std::io::Error)`.
//!   That is tempting because `?` then lifts a `std::io::Error` straight
//!   into a `WorkspaceError`, but `std::io::Error` is **not** `Serialize`,
//!   so the enum cannot be sent over a gRPC stream as-is. To keep the
//!   wire types crate fully serializable we replace it with a
//!   serializable `Io { message: String, kind: IoKind }` payload and an
//!   [`IoKind`] enum that mirrors every currently-stable variant of
//!   [`std::io::ErrorKind`]. Conversion from `std::io::Error` happens
//!   manually at the workspace-crate boundary via
//!   [`WorkspaceError::from_io`].
//!
//! - **`Tool`**: a natural definition would be `Tool(#[from] echo_agent_tools::ToolError)`.
//!   That coupling would force this crate to depend on
//!   `echo-agent-tools`, which would defeat the lightweight
//!   wire-types-only goal. Tool errors are surfaced as a generic
//!   `Tool { code, message }` payload here; the runtime workspace crate
//!   is responsible for translating its native `ToolError` into and out
//!   of this shape.
//!
//! - **`Vcs`**: the doc shows `Vcs(#[from] VcsError)`, but `VcsError` is
//!   never defined in the doc -- effectively a placeholder. We use a
//!   `Vcs(String)` payload here. The runtime workspace crate translates
//!   native git/jj errors into this string form. When the VCS subsystem
//!   is extracted into the workspace crate we can promote this to a
//!   structured `VcsErrorKind` enum without breaking the wire format
//!   (the JSON shape stays a string).
//!
//! - **`Internal`**: the doc shows
//!   `Internal(#[source] Box<dyn Error + Send + Sync>)`, but
//!   `Box<dyn Error>` is not `Serialize`. We use `Internal(String)` and
//!   ask callers with richer error types to format with `format!("{err:#}")`
//!   before constructing. The underlying error chain is lost on the
//!   wire boundary.
//!
//! - **`ProtocolMismatch.expected`**: the doc shows
//!   `expected: &'static str`, but `&'static str` cannot deserialize
//!   into a borrowed `'static` lifetime without serde-borrow gymnastics.
//!   We use an owned `String`. Construction sites typically pass a
//!   `&'static str` literal that is `.into()`'d at the boundary.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::chunks::ChunkKind;
use crate::identity::SessionId;

/// All errors surfaced by a workspace transport.
///
/// Every variant is fully serializable so it can travel over the gRPC
/// transport. Conversion from non-serializable runtime errors (most
/// notably `std::io::Error` and `echo_agent_tools::ToolError`) happens
/// at the workspace-crate boundary -- see this module's doc comment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Error)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum WorkspaceError {
    /// Filesystem I/O failure. Mirrors `std::io::Error` shape.
    #[error("io: {message}")]
    Io {
        /// `std::io::Error::to_string()` value.
        message: String,
        /// Serializable mirror of `std::io::ErrorKind`.
        kind: IoKind,
    },

    /// Version-control (git/jj) failure.
    #[error("vcs: {0}")]
    Vcs(String),

    /// Permission denied at the workspace policy layer.
    #[error("permission denied: {reason}")]
    Permission {
        /// Human-readable reason.
        reason: String,
    },

    /// Resource not found.
    #[error("not found: {0}")]
    NotFound(String),

    /// Operation cancelled (caller dropped the receiver or fired the
    /// cancel token).
    #[error("cancelled")]
    Cancelled,

    /// Operation exceeded its deadline.
    #[error("deadline exceeded after {elapsed_ms}ms")]
    Timeout {
        /// Elapsed milliseconds from start to timeout.
        elapsed_ms: u64,
    },

    /// Session id was not registered with the workspace.
    #[error("session not found: {0}")]
    SessionNotFound(SessionId),

    /// A tool returned an error. The runtime crate translates its
    /// native `ToolError` into this generic shape.
    #[error("tool error [{code}]: {message}")]
    Tool {
        /// Stable, machine-readable code (e.g. `"timeout"`,
        /// `"invalid_args"`).
        code: String,
        /// Human-readable description.
        message: String,
    },

    /// Generic transport-layer failure (gRPC handshake, TLS, ...).
    #[error("transport: {0}")]
    Remote(String),

    /// The wrong chunk kind arrived on the stream.
    #[error("protocol mismatch: expected {expected}, got {got}")]
    ProtocolMismatch {
        /// Static name of the expected variant.
        expected: String,
        /// Discriminator of the chunk that actually arrived.
        got: ChunkKind,
    },

    /// The stream produced something inconsistent with the stream
    /// contract (e.g. a unary op yielded extra chunks).
    #[error("protocol violation: {0}")]
    ProtocolViolation(String),

    /// The stream closed before yielding any chunk.
    #[error("empty stream (expected at least one chunk)")]
    EmptyStream,

    /// Catch-all for unexpected internal failures.
    #[error("internal: {0}")]
    Internal(String),
}

impl WorkspaceError {
    /// Convert from a `std::io::Error`. Used at the workspace-crate
    /// boundary -- this crate does not implement `From<io::Error>`
    /// because `io::Error` is not serializable.
    ///
    /// Consumes the error by value (idiomatic Rust conversion-from
    /// constructor); only `to_string()` and `kind()` are read so this
    /// is fine.
    pub fn from_io(err: std::io::Error) -> Self {
        Self::Io {
            message: err.to_string(),
            kind: IoKind::from(err.kind()),
        }
    }

    /// Whether the operation is safe to retry.
    ///
    /// Retryable cases:
    /// - [`Self::Timeout`] -- the deadline was exceeded but the upstream
    ///   may simply be slow.
    /// - [`Self::Remote`] -- a transport-layer failure, often transient.
    /// - [`Self::Io`] with one of these transient kinds:
    ///   [`IoKind::BrokenPipe`], [`IoKind::ConnectionReset`],
    ///   [`IoKind::ConnectionAborted`], [`IoKind::ConnectionRefused`],
    ///   [`IoKind::TimedOut`], [`IoKind::Interrupted`],
    ///   [`IoKind::WouldBlock`], [`IoKind::HostUnreachable`],
    ///   [`IoKind::NetworkUnreachable`], [`IoKind::NetworkDown`],
    ///   [`IoKind::ResourceBusy`], [`IoKind::Deadlock`].
    ///
    /// Non-retryable IO kinds (`NotFound`, `PermissionDenied`,
    /// `InvalidInput`, `InvalidData`, `AlreadyExists`, `Unsupported`,
    /// `WriteZero`, ...) and all domain errors (`Permission`,
    /// `NotFound`, `SessionNotFound`, ...) return `false`.
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Timeout { .. } | Self::Remote(_) => true,
            Self::Io { kind, .. } => kind.is_transient(),
            _ => false,
        }
    }

    /// Whether this is a cancellation.
    pub fn is_cancelled(&self) -> bool {
        matches!(self, Self::Cancelled)
    }
}

/// Serializable mirror of [`std::io::ErrorKind`].
///
/// Tracks every currently-stable variant of [`std::io::ErrorKind`] as of
/// Rust 1.83+. Conversion from `std::io::ErrorKind` is lossless for
/// every enumerated variant; future-stabilized variants collapse to
/// [`IoKind::Other`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IoKind {
    /// Connection refused.
    ConnectionRefused,
    /// Connection reset by peer.
    ConnectionReset,
    /// Host unreachable.
    HostUnreachable,
    /// Network unreachable.
    NetworkUnreachable,
    /// Connection aborted.
    ConnectionAborted,
    /// Not connected.
    NotConnected,
    /// Address in use.
    AddrInUse,
    /// Address not available.
    AddrNotAvailable,
    /// Network down.
    NetworkDown,
    /// Broken pipe.
    BrokenPipe,
    /// Already exists.
    AlreadyExists,
    /// Would block.
    WouldBlock,
    /// Not a directory.
    NotADirectory,
    /// Is a directory.
    IsADirectory,
    /// Directory not empty.
    DirectoryNotEmpty,
    /// Read-only filesystem.
    ReadOnlyFilesystem,
    /// Stale network filesystem handle.
    StaleNetworkFileHandle,
    /// Invalid input.
    InvalidInput,
    /// Invalid data.
    InvalidData,
    /// Timed out.
    TimedOut,
    /// Write zero.
    WriteZero,
    /// Storage full.
    StorageFull,
    /// Not seekable.
    NotSeekable,
    /// Quota exceeded.
    QuotaExceeded,
    /// File too large.
    FileTooLarge,
    /// Resource busy.
    ResourceBusy,
    /// Executable file is busy.
    ExecutableFileBusy,
    /// Deadlock.
    Deadlock,
    /// Crosses devices.
    CrossesDevices,
    /// Too many links.
    TooManyLinks,
    /// Invalid filename.
    InvalidFilename,
    /// Argument list too long.
    ArgumentListTooLong,
    /// Interrupted.
    Interrupted,
    /// Unexpected end of file.
    UnexpectedEof,
    /// Unsupported.
    Unsupported,
    /// Out of memory.
    OutOfMemory,
    /// Not found.
    NotFound,
    /// Permission denied.
    PermissionDenied,
    /// Other / unrecognized (catches future-stabilized variants).
    Other,
}

impl IoKind {
    /// Whether the I/O kind is transient (the same operation may
    /// succeed if retried).
    ///
    /// Used by [`WorkspaceError::is_retryable`].
    pub fn is_transient(self) -> bool {
        matches!(
            self,
            Self::BrokenPipe
                | Self::ConnectionReset
                | Self::ConnectionAborted
                | Self::ConnectionRefused
                | Self::TimedOut
                | Self::Interrupted
                | Self::WouldBlock
                | Self::HostUnreachable
                | Self::NetworkUnreachable
                | Self::NetworkDown
                | Self::ResourceBusy
                | Self::Deadlock
        )
    }
}

impl From<std::io::ErrorKind> for IoKind {
    fn from(kind: std::io::ErrorKind) -> Self {
        use std::io::ErrorKind as K;
        match kind {
            K::NotFound => Self::NotFound,
            K::PermissionDenied => Self::PermissionDenied,
            K::ConnectionRefused => Self::ConnectionRefused,
            K::ConnectionReset => Self::ConnectionReset,
            K::HostUnreachable => Self::HostUnreachable,
            K::NetworkUnreachable => Self::NetworkUnreachable,
            K::ConnectionAborted => Self::ConnectionAborted,
            K::NotConnected => Self::NotConnected,
            K::AddrInUse => Self::AddrInUse,
            K::AddrNotAvailable => Self::AddrNotAvailable,
            K::NetworkDown => Self::NetworkDown,
            K::BrokenPipe => Self::BrokenPipe,
            K::AlreadyExists => Self::AlreadyExists,
            K::WouldBlock => Self::WouldBlock,
            K::NotADirectory => Self::NotADirectory,
            K::IsADirectory => Self::IsADirectory,
            K::DirectoryNotEmpty => Self::DirectoryNotEmpty,
            K::ReadOnlyFilesystem => Self::ReadOnlyFilesystem,
            K::StaleNetworkFileHandle => Self::StaleNetworkFileHandle,
            K::InvalidInput => Self::InvalidInput,
            K::InvalidData => Self::InvalidData,
            K::TimedOut => Self::TimedOut,
            K::WriteZero => Self::WriteZero,
            K::StorageFull => Self::StorageFull,
            K::NotSeekable => Self::NotSeekable,
            K::QuotaExceeded => Self::QuotaExceeded,
            K::FileTooLarge => Self::FileTooLarge,
            K::ResourceBusy => Self::ResourceBusy,
            K::ExecutableFileBusy => Self::ExecutableFileBusy,
            K::Deadlock => Self::Deadlock,
            K::CrossesDevices => Self::CrossesDevices,
            K::TooManyLinks => Self::TooManyLinks,
            K::InvalidFilename => Self::InvalidFilename,
            K::ArgumentListTooLong => Self::ArgumentListTooLong,
            K::Interrupted => Self::Interrupted,
            K::UnexpectedEof => Self::UnexpectedEof,
            K::Unsupported => Self::Unsupported,
            K::OutOfMemory => Self::OutOfMemory,
            // Future-stabilized variants (and the historical `Other` /
            // `Uncategorized`) bucket here.
            _ => Self::Other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// All `IoKind` variants. Hand-maintained; the
    /// `io_kind_from_round_trips_for_every_std_kind` test below covers
    /// the conversion side.
    const ALL_IO_KINDS: &[IoKind] = &[
        IoKind::ConnectionRefused,
        IoKind::ConnectionReset,
        IoKind::HostUnreachable,
        IoKind::NetworkUnreachable,
        IoKind::ConnectionAborted,
        IoKind::NotConnected,
        IoKind::AddrInUse,
        IoKind::AddrNotAvailable,
        IoKind::NetworkDown,
        IoKind::BrokenPipe,
        IoKind::AlreadyExists,
        IoKind::WouldBlock,
        IoKind::NotADirectory,
        IoKind::IsADirectory,
        IoKind::DirectoryNotEmpty,
        IoKind::ReadOnlyFilesystem,
        IoKind::StaleNetworkFileHandle,
        IoKind::InvalidInput,
        IoKind::InvalidData,
        IoKind::TimedOut,
        IoKind::WriteZero,
        IoKind::StorageFull,
        IoKind::NotSeekable,
        IoKind::QuotaExceeded,
        IoKind::FileTooLarge,
        IoKind::ResourceBusy,
        IoKind::ExecutableFileBusy,
        IoKind::Deadlock,
        IoKind::CrossesDevices,
        IoKind::TooManyLinks,
        IoKind::InvalidFilename,
        IoKind::ArgumentListTooLong,
        IoKind::Interrupted,
        IoKind::UnexpectedEof,
        IoKind::Unsupported,
        IoKind::OutOfMemory,
        IoKind::NotFound,
        IoKind::PermissionDenied,
        IoKind::Other,
    ];

    fn variants() -> Vec<WorkspaceError> {
        vec![
            WorkspaceError::Io {
                message: "no such file".to_owned(),
                kind: IoKind::NotFound,
            },
            WorkspaceError::Vcs("dirty index".to_owned()),
            WorkspaceError::Permission {
                reason: "no token".to_owned(),
            },
            WorkspaceError::NotFound("/missing".to_owned()),
            WorkspaceError::Cancelled,
            WorkspaceError::Timeout { elapsed_ms: 250 },
            WorkspaceError::SessionNotFound(SessionId::new("s1")),
            WorkspaceError::Tool {
                code: "invalid_args".to_owned(),
                message: "bad input".to_owned(),
            },
            WorkspaceError::Remote("connection refused".to_owned()),
            WorkspaceError::ProtocolMismatch {
                expected: "GitStatus".to_owned(),
                got: ChunkKind::Ack,
            },
            WorkspaceError::ProtocolViolation("extra chunk".to_owned()),
            WorkspaceError::EmptyStream,
            WorkspaceError::Internal("unexpected".to_owned()),
        ]
    }

    #[test]
    fn every_variant_round_trips() {
        for err in variants() {
            let json = serde_json::to_string(&err).unwrap();
            let back: WorkspaceError = serde_json::from_str(&json).unwrap();
            assert_eq!(err, back, "round-trip failed for {err:?}");
        }
    }

    #[test]
    fn every_variant_renders_via_display() {
        for err in variants() {
            let s = err.to_string();
            assert!(!s.is_empty(), "empty Display for {err:?}");
        }
    }

    #[test]
    fn protocol_mismatch_uses_chunk_kind_display_not_debug() {
        let err = WorkspaceError::ProtocolMismatch {
            expected: "GitStatus".into(),
            got: ChunkKind::Ack,
        };
        // ChunkKind::Ack's Display is `"Ack"` (not `"Ack"` from Debug,
        // but they happen to coincide); guard against {got:?} regression
        // by asserting the rendered string.
        assert_eq!(
            err.to_string(),
            "protocol mismatch: expected GitStatus, got Ack"
        );
    }

    #[test]
    fn is_retryable_only_for_transient_io_remote_timeout() {
        // Retryable.
        assert!(WorkspaceError::Timeout { elapsed_ms: 1 }.is_retryable());
        assert!(WorkspaceError::Remote("x".into()).is_retryable());
        for kind in [
            IoKind::BrokenPipe,
            IoKind::ConnectionReset,
            IoKind::ConnectionAborted,
            IoKind::ConnectionRefused,
            IoKind::TimedOut,
            IoKind::Interrupted,
            IoKind::WouldBlock,
            IoKind::HostUnreachable,
            IoKind::NetworkUnreachable,
            IoKind::NetworkDown,
            IoKind::ResourceBusy,
            IoKind::Deadlock,
        ] {
            assert!(
                WorkspaceError::Io {
                    message: "x".into(),
                    kind
                }
                .is_retryable(),
                "expected {kind:?} to be retryable"
            );
        }
        // Non-retryable IO kinds.
        for kind in [
            IoKind::NotFound,
            IoKind::PermissionDenied,
            IoKind::InvalidInput,
            IoKind::InvalidData,
            IoKind::AlreadyExists,
            IoKind::Unsupported,
            IoKind::WriteZero,
            IoKind::IsADirectory,
            IoKind::NotADirectory,
            IoKind::ReadOnlyFilesystem,
            IoKind::StorageFull,
            IoKind::FileTooLarge,
            IoKind::Other,
        ] {
            assert!(
                !WorkspaceError::Io {
                    message: "x".into(),
                    kind
                }
                .is_retryable(),
                "expected {kind:?} to be non-retryable"
            );
        }
        // Non-retryable domain errors.
        assert!(!WorkspaceError::Cancelled.is_retryable());
        assert!(!WorkspaceError::Permission { reason: "x".into() }.is_retryable());
        assert!(!WorkspaceError::EmptyStream.is_retryable());
    }

    #[test]
    fn is_cancelled_only_for_cancelled() {
        assert!(WorkspaceError::Cancelled.is_cancelled());
        assert!(!WorkspaceError::Timeout { elapsed_ms: 1 }.is_cancelled());
    }

    #[test]
    fn from_io_preserves_kind_and_message() {
        let io = std::io::Error::new(std::io::ErrorKind::NotFound, "missing.txt");
        let err = WorkspaceError::from_io(io);
        match err {
            WorkspaceError::Io { kind, message } => {
                assert_eq!(kind, IoKind::NotFound);
                assert!(message.contains("missing.txt"), "got {message}");
            }
            other => panic!("expected Io variant, got {other:?}"),
        }
    }

    #[test]
    fn io_kind_round_trips() {
        for kind in ALL_IO_KINDS {
            let json = serde_json::to_string(kind).unwrap();
            let back: IoKind = serde_json::from_str(&json).unwrap();
            assert_eq!(*kind, back);
        }
    }

    #[test]
    fn io_kind_from_round_trips_for_every_std_kind() {
        // Exercise the From impl on every std::io::ErrorKind we mirror,
        // ensuring no kind silently collapses to Other.
        use std::io::ErrorKind as K;
        let cases: &[(K, IoKind)] = &[
            (K::NotFound, IoKind::NotFound),
            (K::PermissionDenied, IoKind::PermissionDenied),
            (K::ConnectionRefused, IoKind::ConnectionRefused),
            (K::ConnectionReset, IoKind::ConnectionReset),
            (K::HostUnreachable, IoKind::HostUnreachable),
            (K::NetworkUnreachable, IoKind::NetworkUnreachable),
            (K::ConnectionAborted, IoKind::ConnectionAborted),
            (K::NotConnected, IoKind::NotConnected),
            (K::AddrInUse, IoKind::AddrInUse),
            (K::AddrNotAvailable, IoKind::AddrNotAvailable),
            (K::NetworkDown, IoKind::NetworkDown),
            (K::BrokenPipe, IoKind::BrokenPipe),
            (K::AlreadyExists, IoKind::AlreadyExists),
            (K::WouldBlock, IoKind::WouldBlock),
            (K::NotADirectory, IoKind::NotADirectory),
            (K::IsADirectory, IoKind::IsADirectory),
            (K::DirectoryNotEmpty, IoKind::DirectoryNotEmpty),
            (K::ReadOnlyFilesystem, IoKind::ReadOnlyFilesystem),
            (K::StaleNetworkFileHandle, IoKind::StaleNetworkFileHandle),
            (K::InvalidInput, IoKind::InvalidInput),
            (K::InvalidData, IoKind::InvalidData),
            (K::TimedOut, IoKind::TimedOut),
            (K::WriteZero, IoKind::WriteZero),
            (K::StorageFull, IoKind::StorageFull),
            (K::NotSeekable, IoKind::NotSeekable),
            (K::QuotaExceeded, IoKind::QuotaExceeded),
            (K::FileTooLarge, IoKind::FileTooLarge),
            (K::ResourceBusy, IoKind::ResourceBusy),
            (K::ExecutableFileBusy, IoKind::ExecutableFileBusy),
            (K::Deadlock, IoKind::Deadlock),
            (K::CrossesDevices, IoKind::CrossesDevices),
            (K::TooManyLinks, IoKind::TooManyLinks),
            (K::InvalidFilename, IoKind::InvalidFilename),
            (K::ArgumentListTooLong, IoKind::ArgumentListTooLong),
            (K::Interrupted, IoKind::Interrupted),
            (K::UnexpectedEof, IoKind::UnexpectedEof),
            (K::Unsupported, IoKind::Unsupported),
            (K::OutOfMemory, IoKind::OutOfMemory),
        ];
        for &(k, expected) in cases {
            assert_eq!(IoKind::from(k), expected, "mismatch for {k:?}");
        }
    }
}
