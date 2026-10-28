//! Smart re-notify's store half: sessions, candidates, busy intervals, and
//! the banner record.

use super::super::*;
use super::support::*;
use crate::renotify::{BusySpan, Trigger};
use chrono::{Duration, TimeZone};

fn at(h: u32, m: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 28, h, m, 0).unwrap()
}

/// A notified message: the `messages` row and its `events` row.
fn notified(store: &SqliteStore, acct: AccountId, id: i64) {
    store
        .append_event(&new_event(store, acct, id))
        .unwrap()
        .unwrap();
}

fn triage_row(store: &SqliteStore, acct: AccountId, id: i64, set: &str) {
    store
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO triage(message_id, account_id, created_at) VALUES(?1, ?2, ?3)
             ON CONFLICT(message_id) DO NOTHING",
            params![id, acct, Utc::now().to_rfc3339()],
        )
        .unwrap();
    store
        .lock()
        .unwrap()
        .execute(
            &format!("UPDATE triage SET {set} WHERE message_id = ?1"),
            params![id],
        )
        .unwrap();
}

fn ids(store: &SqliteStore, acct: AccountId) -> Vec<i64> {
    store
        .renotify_candidates(acct)
        .unwrap()
        .iter()
        .map(|c| c.message_id)
        .collect()
}

#[test]
fn the_first_session_is_not_an_arrival_and_a_gap_is_measured() {
    let (store, acct) = store();
    let (first, away) = store.open_client_session(acct, "macos", at(8, 0)).unwrap();
    assert_eq!(away, None, "a first connection is onboarding");

    // A second stream while the first is open: no gap at all.
    let (second, away) = store.open_client_session(acct, "macos", at(8, 5)).unwrap();
    assert_eq!(away, Some(Duration::zero()));

    assert!(store.close_client_session(acct, first, at(9, 0)).unwrap());
    assert!(store.close_client_session(acct, second, at(9, 30)).unwrap());
    assert!(
        !store.close_client_session(acct, second, at(10, 0)).unwrap(),
        "closing twice keeps the first stamp"
    );
    assert!(!store.client_session_open(acct, second).unwrap());

    // Away since the LATEST disconnect.
    let (_, away) = store
        .open_client_session(acct, "macos", at(12, 30))
        .unwrap();
    assert_eq!(away, Some(Duration::hours(3)));
}

#[test]
fn sessions_are_per_platform_and_per_account() {
    let (store, acct) = store();
    let other = store.ensure_account("other@example.com").unwrap();
    let (s, _) = store.open_client_session(acct, "macos", at(8, 0)).unwrap();
    store.close_client_session(acct, s, at(9, 0)).unwrap();
    // Another account's open session says nothing about this one.
    store
        .open_client_session(other, "macos", at(9, 30))
        .unwrap();
    // Nor does a phone.
    store.open_client_session(acct, "ios", at(9, 30)).unwrap();
    let (_, away) = store.open_client_session(acct, "macos", at(11, 0)).unwrap();
    assert_eq!(away, Some(Duration::hours(2)));
    // Another account cannot close this one's session.
    let (mine, _) = store.open_client_session(acct, "macos", at(11, 5)).unwrap();
    assert!(!store.close_client_session(other, mine, at(11, 6)).unwrap());
}

#[test]
fn a_restart_closes_what_the_crash_left_open() {
    let (store, acct) = store();
    store.open_client_session(acct, "macos", at(8, 0)).unwrap();
    assert_eq!(
        store.close_stale_client_sessions(acct, at(10, 0)).unwrap(),
        1
    );
    // The Mac reconnecting right after the restart is no arrival.
    let (_, away) = store.open_client_session(acct, "macos", at(10, 1)).unwrap();
    assert_eq!(away, Some(Duration::minutes(1)));
}

#[test]
fn candidates_are_notified_mail_still_unhandled() {
    let (store, acct) = store();
    for id in 1..=6 {
        notified(&store, acct, id);
    }
    triage_row(
        &store,
        acct,
        2,
        &format!("opened_at = '{}'", Utc::now().to_rfc3339()),
    );
    triage_row(&store, acct, 3, "status = 'done'");
    triage_row(
        &store,
        acct,
        4,
        &format!("remind_at = '{}'", Utc::now().to_rfc3339()),
    );
    // A reply on message 5's thread, sent after it.
    store
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO messages(id,account_id,gmail_msg_id,thread_id,from_addr,subject,received_at,snippet,is_sent)
             VALUES(50,?1,'reply','t5','me@example.com','Re',?2,'',1)",
            params![acct, Utc::now().to_rfc3339()],
        )
        .unwrap();
    // An open-tracking notice is not a candidate either.
    let mut opened = new_event(&store, acct, 7);
    opened.kind = EventKind::Opened;
    store.append_event(&opened).unwrap().unwrap();
    // Nor an auth alert.
    let mut auth = new_event(&store, acct, 8);
    auth.is_auth = true;
    store.append_event(&auth).unwrap().unwrap();

    assert_eq!(ids(&store, acct), vec![6, 1], "newest first");
    let other = store.ensure_account("other@example.com").unwrap();
    assert!(ids(&store, other).is_empty());
}

