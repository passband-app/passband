//! The calendar sync loop: WHEN the Calendar API is called, on top of
//! [`CalendarApi`], which knows how.
//!
//! ITS OWN TASK, OFF THE GMAIL PATH (invariant: a calendar failure never fails
//! Gmail sync, triage or ingest). It shares nothing with the mail engine but
//! the store, which it touches only in short transactions over data already
//! fetched, never across a network call, and the credentials file, whose
//! writes are serialized ([`crate::credentials`]). Every failure here (no
//! grant, a dead token, 403, 5xx, a timeout, a body that does not parse, a
//! calendar too large to page through) is recorded as a kind on the
//! calendar's row; nothing reaches the mail engine's metrics, so
//! `gmail.connected` on `/client/stats` cannot move.
//!
//! WHO BACKS OFF. The whole loop backs off only when the credential fails,
//! when the PRIMARY calendar fails, or when Google answers with a quota
//! limit (a per-user limit: reading the other calendars would only be
//! refused too). A SECONDARY calendar that fails (a shared calendar that
//! answers 404 or 403 forever) backs off on its own row (`retry_at`, doubling
//! to [`BACKOFF_CAP`]) while the others keep their 5-minute cadence, and its
//! failure is not served as the account's error.
//!
//! SHUTDOWN INTERRUPTS A PASS. A pass is raced against the shutdown watch, so
//! a Google that has stopped answering cannot hold the daemon's exit for each
//! request's timeout in turn. What an interrupted pass leaves is consistent:
//! every page was its own transaction, and a calendar whose read was cut off
//! keeps its old sync token and rows (no sweep runs without the last page).
//! A token refresh already running on a blocking thread finishes on its own,
//! bounded by its HTTP timeout.
//!
//! ALWAYS STARTED, granted or not. Without a grant it idles and looks again
//! every [`IDLE_RECHECK`], because a grant arrives under a running daemon on
//! self-host (`squelchd auth --calendar`) and a kick from the calendar page
//! looks at once.
//!
//! A pass: resolve the token; decide which calendars to read (the primary
//! one, plus every calendar the user shows when the list scope was granted,
//! at most [`MAX_CALENDARS`]); then read each one, incrementally with its
//! sync token, or in full over the window when it has none, Google answered
//! 410, or its last full read is a day old (a sync token never carries an
//! event that moves INTO the window as the window slides forward).

use std::collections::HashSet;
use std::hash::BuildHasher;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use futures::FutureExt;
use tokio::sync::{Notify, watch};

use super::client::{API_BASE, CalendarApi, CallError, EventsQuery};
use super::{
    CalendarErrorKind, CalendarMeta, MAX_CALENDARS, PRIMARY, StoredCalendar, read_calendar_entry,
    read_event, window,
};
use crate::credentials::CalendarCredentials;
use crate::error::{CoreError, Result};
use crate::store::SqliteStore;
use crate::types::AccountId;

/// How often a granted calendar is read.
pub const SYNC_INTERVAL: Duration = Duration::from_secs(300);
/// How often a daemon without a grant looks for one.
pub const IDLE_RECHECK: Duration = Duration::from_secs(60);
/// How old a calendar's last full read may be before the next read is full.
const FULL_EVERY: chrono::Duration = chrono::Duration::hours(24);
/// Pages read from one calendar in one pass at most: 40 pages of 250 is
/// 10,000 events. A calendar past it is reported unavailable rather than
/// paged through without end.
const MAX_PAGES: usize = 40;
/// Pages of the calendar list at most (250 calendars each).
const MAX_LIST_PAGES: usize = 4;

const BACKOFF_BASE: Duration = Duration::from_secs(60);
const BACKOFF_CAP: Duration = Duration::from_secs(1800);

/// Startup delay window: the first pass lands in [20s, 80s], off the startup
/// burst of the mail engine and desynchronized across a fleet.
const STARTUP_DELAY_MIN: Duration = Duration::from_secs(20);
const STARTUP_DELAY_SPAN_SECS: u64 = 60;

fn startup_delay() -> Duration {
    let n = std::collections::hash_map::RandomState::new().hash_one(1u8);
    STARTUP_DELAY_MIN + Duration::from_secs(n % (STARTUP_DELAY_SPAN_SECS + 1))
}

fn next_backoff(current: Option<Duration>, base: Duration) -> Duration {
    match current {
        None => base,
        Some(d) => (d * 2).min(BACKOFF_CAP.max(base)),
    }
}

