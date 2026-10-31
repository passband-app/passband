//! Calendar reminders on the human door (docs/CALENDAR.md, "Reminders and
//! tickets"): the worker that plans and fires them, the frame that carries one
//! to the Mac, the ping that carries one to the phone, and the routes the
//! apps read.
//!
//! ONE WORKER, ITS OWN CADENCE. At start and every
//! [`CalendarRemindTiming::tick`] (30 seconds) it fires what is due; at start
//! and every `plan_every` ticks (5 minutes) it first asks the small model for
//! the plans still missing. A fire pass asks too, for an item about to fire
//! with no plan or a stale one, so it fires as planned rather than as the
//! fallback. The planner runs
//! here rather than after a calendar sync pass because committed ghosts get
//! reminders without a calendar grant, and the sync loop idles without one.
//! What a plan is and when a reminder is due is [`squelch_core::gcal::remind`]'s
//! to decide, never this module's.
//!
//! HOW IT ARRIVES. A `calendar_reminder` frame on every open Mac event stream
//! (`/client/events?opens=1`), like `renotify`: NO `id:`, so it moves no
//! cursor, and a payload key no event has, so an app that does not know it
//! carries on. And a [`CalendarPing`] to the APNs pusher, which pushes
//! `"<tag>:cal:<fire id>:<fired>"` with the relay's fixed "An event is coming
//! up"; the phone's extension fetches the frame by id. Each firing is a row
//! (`calendar_reminder_fires`, 7 days), so it fires ONCE PER START, at most
//! twice a day per item, and never under both a ghost's key and its event's
//! ([`remind::may_fire`]).
//!
//! CAPS. [`remind::DAILY_CAP`] model calls a UTC day, counted durably per
//! ATTEMPT (`calendar_reminder_calls`), so neither timeouts nor restarts buy
//! more; an item whose ask failed backs off 30 minutes, doubling to 4 hours
//! ([`remind::backoff`]).
//!
//! OFF. The owner preference (`calendar_reminders`, default on) stops all of
//! it: no plan, no model call, no fire. With the `calendar` hidden feature off
//! nothing runs either, like every route the reminders depend on.
//!
//! HUMAN DOOR ONLY. The agent door has no route here and no tool that reads a
//! plan, a firing or the preference.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use axum::{
    Json,
    extract::{Path, State},
    http::header,
    response::IntoResponse,
};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use squelch_core::gcal::remind::{self, Built, Input, RemindClient, StoredPlan};
use squelch_core::gcal::{SyncedEvent, own::Owned, place::Place};
use squelch_core::push::CalendarPing;
use squelch_core::store::Store;
use squelch_core::types::GhostEvent;
use tokio::sync::{broadcast, watch};

use crate::error::ApiError;
use crate::handlers::{blocking, no_store, store_call};
use crate::state::ApiState;

/// Frames buffered toward slow Macs. A reminder is about now: a receiver that
/// lags simply misses one (the phone still has it).
const FRAME_BUFFER: usize = 8;
/// Items a fire pass plans first at most (each a model call, in turn).
const FIRE_PLANS: usize = 4;
/// How far back and ahead a fire pass reads: a day either side covers every
/// owner zone's day around now and every lead.
const FIRE_BEHIND: chrono::Duration = chrono::Duration::days(1);
const FIRE_AHEAD: chrono::Duration = chrono::Duration::days(2);
/// What a plan pass reads: the timeline's own reach.
const PLAN_BEHIND: chrono::Duration = chrono::Duration::days(1);
const PLAN_AHEAD: chrono::Duration = chrono::Duration::days(400);

/// What every clone of the state shares about calendar reminders.
#[derive(Clone)]
pub(crate) struct CalendarRemindShared {
    /// Reminder frames, encoded, to every Mac stream listening.
    pub(crate) frames: broadcast::Sender<Arc<str>>,
    /// Reminders for the phone, to the APNs pusher.
    pub(crate) pings: broadcast::Sender<CalendarPing>,
    /// The notify model, when one is configured. None: every reminder is
    /// the fallback.
    pub(crate) client: Option<Arc<RemindClient>>,
}

