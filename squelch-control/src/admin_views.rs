//! Every HTML byte behind the admin token, and the doors in front of it.
//!
//! THE CONSOLE IS AN APP WITH NO SCRIPT. A sidebar down the left names every
//! page and carries the live counts; the pages beside it are server-rendered
//! documents. The CSP is `default-src 'none'` with inline style only (see
//! [`crate::pages`]), so everything that feels like an app here is plain web:
//! a filter is a link, a search is a GET form, a button is a POST form, and an
//! action answers with a redirect back to the page it was pressed on, carrying
//! a `done` code that page turns into a banner.
//!
//! WHERE AN ACTION RETURNS TO rides in every form as a hidden [`NEXT_FIELD`].
//! It is never used as given: [`Place::parse`] reads it into one of the pages
//! below and [`Place::href`] writes that page's URL fresh, so the `Location`
//! this service sends is always one it built. Anything that does not parse is
//! the overview.
//!
//! EVERYTHING INTERPOLATED IS ESCAPED, including strings that were validated
//! elsewhere. The one this page would break on is the name off the public
//! waitlist form, and the rule has no exceptions for better-behaved sources.
//!
//! PRIVACY: a waitlist address is shown here and nowhere else, and no URL this
//! module builds carries one. A search term is the operator's own and does.

use axum::{http::StatusCode, response::Response};
use chrono::{DateTime, Utc};
use squelch_core::features::Feature;

use crate::pages::{console, console_app, escape_html, percent_encode};
use crate::store::{FeatureBoard, USER_APPROVED_LIMIT, USER_PENDING, UserRow};

/// The hidden field that says where an action came from. See [`Place`].
pub const NEXT_FIELD: &str = "next";

/// The hidden field that says the operator answered the question on
/// [`admin_confirm`]. Lives here so the page that writes it and the handlers
/// that read it cannot drift to two different spellings.
pub const CONFIRM_FIELD: &str = "confirmed";

/// The longest search term a URL is rebuilt with. A search box is for an
/// address or a name, and this bounds the work a long query buys.
const MAX_QUERY: usize = 120;

/// How many waiting rows the overview shows before it points at the full list.
const OVERVIEW_WAITING: usize = 6;

/// How many lines of recent activity the overview shows.
const OVERVIEW_FEED: usize = 10;

// ---------------------------------------------------------------------------
// The lifecycle.

/// Where one person is in the funnel, as the board reads it.
///
/// READ AS PRECEDENCE, NOT HISTORY: somebody active also signed up, was
/// invited, and was approved, and the row has room for one answer. The answer
/// worth having is the furthest they got.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// On the waitlist, not approved.
    Waiting,
    /// Approved, and the invite email never went out. The one state on this
    /// board that is WRONG rather than early, and it is drawn loud.
    Unsent,
    /// Approved and mailed; the code is outstanding.
    Invited,
    /// The code was spent and a mailbox exists.
    SignedUp,
    /// A device that is not the console paired with that mailbox.
    Active,
}

impl Stage {
    pub fn of(r: &UserRow) -> Stage {
        if r.status == USER_PENDING {
            Stage::Waiting
        } else if r.first_paired_at.is_some() {
            Stage::Active
        } else if r.signed_up_at.is_some() {
            Stage::SignedUp
        } else if r.notified_at.is_some() {
            Stage::Invited
        } else {
            Stage::Unsent
        }
    }

    fn word(self) -> &'static str {
        match self {
            Stage::Waiting => "waiting",
            Stage::Unsent => "email not sent",
            Stage::Invited => "invited",
            Stage::SignedUp => "signed up",
            Stage::Active => "active",
        }
    }

    /// The stylesheet's name for this stage's colour.
    fn tone(self) -> &'static str {
        match self {
            Stage::Waiting => "wait",
            Stage::Unsent => "flag",
            Stage::Invited => "sent",
            Stage::SignedUp => "done",
            Stage::Active => "live",
        }
    }

    fn pill(self) -> String {
        format!(
            r#"<span class="pill {}">{}</span>"#,
            self.tone(),
            self.word()
        )
    }
}

/// Which slice of the people list a page shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Filter {
    #[default]
    All,
    Waiting,
    /// Approved and not yet signed up, sent or not.
    Invited,
    /// Signed up, active or not.
    Members,
}

impl Filter {
    const EVERY: [Filter; 4] = [
        Filter::All,
        Filter::Waiting,
        Filter::Invited,
        Filter::Members,
    ];

    pub fn parse(s: &str) -> Filter {
        match s {
            "waiting" => Filter::Waiting,
            "invited" => Filter::Invited,
            "members" => Filter::Members,
            _ => Filter::All,
        }
    }

    fn key(self) -> &'static str {
        match self {
            Filter::All => "all",
            Filter::Waiting => "waiting",
            Filter::Invited => "invited",
            Filter::Members => "members",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Filter::All => "Everyone",
            Filter::Waiting => "Waiting",
            Filter::Invited => "Invited",
            Filter::Members => "Members",
        }
    }

    fn lede(self) -> &'static str {
        match self {
            Filter::All => "Everybody on the list, newest approvals after the queue.",
            Filter::Waiting => {
                "Oldest first. Approving mints one invite code and emails it to that address."
            }
            Filter::Invited => {
                "Approved, and the code is not spent yet. A row that says the email was not sent is asking for its button."
            }
            Filter::Members => "Somebody spent their code and a mailbox exists.",
        }
    }

    fn empty(self) -> &'static str {
        match self {
            Filter::All => "Nobody is on the list yet.",
            Filter::Waiting => "Nobody is waiting.",
            Filter::Invited => "No invites are outstanding.",
            Filter::Members => "Nobody has signed up yet.",
        }
    }

    fn admits(self, stage: Stage) -> bool {
        match self {
            Filter::All => true,
            Filter::Waiting => stage == Stage::Waiting,
            Filter::Invited => matches!(stage, Stage::Unsent | Stage::Invited),
            Filter::Members => matches!(stage, Stage::SignedUp | Stage::Active),
        }
    }

    fn count(self, c: &Counts) -> usize {
        match self {
            Filter::All => c.waiting + c.approved,
            Filter::Waiting => c.waiting,
            Filter::Invited => c.invited,
            Filter::Members => c.signed_up,
        }
    }
}

/// The funnel, counted once per page for the sidebar and the scope.
///
/// "OF THOSE" NESTS, so an active person is counted in BOTH `signed_up` and
/// `active`: everybody active signed up. Partitioning them instead would make
/// "3 signed up, 2 active" read as five people.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub waiting: usize,
    /// Everybody past the queue, whatever happened next.
    pub approved: usize,
    pub unsent: usize,
    /// Approved and not signed up: `unsent` plus the ones that went out.
    pub invited: usize,
    pub signed_up: usize,
    pub active: usize,
    /// The approved half hit the store's listing cap, so older rows exist that
    /// this page is not showing.
    pub capped: bool,
}

impl Counts {
    pub fn of(rows: &[UserRow]) -> Counts {
        let mut c = Counts::default();
        for r in rows {
            match Stage::of(r) {
                Stage::Waiting => c.waiting += 1,
                Stage::Unsent => {
                    c.unsent += 1;
                    c.invited += 1;
                }
                Stage::Invited => c.invited += 1,
                Stage::SignedUp => c.signed_up += 1,
                Stage::Active => {
                    c.signed_up += 1;
                    c.active += 1;
                }
            }
        }
        c.approved = rows.len() - c.waiting;
        c.capped = i64::try_from(c.approved).unwrap_or(i64::MAX) >= USER_APPROVED_LIMIT;
        c
    }
}

// ---------------------------------------------------------------------------
// Where the operator is.

/// One page behind the door. The sidebar highlights it, and every form on it
/// carries it as [`NEXT_FIELD`] so the action can come back here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Place {
    Overview,
    People { filter: Filter, q: String },
    Person(i64),
    Invite,
    Features,
}

impl Place {
    /// This page's URL, built from its parts. The search term is
    /// percent-encoded, so nothing in it can end the parameter it sits in.
    pub fn href(&self) -> String {
        match self {
            Place::Overview => "/admin".to_string(),
            Place::People { filter, q } => {
                let mut href = "/admin/people".to_string();
                let mut sep = '?';
                if *filter != Filter::All {
                    href.push_str(&format!("{sep}stage={}", filter.key()));
                    sep = '&';
                }
                if !q.is_empty() {
                    href.push_str(&format!("{sep}q={}", percent_encode(q)));
                }
                href
            }
            Place::Person(id) => format!("/admin/people/{id}"),
            Place::Invite => "/admin/invite".to_string(),
            Place::Features => "/admin/features".to_string(),
        }
    }

    /// The same URL with an action's outcome on it, for the redirect.
    pub fn href_done(&self, done: Done) -> String {
        let href = self.href();
        let sep = if href.contains('?') { '&' } else { '?' };
        format!("{href}{sep}done={}", done.key())
    }

    /// A `next` field read back into a page, or `None` for anything that is not
    /// one of them. Never trusted as a URL: see the module docs.
    pub fn parse(next: &str) -> Option<Place> {
        let (path, query) = next.split_once('?').unwrap_or((next, ""));
        match path {
            "/admin" => Some(Place::Overview),
            "/admin/people" => {
                let (filter, q) = people_query(query);
                Some(Place::People { filter, q })
            }
            "/admin/invite" => Some(Place::Invite),
            "/admin/features" => Some(Place::Features),
            _ => {
                let id = path.strip_prefix("/admin/people/")?;
                // Digits only: `+1` and ` 1` parse as an i64 and are not a
                // path this page ever wrote.
                if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) {
                    return None;
                }
                id.parse().ok().map(Place::Person)
            }
        }
    }
}

/// The people list's filter and search term, out of a query string.
pub fn people_query(query: &str) -> (Filter, String) {
    let mut filter = Filter::All;
    let mut q = String::new();
    for (k, v) in url::form_urlencoded::parse(query.as_bytes()) {
        match k.as_ref() {
            "stage" => filter = Filter::parse(&v),
            "q" => q = v.trim().chars().take(MAX_QUERY).collect(),
            _ => {}
        }
    }
    (filter, q)
}

