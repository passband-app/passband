//! ASSESS ON SURFACING, the agent door's half of the archive tier.
//!
//! Archive mail is stored without an access check, because a check is a model
//! call and a year of mail is a lot of them. The check is bought the moment an
//! external caller's KEYWORD search actually matches such a row: queue it, wake
//! the worker, wait a short while, then search. Both doors (MCP `search_mail`
//! and `GET /client/agent/search`) run the same steps through this module so
//! the caps, the wait and the horizon cannot drift apart. `get_thread` uses the
//! same wait for the sibling checks an explicit open queues.
//!
//! NOTHING ABOUT A PENDING ROW LEAVES THIS MODULE. The doors get back the
//! readable hits and a horizon that is the same whatever the query was: no
//! count of unchecked matches, no flag that there were any. A caller who could
//! read "3 older matches pending" could ask it about any word, sender or date
//! and read the answer off mail nobody has cleared.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::Serialize;
use tokio::sync::Notify;

use crate::error::{CoreError, Result};
use crate::store::{ArchiveAccessCaps, SearchFilter, SearchHit, SqliteStore, Store};
use crate::types::AccountId;

/// THE DOORS' HANDLE ON THE REFINE LANE: a poke ("I just queued work somebody
/// is waiting on") and the lane's own word on whether it can run at all.
///
/// The second half is what stops a door waiting on a worker that is not
/// there. The lane states it at the top of every round (a poll has succeeded,
/// a model is configured, the provider circuit is closed) and withdraws it
/// when it exits; a handle no lane ever took reads `false` for good, which is
/// the standalone MCP binary and every test that builds no engine. One atomic
/// read, and nothing here knows what a sync engine is.
#[derive(Debug, Clone, Default)]
pub struct RefinePoke {
    wake: Arc<Notify>,
    runnable: Arc<AtomicBool>,
}

impl RefinePoke {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start a round now. Coalesces: `Notify` holds one permit.
    pub fn poke(&self) {
        self.wake.notify_one();
    }

    /// The lane's side of [`Self::poke`].
    pub async fn poked(&self) {
        self.wake.notified().await;
    }

    /// The lane's side of [`Self::runnable`].
    pub fn set_runnable(&self, runnable: bool) {
        self.runnable.store(runnable, Ordering::Relaxed);
    }

    /// Whether a poke would be acted on: a lane holds this handle and said,
    /// at its last round, that it can spend.
    pub fn runnable(&self) -> bool {
        self.runnable.load(Ordering::Relaxed)
    }
}

/// How long a door waits for the checks it queued. Injectable so tests do not
/// sleep for seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SurfaceWait {
    /// The hard cap. Each door lowers it to its own ceiling with
    /// [`Self::capped`].
    pub deadline: Duration,
    /// Give up this soon when no worker has picked up even one of the checks:
    /// the lane is busy with something long, and the caller should not pay
    /// the whole deadline to learn that.
    pub idle: Duration,
    pub interval: Duration,
}

impl SurfaceWait {
    /// MCP `search_mail` / `get_thread`.
    pub const MCP_DEADLINE: Duration = Duration::from_secs(8);
    /// `GET /client/agent/*`: the Mac client gives a request 15 s, and this
    /// wait can happen on both sides of a second search.
    pub const HTTP_DEADLINE: Duration = Duration::from_secs(6);

    pub fn capped(mut self, cap: Duration) -> Self {
        self.deadline = self.deadline.min(cap);
        self
    }
}

impl Default for SurfaceWait {
    fn default() -> Self {
        Self {
            deadline: Self::MCP_DEADLINE,
            idle: Duration::from_secs(2),
            interval: Duration::from_millis(250),
        }
    }
}

/// Everything a door needs to surface archive mail, handed over as one value
/// so the two doors are configured identically. The default (no archive, no
/// poke) is what a test or the standalone binary gets.
#[derive(Debug, Clone, Default)]
pub struct ArchiveSurfacing {
    /// `sync.archive_days`; 0 means the tier is off and no horizon is reported.
    pub archive_days: u32,
    /// The refine lane's handle (`SyncEngine::with_refine_poke`). `None` when
    /// no sync engine runs in this process: checks are queued, never awaited.
    pub poke: Option<RefinePoke>,
    pub wait: SurfaceWait,
    /// What surfacing may spend, from `triage.agent`.
    pub caps: ArchiveAccessCaps,
}

