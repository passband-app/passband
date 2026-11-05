//! Every HTML byte this service serves.
//!
//! NO JAVASCRIPT, no external assets, no fonts to fetch, and a
//! `default-src 'none'` CSP that states it to the browser rather than only to
//! the tests. A signup page that pulls in a third party is a signup page a
//! third party can watch, and the one thing a user does here is decide whether
//! to hand over access to their mail.
//!
//! ONE FIRST-PARTY FETCH: the Newsreader webfont, served by this service
//! itself at [`FONT_PATH`] so the headings are the landing page's letterforms.
//! Same origin, so the CSP names `'self'` for fonts and nothing else.
//!
//! THE LOGO IS SUBJECT TO THAT RULE RATHER THAN AN EXCEPTION TO IT. These pages
//! carry the Passband masthead so that arriving from the landing page does not
//! feel like leaving the product, and the mark reaches them as an inline `<svg>`
//! element ([`MARK`]) because that is markup rather than a fetch: `img-src` is
//! still `'none'` and the CSP did not have to move an inch to let a logo in. The
//! wordmark's serif is the platform's, for the same reason there is no webfont
//! anywhere here.
//!
//! Everything interpolated goes through [`escape_html`], including strings that
//! were validated elsewhere: "this one was checked already" is how the
//! exception becomes the rule.
//!
//! COPY RULES (house style): the product is Passband, the daemon is squelchd,
//! and there are no em dashes in anything a person reads.

use crate::reconnect::{Purpose, ReconnectView};
use axum::{
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};

/// The Content-Security-Policy every page carries. The single allowance is the
/// inline `<style>`; `frame-ancestors 'none'` (with the older `X-Frame-Options`
/// beside it) keeps a signup decision from being framed inside someone else's
/// page. `form-action` names accounts.google.com as well as 'self' because
/// Chrome enforces form-action against the REDIRECT TARGET of a form
/// submission (a long-standing, spec-contested behavior): POST /signup answers
/// 303 to Google consent, and under `form-action 'self'` Chrome silently eats
/// that navigation - the button "does nothing" while the server logs a
/// perfectly healthy signup. Found live on the first real signup, 2026-08-11.
/// Microsoft's two sign-in hosts are named for the same reason: an Outlook
/// signup's 303 goes to `login.microsoftonline.com`, which bounces personal
/// accounts through `login.live.com`.
/// `font-src 'self'` admits exactly one file, [`NEWSREADER`], served by this
/// same origin: the landing page's serif, and still nothing a third party sees.
/// The mailbox products' marks for the signup's provider tiles, which are
/// named by product (Gmail, Outlook), not by sign-in company. Inline:
/// markup rather than a fetch, so the `default-src 'none'` CSP needs nothing
/// new. Each sits on a small white tile (`.mark`) so the brand colors read
/// on any background. Decorative: the tile's text names the product.
pub(crate) const GMAIL_MARK: &str = r##"<span class="mark" aria-hidden="true"><svg viewBox="52 42 88 66" width="17" height="13"><path fill="#4285F4" d="M58 108h14V74L52 59v43c0 3.32 2.69 6 6 6"/><path fill="#34A853" d="M120 108h14c3.32 0 6-2.69 6-6V59l-20 15"/><path fill="#FBBC04" d="M120 48v26l20-15v-8c0-7.42-8.47-11.65-14.4-7.2"/><path fill="#EA4335" d="M72 74V48l24 18 24-18v26L96 92"/><path fill="#C5221F" d="M52 51v8l20 15V48l-5.6-4.2c-5.94-4.45-14.4-.22-14.4 7.2"/></svg></span>"##;
pub(crate) const OUTLOOK_MARK: &str = r##"<span class="mark" aria-hidden="true"><svg viewBox="0 0 24 24" width="17" height="17"><rect x="9" y="4" width="14" height="16" rx="1.8" fill="#28A8EA"/><path d="M9.5 8.5 16 13l6.5-4.5" fill="none" stroke="#0A5EA8" stroke-width="1.3" stroke-linejoin="round"/><rect x="1" y="6" width="12" height="12" rx="1.8" fill="#0078D4"/><ellipse cx="7" cy="12" rx="2.7" ry="3.2" fill="none" stroke="#fff" stroke-width="1.7"/></svg></span>"##;

const CSP: &str = "default-src 'none'; style-src 'unsafe-inline'; font-src 'self'; form-action 'self' https://accounts.google.com https://login.microsoftonline.com https://login.live.com; frame-ancestors 'none'; base-uri 'none'";

/// Escape text for HTML.
pub fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// Percent-encode a query-parameter VALUE. Only the unreserved set survives,
/// so a label or a code can never break out of the parameter it sits in.
pub fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// The `passband://pair` deep link the native client claims.
///
/// Built HERE from this deployment's own tenant URL and the validated pairing
/// code, never echoed from the warden's answer: an `href` assembled from a
/// remote service's response is an open redirect with our domain in front of it.
pub fn deep_link(tenant_url: &str, pair_code: &str, analytics_id: Option<&str>) -> String {
    // `aid` is the person's opaque analytics id, riding along so the app can
    // adopt it as its PostHog distinct_id after a SUCCESSFUL pairing — the
    // join between the funnel and app behavior that never names an address.
    // Optional because a person can exist without the stamp having landed
    // (the fail-soft record_signup window), and the pairing must never wait
    // on analytics. Shipped app builds ignore unknown params, so the link
    // stays backward compatible. The encoder is the rule, not the content: a
    // UUID encodes to itself, and that is not a reason to skip it.
    match analytics_id {
        Some(aid) => format!(
            "passband://pair?url={}&code={}&aid={}",
            percent_encode(tenant_url),
            percent_encode(pair_code),
            percent_encode(aid)
        ),
        None => format!(
            "passband://pair?url={}&code={}",
            percent_encode(tenant_url),
            percent_encode(pair_code)
        ),
    }
}

/// Where the client is downloaded from.
///
/// THIS IS WHERE THE DOWNLOAD LIVES NOW. The landing page used to lead with it
/// and leads with the waitlist instead: an app is not worth holding before
/// there is a mailbox behind it to point at, so the download waits until the
/// end of this flow, which is the first moment somebody actually needs it. The
/// pages that name it are the two that finish a flow, [`success`] and
/// [`app_signed_in`], and on both of them it is the literal next step.
///
/// HARDCODED, NOT CONFIGURED, for the reason [`crate::resend::MARK_URL`] is:
/// there is one place the Mac client is published and it is not the operator's
/// origin. A self-hoster's own host serves no zips, so a variable pointing this
/// at their site would only ever produce a 404 with their domain in front of
/// it. `/download/latest` is the site's own redirect to whatever the appcast
/// says shipped last (`passband-site/server.ts`), so this URL does not move
/// when a release does.
const DOWNLOAD_URL: &str = "https://passband.app/download/latest";

/// Where a finished console login sends the browser: the tenant's own console,
/// carrying the pairing code it will claim.
///
/// BUILT HERE FROM TWO THINGS THIS DEPLOYMENT OWNS, and that is the whole reason
/// the console-login route takes no return URL: `tenant_url` comes from this
/// deployment's configured base domain and a label this crate validated, and the
/// code is the warden's answer, shape-checked and then percent-encoded. There is
/// no caller-supplied string anywhere in the result, so there is no open
/// redirect to find. A `?next=` parameter would have been one line and a
/// standing invitation.
pub fn console_callback_url(tenant_url: &str, pair_code: &str) -> String {
    format!(
        "{tenant_url}/console/callback?code={}",
        percent_encode(pair_code)
    )
}

/// The Passband mark, inline, in one colour.
///
/// INLINE AND MONOCHROME BECAUSE OF THE CSP AT THE TOP OF THIS FILE. `img-src`
/// is `'none'`, so there is no `<img src>` to point at `mark.svg` and no data
/// URI either; an inline `<svg>` element is markup rather than a fetch, so it is
/// the one way a logo reaches these pages without opening a hole for a third
/// party to be watching from. `currentColor` on both the fill and the stroke
/// makes the whole thing take its colour from CSS, which is what lets one copy
/// serve the light and dark grounds.
///
/// GENERATED, NOT DRAWN. The bars are `brand/svg/mark-mono.svg`'s verbatim, and
/// the curve is that file's path run through Douglas-Peucker at a tolerance of
/// one canvas unit: 350 points become 39, 4.5 kB becomes 0.3 kB, and at masthead
/// size (one unit is 0.025 px there) the two are the same picture. `brand/`
/// remains the source; if the geometry ever moves, this is regenerated from it
/// rather than edited.
///
/// THE SIMPLIFICATION KEEPS THE RULE `brand/generate.ts` FAILS ITS BUILD OVER:
/// no bar breaches the line, because the curve is the filter admitting the bars
/// and not a decoration laid over them. Checked against every bar across its
/// full width, the original clears by 0.59 units and this by 0.32; a cheaper
/// simplification that looked identical at 26 px put three bars through the
/// right shoulder, which is why the tolerance is where it is.
///
/// `aria-hidden` because the wordmark beside it already says Passband, and a
/// screen reader should not say it twice.
const MARK: &str = concat!(
    r#"<svg viewBox="0 221 1024 557" aria-hidden="true" fill="currentColor"><g>"#,
    r#"<rect x="129.8" y="697.3" width="44.4" height="74.7" rx="22.2"/>"#,
    r#"<rect x="189.8" y="600.7" width="44.4" height="171.3" rx="22.2"/>"#,
    r#"<rect x="249.8" y="550.1" width="44.4" height="221.9" rx="22.2"/>"#,
    r#"<rect x="309.8" y="420.4" width="44.4" height="351.6" rx="22.2"/>"#,
    r#"<rect x="369.8" y="297.4" width="44.4" height="474.6" rx="22.2"/>"#,
    r#"<rect x="429.8" y="370.0" width="44.4" height="402.0" rx="22.2"/>"#,
    r#"<rect x="489.8" y="253.1" width="44.4" height="518.9" rx="22.2"/>"#,
    r#"<rect x="549.8" y="329.5" width="44.4" height="442.5" rx="22.2"/>"#,
    r#"<rect x="609.8" y="356.3" width="44.4" height="415.7" rx="22.2"/>"#,
    r#"<rect x="669.8" y="362.2" width="44.4" height="409.8" rx="22.2"/>"#,
    r#"<rect x="729.8" y="555.9" width="44.4" height="216.1" rx="22.2"/>"#,
    r#"<rect x="789.8" y="619.1" width="44.4" height="152.9" rx="22.2"/>"#,
    r#"<rect x="849.8" y="699.3" width="44.4" height="72.7" rx="22.2"/>"#,
    r#"</g><path fill="none" stroke="currentColor" stroke-width="12" "#,
    r#"stroke-linecap="round" stroke-linejoin="round" d="#,
    r#"M-12 768L12 765L33 760L54 752L69 745L87 733L102 721L117 707L135 686"#,
    r#"L159 653L189 602L279 420L309 364L339 316L369 279L384 265L399 254"#,
    r#"L414 244L429 238L462 229L501 227L552 228L588 235L606 242L621 251"#,
    r#"L636 262L651 275L681 311L711 357L744 418L834 600L861 646L888 685"#,
    r#"L918 718L936 733L954 744L972 753L993 760L1035 768"/></svg>"#,
);

