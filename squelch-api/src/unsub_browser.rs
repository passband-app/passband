//! The BROWSER RUNG of the automatic unsubscribe: a rented remote browser, and
//! the loop that lets a small model steer it through a sender's page.
//!
//! `crate::unsub_job` gets here only after its plain reader gave up on a page
//! (a preference center, a page that is empty until its script runs, a form
//! that wants the address typed), and only for a mailbox that has the hidden
//! feature and a daemon that has a Browserbase key. Everything about WHEN lives
//! there. This module is HOW:
//!
//!   - [`Browserbase`] rents one browser per job and gives it back. The browser
//!     runs on their network, not ours, so a stranger's page and its scripts
//!     never execute anywhere that can reach this daemon, its volume, or its
//!     neighbours. That is the whole reason it is rented.
//!   - [`drive`] is the loop: read the page, ask the model
//!     (`squelch_core::triage::unsub_agent`), check each action it names
//!     against what is actually on the page, carry it out, read again.
//!
//! WHAT THE AGENT CAN DO IS DECIDED HERE, NOT IN THE PROMPT ([`permit`]): press
//! a button or link that is not on the short list of things no unsubscribe
//! needs, flip a toggle, choose an option, and type ONE string, the address
//! this mail was sent to, into a field that is for an email address. It has no
//! password to give, no card, no name, and no way to type one if a page asked.
//!
//! WHAT THE RENTED BROWSER LEARNS: the unsubscribe link (which names the
//! recipient to the sender, as it always did), and that address if a form asks
//! for it. Sessions are created with recording and logging off. Nothing else
//! about the mailbox leaves the daemon; the agent has no route to mail at all.

use std::sync::{Arc, LazyLock};
use std::time::Duration;

use regex::Regex;
use serde_json::json;
use squelch_core::triage::unsub_agent::{Action, MAX_STEPS, Observation, Raise, Step};

use crate::unsub_cdp::{BrowserPage, Kind, PageError, Snapshot};

/// Browsers rented at once, per daemon. A job holds one for a minute or so,
/// and the account's plan has its own ceiling; two keeps a burst of `u`
/// presses from spending it.
const CONCURRENCY: usize = 2;
/// Seconds until Browserbase ends a session on its own. Their floor is 60. The
/// job releases its session when it finishes; this is for the job that died.
const SESSION_TIMEOUT_SECS: u64 = 120;
const API_TIMEOUT: Duration = Duration::from_secs(15);
const DEFAULT_API_BASE: &str = "https://api.browserbase.com";

/// The key. Its presence is what makes the rung possible on this daemon.
pub(crate) const API_KEY_VAR: &str = "SQUELCH_BROWSERBASE_API_KEY";
/// Browserbase's own name for the same key, read when ours is unset: it is
/// what their dashboard tells people to export, so it is what a `.env` holds.
pub(crate) const API_KEY_FALLBACK_VAR: &str = "BROWSERBASE_API_KEY";
/// Optional: which project sessions are created in. Unset, Browserbase uses
/// the key's own.
pub(crate) const PROJECT_VAR: &str = "SQUELCH_BROWSERBASE_PROJECT_ID";
/// Optional: `us-west-2` (theirs by default), `us-east-1`, `eu-central-1`,
/// `ap-southeast-1`.
pub(crate) const REGION_VAR: &str = "SQUELCH_BROWSERBASE_REGION";

/// The account that rents browsers.
pub(crate) struct Browserbase {
    http: reqwest::Client,
    api_base: String,
    api_key: String,
    project_id: Option<String>,
    region: Option<String>,
    pub(crate) slots: Arc<tokio::sync::Semaphore>,
}

impl std::fmt::Debug for Browserbase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Browserbase")
            .field("api_base", &self.api_base)
            .field("region", &self.region)
            .finish_non_exhaustive()
    }
}

/// One rented browser. `connect_url` IS A CREDENTIAL and must not be logged.
pub(crate) struct Session {
    pub id: String,
    pub connect_url: String,
}