impl ArchiveSurfacing {
    /// The same settings with this door's own ceiling on the wait.
    pub fn capped(mut self, deadline: Duration) -> Self {
        self.wait = self.wait.capped(deadline);
        self
    }
}

/// What a door's search asked the KEYWORD leg, which is all that surfacing
/// looks at. A door passes `None` when no keyword leg ran: semantic mode, or
/// a query of operators with no text. A date range or a sender is not a match.
#[derive(Debug, Clone, Default)]
pub struct KeywordMatch {
    pub text: String,
    /// The operators the search applies, so a check is not bought for a row
    /// the search itself would then filter out.
    pub filter: SearchFilter,
    /// Whether the leg that runs includes the owner's own sent mail.
    pub include_sent: bool,
}

/// One agent search, start to finish: pick the unassessed archive rows the
/// keyword leg matches (a bounded query of its own), send them for an access
/// check, wait briefly, then run the search and say how far back the index
/// goes. `search` runs on the blocking pool, ONCE: the checks are settled
/// before it starts, so there is nothing to run it again for.
///
/// Returns the readable hits and the horizon, and nothing else: what is still
/// unassessed after the wait never reaches the caller (see the module doc).
pub async fn search_with_surfacing<F>(
    store: &Arc<SqliteStore>,
    account: AccountId,
    surfacing: &ArchiveSurfacing,
    keyword: Option<KeywordMatch>,
    search: F,
) -> Result<(Vec<SearchHit>, SearchHorizon)>
where
    F: FnOnce(&SqliteStore) -> Result<Vec<SearchHit>> + Send + 'static,
{
    if let Some(keyword) = keyword {
        let pending = {
            let store = store.clone();
            blocking(move || {
                store.archive_surface_candidates(
                    account,
                    &keyword.text,
                    &keyword.filter,
                    keyword.include_sent,
                )
            })
            .await?
        };
        surface_archive_matches(store, account, &pending, surfacing).await?;
    }
    let hits = {
        let store = store.clone();
        blocking(move || search(&store)).await?
    };
    let archive_days = surfacing.archive_days;
    let horizon = {
        let store = store.clone();
        blocking(move || search_horizon(&store, account, archive_days, Utc::now())).await?
    };
    Ok((hits, horizon))
}

/// Queue an access check for the best-ranked of `pending` (the store applies
/// every cap and skips rows that already have a job), poke the worker and wait
/// for those checks as [`wait_for_checks`] does.
///
/// Returns whether any of them moved. NEVER BLOCKS WHEN NOTHING WAS QUEUED.
pub async fn surface_archive_matches(
    store: &Arc<SqliteStore>,
    account: AccountId,
    pending: &[i64],
    surfacing: &ArchiveSurfacing,
) -> Result<bool> {
    if pending.is_empty() {
        return Ok(false);
    }
    let queued = {
        let store = store.clone();
        let pending = pending.to_vec();
        let caps = surfacing.caps;
        blocking(move || store.request_archive_access(account, &pending, caps, Utc::now())).await?
    };
    wait_for_checks(store, account, queued, surfacing).await
}

/// An explicit thread open at the agent door: queue the checks its unassessed
/// siblings need and wait for them BEFORE the caller reads the thread, so a
/// thread is not served with a sibling omitted and then closed a moment later
/// when that sibling comes back restricted. Siblings still unchecked after the
/// wait are omitted by the read, as ever.
pub async fn await_thread_access(
    store: &Arc<SqliteStore>,
    account: AccountId,
    thread_id: &str,
    surfacing: &ArchiveSurfacing,
) -> Result<()> {
    let queued = {
        let store = store.clone();
        let thread_id = thread_id.to_string();
        let caps = surfacing.caps;
        blocking(move || store.request_thread_access(account, &thread_id, caps)).await?
    };
    wait_for_checks(store, account, queued, surfacing).await?;
    Ok(())
}

