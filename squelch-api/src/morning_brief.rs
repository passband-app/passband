//! The morning brief: once a local day, around 08:00 in the owner's zone, a
//! push to the owner's iPhone(s) saying what today holds, from their mail and
//! calendar. "2 need you · PG&E bill due today", "First up: Dentist at
//! 9:30 AM". A tap opens the app's board.
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
//! EMPTY DAYS ARE SKIPPED. Nothing needing the owner, no bill due today or
//! tomorrow, no package arriving and nothing on the calendar: no push. A
//! content-free daily buzz teaches people to swipe us away.
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
use serde::{Deserialize, Serialize};
use squelch_core::owner_clock::{OwnerZone, OwnerZoneSource};
use squelch_core::push::{BriefPing, IOS_PLATFORM};
use squelch_core::store::agent_triage::{AgentListQuery, AgentTriageStore};
use squelch_core::store::{SqliteStore, Store};
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
    /// The For-your-eyes band, auth mail left out: a login code from last
    /// night is not something that needs anybody this morning.
    pub needs_you: usize,
    /// Bills due today or tomorrow, by who sent them (empty when unknown).
    pub bills: Vec<(String, Due)>,
    /// Packages out for delivery or due today.
    pub packages: usize,
    /// The first timed event still ahead today: its title and local start.
    pub first_event: Option<(String, NaiveTime)>,
}

/// The banner: a title and one or two short lines.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Brief {
    pub title: String,
    pub lines: Vec<String>,
}

/// THE COPY. None for an empty day. Short and calm: counts, at most one
/// name per clause, no exclamation marks, no emoji, no dashes.
///
///   Thursday, Nov 5
///   2 need you · PG&E bill due today · A package arrives today
///   First up: Dentist at 9:30 AM
pub fn compose(facts: &Facts, today: NaiveDate) -> Option<Brief> {
    let mut mail: Vec<String> = Vec::new();
    match facts.needs_you {
        0 => {}
        1 => mail.push("1 needs you".into()),
        n => mail.push(format!("{n} need you")),
    }
    match facts.bills.as_slice() {
        [] => {}
        [(name, due)] => {
            let when = match due {
                Due::Today => "today",
                Due::Tomorrow => "tomorrow",
            };
            let name = clip(name);
            mail.push(if name.is_empty() {
                format!("A bill due {when}")
            } else {
                format!("{name} bill due {when}")
            });
        }
        many => {
            let n = many.len();
            mail.push(if many.iter().all(|(_, d)| *d == Due::Today) {
                format!("{n} bills due today")
            } else {
                format!("{n} bills due by tomorrow")
            });
        }
    }
    match facts.packages {
        0 => {}
        1 => mail.push("A package arrives today".into()),
        n => mail.push(format!("{n} packages arrive today")),
    }
    let mut lines = Vec::new();
    if !mail.is_empty() {
        lines.push(mail.join(" · "));
    }
    if let Some((title, at)) = &facts.first_event {
        let title = clip(title);
        let at = clock(*at);
        lines.push(if title.is_empty() {
            format!("First up: an event at {at}")
        } else {
            format!("First up: {title} at {at}")
        });
    }
    if lines.is_empty() {
        return None;
    }
    Some(Brief {
        title: format!("{}, {}", today.format("%A"), today.format("%b %-d")),
        lines,
    })
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
/// runs: the FYE band ([`AgentTriageStore::agent_fye`]), the Records board's
/// bills ([`AgentTriageStore::agent_records_with_query`], as the agent door's
/// `get_deadlines` reads them), the shipments listing, and the calendar
/// timeline's events (only while calendar is granted).
#[allow(clippy::too_many_arguments)]
pub fn gather(
    store: &SqliteStore,
    account: AccountId,
    now: DateTime<Utc>,
    zone: Tz,
    ranking: &squelch_core::triage::agent_config::RankingConfig,
    policy: squelch_core::config::ShipmentListPolicy,
    calendar: bool,
) -> squelch_core::Result<Facts> {
    let local_now = now.with_timezone(&zone);
    let today = local_now.date_naive();
    let tomorrow = today.succ_opt().unwrap_or(today);

    let needs_you = store
        .agent_fye(account, usize::MAX, ranking, now)?
        .iter()
        .filter(|item| !item.decision.auth.is_auth())
        .count();

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
        needs_you,
        bills,
        packages,
        first_event,
    })
}

