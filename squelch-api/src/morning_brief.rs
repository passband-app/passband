//! The morning brief: once a local day, around 08:00 in the owner's zone, a
//! push to the owner's iPhone(s) saying what today holds, from their mail and
//! calendar: "PG&E bill due tomorrow. A package arrives today. First up:
//! Dentist at 9:30 AM." A tap opens the app's board.
//!
//! THE RELAY STAYS BLIND. The worker hands the APNs pusher a
//! [`BriefPing`] (the owner's local day and nothing else); the relay pushes
//! its fixed "Your morning brief is ready" and the phone's extension fetches
//! `GET /client/brief` from this daemon to write the real banner. The words
//! are built here, at fetch time, so the banner says what is true when it is
//! read, not what was true at the minute the worker woke.
//!
//! WHEN. From [`SEND_FROM`] to [`SEND_UNTIL`] owner-local: 08:00, or as soon
//! after as the daemon is up, and never after 11:00, because a brief at
//! lunchtime is a different and worse notification. ONCE PER LOCAL DAY, by a
//! durable stamp in `app_settings` ([`SENT_KEY`]) written BEFORE the ping goes
//! out, so neither a restart nor a crash mid-send can buzz the phone twice.
//! A day that is decided (sent, or empty) is decided for good. A daemon with
//! no idea where its owner is (no app has reported a zone and no calendar
//! names one) sends nothing: 08:00 UTC is midnight in California.
//!
//! EMPTY DAYS ARE SKIPPED. No bill due today or tomorrow, no package
//! arriving and nothing on the calendar: no push. A content-free daily buzz
//! teaches people to swipe us away. Mail waiting on the owner is NOT part of
//! the brief on purpose: a morning count of it reads as a to-do list.
//!
//! OFF. One owner preference ([`ENABLED_KEY`], on unless turned off), per
//! mailbox, behind `GET`/`PUT /client/brief/settings`. Only iPhones are
//! pushed ([`squelch_core::push::IOS_PLATFORM`]); a daemon with none
//! registered does nothing at all.
//!
//! HUMAN DOOR ONLY. The agent door has no route here.

use std::time::Duration;

use axum::{Json, extract::State, response::IntoResponse};
use chrono::{DateTime, NaiveDate, NaiveTime, TimeZone, Timelike, Utc};
use chrono_tz::Tz;
use serde::Deserialize;
use squelch_core::owner_clock::{OwnerZone, OwnerZoneSource};
use squelch_core::push::{BriefPing, IOS_PLATFORM};
use squelch_core::store::agent_triage::{AgentListQuery, AgentTriageStore};
use squelch_core::store::{ClientPlatform, SqliteStore, Store};
use squelch_core::triage::decision::{RecordProposal, StatedTime};
use squelch_core::types::AccountId;
use tokio::sync::watch;

use crate::error::ApiError;
use crate::handlers::{blocking, no_store, store_call};
use crate::state::ApiState;

/// The owner preference, in `app_settings`: `"0"` is off, anything else (or
/// nothing) is on.
pub const ENABLED_KEY: &str = "morning_brief";
/// The local day (`YYYY-MM-DD`) the brief was last decided for, sent or
/// skipped as empty.
pub const SENT_KEY: &str = "morning_brief_day";
/// The first local hour the brief goes out in.
pub const SEND_FROM: u32 = 8;
/// The local hour it stops trying: a daemon that was down until 11:00 skips
/// the day.
pub const SEND_UNTIL: u32 = 11;

/// Longest an event's or a biller's name runs in the banner before it is cut.
const NAME_CAP: usize = 32;

/// The worker's clock. [`MorningBriefTiming::daemon`] in production.
#[derive(Debug, Clone, Copy)]
pub struct MorningBriefTiming {
    pub tick: Duration,
}

impl MorningBriefTiming {
    pub fn daemon() -> Self {
        Self {
            tick: Duration::from_secs(60),
        }
    }
}