impl Browserbase {
    pub fn new(
        api_key: String,
        project_id: Option<String>,
        region: Option<String>,
    ) -> Option<Self> {
        // Redirects refused: the key rides every request.
        let http = reqwest::Client::builder()
            .timeout(API_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .ok()?;
        Some(Self {
            http,
            api_base: DEFAULT_API_BASE.to_string(),
            api_key,
            project_id,
            region,
            slots: Arc::new(tokio::sync::Semaphore::new(CONCURRENCY)),
        })
    }

    /// From the environment, or `None` when no key is set: every self-host
    /// that did not bring one, and every test.
    pub fn from_env() -> Option<Self> {
        let var = |name: &str| {
            std::env::var(name)
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        let key = var(API_KEY_VAR).or_else(|| var(API_KEY_FALLBACK_VAR))?;
        Self::new(key, var(PROJECT_VAR), var(REGION_VAR))
    }

    /// TEST HOOK: point the client at a stand-in API.
    #[cfg(test)]
    pub fn with_api_base(mut self, base: &str) -> Self {
        self.api_base = base.trim_end_matches('/').to_string();
        self
    }

    /// Rent a browser. `None` for every failure (no capacity, a bad key, their
    /// outage): the caller's answer to all of them is the same, the user gets
    /// the link, and only the status is worth a log line.
    pub async fn open(&self) -> Option<Session> {
        let mut body = json!({
            "timeout": SESSION_TIMEOUT_SECS,
            "browserSettings": {
                // The page shows the recipient's address and the link carries
                // their token: no replay and no logs are kept of either.
                "recordSession": false,
                "logSession": false,
                // A captcha is a site asking for a person. It gets one.
                "solveCaptchas": false,
            },
        });
        if let Some(p) = &self.project_id {
            body["projectId"] = json!(p);
        }
        if let Some(r) = &self.region {
            body["region"] = json!(r);
        }
        let resp = self
            .http
            .post(format!("{}/v1/sessions", self.api_base))
            .header("X-BB-API-Key", &self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|_| eprintln!("squelch: unsubscribe browser could not be reached"))
            .ok()?;
        let status = resp.status();
        if !status.is_success() {
            eprintln!("squelch: unsubscribe browser was not rented ({status})");
            return None;
        }
        let v: serde_json::Value = resp.json().await.ok()?;
        Some(Session {
            id: v["id"].as_str()?.to_string(),
            connect_url: v["connectUrl"].as_str()?.to_string(),
        })
    }

    /// Give a browser back. Best effort: an unreleased session ends itself at
    /// [`SESSION_TIMEOUT_SECS`].
    pub async fn release(&self, session: &Session) {
        let mut body = json!({ "status": "REQUEST_RELEASE" });
        if let Some(p) = &self.project_id {
            body["projectId"] = json!(p);
        }
        let _ = self
            .http
            .post(format!("{}/v1/sessions/{}", self.api_base, session.id))
            .header("X-BB-API-Key", &self.api_key)
            .json(&body)
            .send()
            .await;
    }
}

/// How a drive ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Driven {
    /// The page said the user is out.
    Done,
    /// The agent wants a person, and why.
    Raise(Raise),
    /// The machinery failed (no model, a dead browser, a page that never
    /// loaded). Not a finding about the page: the caller keeps the reason it
    /// already had.
    Lost,
}

/// Who answers "what next". A trait so the loop is testable with a script;
/// production asks the model (`crate::unsub_job`). `None` means there is no
/// answer to be had (no model, the daily cap), which ends the drive as
/// [`Driven::Lost`].
pub(crate) trait Decide {
    async fn decide(&self, observation: &Observation<'_>) -> Option<Step>;
}

/// What a drive needs to know that the page does not get to tell it.
pub(crate) struct Errand<'a> {
    /// The ORIGINAL unsubscribe link.
    pub url: &'a str,
    pub sender: &'a str,
    /// The one string that may be typed.
    pub email: &'a str,
}

/// Labels no unsubscribe needs pressed. The model is told the same; this is
/// for the page that talks it round. Matched on the control's own label, as
/// whole words: "subscribe" does not match inside "unsubscribe".
static NEVER_PRESS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(?i)\b(",
        r"re-?subscribe|subscribe|undo|stay subscribed|keep me subscribed|",
        r"sign ?in|log ?in|sign ?up|register|create (an |my |your )?account|",
        r"(delete|close|deactivate|remove) (my |your |this )?account|",
        r"download|install|buy|purchase|checkout|upgrade|add to cart",
        r")\b"
    ))
    .expect("static regex compiles")
});