/// What an action did, as the code its redirect carries and the banner the page
/// it lands on shows. Only these: a code that is not one of them shows nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Done {
    Approved,
    Invited,
    Sent,
    /// The mint landed and the mail did not. The row says so too.
    Unsent,
}

impl Done {
    fn key(self) -> &'static str {
        match self {
            Done::Approved => "approved",
            Done::Invited => "invited",
            Done::Sent => "sent",
            Done::Unsent => "unsent",
        }
    }

    /// The `done` value off a query string, if it is one.
    pub fn from_query(query: &str) -> Option<Done> {
        url::form_urlencoded::parse(query.as_bytes())
            .find(|(k, _)| k == "done")
            .and_then(|(_, v)| match v.as_ref() {
                "approved" => Some(Done::Approved),
                "invited" => Some(Done::Invited),
                "sent" => Some(Done::Sent),
                "unsent" => Some(Done::Unsent),
                _ => None,
            })
    }

    fn sentence(self) -> &'static str {
        match self {
            Done::Approved => "Approved, and the invite is on its way.",
            Done::Invited => "Invited, and the code is on its way.",
            Done::Sent => "A fresh invite is on its way. The old code no longer works.",
            Done::Unsent => {
                "The invite was minted, but the email did not go out. The row says so, and its button tries again."
            }
        }
    }
}

/// The line above a page's content: what went wrong, or what just went right.
#[derive(Clone, Copy, Debug)]
pub enum Banner<'a> {
    Stop(&'a str),
    Done(Done),
    Saved,
}