/// The local day a brief is due for at `now`, or None. Due when the owner's
/// zone is known, the local clock reads [`SEND_FROM`]:00 to before
/// [`SEND_UNTIL`]:00, and `sent` (the stamp) is not already that day.
pub fn due_day(now: DateTime<Utc>, zone: OwnerZone, sent: Option<&str>) -> Option<NaiveDate> {
    if zone.source == OwnerZoneSource::Default {
        return None;
    }
    let local = now.with_timezone(&zone.zone);
    if !(SEND_FROM..SEND_UNTIL).contains(&local.hour()) {
        return None;
    }
    let today = local.date_naive();
    let stamp = today.format("%Y-%m-%d").to_string();
    (sent.map(str::trim) != Some(stamp.as_str())).then_some(today)
}

/// When a bill falls, relative to the owner's today.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Due {
    Today,
    Tomorrow,
}

/// What today holds, as read from the store. The input to [`compose`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Facts {
    /// Bills due today or tomorrow, by who sent them (empty when unknown).
    pub bills: Vec<(String, Due)>,
    /// Packages out for delivery or due today.
    pub packages: usize,
    /// The first timed event still ahead today: its title and local start.
    pub first_event: Option<(String, NaiveTime)>,
}

/// THE COPY: the banner's body, a few short sentences about what today
/// holds, or None for an empty day. No title (the phone shows the app's
/// name above it), no date, no count of mail waiting: a calm summary, not a
/// to-do tally. No exclamation marks, no emoji, no dashes.
///
///   PG&E bill due tomorrow. A package arrives today. First up: Dentist at 9:30 AM.
pub fn compose(facts: &Facts) -> Option<String> {
    let mut sentences: Vec<String> = Vec::new();
    match facts.bills.as_slice() {
        [] => {}
        [(name, due)] => {
            let when = match due {
                Due::Today => "today",
                Due::Tomorrow => "tomorrow",
            };
            let name = clip(name);
            sentences.push(if name.is_empty() {
                format!("A bill due {when}.")
            } else {
                format!("{name} bill due {when}.")
            });
        }
        many => {
            let n = many.len();
            sentences.push(if many.iter().all(|(_, d)| *d == Due::Today) {
                format!("{n} bills due today.")
            } else {
                format!("{n} bills due by tomorrow.")
            });
        }
    }
    match facts.packages {
        0 => {}
        1 => sentences.push("A package arrives today.".into()),
        n => sentences.push(format!("{n} packages arrive today.")),
    }
    if let Some((title, at)) = &facts.first_event {
        let title = clip(title);
        let title = title.trim_end_matches('.');
        let at = clock(*at);
        sentences.push(if title.is_empty() {
            format!("First up: an event at {at}.")
        } else {
            format!("First up: {title} at {at}.")
        });
    }
    (!sentences.is_empty()).then(|| sentences.join(" "))
}

/// `9:30 AM`, `2 PM`: the minutes only when there are some.
fn clock(at: NaiveTime) -> String {
    let (pm, hour) = at.hour12();
    let half = if pm { "PM" } else { "AM" };
    if at.minute() == 0 {
        format!("{hour} {half}")
    } else {
        format!("{hour}:{:02} {half}", at.minute())
    }
}

/// One line of untrusted text (an event title, a biller's name), folded to
/// single spaces and cut at [`NAME_CAP`] characters with an ellipsis.
fn clip(raw: &str) -> String {
    let folded = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if folded.chars().count() <= NAME_CAP {
        return folded;
    }
    let cut: String = folded.chars().take(NAME_CAP - 1).collect();
    format!("{}…", cut.trim_end())
}

/// The day a stated time falls on, in `zone`.
fn local_day(time: &squelch_core::triage::decision::SupportedTime, zone: Tz) -> Option<NaiveDate> {
    Some(match time.stated()? {
        StatedTime::Instant(at) => at.with_timezone(&zone).date_naive(),
        StatedTime::Local(at) => at.date(),
        StatedTime::Day(day) => day,
    })
}

