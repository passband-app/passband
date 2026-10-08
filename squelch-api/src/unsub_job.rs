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
//!   3. The browser, for a mailbox that has the hidden feature and a daemon
//!      that has a Browserbase key ([`crate::unsub_browser`]). A page the
//!      reader gave up on for a reason a real browser could fix (it wants a
//!      choice made, a script run, the address typed) is opened in a rented
//!      remote browser that a small model steers, a few actions at most. It
//!      ends the same two ways, with more precise reasons for the user when a
//!      person really is what the page wants: a sign-in, a captcha, a
//!      confirmation link sent by mail.
//!
//! The original link and not the last redirect hop, because preference centers
//! often set a session cookie on the first hop that this job's jar holds and the
//! user's browser does not; the original replays the whole chain for them, and a
//! page that was NOT a one-GET unsubscribe (that is why it reached the user) is
//! safe to GET again.
//!
//! A job ends in exactly one ledger write ([`Store::finish_unsubscribe_job`]):
//! `done` or `needs_you`. A daemon that dies mid-job leaves `running`, which the
//! next listing reaps into `needs_you` (see [`reap`]).
//!
//! [`Store::finish_unsubscribe_job`]: squelch_core::store::Store::finish_unsubscribe_job

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::Utc;
use squelch_core::features;
use squelch_core::store::Store;
use squelch_core::triage::llm::Usage;
use squelch_core::triage::unsub_agent::{self, Observation, Raise, Step};
use squelch_core::triage::unsub_page::{self, PageDigest, PageVerdict, UnsubPageClient};

use crate::handlers::{audit_action, store_call};
use crate::state::ApiState;
use crate::unsub_browser::{self, Browserbase, Decide, Driven, Errand};
use crate::unsub_cdp::{Cdp, CdpPage};
use crate::unsub_fetch::{FetchError, Fetched, Fetcher};
use crate::unsub_html::{self, Page};

/// Jobs in flight at once, per daemon. Each is a handful of requests to a
/// stranger's site; this only bounds what a burst of `u` presses can open.
const CONCURRENCY: usize = 4;
/// The plain ladder (one-click, then the page), every hop and the model call
/// included, and the wait for a slot. Past it the job is treated as a page
/// nobody could read.
const JOB_BUDGET: Duration = Duration::from_secs(45);
/// The browser rung, on top: the wait for a browser, renting it, every step,
/// and giving it back. A tray that spins this long is the price of not being
/// asked; it is paid only by pages that would otherwise have asked.
const BROWSER_BUDGET: Duration = Duration::from_secs(100);
/// Inside [`BROWSER_BUDGET`]: the drive itself, leaving room to release the
/// browser when the drive is what ran out of time.
const DRIVE_BUDGET: Duration = Duration::from_secs(85);

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

/// Which plan, if any, the automatic path may run for this link.
///
/// RFC 8058 one-click is a property of the HEADER's URL, and the RFC requires
/// it to be https. A header link is the sender naming its own endpoint, so it
/// is always eligible. A FOOTER link is us picking one out of the body, and the
/// body is not always the sender's: a colleague's forward of a newsletter is
/// authenticated mail from the colleague carrying the newsletter's footer, and
/// following it would unsubscribe THEM. So a footer link runs automatically only
/// when its site is the sender's own; otherwise `None`, and the user gets the
/// browser.
pub(crate) fn choose_plan(
    from_header: bool,
    one_click: bool,
    url: &str,
    sender: &str,
) -> Option<Plan> {
    if from_header {
        return Some(
            if one_click && url.to_ascii_lowercase().starts_with("https://") {
                Plan::OneClick(url.to_string())
            } else {
                Plan::Page(url.to_string())
            },
        );
    }
    let link_host = url::Url::parse(url).ok()?.host_str()?.to_ascii_lowercase();
    let sender_host = sender.rsplit_once('@')?.1.to_ascii_lowercase();
    (site_of(&link_host) == site_of(&sender_host)).then(|| Plan::Page(url.to_string()))
}

