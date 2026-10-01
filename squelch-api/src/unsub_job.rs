//! The AUTOMATIC unsubscribe: what happens after the user presses `u` on a
//! client that asked for it (`POST /client/unsubscribe` with `auto: true`).
//!
//! NOTHING HERE RUNS BEFORE THE USER ASKS. An unsubscribe link may unsubscribe
//! on a plain GET, so it is never prefetched, previewed or probed; the first
//! request to it is the unsubscribe.
//!
//! The ladder, cheapest and most certain first:
//!
//!   1. RFC 8058 one-click. The header advertised it, so one POST of
//!      `List-Unsubscribe=One-Click` IS the unsubscribe, and a 2xx is done. No
//!      page, no reading. This is most bulk mail (Gmail and Yahoo require it of
//!      bulk senders).
//!   2. The page. GET the link and read what came back:
//!        - it says the user is already out (a survey after the fact
//!          included): done, and the survey stays unanswered;
//!        - it is one click from done (a lone confirm button, a "yes,
//!          unsubscribe me" link): take that one action, then require the
//!          page that comes back to say done;
//!        - it is a preference center, an app shell, a bot wall, an error, or
//!          anything we cannot read with confidence: hand the ORIGINAL link to
//!          the user (`needs_you`).
//!
//! The original link and not the last redirect hop, because preference centers
//! often set a session cookie on the first hop that this job's jar holds and the
//! user's browser does not; the original replays the whole chain for them, and a
//! page that was NOT a one-GET unsubscribe (that is why it reached the user) is
//! safe to GET again.
//!
//! A job ends in exactly one ledger write ([`Store::finish_unsubscribe_job`]):
//! `done` or `needs_you`. A daemon that dies mid-job leaves `running`, which the
//! next listing reaps into `needs_you` (see [`UnsubJobs::reap`]).
//!
//! [`Store::finish_unsubscribe_job`]: squelch_core::store::Store::finish_unsubscribe_job

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::Utc;
use squelch_core::store::Store;
use squelch_core::triage::unsub_page::{self, PageDigest, PageVerdict, UnsubPageClient};

use crate::handlers::{audit_action, store_call};
use crate::state::ApiState;
use crate::unsub_fetch::{FetchError, Fetched, Fetcher};
use crate::unsub_html::{self, Page};

/// Jobs in flight at once, per daemon. Each is a handful of requests to a
/// stranger's site; this only bounds what a burst of `u` presses can open.
const CONCURRENCY: usize = 4;
/// The whole job, every hop and the model call included. Past it the user gets
/// the link: a tray that spins for a minute is worse than one more click.
const JOB_BUDGET: Duration = Duration::from_secs(45);

/// What the job starts from.
#[derive(Debug, Clone)]
pub(crate) enum Plan {
    /// RFC 8058 advertised, with an https URL.
    OneClick(String),
    /// A link to fetch and read: the header's URL without one-click, or the
    /// footer link.
    Page(String),
}

impl Plan {
    pub fn url(&self) -> &str {
        match self {
            Plan::OneClick(u) | Plan::Page(u) => u,
        }
    }
}

/// Why a job needs the user. These strings are wire values the client maps to
/// copy; add one there before adding one here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NeedsYou {
    /// A preference center: the user has to choose what to stop.
    ChooseTopics,
    /// The page wants a real browser: scripts, a captcha, a bot wall.
    NeedsBrowser,
    /// The link is dead or refused (4xx, unreachable, not a page).
    LinkError,
    /// We could not tell, or the one action we took did not say done.
    Unclear,
}