/// Read what today holds. Every source reuses the query its board already
/// runs: the Records board's bills ([`AgentTriageStore::agent_records_with_query`], as the agent door's
/// `get_deadlines` reads them), the shipments listing, and the calendar
/// timeline's events (only while calendar is granted).
#[allow(clippy::too_many_arguments)]
pub fn gather(
    store: &SqliteStore,
    account: AccountId,
    now: DateTime<Utc>,
    zone: Tz,
    policy: squelch_core::config::ShipmentListPolicy,
    calendar: bool,
) -> squelch_core::Result<Facts> {
    let local_now = now.with_timezone(&zone);
    let today = local_now.date_naive();
    let tomorrow = today.succ_opt().unwrap_or(today);

    let mut bills: Vec<(String, Due)> = Vec::new();
    let records = store.agent_records_with_query(
        account,
        usize::MAX,
        &AgentListQuery {
            since: None,
            include_done: false,
            reading_board: false,
        },
    )?;
    for item in &records {
        for record in &item.decision.records {
            if let RecordProposal::Bill {
                merchant,
                due: Some(due),
                autopay,
                ..
            } = record
            {
                // Autopay takes care of itself; it needs nobody this morning.
                if *autopay == Some(true) {
                    continue;
                }
                let when = match local_day(due, zone) {
                    Some(d) if d == today => Due::Today,
                    Some(d) if d == tomorrow => Due::Tomorrow,
                    _ => continue,
                };
                bills.push((merchant.trim().to_string(), when));
            }
        }
    }

    let packages = store
        .list_shipments(account, false, policy)?
        .iter()
        .filter(|s| match s.status.as_str() {
            "out_for_delivery" => true,
            "ordered" | "shipped" => s
                .eta
                .is_some_and(|eta| eta.with_timezone(&zone).date_naive() == today),
            _ => false,
        })
        .count();

    let first_event = if calendar {
        let end_of_day = zone
            .from_local_datetime(&tomorrow.and_hms_opt(0, 0, 0).unwrap_or_default())
            .earliest()
            .map(|t| t.with_timezone(&Utc))
            .unwrap_or(now + chrono::Duration::hours(16));
        let (events, _) = store.list_calendar_events(
            account,
            now.fixed_offset(),
            end_of_day.fixed_offset(),
            now,
        )?;
        events
            .iter()
            .filter(|e| e.row.self_response != Some(squelch_core::gcal::SelfResponse::Declined))
            .filter_map(|e| match e.row.start {
                squelch_core::triage::ghost::EventTime::At(at) => {
                    let at = at.with_timezone(&Utc);
                    (at >= now && at < end_of_day).then_some((at, e))
                }
                squelch_core::triage::ghost::EventTime::Date(_) => None,
            })
            .min_by_key(|(at, _)| *at)
            .map(|(at, e)| {
                (
                    e.row.summary.clone().unwrap_or_default(),
                    at.with_timezone(&zone).time(),
                )
            })
    } else {
        None
    };

    Ok(Facts {
        bills,
        packages,
        first_event,
    })
}

/// The owner's day and its brief (None when it is empty), read now.
async fn assemble(
    state: &ApiState,
    now: DateTime<Utc>,
) -> Result<(NaiveDate, Option<String>), ApiError> {
    let calendar = state.features().enabled(squelch_core::features::CALENDAR)
        && crate::calendar_remind::calendar_reach(state).await.0;
    let policy = state.shipment_policy;
    store_call(state, move |store, account| {
        let zone = store.owner_zone(account)?.zone;
        let today = now.with_timezone(&zone).date_naive();
        let facts = gather(store, account, now, zone, policy, calendar)?;
        Ok((today, compose(&facts)))
    })
    .await
}

/// Whether the owner has the brief on. A missing row is on.
fn enabled(store: &SqliteStore, account: AccountId) -> squelch_core::Result<bool> {
    Ok(store
        .get_app_setting(account, ENABLED_KEY)?
        .as_deref()
        .map(str::trim)
        != Some("0"))
}