#[test]
fn a_banner_is_recorded_counted_and_tapped() {
    let (store, acct) = store();
    notified(&store, acct, 1);
    notified(&store, acct, 2);
    let fired = at(9, 0);
    let batch = store
        .record_renotify(
            acct,
            Trigger::ClientOnline,
            fired,
            1,
            &[(2, None), (1, None)],
        )
        .unwrap();

    let cands = store.renotify_candidates(acct).unwrap();
    assert!(cands.iter().all(|c| c.renotified == 1));
    assert!(cands.iter().all(|c| c.last_renotified_at == Some(fired)));
    assert_eq!(store.renotify_fired_since(acct, at(0, 0)).unwrap(), 1);
    assert_eq!(store.renotify_fired_since(acct, at(9, 1)).unwrap(), 0);

    let other = store.ensure_account("other@example.com").unwrap();
    assert!(
        !store
            .mark_renotify_opened(other, batch, None, at(9, 5))
            .unwrap()
    );
    assert!(
        !store
            .mark_renotify_opened(acct, batch, Some(99), at(9, 5))
            .unwrap(),
        "a message the banner did not name"
    );
    assert!(
        store
            .mark_renotify_opened(acct, batch, Some(1), at(9, 5))
            .unwrap()
    );

    // Message 1 is then dealt with; message 2 is not.
    triage_row(
        &store,
        acct,
        1,
        &format!("status = 'done', resolved_at = '{}'", at(9, 6).to_rfc3339()),
    );
    let outcomes = store.renotify_outcomes(acct, at(0, 0)).unwrap();
    assert_eq!(outcomes.len(), 1);
    assert!(outcomes[0].opened);
    assert_eq!((outcomes[0].items, outcomes[0].handled), (2, 1));
    assert_eq!(outcomes[0].trigger, Trigger::ClientOnline);
    assert!(store.renotify_outcomes(acct, at(10, 0)).unwrap().is_empty());
}

#[test]
fn handling_before_the_banner_is_not_credited_to_it() {
    let (store, acct) = store();
    notified(&store, acct, 1);
    triage_row(
        &store,
        acct,
        1,
        &format!("status = 'done', resolved_at = '{}'", at(8, 0).to_rfc3339()),
    );
    store
        .record_renotify(acct, Trigger::MeetingEnd, at(9, 0), 1, &[(1, None)])
        .unwrap();
    let outcomes = store.renotify_outcomes(acct, at(0, 0)).unwrap();
    assert_eq!(outcomes[0].handled, 0);
}

fn calendar_event(
    store: &SqliteStore,
    acct: AccountId,
    id: &str,
    start: &str,
    end: Option<&str>,
    extra: &str,
) {
    let conn = store.lock().unwrap();
    conn.execute(
        "INSERT OR IGNORE INTO calendar_calendars(account_id, calendar_id, is_primary) VALUES(?1,'primary',1)",
        params![acct],
    )
    .unwrap();
    let start_sort =
        super::super::ghosts::sort_key(&crate::triage::ghost::EventTime::parse(start).unwrap());
    conn.execute(
        "INSERT INTO calendar_events(account_id, calendar_id, event_id, status, summary, starts_at,
                                     start_sort, ends_at, all_day)
         VALUES(?1,'primary',?2,'confirmed','Secret title',?3,?4,?5,0)",
        params![acct, id, start, start_sort, end],
    )
    .unwrap();
    if !extra.is_empty() {
        conn.execute(
            &format!("UPDATE calendar_events SET {extra} WHERE event_id = ?1"),
            params![id],
        )
        .unwrap();
    }
}