/// One secondary calendar's wait after a failure, from how many reads of it
/// had already failed in a row: the base, doubling to the cap.
fn calendar_backoff(failures_before: u32, base: Duration) -> Duration {
    let cap = BACKOFF_CAP.max(base);
    base.checked_mul(1u32 << failures_before.min(16))
        .map_or(cap, |d| d.min(cap))
}

/// Resolves once shutdown is asked for (or its sender is gone, which `wait`
/// also reads as a stop).
async fn stopped(shutdown: &mut watch::Receiver<bool>) {
    let _ = shutdown.wait_for(|stop| *stop).await;
}

/// What one pass came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PassOutcome {
    /// Calendar is not granted: nothing was called.
    NotGranted,
    /// The credential and the primary calendar were read, and Google set no
    /// quota limit. A secondary calendar may have failed: that is on its own
    /// row, with its own retry time.
    Synced,
    /// The credential or the primary calendar failed, or Google answered
    /// with a quota limit: the loop backs off.
    Failed(CalendarErrorKind),
}

/// The pacing knobs. Tests shrink them.
#[derive(Debug, Clone, Copy)]
struct Timing {
    startup_delay: Duration,
    interval: Duration,
    idle_recheck: Duration,
    backoff_base: Duration,
}

pub struct CalendarSync {
    store: Arc<SqliteStore>,
    account_id: AccountId,
    creds: Arc<CalendarCredentials>,
    api: CalendarApi,
    kick: Arc<Notify>,
    timing: Timing,
}

/// A credential failure as a kind: Google refusing the refresh token (or no
/// refresh token to refresh with) is the token being dead; anything else
/// (a network failure mid-refresh, a backend read error) is transient.
fn credential_kind(error: &CoreError) -> CalendarErrorKind {
    let dead = match error {
        CoreError::Credential(msg) => {
            msg.contains("invalid_grant") || msg.contains("squelchd auth")
        }
        _ => false,
    };
    if dead {
        CalendarErrorKind::Auth
    } else {
        CalendarErrorKind::Unavailable
    }
}

impl CalendarSync {
    /// The loop against Google's production API.
    pub fn new(
        store: Arc<SqliteStore>,
        account_id: AccountId,
        creds: Arc<CalendarCredentials>,
    ) -> reqwest::Result<Self> {
        Self::with_api_base(store, account_id, creds, API_BASE)
    }

    /// The loop against another API origin: a test's local fake.
    pub fn with_api_base(
        store: Arc<SqliteStore>,
        account_id: AccountId,
        creds: Arc<CalendarCredentials>,
        base: &str,
    ) -> reqwest::Result<Self> {
        Ok(Self {
            store,
            account_id,
            creds,
            api: CalendarApi::new(base)?,
            kick: Arc::new(Notify::new()),
            timing: Timing {
                startup_delay: startup_delay(),
                interval: SYNC_INTERVAL,
                idle_recheck: IDLE_RECHECK,
                backoff_base: BACKOFF_BASE,
            },
        })
    }

    /// TEST HOOK: no startup delay and every wait at `tick`.
    #[doc(hidden)]
    pub fn with_test_timing(mut self, tick: Duration) -> Self {
        self.timing = Timing {
            startup_delay: Duration::ZERO,
            interval: tick,
            idle_recheck: tick,
            backoff_base: tick,
        };
        self
    }

    /// Notifying it starts a pass now (the calendar page opening). It does
    /// not cut short a quota backoff. The human door throttles it.
    pub fn kick_handle(&self) -> Arc<Notify> {
        self.kick.clone()
    }

    /// Run until shutdown.
    pub async fn run(self, mut shutdown: watch::Receiver<bool>) -> Result<()> {
        if self
            .wait(self.timing.startup_delay, false, &mut shutdown)
            .await
        {
            return Ok(());
        }
        let mut backoff: Option<Duration> = None;
        loop {
            let outcome = tokio::select! {
                outcome = self.pass(Utc::now()) => outcome,
                () = stopped(&mut shutdown) => {
                    eprintln!("squelch: calendar sync: pass interrupted by shutdown");
                    return Ok(());
                }
            };
            let (delay, quota) = match outcome {
                PassOutcome::NotGranted => {
                    backoff = None;
                    (self.timing.idle_recheck, false)
                }
                PassOutcome::Synced => {
                    backoff = None;
                    (self.timing.interval, false)
                }
                PassOutcome::Failed(kind) => {
                    let next = next_backoff(backoff, self.timing.backoff_base);
                    backoff = Some(next);
                    (next, kind == CalendarErrorKind::Quota)
                }
            };
            if self.wait(delay, quota, &mut shutdown).await {
                return Ok(());
            }
        }
    }