impl<'a> Banner<'a> {
    pub fn stop(error: Option<&'a str>) -> Option<Banner<'a>> {
        error.map(Banner::Stop)
    }
}

/// `role="alert"` on a refusal because a form post here is a whole new
/// document: without it, a screen reader lands back at the top of a page that
/// looks like the one just submitted, with the sentence that changed sitting
/// silently in the middle of it.
fn banner(b: Option<Banner>) -> String {
    match b {
        None => String::new(),
        Some(Banner::Stop(e)) => format!(
            r#"<p class="stop" role="alert"><strong>{}</strong></p>"#,
            escape_html(e)
        ),
        Some(Banner::Done(Done::Unsent)) => format!(
            r#"<p class="stop" role="alert"><strong>{}</strong></p>"#,
            escape_html(Done::Unsent.sentence())
        ),
        Some(Banner::Done(d)) => format!(
            r#"<p class="ok" role="status">{}</p>"#,
            escape_html(d.sentence())
        ),
        Some(Banner::Saved) => r#"<p class="ok" role="status">Saved.</p>"#.to_string(),
    }
}

// ---------------------------------------------------------------------------
// The frame.

/// A sixteen-unit stroke icon. Markup rather than a fetch, like the mark.
fn icon(paths: &str) -> String {
    format!(
        r#"<svg class="ic" viewBox="0 0 16 16" aria-hidden="true" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round">{paths}</svg>"#
    )
}

const IC_OVERVIEW: &str = r#"<rect x="2" y="2" width="5" height="5" rx="1.2"/><rect x="9" y="2" width="5" height="5" rx="1.2"/><rect x="2" y="9" width="5" height="5" rx="1.2"/><rect x="9" y="9" width="5" height="5" rx="1.2"/>"#;
const IC_WAITING: &str = r#"<circle cx="8" cy="8" r="6"/><path d="M8 4.75V8l2.25 1.5"/>"#;
const IC_INVITED: &str = r#"<rect x="1.75" y="3.5" width="12.5" height="9" rx="1.5"/><path d="M2.5 4.5 8 8.75l5.5-4.25"/>"#;
const IC_MEMBERS: &str = r#"<circle cx="8" cy="5.5" r="2.75"/><path d="M2.75 13.5c.6-2.6 2.75-4 5.25-4s4.65 1.4 5.25 4"/>"#;
const IC_EVERYONE: &str = r#"<circle cx="6" cy="5.75" r="2.25"/><path d="M1.75 13c.5-2.2 2.2-3.4 4.25-3.4s3.75 1.2 4.25 3.4"/><path d="M10.5 3.6a2.25 2.25 0 0 1 0 4.3M12.1 9.9c1.15.5 1.95 1.6 2.2 3.1"/>"#;
const IC_INVITE: &str = r#"<circle cx="6.5" cy="5.5" r="2.5"/><path d="M1.75 13.25c.55-2.4 2.45-3.75 4.75-3.75 1.3 0 2.45.4 3.3 1.15"/><path d="M12.5 8.5v5M10 11h5"/>"#;
const IC_FEATURES: &str =
    r#"<rect x="1.5" y="4.5" width="13" height="7" rx="3.5"/><circle cx="11" cy="8" r="2"/>"#;
const IC_SIGNOUT: &str =
    r#"<path d="M6.5 2.5h-3a1 1 0 0 0-1 1v9a1 1 0 0 0 1 1h3"/><path d="M10 5l3 3-3 3M13 8H6"/>"#;

/// The sidebar: every page, the counts beside the ones that have them, and the
/// way out at the foot.
///
/// `counts` is `None` when the store did not answer, and then there are no
/// badges at all rather than zeroes: "0 waiting" is a claim.
fn sidebar(here: &Place, counts: Option<&Counts>) -> String {
    let current = |p: &Place| -> bool {
        match (p, here) {
            (Place::People { filter: a, .. }, Place::People { filter: b, .. }) => a == b,
            // A person's page sits under "Everyone", the list that holds them
            // whatever their stage.
            (Place::People { filter, .. }, Place::Person(_)) => *filter == Filter::All,
            (a, b) => a == b,
        }
    };
    let link = |p: Place, ic: &str, label: &str, badges: String| -> String {
        let aria = if current(&p) {
            r#" aria-current="page""#
        } else {
            ""
        };
        format!(
            r#"<a href="{href}"{aria}>{icon}<span class="lbl">{label}</span>{badges}</a>"#,
            href = escape_html(&p.href()),
            icon = icon(ic),
        )
    };
    let badge = |n: usize, tone: &str, title: &str| -> String {
        format!(
            r#"<span class="badge {tone}" title="{title}">{n}</span>"#,
            title = escape_html(title)
        )
    };
    let people = |filter: Filter| Place::People {
        filter,
        q: String::new(),
    };
    let (waiting, invited, members) = match counts {
        None => (String::new(), String::new(), String::new()),
        Some(c) => {
            let waiting = badge(
                c.waiting,
                if c.waiting > 0 { "wait" } else { "" },
                "waiting",
            );
            let mut invited = badge(c.invited, "", "invited, not signed up");
            if c.unsent > 0 {
                invited.push_str(&badge(c.unsent, "flag", "email not sent"));
            }
            (waiting, invited, badge(c.signed_up, "", "signed up"))
        }
    };
    format!(
        r#"<nav class="side-nav" aria-label="Admin">
<p class="nav-h">Funnel</p>
{overview}
{waiting}
{invited}
{members}
{everyone}
<p class="nav-h">Manage</p>
{invite}
{features}
</nav>
<div class="side-foot">
<form method="post" action="/admin/logout"><button type="submit" class="quiet">{signout}Sign out</button></form>
<p class="hint">This session lasts {days} days.</p>
</div>"#,
        overview = link(Place::Overview, IC_OVERVIEW, "Overview", String::new()),
        waiting = link(people(Filter::Waiting), IC_WAITING, "Waiting", waiting),
        invited = link(people(Filter::Invited), IC_INVITED, "Invited", invited),
        members = link(people(Filter::Members), IC_MEMBERS, "Members", members),
        everyone = link(people(Filter::All), IC_EVERYONE, "Everyone", String::new()),
        invite = link(Place::Invite, IC_INVITE, "Invite someone", String::new()),
        features = link(Place::Features, IC_FEATURES, "Features", String::new()),
        signout = icon(IC_SIGNOUT),
        days = crate::cookie::ADMIN_COOKIE_TTL_SECS / (24 * 60 * 60),
    )
}

/// The heading every page opens with: a small label, the title, an optional
/// sentence, and the page's own buttons to the right.
///
/// `hero` is the overview's: the big heading with the glitch the operator
/// asked to keep. Every other page gets the plain one.
fn page_head(eyebrow: &str, title: &str, lede: Option<&str>, actions: &str, hero: bool) -> String {
    let title = escape_html(title);
    let (class, h1) = if hero {
        (
            "page-head hero",
            format!(r#"<h1 data-text="{title}">{title}</h1>"#),
        )
    } else {
        ("page-head", format!("<h1>{title}</h1>"))
    };
    let lede = lede
        .map(|l| format!(r#"<p class="lede">{}</p>"#, escape_html(l)))
        .unwrap_or_default();
    let actions = if actions.is_empty() {
        String::new()
    } else {
        format!(r#"<div class="head-actions">{actions}</div>"#)
    };
    format!(
        r#"<header class="{class}"><div class="head-text"><p class="eyebrow">{eyebrow}</p>{h1}{lede}</div>{actions}</header>"#,
        eyebrow = escape_html(eyebrow),
    )
}

/// The primary way to invite, as a link styled like a button. It goes to a page
/// rather than posting, so it carries no form.
fn invite_button() -> String {
    format!(
        r#"<a class="button" href="/admin/invite">{}Invite someone</a>"#,
        icon(IC_INVITE)
    )
}

/// A panel: a lit dot in its tone, a label, a count, and a hint.
fn panel(tone: &str, title: &str, count: Option<usize>, extra: &str, body: &str) -> String {
    let count = count
        .map(|n| format!(r#"<span class="count">{n}</span>"#))
        .unwrap_or_default();
    format!(
        r#"<section class="rack {tone}">
<div class="rack-head"><span class="led"></span><h2>{title}</h2>{count}{extra}</div>
{body}
</section>"#,
        title = escape_html(title),
    )
}

// ---------------------------------------------------------------------------
// Rows.

/// The "who" cell: the address this person is known by, linked to their page;
/// when the Google account that actually signed up is a DIFFERENT address, a
/// muted line naming that too; and last, the name they typed into the form.
///
/// THE LINES ARE ORDERED BY AUTHORITY, AND THE NAME IS QUOTED. The name arrives
/// as free text on a public form, and it once rendered in the same muted line
/// as the `signed up as` provenance, so a stranger who submitted the name
/// `signed up as ada@example.com` produced a row byte-for-byte identical to a
/// genuine Google-verified one. So the page asserts first and quotes last: the
/// curly quotes are the frame, and `store::normalize_name` strips both of those
/// characters out of every stored name, which is what makes the frame something
/// free text cannot spell.
///
/// AN INVITE IS A BEARER CODE, so the two addresses are not promised to match.
/// The provenance line shows only on a mismatch: the same address twice on
/// every row would train the eye to skip the line that matters.
fn who_cell(r: &UserRow) -> String {
    let mut cell = format!(
        r#"<a class="person" href="/admin/people/{id}">{email}</a>"#,
        id = r.id,
        email = escape_html(&r.email),
    );
    if let Some(account) = r.account_email.as_deref()
        && account != r.email
    {
        cell.push_str(&format!(
            r#"<br><span class="muted">signed up as {}</span>"#,
            escape_html(account)
        ));
    }
    if let Some(name) = r.name.as_deref() {
        cell.push_str(&format!(
            "<br><span class=\"given\">\u{201c}{}\u{201d}</span>",
            escape_html(name)
        ));
    }
    cell
}

/// The mailbox a row became, as a `<code>` label, or nothing at all.
fn tenant_code(r: &UserRow) -> String {
    r.tenant_label
        .as_deref()
        .map(|l| format!(" <code>{}</code>", escape_html(l)))
        .unwrap_or_default()
}

/// The stage cell: the pill, and the one fact that goes with it.
fn stage_cell(r: &UserRow) -> String {
    let stage = Stage::of(r);
    let fact = match stage {
        Stage::Waiting | Stage::Unsent => String::new(),
        Stage::Invited => r
            .notified_at
            .map(|at| format!(r#" <span class="muted">{}</span>"#, day(at)))
            .unwrap_or_default(),
        Stage::SignedUp | Stage::Active => tenant_code(r),
    };
    format!("{}{fact}", stage.pill())
}

/// One row's button, as its own form. No JavaScript here, so a button that acts
/// is a form that posts, and the row it acts on and the page it was pressed on
/// ride in hidden fields.
fn action_form(action: &str, id: i64, label: &str, quiet: bool, back: &Place) -> String {
    let class = if quiet { r#" class="quiet""# } else { "" };
    format!(
        r#"<form method="post" action="{action}"><input type="hidden" name="id" value="{id}"><input type="hidden" name="{NEXT_FIELD}" value="{next}"><button type="submit"{class}>{label}</button></form>"#,
        action = escape_html(action),
        next = escape_html(&back.href()),
        label = escape_html(label),
    )
}

/// What a row offers, by stage.
///
/// NOTHING ON A ROW THAT BECAME A MAILBOX. `admin::send` refuses a spent code
/// because a second code for somebody who already has a mailbox is a tenant
/// nobody approved, so a button there could only ever fail, and pressing it was
/// once the only way to learn they had signed up.
fn row_action(r: &UserRow, back: &Place) -> String {
    match Stage::of(r) {
        // "Approve", not "Approve and email invite": the sentence above the
        // list says it once, and the button is a word.
        Stage::Waiting => action_form("/admin/approve", r.id, "Approve", true, back),
        // Loud, and the only row action on the board that is.
        Stage::Unsent => action_form("/admin/send", r.id, "Send invite", false, back),
        Stage::Invited => action_form("/admin/send", r.id, "Re-send", true, back),
        Stage::SignedUp | Stage::Active => String::new(),
    }
}

/// A list of people, or a sentence saying there is nobody to list.
fn people_table(rows: &[&UserRow], back: &Place, show_stage: bool, empty: &str) -> String {
    if rows.is_empty() {
        return format!(r#"<p class="muted empty">{}</p>"#, escape_html(empty));
    }
    let body: String = rows
        .iter()
        .map(|r| {
            let stage = if show_stage {
                format!(r#"<td class="stage">{}</td>"#, stage_cell(r))
            } else {
                String::new()
            };
            format!(
                r#"<tr class="{tone}"><td class="who">{who}</td>{stage}<td class="when">{joined}</td><td class="act">{action}</td></tr>
"#,
                tone = Stage::of(r).tone(),
                who = who_cell(r),
                joined = day(r.created_at),
                action = row_action(r, back),
            )
        })
        .collect();
    let stage_head = if show_stage { "<th>Stage</th>" } else { "" };
    format!(
        r#"<table class="people"><tr><th>Who</th>{stage_head}<th>Joined</th><th><span class="sr">Action</span></th></tr>
{body}</table>"#
    )
}

/// Whether a row answers a search. Every field a person could be found by:
/// the address, the account that signed up, the name, and the mailbox.
fn matches(r: &UserRow, q: &str) -> bool {
    if q.is_empty() {
        return true;
    }
    let q = q.to_lowercase();
    [
        Some(r.email.as_str()),
        r.account_email.as_deref(),
        r.name.as_deref(),
        r.tenant_label.as_deref(),
    ]
    .into_iter()
    .flatten()
    .any(|f| f.to_lowercase().contains(&q))
}

/// A date as the operator reads it. Escaped like everything else: an exception
/// for "this one is only ever digits" is how the rule stops being one.
fn day(ts: DateTime<Utc>) -> String {
    escape_html(&ts.format("%Y-%m-%d").to_string())
}

// ---------------------------------------------------------------------------
// The doors.

/// The admin door: one field, and nothing that describes what is behind it.
///
/// Anybody can ask for `/admin`, so this page says as little as a page can. It
/// names no waitlist, no counts, and no operator.
///
/// `error` is present only on a refusal, so it decides the status too: a
/// message rendered here means the request was turned away, and a 200 with a
/// "not accepted" sentence in it is a lie to everything that reads status
/// codes.
pub fn admin_login(error: Option<&str>) -> Response {
    let status = if error.is_some() {
        StatusCode::UNAUTHORIZED
    } else {
        StatusCode::OK
    };
    admin_door(status, &banner(Banner::stop(error)))
}

/// The door after a DELIBERATE sign out, which is not a refusal and must not be
/// dressed as one: a 200 and a quiet note, where [`admin_login`] answers 401
/// with a red banner.
pub fn admin_signed_out() -> Response {
    admin_door(StatusCode::OK, r#"<p class="hint">Signed out.</p>"#)
}

fn admin_door(status: StatusCode, note: &str) -> Response {
    console(
        status,
        "Passband admin",
        &format!(
            r#"<h1>Passband admin</h1>
{note}
<form method="post" action="/admin/login">
<label for="token">Admin token</label>
<input type="password" id="token" name="token" autocomplete="current-password"
  autocapitalize="off" spellcheck="false" required>
<button type="submit">Sign in</button>
</form>"#
        ),
    )
}

/// The CSRF refusal, naming the origin this deployment answers to and the one
/// the request actually stated.
///
/// The operator meets this when an extension, a proxy, or a sandboxed frame
/// rewrites those headers under an address bar that reads correctly, so it says
/// what was expected and what arrived. The attacker's half of that is a request
/// they wrote themselves.
pub fn admin_cross_origin(expected: &str, report: &str) -> Response {
    console(
        StatusCode::FORBIDDEN,
        "Passband admin",
        &format!(
            r#"<h1>That request did not come from this site</h1>
<p>This page only accepts form posts made by a page loaded from
<code>{expected}</code>, and this one stated something else. Nothing was
changed.</p>
<p class="muted">What arrived: <code>{report}</code></p>
<p class="muted">A browser on this origin sends its own address here. Something
between the form and this service rewrote it, which is usually an extension, a
proxy, or the page running inside a sandboxed frame. Opening
<code>{expected}/admin</code> in an ordinary tab is the fix.</p>"#,
            expected = escape_html(expected.trim_end_matches('/')),
            report = escape_html(report),
        ),
    )
}

/// The one question the operator answers before any invite leaves the building.
///
/// A DOCUMENT AND NOT A `confirm()`, because the CSP has no `script-src` at
/// all. It is a door rather than an app page on purpose: an interruption with
/// a sidebar beside it is one the eye can wander off.
///
/// WHY THE QUESTION IS WORTH A WHOLE PAGE. The OAuth client is not verified yet
/// (`docs/VERIFICATION.md`), so Google refuses consent to any account that is
/// not on the project's test-user list. An invite mailed to somebody who is not
/// on that list is an invite to a wall, and the first anyone hears of it is the
/// person writing back.
///
/// NOTHING HAS HAPPENED YET when this renders. Every caller checks for the
/// confirmation BEFORE it approves, mints, revokes, or mails, so cancelling here
/// is genuinely a no-op. `back` is where both buttons lead: the confirmation
/// carries it on as [`NEXT_FIELD`], and Cancel is a link to it.
pub fn admin_confirm(
    action: &str,
    hidden: &[(&str, &str)],
    who: &str,
    confirm_label: &str,
    back: &Place,
) -> Response {
    let next = back.href();
    let fields: String = hidden
        .iter()
        .chain(std::iter::once(&(NEXT_FIELD, next.as_str())))
        .map(|(name, value)| {
            format!(
                r#"<input type="hidden" name="{}" value="{}">"#,
                escape_html(name),
                escape_html(value),
            )
        })
        .collect();
    console(
        StatusCode::OK,
        "One thing first",
        &format!(
            r#"<h1>One thing first</h1>
<p class="stop"><strong>Have you added this person to the Google
test user list?</strong></p>
<p>{who}</p>
<p class="hint">Passband's OAuth client is not verified yet, so Google will not
let an account finish consent unless that account is on the project's test-user
list. Mailing an invite to somebody who is not on it sends them to a wall, and
the code is spent by the time they tell you. Add them in the Google Cloud
console first, then come back and press this.</p>
<form method="post" action="{action}">{fields}<input type="hidden" name="{CONFIRM_FIELD}" value="yes">
<button type="submit">{confirm_label}</button>
</form>
<p class="hint"><a href="{cancel}">Cancel and go back.</a> Nothing has been
approved, minted, or mailed.</p>"#,
            who = escape_html(who),
            action = escape_html(action),
            confirm_label = escape_html(confirm_label),
            cancel = escape_html(&next),
        ),
    )
}

// ---------------------------------------------------------------------------
// The pages.

/// `rows` is `None` when the store did not answer. Every page below then shows
/// its banner and NO list: "nobody is waiting" with no explanation is the one
/// wrong thing to show an operator.
fn unavailable() -> String {
    r#"<p class="muted empty">Nothing to show until the store answers.</p>"#.to_string()
}

/// The overview: the funnel as the scope draws it, the queue, anything broken,
/// and what happened lately.
///
/// ALWAYS 200, even with a banner. The lists under it are correct either way,
/// and the banner says what a button did NOT do; that is a finished page
/// describing a real state, not a failed request.
pub fn admin_overview(rows: Option<&[UserRow]>, flash: Option<Banner>) -> Response {
    let here = Place::Overview;
    let counts = rows.map(Counts::of);
    let head = page_head("Waitlist", "Overview", None, &invite_button(), true);
    let content = match rows {
        None => unavailable(),
        Some(rows) => {
            let c = counts.unwrap_or_default();
            let waiting: Vec<&UserRow> = rows
                .iter()
                .filter(|r| Stage::of(r) == Stage::Waiting)
                .collect();
            let unsent: Vec<&UserRow> = rows
                .iter()
                .filter(|r| Stage::of(r) == Stage::Unsent)
                .collect();
            let shown = &waiting[..waiting.len().min(OVERVIEW_WAITING)];
            let more = if waiting.len() > shown.len() {
                format!(
                    r#"<p class="more"><a href="{}">See all {} waiting</a></p>"#,
                    escape_html(
                        &Place::People {
                            filter: Filter::Waiting,
                            q: String::new()
                        }
                        .href()
                    ),
                    waiting.len(),
                )
            } else {
                String::new()
            };
            let queue = panel(
                "wait",
                "Waiting",
                Some(waiting.len()),
                &format!(
                    r#"<p class="hint">Oldest first. Approving mints one invite code and emails it. The code works once and expires in {} days.</p>"#,
                    crate::invites::DEFAULT_TTL_DAYS
                ),
                &format!(
                    "{}{more}",
                    people_table(shown, &here, false, "Nobody is waiting.")
                ),
            );
            let broken = panel(
                if unsent.is_empty() { "" } else { "flag" },
                "Email not sent",
                Some(unsent.len()),
                r#"<p class="hint">Approved, and nobody was told.</p>"#,
                &people_table(&unsent, &here, false, "Every approved invite went out."),
            );
            format!(
                r#"{scope}
<div class="cols">
{queue}
{broken}
</div>
{feed}"#,
                scope = scope(&c),
                feed = panel("", "Recent activity", None, "", &feed(rows)),
            )
        }
    };
    console_app(
        StatusCode::OK,
        "Overview",
        &sidebar(&here, counts.as_ref()),
        &format!("{head}\n{}\n{content}", banner(flash)),
    )
}

/// One thing that happened to one person, for the overview's feed.
struct Event<'a> {
    at: DateTime<Utc>,
    row: &'a UserRow,
    stage: Stage,
    /// Markup: authored words, and anything from the row escaped.
    what: String,
}

/// The latest stamps across every row, newest first.
///
/// One event per stamp, and the invite is ONE event whether or not it was
/// mailed: approval and delivery land within a second of each other, and two
/// lines for one press is a feed that says everything twice.
fn feed(rows: &[UserRow]) -> String {
    let mut events: Vec<Event> = Vec::new();
    for r in rows {
        if r.status == USER_PENDING {
            events.push(Event {
                at: r.created_at,
                row: r,
                stage: Stage::Waiting,
                what: "joined the waitlist".to_string(),
            });
            continue;
        }
        match (r.notified_at, r.approved_at) {
            (Some(at), _) => events.push(Event {
                at,
                row: r,
                stage: Stage::Invited,
                what: "was invited".to_string(),
            }),
            (None, Some(at)) => events.push(Event {
                at,
                row: r,
                stage: Stage::Unsent,
                what: "was approved, and the email did not go out".to_string(),
            }),
            (None, None) => {}
        }
        if let Some(at) = r.signed_up_at {
            events.push(Event {
                at,
                row: r,
                stage: Stage::SignedUp,
                what: match r.tenant_label.as_deref() {
                    Some(l) => format!("signed up as <code>{}</code>", escape_html(l)),
                    None => "signed up".to_string(),
                },
            });
        }
        if let Some(at) = r.first_paired_at {
            events.push(Event {
                at,
                row: r,
                stage: Stage::Active,
                what: "paired a device".to_string(),
            });
        }
    }
    if events.is_empty() {
        return r#"<p class="muted empty">Nothing has happened yet.</p>"#.to_string();
    }
    // Newest first; a tie keeps the later step of the same person on top.
    events.sort_by(|a, b| b.at.cmp(&a.at).then(b.stage.rank().cmp(&a.stage.rank())));
    let items: String = events
        .iter()
        .take(OVERVIEW_FEED)
        .map(|e| {
            format!(
                r#"<li class="{tone}"><span class="dot"></span><span class="what"><a href="/admin/people/{id}">{email}</a> {what}</span><time>{when}</time></li>
"#,
                tone = e.stage.tone(),
                id = e.row.id,
                email = escape_html(&e.row.email),
                what = e.what,
                when = day(e.at),
            )
        })
        .collect();
    format!(r#"<ol class="feed">{items}</ol>"#)
}

impl Stage {
    /// How far along the funnel, for ordering things that happened at once.
    fn rank(self) -> u8 {
        match self {
            Stage::Waiting => 0,
            Stage::Unsent | Stage::Invited => 1,
            Stage::SignedUp => 2,
            Stage::Active => 3,
        }
    }
}

/// The people list: one table, filtered by stage and by a search, with the
/// counts on the filters.
pub fn admin_people(
    rows: Option<&[UserRow]>,
    filter: Filter,
    q: &str,
    flash: Option<Banner>,
) -> Response {
    let here = Place::People {
        filter,
        q: q.to_string(),
    };
    let counts = rows.map(Counts::of);
    let head = page_head(
        "People",
        filter.title(),
        Some(filter.lede()),
        &invite_button(),
        false,
    );
    let content = match rows {
        None => unavailable(),
        Some(rows) => {
            let c = counts.unwrap_or_default();
            let tabs: String = Filter::EVERY
                .iter()
                .map(|f| {
                    let aria = if *f == filter {
                        r#" aria-current="page""#
                    } else {
                        ""
                    };
                    format!(
                        r#"<a href="{href}"{aria}>{title}<span class="n">{n}</span></a>"#,
                        href = escape_html(
                            &Place::People {
                                filter: *f,
                                q: q.to_string()
                            }
                            .href()
                        ),
                        title = f.title(),
                        n = f.count(&c),
                    )
                })
                .collect();
            let stage_field = if filter == Filter::All {
                String::new()
            } else {
                format!(
                    r#"<input type="hidden" name="stage" value="{}">"#,
                    filter.key()
                )
            };
            let clear = if q.is_empty() {
                String::new()
            } else {
                format!(
                    r#"<a class="clear" href="{}">Clear</a>"#,
                    escape_html(
                        &Place::People {
                            filter,
                            q: String::new()
                        }
                        .href()
                    )
                )
            };
            let toolbar = format!(
                r#"<div class="toolbar">
<nav class="seg" aria-label="Filter by stage">{tabs}</nav>
<form class="search" method="get" action="/admin/people" role="search">{stage_field}
<label class="sr" for="q">Search</label>
<input type="search" id="q" name="q" value="{q}" placeholder="Address, name, or mailbox" autocomplete="off" autocapitalize="off" spellcheck="false">
<button type="submit" class="quiet">Search</button>{clear}
</form>
</div>"#,
                q = escape_html(q),
            );
            let shown: Vec<&UserRow> = rows
                .iter()
                .filter(|r| filter.admits(Stage::of(r)) && matches(r, q))
                .collect();
            let empty = if q.is_empty() {
                filter.empty().to_string()
            } else {
                format!("Nobody here matches \u{201c}{q}\u{201d}.")
            };
            let capped = if c.capped && filter != Filter::Waiting {
                format!(
                    r#"<p class="hint foot">Showing the {USER_APPROVED_LIMIT} most recent approvals. Older rows are still in the store.</p>"#
                )
            } else {
                String::new()
            };
            format!(
                "{toolbar}\n{}{capped}",
                panel(
                    "",
                    filter.title(),
                    Some(shown.len()),
                    "",
                    &people_table(&shown, &here, true, &empty),
                ),
            )
        }
    };
    console_app(
        StatusCode::OK,
        filter.title(),
        &sidebar(&here, counts.as_ref()),
        &format!("{head}\n{}\n{content}", banner(flash)),
    )
}

/// One person: what is known about them, the ladder they are on, and the one
/// thing to do next.
///
/// `counts` is for the sidebar alone, and `None` leaves it without badges.
pub fn admin_person(row: &UserRow, counts: Option<&Counts>, flash: Option<Banner>) -> Response {
    let here = Place::Person(row.id);
    let stage = Stage::of(row);
    let everyone = Place::People {
        filter: Filter::All,
        q: String::new(),
    };
    let crumbs = format!(
        r#"<nav class="crumbs" aria-label="Breadcrumb"><a href="{}">People</a><span aria-hidden="true">/</span><span>{}</span></nav>"#,
        escape_html(&everyone.href()),
        escape_html(&row.email),
    );
    let head = page_head(stage.word(), &row.email, None, "", false);

    let none = |s: &str| format!(r#"<span class="muted">{s}</span>"#);
    let name = match row.name.as_deref() {
        Some(n) => format!(
            "<span class=\"given\">\u{201c}{}\u{201d}</span>",
            escape_html(n)
        ),
        None => none("none given"),
    };
    let account = match row.account_email.as_deref() {
        Some(a) => escape_html(a),
        None => none("not yet"),
    };
    let mailbox = match row.tenant_label.as_deref() {
        Some(l) => format!("<code>{}</code>", escape_html(l)),
        None => none("none yet"),
    };
    let facts = format!(
        r#"<dl class="facts">
<dt>Address</dt><dd>{email}</dd>
<dt>Name, as typed</dt><dd>{name}</dd>
<dt>Signed up as</dt><dd>{account}</dd>
<dt>Mailbox</dt><dd>{mailbox}</dd>
<dt>Row</dt><dd><code>{id}</code></dd>
</dl>"#,
        email = escape_html(&row.email),
        id = row.id,
    );

    // The ladder, every rung, in order. A rung not reached yet says so rather
    // than vanishing, because "not yet" is the thing being looked up.
    let invite_step = match (row.notified_at, row.approved_at) {
        (Some(at), _) => ("done", "Invite emailed", day(at)),
        (None, Some(_)) => ("flag", "Invite emailed", "not sent".to_string()),
        (None, None) => ("", "Invite emailed", "not yet".to_string()),
    };
    let step = |at: Option<DateTime<Utc>>, label: &'static str| match at {
        Some(at) => ("done", label, day(at)),
        None => ("", label, "not yet".to_string()),
    };
    let steps = [
        ("done", "Joined", day(row.created_at)),
        step(row.approved_at, "Approved"),
        invite_step,
        step(row.signed_up_at, "Signed up"),
        step(row.first_paired_at, "Paired a device"),
    ];
    let timeline: String = steps
        .iter()
        .map(|(tone, label, when)| {
            format!(
                r#"<li class="{tone}"><span class="dot"></span><span class="what">{label}</span><time>{when}</time></li>
"#
            )
        })
        .collect();

    let (next_tone, next_body) = match stage {
        Stage::Waiting => (
            "wait",
            format!(
                r#"<p>Approving mints one invite code and emails it to this address. The code works once and expires in {} days.</p>{}"#,
                crate::invites::DEFAULT_TTL_DAYS,
                action_form("/admin/approve", row.id, "Approve", false, &here),
            ),
        ),
        Stage::Unsent => (
            "flag",
            format!(
                r#"<p>Approved, and the invite email never went out. Nobody has been told.</p>{}"#,
                action_form("/admin/send", row.id, "Send invite", false, &here),
            ),
        ),
        Stage::Invited => (
            "",
            format!(
                r#"<p>Nothing to do until they sign up. If the email went astray, a fresh code replaces the old one, which stops working.</p>{}"#,
                action_form("/admin/send", row.id, "Re-send", true, &here),
            ),
        ),
        Stage::SignedUp => (
            "done",
            format!(
                r#"<p>They have a mailbox,{}, and have not paired a device with it yet. There is nothing to send.</p>"#,
                tenant_code(row),
            ),
        ),
        Stage::Active => (
            "live",
            format!(
                r#"<p>They have a mailbox,{}, and a device paired with it. There is nothing to send.</p>"#,
                tenant_code(row),
            ),
        ),
    };

    let body = format!(
        r#"{crumbs}
{head}
{flash}
<div class="cols">
{details}
{timeline}
</div>
{next}"#,
        flash = banner(flash),
        details = panel("", "Details", None, &stage.pill(), &facts),
        timeline = panel(
            "",
            "Timeline",
            None,
            "",
            &format!(r#"<ol class="feed ladder">{timeline}</ol>"#)
        ),
        next = panel(
            next_tone,
            "Next step",
            None,
            "",
            &format!(r#"<div class="next">{next_body}</div>"#)
        ),
    );
    console_app(StatusCode::OK, &row.email, &sidebar(&here, counts), &body)
}

/// A person's page for an id that names nobody: the sidebar, the banner, and
/// the way back to the list.
pub fn admin_no_person(counts: Option<&Counts>, error: &str) -> Response {
    let everyone = Place::People {
        filter: Filter::All,
        q: String::new(),
    };
    console_app(
        StatusCode::NOT_FOUND,
        "Nobody here",
        &sidebar(&everyone, counts),
        &format!(
            r#"{head}
{banner}
<p><a href="{href}">Back to everyone</a></p>"#,
            head = page_head("People", "Nobody here", None, "", false),
            banner = banner(Some(Banner::Stop(error))),
            href = escape_html(&everyone.href()),
        ),
    )
}

/// Inviting somebody who never joined the list, as its own page.
pub fn admin_invite(counts: Option<&Counts>, flash: Option<Banner>) -> Response {
    let here = Place::Invite;
    let head = page_head(
        "Manage",
        "Invite someone",
        Some(
            "They do not have to be on the waitlist first. The invite lands on the same ledger as an approved row, so it shows under Invited with the same buttons.",
        ),
        "",
        false,
    );
    let form = format!(
        r#"<form class="invite" method="post" action="/admin/invite">
<input type="hidden" name="{NEXT_FIELD}" value="{next}">
<label class="sr" for="email">Email address</label>
<input type="email" id="email" name="email" placeholder="them@example.com"
  autocomplete="off" autocapitalize="off" spellcheck="false" required>
<button type="submit">Mint and email an invite</button>
</form>
<p class="hint">You will be asked one question before anything is minted. The code works once and expires in {ttl} days.</p>"#,
        next = escape_html(&here.href()),
        ttl = crate::invites::DEFAULT_TTL_DAYS,
    );
    let before = r#"<p>Until Passband's OAuth client is verified, Google only lets an account finish consent if it is on the project's test-user list. Add the address there first, or the invite sends them to a wall.</p>"#;
    console_app(
        StatusCode::OK,
        "Invite someone",
        &sidebar(&here, counts),
        &format!(
            "{head}\n{}\n{}\n{}",
            banner(flash),
            panel("send", "Address", None, "", &form),
            panel("wait", "Before you send", None, "", before),
        ),
    )
}

/// The form field names on the features page. Here so the page that writes
/// them and the handler that reads them cannot drift apart.
///
/// `FEATURE_TENANT_FIELD` rides once per row as a hidden field and says "this
/// tenant was on the page", which is what lets an UNTICKED box mean "off": a
/// checkbox that is not ticked sends nothing at all.
pub const FEATURE_TENANT_FIELD: &str = "t";
/// One COLUMN the page drew, for the same reason turned ninety degrees: a save
/// may only untick boxes in columns this page actually had.
pub const FEATURE_KEY_FIELD: &str = "k";
/// One ticked box: `<tenant id>.<feature key>`.
pub const FEATURE_GRANT_FIELD: &str = "g";
/// One ticked box on the "everyone" row: a feature key.
pub const FEATURE_EVERYONE_FIELD: &str = "all";

/// The features page: which hidden features each tenant has been let into.
///
/// `board` is `None` when the store did not answer, and then THERE IS NO FORM.
/// A form drawn over an empty list would post "every box is unticked" and the
/// save would believe it, so the one thing this page must never do is offer a
/// Save button under a list it failed to read.
pub fn admin_features(
    registry: &[Feature],
    board: Option<&FeatureBoard>,
    counts: Option<&Counts>,
    error: Option<&str>,
    saved: bool,
    refresh_minutes: u64,
) -> Response {
    let legend: String = registry
        .iter()
        .map(|f| {
            format!(
                "<dt><code>{}</code></dt><dd>{}</dd>\n",
                escape_html(f.key),
                escape_html(f.summary),
            )
        })
        .collect();

    let body = match (registry.is_empty(), board) {
        (true, _) => panel(
            "",
            "Tenants",
            None,
            "",
            r#"<p class="muted empty">This build hides nothing: every feature it has is on for everyone. A hidden feature
shows up here once it is listed in <code>squelch-core/src/features.rs</code> and that build is deployed.</p>"#,
        ),
        (false, None) => String::new(),
        (false, Some(board)) => {
            let head: String = registry
                .iter()
                .map(|f| format!("<th>{}</th>", escape_html(f.key)))
                .collect();
            let boxes = |name: &str, prefix: &str, on: &dyn Fn(&str) -> bool| -> String {
                registry
                    .iter()
                    .map(|f| {
                        let key = escape_html(f.key);
                        let checked = if on(f.key) { " checked" } else { "" };
                        format!(
                            r#"<td class="tick"><label><input type="checkbox" name="{name}" value="{prefix}{key}"{checked}><span>{key}</span></label></td>"#
                        )
                    })
                    .collect()
            };
            let everyone_row = format!(
                r#"<tr class="everyone"><td class="who">Everyone<br><span class="muted">every tenant, and anybody who signs up later</span></td>{}</tr>
"#,
                boxes(FEATURE_EVERYONE_FIELD, "", &|key| board
                    .everyone
                    .contains(key)),
            );
            let tenant_rows: String = board
                .tenants
                .iter()
                .map(|t| {
                    // A row whose daemon cannot ask is still tickable: the
                    // grant is stored and goes live when a token is minted.
                    // What it must not do is look like every other row.
                    let tokenless = if t.has_token {
                        ""
                    } else {
                        r#"<br><span class="pill flag">no control token</span><span class="muted">waits for <code>share mint</code></span>"#
                    };
                    format!(
                        r#"<tr><td class="who"><input type="hidden" name="{FEATURE_TENANT_FIELD}" value="{id}"><code>{label}</code><br><span class="muted">{email}</span>{tokenless}</td>{boxes}</tr>
"#,
                        id = t.id,
                        label = escape_html(&t.label),
                        email = escape_html(&t.account_email),
                        boxes = boxes(FEATURE_GRANT_FIELD, &format!("{}.", t.id), &|key| t
                            .features
                            .contains(key)),
                    )
                })
                .collect();
            let nobody = if board.tenants.is_empty() {
                r#"<p class="muted empty">No active tenants yet.</p>"#
            } else {
                ""
            };
            let columns: String = registry
                .iter()
                .map(|f| {
                    format!(
                        r#"<input type="hidden" name="{FEATURE_KEY_FIELD}" value="{}">"#,
                        escape_html(f.key)
                    )
                })
                .collect();
            format!(
                r#"<form class="features" method="post" action="/admin/features">{columns}
{panel}
<div class="footer-row"><button type="submit">Save</button>
<p class="hint">Saves the whole page at once.</p></div>
</form>"#,
                panel = panel(
                    "",
                    "Tenants",
                    Some(board.tenants.len()),
                    &format!(
                        r#"<p class="hint">A ticked box is on. A mailbox's daemon hears about a change within {refresh_minutes} minutes, with no restart.</p>"#
                    ),
                    &format!(
                        r#"<table class="matrix"><tr><th>Who</th>{head}</tr>
{everyone_row}{tenant_rows}</table>
{nobody}"#
                    ),
                ),
            )
        }
    };

    let legend = if legend.is_empty() {
        String::new()
    } else {
        panel(
            "",
            "Hidden features",
            Some(registry.len()),
            "",
            &format!("<dl class=\"legend\">\n{legend}</dl>"),
        )
    };
    let flash = match error {
        Some(e) => Some(Banner::Stop(e)),
        None if saved => Some(Banner::Saved),
        None => None,
    };
    let here = Place::Features;
    console_app(
        StatusCode::OK,
        "Features",
        &sidebar(&here, counts),
        &format!(
            "{head}\n{}\n{legend}\n{body}",
            banner(flash),
            head = page_head(
                "Manage",
                "Features",
                Some("Which tenants see what this build still hides."),
                "",
                false
            ),
        ),
    )
}

// ---------------------------------------------------------------------------
// The scope.

/// One rung of the funnel, as the scope draws it: a carrier with a marker.
struct Rung {
    label: &'static str,
    count: usize,
    /// A CSS colour, always one of the stylesheet's own variables. Never data.
    colour: &'static str,
    /// The rung this one is a fraction of, and its name, for the rate under it.
    of: Option<(usize, &'static str)>,
    /// The list this rung's readout opens.
    filter: Filter,
}

/// The funnel as the landing page's analyzer would draw it: a noise floor with
/// one carrier per rung standing out of it, and a marker on each peak.
///
/// THE PEAKS ARE THE DATA. Each carrier's height is its count on a log scale
/// against the largest rung, so a board of 200 waiting and 3 active still shows
/// three as a real signal rather than a flat line. A rung of zero is just the
/// floor, which is the honest picture.
///
/// THE NOISE IS NOT. It comes from a fixed seed, so the same counts draw the
/// same trace on every refresh and the only thing that moves the picture is
/// the funnel moving.
///
/// The readouts under it are links into the list each one counts. The picture
/// is `aria-hidden`; the readouts are what a screen reader gets.
fn scope(c: &Counts) -> String {
    let rungs = [
        Rung {
            label: "waiting",
            count: c.waiting,
            colour: "var(--warn)",
            of: None,
            filter: Filter::Waiting,
        },
        Rung {
            label: "approved",
            count: c.approved,
            colour: "var(--accent)",
            of: None,
            filter: Filter::All,
        },
        Rung {
            label: "signed up",
            count: c.signed_up,
            colour: "var(--accent-ink)",
            of: Some((c.approved, "approved")),
            filter: Filter::Members,
        },
        Rung {
            label: "active",
            count: c.active,
            colour: "var(--positive)",
            of: Some((c.signed_up, "signed up")),
            filter: Filter::Members,
        },
    ];
    const W: f64 = 1000.0;
    const H: f64 = 300.0;
    const FLOOR: f64 = 262.0;
    const RANGE: f64 = 200.0;
    let centres = [125.0, 375.0, 625.0, 875.0];
    let max = rungs.iter().map(|r| r.count).max().unwrap_or(0);
    let heights: Vec<f64> = rungs
        .iter()
        .map(|r| {
            if r.count == 0 || max == 0 {
                0.0
            } else {
                // A single person still clears the floor by a visible margin.
                36.0 + (RANGE - 36.0) * ((1 + r.count) as f64).ln() / ((1 + max) as f64).ln()
            }
        })
        .collect();

    // A tiny LCG: deterministic, dependency free, and plenty random for grass.
    let noise = |seed: u64| {
        let mut state = seed;
        move || {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((state >> 33) as f64) / ((1u64 << 31) as f64)
        }
    };
    let signal = |x: f64| -> f64 {
        centres
            .iter()
            .zip(&heights)
            .map(|(c, h)| {
                let d = x - c;
                // A sharp Lorentzian carrier on a broad skirt, which is the
                // shape a real carrier has through a resolution filter.
                h * (0.8 / (1.0 + (d / 11.0).powi(2)) + 0.2 / (1.0 + (d / 55.0).powi(2)))
            })
            .sum()
    };
    let mut rnd = noise(7);
    let mut live = String::new();
    let mut x = 0.0;
    while x <= W {
        let y = (FLOOR - 12.0 * rnd() - signal(x)).max(8.0);
        live.push_str(&format!(
            "{}{x:.0} {y:.1}",
            if x == 0.0 { "M" } else { "L" }
        ));
        x += 4.0;
    }

    let markers: String = rungs
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let top = (FLOOR - 12.0 - heights[i]).max(8.0);
            format!(
                r#"<i class="mkr" style="--c:{c};left:{l:.2}%;top:{t:.2}%"></i>"#,
                c = r.colour,
                l = centres[i] / W * 100.0,
                t = top / H * 100.0,
            )
        })
        .collect();
    let readouts: String = rungs
        .iter()
        .map(|r| {
            // A rate only where there is one to state.
            let sub = match r.of {
                Some((of, name)) if of > 0 => format!(
                    r#"<span class="sub">{}% of {name}</span>"#,
                    (r.count as f64 / of as f64 * 100.0).round()
                ),
                _ => String::new(),
            };
            format!(
                r#"<a class="readout" href="{href}" style="--c:{c}"><span class="lbl">{label}</span><span class="num">{count}</span>{sub}</a>"#,
                href = escape_html(
                    &Place::People {
                        filter: r.filter,
                        q: String::new()
                    }
                    .href()
                ),
                c = r.colour,
                label = r.label,
                count = r.count,
            )
        })
        .collect();

    format!(
        r#"<figure class="scope"><div class="scope-screen" aria-hidden="true"><svg viewBox="0 0 {W} {H}" preserveAspectRatio="none"><path class="fill" d="{live} L{W} {H} L0 {H} Z"/><path class="trace" vector-effect="non-scaling-stroke" pathLength="4000" d="{live}"/></svg>{markers}</div><div class="readouts">{readouts}</div></figure>"#,
    )
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use axum::body::to_bytes;
    use axum::http::header;

    pub(crate) async fn body_of(r: Response) -> String {
        String::from_utf8(to_bytes(r.into_body(), 1 << 20).await.unwrap().to_vec()).unwrap()
    }

    /// The page without the shell's stylesheet: what a reader can see. Use it
    /// for every "must not appear" assertion, since the stylesheet's own
    /// comments and selectors would otherwise answer for the page.
    async fn document_of(r: Response) -> String {
        body_of(r)
            .await
            .split_once("</style>")
            .expect("every page carries the shell's stylesheet")
            .1
            .to_string()
    }

    fn at() -> DateTime<Utc> {
        // 2026-01-01T00:00:00Z.
        DateTime::from_timestamp(1_767_225_600, 0).unwrap()
    }

    /// A waiting row, as the store hands one over.
    pub(crate) fn waiting(id: i64, email: &str) -> UserRow {
        UserRow {
            id,
            email: email.to_string(),
            name: None,
            created_at: at(),
            status: USER_PENDING.to_string(),
            approved_at: None,
            invite_id: None,
            notified_at: None,
            signed_up_at: None,
            tenant_label: None,
            account_email: None,
            first_paired_at: None,
        }
    }

    /// An approved row, mailed or not.
    pub(crate) fn row(id: i64, email: &str, notified: bool) -> UserRow {
        UserRow {
            status: crate::store::USER_APPROVED.to_string(),
            approved_at: Some(at()),
            invite_id: Some(7),
            notified_at: notified.then_some(at()),
            ..waiting(id, email)
        }
    }

    /// The same row after they spent the code: a mailbox exists.
    fn accepted_row(id: i64, email: &str, label: &str) -> UserRow {
        UserRow {
            signed_up_at: Some(at()),
            tenant_label: Some(label.to_string()),
            account_email: Some(email.to_string()),
            ..row(id, email, true)
        }
    }

    /// ...and after a device that is not the console paired with it.
    fn active_row(id: i64, email: &str, label: &str) -> UserRow {
        UserRow {
            first_paired_at: Some(at()),
            ..accepted_row(id, email, label)
        }
    }

    fn all() -> Filter {
        Filter::All
    }

    async fn people(rows: &[UserRow], filter: Filter) -> String {
        document_of(admin_people(Some(rows), filter, "", None)).await
    }

    /// Every page a rendered string reaches, for the assertions that must hold
    /// on all of them.
    pub(crate) async fn every_admin_page(rows: &[UserRow]) -> Vec<String> {
        let mut pages = vec![
            body_of(admin_login(Some("no"))).await,
            body_of(admin_signed_out()).await,
            body_of(admin_overview(Some(rows), Some(Banner::Stop("no")))).await,
            body_of(admin_overview(None, Some(Banner::Stop("no")))).await,
            body_of(admin_people(Some(rows), all(), "", None)).await,
            body_of(admin_people(Some(rows), Filter::Waiting, "ada", None)).await,
            body_of(admin_invite(None, Some(Banner::Done(Done::Invited)))).await,
            body_of(admin_features(&[], None, None, None, true, 6)).await,
            body_of(admin_confirm(
                "/admin/approve",
                &[("id", "1")],
                "ada@example.com",
                "Yes",
                &Place::Overview,
            ))
            .await,
            body_of(admin_cross_origin("https://signup.passband.app", "x")).await,
        ];
        for r in rows {
            pages.push(body_of(admin_person(r, Some(&Counts::of(rows)), None)).await);
        }
        pages
    }

    /// The board is the one surface that renders a string a stranger typed into
    /// a public form, so it is the one where escaping is the whole defense.
    /// `<`, `>`, and `"` all survive the address shape check.
    #[tokio::test]
    async fn every_page_escapes_an_address_that_is_an_attack() {
        let hostile = r#""><script>alert(1)</script>@evil.test"#;
        let rows = [
            waiting(1, hostile),
            row(2, hostile, true),
            accepted_row(3, hostile, "eve"),
        ];
        for html in [
            document_of(admin_overview(
                Some(&rows),
                Some(Banner::Stop("<b>nope</b>")),
            ))
            .await,
            people(&rows, all()).await,
            document_of(admin_person(
                &rows[0],
                None,
                Some(Banner::Stop("<b>nope</b>")),
            ))
            .await,
            document_of(admin_confirm(
                "/admin/send",
                &[("id", "2")],
                hostile,
                "Yes",
                &Place::Person(2),
            ))
            .await,
        ] {
            assert!(!html.contains("<script>alert(1)"), "{html}");
            assert!(!html.contains("<b>nope</b>"), "{html}");
            assert!(html.contains("&lt;script&gt;"), "{html}");
            assert!(html.contains("&quot;&gt;"), "{html}");
        }
    }

    /// A search term is the operator's, echoed into the box, the empty-state
    /// sentence, and every link that keeps it; escaped in all three, and
    /// percent-encoded in the links so it cannot start a parameter of its own.
    #[tokio::test]
    async fn a_search_term_is_escaped_and_encoded_wherever_it_lands() {
        let q = r#""><script>x</script>&stage=members"#;
        let html = document_of(admin_people(Some(&[]), Filter::Waiting, q, None)).await;
        assert!(!html.contains("<script>x"), "{html}");
        assert!(
            html.contains(r#"value="&quot;&gt;&lt;script&gt;"#),
            "{html}"
        );
        assert!(
            html.contains("q=%22%3E%3Cscript%3Ex%3C%2Fscript%3E%26stage%3Dmembers"),
            "{html}"
        );
    }

    /// Which button a row gets is the whole state machine the operator sees.
    #[tokio::test]
    async fn each_stage_offers_its_own_button() {
        let html = people(
            &[
                waiting(1, "ada@example.com"),
                row(2, "bob@example.com", false),
                row(3, "cy@example.com", true),
            ],
            all(),
        )
        .await;
        assert!(html.contains(r#"action="/admin/approve""#), "{html}");
        assert!(html.contains(r#"name="id" value="1""#), "{html}");
        assert!(html.contains(">Approve</button>"), "{html}");
        // Loud: this one is asking to be pressed.
        assert!(
            html.contains(r#"<span class="pill flag">email not sent</span>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<button type="submit">Send invite</button>"#),
            "{html}"
        );
        // Quiet: the row is fine and the button is for a lost email.
        assert!(
            html.contains(r#"<button type="submit" class="quiet">Re-send</button>"#),
            "{html}"
        );
        assert!(
            html.contains(
                r#"<span class="pill sent">invited</span> <span class="muted">2026-01-01</span>"#
            ),
            "{html}"
        );
        assert!(!html.contains("<script"), "{html}");
    }

    /// Every button carries the page it was pressed on, so the action comes
    /// back to it rather than to the top of the board.
    #[tokio::test]
    async fn every_button_carries_its_page() {
        let html = document_of(admin_people(
            Some(&[waiting(1, "ada@example.com")]),
            Filter::Waiting,
            "ada",
            None,
        ))
        .await;
        assert!(
            html.contains(r#"name="next" value="/admin/people?stage=waiting&amp;q=ada""#),
            "{html}"
        );
        let html = document_of(admin_person(&waiting(1, "ada@example.com"), None, None)).await;
        assert!(
            html.contains(r#"name="next" value="/admin/people/1""#),
            "{html}"
        );
        let html = document_of(admin_confirm(
            "/admin/approve",
            &[("id", "1")],
            "ada@example.com",
            "Yes",
            &Place::Person(1),
        ))
        .await;
        assert!(
            html.contains(r#"name="next" value="/admin/people/1""#),
            "{html}"
        );
        assert!(
            html.contains(r#"<a href="/admin/people/1">Cancel"#),
            "{html}"
        );
    }

    /// THE FAR END OF THE FUNNEL: a spent invite means a mailbox, the row says
    /// which one, and it offers NO button, because the only one it could offer
    /// is a guaranteed refusal.
    #[tokio::test]
    async fn a_mailbox_row_says_which_and_offers_no_button() {
        for (r, word) in [
            (accepted_row(1, "ada@example.com", "ada"), "signed up"),
            (active_row(1, "ada@example.com", "ada"), "active"),
        ] {
            for html in [
                people(std::slice::from_ref(&r), all()).await,
                document_of(admin_person(&r, None, None)).await,
            ] {
                assert!(html.contains(&format!(">{word}</span>")), "{html}");
                assert!(html.contains("<code>ada</code>"), "{html}");
                assert!(!html.contains("Re-send"), "{html}");
                assert!(!html.contains("/admin/send"), "{html}");
            }
        }
    }

    /// The tally nests: an active person is counted as signed up too, and the
    /// rates say so.
    #[tokio::test]
    async fn the_scope_counts_nest() {
        let rows = [
            waiting(1, "a@example.com"),
            row(2, "b@example.com", true),
            accepted_row(3, "c@example.com", "c"),
            active_row(4, "d@example.com", "d"),
        ];
        let c = Counts::of(&rows);
        assert_eq!((c.waiting, c.approved, c.signed_up, c.active), (1, 3, 2, 1));
        let html = document_of(admin_overview(Some(&rows), None)).await;
        assert!(html.contains("67% of approved"), "{html}");
        assert!(html.contains("50% of signed up"), "{html}");
        // The readouts open the lists they count.
        assert!(
            html.contains(r#"<a class="readout" href="/admin/people?stage=waiting""#),
            "{html}"
        );
    }

    /// The filters show what they say, and the counts on them are the store's.
    #[tokio::test]
    async fn the_filters_split_the_list_by_stage() {
        let rows = [
            waiting(1, "wait@example.com"),
            row(2, "unsent@example.com", false),
            row(3, "sent@example.com", true),
            accepted_row(4, "member@example.com", "m"),
            active_row(5, "live@example.com", "l"),
        ];
        let has = |html: &str, who: &str| html.contains(&format!(">{who}@example.com</a>"));
        let w = people(&rows, Filter::Waiting).await;
        assert!(
            has(&w, "wait") && !has(&w, "sent") && !has(&w, "member"),
            "{w}"
        );
        let i = people(&rows, Filter::Invited).await;
        assert!(
            has(&i, "unsent") && has(&i, "sent") && !has(&i, "wait") && !has(&i, "live"),
            "{i}"
        );
        let m = people(&rows, Filter::Members).await;
        assert!(
            has(&m, "member") && has(&m, "live") && !has(&m, "sent"),
            "{m}"
        );
        let a = people(&rows, all()).await;
        assert!(
            ["wait", "unsent", "sent", "member", "live"]
                .iter()
                .all(|w| has(&a, w)),
            "{a}"
        );
        // Filter tabs carry counts.
        assert!(a.contains(r#"Invited<span class="n">2</span>"#), "{a}");
        // The sidebar flags the broken invite on its own badge.
        assert!(
            a.contains(r#"<span class="badge flag" title="email not sent">1</span>"#),
            "{a}"
        );
    }

    /// A search finds a person by any name they go by, and says so when it
    /// finds nobody.
    #[tokio::test]
    async fn search_matches_every_name_a_person_goes_by() {
        let rows = [
            UserRow {
                name: Some("Ada Lovelace".into()),
                ..waiting(1, "ada@example.com")
            },
            UserRow {
                account_email: Some("grace@navy.mil".into()),
                ..accepted_row(2, "hopper@example.com", "amazing")
            },
        ];
        for (q, who) in [("LOVELACE", "ada"), ("navy", "hopper"), ("amaz", "hopper")] {
            let html = document_of(admin_people(Some(&rows), all(), q, None)).await;
            assert!(
                html.contains(&format!(">{who}@example.com</a>")),
                "{q}: {html}"
            );
            assert_eq!(
                html.matches(r#"<a class="person""#).count(),
                1,
                "{q}: {html}"
            );
        }
        let html = document_of(admin_people(Some(&rows), all(), "zzz", None)).await;
        assert!(
            html.contains("Nobody here matches \u{201c}zzz\u{201d}."),
            "{html}"
        );
    }

    /// A store that did not answer shows the banner and no list: "nobody is
    /// waiting" with no explanation is the one wrong thing to show an operator,
    /// and the sidebar carries no zeroes it does not know.
    #[tokio::test]
    async fn a_silent_store_draws_no_list_and_no_counts() {
        for html in [
            document_of(admin_overview(
                None,
                Some(Banner::Stop("The store did not answer.")),
            ))
            .await,
            document_of(admin_people(
                None,
                Filter::Waiting,
                "",
                Some(Banner::Stop("The store did not answer.")),
            ))
            .await,
        ] {
            assert!(html.contains("The store did not answer."), "{html}");
            assert!(!html.contains("Nobody is waiting"), "{html}");
            assert!(!html.contains(r#"class="badge"#), "{html}");
            assert!(!html.contains("<table"), "{html}");
        }
    }

    /// The overview names the queue and the broken invites, and points at the
    /// rest of the queue rather than drawing all of it.
    #[tokio::test]
    async fn the_overview_shows_the_queue_and_what_is_broken() {
        let mut rows: Vec<UserRow> = (1..=8)
            .map(|i| waiting(i, &format!("w{i}@example.com")))
            .collect();
        rows.push(row(9, "broken@example.com", false));
        let html = document_of(admin_overview(Some(&rows), None)).await;
        assert_eq!(
            html.matches(">Approve</button>").count(),
            OVERVIEW_WAITING,
            "{html}"
        );
        assert!(html.contains("See all 8 waiting"), "{html}");
        assert!(
            html.contains(r#"<button type="submit">Send invite</button>"#),
            "{html}"
        );
        // The feed, newest first: everything here happened on one day, and the
        // approval outranks a join that landed with it.
        assert!(
            html.contains("was approved, and the email did not go out"),
            "{html}"
        );

        let calm = document_of(admin_overview(Some(&[]), None)).await;
        assert!(calm.contains("Nobody is waiting."), "{calm}");
        assert!(calm.contains("Every approved invite went out."), "{calm}");
        assert!(calm.contains("Nothing has happened yet."), "{calm}");
    }

    /// The person page: every fact, every rung, and "not yet" on the ones not
    /// reached rather than a gap.
    #[tokio::test]
    async fn a_person_page_lays_out_the_whole_ladder() {
        let r = UserRow {
            name: Some("Ada Lovelace".into()),
            ..row(4, "ada@example.com", true)
        };
        let html = document_of(admin_person(&r, None, None)).await;
        for want in [
            "<dt>Address</dt><dd>ada@example.com</dd>",
            "\u{201c}Ada Lovelace\u{201d}",
            r#"<li class="done"><span class="dot"></span><span class="what">Invite emailed</span><time>2026-01-01</time></li>"#,
            r#"<li class=""><span class="dot"></span><span class="what">Signed up</span><time>not yet</time></li>"#,
            "Re-send",
        ] {
            assert!(html.contains(want), "{want}: {html}");
        }
        let unsent = document_of(admin_person(&row(4, "ada@example.com", false), None, None)).await;
        assert!(unsent.contains("<time>not sent</time>"), "{unsent}");
    }

    /// A NAME CANNOT SPELL THE ONE LINE ON THIS PAGE THAT COMES FROM GOOGLE.
    ///
    /// The provenance line is the board's most authoritative field and the name
    /// beneath it is free text off a public form. The frame (curly quotes that
    /// `normalize_name` strips from every stored name) and the order (what the
    /// page knows first, what somebody typed last) are what keep them apart.
    #[tokio::test]
    async fn a_name_cannot_forge_the_line_that_came_from_google() {
        let forged = UserRow {
            name: Some("signed up as ada@example.com".to_string()),
            ..waiting(1, "eve@evil.test")
        };
        let forged = people(&[forged], all()).await;
        let genuine = UserRow {
            account_email: Some("ada@example.com".to_string()),
            ..accepted_row(1, "eve@evil.test", "eve")
        };
        let genuine = people(&[genuine], all()).await;

        let cell = |html: &str| {
            let start = html.find(">eve@evil.test").unwrap();
            html[start..start + 120].to_string()
        };
        assert_ne!(
            cell(&forged),
            cell(&genuine),
            "the forgery is indistinguishable"
        );
        assert!(
            genuine.contains(r#"<span class="muted">signed up as ada@example.com</span>"#),
            "{genuine}"
        );
        assert!(
            forged.contains(
                "<span class=\"given\">\u{201c}signed up as ada@example.com\u{201d}</span>"
            ),
            "{forged}"
        );
        assert!(
            !forged.contains(r#"<span class="muted">signed up as"#),
            "{forged}"
        );

        // And on a row that carries both, the page's own line comes first.
        let both = UserRow {
            name: Some("signed up as ada@bigco.com".to_string()),
            account_email: Some("eve-real@evil.test".to_string()),
            ..accepted_row(1, "eve@evil.test", "eve")
        };
        let html = people(&[both], all()).await;
        assert!(
            html.find("eve-real@evil.test").unwrap() < html.find("ada@bigco.com").unwrap(),
            "{html}"
        );
    }

    /// AN INVITE IS A BEARER CODE: when the account that signed up differs from
    /// the invited address the row says so, and when they agree it says nothing.
    #[tokio::test]
    async fn a_signup_under_another_address_says_which() {
        let mismatch = UserRow {
            account_email: Some(r#"<script>alert(1)</script>@evil.test"#.to_string()),
            ..accepted_row(1, "ada@example.com", "ada")
        };
        let html = people(&[mismatch], all()).await;
        assert!(
            html.contains(r#"<span class="muted">signed up as &lt;script&gt;"#),
            "{html}"
        );
        let agreed = people(&[accepted_row(1, "ada@example.com", "ada")], all()).await;
        assert!(!agreed.contains("signed up as"), "{agreed}");
    }

    /// The name is context under the address, and only when there is one.
    #[tokio::test]
    async fn a_name_sits_under_the_address_when_there_is_one() {
        let named = UserRow {
            name: Some("Ada Lovelace".to_string()),
            ..waiting(1, "ada@example.com")
        };
        let html = people(&[named], all()).await;
        assert!(
            html.contains("<span class=\"given\">\u{201c}Ada Lovelace\u{201d}</span>"),
            "{html}"
        );
        assert!(html.find("ada@example.com").unwrap() < html.find("Ada Lovelace").unwrap());
        let html = people(&[row(3, "grace@example.com", true)], all()).await;
        assert!(!html.contains(r#"class="given""#), "{html}");
    }

    /// A `next` field becomes one of this console's pages or nothing, and the
    /// URL a redirect carries is always one this module wrote.
    #[test]
    fn next_parses_only_into_pages_and_writes_them_fresh() {
        assert_eq!(Place::parse("/admin"), Some(Place::Overview));
        assert_eq!(Place::parse("/admin/people/42"), Some(Place::Person(42)));
        assert_eq!(
            Place::parse("/admin/people?stage=invited&q=a%20b"),
            Some(Place::People {
                filter: Filter::Invited,
                q: "a b".into()
            })
        );
        for bad in [
            "",
            "https://evil.test/admin",
            "//evil.test",
            "/admin/../x",
            "/admin/people/+1",
            "/admin/people/1/x",
            "/admin/people/",
            "/adminx",
            "/admin/people/99999999999999999999999",
        ] {
            assert_eq!(Place::parse(bad), None, "{bad:?}");
        }
        let p = Place::People {
            filter: Filter::Waiting,
            q: "a&b\r\nSet-Cookie: x".into(),
        };
        assert_eq!(
            p.href(),
            "/admin/people?stage=waiting&q=a%26b%0D%0ASet-Cookie%3A%20x"
        );
        assert_eq!(
            p.href_done(Done::Approved),
            format!("{}&done=approved", p.href())
        );
        assert_eq!(Place::Overview.href_done(Done::Sent), "/admin?done=sent");
        assert_eq!(Done::from_query("x=1&done=sent"), Some(Done::Sent));
        assert_eq!(Done::from_query("done=<b>"), None);
    }

    /// A DELIBERATE SIGN OUT IS NOT A REFUSAL: a 200 with the door on it. The
    /// expired-session door is the 401, and still says so.
    #[tokio::test]
    async fn signing_out_is_a_200_and_being_thrown_out_is_a_401() {
        let out = admin_signed_out();
        assert_eq!(out.status(), StatusCode::OK);
        let html = document_of(out).await;
        assert!(html.contains("Signed out."), "{html}");
        assert!(html.contains(r#"action="/admin/login""#), "{html}");
        assert!(!html.contains(r#"class="stop""#), "{html}");
        assert_eq!(
            admin_login(Some("Your admin session has ended.")).status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(admin_login(None).status(), StatusCode::OK);
    }

    /// The way out is on every page, in the sidebar, and it is a form, because
    /// a link cannot carry the same-origin POST every other action is held to.
    #[tokio::test]
    async fn every_app_page_carries_the_way_out() {
        for html in [
            document_of(admin_overview(Some(&[]), None)).await,
            document_of(admin_people(Some(&[]), all(), "", None)).await,
            document_of(admin_invite(None, None)).await,
            document_of(admin_features(&[], None, None, None, false, 6)).await,
            document_of(admin_person(&waiting(1, "a@example.com"), None, None)).await,
        ] {
            assert!(html.contains(r#"<aside class="side">"#), "{html}");
            assert!(html.contains(r#"action="/admin/logout""#), "{html}");
            assert!(html.contains("This session lasts 30 days"), "{html}");
        }
    }

    /// The sidebar knows where it is.
    #[tokio::test]
    async fn the_sidebar_marks_the_page_it_is_on() {
        let html = document_of(admin_people(Some(&[]), Filter::Members, "", None)).await;
        assert!(
            html.contains(r#"<a href="/admin/people?stage=members" aria-current="page">"#),
            "{html}"
        );
        assert_eq!(
            html.matches(r#"aria-current="page""#).count(),
            2,
            "sidebar and tab: {html}"
        );
        let html = document_of(admin_invite(None, None)).await;
        assert!(
            html.contains(r#"<a href="/admin/invite" aria-current="page">"#),
            "{html}"
        );
    }

    /// The door names nothing that is behind it.
    #[tokio::test]
    async fn the_admin_door_says_nothing_and_refuses_with_a_status() {
        let r = admin_login(None);
        assert_eq!(r.status(), StatusCode::OK);
        let html = document_of(r).await;
        assert!(html.contains(r#"type="password""#), "{html}");
        assert!(html.contains(r#"action="/admin/login""#), "{html}");
        assert!(!html.to_lowercase().contains("waitlist"), "{html}");
        assert!(!html.contains("side-nav"), "{html}");
    }

    /// The direct-invite form is styled: the stylesheet once named input types
    /// by hand, and an `email` input rendered as a browser default.
    #[tokio::test]
    async fn the_invite_page_carries_a_styled_form() {
        let page = body_of(admin_invite(None, None)).await;
        assert!(page.contains(r#"action="/admin/invite""#), "{page}");
        assert!(page.contains(r#"name="email""#), "{page}");
        assert!(page.contains("Mint and email an invite"), "{page}");
        assert!(page.contains("input[type=email]"), "{page}");
        assert!(page.contains("input[type=search]"), "{page}");
    }

    /// House rules, on every admin page: no script, no typographic dashes, the
    /// security headers, and the mark.
    #[tokio::test]
    async fn every_admin_page_keeps_the_house_rules() {
        const BANNED: [char; 5] = ['\u{2012}', '\u{2013}', '\u{2014}', '\u{2015}', '\u{2212}'];
        let rows = [
            waiting(1, "ada@example.com"),
            row(2, "bob@example.com", false),
            row(3, "cy@example.com", true),
            accepted_row(4, "di@example.com", "di"),
            active_row(5, "ed@example.com", "ed"),
        ];
        for html in every_admin_page(&rows).await {
            for dash in BANNED {
                assert!(!html.contains(dash), "U+{:04X} in {html}", u32::from(dash));
            }
            assert!(!html.contains("<script"), "{html}");
            assert!(html.contains(r#"<header class="brand">"#), "{html}");
            assert_eq!(html.matches("url(").count(), 1, "{html}");
        }
        for r in [
            admin_overview(Some(&rows), None),
            admin_person(&rows[0], None, None),
            admin_login(None),
        ] {
            let h = r.headers().clone();
            assert_eq!(h[header::X_FRAME_OPTIONS], "DENY");
            assert_eq!(h[header::CACHE_CONTROL], "no-store, no-cache");
            assert!(
                h[header::CONTENT_SECURITY_POLICY]
                    .to_str()
                    .unwrap()
                    .contains("default-src 'none'")
            );
        }
    }
}
