//! Atomic spend reservations for the bounded investigation worker.
use super::*;
use crate::triage::agent_config::{
    ACCESS_BUDGET_KEY, AgentConfig, BACKGROUND_BUDGET_KEY, DailyBudgetStatus, TOTAL_BUDGET_KEY,
};

/// What a held job's `last_error` reads while it waits for tomorrow.
const HELD_BY_BUDGET: &str = "daily_budget_exhausted";

/// The `wake_budget` row holding a one-day grant on top of `key`'s cap. It is
/// keyed by day like the spend it extends, so it lapses at midnight UTC with
/// nothing to undo.
fn grant_key(key: &str) -> String {
    format!("{key}#grant")
}

/// Today's one-day grant on `key`, if an operator made one.
pub(super) fn granted(
    connection: &Connection,
    account: AccountId,
    key: &str,
    day: &str,
) -> Result<u32> {
    runs(connection, account, &grant_key(key), day)
}

/// The cap `key` is held to today: the configured one plus today's grant.
/// The one statement of that rule, for the reservation, the status and the
/// archive surfacing check alike.
pub(super) fn cap(
    connection: &Connection,
    account: AccountId,
    key: &str,
    configured: u32,
    day: &str,
) -> Result<u32> {
    Ok(configured.saturating_add(granted(connection, account, key, day)?))
}

/// Today's spend on `key`.
pub(super) fn runs(
    connection: &Connection,
    account: AccountId,
    key: &str,
    day: &str,
) -> Result<u32> {
    let used: i64 = connection.query_row(
        "SELECT COALESCE((SELECT model_calls FROM wake_budget WHERE account_id=?1 AND thread_id=?2 AND day=?3),0)",
        params![account, key, day],
        |row| row.get(0),
    )?;
    Ok(used.clamp(0, i64::from(u32::MAX)) as u32)
}

/// Jobs the daily cap parked and that are still waiting. `?1` account, `?2`
/// [`HELD_BY_BUDGET`], `?3` now. Joined as `j` with its lane as `l`.
const HELD: &str = "j.account_id=?1 AND j.kind IN ('triage','access') AND j.state='queued'
    AND j.last_error=?2 AND j.available_at>?3";
/// Jobs the cap parked that are due again (released, or past midnight) but
/// have not run yet. They will spend, so they are not room for more.
const DUE: &str = "j.account_id=?1 AND j.kind IN ('triage','access') AND j.state='queued'
    AND j.last_error=?2 AND j.available_at<=?3";
/// The four lanes, mirroring `investigation_budget_limits` (a test below holds
/// the two to each other). A revisit answers to its own cap as well, whatever
/// its kind (a revisit of sent or spam mail is an access job); an access
/// check spends the access lane whatever its lane flag; otherwise a job
/// spends background capacity unless it is a foreground triage. An archive
/// check an external caller asked for is on the access lane too, and answers
/// to a sub-cap of its own as well, which no grant raises: a grant releases
/// it only within that sub-cap's room.
const REVISIT: &str = "j.trigger LIKE 'revisit:%'";
const ACCESS: &str = "j.trigger NOT LIKE 'revisit:%' AND j.kind='access'";
const ARCHIVE: &str = "j.trigger='archive_access'";
const ARRIVAL: &str =
    "j.trigger NOT LIKE 'revisit:%' AND COALESCE(l.foreground,0)=1 AND j.kind='triage'";
const BACKGROUND: &str =
    "j.trigger NOT LIKE 'revisit:%' AND COALESCE(l.foreground,0)=0 AND j.kind='triage'";