    /// Sleep, or wake on a kick (unless `ignore_kick`), or stop. `true` means
    /// stop.
    async fn wait(
        &self,
        delay: Duration,
        ignore_kick: bool,
        shutdown: &mut watch::Receiver<bool>,
    ) -> bool {
        if *shutdown.borrow() {
            return true;
        }
        if delay.is_zero() {
            return false;
        }
        let kicked = async {
            if ignore_kick {
                std::future::pending::<()>().await
            } else {
                self.kick.notified().await
            }
        };
        let stop = tokio::select! {
            _ = tokio::time::sleep(delay) => false,
            _ = kicked => false,
            changed = shutdown.changed() => changed.is_err() || *shutdown.borrow(),
        };
        if ignore_kick {
            // A kick that arrived during the quota backoff found no waiter
            // and left a permit; taken now, so the pass after the backoff is
            // not followed by a second one at once.
            let _ = self.kick.notified().now_or_never();
        }
        stop
    }

    /// Run a store call off the async threads: the store's one connection is
    /// a blocking mutex that a triage commit may be holding.
    async fn db<T, F>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&SqliteStore, AccountId) -> Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let store = self.store.clone();
        let account = self.account_id;
        tokio::task::spawn_blocking(move || f(&store, account))
            .await
            .map_err(|e| CoreError::Other(anyhow::anyhow!("calendar store task: {e}")))?
    }

    /// Record a failure kind on a calendar's row, and when it may be read
    /// again (None: on the loop's schedule). A store error here is logged and
    /// dropped: the read has already failed.
    async fn record(
        &self,
        calendar_id: &str,
        kind: CalendarErrorKind,
        retry_at: Option<DateTime<Utc>>,
    ) {
        let id = calendar_id.to_string();
        if let Err(e) = self
            .db(move |s, a| s.calendar_record_error(a, &id, kind, retry_at))
            .await
        {
            eprintln!("squelch: calendar sync could not record a failure: {e}");
        }
    }

    /// One pass. Public for tests, which drive passes one at a time.
    pub async fn pass(&self, now: DateTime<Utc>) -> PassOutcome {
        let access = match self.creds.calendar_access(self.account_id).await {
            Ok(None) => return PassOutcome::NotGranted,
            Ok(Some(access)) => access,
            Err(e) => {
                let kind = credential_kind(&e);
                eprintln!(
                    "squelch: calendar sync: no usable calendar token ({})",
                    kind.as_str()
                );
                self.record(PRIMARY, kind, None).await;
                return PassOutcome::Failed(kind);
            }
        };
        let token = access.token.access_token;
        let calendars = match self.calendars_to_read(&token, access.list_granted).await {
            Ok(calendars) => calendars,
            Err(kind) => {
                self.record(PRIMARY, kind, None).await;
                return PassOutcome::Failed(kind);
            }
        };
        let selection = calendars.clone();
        if let Err(e) = self
            .db(move |s, a| s.calendar_set_selection(a, &selection))
            .await
        {
            eprintln!("squelch: calendar sync could not record the calendars: {e}");
            return PassOutcome::Failed(CalendarErrorKind::Unavailable);
        }
        let stored = match self.db(|s, a| s.calendar_calendars(a)).await {
            Ok(stored) => stored,
            Err(e) => {
                eprintln!("squelch: calendar sync could not read the calendars: {e}");
                return PassOutcome::Failed(CalendarErrorKind::Unavailable);
            }
        };
        let window = window(now);
        // Only the primary calendar's failure, or a quota answer from any,
        // is the pass failing; see the module doc.
        let mut failed: Option<CalendarErrorKind> = None;
        for (index, meta) in calendars.iter().enumerate() {
            let Some(row) = stored.iter().find(|c| c.calendar_id == meta.calendar_id) else {
                continue;
            };
            let primary = row.calendar_id == PRIMARY;
            // A secondary calendar that failed waits out its own backoff.
            if !primary && row.retry_at.is_some_and(|at| now < at) {
                continue;
            }
            let Err(kind) = self.sync_calendar(row, &token, window, now).await else {
                continue;
            };
            // The position in this pass, never the id: a calendar id is
            // usually an email address.
            eprintln!(
                "squelch: calendar sync: calendar {index} failed ({})",
                kind.as_str()
            );
            let quota = kind == CalendarErrorKind::Quota;
            // The primary calendar and a quota answer go on the loop's own
            // backoff; any other secondary failure gets one of its own.
            let retry_at = (!primary && !quota).then(|| {
                let wait = calendar_backoff(row.failures, self.timing.backoff_base);
                chrono::Duration::from_std(wait).map_or(now, |wait| now + wait)
            });
            self.record(&meta.calendar_id, kind, retry_at).await;
            if primary || quota {
                failed = failed.max(Some(kind));
            }
            if quota {
                // A per-user limit: the rest would be refused too.
                break;
            }
        }
        failed.map_or(PassOutcome::Synced, PassOutcome::Failed)
    }

    /// The calendars to read: the primary one alone without the list scope;
    /// with it, the primary one first and then every shown calendar in the
    /// list's order, at most [`MAX_CALENDARS`]. When the list cannot be read
    /// the calendars already being read are kept, so a transient failure does
    /// not drop shared calendars from the timeline; a list that is refused
    /// (the list scope taken back) falls to the primary alone.
    async fn calendars_to_read(
        &self,
        token: &str,
        list_granted: bool,
    ) -> std::result::Result<Vec<CalendarMeta>, CalendarErrorKind> {
        let primary_only = || {
            vec![CalendarMeta {
                calendar_id: PRIMARY.to_string(),
                summary: None,
                is_primary: true,
                time_zone: None,
            }]
        };
        if !list_granted {
            return Ok(primary_only());
        }
        match self.read_list(token).await {
            Ok(list) => Ok(list),
            Err(CallError::Kind(CalendarErrorKind::Auth)) => {
                eprintln!(
                    "squelch: calendar sync: the calendar list was refused; reading the primary calendar only"
                );
                Ok(primary_only())
            }
            Err(e) => {
                eprintln!(
                    "squelch: calendar sync: the calendar list is unavailable ({e}); keeping the calendars already read"
                );
                let stored = self
                    .db(|s, a| s.calendar_calendars(a))
                    .await
                    .map_err(|_| CalendarErrorKind::Unavailable)?;
                let mut kept: Vec<CalendarMeta> = stored
                    .into_iter()
                    .filter(|c| c.selected)
                    .map(|c| CalendarMeta {
                        calendar_id: c.calendar_id,
                        summary: c.summary,
                        is_primary: c.is_primary,
                        time_zone: c.time_zone,
                    })
                    .collect();
                if !kept.iter().any(|c| c.calendar_id == PRIMARY) {
                    kept.insert(0, primary_only().remove(0));
                }
                kept.truncate(MAX_CALENDARS);
                Ok(kept)
            }
        }
    }

    async fn read_list(&self, token: &str) -> std::result::Result<Vec<CalendarMeta>, CallError> {
        let mut primary: Option<CalendarMeta> = None;
        let mut others: Vec<CalendarMeta> = Vec::new();
        let mut page_token: Option<String> = None;
        for _ in 0..MAX_LIST_PAGES {
            let page = self.api.calendar_list(token, page_token.as_deref()).await?;
            for item in &page.items {
                match read_calendar_entry(item) {
                    Some((meta, true)) if meta.is_primary => primary = Some(meta),
                    Some((meta, true)) => {
                        if !others.iter().any(|o| o.calendar_id == meta.calendar_id) {
                            others.push(meta);
                        }
                    }
                    _ => {}
                }
            }
            match page.next_page_token {
                Some(next) => page_token = Some(next),
                None => break,
            }
        }
        let mut list = vec![primary.unwrap_or(CalendarMeta {
            calendar_id: PRIMARY.to_string(),
            summary: None,
            is_primary: true,
            time_zone: None,
        })];
        list.extend(others);
        list.truncate(MAX_CALENDARS);
        Ok(list)
    }

    /// Read one calendar: incrementally, or in full, with one retry in full
    /// after a 410. Each page is written in its own short transaction as it
    /// arrives; the sync token is recorded only after the last page.
    async fn sync_calendar(
        &self,
        calendar: &StoredCalendar,
        token: &str,
        window: (DateTime<Utc>, DateTime<Utc>),
        now: DateTime<Utc>,
    ) -> std::result::Result<(), CalendarErrorKind> {
        let id = calendar.calendar_id.clone();
        let stale = calendar
            .full_synced_at
            .is_none_or(|at| now - at >= FULL_EVERY);
        let mut since = calendar.sync_token.clone().filter(|_| !stale);
        let mut zone = calendar.time_zone.clone();
        let store_err = |e: CoreError| {
            eprintln!("squelch: calendar sync could not store a page: {e}");
            CalendarErrorKind::Unavailable
        };
        loop {
            let full = since.is_none();
            let query = match &since {
                Some(sync_token) => EventsQuery::Since(sync_token.clone()),
                None => EventsQuery::Full {
                    time_min: window.0,
                    time_max: window.1,
                },
            };
            let mut seen: HashSet<String> = HashSet::new();
            let mut cut = false;
            let mut page_token: Option<String> = None;
            let mut gone = false;
            for _ in 0..MAX_PAGES {
                let page = match self
                    .api
                    .events(token, &id, &query, page_token.as_deref())
                    .await
                {
                    Ok(page) => page,
                    // The token is no longer valid: forget it and read the
                    // window in full, now. A 410 on a full read is Google
                    // misbehaving, not a reason to loop.
                    Err(CallError::Gone) if !full => {
                        let cal = id.clone();
                        self.db(move |s, a| s.calendar_clear_token(a, &cal))
                            .await
                            .map_err(store_err)?;
                        gone = true;
                        break;
                    }
                    Err(e) => return Err(e.kind()),
                };
                if zone.is_none() {
                    zone = page.time_zone.clone();
                }
                // The calendar's own title and zone, for a calendar no list
                // entry described (the primary one without the list scope).
                if calendar.summary.is_none() || calendar.time_zone.is_none() {
                    let summary = page
                        .summary
                        .as_deref()
                        .and_then(crate::triage::ghost::clean_text);
                    let tz = page
                        .time_zone
                        .as_deref()
                        .and_then(crate::triage::ghost::iana_zone);
                    let cal = id.clone();
                    self.db(move |s, a| {
                        s.calendar_note_meta(a, &cal, summary.as_deref(), tz.as_deref())
                    })
                    .await
                    .map_err(store_err)?;
                }
                let items: Vec<_> = page
                    .items
                    .iter()
                    .filter_map(|item| read_event(item, zone.as_deref(), window))
                    .collect();
                let cal = id.clone();
                let applied = self
                    .db(move |s, a| s.calendar_apply_page(a, &cal, &items, now))
                    .await
                    .map_err(store_err)?;
                cut |= applied.dropped > 0;
                if full {
                    seen.extend(applied.stored);
                }
                if let Some(next) = page.next_page_token {
                    page_token = Some(next);
                    continue;
                }
                let cal = id.clone();
                let sync_token = page.next_sync_token;
                let seen = std::mem::take(&mut seen);
                self.db(move |s, a| {
                    s.calendar_finish_sync(
                        a,
                        &cal,
                        sync_token.as_deref(),
                        now,
                        full.then_some((&seen, cut)),
                    )
                })
                .await
                .map_err(store_err)?;
                return Ok(());
            }
            if !gone {
                // Paged past the cap without reaching the end.
                return Err(CalendarErrorKind::Unavailable);
            }
            since = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_to_its_cap() {
        let base = Duration::from_secs(60);
        let mut d = None;
        let mut seen = Vec::new();
        for _ in 0..8 {
            let next = next_backoff(d, base);
            seen.push(next.as_secs());
            d = Some(next);
        }
        assert_eq!(seen, [60, 120, 240, 480, 960, 1800, 1800, 1800]);
    }

    #[test]
    fn a_calendars_own_backoff_doubles_from_the_base_to_its_cap() {
        let base = Duration::from_secs(60);
        let seen: Vec<_> = [0, 1, 2, 5, 6, 40, u32::MAX]
            .into_iter()
            .map(|n| calendar_backoff(n, base).as_secs())
            .collect();
        assert_eq!(seen, [60, 120, 240, 1800, 1800, 1800, 1800]);
    }

    #[test]
    fn a_refused_refresh_is_auth_and_a_network_failure_is_not() {
        let dead = CoreError::Credential(
            "refresh failed: invalid_grant: Token has been expired or revoked.".into(),
        );
        assert_eq!(credential_kind(&dead), CalendarErrorKind::Auth);
        let blip = CoreError::Credential("refresh failed: connection reset by peer".into());
        assert_eq!(credential_kind(&blip), CalendarErrorKind::Unavailable);
    }

    #[test]
    fn the_startup_delay_is_inside_its_window() {
        for _ in 0..20 {
            let d = startup_delay();
            assert!(d >= STARTUP_DELAY_MIN);
            assert!(d <= STARTUP_DELAY_MIN + Duration::from_secs(STARTUP_DELAY_SPAN_SECS));
        }
    }
}
