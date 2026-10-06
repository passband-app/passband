//! Atomic spend reservations for the bounded investigation worker.
use super::*;
use crate::triage::agent_config::{
    AgentConfig, BACKGROUND_BUDGET_KEY, DailyBudgetStatus, TOTAL_BUDGET_KEY,
};

/// What a held job's `last_error` reads while it waits for tomorrow.
const HELD_BY_BUDGET: &str = "daily_budget_exhausted";

/// The `wake_budget` row holding a one-day grant on top of `key`'s cap. It is
/// keyed by day like the spend it extends, so it lapses at midnight UTC with
/// nothing to undo.
fn grant_key(key: &str) -> String {
    format!("{key}#grant")
}

fn runs(connection: &Connection, account: AccountId, key: &str, day: &str) -> Result<u32> {
    let used: i64 = connection.query_row(
        "SELECT COALESCE((SELECT model_calls FROM wake_budget WHERE account_id=?1 AND thread_id=?2 AND day=?3),0)",
        params![account, key, day],
        |row| row.get(0),
    )?;
    Ok(used.clamp(0, i64::from(u32::MAX)) as u32)
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
        // Mirrors `investigation_budget_limits`: a job spends background
        // capacity unless it is a foreground triage.
        let (held_arrival, held_background): (u64, u64) = connection.query_row(
            "SELECT
                 COALESCE(SUM(COALESCE(l.foreground,0)=1 AND j.kind='triage'),0),
                 COALESCE(SUM(NOT (COALESCE(l.foreground,0)=1 AND j.kind='triage')),0)
             FROM agent_triage_jobs j LEFT JOIN agent_job_lanes l ON l.job_id=j.id
             WHERE j.account_id=?1 AND j.kind IN ('triage','access') AND j.state='queued'
               AND j.last_error=?2 AND j.available_at>?3",
            params![account, HELD_BY_BUDGET, now.to_rfc3339()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        Ok(DailyBudgetStatus {
            total_used: runs(&connection, account, TOTAL_BUDGET_KEY, &day)?,
            total_cap: config.daily_run_cap.saturating_add(runs(
                &connection,
                account,
                &grant_key(TOTAL_BUDGET_KEY),
                &day,
            )?),
            background_used: runs(&connection, account, BACKGROUND_BUDGET_KEY, &day)?,
            background_cap: config
                .effective_background_daily_run_cap()
                .saturating_add(runs(
                    &connection,
                    account,
                    &grant_key(BACKGROUND_BUDGET_KEY),
                    &day,
                )?),
            held_arrival,
            held_background,
        })
    }

    /// Raise today's cap on `key` by `extra` runs and let the jobs it parked
    /// run now. Returns how many jobs were released.
    ///
    /// The grant REPLACES any earlier one for the day rather than adding to
    /// it, so running the command twice is not twice the spend, and zero takes
    /// it back. Released jobs that still do not fit park themselves again.
    pub fn grant_daily_runs(
        &self,
        account: AccountId,
        key: &str,
        extra: u32,
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
        let released = transaction.execute(
            "UPDATE agent_triage_jobs SET available_at=?3
             WHERE account_id=?1 AND kind IN ('triage','access') AND state='queued'
               AND last_error=?2 AND available_at>?3",
            params![account, HELD_BY_BUDGET, now.to_rfc3339()],
        )?;
        transaction.commit()?;
        Ok(released)
    }
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
    for (key, cap) in limits {
        let used: i64 = transaction.query_row(
            "SELECT COALESCE((SELECT model_calls FROM wake_budget WHERE account_id=?1 AND thread_id=?2 AND day=?3),0)",
            params![account,key,day], |row| row.get(0))?;
        let granted: i64 = transaction.query_row(
            "SELECT COALESCE((SELECT model_calls FROM wake_budget WHERE account_id=?1 AND thread_id=?2 AND day=?3),0)",
            params![account,grant_key(key),day], |row| row.get(0))?;
        if used >= i64::from(*cap) + granted {
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
        let limits = vec![(TOTAL_BUDGET_KEY.to_string(), 1)];
        let reserve = |day: &str| store.reserve_agent_budget(account, day, &limits).unwrap();
        assert!(reserve("2026-10-06"));
        assert!(!reserve("2026-10-06"));

        store
            .grant_daily_runs(account, TOTAL_BUDGET_KEY, 5, now)
            .unwrap();
        store
            .grant_daily_runs(account, TOTAL_BUDGET_KEY, 2, now)
            .unwrap();
        assert!(reserve("2026-10-06"));
        assert!(reserve("2026-10-06"));
        assert!(!reserve("2026-10-06"), "2 granted, not 5 + 2");

        // Tomorrow is back on the configured cap.
        assert!(reserve("2026-10-07"));
        assert!(!reserve("2026-10-07"));

        let config = AgentConfig {
            daily_run_cap: 1,
            ..AgentConfig::default()
        };
        let status = store.daily_budget_status(account, &config, now).unwrap();
        assert_eq!((status.total_used, status.total_cap), (3, 3));
        // The grant is not spend, and background has none of its own.
        assert_eq!((status.background_used, status.background_cap), (0, 0));

        store
            .grant_daily_runs(account, TOTAL_BUDGET_KEY, 0, now)
            .unwrap();
        assert_eq!(
            store
                .daily_budget_status(account, &config, now)
                .unwrap()
                .total_cap,
            1
        );
    }

    /// Held counts split the way the worker charges, see only jobs parked by
    /// the budget and still waiting, and a grant releases exactly those.
    #[test]
    fn held_jobs_are_counted_by_lane_and_released_by_a_grant() {
        let store = SqliteStore::open_in_memory().unwrap();
        assert_eq!(store.ensure_account("held@example.com").unwrap(), 1);
        assert_eq!(store.ensure_account("other@example.com").unwrap(), 2);
        let now = Utc::now();
        let later = (now + chrono::Duration::hours(6)).to_rfc3339();
        let earlier = (now - chrono::Duration::hours(1)).to_rfc3339();
        {
            let conn = store.lock().unwrap();
            // (id, account, kind, trigger, last_error, available_at, foreground)
            for (id, account, kind, trigger, error, at, foreground) in [
                (
                    1,
                    1,
                    "triage",
                    "arrival",
                    Some(HELD_BY_BUDGET),
                    &later,
                    Some(1),
                ),
                (
                    2,
                    1,
                    "triage",
                    "migration",
                    Some(HELD_BY_BUDGET),
                    &later,
                    Some(0),
                ),
                (
                    3,
                    1,
                    "triage",
                    "backfill",
                    Some(HELD_BY_BUDGET),
                    &later,
                    None,
                ),
                // An access job is background even in the foreground lane.
                (
                    4,
                    1,
                    "access",
                    "access",
                    Some(HELD_BY_BUDGET),
                    &later,
                    Some(1),
                ),
                // Already due again: no longer held.
                (
                    5,
                    1,
                    "triage",
                    "due",
                    Some(HELD_BY_BUDGET),
                    &earlier,
                    Some(1),
                ),
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
                (
                    8,
                    2,
                    "triage",
                    "arrival",
                    Some(HELD_BY_BUDGET),
                    &later,
                    Some(1),
                ),
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
        let config = AgentConfig::default();
        let status = store.daily_budget_status(1, &config, now).unwrap();
        assert_eq!((status.held_arrival, status.held_background), (1, 3));

        assert_eq!(
            store
                .grant_daily_runs(1, BACKGROUND_BUDGET_KEY, 50, now)
                .unwrap(),
            4
        );
        let status = store.daily_budget_status(1, &config, now).unwrap();
        assert_eq!((status.held_arrival, status.held_background), (0, 0));
        assert_eq!(status.background_cap, 250);
        assert_eq!(status.total_cap, 1000);
        // The other account's job was not touched.
        assert_eq!(
            store
                .daily_budget_status(2, &config, now)
                .unwrap()
                .held_arrival,
            1
        );
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