/// The registrable part of a host, approximately: the last two labels, or the
/// last three under a two-letter country code's common second level
/// (`shop.co.uk`). Approximate is safe here: a miss only sends a footer link to
/// the browser, which is where every link went before this feature.
fn site_of(host: &str) -> String {
    let labels: Vec<&str> = host.trim_end_matches('.').split('.').collect();
    let n = labels.len();
    let take = if n >= 3
        && labels[n - 1].len() == 2
        && matches!(
            labels[n - 2],
            "co" | "com" | "net" | "org" | "gov" | "ac" | "edu" | "or" | "ne"
        ) {
        3
    } else {
        2
    };
    labels[n.saturating_sub(take)..].join(".")
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
    // The browser rung's reasons: what a page turned out to want, once
    // something could actually look at it.
    /// The page wants the user logged in.
    SignIn,
    /// A captcha or a bot wall, seen in a real browser.
    Captcha,
    /// The sender mailed a confirmation link the user has to click.
    CheckEmail,
    /// The page asks something only the user knows or can decide.
    YourCall,
}

impl NeedsYou {
    pub fn as_str(self) -> &'static str {
        match self {
            NeedsYou::ChooseTopics => "choose_topics",
            NeedsYou::NeedsBrowser => "needs_browser",
            NeedsYou::LinkError => "link_error",
            NeedsYou::Unclear => "unclear",
            NeedsYou::SignIn => "sign_in",
            NeedsYou::Captcha => "captcha",
            NeedsYou::CheckEmail => "check_email",
            NeedsYou::YourCall => "your_call",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// Done, and how: `one_click`, `page` or `agent`.
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
    /// Where the browser rung rents its browser. `None` (no key) and the rung
    /// does not exist on this daemon, whatever the feature flag says.
    pub(crate) browser: Option<Arc<Browserbase>>,
}

impl Default for UnsubJobs {
    fn default() -> Self {
        Self {
            running: Arc::new(Mutex::new(HashSet::new())),
            slots: Arc::new(tokio::sync::Semaphore::new(CONCURRENCY)),
            allow_local: false,
            page_reader: None,
            browser: None,
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
///
/// `email` is the address this mail was sent to ([`address_to_type`]): the one
/// string the browser rung may type, should a form ask who is unsubscribing.
pub(crate) async fn start(
    state: &ApiState,
    sender: String,
    message_id: i64,
    plan: Plan,
    email: String,
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
        // The budget covers the wait for a slot too: a queued job that never
        // got one in time goes to the user rather than spinning forever.
        //
        // The work runs in a task of its own so that a PANIC in it (hostile
        // input finding a bug) is a JoinError here, not a lost settle: the row
        // must never be left `running` with the sender claimed until restart.
        let inner = state.clone();
        let who = sender.clone();
        let work = tokio::spawn(async move {
            let job = async {
                let _slot = inner.unsub.slots.clone().acquire_owned().await;
                run(&inner, &plan).await
            };
            // The slot is given back before the browser rung: that one waits
            // on its own, scarcer, count.
            let plain = tokio::time::timeout(JOB_BUDGET, job)
                .await
                .unwrap_or(Outcome::NeedsYou(NeedsYou::Unclear));
            let errand = Errand {
                url: plan.url(),
                sender: &who,
                email: &email,
            };
            escalate(&inner, &errand, plain).await
        });
        let outcome = work.await.unwrap_or(Outcome::NeedsYou(NeedsYou::Unclear));
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

/// The address a form should be given when it asks who is unsubscribing: the
/// one this mail was SENT TO, when that is plainly the user's own.
///
/// The account's address is the default and is what nearly every sender has.
/// The exception worth getting right is a plus address (`me+shop@…`): the
/// sender's list holds THAT string, and a form given the bare address answers
/// "not subscribed" or, worse, signs the bare one up. Nothing looser is
/// matched: a `To:` that is somebody else's list address or an alias we cannot
/// vouch for is never typed anywhere.
pub(crate) fn address_to_type(account_email: &str, to_addrs: Option<&str>) -> String {
    let account = account_email.trim().to_ascii_lowercase();
    let Some((local, domain)) = account.split_once('@') else {
        return account;
    };
    let base = local.split('+').next().unwrap_or(local);
    to_addrs
        .map(squelch_core::sync::ingest::parse_stored_recipients)
        .unwrap_or_default()
        .into_iter()
        .map(|a| a.trim().to_ascii_lowercase())
        .find(|a| {
            a.split_once('@').is_some_and(|(l, d)| {
                d == domain
                    && l.split_once('+')
                        .is_some_and(|(b, tag)| b == base && !tag.is_empty())
            })
        })
        .unwrap_or(account)
}

/// Can a real browser plausibly finish what the plain reader could not?
/// A dead link stays dead in any browser; everything else the reader gives up
/// on is exactly what the browser rung is for.
fn worth_a_browser(why: NeedsYou) -> bool {
    matches!(
        why,
        NeedsYou::ChooseTopics | NeedsYou::NeedsBrowser | NeedsYou::Unclear
    )
}

/// The browser rung. Takes the plain ladder's outcome and returns it untouched
/// unless every condition for a browser holds; then the outcome is the
/// browser's, EXCEPT when the machinery itself fails (no browser to rent, a
/// dropped socket, the model's cap), where the reader's own reason still
/// stands. Infallible and bounded by [`BROWSER_BUDGET`].
async fn escalate(state: &ApiState, errand: &Errand<'_>, plain: Outcome) -> Outcome {
    let Outcome::NeedsYou(why) = plain else {
        return plain;
    };
    if !worth_a_browser(why) || !state.features().enabled(features::UNSUB_BROWSER) {
        return plain;
    }
    let (Some(browser), Some(model)) =
        (state.unsub.browser.clone(), state.unsub.page_reader.clone())
    else {
        return plain;
    };
    // The browser is not on our network, so this is not the SSRF guard it is
    // for the fetcher. It keeps the rule the same on every rung: a link that
    // points at a private name or an odd port is not one we follow.
    let ok = url::Url::parse(errand.url)
        .is_ok_and(|u| crate::unsub_fetch::check_url(&u, state.unsub.allow_local).is_ok());
    if !ok {
        return plain;
    }

    let rung = async {
        let _slot = browser.slots.clone().acquire_owned().await;
        let session = browser.open().await?;
        let decider = ModelDecider {
            state,
            client: &model,
        };
        let driven = tokio::time::timeout(DRIVE_BUDGET, async {
            let cdp = Cdp::connect(&session.connect_url).await.ok()?;
            let mut page = CdpPage::attach(cdp).await.ok()?;
            Some(unsub_browser::drive(&mut page, &decider, errand).await)
        })
        .await
        .ok()
        .flatten();
        browser.release(&session).await;
        driven
    };
    match tokio::time::timeout(BROWSER_BUDGET, rung).await {
        Ok(Some(Driven::Done)) => Outcome::Done("agent"),
        Ok(Some(Driven::Raise(raise))) => Outcome::NeedsYou(match raise {
            Raise::SignIn => NeedsYou::SignIn,
            Raise::Captcha => NeedsYou::Captcha,
            Raise::CheckEmail => NeedsYou::CheckEmail,
            Raise::YourCall => NeedsYou::YourCall,
            Raise::Broken => NeedsYou::LinkError,
            // The agent could not say more than the reader already had.
            Raise::Unclear => why,
        }),
        Ok(Some(Driven::Lost)) | Ok(None) | Err(_) => plain,
    }
}

/// The model, as the browser rung's decider: one capped, billed call a step.
struct ModelDecider<'a> {
    state: &'a ApiState,
    client: &'a UnsubPageClient,
}

impl Decide for ModelDecider<'_> {
    async fn decide(&self, observation: &Observation<'_>) -> Option<Step> {
        if !under_cap(
            self.state,
            unsub_agent::USAGE_CATEGORY,
            unsub_agent::DAILY_CAP,
        )
        .await
        {
            return None;
        }
        let (step, usage) = unsub_agent::next_step(self.client, observation).await;
        bill(self.state, unsub_agent::USAGE_CATEGORY, usage).await;
        Some(step)
    }
}

/// The plain ladder. Infallible: every failure is a [`NeedsYou`].
pub(crate) async fn run(state: &ApiState, plan: &Plan) -> Outcome {
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
    let Some(mut page) = read(&fetched).await else {
        return Outcome::NeedsYou(http_failure(&fetched));
    };
    let mut page_url = fetched.final_url.clone();

    // A script-free redirect, once. Some senders land on a "processing..." page
    // that refreshes into the real one.
    if !page.says_done()
        && page.actions.is_empty()
        && let Some(next) = page.meta_refresh.clone()
        && let Ok(f) = fetcher.get(next.as_str()).await
        && let Some(p) = read(&f).await
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
    if page.actions.get(index).is_some_and(|a| a.needs_script()) {
        return Outcome::NeedsYou(NeedsYou::NeedsBrowser);
    }
    let Some(submission) = page.actions.get(index).and_then(|a| a.submission()) else {
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
    let Some(result) = read(&after).await else {
        return Outcome::NeedsYou(http_failure(&after));
    };
    if result.says_done_after_submit() {
        return Outcome::Done("page");
    }
    match verdict_after_submit(state, &after.final_url, &result).await {
        PageVerdict::Unsubscribed => Outcome::Done("page"),
        PageVerdict::Preferences => Outcome::NeedsYou(NeedsYou::ChooseTopics),
        _ => Outcome::NeedsYou(NeedsYou::Unclear),
    }
}

/// A 2xx HTML page, parsed; `None` for anything else. Parsing is bounded
/// (`unsub_html` caps its walk) but still CPU work on hostile input, so it runs
/// on the blocking pool, never on a runtime worker the job timeout cannot
/// preempt.
async fn read(f: &Fetched) -> Option<Page> {
    if !f.status.is_success() {
        return None;
    }
    // A plain-text "you have been unsubscribed" is an answer too.
    if f.content_type
        .as_deref()
        .is_some_and(|c| c.to_ascii_lowercase().starts_with("text/plain"))
    {
        return Some(Page::from_text(&f.body));
    }
    if !f.is_html() {
        return None;
    }
    let (body, url) = (f.body.clone(), f.final_url.clone());
    tokio::task::spawn_blocking(move || unsub_html::parse_page(&body, &url))
        .await
        .ok()
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
    if !under_cap(state, unsub_page::USAGE_CATEGORY, unsub_page::DAILY_CAP).await {
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
    bill(state, unsub_page::USAGE_CATEGORY, usage).await;
    Some(verdict)
}

/// Has this account made fewer than `cap` calls under `category` today? A
/// ledger that cannot be read is a cap reached: the fallback is the user's
/// browser, which is never wrong.
async fn under_cap(state: &ApiState, category: &'static str, cap: u32) -> bool {
    store_call(state, move |store, account_id| {
        let today = Utc::now().format("%Y-%m-%d").to_string();
        let spent: u64 = store
            .list_usage_by_category(account_id, 1)?
            .into_iter()
            .filter(|(c, _)| c == category)
            .flat_map(|(_, days)| days)
            .filter(|d| d.day == today)
            .map(|d| d.calls)
            .sum();
        Ok(spent < u64::from(cap))
    })
    .await
    .unwrap_or(false)
}

/// Record one model call's usage under `category`.
async fn bill(state: &ApiState, category: &'static str, usage: Option<Usage>) {
    let Some(u) = usage else { return };
    let _ = store_call(state, move |store, account_id| {
        let day = Utc::now().format("%Y-%m-%d").to_string();
        if let Err(e) = store.extract_bump_usage(account_id, &day, category, u.into()) {
            eprintln!("squelch: unsubscribe {category} usage ledger bump failed ({e})");
        }
        Ok(())
    })
    .await;
}

/// A `running` row older than this cannot belong to a live job: every job
/// settles inside [`JOB_BUDGET`] plus [`BROWSER_BUDGET`], slot waits included.
const REAP_AFTER: Duration = Duration::from_secs(180);

/// Reap rows orphaned by a restart. Called from the listing. Only rows past
/// [`REAP_AFTER`] AND not owned in this process: the age test is what makes it
/// race-free, since a job claimed while this runs has a fresh `status_at`.
pub(crate) async fn reap(state: &ApiState) {
    let live = state.unsub.live();
    let now = Utc::now();
    let stale_before = now - chrono::Duration::from_std(REAP_AFTER).unwrap_or_default();
    let _ = store_call(state, move |store, account_id| {
        store.reap_unsubscribe_jobs(account_id, &live, stale_before, now)
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

    #[test]
    fn plan_choice_follows_the_header_the_rfc_and_the_senders_own_site() {
        let one = |h, oc, u: &str, s: &str| {
            choose_plan(h, oc, u, s).map(|p| match p {
                Plan::OneClick(_) => "one_click",
                Plan::Page(_) => "page",
            })
        };
        assert_eq!(
            one(true, true, "https://x.example/u", "a@b.example"),
            Some("one_click")
        );
        assert_eq!(
            one(true, true, "http://x.example/u", "a@b.example"),
            Some("page"),
            "RFC 8058 wants https"
        );
        assert_eq!(
            one(true, false, "https://x.example/u", "a@b.example"),
            Some("page")
        );
        // Footer links: the sender's own site only.
        assert_eq!(
            one(
                false,
                false,
                "https://email.shop.example/u",
                "news@shop.example"
            ),
            Some("page")
        );
        assert_eq!(
            one(
                false,
                false,
                "https://news.shop.co.uk/u",
                "hi@mail.shop.co.uk"
            ),
            Some("page")
        );
        assert_eq!(
            one(
                false,
                false,
                "https://click.esp.example/u",
                "alice@coworker.example"
            ),
            None,
            "a forwarded newsletter's footer is not the forwarder's to unsubscribe from"
        );
        assert_eq!(
            one(false, true, "https://other.co.uk/u", "a@shop.co.uk"),
            None
        );
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

    #[test]
    fn the_address_typed_is_the_one_the_mail_was_sent_to_when_it_is_plainly_ours() {
        let me = "Me@Example.com";
        assert_eq!(address_to_type(me, None), "me@example.com");
        assert_eq!(
            address_to_type(me, Some("me@example.com")),
            "me@example.com"
        );
        assert_eq!(
            address_to_type(me, Some("Shop <ME+Shop@example.com>")),
            "me+shop@example.com",
            "the list holds the plus address, so that is what a form must be given"
        );
        // Nothing looser: not somebody else's list address, not another
        // domain's lookalike, not a different person at ours.
        for to in [
            "subscribers@shop.example",
            "me+shop@evil.example",
            "meow+shop@example.com",
            "me+@example.com",
            "other@example.com",
        ] {
            assert_eq!(address_to_type(me, Some(to)), "me@example.com", "{to}");
        }
    }

    // ---- the browser rung ---------------------------------------------------

    use axum::extract::State;
    use squelch_core::config::Stage2Provider;
    use squelch_core::features::Features;

    /// A stand-in for Browserbase's API. `connect` is what a rented session's
    /// `connectUrl` will be; `None` answers 429, an account out of browsers.
    async fn browserbase(connect: Option<String>) -> (Browserbase, Arc<Mutex<Vec<String>>>) {
        let hits = Arc::new(Mutex::new(Vec::new()));
        let app = axum::Router::new()
            .route(
                "/v1/sessions",
                post(move |State(h): State<Arc<Mutex<Vec<String>>>>| async move {
                    h.lock().unwrap().push("create".to_string());
                    match connect {
                        Some(url) => (
                            StatusCode::CREATED,
                            axum::Json(serde_json::json!({ "id": "s1", "connectUrl": url })),
                        ),
                        None => (
                            StatusCode::TOO_MANY_REQUESTS,
                            axum::Json(serde_json::json!({})),
                        ),
                    }
                }),
            )
            .route(
                "/v1/sessions/{id}",
                post(
                    |State(h): State<Arc<Mutex<Vec<String>>>>, body: String| async move {
                        h.lock().unwrap().push(format!("update {body}"));
                        axum::Json(serde_json::json!({}))
                    },
                ),
            )
            .with_state(hits.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let bb = Browserbase::new("bb_key".into(), None, None)
            .unwrap()
            .with_api_base(&format!("http://{addr}"));
        (bb, hits)
    }

    /// A sender whose page is a preference center drawn by script: the plain
    /// reader sees an empty shell, a browser sees two ticked topics, an email
    /// field and a save button that posts with `fetch`.
    async fn prefs_site() -> (String, Arc<Mutex<Vec<String>>>) {
        const PAGE: &str = r#"<!doctype html><html><head><title>Preferences</title></head><body>
<noscript>Please enable JavaScript to manage your preferences.</noscript><div id="app"></div>
<script>
document.getElementById('app').innerHTML = `
  <h1>Email preferences</h1>
  <label><input type="checkbox" id="deals" checked> Weekly deals</label>
  <label><input type="checkbox" id="news" checked> Product news</label>
  <input type="email" id="em" placeholder="Email address">
  <button id="save">Save preferences</button>`;
document.getElementById('save').addEventListener('click', async () => {
  await fetch('/save', { method: 'POST', body: JSON.stringify({
    deals: document.getElementById('deals').checked,
    news: document.getElementById('news').checked,
    email: document.getElementById('em').value }) });
  document.getElementById('app').innerHTML = '<h1>Your preferences have been saved.</h1>';
});
</script></body></html>"#;
        let hits = Arc::new(Mutex::new(Vec::new()));
        let app = axum::Router::new()
            .route(
                "/prefs",
                axum::routing::get(|| async { axum::response::Html(PAGE) }),
            )
            .route(
                "/save",
                post(
                    |State(h): State<Arc<Mutex<Vec<String>>>>, body: String| async move {
                        h.lock().unwrap().push(body);
                        "ok"
                    },
                ),
            )
            .with_state(hits.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}/prefs"), hits)
    }

    fn strings(v: &serde_json::Value, out: &mut String) {
        match v {
            serde_json::Value::String(s) => {
                out.push_str(s);
                out.push('\n');
            }
            serde_json::Value::Array(a) => a.iter().for_each(|x| strings(x, out)),
            serde_json::Value::Object(o) => o.values().for_each(|x| strings(x, out)),
            _ => {}
        }
    }

    /// A model endpoint that plays both parts off the REAL prompts: asked to
    /// classify a page it says "preferences"; asked for an agent step it reads
    /// the controls out of the page it was sent, switches every ticked topic
    /// off, fills the email field and presses save, then says done once the
    /// page says saved.
    async fn model() -> String {
        async fn answer(body: String) -> impl axum::response::IntoResponse {
            let req: serde_json::Value = serde_json::from_str(&body).unwrap();
            let mut text = String::new();
            strings(&req, &mut text);
            let verdict = if !text.contains("You finish an email unsubscribe") {
                serde_json::json!({ "verdict": "preferences", "form": -1 })
            } else {
                let page = text
                    .rsplit("-----BEGIN UNTRUSTED UNSUBSCRIBE PAGE-----")
                    .next()
                    .and_then(|fenced| fenced.split("\npage:\n").nth(1))
                    // The system prompt follows in this dump, with example
                    // controls of its own: stop at the fence.
                    .and_then(|page| page.split("-----END UNTRUSTED").next())
                    .unwrap_or_default();
                if page.contains("have been saved") {
                    serde_json::json!({ "status": "done", "reason": "none", "actions": [] })
                } else {
                    let mut actions = Vec::new();
                    let refs = |pattern: &str| -> Vec<i64> {
                        regex::Regex::new(pattern)
                            .unwrap()
                            .captures_iter(page)
                            .map(|c| c[1].parse().unwrap())
                            .collect()
                    };
                    for r in refs(r"\[(\d+) checkbox on ") {
                        actions.push(
                            serde_json::json!({ "action": "uncheck", "ref": r, "option": -1 }),
                        );
                    }
                    for r in refs(r"\[(\d+) email ") {
                        actions.push(
                            serde_json::json!({ "action": "fill_email", "ref": r, "option": -1 }),
                        );
                    }
                    for r in refs(r#"\[(\d+) button "Save"#) {
                        actions
                            .push(serde_json::json!({ "action": "click", "ref": r, "option": -1 }));
                    }
                    serde_json::json!({ "status": "act", "reason": "none", "actions": actions })
                }
            };
            let body = serde_json::json!({
                "content": [{ "type": "text", "text": verdict.to_string() }],
                "stop_reason": "end_turn",
                "usage": { "input_tokens": 900, "output_tokens": 40 },
            })
            .to_string();
            ([("content-type", "application/json")], body)
        }
        let app = axum::Router::new().fallback(answer);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}")
    }

    fn rung_state(
        features: Features,
        bb: Option<Browserbase>,
        model: Option<String>,
    ) -> (ApiState, Arc<SqliteStore>, i64) {
        let store = Arc::new(SqliteStore::open_in_memory().unwrap());
        let acct = store.ensure_account("me@example.com").unwrap();
        let mut state = ApiState::new(store.clone(), acct, "t")
            .with_unsub_fetch_allow_local(true)
            .with_features(features)
            .with_unsub_browser(bb);
        if let Some(url) = model {
            state = state.with_unsub_page_reader(Some(UnsubPageClient::new(
                reqwest::Client::new(),
                url,
                "sk-test",
                Stage2Provider::Anthropic,
                "claude-haiku-4-5",
                None,
            )));
        }
        (state, store, acct)
    }

    fn errand(url: &str) -> Errand<'_> {
        Errand {
            url,
            sender: "news@shop.example",
            email: "me+shop@example.com",
        }
    }

    const ON: [&str; 1] = [features::UNSUB_BROWSER];

    #[tokio::test]
    async fn no_browser_is_rented_without_the_feature_the_key_and_a_reason_to() {
        let stuck = Outcome::NeedsYou(NeedsYou::ChooseTopics);
        let url = "http://127.0.0.1:9/prefs";

        // The key and a model, but the mailbox has not been let in.
        let (bb, hits) = browserbase(None).await;
        let (state, ..) = rung_state(Features::none(), Some(bb), Some(model().await));
        assert_eq!(escalate(&state, &errand(url), stuck).await, stuck);
        assert!(hits.lock().unwrap().is_empty(), "feature off");

        // Let in, but this daemon has no key.
        let (state, ..) = rung_state(Features::pinned(ON), None, Some(model().await));
        assert_eq!(escalate(&state, &errand(url), stuck).await, stuck);

        // Let in, with a key, but no model to steer with.
        let (bb, hits) = browserbase(None).await;
        let (state, ..) = rung_state(Features::pinned(ON), Some(bb), None);
        assert_eq!(escalate(&state, &errand(url), stuck).await, stuck);
        assert!(hits.lock().unwrap().is_empty(), "no model");

        // Everything in place, and outcomes a browser cannot improve on.
        let (bb, hits) = browserbase(None).await;
        let (state, ..) = rung_state(Features::pinned(ON), Some(bb), Some(model().await));
        for settled in [
            Outcome::Done("one_click"),
            Outcome::Done("page"),
            Outcome::NeedsYou(NeedsYou::LinkError),
        ] {
            assert_eq!(escalate(&state, &errand(url), settled).await, settled);
        }
        assert!(hits.lock().unwrap().is_empty(), "nothing to fix");
    }

    #[tokio::test]
    async fn a_link_the_fetcher_would_refuse_is_not_handed_to_a_browser_either() {
        let (bb, hits) = browserbase(None).await;
        let (state, ..) = rung_state(Features::pinned(ON), Some(bb), Some(model().await));
        // Production: no test hook, so loopback is refused.
        let state = state.with_unsub_fetch_allow_local(false);
        let stuck = Outcome::NeedsYou(NeedsYou::NeedsBrowser);
        for url in [
            "http://127.0.0.1:8848/client/stats",
            "http://router.lan/",
            "file:///etc/passwd",
            "https://shop.example:8443/u",
        ] {
            assert_eq!(escalate(&state, &errand(url), stuck).await, stuck, "{url}");
        }
        assert!(hits.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_browser_that_cannot_be_rented_leaves_the_readers_reason_standing() {
        let (bb, hits) = browserbase(None).await;
        let (state, ..) = rung_state(Features::pinned(ON), Some(bb), Some(model().await));
        let stuck = Outcome::NeedsYou(NeedsYou::ChooseTopics);
        assert_eq!(
            escalate(&state, &errand("http://127.0.0.1:9/prefs"), stuck).await,
            stuck
        );
        assert_eq!(
            hits.lock().unwrap().clone(),
            ["create"],
            "asked once, not retried"
        );
    }

    #[tokio::test]
    async fn a_rented_browser_that_never_answers_is_given_back_and_the_reason_stands() {
        // A session whose connect URL goes nowhere.
        let (bb, hits) = browserbase(Some("ws://127.0.0.1:9/devtools/browser/x".into())).await;
        let (state, ..) = rung_state(Features::pinned(ON), Some(bb), Some(model().await));
        let stuck = Outcome::NeedsYou(NeedsYou::NeedsBrowser);
        assert_eq!(
            escalate(&state, &errand("http://127.0.0.1:9/prefs"), stuck).await,
            stuck
        );
        let hits = hits.lock().unwrap().clone();
        assert_eq!(hits.len(), 2, "{hits:?}");
        assert!(hits[1].contains("REQUEST_RELEASE"), "{hits:?}");
    }

    /// THE WHOLE RUNG, with a real browser standing in for the rented one: the
    /// plain reader gives up on a script-drawn preference center, and the
    /// agent finishes it. Needs a local Chrome, so ignored by default:
    /// `cargo test -p squelch-api unsub_job -- --ignored --test-threads=1`.
    #[tokio::test]
    #[ignore = "needs a local Chrome"]
    async fn a_page_the_reader_gives_up_on_is_finished_in_a_browser() {
        let chrome = crate::unsub_cdp::tests::chrome().await;
        let (url, saves) = prefs_site().await;
        let (bb, rented) = browserbase(Some(chrome.ws.clone())).await;
        let (state, store, acct) = rung_state(Features::pinned(ON), Some(bb), Some(model().await));

        let plain = run(&state, &Plan::Page(url.clone())).await;
        assert!(
            matches!(plain, Outcome::NeedsYou(_)),
            "the plain reader cannot finish this page: {plain:?}"
        );
        assert!(saves.lock().unwrap().is_empty());

        let out = escalate(&state, &errand(&url), plain).await;
        assert_eq!(out, Outcome::Done("agent"));

        let saves = saves.lock().unwrap().clone();
        assert_eq!(saves.len(), 1, "{saves:?}");
        let sent: serde_json::Value = serde_json::from_str(&saves[0]).unwrap();
        assert_eq!(
            sent,
            serde_json::json!({ "deals": false, "news": false, "email": "me+shop@example.com" })
        );
        let rented = rented.lock().unwrap().clone();
        assert_eq!(
            rented.len(),
            2,
            "one browser, rented and given back: {rented:?}"
        );
        assert!(rented[1].contains("REQUEST_RELEASE"));

        // Two steps (act, then done), billed under the agent's own category.
        let usage = store.list_usage_by_category(acct, 1).unwrap();
        let steps: u64 = usage
            .iter()
            .filter(|(c, _)| c == unsub_agent::USAGE_CATEGORY)
            .flat_map(|(_, days)| days)
            .map(|d| d.calls)
            .sum();
        assert_eq!(steps, 2, "{usage:?}");
    }
}