/// Is this action one the agent may take on THIS page? `Err` is a line for the
/// model saying why not. The check runs against the controls the daemon found,
/// never against what the model believes is there.
pub(crate) fn permit(snap: &Snapshot, action: Action) -> Result<(), &'static str> {
    let r = match action {
        Action::Click { r }
        | Action::SetChecked { r, .. }
        | Action::Select { r, .. }
        | Action::FillEmail { r } => r,
    };
    let Some(c) = snap.controls.get(r) else {
        return Err("there is no control with that number");
    };
    if c.disabled {
        return Err("that control is disabled");
    }
    match action {
        Action::Click { .. } => {
            if !matches!(
                c.kind,
                Kind::Link | Kind::Button | Kind::Checkbox | Kind::Radio | Kind::Switch
            ) {
                return Err("that control cannot be clicked");
            }
            if NEVER_PRESS.is_match(&c.label) {
                return Err("the client will not press that control");
            }
        }
        Action::SetChecked { .. } => {
            if !matches!(c.kind, Kind::Checkbox | Kind::Radio | Kind::Switch) {
                return Err("that control is not a checkbox, radio or switch");
            }
        }
        Action::Select { option, .. } => {
            if c.kind != Kind::Select {
                return Err("that control is not a select");
            }
            if option >= c.options {
                return Err("that select has no such option");
            }
        }
        Action::FillEmail { .. } => {
            if !(c.kind == Kind::Email || (c.kind == Kind::Text && c.email_like)) {
                return Err("that field is not for an email address");
            }
        }
    }
    Ok(())
}

/// One line of history: what was done, in our words, quoting the page's label.
fn describe(snap: &Snapshot, action: Action) -> String {
    let label = |r: usize| {
        snap.controls
            .get(r)
            .map(|c| c.label.as_str())
            .unwrap_or_default()
    };
    match action {
        Action::Click { r } => format!("pressed {r} \"{}\"", label(r)),
        Action::SetChecked { r, on: true } => format!("turned on {r} \"{}\"", label(r)),
        Action::SetChecked { r, on: false } => format!("turned off {r} \"{}\"", label(r)),
        Action::Select { r, option } => format!("chose option {option} of {r} \"{}\"", label(r)),
        Action::FillEmail { r } => format!("typed the user's address into {r} \"{}\"", label(r)),
    }
}