impl Default for CalendarRemindShared {
    fn default() -> Self {
        Self::new(None)
    }
}

impl CalendarRemindShared {
    pub(crate) fn new(client: Option<RemindClient>) -> Self {
        Self {
            frames: broadcast::channel(FRAME_BUFFER).0,
            pings: broadcast::channel(16).0,
            client: client.map(Arc::new),
        }
    }
}

/// The worker's clock. [`CalendarRemindTiming::daemon`] in production.
#[derive(Debug, Clone, Copy)]
pub struct CalendarRemindTiming {
    /// How often due reminders are fired (and once at start).
    pub tick: Duration,
    /// Plan every this many ticks (and at start).
    pub plan_every: u32,
}

impl CalendarRemindTiming {
    pub fn daemon() -> Self {
        Self {
            tick: Duration::from_secs(30),
            plan_every: 10,
        }
    }
}

/// What a pass did, for the log and for tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemindOutcome {
    /// The `calendar` feature is off, or the owner turned reminders off.
    Off,
    /// A plan pass: how many items it asked about, and how many plans it
    /// stored.
    Planned { asked: usize, stored: usize },
    /// A fire pass: the firings it recorded, by id.
    Fired(Vec<i64>),
}

/// Everything a pass reads, in one store visit.
struct Snapshot {
    zone: squelch_core::owner_clock::OwnerZone,
    account_email: String,
    granted: bool,
    ghosts: Vec<GhostEvent>,
    events: Vec<SyncedEvent>,
    owned: Vec<Owned>,
    places: HashMap<String, Option<Place>>,
    known: HashSet<String>,
    mail: HashMap<i64, remind::LinkedMail>,
    plans: HashMap<String, StoredPlan>,
    fires: Vec<remind::Fire>,
    backed_off: HashSet<String>,
}

impl Snapshot {
    fn build(&self, now: DateTime<Utc>) -> Built {
        remind::build(
            &Input {
                ghosts: &self.ghosts,
                events: &self.events,
                owned: &self.owned,
                places: &self.places,
                mail: &self.mail,
                plans: &self.plans,
                account_email: &self.account_email,
                known: &self.known,
                granted: self.granted,
            },
            self.zone.zone,
            now,
        )
    }
}

/// Reminders run for this mailbox: the `calendar` feature is on and the
/// owner has not turned them off. A store error reads as off.
async fn on(state: &ApiState) -> bool {
    if !state.features().enabled(squelch_core::features::CALENDAR) {
        return false;
    }
    store_call(state, |store, account| {
        store.calendar_reminders_enabled(account)
    })
    .await
    .unwrap_or(false)
}

/// The calendar's grant and the mailbox's address, as the timeline reads
/// them.
pub(crate) async fn calendar_reach(state: &ApiState) -> (bool, String) {
    match state.calendar() {
        Some(calendar) => (
            calendar.granted().await,
            calendar.account_email().to_string(),
        ),
        None => (false, String::new()),
    }
}

/// The known contacts among `events`' organizers ([`remind::attends`]).
pub(crate) fn known_organizers(
    store: &squelch_core::store::SqliteStore,
    account: squelch_core::types::AccountId,
    events: &[SyncedEvent],
) -> squelch_core::Result<HashSet<String>> {
    let addrs: Vec<String> = events
        .iter()
        .filter_map(|e| e.row.organizer_email.clone())
        .collect();
    store.calendar_known_among(account, &addrs)
}