/// Today's spend and effective caps, read inside whatever transaction holds
/// `connection`.
fn budget(
    connection: &Connection,
    account: AccountId,
    config: &AgentConfig,
    day: &str,
) -> Result<DailyBudgetStatus> {
    Ok(DailyBudgetStatus {
        total_used: runs(connection, account, TOTAL_BUDGET_KEY, day)?,
        total_cap: cap(
            connection,
            account,
            TOTAL_BUDGET_KEY,
            config.daily_run_cap,
            day,
        )?,
        background_used: runs(connection, account, BACKGROUND_BUDGET_KEY, day)?,
        background_cap: cap(
            connection,
            account,
            BACKGROUND_BUDGET_KEY,
            config.effective_background_daily_run_cap(),
            day,
        )?,
        access_used: runs(connection, account, ACCESS_BUDGET_KEY, day)?,
        access_cap: cap(
            connection,
            account,
            ACCESS_BUDGET_KEY,
            config.effective_access_daily_run_cap(),
            day,
        )?,
        ..DailyBudgetStatus::default()
    })
}

impl SqliteStore {
    /// Today's spend, effective caps and held jobs, for the scrape and for
    /// `squelchd triage status`.
    pub fn daily_budget_status(
        &self,
        account: AccountId,
        config: &AgentConfig,
        now: DateTime<Utc>,
    ) -> Result<DailyBudgetStatus> {
        let day = now.format("%Y-%m-%d").to_string();
        let connection = self.lock()?;
        let (held_arrival, held_background, held_access, held_revisit): (u64, u64, u64, u64) =
            connection.query_row(
                &format!(
                    "SELECT COALESCE(SUM({ARRIVAL}),0), COALESCE(SUM({BACKGROUND}),0),
                            COALESCE(SUM({ACCESS}),0), COALESCE(SUM({REVISIT}),0)
                         FROM agent_triage_jobs j LEFT JOIN agent_job_lanes l ON l.job_id=j.id
                         WHERE {HELD}"
                ),
                params![account, HELD_BY_BUDGET, now.to_rfc3339()],
                |row| {
                    Ok((
                        count(row, 0)?,
                        count(row, 1)?,
                        count(row, 2)?,
                        count(row, 3)?,
                    ))
                },
            )?;
        Ok(DailyBudgetStatus {
            held_arrival,
            held_background,
            held_access,
            held_revisit,
            ..budget(&connection, account, config, &day)?
        })
    }