/// The first iPhone app that fetches and shows the brief (and has its
/// switch). 0.0.14 is the last release without it. The daemon rolls before
/// the phone ships, so until this mailbox has seen an iPhone at least this
/// new, there is no brief at all rather than a daily empty banner.
pub const MIN_IOS_VERSION: [u32; 3] = [0, 0, 15];

/// Whether an iPhone app at `version` (dotted digits, as `ClientTag` keeps
/// it) shows the brief. Compared as numbers: "0.0.9" is older than "0.0.15".
fn dresses_brief(version: &str) -> bool {
    let parts: Option<Vec<u32>> = version.split('.').map(|p| p.parse().ok()).collect();
    parts.is_some_and(|v| v.as_slice() >= MIN_IOS_VERSION.as_slice())
}

/// What a pass did, for the log and for tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BriefOutcome {
    /// The owner turned it off.
    Off,
    /// Outside the window, already decided today, or no zone known.
    NotDue,
    /// No iPhone registered for this mailbox: nothing decided, so a phone
    /// that registers at 9 still hears today's.
    NoPhone,
    /// An iPhone is registered, but no iPhone app new enough to dress the
    /// brief has ever connected ([`MIN_IOS_VERSION`]): an older app would
    /// show the relay's bare "Your morning brief is ready" every day and has
    /// no switch to stop it. Nothing decided, as with [`Self::NoPhone`].
    OldPhone,
    /// Nothing worth saying today; decided, and nothing sent.
    Empty(NaiveDate),
    /// Stamped and handed to the pusher.
    Sent(NaiveDate),
}

/// ONE PASS: send today's brief if it is due, there is an iPhone to send it
/// to, and there is something to say. The day is stamped BEFORE the ping
/// leaves, whatever was decided.
pub async fn run_brief_pass(
    state: &ApiState,
    now: DateTime<Utc>,
) -> Result<BriefOutcome, ApiError> {
    let gate = store_call(state, move |store, account| {
        if !enabled(store, account)? {
            return Ok(Err(BriefOutcome::Off));
        }
        let zone = store.owner_zone(account)?;
        let sent = store.get_app_setting(account, SENT_KEY)?;
        let Some(day) = due_day(now, zone, sent.as_deref()) else {
            return Ok(Err(BriefOutcome::NotDue));
        };
        let phone = store
            .list_devices(account)?
            .iter()
            .any(|d| d.tag.is_some() && d.platform == IOS_PLATFORM);
        if !phone {
            return Ok(Err(BriefOutcome::NoPhone));
        }
        let dressed = store
            .client_versions_seen(account)?
            .iter()
            .any(|c| c.platform == ClientPlatform::Ios && dresses_brief(&c.version));
        if !dressed {
            return Ok(Err(BriefOutcome::OldPhone));
        }
        Ok(Ok(day))
    })
    .await?;
    let day = match gate {
        Ok(day) => day,
        Err(outcome) => return Ok(outcome),
    };
    let (today, brief) = assemble(state, now).await?;
    // The zone moved between the two reads (an app reported a new one):
    // decide on the next pass, with one answer for both.
    if today != day {
        return Ok(BriefOutcome::NotDue);
    }
    let stamp = day.format("%Y-%m-%d").to_string();
    store_call(state, move |store, account| {
        store.set_app_setting(account, SENT_KEY, &stamp)
    })
    .await?;
    if brief.is_none() {
        return Ok(BriefOutcome::Empty(day));
    }
    // Nobody listening (no relay configured) is not a failure: the stamp is
    // written and the day is over either way.
    let _ = state.brief_pings.send(BriefPing { day });
    Ok(BriefOutcome::Sent(day))
}