/// Poke the worker and wait for the checks on `queued`. Returns whether any
/// of them moved, which is whether reading again could show something new.
///
/// IT WAITS ONLY FOR A WORKER THAT CAN RUN. No handle, or a lane that last
/// said it cannot spend (no successful poll, no model, an open circuit): the
/// checks stay queued for whoever drains them and the caller answers now.
/// With a live lane it stops at the first of: every check finished or parked;
/// `wait.idle` gone by with none of them picked up; `wait.deadline`.
async fn wait_for_checks(
    store: &Arc<SqliteStore>,
    account: AccountId,
    queued: Vec<i64>,
    surfacing: &ArchiveSurfacing,
) -> Result<bool> {
    if queued.is_empty() {
        return Ok(false);
    }
    let Some(poke) = surfacing.poke.as_ref().filter(|poke| poke.runnable()) else {
        return Ok(false);
    };
    let wait = surfacing.wait;
    poke.poke();
    let started = tokio::time::Instant::now();
    loop {
        tokio::time::sleep(wait.interval).await;
        let progress = {
            let store = store.clone();
            let ids = queued.clone();
            blocking(move || store.archive_access_progress(account, &ids, Utc::now())).await?
        };
        let idle = progress.untouched == queued.len();
        if progress.waiting == 0 {
            return Ok(true);
        }
        let elapsed = started.elapsed();
        if elapsed >= wait.deadline || (idle && elapsed >= wait.idle) {
            return Ok(!idle);
        }
    }
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|error| CoreError::Other(anyhow::anyhow!("store task: {error}")))?
}

/// What the archive tier says about itself. THE SAME FOR EVERY QUERY: nothing
/// in it depends on what was searched for or what matched.
pub const OLDER_MAIL_POLICY: &str = "checked_on_first_match";

/// How far back the archive walk is meant to reach, and whether it has.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ArchiveHorizon {
    /// The date the walk is working back to (`YYYY-MM-DD`, UTC).
    pub target: String,
    /// Whether every week back to `target` has been stored.
    pub walk_complete: bool,
    /// Always [`OLDER_MAIL_POLICY`]: older mail is access-checked the first
    /// time a keyword search matches it. A constant, so a caller learns the
    /// rule and nothing about this search.
    pub older_mail: &'static str,
}

/// What a search can and cannot see, so an agent says "search covers mail
/// since X" instead of concluding a message does not exist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SearchHorizon {
    /// The DATE (`YYYY-MM-DD`, UTC) search reaches back to. A date and never
    /// an instant: with a walk it is where the walk has got to, without one
    /// it is where stored mail first becomes dense (see
    /// `SqliteStore::indexed_since`). `None` on an empty mailbox.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub indexed_since: Option<String>,
    /// Absent when `sync.archive_days` is 0: there is no walk to report.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archive: Option<ArchiveHorizon>,
}

pub fn search_horizon(
    store: &SqliteStore,
    account: AccountId,
    archive_days: u32,
    now: DateTime<Utc>,
) -> Result<SearchHorizon> {
    let walked_to = Store::archive_walked_to(store, account)?;
    // The walk's cursor is the honest horizon once there is one: every week
    // newer than it has been listed, whatever the oldest stray row says.
    let indexed_since = match walked_to {
        Some(at) => Some(at.date_naive()),
        None => store.indexed_since(account)?,
    }
    .map(|date| date.format("%Y-%m-%d").to_string());
    let archive = if archive_days == 0 {
        None
    } else {
        // Clamped here as well as at config load: this value also arrives from
        // callers that built their own `ArchiveSurfacing`.
        let days = archive_days.min(crate::config::ARCHIVE_DAYS_MAX);
        let target = now - chrono::Duration::days(i64::from(days));
        Some(ArchiveHorizon {
            target: target.format("%Y-%m-%d").to_string(),
            walk_complete: walked_to.is_some_and(|at| at <= target),
            older_mail: OLDER_MAIL_POLICY,
        })
    };
    Ok(SearchHorizon {
        indexed_since,
        archive,
    })
}