/// Where the Newsreader webfont is served. Same origin as every page here.
pub const FONT_PATH: &str = "/fonts/newsreader.woff2";

/// Newsreader, byte for byte the subset `passband-site/newsreader-var.woff2`
/// the landing page ships, vendored into this crate because the control image
/// is built from the crates alone and never sees `passband-site/`. If the
/// site's subset changes, copy it over again.
const NEWSREADER: &[u8] = include_bytes!("../assets/newsreader.woff2");

/// `GET /fonts/newsreader.woff2`. Immutable for a year: the bytes only change
/// with a deploy, and a changed font is a changed file worth a new path.
pub async fn newsreader() -> Response {
    (
        [
            (header::CONTENT_TYPE, "font/woff2"),
            (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        NEWSREADER,
    )
        .into_response()
}

/// The shell every page shares.
///
/// THE MASTHEAD IS PART OF THE SHELL, so every page carries it: the mark, then
/// the name in the brand's serif, top left. It is NOT A LINK, deliberately. The
/// obvious `href` is the marketing site, and the page this sits on hardest is
/// the one where somebody is deciding whether to hand over their mail; a way out
/// in the top left corner of that decision buys continuity with the landing page
/// at the cost of a door out of the flow, and the flow is the thing. Being
/// link-free also keeps it free of configuration, which is what lets the shell
/// carry it rather than every call site having to pass an origin down.
fn page(status: StatusCode, title: &str, body: &str) -> Response {
    render(status, title, body, Shell::Auth)
}

/// The same shell on the OPERATOR'S surface: the landing page's dark ground in
/// both palettes, its mark, its type, and none of its selling. See the
/// `body.console` block in the stylesheet for what that means and why the
/// colours are restated there rather than inherited from the dark media query.
///
/// The admin doors (sign in, confirm, cross-origin) and nothing else: one
/// panel, centred. The pages behind the door are [`console_app`]. A signup page
/// is read once by somebody deciding whether to hand over their mail; these are
/// read every day by one person who already decided.
pub(crate) fn console(status: StatusCode, title: &str, body: &str) -> Response {
    render(status, title, body, Shell::Console)
}

/// The console as an app: the masthead and `sidebar` in a column down the left,
/// `body` beside it. `sidebar` is markup the caller built and escaped; it rides
/// in under the masthead so the brand stays where every other page has it,
/// first in the document.
pub(crate) fn console_app(status: StatusCode, title: &str, sidebar: &str, body: &str) -> Response {
    render(status, title, body, Shell::App(sidebar))
}

/// Which frame [`render`] puts a page in.
#[derive(Clone, Copy)]
enum Shell<'a> {
    /// The signup and account pages: the masthead above the column.
    Auth,
    /// The admin doors.
    Console,
    /// The admin app, carrying its sidebar.
    App(&'a str),
}

/// The operator surface's own stylesheet, appended to the shell's only on the
/// pages [`console`] renders. See the file itself for what it is doing; it is a
/// file rather than more of the format string because it is long, and because
/// it is plain CSS with single braces that can be edited as CSS.
const CONSOLE_CSS: &str = include_str!("console.css");

/// The backdrop behind every console page: a glow and the analyzer's
/// graticule, fixed and still. `aria-hidden`, the same on every page, and NONE
/// of it data: the sign in door renders it too, and that page names no
/// waitlist, no counts, and no operator.
const CONSOLE_CHROME: &str =
    r#"<div class="fx" aria-hidden="true"><i class="glow"></i><i class="grat"></i></div>"#;

fn render(status: StatusCode, title: &str, body: &str, shell: Shell) -> Response {
    let title = escape_html(title);
    let masthead =
        format!(r#"<header class="brand">{MARK}<span class="wordmark">Passband</span></header>"#);
    let (body_class, opening, closing) = match shell {
        Shell::Auth => (r#" class="auth""#, format!("{masthead}<main>"), "</main>"),
        Shell::Console => (
            r#" class="console""#,
            format!("{CONSOLE_CHROME}<main>{masthead}"),
            "</main>",
        ),
        Shell::App(sidebar) => (
            r#" class="console app""#,
            format!(
                r#"{CONSOLE_CHROME}<div class="shell"><aside class="side">{masthead}{sidebar}</aside><main>"#
            ),
            "</main></div>",
        ),
    };
    let console_css = match shell {
        Shell::Console | Shell::App(_) => CONSOLE_CSS,
        Shell::Auth => "",
    };
    let html = format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="robots" content="noindex, nofollow">
<title>{title}</title>
<style>
/* THE BRAND ACCENT, stated once. `--brand` is the mark's ink ramp on a light
   ground and its lit ramp on a dark one (brand/README.md's palette), and it is
   the only colour on these pages that is the product's rather than the
   document's: the mark, the links, and the focus ring. The button stays neutral
   because the landing page has no primary button in this blue and inventing one
   here would match nothing. */
:root {{ color-scheme: light dark; --brand: #1f7099; }}
body {{ margin: 0; padding: 2.25rem 1.25rem 3rem; background: #fbfaf8; color: #1a1a1a;
  font: 1rem/1.55 ui-sans-serif, system-ui, -apple-system, "Segoe UI", Roboto, Helvetica, Arial, sans-serif; }}
main {{ max-width: 34rem; margin: 0 auto; }}
/* The masthead, aligned to the reading column rather than to the viewport, so
   it sits over the content instead of drifting off into the margin on a wide
   window. On a phone the two are the same corner. */
.brand {{ display: flex; align-items: center; gap: 0.55rem; margin: 0 0 2.25rem; }}
.brand svg {{ height: 1.5rem; width: auto; flex: none; color: var(--brand); }}
/* The brand's ONE serif moment, rationed the way the site and the Swift
   client's Typo ration it: Newsreader for the wordmark and nowhere else, since
   a display serif spread further becomes wallpaper. The webfont is served from
   this origin and declared once in this sheet (see `body.auth`), so every page
   that names it, the operator console's wordmark and heading included, gets it. */
.wordmark {{ font-family: "Newsreader", ui-serif, Georgia, "Times New Roman", serif;
  font-size: 1.32rem; font-weight: 500; letter-spacing: -0.005em; }}
/* The admin dashboard is the one page with a table, and the reading width the
   rest of the site is set to folds it into three cramped columns. */
main:has(table) {{ max-width: 44rem; }}
h1 {{ font-size: 1.6rem; font-weight: 600; letter-spacing: -0.02em; margin: 0 0 0.75rem; }}
h2 {{ font-size: 1.05rem; font-weight: 600; margin: 1.75rem 0 0.6rem; }}
p {{ margin: 0 0 1rem; }}
ul, ol {{ margin: 0 0 1.25rem; padding-left: 1.2rem; }}
li {{ margin: 0 0 0.5rem; }}
a {{ color: var(--brand); text-underline-offset: 0.15em; }}
label {{ display: block; font-weight: 600; margin: 0 0 0.35rem; }}
input[type=text], input[type=password], input[type=email] {{ width: 100%; box-sizing: border-box; padding: 0.6rem 0.7rem;
  margin: 0 0 1.25rem;
  border: 1px solid #cdc7bd; border-radius: 6px; background: #fff; color: inherit; font: inherit; }}
button {{ padding: 0.65rem 1.15rem; border: 0; border-radius: 6px; background: #1a1a1a; color: #fbfaf8;
  font: inherit; font-weight: 500; cursor: pointer; }}
/* One ring for every interactive thing, drawn OUTSIDE the control so it never
   changes the layout it lands on. Keyboard-only (`:focus-visible`), so clicking
   a button does not leave it haloed. */
:is(input, button, a, summary):focus-visible {{ outline: 2px solid var(--brand); outline-offset: 2px; }}
/* THE FORM IS THE PAGE. The signup form asks for two things and everything else
   on the page is context for them, so the fields sit on their own ground and
   the prose does not. */
.card {{ background: #fff; border: 1px solid #e6e1d9; border-radius: 10px;
  padding: 1.35rem 1.35rem 1.1rem; margin: 0 0 1.5rem; }}
.card > :last-child {{ margin-bottom: 0; }}
/* A field's hint belongs TO the input, not to the gap under it: the input's own
   bottom margin is dropped so the two read as one block. */
.field {{ margin: 0 0 1.25rem; }}
.field input {{ margin-bottom: 0.4rem; }}
.field .hint {{ margin: 0; }}
.hint {{ color: #6b6b6b; font-size: 0.9rem; }}
/* The second route off this page: a border rather than a second filled button,
   because there is exactly one primary action here and it is not this one. */
.alt {{ border: 1px solid #e6e1d9; border-radius: 10px; padding: 0.9rem 1.1rem;
  margin: 0 0 1.75rem; font-size: 0.95rem; }}
/* Collapsed by default and it stays that way on a submit, because a form post
   that fails re-renders a fresh page: whoever opened this read it already. */
details {{ border-top: 1px solid #e6e1d9; padding: 1rem 0 0; margin: 0 0 1.5rem; }}
/* The line under the button explains what pressing it does, so it sits close to
   it, but not touching: the button has no margin of its own. */
button + .hint {{ margin-top: 0.9rem; }}
summary {{ cursor: pointer; font-weight: 600; }}
details > :not(summary) {{ margin-top: 0.9rem; }}
table {{ width: 100%; border-collapse: collapse; margin: 0 0 1.5rem; font-size: 0.95rem; }}
th {{ font-size: 0.78rem; font-weight: 600; text-transform: uppercase; letter-spacing: 0.05em;
  color: #6b6b6b; }}
th, td {{ text-align: left; vertical-align: top; padding: 0.6rem 0.75rem 0.6rem 0;
  border-bottom: 1px solid #e6e1d9; }}
td form {{ display: inline; }}
/* The second button on a row: same shape, less pull. Re-sending is the rarer
   errand and must not read as the thing to click. */
button.quiet {{ background: none; color: inherit; border: 1px solid #cdc7bd;
  padding: 0.35rem 0.6rem; font-size: 0.85rem; font-weight: 400; }}
.muted {{ color: #6b6b6b; font-size: 0.9rem; }}
/* The name somebody typed into the public form, and it is deliberately NOT
   `.muted`: it sits in the same cell as the one line on this page that claims
   to come from Google, and the two must not be able to look alike. Italic and
   quoted is the "as claimed" register. */
.given {{ color: #6b6b6b; font-size: 0.9rem; font-style: italic; }}
/* A name is free text from a public form and an address can run to 254
   characters, so this is the one cell that a single unbroken token can push
   past the table's width, taking the buttons off the right edge with it. The
   same rule `code` already carries, for the same reason. */
td.who {{ overflow-wrap: anywhere; }}
.suffix {{ color: #6b6b6b; }}
.stop {{ border-left: 3px solid #b3261e; padding: 0.1rem 0 0.1rem 0.85rem; }}
.code {{ font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 1.5rem;
  letter-spacing: 0.08em; background: #efece7; padding: 0.4rem 0.7rem; border-radius: 6px;
  display: inline-block; user-select: all; }}
code {{ font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 0.9em;
  background: #efece7; padding: 0.1em 0.35em; border-radius: 3px; word-break: break-all; }}
a.button {{ display: inline-block; margin: 0.25rem 0 1.25rem; padding: 0.65rem 1.15rem;
  background: #1a1a1a; color: #fbfaf8; text-decoration: none; border-radius: 6px; font-weight: 500; }}
/* THE DARK GROUND IS THE LANDING PAGE'S GROUND, value for value: #0f0f10 under
   #f5f5f7, cards and inputs as the white washes it builds them from, and the
   mark's lit ramp for the accent. Almost everybody who arrives here arrived
   from that page, which has no light mode at all, so this is the transition
   they actually see. The light ground stays the warm paper it was: there is no
   landing page in light mode to match it to. */
@media (prefers-color-scheme: dark) {{
  :root {{ --brand: #7cc8eb; }}
  body {{ background: #0f0f10; color: #f5f5f7; }}
  .muted, .given, .suffix, .hint, th {{ color: #a0a0a7; }}
  code, .code {{ background: rgba(255, 255, 255, 0.07); }}
  input[type=text], input[type=password], input[type=email] {{ background: rgba(255, 255, 255, 0.04);
    border-color: rgba(255, 255, 255, 0.14); }}
  button, a.button {{ background: #f5f5f7; color: #0f0f10; }}
  button.quiet {{ background: none; color: #f5f5f7; border-color: rgba(255, 255, 255, 0.14); }}
  th, td {{ border-bottom-color: rgba(255, 255, 255, 0.11); }}
  .stop {{ border-left-color: #f2b8b5; }}
  .card {{ background: rgba(255, 255, 255, 0.055); border-color: rgba(255, 255, 255, 0.11); }}
  .alt, details {{ border-color: rgba(255, 255, 255, 0.11); }}
}}
/* THE ACCOUNT PAGES WEAR THE LANDING PAGE (passband-site/App.tsx), token for
   token: its night ground, its analyzer graticule, Newsreader for the words
   that matter, and ONE material for controls, the dark machined "rig" edged in
   passband blue. No highlight line along one edge: on a rounded control it
   bends around the corners into an arc, which reads as generated UI. Somebody arrives here from that
   page, and the signup should read as the next screen of it rather than as a
   form somewhere else.
   DARK BY DEFAULT, like the landing page, which has no light mode at all.
   `?theme=light` still works (the app passes its own appearance), and restates
   the tokens below rather than inheriting anything. */
@font-face {{ font-family: "Newsreader"; src: url("{font_path}") format("woff2");
  font-weight: 200 800; font-style: normal; font-display: swap; }}
body.auth {{ color-scheme: dark;
  --bg: #090D16; --ink: #E9EEF5; --dim: #A8B4C4; --faint: #808E9F; --faintest: #62707F;
  --hair: rgba(233, 238, 245, 0.08); --accent: #4E9BEA; --brand: #82BAF5; --danger: #FF7A68;
  --grid: rgba(130, 186, 245, 0.06); --rig-top: #152032; --rig-bottom: #0c121c;
  --rig-edge: rgba(78, 155, 234, 0.22); --rig-lit: rgba(78, 155, 234, 0.62);
  --rig-glow: rgba(78, 155, 234, 0.45); --rig-shadow: rgba(0, 0, 0, 0.5);
  --slot-rule: rgba(78, 155, 234, 0.18);
  --cta-ink: #dfe8f3; --cta-ink-lit: #f4f8fd; --autofill: #101826;
  --serif: "Newsreader", ui-serif, Georgia, serif;
  --sans: -apple-system, BlinkMacSystemFont, "SF Pro Text", system-ui, sans-serif;
  --gutter: clamp(1rem, 4vw, 2.75rem);
  margin: 0; min-height: 100vh; padding: 1.4rem var(--gutter) 4rem; box-sizing: border-box;
  background: var(--bg); color: var(--ink); font-family: var(--sans);
  -webkit-font-smoothing: antialiased; }}
body.auth[data-theme="light"] {{ color-scheme: light;
  --bg: #F6F8FB; --ink: #141C26; --dim: #455465; --faint: #5F6D7E; --faintest: #7B8796;
  --hair: rgba(20, 28, 38, 0.1); --accent: #1C63AE; --brand: #1C63AE; --danger: #B3261E;
  --grid: rgba(28, 99, 174, 0.07); --rig-top: #FFFFFF; --rig-bottom: #F3F6FA;
  --rig-edge: rgba(28, 99, 174, 0.24); --rig-lit: rgba(28, 99, 174, 0.6);
  --rig-glow: rgba(28, 99, 174, 0.22); --rig-shadow: rgba(20, 28, 38, 0.1);
  --slot-rule: rgba(28, 99, 174, 0.16);
  --cta-ink: #141C26; --cta-ink-lit: #0B1C33; --autofill: #FFFFFF; }}
/* The analyzer's graticule, faded out toward the edges exactly as the landing
   page fades it, so the ground has texture and no border. */
body.auth::before {{ content: ""; position: fixed; inset: 0; z-index: -1; pointer-events: none;
  background-image:
    repeating-linear-gradient(90deg, var(--grid) 0 1px, transparent 1px 10%),
    repeating-linear-gradient(180deg, var(--grid) 0 1px, transparent 1px 12.5%);
  -webkit-mask-image: radial-gradient(ellipse 70% 65% at 50% 45%, #000 20%, transparent 80%);
          mask-image: radial-gradient(ellipse 70% 65% at 50% 45%, #000 20%, transparent 80%); }}
.auth > .brand {{ margin: 0; gap: 0.6rem; }}
.auth > .brand svg {{ height: 1.45rem; color: var(--brand); }}
.auth .wordmark {{ font-family: var(--serif); font-size: 1.5rem; }}
.auth main {{ max-width: 28rem; margin: clamp(3.5rem, 13vh, 8rem) auto 0; overflow-wrap: anywhere; }}
.auth h1 {{ font-family: var(--serif); font-weight: 500; font-size: clamp(2.2rem, 5vw, 2.9rem);
  line-height: 1.05; letter-spacing: -0.01em; margin: 0 0 0.9rem; }}
.auth p {{ color: var(--dim); }}
.auth .lead {{ font-size: 1rem; line-height: 1.6; margin: 0 0 2rem; }}
.auth a {{ color: var(--brand); }}
.auth :is(.hint, .muted, .suffix) {{ color: var(--faintest); font-size: 0.8rem; }}
.auth .alt {{ border: 0; padding: 0; margin: 1.5rem 0 0; font-size: 0.85rem; color: var(--faint); }}
.auth .stop {{ border: 0; padding: 0; margin: 0 0 1.25rem; color: var(--danger); font-size: 0.9rem; }}
.auth .stop strong {{ font-weight: 500; }}
.auth :is(code, .code) {{ background: var(--hair); color: var(--ink); }}
/* THE MATERIAL. One recipe for every control on these pages: a standalone
   button is a small piece of it, and the form is a bigger piece with slots
   cut in it. Lit as one instrument on hover or focus: the whole edge and a
   glow, never one side of it. */
.auth :is(.rig, a.button, button) {{ position: relative; isolation: isolate; overflow: hidden;
  border-radius: 0.9rem; border: 1px solid var(--rig-edge);
  background: linear-gradient(180deg, var(--rig-top), var(--rig-bottom));
  box-shadow: 0 12px 30px var(--rig-shadow);
  transition: transform 0.4s cubic-bezier(0.2, 0.8, 0.2, 1), border-color 0.4s ease,
    box-shadow 0.4s ease, color 0.4s ease; }}
.auth :is(.rig:hover, .rig:focus-within, a.button:hover, a.button:focus-visible, button:hover, button:focus-visible) {{
  border-color: var(--rig-lit);
  box-shadow: 0 0 34px -6px var(--rig-glow), 0 16px 38px var(--rig-shadow); }}
.auth :is(a.button, button) {{ display: inline-flex; align-items: center; justify-content: center;
  gap: 0.6rem; width: auto; margin: 0; padding: 0.95rem 1.9rem; box-sizing: border-box;
  color: var(--cta-ink); font: 600 0.95rem/1.2 var(--sans); letter-spacing: 0.015em;
  text-decoration: none; cursor: pointer; }}
.auth :is(a.button, button):is(:hover, :focus-visible) {{ color: var(--cta-ink-lit); transform: translateY(-2px); }}
.auth :is(a.button, button):active {{ transform: translateY(0) scale(0.995); }}
.auth :is(a.button, button):focus-visible {{ outline: 2px solid var(--rig-lit); outline-offset: 3px; }}
.auth .arrow {{ transition: transform 0.4s cubic-bezier(0.2, 0.8, 0.2, 1); }}
/* A provider's mark: one white tile, the same size on every button, so the
   brand colors read on the dark primary and the outlined secondary alike. */
.mark {{ display: inline-flex; align-items: center; justify-content: center; flex: none;
  width: 1.5rem; height: 1.5rem; border-radius: 0.4rem; background: #fff;
  box-shadow: 0 0 0 1px rgba(0, 0, 0, 0.08); }}
.mark svg {{ display: block; }}

.auth :is(a.button, button):is(:hover, :focus-visible) .arrow {{ transform: translateX(3px); }}
/* The form: slots cut into one rig, each with its label engraved in it, and
   the submit as the rig's last slot rather than a button sitting under it. */
.auth .rig {{ display: flex; flex-direction: column; margin: 0; padding: 0; }}
.auth .slot {{ position: relative; margin: 0; padding: 0.8rem 1.1rem 0.85rem;
  border-bottom: 1px solid var(--slot-rule); }}
.auth .slot:focus-within > label:first-child {{ color: var(--brand); }}
/* A code that came filled in from the emailed link: out of the way until the
   link under the form targets it. The link goes when the field comes back. */
.auth .slot.folded:not(:target) {{ display: none; }}
.auth .sr {{ position: absolute; width: 1px; height: 1px; overflow: hidden;
  clip-path: inset(50%); white-space: nowrap; }}
.auth main:has(.slot.folded:target) .swap {{ display: none; }}
.auth .swap a {{ color: var(--faint); }}
.auth .slot > label:first-child {{ display: block; margin: 0 0 0.3rem; color: var(--faintest);
  font-size: 0.68rem; font-weight: 600; letter-spacing: 0.14em; text-transform: uppercase; }}
.auth .slot input {{ display: block; width: 100%; margin: 0; padding: 0; border: 0; border-radius: 0;
  background: none; outline: none; color: var(--ink); font: 1rem/1.4 var(--sans); }}
.auth .slot input::placeholder {{ color: var(--faintest); }}
.auth .slot input:-webkit-autofill {{ -webkit-text-fill-color: var(--ink); caret-color: var(--ink);
  box-shadow: inset 0 0 0 100vw var(--autofill); transition: background-color 9999s; }}
.auth .rig button {{ width: 100%; padding: 1rem 1.1rem; border: 0; border-radius: 0;
  background: none; box-shadow: none; }}
.auth .rig button:is(:hover, :focus-visible) {{ transform: none; box-shadow: none;
  background: linear-gradient(180deg, transparent, var(--slot-rule)); }}
.auth .rig button:focus-visible {{ outline-offset: -3px; }}
/* THE PROVIDERS: their own rig under the code and username, the label engraved
   like a slot's, then equal tiles, two to a row, each named by product.
   More providers wrap into the grid; a lone one spans the row. */
.auth form.signup {{ display: flex; flex-direction: column; gap: 0.9rem; margin: 0; }}
.auth .providers .rig-label {{ display: block; padding: 0.8rem 1.1rem 0.7rem; color: var(--faintest);
  font-size: 0.68rem; font-weight: 600; letter-spacing: 0.14em; text-transform: uppercase;
  border-bottom: 1px solid var(--slot-rule); }}
.auth .provider-grid {{ display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); }}
.auth .provider-grid button:only-child {{ grid-column: 1 / -1; }}
.auth .provider-grid button {{ gap: 0.65rem; padding: 1.05rem 1rem; font-size: 1rem; }}
.auth .provider-grid button:nth-child(2n) {{ border-left: 1px solid var(--slot-rule); }}
.auth .provider-grid button:nth-child(n + 3) {{ border-top: 1px solid var(--slot-rule); }}
/* The username slot: the input and its domain on one line, the domain in the
   placeholder's grey riding right behind what is typed. See [`signup_form`]
   for why this needs no script. */
.auth label.affix {{ display: flex; align-items: baseline; margin: 0; padding: 0; cursor: text;
  font: inherit; color: inherit; letter-spacing: normal; text-transform: none; }}
.auth .affix input {{ flex: 1 1 auto; width: 100%; min-width: 1ch; text-transform: lowercase; }}
@supports (field-sizing: content) {{
  .auth .affix input {{ flex: 0 1 auto; width: auto; field-sizing: content; }}
}}
.auth .affix .domain {{ flex: none; white-space: nowrap; color: var(--faintest); }}
.auth details {{ margin: 1rem 0 0; padding: 0; border: 0; color: var(--faint); font-size: 0.8rem; }}
.auth summary {{ color: var(--faintest); font-weight: 400; list-style: none; }}
.auth summary::-webkit-details-marker {{ display: none; }}
.auth summary::after {{ content: "Why?"; margin-left: 0.3em; color: var(--faint); text-decoration: underline;
  text-underline-offset: 0.15em; }}
.auth details[open] summary::after {{ content: ""; }}
.auth summary:hover::after {{ color: var(--ink); }}
.auth details ul {{ margin: 0.6rem 0 0.5rem; }}
.auth details li {{ margin: 0 0 0.25rem; }}
.auth details p {{ color: var(--faint); margin: 0; }}
.auth details strong {{ color: var(--ink); font-weight: 500; }}
@media (max-width: 600px) {{
  .auth main {{ margin-top: 3rem; }}
}}
@media (prefers-reduced-motion: reduce) {{
  .auth :is(.rig, a.button, button, .arrow) {{ transition-duration: 0.01ms; }}
  .auth :is(a.button, button):is(:hover, :focus-visible) {{ transform: none; }}
  .auth :is(a.button, button):is(:hover, :focus-visible) .arrow {{ transform: none; }}
}}
{console_css}</style>
</head>
<body{body_class} data-theme="{theme}">{opening}
{body}{closing}</body>
</html>
"#,
        title = title,
        opening = opening,
        font_path = FONT_PATH,
        theme = crate::appearance::current().name(),
        body = body,
        body_class = body_class,
        closing = closing,
    );
    (
        status,
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            // These pages carry a pairing code, name a mailbox, and on the
            // admin side list addresses that belong to people who are not
            // customers. A shared cache must not keep them, and the click
            // through to Google must not carry this URL as a referer.
            (header::CACHE_CONTROL, "no-store, no-cache"),
            (header::REFERRER_POLICY, "no-referrer"),
            (header::CONTENT_SECURITY_POLICY, CSP),
            (header::X_FRAME_OPTIONS, "DENY"),
        ],
        html,
    )
        .into_response()
}

/// The signup form.
///
/// TWO FIELDS AND ONE BUTTON ARE THE PAGE, and everything else is arranged
/// around that. The fields are slots cut into one rig, the landing page's
/// control material. The
/// username hint is tied to its input with `aria-describedby`; privacy details
/// stay available in a collapsed disclosure.
///
/// THE DOMAIN TRAILS THE TYPING WITHOUT A SCRIPT. The username input and a
/// greyed `.{domain}` sit together inside one bordered `.affix` box, and
/// `field-sizing: content` shrinks the input to what has been typed (or to its
/// placeholder), so the suffix rides right behind the cursor. A browser without
/// `field-sizing` lets the input fill the box instead and the suffix sits at
/// the right edge, which still says which namespace is being claimed. The box
/// is a second `<label>` so a click on the suffix lands in the field.
///
/// A CODE THAT ARRIVED FILLED IN IS FOLDED AWAY, on that first render only. Somebody who clicked the
/// emailed link has nothing to do with the invite field, so it is hidden and
/// the form is one field. It is still in the form (a `display: none` input
/// submits like any other), and a quiet link under the form reveals it through
/// `:target`, which again needs no script. It unfolds by itself when the code
/// is the thing that was refused, because a field somebody has to fix cannot be
/// one they have to find first. After any refused submit the field is shown,
/// because by then the code may be one the person typed.
///
/// THE GRANT IS STILL STATED BESIDE THE BUTTON, in one line naming all three
/// required permissions and the optional calendar, because the consent screen that follows is Google's and says it
/// in Google's vocabulary. That line is the `<summary>` of the disclosure, so
/// it is always on screen and is also the way into the detail. The username
/// rule is not printed: the server's refusal states whichever rule was broken,
/// and a screen reader gets it up front from a visually hidden hint. No
/// `pattern` on the field, because the server trims and case-folds first and a
/// browser pattern would refuse `ada ` (a predictive-text space) that the
/// server accepts, with a tooltip that cannot say which rule it meant. What moved is the LONG version: three paragraphs of
/// scope detail above the fold pushed the form off a phone screen and read as
/// the page's subject, which it is not. It is a `<details>` now, shut by
/// default, open to anybody who wants it, and made of the same markup as before
/// (native disclosure, no JavaScript, nothing to fetch).
///
/// THE COPY IS DELIBERATELY SHORT, and the cuts were length rather than
/// substance. Every disclosure this page ever made it still makes: all three
/// scope names, what each one is for, that a partial grant is refused, and that
/// we hold the refresh token with self-hosting as the way out. What went is the
/// prose around them, because a wall of text in front of a two-field form is
/// read by nobody and so discloses nothing. Anything added here should be a
/// clause, not a paragraph.
///
/// `waitlist_url` is THE OTHER WAY OFF THIS PAGE: somebody who has no invite
/// code cannot get one here, and until this link existed their only move was to
/// guess at the field or close the tab. It is rendered UNCONDITIONALLY when the
/// waitlist is configured, never only on a refusal, so it answers nothing about
/// whether the code that was just typed exists. `None` (a deployment with no
/// waitlist configured) renders no link rather than a dead one.
///
/// `error` re-renders the form with what went wrong; `label` and `invite` are
/// echoed back so a person does not retype the half that was fine. The invite
/// code is echoed only because the browser already has it, and it is escaped
/// like everything else.
pub fn signup_form(
    base_domain: &str,
    waitlist_url: Option<&str>,
    label: &str,
    invite: &str,
    error: Option<&str>,
) -> Response {
    signup_form_with(base_domain, waitlist_url, label, invite, error, false)
}

/// [`signup_form`], with the Outlook button when this deployment has the
/// Microsoft app (`outlook`). The two buttons submit the same form; the one
/// pressed names the company in `provider`.
pub fn signup_form_with(
    base_domain: &str,
    waitlist_url: Option<&str>,
    label: &str,
    invite: &str,
    error: Option<&str>,
    outlook: bool,
) -> Response {
    let error_html = stop_note(error);
    // Folded only on the first render of an emailed link: a filled-in code
    // and nothing refused yet. Any re-render after a submit shows the code, so
    // a code somebody typed (or just changed) is never hidden behind an error
    // about something else.
    let folded = !invite.trim().is_empty() && error.is_none();
    let (invite_class, invite_focus, label_focus) = if folded {
        ("slot folded", "", " autofocus")
    } else {
        ("slot", " autofocus", "")
    };
    // The quiet row under the form: the way out for somebody with no code, and
    // the way back to a folded one. The waitlist half is built from the
    // configured origin, which this crate owns, and escaped anyway, because the
    // rule is that nothing is interpolated raw.
    let waitlist = waitlist_url.map(|url| {
        format!(
            r#"No invite code? <a href="{url}">Join the waitlist</a>"#,
            url = escape_html(url),
        )
    });
    let swap = folded.then(|| {
        let dot = if waitlist.is_some() { " · " } else { "" };
        format!(r##"<span class="swap">{dot}<a href="#code">Use a different code</a></span>"##)
    });
    let links_html = if waitlist.is_none() && swap.is_none() {
        String::new()
    } else {
        format!(
            r#"<p class="alt">{}{}</p>"#,
            waitlist.unwrap_or_default(),
            swap.unwrap_or_default()
        )
    };
    page(
        StatusCode::OK,
        "Set up Passband",
        &format!(
            r#"<h1>Set up Passband</h1>
<p class="lead">Pick a username, then connect {mailboxes}.</p>
{error_html}
<form class="signup" method="post" action="/signup">
<div class="rig">
<div class="{invite_class}" id="code">
<label for="invite">Invite code</label>
<input type="text" id="invite" name="invite" value="{invite}" placeholder="XXXX-XXXX-XXXX-XXXX"
  autocomplete="off" autocapitalize="off" spellcheck="false"{invite_focus} required>
</div>
<div class="slot">
<label for="label">Your Passband username</label>
<label class="affix"><input type="text" id="label" name="label" value="{label}" placeholder="yourname"
  aria-describedby="label-hint" maxlength="{max}" autocomplete="off" autocapitalize="off" spellcheck="false"{label_focus} required><span class="domain">.{domain}</span></label>
<span class="sr" id="label-hint">{min} to {max} lowercase letters, numbers or hyphens.</span>
</div>
</div>
<div class="rig providers" role="group" aria-labelledby="providers-label">
<span class="rig-label" id="providers-label">Connect your mail</span>
<div class="provider-grid">
<button type="submit" name="provider" value="google" aria-label="Connect Gmail">{gmail_mark}<span>Gmail</span></button>
{outlook_button}</div>
</div>
</form>
<details>
<summary>{consent_summary}</summary>
<ul>
<li><strong>Read</strong>: mail triage.</li>
<li><strong>Change</strong>: archiving, labeling and moving to trash, never permanent deletion.</li>
<li><strong>Send</strong>: replying from the app.</li>
</ul>
<p>All three come from one Google permission, <code>gmail.modify</code>, so keep that box checked. It also asks for your name and profile photo, to show them in the app, and for Google Calendar (<code>calendar.events</code>), which can view and edit your events; both are optional. Calendar is asked for now so it never needs a second consent, and nothing touches your calendar until calendar is available on your account. Your Google refresh token is stored encrypted so mail keeps syncing; self-host to hold it yourself.</p>
{outlook_note}</details>
{links_html}"#,
            error_html = error_html,
            links_html = links_html,
            mailboxes = if outlook { "Gmail or Outlook" } else { "Gmail" },
            gmail_mark = GMAIL_MARK,
            consent_summary = if outlook {
                "Google or Microsoft will ask to read, change, and send your mail, and for your calendar."
            } else {
                "Google will ask to read, change, and send your Gmail, and optionally for your calendar."
            },
            outlook_button = if outlook {
                format!(
                    r#"<button type="submit" name="provider" value="microsoft" aria-label="Connect Outlook">{OUTLOOK_MARK}<span>Outlook</span></button>
"#
                )
            } else {
                String::new()
            },
            outlook_note = if outlook {
                r#"<p>With Microsoft, Outlook asks once to read, change, and send your mail and to view and edit your calendar (<code>Mail.ReadWrite</code>, <code>Mail.Send</code>, <code>Calendars.ReadWrite</code>), plus your profile and staying signed in. Your Microsoft refresh token is stored encrypted the same way.</p>
"#
            } else {
                ""
            },
            invite_class = invite_class,
            invite_focus = invite_focus,
            label_focus = label_focus,
            invite = escape_html(invite),
            label = escape_html(label),
            domain = escape_html(base_domain),
            min = crate::labels::MIN_LABEL_LEN,
            max = crate::labels::MAX_LABEL_LEN,
        ),
    )
}

/// The end of the signup flow: the tenant exists and the daemon is running.
///
/// ONE BUTTON, THE DOWNLOAD. Somebody arriving from an invite email has no app
/// yet, and the app's own onboarding signs in with Gmail and pairs itself
/// through [`app_signed_in`], so this page carries no deep link and no pairing
/// code: a second connect path here was a step the app already does.
pub fn success(tenant_url: &str) -> Response {
    let host = tenant_url
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    page(
        StatusCode::OK,
        "Your mailbox is ready",
        &format!(
            r#"<h1>Your mailbox is ready</h1>
<p class="lead">{host} is live. Get the app, then sign in to finish.</p>
<a class="button" href="{download}">Download Passband for Mac <span class="arrow" aria-hidden="true">→</span></a>"#,
            host = escape_html(host),
            download = DOWNLOAD_URL,
        ),
    )
}

/// Where an app login ends: signed in, and one press from a connected app.
///
/// THE DEEP LINK IS THE PAGE. Everything else on it exists for the cases the
/// link cannot cover: a browser that will not hand a custom scheme to the OS, a
/// sign in finished on a phone for a Mac, an app that is not installed yet. So
/// the code and the server are on it as `user-select: all` text too: the link
/// is the fast path and typing is the path that always works.
///
/// They are folded into a disclosure now rather than standing open, and the
/// summary carries what the old standing `<h2>If that button does nothing</h2>`
/// carried: the SYMPTOM. Every case in the paragraph above arrives here the same
/// way, by pressing the button and watching nothing happen, so that is what the
/// one line they will read has to say. A summary reading "connect manually"
/// describes the remedy to somebody who has not yet worked out they need one.
///
/// THE PAGE OPENS THE APP BY ITSELF, through a `Refresh` header, because the CSP
/// allows no script. This used to
/// be a press, on the argument that a live pairing code should not go to
/// whatever claims `passband://` before anybody agreed to it. In practice
/// nobody reached this page except from the app's own "Sign in with Gmail",
/// so the press was a leftover tab with one obvious button on it; and the
/// browser asks before it hands a custom scheme to the OS anyway, which is
/// the agreement the press stood for. The button stays as the fallback, for
/// the cases the opening paragraph lists, where the redirect does nothing.
///
/// The mailbox is named on it because this is the one screen that can say it: an
/// app login never asked who the person was, so "signed in as" is the only
/// confirmation they get that Google picked the account they meant.
pub fn app_signed_in(
    account_email: &str,
    tenant_url: &str,
    pair_code: &str,
    minutes: i64,
    analytics_id: Option<&str>,
) -> Response {
    let link = deep_link(tenant_url, pair_code, analytics_id);
    let mut response = page(
        StatusCode::OK,
        "Signed in",
        &format!(
            r#"<h1>You’re signed in</h1>
<p class="lead">Opening Passband for {email}…</p>
<a class="button" href="{link}">Open Passband <span class="arrow" aria-hidden="true">→</span></a>
<p class="alt">Need the app? <a href="{download}">Download Passband</a>.</p>
<details>
<summary>Passband didn’t open? Connect manually</summary>
<p>In Passband, choose Sign in with Gmail or Outlook and enter:</p>
<p><code>{url}</code></p>
<p><span class="code">{code}</span></p>
<p>Works once, expires in {minutes} minutes. If it lapses, sign in again.</p>
</details>"#,
            email = escape_html(account_email),
            url = escape_html(tenant_url),
            code = escape_html(pair_code),
            link = escape_html(&link),
            download = DOWNLOAD_URL,
        ),
    );
    // Every byte of the link is ours and percent-encoded (see [`deep_link`]),
    // so it is always a legal header value; a failure here would be a bug in
    // the encoder, and the page still has its button.
    if let Ok(refresh) = header::HeaderValue::from_str(&format!("0; url={link}")) {
        response.headers_mut().insert(header::REFRESH, refresh);
    }
    response
}

/// The reconnect status page, one arm per [`ReconnectView`]. The states the
/// worker can still move carry a `Refresh` header, so the page reloads with no
/// script and without replaying the OAuth callback; the rest stop and offer
/// the one next action that fits.
///
/// EXHAUSTIVE ON PURPOSE. This page's failure mode is a state that reads as
/// "still working" forever, which is the bug the whole worker exists to end,
/// and a `_` arm would let a new terminal state ship straight into it.
///
/// `purpose` picks the words, never the behaviour: a calendar grant installs
/// exactly as a reconnect does, so the states, the refresh and the links are
/// the same list, and a view the row could not be read for is told in the
/// reconnect's neutral words.
pub fn reconnect_result(view: ReconnectView, purpose: Purpose) -> Response {
    let (heading, detail, link, button) = match purpose {
        Purpose::Reconnect => reconnect_words(view),
        Purpose::Calendar => calendar_words(view),
    };
    let mut response = page(
        StatusCode::OK,
        heading,
        &format!(
            "<h1>{}</h1><p class=\"lead\" role=\"status\">{}</p><a class=\"button\" href=\"{}\">{} <span class=\"arrow\" aria-hidden=\"true\">→</span></a>",
            escape_html(heading),
            escape_html(detail),
            escape_html(link),
            escape_html(button),
        ),
    );
    if view.refreshes() {
        response.headers_mut().insert(
            header::REFRESH,
            header::HeaderValue::from_static("5; url=/reconnect/status"),
        );
    }
    response
}

/// `(heading, detail, link, button)` for a plain reconnect.
fn reconnect_words(
    view: ReconnectView,
) -> (&'static str, &'static str, &'static str, &'static str) {
    match view {
        ReconnectView::Pending => (
            "Reconnecting your mailbox…",
            "We’re restarting your mailbox with your new Google connection. This can take a few minutes. You can close this page; reconnecting will continue.",
            "/reconnect/status",
            "Check progress",
        ),
        ReconnectView::Retrying => (
            "Reconnecting is taking longer than usual",
            "We’re still working on your connection and will keep trying automatically. You can close this page and return later.",
            "/reconnect/status",
            "Check progress",
        ),
        ReconnectView::Checking => (
            "Checking your reconnection…",
            "We’re checking the latest progress. This page will update automatically.",
            "/reconnect/status",
            "Check progress",
        ),
        ReconnectView::Complete => (
            "You’re reconnected",
            "Your mailbox is connected to Google again. Open Passband to see your mail as syncing resumes.",
            "passband://open",
            "Open Passband",
        ),
        // NEUTRAL ON PURPOSE. A refusal is the cluster disagreeing with this
        // service about who owns the mailbox or whether it is running, and
        // which of those it was is not the browser's to learn (the same rule
        // the pre-check in `handlers` keeps). Nor is "use the right account"
        // honest here: the account that reached this page is the one the
        // store says owns the mailbox.
        ReconnectView::Refused => (
            "Your mailbox couldn’t be reconnected",
            "Passband couldn’t reconnect this mailbox, and nothing changed. Open Passband to check on it; if it still needs reconnecting in a few minutes, start again from there.",
            "passband://open",
            "Open Passband",
        ),
        ReconnectView::GaveUp => (
            "Reconnecting didn’t finish",
            "We kept trying for a while but couldn’t confirm your new Google connection. Nothing needs undoing. Try again, or open Passband to see whether mail is arriving.",
            "/reconnect",
            "Try again",
        ),
        ReconnectView::Expired => (
            "This reconnect page has expired",
            "Open Passband to check your mailbox. If it needs reconnecting, start a new connection.",
            "passband://open",
            "Open Passband",
        ),
    }
}

/// `(heading, detail, link, button)` for the calendar grant. The same states
/// as [`reconnect_words`], and the same neutrality on a refusal: which record
/// disagreed is not the browser's to learn. Every link is a constant.
fn calendar_words(view: ReconnectView) -> (&'static str, &'static str, &'static str, &'static str) {
    match view {
        ReconnectView::Pending => (
            "Connecting your calendar…",
            "We’re restarting your mailbox with calendar access. This can take a few minutes. You can close this page; connecting will continue.",
            "/reconnect/status",
            "Check progress",
        ),
        ReconnectView::Retrying => (
            "Connecting your calendar is taking longer than usual",
            "We’re still working on it and will keep trying automatically. You can close this page and return later.",
            "/reconnect/status",
            "Check progress",
        ),
        ReconnectView::Checking => (
            "Checking your calendar connection…",
            "We’re checking the latest progress. This page will update automatically.",
            "/reconnect/status",
            "Check progress",
        ),
        ReconnectView::Complete => (
            "Calendar is connected",
            "Calendar is connected. You can close this tab and go back to Passband.",
            "passband://open",
            "Open Passband",
        ),
        ReconnectView::Refused => (
            "Your calendar couldn’t be connected",
            "Passband couldn’t connect your calendar, and nothing about your mail changed. Open Passband to try again later.",
            "passband://open",
            "Open Passband",
        ),
        ReconnectView::GaveUp => (
            "Connecting your calendar didn’t finish",
            // Back to the app, not a bare `/reconnect?calendar=1`: the app's
            // own link carries the mailbox hint, and a retry started here
            // would run without the wrong-account guard.
            "We kept trying for a while but couldn’t confirm the new connection. Nothing needs undoing. Go back to Passband and connect your calendar again from there.",
            "passband://open",
            "Open Passband",
        ),
        // Unreachable in practice (an expired view has no row to read a
        // purpose from) and answered in the calendar's words anyway.
        ReconnectView::Expired => (
            "This page has expired",
            "Open Passband to check on your calendar. If it is still not connected, start again from there.",
            "passband://open",
            "Open Passband",
        ),
    }
}

/// The page BOTH logins get when they are refused: the same shell as [`problem`]
/// with NO link out.
///
/// Two reasons it is not [`problem`]. The link there goes to the signup form,
/// which is the wrong place to send somebody who already has a mailbox and was
/// trying to sign in to it. And the only honest link back would be built from
/// the label this request named, which on a uniform refusal is a label that may
/// not exist: rendering it would answer, in an `href`, the question the refusal
/// exists to leave unanswered. An app login has it worse still: it never named a
/// label at all, so there is not even a wrong link to render.
pub fn console_problem(status: StatusCode, heading: &str, detail: &str) -> Response {
    page(
        status,
        heading,
        &format!(
            r#"<h1>{heading}</h1>
<p class="lead">{detail}</p>"#,
            heading = escape_html(heading),
            detail = escape_html(detail),
        ),
    )
}

/// The console's problem page WITH the one link that is always right on it:
/// back to the console this sign in was for.
///
/// Used where the label is one this service MINTED into a session of its own
/// (an expired console login, a replayed callback, a cookie that does not match
/// its session), so the `href` echoes nothing a stranger chose and answers
/// nothing about which addresses exist: it is the address the person already
/// typed, handed back. [`console_problem`] stays link-free for the refusals that
/// turn on identity, where the only link available would be built from a label
/// the refusal exists to say nothing about.
///
/// NEVER the signup form. Somebody signing in to a mailbox they already own is
/// not somebody to send off to make a second one.
pub fn console_problem_with_link(
    status: StatusCode,
    heading: &str,
    detail: &str,
    console_url: &str,
) -> Response {
    page(
        status,
        heading,
        &format!(
            r#"<h1>{heading}</h1>
<p class="lead">{detail}</p>
<a class="button" href="{url}">Back to your console <span class="arrow" aria-hidden="true">→</span></a>"#,
            heading = escape_html(heading),
            detail = escape_html(detail),
            url = escape_html(console_url),
        ),
    )
}

/// The app login's answer for a Google account with no mailbox here, WITH the
/// way forward on it: the app has one "sign in with Gmail" door for everybody,
/// so a newcomer arrives here, and this is where they learn to sign up.
///
/// The link is the signup form at this deployment's own root, never anything a
/// caller named. Not an oracle for the same reason the page itself is not: only
/// the person who just proved they hold this Google account can read it.
pub fn app_no_tenant(heading: &str, detail: &str) -> Response {
    page(
        StatusCode::NOT_FOUND,
        heading,
        &format!(
            r#"<h1>{heading}</h1>
<p class="lead">{detail}</p>
<a class="button" href="/">Sign up for Passband <span class="arrow" aria-hidden="true">→</span></a>"#,
            heading = escape_html(heading),
            detail = escape_html(detail),
        ),
    )
}

/// The CSP of the agent confirmation page: [`CSP`] with `form-action` widened
/// to any https origin, and nothing else changed (`frame-ancestors 'none'`
/// included, which is what stops the Continue button being clickjacked).
///
/// WIDENED BECAUSE OF THE REDIRECT, for the reason [`CSP`] already names
/// accounts.google.com: Chrome enforces `form-action` against where a form
/// submission is REDIRECTED, and `POST /agent/confirm` answers with a 303 to
/// the AuthKit URL WorkOS returned. That host is the tenant fleet's issuer,
/// which this service is not configured with, so it cannot be named; under
/// `form-action 'self'` the press would silently do nothing. What the
/// widening does not open: the form's own `action` is a fixed same-origin path
/// with nothing interpolated, and the redirect target was already held to an
/// absolute https URL before it was sent (see
/// [`crate::workos::acceptable_redirect`]).
///
/// AND LOOPBACK, because the chain does not always stop at AuthKit. When
/// AuthKit already holds this agent's consent it shows no screen of its own
/// and answers with a redirect straight to the agent's callback, which makes
/// that callback one more hop of THIS form's submission. A desktop agent's
/// callback is plain http on loopback (`http://127.0.0.1:<port>/callback`),
/// and under `https:` alone Chrome ate the press: the completion had happened
/// and the browser stayed on this page (seen on production, 2026-10-05, on the
/// second authorization of one client). Loopback only, any port; plain http
/// to anywhere else stays refused.
const CSP_AGENT_CONFIRM: &str = "default-src 'none'; style-src 'unsafe-inline'; font-src 'self'; form-action 'self' https: http://127.0.0.1:* http://localhost:*; frame-ancestors 'none'; base-uri 'none'";

/// The same, on a plain-http local run, where the redirect may be plain http.
const CSP_AGENT_CONFIRM_INSECURE: &str = "default-src 'none'; style-src 'unsafe-inline'; font-src 'self'; form-action 'self' https: http:; frame-ancestors 'none'; base-uri 'none'";

/// The agent login's confirmation page: the one page of OURS between Google and
/// AuthKit's consent screen.
///
/// IT EXISTS BECAUSE THE LINK CAN BE SENT. Whoever starts an agent's
/// authorization can mail somebody else the `/agent/auth` link; Google's
/// chooser asks a returning user nothing, and AuthKit's screen names the agent
/// with whatever its builder registered. So this page says, before anything is
/// sent anywhere, what is about to be granted, to which mailbox (escaped: a
/// person with several Google accounts must see WHICH one), and when to walk
/// away. Short, because a long warning is a warning nobody reads.
///
/// ONE FORM, TWO BUTTONS. Cancel comes FIRST in the markup, so if anything
/// ever submits the form implicitly (Enter in a field), the default is the
/// safe answer; Continue is the solid button beside it, and Cancel is quiet. Both carry the one-shot CSRF token; the server takes the
/// pending state either way, so Cancel really does discard it.
pub fn agent_confirm(mailbox: &str, csrf: &str, allow_http: bool) -> Response {
    let mut resp = page(
        StatusCode::OK,
        "Connect an AI agent?",
        &format!(
            r#"<h1>Connect an AI agent to your mail?</h1>
<p class="lead">An AI agent is asking to read the mail in <strong>{mailbox}</strong>.</p>
<p>Only continue if you just started connecting an agent yourself, on this device. If someone sent you a link to this page, close it.</p>
<p class="hint">The next screen names the agent. That name is chosen by whoever built it, so it does not prove who they are.</p>
<form method="post" action="/agent/confirm"><input type="hidden" name="csrf" value="{csrf}">
<button type="submit" name="decision" value="cancel" class="quiet">Cancel</button>
<button type="submit" name="decision" value="continue">Continue <span class="arrow" aria-hidden="true">→</span></button>
</form>"#,
            mailbox = escape_html(mailbox),
            csrf = escape_html(csrf),
        ),
    );
    let csp = if allow_http {
        CSP_AGENT_CONFIRM_INSECURE
    } else {
        CSP_AGENT_CONFIRM
    };
    resp.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        header::HeaderValue::from_static(csp),
    );
    resp
}

/// The refusal banner the signup form renders.
///
/// `role="alert"` because these pages have no JavaScript and a refusal is a
/// whole new document: without it, a screen reader lands the user back at the
/// top of a page that looks identical to the one they just submitted, with the
/// one sentence that changed sitting silently in the middle of it.
fn stop_note(error: Option<&str>) -> String {
    error
        .map(|e| {
            format!(
                r#"<p class="stop" role="alert"><strong>{}</strong></p>"#,
                escape_html(e)
            )
        })
        .unwrap_or_default()
}

/// Something went wrong after the user left for Google. `status` is the HTTP
/// status; `detail` is a sentence a person can act on and never a machine
/// reason.
pub fn problem(status: StatusCode, heading: &str, detail: &str) -> Response {
    page(
        status,
        heading,
        &format!(
            r#"<h1>{heading}</h1>
<p class="lead">{detail}</p>
<a class="button" href="/">Start again <span class="arrow" aria-hidden="true">→</span></a>"#,
            heading = escape_html(heading),
            detail = escape_html(detail),
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::admin_views::tests::{row, waiting};
    use crate::admin_views::{admin_login, admin_overview};
    use axum::body::to_bytes;

    /// The mailbox is ESCAPED on the confirmation page (the store holds
    /// whatever Google verified, and this is the line where it meets HTML),
    /// the page is unframeable and uncacheable, and it has no em dash.
    #[tokio::test]
    async fn the_agent_confirmation_escapes_the_mailbox_and_cannot_be_framed() {
        let r = agent_confirm(r#"a"<b>&'c@example.com"#, "tok\"en", false);
        assert_eq!(r.status(), StatusCode::OK);
        let h = r.headers().clone();
        let csp = h[header::CONTENT_SECURITY_POLICY].to_str().unwrap();
        assert!(csp.contains("frame-ancestors 'none'"), "{csp}");
        assert!(csp.contains("form-action 'self' https:"), "{csp}");
        // A returning agent's loopback callback is a hop of this form's
        // submission; plain http to anywhere else is not allowed.
        assert!(
            csp.contains("form-action 'self' https: http://127.0.0.1:* http://localhost:*;"),
            "{csp}"
        );
        assert!(!csp.contains("http:;") && !csp.contains("http: "), "{csp}");
        assert_eq!(h[header::X_FRAME_OPTIONS], "DENY");
        assert_eq!(h[header::CACHE_CONTROL], "no-store, no-cache");
        assert_eq!(h[header::REFERRER_POLICY], "no-referrer");
        let body = body_of(r).await;
        assert!(
            body.contains("a&quot;&lt;b&gt;&amp;&#39;c@example.com"),
            "{body}"
        );
        assert!(!body.contains("<b>"), "{body}");
        assert!(body.contains(r#"value="tok&quot;en""#), "{body}");
        assert!(!body.contains('\u{2014}'), "{body}");
        // Plain http is admitted to form-action only on an insecure run.
        let insecure = agent_confirm("a@example.com", "t", true);
        let csp = insecure.headers()[header::CONTENT_SECURITY_POLICY]
            .to_str()
            .unwrap()
            .to_string();
        assert!(csp.contains("https: http:"), "{csp}");
    }

    async fn body_of(r: Response) -> String {
        String::from_utf8(to_bytes(r.into_body(), 1 << 20).await.unwrap().to_vec()).unwrap()
    }

    /// The rendered page WITHOUT the shell's stylesheet: what a reader can
    /// actually see.
    ///
    /// USE THIS FOR ANY "MUST NOT APPEAR" ASSERTION. Every page here renders
    /// through the one shell in [`render`], inline stylesheet and comments and
    /// all, so a negative match against the whole response is really a match
    /// against whatever those comments happen to say this week. That has broken
    /// three separate assertions already: a comment about the signup form, one
    /// about the admin board's `.invite` row, and one about re-sending being the
    /// rarer errand. None of the three was ever about the stylesheet.
    async fn document_of(r: Response) -> String {
        let html = body_of(r).await;
        html.split_once("</style>")
            .expect("every page carries the shell's stylesheet")
            .1
            .to_string()
    }

    #[test]
    fn escapes_the_characters_that_end_a_page() {
        assert_eq!(
            escape_html(r#"<script>alert("x&y")</script>"#),
            "&lt;script&gt;alert(&quot;x&amp;y&quot;)&lt;/script&gt;"
        );
        assert_eq!(escape_html("it's"), "it&#39;s");
    }

    #[test]
    fn percent_encodes_everything_outside_the_unreserved_set() {
        assert_eq!(percent_encode("ada-lovelace_1.0~"), "ada-lovelace_1.0~");
        assert_eq!(percent_encode("https://a.b"), "https%3A%2F%2Fa.b");
        assert_eq!(percent_encode("a&b=c"), "a%26b%3Dc");
        assert_eq!(percent_encode("a b"), "a%20b");
    }

    /// The link shape the Swift client parses (`passband://pair?url=…&code=…`),
    /// with and without the analytics ride-along. The no-aid string is pinned
    /// EXACTLY: self-host and the fail-soft window must keep producing the
    /// link every shipped build already understands.
    #[test]
    fn builds_the_deep_link_the_app_expects() {
        assert_eq!(
            deep_link("https://ada.passband.email", "ABCD-EFGH", None),
            "passband://pair?url=https%3A%2F%2Fada.passband.email&code=ABCD-EFGH"
        );
        assert_eq!(
            deep_link(
                "https://ada.passband.email",
                "ABCD-EFGH",
                Some("0e51e11e-89ff-4c8f-a566-95a8f9b169b0"),
            ),
            "passband://pair?url=https%3A%2F%2Fada.passband.email&code=ABCD-EFGH&aid=0e51e11e-89ff-4c8f-a566-95a8f9b169b0"
        );
    }

    /// The app sign-in page carries the aid'd link when a person is known,
    /// HTML-escaped like every href on them. The `&amp;` is the point of the
    /// assertion: a raw `&` in an attribute is the kind of thing only a test
    /// notices.
    #[tokio::test]
    async fn the_finishing_pages_carry_the_analytics_ride_along() {
        let aid = "0e51e11e-89ff-4c8f-a566-95a8f9b169b0";
        let expected = format!(
            "passband://pair?url=https%3A%2F%2Fada.passband.email&amp;code=ABCD-EFGH&amp;aid={aid}"
        );
        let app_html = body_of(app_signed_in(
            "ada@example.com",
            "https://ada.passband.email",
            "ABCD-EFGH",
            10,
            Some(aid),
        ))
        .await;
        assert!(app_html.contains(&expected), "{app_html}");
    }

    /// The form is the only place the three grants are explained in the
    /// product's own words before Google states them in Google's, so all three
    /// are named and each says what it is for.
    #[tokio::test]
    async fn the_form_states_all_three_grants_and_the_base_domain() {
        let html = body_of(signup_form("passband.email", None, "", "", None)).await;
        // One Google permission covers all three, and it is named.
        assert!(html.contains("gmail.modify"));
        assert!(!html.contains("gmail.readonly"), "never asked for any more");
        assert!(!html.contains("gmail.send"), "never asked for any more");
        // The fourth ask is named too, and named as the optional one.
        assert!(html.contains("name and profile photo"), "{html}");
        // ...and the reason for each, not just the scope name.
        assert!(html.contains("triage"), "{html}");
        assert!(html.contains("labeling"), "{html}");
        assert!(html.contains("replying from the app"), "{html}");
        assert!(html.contains(".passband.email"));
        assert!(html.contains(r#"action="/signup""#));
        // No script anywhere, and nothing to fetch.
        assert!(!html.contains("<script"));
        assert!(!html.contains("http://"));
    }

    /// The scope detail may be folded away, but all three permissions are named
    /// in the open, beside the button, where somebody deciding whether to press
    /// it will read them: they ARE the disclosure's summary line, which is
    /// always on screen. What each is for sits behind it, shut by default.
    #[tokio::test]
    async fn the_form_names_all_three_permissions_outside_the_disclosure() {
        let html = body_of(signup_form("passband.email", None, "", "", None)).await;
        let (_, details) = html.split_once("<details>").expect("{html}");
        let (summary, folded) = details.split_once("</summary>").expect("{html}");
        // The summary is the line that is always on screen, beside the button.
        assert!(
            summary.contains("read, change, and send your Gmail"),
            "{summary}"
        );
        assert!(folded.contains("keep that box checked"), "{html}");
        // Shut by default: `open` is what would render it expanded.
        assert!(!html.contains("<details open"), "{html}");
        // A disclosure is markup, not script. This page still has none.
        assert!(!html.contains("<script"), "{html}");
    }

    /// The username field carries its domain inside the same box, right after
    /// the input, so the suffix trails what is typed rather than sitting in a
    /// hint underneath it.
    #[tokio::test]
    async fn the_username_field_trails_its_domain() {
        let html = document_of(signup_form("passband.email", None, "ada", "", None)).await;
        assert!(html.contains("Your Passband username"), "{html}");
        let (_, field) = html.split_once(r#"<label class="affix">"#).expect("{html}");
        let (field, _) = field.split_once("</label>").expect("{html}");
        assert!(field.contains(r#"id="label""#), "{field}");
        assert!(field.contains(r#"value="ada""#), "{field}");
        assert!(
            field.ends_with(r#"<span class="domain">.passband.email</span>"#),
            "{field}"
        );
    }

    /// A code that came filled in is folded behind a link, and the username is
    /// what gets focus. An empty field, or any re-render after a submit, shows
    /// the code in the open.
    #[tokio::test]
    async fn a_prefilled_invite_is_folded_only_on_arrival() {
        let code = "ABCD-EFGH-JKMN-PQRS";
        let folded = document_of(signup_form("passband.email", None, "", code, None)).await;
        assert!(
            folded.contains(r#"class="slot folded" id="code""#),
            "{folded}"
        );
        assert!(folded.contains(&format!(r#"value="{code}""#)), "{folded}");
        assert!(folded.contains(r##"href="#code""##), "{folded}");
        let (_, username) = folded.split_once(r#"id="label""#).unwrap();
        let (username, _) = username.split_once('>').unwrap();
        assert!(username.contains("autofocus"), "{username}");

        for open in [
            signup_form("passband.email", None, "", "", None),
            signup_form(
                "passband.email",
                None,
                "ada",
                code,
                Some("That invite code is not usable."),
            ),
            // Any refusal unfolds: the code may be one the person typed.
            signup_form(
                "passband.email",
                None,
                "ada",
                code,
                Some("That username is already taken."),
            ),
        ] {
            let html = document_of(open).await;
            assert!(!html.contains("slot folded"), "{html}");
            assert!(!html.contains(r##"href="#code""##), "{html}");
            let (_, invite) = html.split_once(r#"id="invite""#).unwrap();
            let (invite, _) = invite.split_once('>').unwrap();
            assert!(invite.contains("autofocus"), "{invite}");
        }
    }

    /// The invite field shows the shape of a code this deployment actually
    /// mints. It used to show the eight-symbol shape that predates
    /// [`crate::invites::CODE_LEN`], which is a placeholder no live code has
    /// looked like since.
    #[tokio::test]
    async fn the_invite_placeholder_is_the_shape_a_minted_code_has() {
        let html = body_of(signup_form("passband.email", None, "", "", None)).await;
        let minted = crate::invites::mint().unwrap().code;
        let shape: String = minted
            .chars()
            .map(|c| if c == '-' { '-' } else { 'X' })
            .collect();
        assert!(
            html.contains(&format!(r#"placeholder="{shape}""#)),
            "{html}"
        );
    }

    /// The way off this page for somebody who has no code, and the reason it is
    /// not conditional on anything the user typed: rendering it only on an
    /// invite refusal would make its presence an answer about the code space.
    #[tokio::test]
    async fn the_form_offers_the_waitlist_whether_or_not_anything_was_refused() {
        let url = "https://passband.app/waitlist";
        for error in [None, Some("That invite code is not usable.")] {
            let html = body_of(signup_form("passband.email", Some(url), "", "", error)).await;
            assert!(html.contains(&format!(r#"href="{url}""#)), "{html}");
            assert!(html.contains("No invite code?"), "{html}");
        }
        // A deployment with no waitlist configured links nowhere at all rather
        // than to a page that does not exist.
        let bare = body_of(signup_form("passband.email", None, "", "", None)).await;
        assert!(!bare.contains("waitlist"), "{bare}");
        assert!(!bare.contains("No invite code"), "{bare}");
    }

    /// Both fields are echoed back into the form, so both are escape paths.
    #[tokio::test]
    async fn the_form_escapes_what_it_echoes() {
        let html = body_of(signup_form(
            "passband.email",
            Some(r#"https://evil.test/" onmouseover="alert(3)"#),
            r#""><script>alert(1)</script>"#,
            r#""onfocus="alert(2)"#,
            Some("<b>nope</b>"),
        ))
        .await;
        assert!(!html.contains("<script>alert(1)"));
        assert!(!html.contains(r#"onfocus="alert(2)"#));
        assert!(!html.contains(r#"onmouseover="alert(3)"#));
        assert!(!html.contains("<b>nope</b>"));
        assert!(html.contains("&lt;script&gt;"));
    }

    /// The signup's last page is the download and nothing else: the app's own
    /// onboarding signs in and pairs, so no code and no deep link ride here.
    #[tokio::test]
    async fn the_success_page_is_the_download_and_nothing_else() {
        let html = document_of(success("https://ada.passband.email")).await;
        assert!(html.contains("ada.passband.email"), "{html}");
        assert!(
            html.contains(&format!(r#"href="{DOWNLOAD_URL}""#)),
            "{html}"
        );
        assert!(!html.contains("passband://"), "{html}");
        assert!(!html.contains("<details"), "{html}");
        assert!(!html.contains("<script"), "{html}");
    }

    /// The app page's fallback is FOLDED AWAY BUT LABELLED WITH THE SYMPTOM.
    ///
    /// A `contains` over the whole document cannot tell a code somebody can read
    /// from a code sealed inside a shut `<details>`, which is how the pairing
    /// code came to be two presses deep under a green
    /// `the_success_page_carries_the_code_the_url_and_the_link`. So this asserts
    /// the arrangement rather than the presence: the code is INSIDE the
    /// disclosure, and the disclosure's summary names what went wrong rather
    /// than what to do about it. Moving the code back into the open should fail
    /// here and be a deliberate edit to this test.
    #[tokio::test]
    async fn the_app_page_opens_the_app_and_labels_the_fallback_with_the_symptom() {
        let response = app_signed_in(
            "ada@example.com",
            "https://ada.passband.email",
            "ABCD-EFGH",
            10,
            None,
        );
        // The page moves by itself, to the same link its button carries.
        let refresh = response.headers()[header::REFRESH]
            .to_str()
            .unwrap()
            .to_string();
        assert_eq!(
            refresh,
            format!(
                "0; url={}",
                deep_link("https://ada.passband.email", "ABCD-EFGH", None)
            )
        );
        let html = body_of(response).await;
        let (open, folded) = html.split_once("<details>").expect("{html}");
        // The deep link is the page; the code is the way out when it fails.
        // Asserted on the wrapper, not the code: the `href` above carries
        // the same string, and a code inside a link is not one to read off.
        assert!(open.contains("passband://pair?url="), "{html}");
        assert!(!open.contains(r#"<span class="code">"#), "{html}");
        assert!(
            folded.contains(r#"<span class="code">ABCD-EFGH</span>"#),
            "{html}"
        );
        // Shut, so the summary is the whole of what gets read.
        assert!(!html.contains("<details open"), "{html}");
        let summary = folded
            .split_once("</summary>")
            .expect("{html}")
            .0
            .to_lowercase();
        assert!(summary.contains("didn’t open"), "{html}");
    }

    /// THE DOWNLOAD LIVES AT THE END OF THE FLOW, not on the landing page: that
    /// page leads with the waitlist now, so these two are the only screens that
    /// hand anybody the client. Both need it, and each for its own reason: one
    /// has just built a mailbox for somebody who has no app, and the other is
    /// the page a person lands on precisely when the deep link did nothing,
    /// which is most often because the app is not installed.
    ///
    /// A REAL `href`, not the bare hostname the success page used to print. It
    /// was `<code>passband.app</code>` in the middle of a numbered step, which
    /// asks somebody mid-setup to retype a domain, and now it is the one thing
    /// on that step that can be pressed.
    #[tokio::test]
    async fn the_pages_that_finish_a_flow_hand_over_the_client() {
        for html in [
            body_of(success("https://ada.passband.email")).await,
            body_of(app_signed_in(
                "ada@example.com",
                "https://ada.passband.email",
                "ABCD-EFGH",
                10,
                None,
            ))
            .await,
        ] {
            assert!(
                html.contains(&format!(r#"href="{DOWNLOAD_URL}""#)),
                "{html}"
            );
            assert!(html.contains(">Download Passband"), "{html}");
        }
        // And nowhere else. The signup form is somebody deciding whether to
        // hand over their mail, and an app they cannot yet point at anything is
        // not what that page is for.
        let form = body_of(signup_form("passband.email", None, "", "", None)).await;
        assert!(!form.contains(DOWNLOAD_URL), "{form}");
    }

    /// The console redirect is assembled from a validated tenant URL and a
    /// validated code, and the code is encoded on the way in: nothing in it can
    /// end the parameter it sits in or start a second one.
    #[test]
    fn builds_the_console_callback_from_its_own_parts() {
        assert_eq!(
            console_callback_url("https://ada.passband.email", "ABCD-EFGH"),
            "https://ada.passband.email/console/callback?code=ABCD-EFGH"
        );
        assert_eq!(
            console_callback_url("https://ada.passband.email", "A&next=https://evil.test"),
            "https://ada.passband.email/console/callback?code=A%26next%3Dhttps%3A%2F%2Fevil.test"
        );
    }

    /// The console refusal links nowhere: the only link it could offer would be
    /// built from a label the refusal is deliberately saying nothing about.
    #[tokio::test]
    async fn the_console_refusal_offers_no_way_onward() {
        let html = body_of(console_problem(
            StatusCode::BAD_REQUEST,
            "Nope",
            "Try again.",
        ))
        .await;
        assert!(!html.contains("<a "), "{html}");
        assert!(!html.contains("href"), "{html}");
    }

    /// House rule: no typographic dashes in anything a person reads.
    ///
    /// THE WHOLE FAMILY, not just the em dash this started as. An en dash is the
    /// same mistake one code point over, and it walked straight past a test
    /// written for U+2014 alone: `3–30 lowercase letters` shipped on the signup
    /// form under a green run of exactly this test. A rule enforced for one
    /// member of a set it means to cover is a rule that reads as enforced and
    /// is not. Hyphen-minus is the only dash on these pages.
    ///
    /// The list below is every page that renders authored copy, and adding one
    /// is part of adding a page: the guarantee is only as wide as the list.
    #[tokio::test]
    async fn no_typographic_dashes_in_user_facing_copy() {
        // Figure dash, en dash, em dash, horizontal bar, minus sign.
        const BANNED: [char; 5] = ['\u{2012}', '\u{2013}', '\u{2014}', '\u{2015}', '\u{2212}'];
        for html in [
            body_of(signup_form(
                "passband.email",
                Some("https://passband.app/waitlist"),
                "ada",
                "ABCD-EFGH",
                Some("no"),
            ))
            .await,
            body_of(success("https://ada.passband.email")).await,
            body_of(app_signed_in(
                "ada@example.com",
                "https://ada.passband.email",
                "ABCD-EFGH",
                10,
                None,
            ))
            .await,
            body_of(problem(StatusCode::BAD_REQUEST, "Nope", "Try again.")).await,
            body_of(console_problem(
                StatusCode::BAD_REQUEST,
                "Nope",
                "Try again.",
            ))
            .await,
            body_of(console_problem_with_link(
                StatusCode::BAD_REQUEST,
                "Nope",
                "Try again.",
                "https://ada.passband.email",
            ))
            .await,
            body_of(admin_login(Some("no"))).await,
            body_of(admin_overview(
                Some(&[
                    waiting(1, "ada@example.com"),
                    row(2, "bob@example.com", true),
                ]),
                None,
            ))
            .await,
        ] {
            for dash in BANNED {
                assert!(!html.contains(dash), "U+{:04X} in {html}", u32::from(dash));
            }
        }
    }

    /// The masthead is part of the shell, so it is on every page, and it is
    /// MARKUP rather than anything the browser has to go and get: no `src`, no
    /// `url()`, no scheme anywhere in it. That is the whole reason the logo
    /// could be added without touching a CSP that says `default-src 'none'`.
    #[tokio::test]
    async fn every_page_wears_the_mark_and_fetches_nothing_to_do_it() {
        for html in [
            body_of(signup_form("passband.email", None, "", "", None)).await,
            body_of(success("https://ada.passband.email")).await,
            body_of(app_signed_in(
                "ada@example.com",
                "https://ada.passband.email",
                "ABCD-EFGH",
                10,
                None,
            ))
            .await,
            body_of(problem(StatusCode::BAD_REQUEST, "Nope", "Try again.")).await,
            body_of(console_problem(
                StatusCode::BAD_REQUEST,
                "Nope",
                "Try again.",
            ))
            .await,
            body_of(admin_login(None)).await,
            body_of(admin_overview(Some(&[waiting(1, "ada@example.com")]), None)).await,
        ] {
            assert!(html.contains(r#"<header class="brand">"#), "{html}");
            assert!(
                html.contains(r#"<span class="wordmark">Passband</span>"#),
                "{html}"
            );
            assert!(html.contains("<svg"), "{html}");
            // Inline, not fetched: an `<img src>` or a `url()` in the masthead
            // would be a request `default-src 'none'` refuses, i.e. a broken
            // logo. The stylesheet's one `url()` is the same-origin font.
            let (_, masthead) = html.split_once(r#"<header class="brand">"#).unwrap();
            let (masthead, _) = masthead.split_once("</header>").unwrap();
            assert!(!masthead.contains("url("), "{masthead}");
            assert!(!html.contains("<img"), "{html}");
            assert!(!html.contains("http://"), "{html}");
            assert_eq!(html.matches("url(").count(), 1, "{html}");
            assert!(html.contains(&format!(r#"url("{FONT_PATH}")"#)), "{html}");
        }
    }

    /// The masthead does not link out. The one honest `href` would be the
    /// marketing site, and the page it sits on hardest is the one where somebody
    /// is deciding whether to hand over their mail; a door out of that decision
    /// is not what the top left corner is for. The console refusal leans on
    /// this: it asserts it offers NO way onward, and a linked masthead would
    /// have made that false everywhere at once.
    #[tokio::test]
    async fn the_masthead_is_not_a_way_out_of_the_flow() {
        let html = body_of(signup_form("passband.email", None, "", "", None)).await;
        let (masthead, _) = html.split_once("</header>").expect("{html}");
        assert!(!masthead.contains("<a "), "{masthead}");
        assert!(!masthead.contains("href"), "{masthead}");
    }

    /// The font is served as a font, cacheable, and the CSP admits it from this
    /// origin and nowhere else.
    #[tokio::test]
    async fn the_font_is_first_party_and_cacheable() {
        let r = newsreader().await;
        assert_eq!(r.headers()[header::CONTENT_TYPE], "font/woff2");
        assert!(
            r.headers()[header::CACHE_CONTROL]
                .to_str()
                .unwrap()
                .contains("immutable")
        );
        let bytes = to_bytes(r.into_body(), 1 << 20).await.unwrap();
        assert!(bytes.starts_with(b"wOF2"));
        assert!(CSP.contains("font-src 'self';"));
    }

    /// The vendored font IS the landing page's font. The control image never
    /// sees `passband-site/`, so the copy is by hand; this is what notices when
    /// the site's subset moves and the copy did not.
    #[test]
    fn the_vendored_font_is_the_sites_font() {
        let site = include_bytes!("../../passband-site/newsreader-var.woff2");
        assert!(
            NEWSREADER == site.as_slice(),
            "copy passband-site/newsreader-var.woff2 to squelch-control/assets/newsreader.woff2"
        );
    }

    #[tokio::test]
    async fn every_page_carries_the_security_headers() {
        for r in [
            signup_form("passband.email", None, "", "", None),
            success("https://ada.passband.email"),
            problem(StatusCode::BAD_REQUEST, "Nope", "Try again."),
            console_problem(StatusCode::BAD_REQUEST, "Nope", "Try again."),
            admin_login(None),
            admin_overview(Some(&[waiting(1, "ada@example.com")]), None),
        ] {
            let h = r.headers().clone();
            assert_eq!(h[header::X_FRAME_OPTIONS], "DENY");
            assert_eq!(h[header::REFERRER_POLICY], "no-referrer");
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