/// The owner's day and its brief (None when it is empty), read now.
async fn assemble(
    state: &ApiState,
    now: DateTime<Utc>,
) -> Result<(NaiveDate, Option<Brief>), ApiError> {
    let calendar = state.features().enabled(squelch_core::features::CALENDAR)
        && crate::calendar_remind::calendar_reach(state).await.0;
    let ranking = state.triage_config.ranking.clone();
    let policy = state.shipment_policy;
    store_call(state, move |store, account| {
        let zone = store.owner_zone(account)?.zone;
        let today = now.with_timezone(&zone).date_naive();
        let facts = gather(store, account, now, zone, &ranking, policy, calendar)?;
        Ok((today, compose(&facts, today)))
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
/// notification extension. `{day, title, lines}`; `title` null and `lines`
/// empty on a day with nothing to say. HUMAN DOOR ONLY.
pub async fn get_brief(State(state): State<ApiState>) -> Result<impl IntoResponse, ApiError> {
    let (day, brief) = assemble(&state, Utc::now()).await?;
    let (title, lines) = match brief {
        Some(b) => (Some(b.title), b.lines),
        None => (None, Vec::new()),
    };
    Ok((
        no_store(),
        Json(serde_json::json!({
            "day": day.format("%Y-%m-%d").to_string(),
            "title": title,
            "lines": lines,
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
        assert_eq!(compose(&Facts::default(), day(2026, 11, 5)), None);
    }

    #[test]
    fn the_full_shape() {
        let facts = Facts {
            needs_you: 2,
            bills: vec![("PG&E".into(), Due::Today)],
            packages: 1,
            first_event: Some(("Dentist".into(), NaiveTime::from_hms_opt(9, 30, 0).unwrap())),
        };
        let brief = compose(&facts, day(2026, 11, 5)).unwrap();
        assert_eq!(brief.title, "Thursday, Nov 5");
        assert_eq!(
            brief.lines,
            vec![
                "2 need you · PG&E bill due today · A package arrives today".to_string(),
                "First up: Dentist at 9:30 AM".to_string(),
            ]
        );
    }

    #[test]
    fn counts_and_plurals() {
        let one = Facts {
            needs_you: 1,
            ..Default::default()
        };
        assert_eq!(
            compose(&one, day(2026, 11, 5)).unwrap().lines,
            vec!["1 needs you"]
        );
        let bills = Facts {
            bills: vec![("A".into(), Due::Today), ("B".into(), Due::Tomorrow)],
            packages: 3,
            ..Default::default()
        };
        assert_eq!(
            compose(&bills, day(2026, 11, 5)).unwrap().lines,
            vec!["2 bills due by tomorrow · 3 packages arrive today"]
        );
        let today = Facts {
            bills: vec![("A".into(), Due::Today), ("B".into(), Due::Today)],
            ..Default::default()
        };
        assert_eq!(
            compose(&today, day(2026, 11, 5)).unwrap().lines,
            vec!["2 bills due today"]
        );
        let unnamed = Facts {
            bills: vec![(" ".into(), Due::Tomorrow)],
            ..Default::default()
        };
        assert_eq!(
            compose(&unnamed, day(2026, 11, 5)).unwrap().lines,
            vec!["A bill due tomorrow"]
        );
    }

    #[test]
    fn only_an_event_is_still_a_brief() {
        let facts = Facts {
            first_event: Some((
                "  Standup\nwith   the team ".into(),
                NaiveTime::from_hms_opt(14, 0, 0).unwrap(),
            )),
            ..Default::default()
        };
        let brief = compose(&facts, day(2026, 11, 6)).unwrap();
        assert_eq!(brief.title, "Friday, Nov 6");
        assert_eq!(brief.lines, vec!["First up: Standup with the team at 2 PM"]);
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
        let line = &compose(&facts, day(2026, 11, 5)).unwrap().lines[0];
        assert_eq!(
            line,
            "First up: Quarterly planning offsite with… at 9:05 AM"
        );
        assert!(!line.contains('—') && !line.contains('–'));
    }
}