    /// Raise today's cap on `key` by `extra` runs and let as many held jobs
    /// run now as the caps have room for. Returns how many were released.
    ///
    /// The grant REPLACES any earlier one for the day rather than adding to
    /// it, so running the command twice is not twice the spend, and zero takes
    /// it back. Only jobs that fit are released, new mail first, then
    /// background work, then access checks, each within its own lane's room
    /// and the total's, so the rest stay parked instead of running only to
    /// be turned away again (and each counted as another deferral). Revisits
    /// are never released: they answer to a cap of their own that a grant
    /// does not raise. Archive checks are released last, and only within the
    /// room their own sub-cap (which no grant raises either) still has.
    pub fn grant_daily_runs(
        &self,
        account: AccountId,
        key: &str,
        extra: u32,
        config: &AgentConfig,
        now: DateTime<Utc>,
    ) -> Result<usize> {
        let day = now.format("%Y-%m-%d").to_string();
        let mut connection = self.lock()?;
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT INTO wake_budget(account_id,thread_id,day,model_calls) VALUES(?1,?2,?3,?4)
             ON CONFLICT(account_id,thread_id,day) DO UPDATE SET model_calls=excluded.model_calls",
            params![account, grant_key(key), day, extra],
        )?;
        let status = budget(&transaction, account, config, &day)?;
        let (due_arrival, due_background, due_access, due_archive): (u32, u32, u32, u32) =
            transaction.query_row(
                &format!(
                    "SELECT COALESCE(SUM({ARRIVAL}),0), COALESCE(SUM({BACKGROUND}),0),
                            COALESCE(SUM({ACCESS}),0), COALESCE(SUM({ACCESS} AND {ARCHIVE}),0)
                     FROM agent_triage_jobs j LEFT JOIN agent_job_lanes l ON l.job_id=j.id
                     WHERE {DUE}"
                ),
                params![account, HELD_BY_BUDGET, now.to_rfc3339()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )?;
        let total_room = status
            .total_cap
            .saturating_sub(status.total_used)
            .saturating_sub(due_arrival)
            .saturating_sub(due_background)
            .saturating_sub(due_access);
        let background_room = status
            .background_cap
            .saturating_sub(status.background_used)
            .saturating_sub(due_background);
        let access_room = status
            .access_cap
            .saturating_sub(status.access_used)
            .saturating_sub(due_access);
        let archive_key = super::agent_triage::ARCHIVE_ACCESS_BUDGET_KEY;
        let archive_room = cap(
            &transaction,
            account,
            archive_key,
            config.archive_access_daily_cap,
            &day,
        )?
        .saturating_sub(runs(&transaction, account, archive_key, &day)?)
        .saturating_sub(due_archive);
        let release = |lane: &str, limit: u32| -> Result<usize> {
            Ok(transaction.execute(
                &format!(
                    "UPDATE agent_triage_jobs SET available_at=?3 WHERE id IN (
                         SELECT j.id FROM agent_triage_jobs j
                         LEFT JOIN agent_job_lanes l ON l.job_id=j.id
                         WHERE {HELD} AND {lane} ORDER BY j.id LIMIT ?4)"
                ),
                params![account, HELD_BY_BUDGET, now.to_rfc3339(), limit],
            )?)
        };
        let arrival = release(ARRIVAL, total_room)?;
        let left = total_room.saturating_sub(u32::try_from(arrival).unwrap_or(u32::MAX));
        let background = release(BACKGROUND, left.min(background_room))?;
        let left = left.saturating_sub(u32::try_from(background).unwrap_or(u32::MAX));
        let access = release(
            &format!("{ACCESS} AND NOT {ARCHIVE}"),
            left.min(access_room),
        )?;
        let released = u32::try_from(access).unwrap_or(u32::MAX);
        let archive = release(
            &format!("{ACCESS} AND {ARCHIVE}"),
            left.saturating_sub(released)
                .min(access_room.saturating_sub(released))
                .min(archive_room),
        )?;
        transaction.commit()?;
        Ok(arrival + background + access + archive)
    }
}

/// Every job the caps parked made due now, but revisits, whose own cap
/// nothing lifts, and archive search checks past their own sub-cap's room
/// (released the way [`SqliteStore::grant_daily_runs`] releases them). See
/// [`AgentTriageStore::release_budget_held_jobs`](crate::store::agent_triage::AgentTriageStore::release_budget_held_jobs).
pub(super) fn release_all_held(
    store: &SqliteStore,
    account: AccountId,
    config: &AgentConfig,
    now: DateTime<Utc>,
) -> Result<usize> {
    let day = now.format("%Y-%m-%d").to_string();
    let mut connection = store.lock()?;
    let transaction =
        connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let release = |lane: &str, limit: i64| -> Result<usize> {
        Ok(transaction.execute(
            &format!(
                "UPDATE agent_triage_jobs SET available_at=?3 WHERE id IN (
                     SELECT j.id FROM agent_triage_jobs j
                     LEFT JOIN agent_job_lanes l ON l.job_id=j.id
                     WHERE {HELD} AND {lane} ORDER BY j.id LIMIT ?4)"
            ),
            params![account, HELD_BY_BUDGET, now.to_rfc3339(), limit],
        )?)
    };
    // SQLite reads a negative LIMIT as none.
    let rest = release(&format!("NOT ({REVISIT}) AND NOT ({ARCHIVE})"), -1)?;
    let archive_key = super::agent_triage::ARCHIVE_ACCESS_BUDGET_KEY;
    let due_archive: u32 = transaction.query_row(
        &format!("SELECT COUNT(*) FROM agent_triage_jobs j WHERE {DUE} AND {ACCESS} AND {ARCHIVE}"),
        params![account, HELD_BY_BUDGET, now.to_rfc3339()],
        |row| row.get(0),
    )?;
    let archive_room = cap(
        &transaction,
        account,
        archive_key,
        config.archive_access_daily_cap,
        &day,
    )?
    .saturating_sub(runs(&transaction, account, archive_key, &day)?)
    .saturating_sub(due_archive);
    let archive = release(&format!("{ACCESS} AND {ARCHIVE}"), i64::from(archive_room))?;
    transaction.commit()?;
    Ok(rest + archive)
}

