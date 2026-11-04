//! The store half of the owner profile (docs/UNDERSTANDING.md §2.3): the reads
//! triage and the nightly pass need, and the pass's one write.
//!
//! A SUPERTRAIT OF [`super::Store`] because the sync engine is generic over
//! its store and both of its callers (the triage worker's context and the
//! profile pass) run there. Only what those two need is here. The review
//! page's list, edit and delete are inherent `SqliteStore` methods instead
//! (`sqlite/memory.rs`), reached only through the human door's `store_call`,
//! for the reason `groups.rs` gives: a human-door read on the shared trait is
//! one careless call away from the agent door.

use crate::error::Result;
use crate::triage::memory::{MemoryRecord, RawOp, ServedRow};
use crate::triage::profile::{ApplyReport, ProfileInputSource};
use crate::types::AccountId;
use chrono::{DateTime, NaiveDate, Utc};
use std::collections::BTreeSet;

pub trait MemoryStore: ProfileInputSource {
    /// The candidate lines for one triage run: live entries (not deleted,
    /// retired or expired at `now`) whose scope is one of `scopes` or any
    /// `kind:` scope, each with the newest `received_at` among its provenance
    /// messages. Bounded; ordering and the caps are
    /// [`crate::triage::memory::select_for_context`]'s.
    fn memory_context_rows(
        &self,
        account_id: AccountId,
        scopes: &[String],
        now: DateTime<Utc>,
    ) -> Result<Vec<ServedRow>>;

    /// Every entry the owner has not deleted, retired ones included.
    fn memory_entries(&self, account_id: AccountId) -> Result<Vec<MemoryRecord>>;

    /// Days the DWELL ledger has been running for this account
    /// (`attention::dwell_ledger_since`), the pass's coverage gate and the
    /// review page's empty state.
    fn memory_coverage_days(&self, account_id: AccountId, now: DateTime<Utc>) -> Result<f32>;

    /// The pass's change gate: since `since` (`None` is "never ran", which is
    /// always a change), did the owner reply to a thread, correct triage, or
    /// is a live inferred entry close to lapsing?
    fn profile_changed_since(
        &self,
        account_id: AccountId,
        since: Option<DateTime<Utc>>,
        now: DateTime<Utc>,
        today: NaiveDate,
    ) -> Result<bool>;

    /// Validate `ops` against the store as it is INSIDE one transaction
    /// (ownership, the ledger, the live cap, stated entries) and apply what
    /// passes. `exposed` is the message ids the run showed the model; `today`
    /// is the owner-local date, for a project's `until`.
    fn apply_profile_ops(
        &self,
        account_id: AccountId,
        ops: &[RawOp],
        exposed: &BTreeSet<i64>,
        now: DateTime<Utc>,
        today: NaiveDate,
    ) -> Result<ApplyReport>;
}
