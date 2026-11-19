//! The retention poller: the one writer of the `users` retention columns
//! (`last_seen_at`, `last_action_at`, `mac_paired_at`, `ios_paired_at`,
//! `activity_read_at`, `activity_tried_at`).
//!
//! Activation ([`crate::activation`]) answers "did they ever run the app".
//! This answers what comes after: do they come back, and did they install the
//! iPhone app. The tenant's own daemon knows (`squelchd token activity`), the
//! warden reads it out (`GET /v1/tenants/{label}/activity`), and this stamps it
//! onto the person. The overview reads the store and only the store, exactly as
//! for activation: a page render never execs into a pod.
//!
//! PRIVACY: timestamps per platform cross the wire, for tenants this process
//! provisioned. No device names, no counts, no addresses. Log lines carry
//! labels and tallies, never an email.
//!
//! THE CADENCE IS A COLUMN. Every person with a live tenant is a candidate
//! once [`RETENTION_REFRESH`] has passed since they were last ASKED, and every
//! outcome stamps "asked" (`activity_tried_at`): an answer, a daemon too old to
//! have the command, a pod mid-roll, a warden error. So a tenant that fails
//! forever costs one exec per interval rather than one per pass, and cannot
//! starve the people behind it in the queue. The price is that a transient
//! failure waits out the interval too, which for numbers read in weeks is
//! nothing.

use chrono::Utc;

use crate::state::ControlState;
use crate::warden::TenantActivity;

/// How many tenants one pass may ask about. Each ask is a `pods/exec`, so the
/// pass is small and SERIAL, like activation's. At one pass per
/// `ACTIVATION_POLL_EVERY` this covers a few thousand people per interval,
/// far beyond the beta; anything left over is the next pass's work.
pub const RETENTION_BATCH: i64 = 25;

/// How long a person's retention facts are left alone after they were last
/// asked about. "Active in the last seven days" read at four hours' staleness
/// is the same answer, and it keeps the steady-state exec rate to six per
/// person per day.
pub const RETENTION_REFRESH: chrono::Duration = chrono::Duration::hours(4);

/// One retention pass. Returns how many people's facts were refreshed with a
/// real answer.
///
/// Per-tenant errors are eaten deliberately: the next interval is the retry.
/// A daemon that predates `token activity` is stamped as ASKED and left NOT
/// KNOWN (`activity_read_at` untouched), so the board says "not known yet"
/// for that person rather than counting them as gone; the next fleet roll is
/// what fixes it.
pub async fn poll_activity(state: &ControlState) -> usize {
    let now = Utc::now();
    let labels = match state
        .store()
        .users_due_activity(RETENTION_BATCH, now - RETENTION_REFRESH)
        .await
    {
        Ok(labels) => labels,
        Err(e) => {
            tracing::error!(error = %e, "retention: could not read the candidate set");
            return 0;
        }
    };

    let mut refreshed = 0usize;
    let mut unsupported = 0usize;
    for label in labels {
        let answered = match state.warden().activity(&label).await {
            Ok(TenantActivity::Known(activity)) => {
                match state.store().record_activity(&label, &activity, now).await {
                    Ok(_) => {
                        refreshed += 1;
                        continue;
                    }
                    Err(e) => {
                        tracing::error!(error = %e, %label, "retention: record failed");
                        false
                    }
                }
            }
            Ok(TenantActivity::Unsupported) => {
                unsupported += 1;
                false
            }
            // The candidate join only offers labels with an active tenant
            // row, so the warden not knowing one is its own business; the
            // interval is the retry either way.
            Ok(TenantActivity::NoTenant) => false,
            Err(e) => {
                tracing::debug!(error = %e, %label, "retention: warden not answering");
                false
            }
        };
        if !answered && let Err(e) = state.store().mark_activity_tried(&label, now).await {
            tracing::error!(error = %e, %label, "retention: could not stamp the attempt");
        }
    }
    if refreshed > 0 || unsupported > 0 {
        // PRIVACY: tallies only.
        tracing::debug!(refreshed, unsupported, "retention: pass finished");
    }
    refreshed
}