/// One snapshot over `[now - behind, now + ahead]`, listed
/// [`remind::LISTING_PAD`] wider on both sides, the planner's, the fire
/// pass's and the timeline's alike, so an item has one version whoever reads
/// it.
async fn snapshot(
    state: &ApiState,
    now: DateTime<Utc>,
    behind: chrono::Duration,
    ahead: chrono::Duration,
    with_mail: bool,
) -> Result<Snapshot, ApiError> {
    // Events only while calendar is granted, as the timeline serves them.
    let (granted, account_email) = calendar_reach(state).await;
    store_call(state, move |store, account| {
        let from = (now - behind - remind::LISTING_PAD).fixed_offset();
        let to = (now + ahead + remind::LISTING_PAD).fixed_offset();
        let zone = store.owner_zone(account)?;
        let ghosts = store.list_ghost_events(account, from, to)?;
        let (events, owned, places) = if granted {
            (
                store.list_calendar_events(account, from, to, now)?.0,
                store.calendar_owned(account)?,
                store.calendar_places(account)?,
            )
        } else {
            Default::default()
        };
        let known = known_organizers(store, account, &events)?;
        let mail = if with_mail {
            store.calendar_linked_mail(account, &remind::mail_ids(&ghosts, false))?
        } else {
            HashMap::new()
        };
        Ok(Snapshot {
            zone,
            account_email,
            granted,
            ghosts,
            events,
            owned,
            places,
            known,
            mail,
            plans: store.calendar_reminder_plans(account)?,
            fires: store.calendar_reminder_fired(account)?,
            backed_off: store.calendar_reminder_backed_off(account, now)?,
        })
    })
    .await
}

/// Ask the model about `item` within the durable daily cap, and write down
/// what came of it: the usage, and the plan or the backoff. The plan, when
/// one was stored; None when the cap is spent or the ask failed.
async fn ask(
    state: &ApiState,
    client: &RemindClient,
    item: &remind::Item,
    zone: &squelch_core::owner_clock::OwnerZone,
    now: DateTime<Utc>,
) -> Result<Option<StoredPlan>, ApiError> {
    let day = now.format("%Y-%m-%d").to_string();
    if !take_call(state, &day).await? {
        return Ok(None);
    }
    let result = remind::plan_item(client, item, zone.zone, now).await;
    write_answer(state, item, result, &day, now).await
}

/// Take one of `day`'s calls. False when the cap is spent.
async fn take_call(state: &ApiState, day: &str) -> Result<bool, ApiError> {
    let day = day.to_string();
    store_call(state, move |store, account| {
        store.calendar_reminder_take_call(account, &day, remind::DAILY_CAP)
    })
    .await
}

async fn write_answer(
    state: &ApiState,
    item: &remind::Item,
    result: remind::RemindResult,
    day: &str,
    now: DateTime<Utc>,
) -> Result<Option<StoredPlan>, ApiError> {
    let (key, version, day) = (item.key.clone(), item.version.clone(), day.to_string());
    store_call(state, move |store, account| {
        if let Some(u) = result.usage {
            store.extract_bump_usage(account, &day, remind::USAGE_CATEGORY, u.into())?;
        }
        match result.plan {
            Some(plan) => {
                store.calendar_reminder_put(account, &key, &version, &plan, now)?;
                Ok(Some(StoredPlan {
                    version,
                    prompt_version: remind::PROMPT_VERSION.to_string(),
                    plan,
                }))
            }
            None => {
                store.calendar_reminder_failed(account, &key, now)?;
                Ok(None)
            }
        }
    })
    .await
}