pub(super) fn reserve(
    store: &SqliteStore,
    account: AccountId,
    day: &str,
    limits: &[(String, u32)],
) -> Result<bool> {
    let mut connection = store.lock()?;
    let transaction =
        connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    for (key, configured) in limits {
        if runs(&transaction, account, key, day)?
            >= cap(&transaction, account, key, *configured, day)?
        {
            return Ok(false);
        }
    }
    for (key, _) in limits {
        transaction.execute(
            "INSERT INTO wake_budget(account_id,thread_id,day,model_calls) VALUES(?1,?2,?3,1)
            ON CONFLICT(account_id,thread_id,day) DO UPDATE SET model_calls=model_calls+1",
            params![account, key, day],
        )?;
    }
    transaction.commit()?;
    Ok(true)
}

pub(super) fn refund(
    store: &SqliteStore,
    account: AccountId,
    day: &str,
    limits: &[(String, u32)],
) -> Result<()> {
    let mut connection = store.lock()?;
    let transaction = connection.transaction()?;
    for (key, _) in limits {
        transaction.execute("UPDATE wake_budget SET model_calls=MAX(0,model_calls-1) WHERE account_id=?1 AND thread_id=?2 AND day=?3",
            params![account,key,day])?;
    }
    transaction.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::agent_triage::AgentTriageStore;

    #[test]
    fn scoped_budget_is_atomic_and_refundable_on_the_original_day() {
        let store = SqliteStore::open_in_memory().unwrap();
        let account = store.ensure_account("budget@example.com").unwrap();
        let limits = vec![("global".into(), 2), ("thread:a".into(), 1)];
        assert!(
            store
                .reserve_agent_budget(account, "2026-09-17", &limits)
                .unwrap()
        );
        assert!(
            !store
                .reserve_agent_budget(account, "2026-09-17", &limits)
                .unwrap()
        );
        assert_eq!(
            store
                .stage2_budget_used(account, "global", "2026-09-17")
                .unwrap(),
            1
        );
        store
            .refund_agent_budget(account, "2026-09-17", &limits)
            .unwrap();
        assert!(
            store
                .reserve_agent_budget(account, "2026-09-17", &limits)
                .unwrap()
        );
        assert!(
            store
                .reserve_agent_budget(account, "2026-09-18", &limits)
                .unwrap()
        );
        store
            .refund_agent_budget(account, "2026-09-17", &limits)
            .unwrap();
        assert_eq!(
            store
                .stage2_budget_used(account, "global", "2026-09-18")
                .unwrap(),
            1
        );
    }

    /// A grant lifts the cap for its own day only, replaces rather than adds,
    /// and is taken back by granting zero.
    #[test]
    fn a_grant_raises_one_days_cap_and_replaces_the_last_one() {
        let store = SqliteStore::open_in_memory().unwrap();
        let account = store.ensure_account("grant@example.com").unwrap();
        let now = DateTime::parse_from_rfc3339("2026-10-06T12:00:00Z")
            .unwrap()
            .to_utc();
        let config = AgentConfig {
            daily_run_cap: 1,
            ..AgentConfig::default()
        };
        let limits = vec![(TOTAL_BUDGET_KEY.to_string(), 1)];
        let reserve = |day: &str| store.reserve_agent_budget(account, day, &limits).unwrap();
        assert!(reserve("2026-10-06"));
        assert!(!reserve("2026-10-06"));

        for runs in [5, 2] {
            store
                .grant_daily_runs(account, TOTAL_BUDGET_KEY, runs, &config, now)
                .unwrap();
        }
        assert!(reserve("2026-10-06"));
        assert!(reserve("2026-10-06"));
        assert!(!reserve("2026-10-06"), "2 granted, not 5 + 2");

        // Tomorrow is back on the configured cap.
        assert!(reserve("2026-10-07"));
        assert!(!reserve("2026-10-07"));

        let status = store.daily_budget_status(account, &config, now).unwrap();
        assert_eq!((status.total_used, status.total_cap), (3, 3));
        // The grant is not spend, and background has none of its own.
        assert_eq!((status.background_used, status.background_cap), (0, 0));

        store
            .grant_daily_runs(account, TOTAL_BUDGET_KEY, 0, &config, now)
            .unwrap();
        assert_eq!(
            store
                .daily_budget_status(account, &config, now)
                .unwrap()
                .total_cap,
            1
        );
    }

    /// Held counts split the way the worker charges and see only jobs parked
    /// by the budget and still waiting. A grant releases only as many as the
    /// caps now have room for, new mail first, and never a revisit.
    #[test]
    fn held_jobs_are_counted_by_lane_and_a_grant_releases_what_fits() {
        let store = SqliteStore::open_in_memory().unwrap();
        assert_eq!(store.ensure_account("held@example.com").unwrap(), 1);
        assert_eq!(store.ensure_account("other@example.com").unwrap(), 2);
        let now = Utc::now();
        let later = (now + chrono::Duration::hours(6)).to_rfc3339();
        let earlier = (now - chrono::Duration::hours(1)).to_rfc3339();
        let held = Some(HELD_BY_BUDGET);
        {
            let conn = store.lock().unwrap();
            // (id, account, kind, trigger, last_error, available_at, foreground)
            for (id, account, kind, trigger, error, at, foreground) in [
                (1, 1, "triage", "arrival", held, &later, Some(1)),
                (2, 1, "triage", "migration", held, &later, Some(0)),
                (3, 1, "triage", "backfill", held, &later, None),
                // An access check is on the access lane even in the
                // foreground lane.
                (4, 1, "access", "access", held, &later, Some(1)),
                // Already due again: no longer held.
                (5, 1, "triage", "due", held, &earlier, Some(1)),
                // Waiting for some other reason.
                (
                    6,
                    1,
                    "triage",
                    "cooldown",
                    Some("provider_cooldown"),
                    &later,
                    Some(1),
                ),
                (7, 1, "triage", "fresh", None, &later, Some(1)),
                // Another account's.
                (8, 2, "triage", "arrival", held, &later, Some(1)),
                (9, 1, "triage", "revisit:due", held, &later, Some(0)),
                // An archive check: on the access lane, released only within
                // its own sub-cap's room (none, below).
                (10, 1, "access", "archive_access", held, &later, None),
            ] {
                conn.execute(
                    "INSERT INTO agent_triage_jobs
                     (id,account_id,message_id,kind,trigger,input_revision,state,available_at,last_error)
                     VALUES(?1,?2,?3,?4,?5,1,'queued',?6,?7)",
                    params![id, account, 1, kind, trigger, at, error],
                )
                .unwrap();
                if let Some(foreground) = foreground {
                    conn.execute(
                        "INSERT INTO agent_job_lanes(job_id,foreground) VALUES(?1,?2)",
                        params![id, foreground],
                    )
                    .unwrap();
                }
            }
        }
        // The access lane paused (cap 0): its job waits for a grant of its own.
        let config = AgentConfig {
            daily_run_cap: 10,
            background_daily_run_cap: 2,
            access_daily_run_cap: 0,
            archive_access_daily_cap: 0,
            ..AgentConfig::default()
        };
        let day = now.format("%Y-%m-%d").to_string();
        let both = vec![
            (TOTAL_BUDGET_KEY.to_string(), 10),
            (BACKGROUND_BUDGET_KEY.to_string(), 2),
        ];
        let total = vec![(TOTAL_BUDGET_KEY.to_string(), 10)];
        while store.reserve_agent_budget(1, &day, &both).unwrap() {}
        while store.reserve_agent_budget(1, &day, &total).unwrap() {}

        let status = |account| store.daily_budget_status(account, &config, now).unwrap();
        let held = |account| {
            let s = status(account);
            (
                s.held_arrival,
                s.held_background,
                s.held_access,
                s.held_revisit,
            )
        };
        assert_eq!(held(1), (1, 2, 2, 1));

        // Three more total runs: one is already spoken for by job 5, due and
        // not yet run, one goes to the new mail, and none to background,
        // whose own cap is still spent, nor to the access checks, whose lane
        // has no room at all.
        let grant = |key, runs| store.grant_daily_runs(1, key, runs, &config, now).unwrap();
        assert_eq!(grant(TOTAL_BUDGET_KEY, 3), 1);
        assert_eq!(held(1), (0, 2, 2, 1));
        // One background run, and the total's last run of room: exactly one
        // background job, the oldest.
        assert_eq!(grant(BACKGROUND_BUDGET_KEY, 1), 1);
        assert_eq!(held(1), (0, 1, 2, 1));
        assert_eq!((status(1).total_cap, status(1).background_cap), (13, 3));
        // Granting again before the released jobs run releases nothing more.
        assert_eq!(grant(BACKGROUND_BUDGET_KEY, 1), 0);
        // The access lane's own grant: room in the lane, none in the total
        // (ten spent, three due), so nothing yet; the total's next runs of
        // room go to it, the arrival and background lanes having nothing
        // that fits, and only the plain access check is released: the
        // archive check stays, its own sub-cap (zero here) having no room.
        assert_eq!(grant(ACCESS_BUDGET_KEY, 5), 0);
        assert_eq!((status(1).access_used, status(1).access_cap), (0, 5));
        assert_eq!(grant(TOTAL_BUDGET_KEY, 5), 1);
        assert_eq!(held(1), (0, 1, 1, 1));
        // The other account's job was not touched.
        assert_eq!(held(2), (1, 0, 0, 0));
    }

    /// Lifting the caps releases every parked job of the account but
    /// revisits, and archive checks only within their sub-cap's room, and
    /// touches nothing else.
    #[test]
    fn release_all_held_frees_parked_mail_but_not_revisits() {
        let store = SqliteStore::open_in_memory().unwrap();
        assert_eq!(store.ensure_account("lift@example.com").unwrap(), 1);
        assert_eq!(store.ensure_account("other@example.com").unwrap(), 2);
        let now = Utc::now();
        let later = (now + chrono::Duration::hours(6)).to_rfc3339();
        {
            let conn = store.lock().unwrap();
            for (id, account, kind, trigger, error) in [
                (1, 1, "triage", "arrival", HELD_BY_BUDGET),
                (2, 1, "triage", "migration", HELD_BY_BUDGET),
                (3, 1, "access", "source_access", HELD_BY_BUDGET),
                (4, 1, "triage", "revisit:due", HELD_BY_BUDGET),
                (5, 1, "access", "archive_access", HELD_BY_BUDGET),
                (8, 1, "access", "archive_access", HELD_BY_BUDGET),
                (6, 1, "triage", "arrival", "provider_cooldown"),
                (7, 2, "triage", "arrival", HELD_BY_BUDGET),
            ] {
                conn.execute(
                    "INSERT INTO agent_triage_jobs
                     (id,account_id,message_id,kind,trigger,input_revision,state,available_at,last_error)
                     VALUES(?1,?2,?3,?4,?5,1,'queued',?6,?7)",
                    params![id, account, id, kind, trigger, later, error],
                )
                .unwrap();
            }
        }
        // Archive checks get their sub-cap's room: one of the two here.
        let config = AgentConfig {
            archive_access_daily_cap: 1,
            ..AgentConfig::default()
        };
        assert_eq!(store.release_budget_held_jobs(1, &config, now).unwrap(), 4);
        let due: Vec<i64> = {
            let conn = store.lock().unwrap();
            let mut stmt = conn
                .prepare("SELECT id FROM agent_triage_jobs WHERE available_at<=?1 ORDER BY id")
                .unwrap();
            stmt.query_map(params![now.to_rfc3339()], |row| row.get(0))
                .unwrap()
                .map(|r| r.unwrap())
                .collect()
        };
        assert_eq!(due, [1, 2, 3, 5]);
        // Nothing left to release, the archive room now spoken for.
        assert_eq!(store.release_budget_held_jobs(1, &config, now).unwrap(), 0);
    }

    /// The store's lane SQL and the worker's charge are two statements of
    /// one rule. Every kind, trigger and lane flag a job can carry lands in
    /// the lane whose key the worker charges, so a job the cap parks is
    /// counted, and released by a grant, under the budget that parked it.
    #[test]
    fn the_held_lanes_match_the_keys_the_worker_charges() {
        use crate::store::agent_triage::AgentJob;
        use crate::sync::triage_worker::investigation_budget_limits;
        let now = Utc::now();
        let later = (now + chrono::Duration::hours(6)).to_rfc3339();
        let config = crate::config::Config::default();
        for (kind, trigger, foreground) in [
            ("triage", "arrival", true),
            ("triage", "manual", true),
            ("triage", "migration", false),
            ("triage", "refresh", false),
            ("triage", "revisit:due", false),
            ("access", "source_access", false),
            ("access", "archive_access", false),
            ("access", "arrival", true),
            ("access", "heal", false),
            ("access", "revisit:due", false),
        ] {
            let store = SqliteStore::open_in_memory().unwrap();
            let account = store.ensure_account("lanes@example.com").unwrap();
            {
                let conn = store.lock().unwrap();
                conn.execute(
                    "INSERT INTO agent_triage_jobs
                     (id,account_id,message_id,kind,trigger,input_revision,state,available_at,last_error)
                     VALUES(1,?1,1,?2,?3,1,'queued',?4,?5)",
                    params![account, kind, trigger, later, HELD_BY_BUDGET],
                )
                .unwrap();
                conn.execute(
                    "INSERT INTO agent_job_lanes(job_id,foreground) VALUES(1,?1)",
                    params![foreground],
                )
                .unwrap();
            }
            let job = AgentJob {
                account_id: account,
                message_id: 1,
                kind: kind.into(),
                trigger: trigger.into(),
                foreground,
                ..AgentJob::default()
            };
            let keys: Vec<String> = investigation_budget_limits(&config, &job)
                .into_iter()
                .map(|(key, _)| key)
                .collect();
            let status = store
                .daily_budget_status(account, &config.triage.agent, now)
                .unwrap();
            let held = (
                status.held_arrival,
                status.held_background,
                status.held_access,
                status.held_revisit,
            );
            let expected = if keys.iter().any(|key| key.starts_with("__agent_revisit__")) {
                (0, 0, 0, 1)
            } else if keys.iter().any(|key| key == ACCESS_BUDGET_KEY) {
                (0, 0, 1, 0)
            } else if keys.iter().any(|key| key == BACKGROUND_BUDGET_KEY) {
                (0, 1, 0, 0)
            } else {
                (1, 0, 0, 0)
            };
            assert_eq!(
                held, expected,
                "{kind} {trigger} foreground={foreground}: {keys:?}"
            );
        }
    }

    #[test]
    fn concurrent_threads_cannot_overshoot_the_account_cap() {
        let store = std::sync::Arc::new(SqliteStore::open_in_memory().unwrap());
        let account = store.ensure_account("concurrent@example.com").unwrap();
        let handles: Vec<_> = (0..16)
            .map(|index| {
                let store = store.clone();
                std::thread::spawn(move || {
                    store
                        .reserve_agent_budget(
                            account,
                            "today",
                            &[("global".into(), 3), (format!("thread:{index}"), 1)],
                        )
                        .unwrap()
                })
            })
            .collect();
        let won = handles
            .into_iter()
            .map(|h| usize::from(h.join().unwrap()))
            .sum::<usize>();
        assert_eq!(won, 3);
        assert_eq!(
            store
                .stage2_budget_used(account, "global", "today")
                .unwrap(),
            3
        );
    }
}