impl NeedsYou {
    pub fn as_str(self) -> &'static str {
        match self {
            NeedsYou::ChooseTopics => "choose_topics",
            NeedsYou::NeedsBrowser => "needs_browser",
            NeedsYou::LinkError => "link_error",
            NeedsYou::Unclear => "unclear",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// Done, and how: `one_click` or `page`.
    Done(&'static str),
    NeedsYou(NeedsYou),
}

/// The human door's handle on running jobs. Cheap to clone.
#[derive(Clone)]
pub(crate) struct UnsubJobs {
    /// Senders with a job in THIS process. A `running` row whose sender is not
    /// here was orphaned by a restart.
    running: Arc<Mutex<HashSet<String>>>,
    slots: Arc<tokio::sync::Semaphore>,
    /// TEST HOOK: lets the fetcher reach loopback. Never set in production.
    pub(crate) allow_local: bool,
    pub(crate) page_reader: Option<Arc<UnsubPageClient>>,
}

impl Default for UnsubJobs {
    fn default() -> Self {
        Self {
            running: Arc::new(Mutex::new(HashSet::new())),
            slots: Arc::new(tokio::sync::Semaphore::new(CONCURRENCY)),
            allow_local: false,
            page_reader: None,
        }
    }
}

impl UnsubJobs {
    /// Claim `sender`. `false` when a job for them is already running, in which
    /// case the caller reports that one instead of starting a second.
    fn claim(&self, sender: &str) -> bool {
        self.running
            .lock()
            .map(|mut set| set.insert(sender.to_string()))
            .unwrap_or(false)
    }

    fn release(&self, sender: &str) {
        if let Ok(mut set) = self.running.lock() {
            set.remove(sender);
        }
    }

    pub fn live(&self) -> Vec<String> {
        self.running
            .lock()
            .map(|set| set.iter().cloned().collect())
            .unwrap_or_default()
    }
}

/// Start a job for `sender`, or join the one already running. Writes the
/// `running` row BEFORE spawning, so a listing right after this returns already
/// shows it.
pub(crate) async fn start(
    state: &ApiState,
    sender: String,
    message_id: i64,
    plan: Plan,
) -> Result<(), crate::error::ApiError> {
    if !state.unsub.claim(&sender) {
        return Ok(());
    }
    let (s, url) = (sender.clone(), plan.url().to_string());
    if let Err(e) = store_call(state, move |store, account_id| {
        store.begin_unsubscribe_job(account_id, &s, Some(message_id), &url, Utc::now())
    })
    .await
    {
        state.unsub.release(&sender);
        return Err(e);
    }
    let state = state.clone();
    tokio::spawn(async move {
        let outcome = {
            let _slot = state.unsub.slots.clone().acquire_owned().await;
            match tokio::time::timeout(JOB_BUDGET, run(&state, &plan)).await {
                Ok(outcome) => outcome,
                Err(_) => Outcome::NeedsYou(NeedsYou::Unclear),
            }
        };
        settle(&state, &sender, outcome).await;
        state.unsub.release(&sender);
    });
    Ok(())
}

async fn settle(state: &ApiState, sender: &str, outcome: Outcome) {
    let (status, method, reason) = match outcome {
        Outcome::Done(how) => ("done", how, None),
        Outcome::NeedsYou(why) => ("needs_you", "auto", Some(why.as_str())),
    };
    let s = sender.to_string();
    let _ = store_call(state, move |store, account_id| {
        store.finish_unsubscribe_job(account_id, &s, status, method, reason, Utc::now())
    })
    .await;
    // Sender and verdict only, never the URL: it carries the recipient's token.
    let detail = match reason {
        Some(r) => format!("{status}:{r}:{sender}"),
        None => format!("{status}:{method}:{sender}"),
    };
    audit_action(
        state,
        "unsubscribe_result",
        Some(sender.to_string()),
        &detail,
    )
    .await;
}

/// The ladder. Infallible: every failure is a [`NeedsYou`].
pub(crate) async fn run(state: &ApiState, plan: &Plan) -> Outcome {
    let account_email = store_call(state, |store, account_id| store.account_email(account_id))
        .await
        .unwrap_or_default();
    let Ok(fetcher) = Fetcher::new(state.unsub.allow_local) else {
        return Outcome::NeedsYou(NeedsYou::Unclear);
    };

    let url = match plan {
        Plan::OneClick(url) => {
            match fetcher.one_click(url).await {
                Ok(status) if status.is_success() => return Outcome::Done("one_click"),
                // Refused by the guard: the page path would refuse it too.
                Err(FetchError::Refused(_)) => return Outcome::NeedsYou(NeedsYou::LinkError),
                // A one-click endpoint that answers anything else (a redirect
                // to a landing page, a 405, a timeout) is read as a page: the
                // browser button did exactly that GET before this existed.
                _ => url,
            }
        }
        Plan::Page(url) => url,
    };

    let fetched = match fetcher.get(url).await {
        Ok(f) => f,
        Err(e) => return Outcome::NeedsYou(fetch_failure(&e)),
    };
    let Some(mut page) = read(&fetched) else {
        return Outcome::NeedsYou(http_failure(&fetched));
    };
    let mut page_url = fetched.final_url.clone();

    // A script-free redirect, once. Some senders land on a "processing..." page
    // that refreshes into the real one.
    if !page.says_done()
        && page.actions.is_empty()
        && let Some(next) = page.meta_refresh.clone()
        && let Ok(f) = fetcher.get(next.as_str()).await
        && let Some(p) = read(&f)
    {
        page = p;
        page_url = f.final_url.clone();
    }

    if page.says_done() {
        return Outcome::Done("page");
    }
    if page.needs_browser() {
        return Outcome::NeedsYou(NeedsYou::NeedsBrowser);
    }

    let verdict = verdict(state, &page_url, &page).await;
    let index = match verdict {
        PageVerdict::Unsubscribed => return Outcome::Done("page"),
        PageVerdict::Preferences => return Outcome::NeedsYou(NeedsYou::ChooseTopics),
        PageVerdict::Unclear => return Outcome::NeedsYou(NeedsYou::Unclear),
        PageVerdict::Confirm { form: Some(i) } => i,
        // "Confirm" with no form named: only a page with exactly one obvious
        // action leaves no doubt about which one.
        PageVerdict::Confirm { form: None } => match page.obvious_action() {
            Some(i) => i,
            None => return Outcome::NeedsYou(NeedsYou::Unclear),
        },
    };
    let Some(submission) = page
        .actions
        .get(index)
        .and_then(|a| a.submission(&account_email, &page_url))
    else {
        return Outcome::NeedsYou(NeedsYou::Unclear);
    };

    let after = match fetcher
        .submit(submission.method, submission.url, &submission.pairs)
        .await
    {
        Ok(f) => f,
        Err(e) => return Outcome::NeedsYou(fetch_failure(&e)),
    };
    // THE ACTION HAS TO PROVE ITSELF. A submit that lands on a page not saying
    // done is not done, whatever the first page promised.
    let Some(result) = read(&after) else {
        return Outcome::NeedsYou(http_failure(&after));
    };
    if result.says_done() {
        return Outcome::Done("page");
    }
    match verdict_after_submit(state, &after.final_url, &result).await {
        PageVerdict::Unsubscribed => Outcome::Done("page"),
        PageVerdict::Preferences => Outcome::NeedsYou(NeedsYou::ChooseTopics),
        _ => Outcome::NeedsYou(NeedsYou::Unclear),
    }
}

/// A 2xx HTML page, parsed; `None` for anything else.
fn read(f: &Fetched) -> Option<Page> {
    (f.status.is_success() && f.is_html()).then(|| unsub_html::parse_page(&f.body, &f.final_url))
}

fn fetch_failure(e: &FetchError) -> NeedsYou {
    match e {
        // Reachable only through a browser (a site that refuses this client's
        // network path, or answers too slowly) reads as a browser job.
        FetchError::Transport(_) => NeedsYou::NeedsBrowser,
        FetchError::Refused(_) | FetchError::TooManyRedirects => NeedsYou::LinkError,
    }
}

fn http_failure(f: &Fetched) -> NeedsYou {
    match f.status.as_u16() {
        // Bot walls and rate limits: a person in a browser usually gets through.
        401 | 403 | 429 | 503 => NeedsYou::NeedsBrowser,
        s if s >= 400 => NeedsYou::LinkError,
        // A 2xx that is not HTML (a PDF, JSON, an image): nothing to read.
        _ => NeedsYou::Unclear,
    }
}

/// What the page asks for: the model when one is configured and under its
/// daily cap, else the no-model rule.
async fn verdict(state: &ApiState, page_url: &url::Url, page: &Page) -> PageVerdict {
    if let Some(v) = ask_model(state, page_url, page).await {
        return v;
    }
    match page.obvious_action() {
        Some(i) => PageVerdict::Confirm { form: Some(i) },
        None => PageVerdict::Unclear,
    }
}

/// After the one action: only the model can upgrade a page that did not say
/// done in so many words. Without one, it stays the user's.
async fn verdict_after_submit(state: &ApiState, page_url: &url::Url, page: &Page) -> PageVerdict {
    ask_model(state, page_url, page)
        .await
        .unwrap_or(PageVerdict::Unclear)
}

async fn ask_model(state: &ApiState, page_url: &url::Url, page: &Page) -> Option<PageVerdict> {
    let client = state.unsub.page_reader.clone()?;
    let under_cap = store_call(state, |store, account_id| {
        let today = Utc::now().format("%Y-%m-%d").to_string();
        let spent: u64 = store
            .list_usage_by_category(account_id, 1)?
            .into_iter()
            .filter(|(c, _)| c == unsub_page::USAGE_CATEGORY)
            .flat_map(|(_, days)| days)
            .filter(|d| d.day == today)
            .map(|d| d.calls)
            .sum();
        Ok(spent < u64::from(unsub_page::DAILY_CAP))
    })
    .await
    .unwrap_or(false);
    if !under_cap {
        return None;
    }
    let forms = page.describe_actions();
    let host = page_url.host_str().unwrap_or("");
    let (verdict, usage) = unsub_page::classify_page(
        &client,
        &PageDigest {
            host,
            title: &page.title,
            text: &page.text,
            forms: &forms,
        },
    )
    .await;
    if let Some(u) = usage {
        let _ = store_call(state, move |store, account_id| {
            let day = Utc::now().format("%Y-%m-%d").to_string();
            if let Err(e) =
                store.extract_bump_usage(account_id, &day, unsub_page::USAGE_CATEGORY, u.into())
            {
                eprintln!("squelch: unsubscribe page usage ledger bump failed ({e})");
            }
            Ok(())
        })
        .await;
    }
    Some(verdict)
}

/// Reap rows orphaned by a restart. Called from the listing, so the first look
/// after a restart already tells the truth.
pub(crate) async fn reap(state: &ApiState) {
    let live = state.unsub.live();
    let _ = store_call(state, move |store, account_id| {
        store.reap_unsubscribe_jobs(account_id, &live, Utc::now())
    })
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;
    use axum::routing::post;
    use squelch_core::store::SqliteStore;

    async fn site(status: StatusCode) -> (String, Arc<Mutex<Vec<String>>>) {
        let hits = Arc::new(Mutex::new(Vec::new()));
        let h = hits.clone();
        let app = axum::Router::new().route(
            "/oc",
            post(move |body: String| {
                let h = h.clone();
                async move {
                    h.lock().unwrap().push(format!("POST {body}"));
                    status
                }
            })
            .get(|| async { axum::response::Html("<p>You have been unsubscribed.</p>") }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}/oc"), hits)
    }

    fn state() -> ApiState {
        let store = Arc::new(SqliteStore::open_in_memory().unwrap());
        let acct = store.ensure_account("me@example.com").unwrap();
        ApiState::new(store, acct, "t").with_unsub_fetch_allow_local(true)
    }

    #[tokio::test]
    async fn one_click_is_one_post_of_the_rfc_body_and_done() {
        let (url, hits) = site(StatusCode::OK).await;
        let out = run(&state(), &Plan::OneClick(url)).await;
        assert_eq!(out, Outcome::Done("one_click"));
        assert_eq!(
            hits.lock().unwrap().clone(),
            vec!["POST List-Unsubscribe=One-Click"],
            "exactly one request, and no page read"
        );
    }

    #[tokio::test]
    async fn a_refused_one_click_falls_back_to_reading_the_page() {
        let (url, hits) = site(StatusCode::METHOD_NOT_ALLOWED).await;
        let out = run(&state(), &Plan::OneClick(url)).await;
        assert_eq!(out, Outcome::Done("page"));
        assert_eq!(hits.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn without_the_test_hook_loopback_is_refused_before_any_request() {
        let (url, hits) = site(StatusCode::OK).await;
        let store = Arc::new(SqliteStore::open_in_memory().unwrap());
        let acct = store.ensure_account("me@example.com").unwrap();
        let prod = ApiState::new(store, acct, "t");
        let out = run(&prod, &Plan::OneClick(url)).await;
        assert_eq!(out, Outcome::NeedsYou(NeedsYou::LinkError));
        assert!(hits.lock().unwrap().is_empty());
    }
}
