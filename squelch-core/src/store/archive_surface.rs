//! ASSESS ON SURFACING, the agent door's half of the archive tier.
//!
//! Archive mail is stored without an access check, because a check is a model
//! call and a year of mail is a lot of them. The check is bought the moment an
//! external caller's search actually matches such a row: queue it, wake the
//! worker, wait a short while, search again. Both doors (MCP `search_mail` and
//! `GET /client/agent/search`) run the same steps through this module so the
//! cap, the wait and the horizon cannot drift apart.
//!
//! Nothing here returns anything about a pending row but its id to the caller
//! in this process, and the callers turn those ids into a count.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::Serialize;
use tokio::sync::Notify;

use crate::error::{CoreError, Result};
use crate::store::{ExternalSearch, SqliteStore, Store};
use crate::types::AccountId;

/// How long a search waits for the checks it queued. Injectable so tests do
/// not sleep for twelve seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SurfaceWait {
    pub deadline: Duration,
    pub interval: Duration,
}

impl Default for SurfaceWait {
    fn default() -> Self {
        Self {
            deadline: Duration::from_secs(12),
            interval: Duration::from_millis(400),
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
    /// The refine lane's poke (`SyncEngine::with_refine_poke`). `None` when
    /// no sync engine runs in this process: checks are queued, never awaited.
    pub poke: Option<Arc<Notify>>,
    pub wait: SurfaceWait,
}

/// One agent search, start to finish: search, send the unassessed archive
/// matches for an access check, wait briefly, search again, and say how far
/// back the index goes. `search` runs on the blocking pool, once or twice.
///
/// The returned `pending_archive` is what is STILL unassessed after the wait.
/// Callers report its length and nothing else about it.
pub async fn search_with_surfacing<F>(
    store: &Arc<SqliteStore>,
    account: AccountId,
    surfacing: &ArchiveSurfacing,
    search: F,
) -> Result<(ExternalSearch, SearchHorizon)>
where
    F: Fn(&SqliteStore) -> Result<ExternalSearch> + Clone + Send + 'static,
{
    let run = |search: F| {
        let store = store.clone();
        blocking(move || search(&store))
    };
    let mut found = run(search.clone()).await?;
    if surface_archive_matches(
        store,
        account,
        &found.pending_archive,
        surfacing.poke.as_ref(),
        surfacing.wait,
    )
    .await?
    {
        found = run(search).await?;
    }
    let archive_days = surfacing.archive_days;
    let horizon = {
        let store = store.clone();
        blocking(move || search_horizon(&store, account, archive_days, Utc::now())).await?
    };
    Ok((found, horizon))
}

/// Queue an access check for the best-ranked of `pending` (the store applies
/// the cap and skips rows that already have a job), poke the worker and wait
/// for those checks to finish or `wait.deadline` to pass.
///
/// Returns whether the caller should search again. NEVER BLOCKS WHEN NOTHING
/// WAS QUEUED, and never when there is no `poke` handle: without one nothing
/// in this process is known to be draining the queue (the standalone MCP
/// binary, a test), so the rows are queued for whoever does and the caller
/// reports them as pending.
pub async fn surface_archive_matches(
    store: &Arc<SqliteStore>,
    account: AccountId,
    pending: &[i64],
    poke: Option<&Arc<Notify>>,
    wait: SurfaceWait,
) -> Result<bool> {
    if pending.is_empty() {
        return Ok(false);
    }
    let queued = {
        let store = store.clone();
        let pending = pending.to_vec();
        blocking(move || store.request_archive_access(account, &pending)).await?
    };
    if queued.is_empty() {
        return Ok(false);
    }
    let Some(poke) = poke else {
        return Ok(false);
    };
    poke.notify_one();
    let started = tokio::time::Instant::now();
    while started.elapsed() < wait.deadline {
        tokio::time::sleep(wait.interval).await;
        let store = store.clone();
        let ids = queued.clone();
        let waiting =
            blocking(move || store.archive_access_waiting(account, &ids, Utc::now())).await?;
        if waiting == 0 {
            break;
        }
    }
    Ok(true)
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|error| CoreError::Other(anyhow::anyhow!("store task: {error}")))?
}

/// How far back the archive walk is meant to reach, and whether it has.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ArchiveHorizon {
    /// The date the walk is working back to (`YYYY-MM-DD`, UTC).
    pub target: String,
    /// Whether every week back to `target` has been stored.
    pub walk_complete: bool,
}

/// What a search can and cannot see, so an agent says "search covers mail
/// since X" instead of concluding a message does not exist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SearchHorizon {
    /// The oldest stored non-spam message. `None` on an empty mailbox.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub indexed_since: Option<DateTime<Utc>>,
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
    let indexed_since = store.indexed_since(account)?;
    let archive = if archive_days == 0 {
        None
    } else {
        let target = now - chrono::Duration::days(i64::from(archive_days));
        let walked_to = Store::archive_walked_to(store, account)?;
        Some(ArchiveHorizon {
            target: target.format("%Y-%m-%d").to_string(),
            walk_complete: walked_to.is_some_and(|at| at <= target),
        })
    };
    Ok(SearchHorizon {
        indexed_since,
        archive,
    })
}