fn log(outcome: &Result<BriefOutcome, ApiError>) {
    // Never a count, a name or a time: only that it went.
    match outcome {
        Ok(BriefOutcome::Sent(_)) => eprintln!("squelchd: morning brief sent"),
        Ok(BriefOutcome::Empty(_)) => eprintln!("squelchd: morning brief skipped (nothing to say)"),
        Ok(_) => {}
        Err(_) => eprintln!("squelchd: a morning brief pass failed"),
    }
}

/// THE WORKER: a pass at start and every tick, each raced against shutdown.
pub async fn morning_brief_worker(
    state: ApiState,
    mut shutdown: watch::Receiver<bool>,
    timing: MorningBriefTiming,
) {
    loop {
        if *shutdown.borrow() {
            return;
        }
        tokio::select! {
            outcome = run_brief_pass(&state, Utc::now()) => log(&outcome),
            _ = shutdown.changed() => return,
        }
        tokio::select! {
            _ = tokio::time::sleep(timing.tick) => {}
            res = shutdown.changed() => {
                if res.is_err() || *shutdown.borrow() {
                    return;
                }
            }
        }
    }
}

/// `GET /client/brief`: today's brief as of now, for the phone's
/// notification extension. `{day, body}`; `body` null on a day with nothing
/// to say. No title: the banner has none. HUMAN DOOR ONLY.
pub async fn get_brief(State(state): State<ApiState>) -> Result<impl IntoResponse, ApiError> {
    let (day, body) = assemble(&state, Utc::now()).await?;
    Ok((
        no_store(),
        Json(serde_json::json!({
            "day": day.format("%Y-%m-%d").to_string(),
            "body": body,
        })),
    ))
}

#[derive(Debug, Deserialize)]
pub struct BriefSettings {
    enabled: bool,
}

/// `GET /client/brief/settings`: `{enabled}` (on unless turned off).
pub async fn get_settings(State(state): State<ApiState>) -> Result<impl IntoResponse, ApiError> {
    let enabled = store_call(&state, enabled).await?;
    Ok((no_store(), Json(serde_json::json!({ "enabled": enabled }))))
}

