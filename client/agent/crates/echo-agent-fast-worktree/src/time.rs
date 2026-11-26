//! Time helpers shared across the crate's feature configurations.

use std::time::{SystemTime, UNIX_EPOCH};

/// Whole seconds since the Unix epoch, saturating at `i64::MAX` and clamping a
/// pre-epoch clock to 0. This is the single source for the timestamps the DB and
/// the reclaim writer store.
pub(crate) fn epoch_secs() -> i64 {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    i64::try_from(secs).unwrap_or(i64::MAX)
}
