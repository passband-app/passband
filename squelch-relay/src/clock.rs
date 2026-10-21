//! The one wall clock in the crate.

use std::time::{SystemTime, UNIX_EPOCH};

/// Seconds since the epoch: the APNs JWT's `iat`, an open's `opened_at`, and
/// the retention cutoff. A pre-1970 clock is not a case worth modelling and
/// reads as zero.
pub(crate) fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