#[test]
fn busy_is_timed_accepted_opaque_events_only() {
    let (store, acct) = store();
    calendar_event(
        &store,
        acct,
        "a",
        "2026-10-28T10:00:00Z",
        Some("2026-10-28T11:00:00Z"),
        "",
    );
    calendar_event(
        &store,
        acct,
        "free",
        "2026-10-28T12:00:00Z",
        Some("2026-10-28T13:00:00Z"),
        "transparent = 1",
    );
    calendar_event(
        &store,
        acct,
        "no",
        "2026-10-28T14:00:00Z",
        Some("2026-10-28T15:00:00Z"),
        "self_response = 'declined'",
    );
    calendar_event(&store, acct, "day", "2026-10-28", None, "all_day = 1");
    // An offset is read as the instant it names.
    calendar_event(&store, acct, "pdt", "2026-10-28T09:00:00-07:00", None, "");
    // On a calendar Passband no longer reads.
    {
        let conn = store.lock().unwrap();
        conn.execute(
            "INSERT INTO calendar_calendars(account_id, calendar_id, selected) VALUES(?1,'off',0)",
            params![acct],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO calendar_events(account_id, calendar_id, event_id, status, starts_at, start_sort, ends_at, all_day)
             VALUES(?1,'off','x','confirmed','2026-10-28T17:00:00Z','2026-10-28T17:00:00Z','2026-10-28T18:00:00Z',0)",
            params![acct],
        )
        .unwrap();
    }

    let busy = store.calendar_busy(acct, at(0, 0), at(23, 59)).unwrap();
    assert_eq!(
        busy,
        vec![
            BusySpan {
                start: at(10, 0),
                end: at(11, 0)
            },
            // No end: half an hour.
            BusySpan {
                start: at(16, 0),
                end: at(16, 30)
            },
        ]
    );
    // The window clips by overlap, not by start.
    assert_eq!(
        store
            .calendar_busy(acct, at(10, 30), at(10, 45))
            .unwrap()
            .len(),
        1
    );
    assert!(
        store
            .calendar_busy(acct, at(11, 0), at(12, 0))
            .unwrap()
            .is_empty()
    );
}

/// Mail in For your eyes with a due date, committed the way the worker
/// commits a decision. `attention_at` is the due time.
fn fye_with_due(store: &SqliteStore, acct: AccountId, key: &str, due: &str) -> i64 {
    use crate::triage::decision::{EvidenceRef, SupportedTime, ThreadAttentionDecision};
    let id = super::reading_senders::mail(store, acct, key, "billing@utility.example", 10);
    super::reading_senders::into_reading_with(store, acct, id, |d| {
        d.destinations.clear();
        d.summary = "Electric bill due".into();
        d.attention = ThreadAttentionDecision {
            show_in_fye: true,
            relevant_message_ids: vec![id],
            evidence: vec![EvidenceRef {
                message_id: id,
                location: "body".into(),
            }],
            summary: "Pay the electric bill".into(),
            factors: crate::triage::decision::AttentionFactors {
                importance: 0.8,
                attention_at: Some(SupportedTime {
                    value: due.into(),
                    timezone: None,
                    source_message_id: id,
                    interpreted_relative: false,
                }),
                ..Default::default()
            },
            ..Default::default()
        };
    });
    id
}

#[test]
fn opened_fye_mail_with_a_due_date_is_a_due_soon_candidate_once() {
    use crate::renotify::{Policy, Why};
    let (store, acct) = store();
    let due = Utc::now() + Duration::hours(6);
    let id = fye_with_due(&store, acct, "bill", &due.to_rfc3339());
    // Opened: the ordinary path drops it, the due path does not.
    triage_row(
        &store,
        acct,
        id,
        &format!("opened_at = '{}'", Utc::now().to_rfc3339()),
    );
    assert!(store.renotify_candidates(acct).unwrap().is_empty());

    let p = Policy::default();
    let due_list = |store: &SqliteStore| {
        store
            .renotify_due_candidates(acct, &p, chrono_tz::Tz::UTC, Utc::now())
            .unwrap()
    };
    let found = due_list(&store);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].message_id, id);
    assert_eq!(found[0].why, Why::DueSoon);
    assert_eq!(found[0].due_at, Some(due));
    assert_eq!(found[0].one_line, "Electric bill due");
    assert_eq!(found[0].importance, 80);

    // Once per due date.
    store
        .record_renotify(acct, Trigger::MeetingEnd, Utc::now(), 1, &[(id, Some(due))])
        .unwrap();
    assert!(due_list(&store).is_empty());
    // A due date that MOVES is a new one.
    let later = due + Duration::days(2);
    store
        .lock()
        .unwrap()
        .execute(
            "UPDATE agent_thread_attention SET attention_json =
                 json_set(attention_json, '$.factors.attention_at.value', ?1)",
            params![later.to_rfc3339()],
        )
        .unwrap();
    let moved = due_list(&store);
    assert_eq!(moved.len(), 1);
    assert_eq!(moved[0].due_at, Some(later));
}

#[test]
fn done_or_snoozed_fye_mail_is_not_due_soon() {
    use crate::renotify::Policy;
    let (store, acct) = store();
    let due = (Utc::now() + Duration::hours(6)).to_rfc3339();
    let snoozed = fye_with_due(&store, acct, "snoozed", &due);
    triage_row(
        &store,
        acct,
        snoozed,
        &format!(
            "remind_at = '{}'",
            (Utc::now() + Duration::days(1)).to_rfc3339()
        ),
    );
    let done = fye_with_due(&store, acct, "done", &due);
    triage_row(&store, acct, done, "status = 'done'");
    let found = store
        .renotify_due_candidates(acct, &Policy::default(), chrono_tz::Tz::UTC, Utc::now())
        .unwrap();
    assert!(found.is_empty(), "{found:?}");
}
