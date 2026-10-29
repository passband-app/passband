//! Calendar reminders on the human door (docs/CALENDAR.md, "Reminders and
//! tickets"): the worker that plans and fires them, the frame that carries one
//! to the Mac, the ping that carries one to the phone, and the routes the
//! apps read.
//!
//! ONE WORKER, ITS OWN CADENCE. Every [`CalendarRemindTiming::tick`] (30
//! seconds) it fires what is due; every `plan_every` ticks (5 minutes) it
//! first asks the small model for the plans still missing. The planner runs
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
//! (`calendar_reminder_fires`, 7 days), so it fires ONCE PER START.
//!
//! OFF. The owner preference (`calendar_reminders`, default on) stops all of
//! it: no plan, no model call, no fire. With the `calendar` hidden feature off
//! nothing runs either, like every route the reminders depend on.
//!
//! HUMAN DOOR ONLY. The agent door has no route here and no tool that reads a
//! plan, a firing or the preference.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
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
    /// Model calls ATTEMPTED today, `(day, count)`. The ledger only learns
    /// of calls the provider reported usage for, so a run of timeouts is
    /// counted here; the higher of the two is what has been spent.
    calls: Arc<Mutex<(String, u32)>>,
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
            calls: Arc::new(Mutex::new((String::new(), 0))),
        }
    }

    /// Take one of today's [`remind::DAILY_CAP`] calls, counting the
    /// ledger's `spent` as already taken. False when none is left.
    fn reserve_call(&self, day: &str, spent: u64) -> bool {
        let Ok(mut calls) = self.calls.lock() else {
            return false;
        };
        if calls.0 != day {
            *calls = (day.to_string(), 0);
        }
        if u64::from(calls.1).max(spent) >= u64::from(remind::DAILY_CAP) {
            return false;
        }
        calls.1 += 1;
        true
    }
}

/// The worker's clock. [`CalendarRemindTiming::daemon`] in production.
#[derive(Debug, Clone, Copy)]
pub struct CalendarRemindTiming {
    /// How often due reminders are fired.
    pub tick: Duration,
    /// Plan every this many ticks (and on the first).
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
    ghosts: Vec<GhostEvent>,
    events: Vec<SyncedEvent>,
    owned: Vec<Owned>,
    places: HashMap<String, Option<Place>>,
    mail: HashMap<i64, remind::LinkedMail>,
    plans: HashMap<String, StoredPlan>,
    fired: HashSet<(String, String)>,
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

async fn snapshot(
    state: &ApiState,
    now: DateTime<Utc>,
    behind: chrono::Duration,
    ahead: chrono::Duration,
    with_mail: bool,
) -> Result<Snapshot, ApiError> {
    // Events only while calendar is granted, as the timeline serves them.
    let granted = match state.calendar() {
        Some(calendar) => calendar.granted().await,
        None => false,
    };
    store_call(state, move |store, account| {
        let from = (now - behind).fixed_offset();
        let to = (now + ahead).fixed_offset();
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
        let mail = if with_mail {
            store.calendar_linked_mail(account, &remind::mail_ids(&ghosts, false))?
        } else {
            HashMap::new()
        };
        Ok(Snapshot {
            zone,
            ghosts,
            events,
            owned,
            places,
            mail,
            plans: store.calendar_reminder_plans(account)?,
            fired: store.calendar_reminder_fired(account)?,
        })
    })
    .await
}

/// ONE PLAN PASS: ask the model about up to [`remind::PER_PASS`] items still
/// wanting a plan for their current version, nearest first, within the daily
/// cap; then forget the plans of items gone or over, and firings past their
/// week. A failed call stores nothing.
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
    let keep: HashSet<String> = built
        .items()
        .filter(|i| i.remindable)
        .map(|i| i.key.clone())
        .collect();
    let mut asked = 0;
    let mut stored = 0;
    if let Some(client) = state.calendar_remind.client.clone() {
        let day = now.format("%Y-%m-%d").to_string();
        let ledger_day = day.clone();
        let spent = store_call(state, move |store, account| {
            Ok(store
                .list_usage_by_category(account, 1)?
                .into_iter()
                .filter(|(c, _)| c == remind::USAGE_CATEGORY)
                .flat_map(|(_, days)| days)
                .filter(|d| d.day == ledger_day)
                .map(|d| d.calls)
                .sum::<u64>())
        })
        .await?;
        let mut wanting: Vec<&remind::Item> = built
            .items()
            .filter(|i| remind::wants_plan(i, now))
            .filter(|i| !snap.plans.get(&i.key).is_some_and(|p| p.current(i)))
            .collect();
        wanting.sort_by_key(|i| (remind::start_order(i), i.key.clone()));
        let keys: HashSet<String> = wanting
            .into_iter()
            .take(remind::PER_PASS)
            .take_while(|_| state.calendar_remind.reserve_call(&day, spent))
            .map(|i| i.key.clone())
            .collect();
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
            let day = day.clone();
            let has_plan = result.plan.is_some();
            let wrote = store_call(state, move |store, account| {
                if let Some(u) = result.usage {
                    store.extract_bump_usage(account, &day, remind::USAGE_CATEGORY, u.into())?;
                }
                if let Some(plan) = &result.plan {
                    store.calendar_reminder_put(account, &item.key, &item.version, plan, now)?;
                }
                Ok(())
            })
            .await;
            match wrote {
                Ok(()) if has_plan => stored += 1,
                Ok(()) => {}
                Err(_) => eprintln!("squelchd: a calendar reminder plan could not be stored"),
            }
        }
    }
    let pruned = store_call(state, move |store, account| {
        store.calendar_reminders_prune(account, &keep)?;
        store.calendar_reminder_fires_prune(account, now)
    })
    .await;
    if pruned.is_err() {
        eprintln!("squelchd: calendar reminders could not be pruned");
    }
    Ok(RemindOutcome::Planned { asked, stored })
}

