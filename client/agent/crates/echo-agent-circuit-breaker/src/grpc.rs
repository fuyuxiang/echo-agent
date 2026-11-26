//! [`GrpcRetryPolicy`] — classifies a `tonic::Code` into a [`Disposition`], the
//! gRPC analogue of [`crate::RetryPolicy`]. Behind the `grpc` feature.

use crate::retry_policy::Disposition;
use tonic::Code;

/// Maps a gRPC [`Code`] to a [`Disposition`].
pub struct GrpcRetryPolicy {
    retryable: &'static [Code],
}

impl GrpcRetryPolicy {
    /// Retry only transient connection errors (`Unavailable`, `Unknown`);
    /// excluding `Internal`/`DeadlineExceeded` avoids amplifying a sick peer.
    pub const DEFAULT: Self = Self::new(&[Code::Unavailable, Code::Unknown]);

    /// Permissive preset: also retry `Internal` and `DeadlineExceeded`.
    pub const PERMISSIVE: Self = Self::new(&[
        Code::Unavailable,
        Code::Unknown,
        Code::Internal,
        Code::DeadlineExceeded,
    ]);

    /// Construct from an explicit retryable-code set.
    pub const fn new(retryable: &'static [Code]) -> Self {
        Self { retryable }
    }

    /// Classify `code`. Returns `None` for `Code::Ok` (success, not an error).
    pub fn classify(&self, code: Code) -> Option<Disposition> {
        match code {
            Code::Ok => None,
            c if self.is_retryable(c) => Some(Disposition::Retryable),
            _ => Some(Disposition::Terminal),
        }
    }

    /// `true` iff `code` is in the retryable set.
    pub fn is_retryable(&self, code: Code) -> bool {
        self.retryable.contains(&code)
    }
}

impl Default for GrpcRetryPolicy {
    fn default() -> Self {
        Self::DEFAULT
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_retries_transient_codes() {
        for c in [Code::Unavailable, Code::Unknown] {
            assert!(GrpcRetryPolicy::DEFAULT.is_retryable(c));
            assert_eq!(
                GrpcRetryPolicy::DEFAULT.classify(c),
                Some(Disposition::Retryable)
            );
        }
    }

    #[test]
    fn default_excludes_internal_and_deadline_exceeded() {
        for c in [Code::Internal, Code::DeadlineExceeded] {
            assert!(!GrpcRetryPolicy::DEFAULT.is_retryable(c));
            assert_eq!(
                GrpcRetryPolicy::DEFAULT.classify(c),
                Some(Disposition::Terminal)
            );
        }
    }

    #[test]
    fn default_terminal_for_permanent_codes() {
        for c in [
            Code::NotFound,
            Code::PermissionDenied,
            Code::InvalidArgument,
            Code::AlreadyExists,
            Code::Unauthenticated,
        ] {
            assert_eq!(
                GrpcRetryPolicy::DEFAULT.classify(c),
                Some(Disposition::Terminal)
            );
        }
    }

    #[test]
    fn ok_classifies_as_none() {
        assert_eq!(GrpcRetryPolicy::DEFAULT.classify(Code::Ok), None);
    }

    #[test]
    fn permissive_also_retries_internal_and_deadline() {
        for c in [
            Code::Unavailable,
            Code::Unknown,
            Code::Internal,
            Code::DeadlineExceeded,
        ] {
            assert!(GrpcRetryPolicy::PERMISSIVE.is_retryable(c));
        }
    }

    #[test]
    fn custom_set_is_respected() {
        let policy = GrpcRetryPolicy::new(&[Code::ResourceExhausted]);
        assert!(policy.is_retryable(Code::ResourceExhausted));
        assert!(!policy.is_retryable(Code::Unavailable));
    }
}