/// The timeline's TICKETS buttons over `[from, to]`, by event key and by
/// ghost id: built as the reminder passes build ([`remind::LISTING_PAD`]
/// wider, the same attendance rule and plans), so a plan current for the
/// planner is current here. The display match (`matched_event`) is the
/// timeline's own and is not this.
pub(crate) struct TimelineTickets {
    pub(crate) events: HashMap<String, serde_json::Value>,
    pub(crate) ghosts: HashMap<i64, serde_json::Value>,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn timeline_tickets(
    store: &squelch_core::store::SqliteStore,
    account: squelch_core::types::AccountId,
    from: chrono::DateTime<chrono::FixedOffset>,
    to: chrono::DateTime<chrono::FixedOffset>,
    now: DateTime<Utc>,
    granted: bool,
    account_email: &str,
) -> squelch_core::Result<TimelineTickets> {
    let (from, to) = (from - remind::LISTING_PAD, to + remind::LISTING_PAD);
    let zone = store.owner_zone(account)?.zone;
    let ghosts = store.list_ghost_events(account, from, to)?;
    let (events, owned, places) = if granted {
        (
            store.list_calendar_events(account, from, to, now)?.0,
            store.calendar_owned(account)?,
            store.calendar_places(account)?,
        )
    } else {
        Default::default()
    };
    let known = known_organizers(store, account, &events)?;
    let plans = store.calendar_reminder_plans(account)?;
    // Only the mail of the kinds whose mail holds tickets: all the fallback
    // reads, and a plan's pick is held to source ids, not to mail read here.
    let mail = store.calendar_linked_mail(account, &remind::mail_ids(&ghosts, true))?;
    let built = remind::build(
        &Input {
            ghosts: &ghosts,
            events: &events,
            owned: &owned,
            places: &places,
            mail: &mail,
            plans: &plans,
            account_email,
            known: &known,
            granted,
        },
        zone,
        now,
    );
    Ok(TimelineTickets {
        events: built
            .events
            .iter()
            .map(|item| {
                let pick = remind::tickets(item, plans.get(&item.key));
                (item.key.clone(), remind::tickets_json(pick.as_ref()))
            })
            .collect(),
        ghosts: ghosts
            .iter()
            .enumerate()
            .map(|(i, g)| {
                let pick = built.ghost_tickets(i, &plans);
                (g.id, remind::tickets_json(pick.as_ref()))
            })
            .collect(),
    })
}

/// ONE PLAN PASS: ask the model about up to [`remind::PER_PASS`] items still
/// wanting a plan for their current version and not backed off, nearest
/// first, within the daily cap; then forget the plans of items gone or
/// [`remind::over`], and firings past their week. A failed call stores no
/// plan and backs the item off.
pub async fn run_plan_pass(
    state: &ApiState,
    now: DateTime<Utc>,
) -> Result<RemindOutcome, ApiError> {
    if !on(state).await {
        return Ok(RemindOutcome::Off);
    }
    // Read without the linked mail first: which items want a plan does not
    // depend on it, and only theirs is read below.
    let mut snap = snapshot(state, now, PLAN_BEHIND, PLAN_AHEAD, false).await?;
    let built = snap.build(now);
    let keep = built.keep(now, snap.zone.zone);
    let mut asked = 0;
    let mut stored = 0;
    if let Some(client) = state.calendar_remind.client.clone() {
        let day = now.format("%Y-%m-%d").to_string();
        let mut wanting: Vec<&remind::Item> = built
            .items()
            .filter(|i| remind::wants_plan(i, now))
            .filter(|i| !snap.plans.get(&i.key).is_some_and(|p| p.current(i)))
            .filter(|i| !snap.backed_off.contains(&i.key))
            .collect();
        wanting.sort_by_key(|i| (remind::start_order(i), i.key.clone()));
        let mut keys: HashSet<String> = HashSet::new();
        for item in wanting.into_iter().take(remind::PER_PASS) {
            if !take_call(state, &day).await? {
                break;
            }
            keys.insert(item.key.clone());
        }
        // The mail of the picked items: their linked ghosts' and their
        // candidates'.
        let mut ghost_ids: HashSet<i64> = HashSet::new();
        let mut ids: Vec<i64> = Vec::new();
        for item in built.items().filter(|i| keys.contains(&i.key)) {
            ids.extend(&item.linked_ids);
            ghost_ids.extend(item.candidates.iter().map(|c| c.ghost_id));
        }
        ids.extend(remind::mail_ids(
            &snap
                .ghosts
                .iter()
                .filter(|g| ghost_ids.contains(&g.id))
                .cloned()
                .collect::<Vec<_>>(),
            false,
        ));
        if !keys.is_empty() {
            snap.mail = store_call(state, move |store, account| {
                store.calendar_linked_mail(account, &ids)
            })
            .await?;
        }
        let built = snap.build(now);
        let mut picked: Vec<remind::Item> = built
            .items()
            .filter(|i| keys.contains(&i.key))
            .cloned()
            .collect();
        picked.sort_by_key(|i| (remind::start_order(i), i.key.clone()));
        asked = picked.len();
        // Side by side, so one slow answer does not hold up the rest by its
        // whole timeout times the batch.
        let zone = snap.zone.zone;
        let answers = futures::future::join_all(picked.into_iter().map(|item| {
            let client = client.clone();
            async move {
                let result = remind::plan_item(&client, &item, zone, now).await;
                (item, result)
            }
        }))
        .await;
        for (item, result) in answers {
            match write_answer(state, &item, result, &day, now).await {
                Ok(Some(_)) => stored += 1,
                Ok(None) => {}
                Err(_) => eprintln!("squelchd: a calendar reminder plan could not be stored"),
            }
        }
    }
    // A pass that could not list the calendar knows nothing of the events,
    // so it keeps their plans.
    let events = snap.granted;
    let pruned = store_call(state, move |store, account| {
        store.calendar_reminders_prune(account, &keep, events)?;
        store.calendar_reminder_fires_prune(account, now)
    })
    .await;
    if pruned.is_err() {
        eprintln!("squelchd: calendar reminders could not be pruned");
    }
    Ok(RemindOutcome::Planned { asked, stored })
}

/// ONE FIRE PASS: every remindable item that [`remind::due`] says is due and
/// [`remind::may_fire`] allows is recorded, framed to the Mac and pinged to
/// the phone. An item due with no plan or a stale one is asked about first
/// (at most [`FIRE_PLANS`] a pass, within the caps), so a new event or one
/// whose version just moved fires as planned rather than as the fallback.
pub async fn run_fire_pass(
    state: &ApiState,
    now: DateTime<Utc>,
) -> Result<RemindOutcome, ApiError> {
    if !on(state).await {
        return Ok(RemindOutcome::Off);
    }
    let mut snap = snapshot(state, now, FIRE_BEHIND, FIRE_AHEAD, true).await?;
    let zone = snap.zone.zone;
    let built = snap.build(now);
    let mut fired = Vec::new();
    let mut planned = 0;
    for item in built.items() {
        let mut plan = snap.plans.get(&item.key).cloned();
        let Some(mut due) = remind::due(item, plan.as_ref(), now, zone) else {
            continue;
        };
        if !remind::may_fire(item, &due, &snap.fires, now, zone) {
            continue;
        }
        if let Some(client) = state.calendar_remind.client.as_deref()
            && !plan.as_ref().is_some_and(|p| p.current(item))
            && !snap.backed_off.contains(&item.key)
            && planned < FIRE_PLANS
        {
            planned += 1;
            if let Some(fresh) = ask(state, client, item, &snap.zone, now).await? {
                snap.plans.insert(item.key.clone(), fresh.clone());
                plan = Some(fresh);
                // Planned now: due by what the plan says, or not yet.
                match remind::due(item, plan.as_ref(), now, zone) {
                    Some(d) => due = d,
                    None => continue,
                }
            } else {
                snap.backed_off.insert(item.key.clone());
            }
        }
        let (key, start_key, covers) =
            (item.key.clone(), due.start_key.clone(), item.covers.clone());
        let (item_c, plan_c, due_c) = (item.clone(), plan.clone(), due.clone());
        let covers_c = covers.clone();
        let recorded = store_call(state, move |store, account| {
            store.calendar_reminder_fire(account, &key, &start_key, &covers_c, now, |id| {
                remind::frame(id, now, &item_c, &due_c, plan_c.as_ref(), zone).to_string()
            })
        })
        .await;
        match recorded {
            Ok(Some((id, frame))) => {
                snap.fires.push(remind::Fire {
                    item_key: item.key.clone(),
                    start_key: due.start_key.clone(),
                    fired_at: now,
                    covers,
                });
                // Nobody listening is not a failure: the Mac and the phone
                // are each best effort, and the row is what the phone reads.
                let _ = state.calendar_remind.frames.send(Arc::from(frame));
                let _ = state.calendar_remind.pings.send(CalendarPing {
                    fire_id: id,
                    fired: now.timestamp(),
                });
                fired.push(id);
            }
            Ok(None) => {}
            Err(_) => eprintln!("squelchd: a calendar reminder could not be recorded"),
        }
    }
    Ok(RemindOutcome::Fired(fired))
}

fn log(outcome: &Result<RemindOutcome, ApiError>) {
    // Counts only: never a title, a place or a line.
    match outcome {
        Ok(RemindOutcome::Planned { asked, stored }) if *asked > 0 => {
            eprintln!("squelchd: calendar reminders planned {stored} of {asked}")
        }
        Ok(RemindOutcome::Fired(ids)) if !ids.is_empty() => {
            eprintln!("squelchd: calendar reminders fired {}", ids.len())
        }
        Ok(_) => {}
        Err(_) => eprintln!("squelchd: a calendar reminder pass failed"),
    }
}

/// THE WORKER: at start and then every tick, a fire pass, with a plan pass
/// first at start and on every `plan_every`-th tick. Each pass is raced
/// against shutdown.
pub async fn calendar_remind_worker(
    state: ApiState,
    mut shutdown: watch::Receiver<bool>,
    timing: CalendarRemindTiming,
) {
    let mut n: u32 = 0;
    loop {
        if *shutdown.borrow() {
            return;
        }
        if n.is_multiple_of(timing.plan_every.max(1)) {
            tokio::select! {
                outcome = run_plan_pass(&state, Utc::now()) => log(&outcome),
                _ = shutdown.changed() => return,
            }
        }
        n = n.wrapping_add(1);
        tokio::select! {
            outcome = run_fire_pass(&state, Utc::now()) => log(&outcome),
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

/// `GET /client/calendar/reminders/{fire_id}`: one firing's
/// `calendar_reminder` frame, as the Mac received it, for the phone's
/// notification extension. 404 for an unknown id, another mailbox's, or one
/// older than a week, alike. HUMAN DOOR ONLY.
pub async fn get_reminder(
    State(state): State<ApiState>,
    Path(id): Path<i64>,
) -> Result<impl IntoResponse, ApiError> {
    let now = Utc::now();
    let frame = store_call(&state, move |store, account| {
        store.calendar_reminder_frame(account, id, now)
    })
    .await?
    .ok_or_else(ApiError::not_found)?;
    Ok((
        no_store(),
        [(header::CONTENT_TYPE, "application/json")],
        frame,
    ))
}

#[derive(Debug, Deserialize)]
pub struct ReminderSettings {
    enabled: bool,
}

/// `GET /client/calendar/reminders/settings`: `{enabled}`, the owner
/// preference (on unless turned off).
pub async fn get_settings(State(state): State<ApiState>) -> Result<impl IntoResponse, ApiError> {
    let enabled = store_call(&state, |store, account| {
        store.calendar_reminders_enabled(account)
    })
    .await?;
    Ok((no_store(), Json(serde_json::json!({ "enabled": enabled }))))
}

/// `PUT /client/calendar/reminders/settings` `{enabled: bool}`: set it, and
/// answer it as stored. Off stops every plan, model call and firing at the
/// next pass; tickets on the timeline are served either way.
pub async fn put_settings(
    State(state): State<ApiState>,
    Json(body): Json<ReminderSettings>,
) -> Result<impl IntoResponse, ApiError> {
    let enabled = blocking(&state, {
        let store = state.store.clone();
        let account = state.account_id;
        move || {
            store.set_calendar_reminders_enabled(account, body.enabled)?;
            store.calendar_reminders_enabled(account)
        }
    })
    .await?;
    Ok((no_store(), Json(serde_json::json!({ "enabled": enabled }))))
}