/// `PUT /client/brief/settings` `{enabled: bool}`: set it, and answer it as
/// stored. Off stops the next pass.
pub async fn put_settings(
    State(state): State<ApiState>,
    Json(body): Json<BriefSettings>,
) -> Result<impl IntoResponse, ApiError> {
    let on = blocking(&state, {
        let store = state.store.clone();
        let account = state.account_id;
        move || {
            store.set_app_setting(account, ENABLED_KEY, if body.enabled { "1" } else { "0" })?;
            enabled(&store, account)
        }
    })
    .await?;
    Ok((no_store(), Json(serde_json::json!({ "enabled": on }))))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zone(name: &str) -> OwnerZone {
        OwnerZone {
            zone: name.parse().unwrap(),
            source: OwnerZoneSource::Client,
        }
    }

    fn at(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    #[test]
    fn due_from_eight_until_eleven_local() {
        let la = zone("America/Los_Angeles");
        // 07:59 PST is too early; 08:00 and 10:59 are in; 11:00 is out.
        assert_eq!(due_day(at("2026-11-05T15:59:00Z"), la, None), None);
        assert_eq!(
            due_day(at("2026-11-05T16:00:00Z"), la, None),
            Some(day(2026, 11, 5))
        );
        assert_eq!(
            due_day(at("2026-11-05T18:59:59Z"), la, None),
            Some(day(2026, 11, 5))
        );
        assert_eq!(due_day(at("2026-11-05T19:00:00Z"), la, None), None);
    }

    #[test]
    fn once_a_day_by_the_stamp() {
        let la = zone("America/Los_Angeles");
        let now = at("2026-11-05T16:30:00Z");
        assert_eq!(due_day(now, la, Some("2026-11-05")), None);
        // Yesterday's stamp does not count for today.
        assert_eq!(due_day(now, la, Some("2026-11-04")), Some(day(2026, 11, 5)));
    }

    #[test]
    fn the_day_is_the_owners_not_utcs() {
        // 08:30 in Tokyo is still the 4th in UTC.
        let tokyo = zone("Asia/Tokyo");
        assert_eq!(
            due_day(at("2026-11-04T23:30:00Z"), tokyo, None),
            Some(day(2026, 11, 5))
        );
    }

    #[test]
    fn never_without_a_known_zone() {
        assert_eq!(
            due_day(at("2026-11-05T08:30:00Z"), OwnerZone::UTC, None),
            None
        );
        let calendar = OwnerZone {
            zone: Tz::UTC,
            source: OwnerZoneSource::Calendar,
        };
        assert_eq!(
            due_day(at("2026-11-05T08:30:00Z"), calendar, None),
            Some(day(2026, 11, 5))
        );
    }

    #[test]
    fn an_empty_day_says_nothing() {
        assert_eq!(compose(&Facts::default()), None);
    }

    fn nine_thirty() -> NaiveTime {
        NaiveTime::from_hms_opt(9, 30, 0).unwrap()
    }

    #[test]
    fn the_full_shape() {
        let facts = Facts {
            bills: vec![("PG&E".into(), Due::Tomorrow)],
            packages: 1,
            first_event: Some(("Dentist".into(), nine_thirty())),
        };
        assert_eq!(
            compose(&facts).unwrap(),
            "PG&E bill due tomorrow. A package arrives today. First up: Dentist at 9:30 AM."
        );
    }

    #[test]
    fn one_thing_at_a_time() {
        let package = Facts {
            packages: 1,
            ..Default::default()
        };
        assert_eq!(compose(&package).unwrap(), "A package arrives today.");
        let event = Facts {
            first_event: Some(("Dentist".into(), nine_thirty())),
            ..Default::default()
        };
        assert_eq!(compose(&event).unwrap(), "First up: Dentist at 9:30 AM.");
    }

    #[test]
    fn counts_and_plurals() {
        let bills = Facts {
            bills: vec![("A".into(), Due::Today), ("B".into(), Due::Tomorrow)],
            packages: 3,
            ..Default::default()
        };
        assert_eq!(
            compose(&bills).unwrap(),
            "2 bills due by tomorrow. 3 packages arrive today."
        );
        let today = Facts {
            bills: vec![("A".into(), Due::Today), ("B".into(), Due::Today)],
            ..Default::default()
        };
        assert_eq!(compose(&today).unwrap(), "2 bills due today.");
        let unnamed = Facts {
            bills: vec![(" ".into(), Due::Tomorrow)],
            ..Default::default()
        };
        assert_eq!(compose(&unnamed).unwrap(), "A bill due tomorrow.");
    }

    #[test]
    fn event_titles_are_folded_and_end_one_sentence() {
        let facts = Facts {
            first_event: Some((
                "  Standup\nwith   the team. ".into(),
                NaiveTime::from_hms_opt(14, 0, 0).unwrap(),
            )),
            ..Default::default()
        };
        assert_eq!(
            compose(&facts).unwrap(),
            "First up: Standup with the team at 2 PM."
        );
    }

    #[test]
    fn long_names_are_cut() {
        let facts = Facts {
            first_event: Some((
                "Quarterly planning offsite with the whole leadership group".into(),
                NaiveTime::from_hms_opt(9, 5, 0).unwrap(),
            )),
            ..Default::default()
        };
        let body = compose(&facts).unwrap();
        assert_eq!(
            body,
            "First up: Quarterly planning offsite with… at 9:05 AM."
        );
        assert!(!body.contains('—') && !body.contains('–') && !body.contains('·'));
    }

    #[test]
    fn only_an_iphone_from_0_0_15_on_dresses_the_brief() {
        for old in ["0.0.14", "0.0.9", "0.0.14.9", "garbage"] {
            assert!(!dresses_brief(old), "{old}");
        }
        for new in ["0.0.15", "0.0.15.1", "0.0.100", "0.1", "1.0.0"] {
            assert!(dresses_brief(new), "{new}");
        }
    }
}