/// Drive one page to an ending. Infallible: every failure is a [`Driven`].
pub(crate) async fn drive<P: BrowserPage, D: Decide>(
    page: &mut P,
    decider: &D,
    errand: &Errand<'_>,
) -> Driven {
    if page.open(errand.url).await.is_err() {
        return Driven::Lost;
    }
    let mut history: Vec<String> = Vec::new();
    // The page as last read, to notice a step that changed nothing.
    let mut before: Option<String> = None;
    let mut unchanged = 0;

    for step in 1..=MAX_STEPS {
        let snap = match page.snapshot().await {
            Ok(s) => s,
            Err(_) => return Driven::Lost,
        };
        if before.as_deref() == Some(snap.text.as_str()) {
            unchanged += 1;
            // Twice running: the agent is pressing something that does
            // nothing, and a third try is not a plan.
            if unchanged >= 2 {
                return Driven::Raise(Raise::Unclear);
            }
            history.push("the page looks the same as before those actions".to_string());
        } else {
            unchanged = 0;
        }
        let host = url::Url::parse(&snap.url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_string))
            .unwrap_or_default();
        let decision = decider
            .decide(&Observation {
                sender: errand.sender,
                step,
                host: &host,
                title: &snap.title,
                page: &snap.text,
                history: &history,
            })
            .await;
        let actions = match decision {
            None => return Driven::Lost,
            Some(Step::Done) => return Driven::Done,
            Some(Step::Raise(why)) => return Driven::Raise(why),
            Some(Step::Act(actions)) => actions,
        };
        for action in actions {
            if let Err(why) = permit(&snap, action) {
                // Told, not obeyed, and the rest of the list is dropped: it
                // was written by a model that misread the page.
                history.push(format!("REFUSED ({why}): {}", describe(&snap, action)));
                break;
            }
            let done = match action {
                Action::Click { r } => page.click(r).await,
                Action::SetChecked { r, on } => page.set_checked(r, on).await,
                Action::Select { r, option } => page.select(r, option).await,
                Action::FillEmail { r } => page.fill(r, errand.email).await,
            };
            match done {
                Ok(()) => history.push(describe(&snap, action)),
                // The page moved on (a click navigated, a control vanished):
                // the numbers the rest of the list uses are no longer true.
                Err(PageError::Stale) => {
                    history.push(format!(
                        "could not be done, the page had changed: {}",
                        describe(&snap, action)
                    ));
                    break;
                }
                Err(PageError::Browser(_)) => return Driven::Lost,
            }
            // A press may have navigated; whatever follows it in the list was
            // chosen for the page before.
            if matches!(action, Action::Click { .. }) {
                break;
            }
        }
        before = Some(snap.text);
    }
    // Out of steps without the page saying done.
    Driven::Raise(Raise::Unclear)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::unsub_cdp::Control;
    use std::sync::Mutex;

    fn control(kind: Kind, label: &str) -> Control {
        Control {
            kind,
            label: label.to_string(),
            options: if kind == Kind::Select { 3 } else { 0 },
            email_like: false,
            disabled: false,
        }
    }

    fn snap(text: &str, controls: Vec<Control>) -> Snapshot {
        Snapshot {
            url: "https://prefs.shop.example/u".into(),
            title: "Preferences".into(),
            text: text.into(),
            controls,
        }
    }

    /// A page that is a list of snapshots: each press moves to the next one.
    /// Records everything done to it.
    struct ScriptedPage {
        pages: Vec<Snapshot>,
        at: usize,
        log: Vec<String>,
        stale_on_toggle: bool,
    }

    impl ScriptedPage {
        fn new(pages: Vec<Snapshot>) -> Self {
            Self {
                pages,
                at: 0,
                log: Vec::new(),
                stale_on_toggle: false,
            }
        }
    }

    impl BrowserPage for ScriptedPage {
        async fn open(&mut self, url: &str) -> Result<(), PageError> {
            self.log.push(format!("open {url}"));
            Ok(())
        }
        async fn snapshot(&mut self) -> Result<Snapshot, PageError> {
            Ok(self.pages[self.at.min(self.pages.len() - 1)].clone())
        }
        async fn click(&mut self, r: usize) -> Result<(), PageError> {
            self.log.push(format!("click {r}"));
            self.at += 1;
            Ok(())
        }
        async fn set_checked(&mut self, r: usize, on: bool) -> Result<(), PageError> {
            if self.stale_on_toggle {
                return Err(PageError::Stale);
            }
            self.log.push(format!("set {r} {on}"));
            Ok(())
        }
        async fn select(&mut self, r: usize, option: usize) -> Result<(), PageError> {
            self.log.push(format!("select {r} {option}"));
            Ok(())
        }
        async fn fill(&mut self, r: usize, text: &str) -> Result<(), PageError> {
            self.log.push(format!("fill {r} {text}"));
            Ok(())
        }
    }

    /// A model that answers from a list, and remembers what it was shown.
    struct Scripted {
        steps: Mutex<Vec<Option<Step>>>,
        seen: Mutex<Vec<(String, Vec<String>)>>,
    }

    impl Scripted {
        fn new(mut steps: Vec<Option<Step>>) -> Self {
            steps.reverse();
            Self {
                steps: Mutex::new(steps),
                seen: Mutex::new(Vec::new()),
            }
        }
    }

    impl Decide for Scripted {
        async fn decide(&self, o: &Observation<'_>) -> Option<Step> {
            self.seen
                .lock()
                .unwrap()
                .push((o.page.to_string(), o.history.to_vec()));
            // Past the end of the script the model keeps pressing 0, so a
            // test of the loop's own limits never ends by accident.
            self.steps
                .lock()
                .unwrap()
                .pop()
                .unwrap_or(Some(Step::Act(vec![Action::Click { r: 0 }])))
        }
    }

    const ERRAND: Errand<'static> = Errand {
        url: "https://click.shop.example/u?t=abc",
        sender: "news@shop.example",
        email: "me+shop@example.com",
    };

    fn prefs() -> Snapshot {
        snap(
            "Email preferences [0 checkbox on \"Deals\"] [1 checkbox on \"News\"] [2 email \"Email\" empty] [3 select \"How often\"] [4 button \"Save\"]",
            vec![
                control(Kind::Checkbox, "Deals"),
                control(Kind::Checkbox, "News"),
                control(Kind::Email, "Email"),
                control(Kind::Select, "How often"),
                control(Kind::Button, "Save"),
            ],
        )
    }

    #[tokio::test]
    async fn a_preference_center_is_switched_off_saved_and_confirmed() {
        let mut page =
            ScriptedPage::new(vec![prefs(), snap("Your preferences were saved.", vec![])]);
        let model = Scripted::new(vec![
            Some(Step::Act(vec![
                Action::SetChecked { r: 0, on: false },
                Action::SetChecked { r: 1, on: false },
                Action::FillEmail { r: 2 },
                Action::Select { r: 3, option: 2 },
                Action::Click { r: 4 },
            ])),
            Some(Step::Done),
        ]);
        assert_eq!(drive(&mut page, &model, &ERRAND).await, Driven::Done);
        assert_eq!(
            page.log,
            [
                "open https://click.shop.example/u?t=abc",
                "set 0 false",
                "set 1 false",
                "fill 2 me+shop@example.com",
                "select 3 2",
                "click 4",
            ],
            "the ORIGINAL link is opened, and the one typed string is the errand's address"
        );
        let seen = model.seen.lock().unwrap();
        assert!(seen[0].1.is_empty());
        assert_eq!(seen[1].0, "Your preferences were saved.");
        assert_eq!(
            seen[1].1,
            [
                "turned off 0 \"Deals\"",
                "turned off 1 \"News\"",
                "typed the user's address into 2 \"Email\"",
                "chose option 2 of 3 \"How often\"",
                "pressed 4 \"Save\"",
            ],
            "the next step is told what was done, and the address itself is not in it"
        );
    }

    #[test]
    fn the_page_decides_what_an_action_may_touch_not_the_model() {
        let s = snap(
            "",
            vec![
                control(Kind::Button, "Unsubscribe from all"),
                control(Kind::Password, "Password"),
                control(Kind::Text, "Full name"),
                Control {
                    email_like: true,
                    ..control(Kind::Text, "Your e-mail")
                },
                control(Kind::Select, "Frequency"),
                Control {
                    disabled: true,
                    ..control(Kind::Button, "Save")
                },
                control(Kind::Checkbox, "Deals"),
            ],
        );
        assert!(permit(&s, Action::Click { r: 0 }).is_ok());
        assert!(permit(&s, Action::Click { r: 6 }).is_ok());
        assert!(permit(&s, Action::FillEmail { r: 3 }).is_ok());
        assert!(permit(&s, Action::Select { r: 4, option: 2 }).is_ok());
        assert!(permit(&s, Action::SetChecked { r: 6, on: false }).is_ok());

        // The address goes into an email field and nowhere else.
        assert!(
            permit(&s, Action::FillEmail { r: 1 }).is_err(),
            "a password field"
        );
        assert!(
            permit(&s, Action::FillEmail { r: 2 }).is_err(),
            "a name field"
        );
        assert!(permit(&s, Action::FillEmail { r: 0 }).is_err(), "a button");
        // Numbers the page does not have, kinds that do not fit.
        assert!(permit(&s, Action::Click { r: 7 }).is_err());
        assert!(permit(&s, Action::Click { r: 2 }).is_err());
        assert!(permit(&s, Action::Select { r: 4, option: 3 }).is_err());
        assert!(permit(&s, Action::Select { r: 0, option: 0 }).is_err());
        assert!(permit(&s, Action::SetChecked { r: 0, on: true }).is_err());
        assert!(permit(&s, Action::Click { r: 5 }).is_err(), "disabled");
    }

    #[test]
    fn some_buttons_are_never_pressed_whoever_asks() {
        for label in [
            "Resubscribe",
            "Re-subscribe me",
            "Subscribe",
            "Undo",
            "Stay subscribed",
            "Sign in to manage",
            "Log in",
            "Create an account",
            "Delete my account",
            "Close account",
            "Download our app",
            "Upgrade",
        ] {
            let s = snap("", vec![control(Kind::Button, label)]);
            assert!(permit(&s, Action::Click { r: 0 }).is_err(), "{label}");
        }
        for label in [
            "Unsubscribe",
            "Unsubscribe from all",
            "Yes, unsubscribe me",
            "Save preferences",
            "Update my subscription preferences",
            "Confirm",
            "Reject all cookies",
            "Opt out",
        ] {
            let s = snap("", vec![control(Kind::Button, label)]);
            assert!(permit(&s, Action::Click { r: 0 }).is_ok(), "{label}");
        }
    }

    #[tokio::test]
    async fn a_refused_action_is_reported_and_the_rest_of_the_list_dropped() {
        let first = snap(
            "",
            vec![
                control(Kind::Button, "Resubscribe"),
                control(Kind::Button, "Unsubscribe"),
            ],
        );
        let mut page = ScriptedPage::new(vec![first, snap("Unsubscribed.", vec![])]);
        let model = Scripted::new(vec![
            Some(Step::Act(vec![
                Action::Click { r: 0 },
                Action::Click { r: 1 },
            ])),
            Some(Step::Act(vec![Action::Click { r: 1 }])),
            Some(Step::Done),
        ]);
        assert_eq!(drive(&mut page, &model, &ERRAND).await, Driven::Done);
        assert_eq!(page.log[1..], ["click 1"], "Resubscribe was never pressed");
        let seen = model.seen.lock().unwrap();
        assert!(seen[1].1[0].starts_with("REFUSED"), "{:?}", seen[1].1);
    }

    #[tokio::test]
    async fn a_press_ends_the_step_because_the_numbers_after_it_are_stale() {
        let two = || {
            snap(
                "",
                vec![
                    control(Kind::Button, "Next"),
                    control(Kind::Button, "Confirm"),
                ],
            )
        };
        let mut page = ScriptedPage::new(vec![two(), two(), snap("Done.", vec![])]);
        let model = Scripted::new(vec![
            Some(Step::Act(vec![
                Action::Click { r: 0 },
                Action::Click { r: 1 },
            ])),
            Some(Step::Done),
        ]);
        assert_eq!(drive(&mut page, &model, &ERRAND).await, Driven::Done);
        assert_eq!(page.log[1..], ["click 0"]);
    }

    #[tokio::test]
    async fn the_agents_reasons_come_back_as_they_were_given() {
        for why in [
            Raise::SignIn,
            Raise::Captcha,
            Raise::CheckEmail,
            Raise::YourCall,
        ] {
            let mut page = ScriptedPage::new(vec![prefs()]);
            let model = Scripted::new(vec![Some(Step::Raise(why))]);
            assert_eq!(drive(&mut page, &model, &ERRAND).await, Driven::Raise(why));
            assert_eq!(page.log.len(), 1, "nothing was touched");
        }
    }

    #[tokio::test]
    async fn no_model_is_lost_not_a_verdict_on_the_page() {
        let mut page = ScriptedPage::new(vec![prefs()]);
        let model = Scripted::new(vec![None]);
        assert_eq!(drive(&mut page, &model, &ERRAND).await, Driven::Lost);
    }

    #[tokio::test]
    async fn a_page_that_never_changes_is_given_up_on_before_the_steps_run_out() {
        // Pressing "Save" on a page that stays exactly as it was.
        struct Stuck(ScriptedPage);
        impl BrowserPage for Stuck {
            async fn open(&mut self, url: &str) -> Result<(), PageError> {
                self.0.open(url).await
            }
            async fn snapshot(&mut self) -> Result<Snapshot, PageError> {
                Ok(self.0.pages[0].clone())
            }
            async fn click(&mut self, r: usize) -> Result<(), PageError> {
                self.0.click(r).await
            }
            async fn set_checked(&mut self, r: usize, on: bool) -> Result<(), PageError> {
                self.0.set_checked(r, on).await
            }
            async fn select(&mut self, r: usize, o: usize) -> Result<(), PageError> {
                self.0.select(r, o).await
            }
            async fn fill(&mut self, r: usize, t: &str) -> Result<(), PageError> {
                self.0.fill(r, t).await
            }
        }
        let mut page = Stuck(ScriptedPage::new(vec![snap(
            "[0 button \"Save\"]",
            vec![control(Kind::Button, "Save")],
        )]));
        let model = Scripted::new(vec![]);
        assert_eq!(
            drive(&mut page, &model, &ERRAND).await,
            Driven::Raise(Raise::Unclear)
        );
        let presses = page.0.log.iter().filter(|l| l.starts_with("click")).count();
        assert_eq!(presses, 2, "two tries that change nothing, not eight");
    }

    #[tokio::test]
    async fn the_steps_are_counted_even_when_every_page_is_new() {
        let pages: Vec<Snapshot> = (0..20)
            .map(|i| snap(&format!("page {i}"), vec![control(Kind::Button, "Next")]))
            .collect();
        let mut page = ScriptedPage::new(pages);
        let model = Scripted::new(vec![]);
        assert_eq!(
            drive(&mut page, &model, &ERRAND).await,
            Driven::Raise(Raise::Unclear)
        );
        let presses = page.log.iter().filter(|l| l.starts_with("click")).count();
        assert_eq!(presses, MAX_STEPS);
    }

    #[tokio::test]
    async fn a_page_that_moves_under_a_toggle_gets_read_again() {
        let mut page = ScriptedPage::new(vec![prefs()]);
        page.stale_on_toggle = true;
        let model = Scripted::new(vec![
            Some(Step::Act(vec![
                Action::SetChecked { r: 0, on: false },
                Action::Click { r: 4 },
            ])),
            Some(Step::Raise(Raise::Unclear)),
        ]);
        assert_eq!(
            drive(&mut page, &model, &ERRAND).await,
            Driven::Raise(Raise::Unclear)
        );
        assert_eq!(
            page.log.len(),
            1,
            "Save was not pressed on a page we lost track of"
        );
        let seen = model.seen.lock().unwrap();
        assert!(seen[1].1.iter().any(|l| l.contains("the page had changed")));
    }

    #[tokio::test]
    async fn a_session_is_rented_with_recording_off_and_given_back() {
        use axum::extract::{Path, State};
        use axum::http::HeaderMap;
        use axum::routing::post;
        type Hits = Arc<Mutex<Vec<String>>>;
        let hits: Hits = Arc::new(Mutex::new(Vec::new()));
        let app = axum::Router::new()
            .route(
                "/v1/sessions",
                post(
                    |State(h): State<Hits>, headers: HeaderMap, body: String| async move {
                        let key = headers["x-bb-api-key"].to_str().unwrap().to_string();
                        h.lock().unwrap().push(format!("create {key} {body}"));
                        (
                            axum::http::StatusCode::CREATED,
                            axum::Json(
                                json!({ "id": "s1", "connectUrl": "wss://connect.example/?k=1" }),
                            ),
                        )
                    },
                ),
            )
            .route(
                "/v1/sessions/{id}",
                post(
                    |State(h): State<Hits>, Path(id): Path<String>, body: String| async move {
                        h.lock().unwrap().push(format!("update {id} {body}"));
                        axum::Json(json!({}))
                    },
                ),
            )
            .with_state(hits.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let bb = Browserbase::new("bb_key".into(), Some("proj".into()), None)
            .unwrap()
            .with_api_base(&format!("http://{addr}"));
        let session = bb.open().await.expect("rented");
        assert_eq!(session.id, "s1");
        bb.release(&session).await;

        let hits = hits.lock().unwrap();
        assert!(hits[0].starts_with("create bb_key "), "{hits:?}");
        let sent: serde_json::Value =
            serde_json::from_str(hits[0].splitn(3, ' ').nth(2).unwrap()).unwrap();
        assert_eq!(sent["browserSettings"]["recordSession"], false);
        assert_eq!(sent["browserSettings"]["logSession"], false);
        assert_eq!(sent["browserSettings"]["solveCaptchas"], false);
        assert_eq!(sent["projectId"], "proj");
        assert!(hits[1].starts_with("update s1 "), "{hits:?}");
        assert!(hits[1].contains("REQUEST_RELEASE"));
        assert!(
            !format!("{bb:?}").contains("bb_key"),
            "the key stays out of Debug"
        );
    }

    /// THE REAL BROWSERBASE: rent a browser, attach, read a page, give it
    /// back. Spends a browser-minute on the account, so it runs only by hand:
    /// `BROWSERBASE_API_KEY=... cargo test -p squelch-api live_browserbase -- --ignored --nocapture`
    #[tokio::test]
    #[ignore = "needs a Browserbase key; rents a real browser"]
    async fn live_browserbase_rents_a_browser_we_can_read_a_page_in() {
        use crate::unsub_cdp::{Cdp, CdpPage};
        let bb = Browserbase::from_env().expect("a Browserbase key in the environment");
        let session = bb.open().await.expect("a session was rented");
        let read = async {
            let cdp = Cdp::connect(&session.connect_url).await?;
            let mut page = CdpPage::attach(cdp).await?;
            page.open("https://example.com/").await?;
            page.snapshot().await
        }
        .await;
        bb.release(&session).await;
        let snap = read.expect("the page was read");
        eprintln!("--- {} ---\n{}\n---", snap.url, snap.text);
        assert!(snap.url.starts_with("https://example.com"), "{}", snap.url);
        assert!(
            snap.text.contains("documentation examples"),
            "{}",
            snap.text
        );
        assert!(snap.controls.iter().any(|c| c.kind == Kind::Link));
    }
}