/// ONE FIRE PASS: every remindable item that [`remind::due`] says is due and
/// has not fired for its start is recorded, framed to the Mac and pinged to
/// the phone.
pub async fn run_fire_pass(
    state: &ApiState,
    now: DateTime<Utc>,
) -> Result<RemindOutcome, ApiError> {
    if !on(state).await {
        return Ok(RemindOutcome::Off);
    }
    let snap = snapshot(state, now, FIRE_BEHIND, FIRE_AHEAD, true).await?;
    let built = snap.build(now);
    let mut fired = Vec::new();
    for item in built.items() {
        let plan = snap
            .plans
            .get(&item.key)
            .filter(|p| p.current(item))
            .map(|p| &p.plan);
        let Some(due) = remind::due(item, plan, now, snap.zone.zone) else {
            continue;
        };
        if snap
            .fired
            .contains(&(item.key.clone(), due.start_key.clone()))
        {
            continue;
        }
        let (key, start_key) = (item.key.clone(), due.start_key.clone());
        let (item_c, plan_c, zone) = (item.clone(), plan.cloned(), snap.zone.zone);
        let recorded = store_call(state, move |store, account| {
            store.calendar_reminder_fire(account, &key, &start_key, now, |id| {
                remind::frame(id, now, &item_c, &due, plan_c.as_ref(), zone).to_string()
            })
        })
        .await;
        match recorded {
            Ok(Some((id, frame))) => {
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

/// THE WORKER: a fire pass every tick, a plan pass first on every
/// `plan_every`-th (and the first). A plan pass is raced against shutdown.
pub async fn calendar_remind_worker(
    state: ApiState,
    mut shutdown: watch::Receiver<bool>,
    timing: CalendarRemindTiming,
) {
    let mut n: u32 = 0;
    loop {
        tokio::select! {
            _ = tokio::time::sleep(timing.tick) => {}
            res = shutdown.changed() => {
                if res.is_err() || *shutdown.borrow() {
                    return;
                }
                continue;
            }
        }
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
        log(&run_fire_pass(&state, Utc::now()).await);
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
