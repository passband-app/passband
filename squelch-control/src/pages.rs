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

use axum::{
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};

use crate::reconnect::{Purpose, ReconnectView};
use crate::store::{FeatureBoard, UserRow};
use squelch_core::features::Feature;

/// The Content-Security-Policy every page carries. The single allowance is the
/// inline `<style>`; `frame-ancestors 'none'` (with the older `X-Frame-Options`
/// beside it) keeps a signup decision from being framed inside someone else's
/// page. `form-action` names accounts.google.com as well as 'self' because
/// Chrome enforces form-action against the REDIRECT TARGET of a form
/// submission (a long-standing, spec-contested behavior): POST /signup answers
/// 303 to Google consent, and under `form-action 'self'` Chrome silently eats
/// that navigation - the button "does nothing" while the server logs a
/// perfectly healthy signup. Found live on the first real signup, 2026-08-11.
/// `font-src 'self'` admits exactly one file, [`NEWSREADER`], served by this
/// same origin: the landing page's serif, and still nothing a third party sees.
const CSP: &str = "default-src 'none'; style-src 'unsafe-inline'; font-src 'self'; form-action 'self' https://accounts.google.com; frame-ancestors 'none'; base-uri 'none'";

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
    render(status, title, body, r#" class="auth""#)
}

/// The same shell on the OPERATOR'S surface: the landing page's dark ground in
/// both palettes, its mark, its type, and none of its selling. See the
/// `body.console` block in the stylesheet for what that means and why the
/// colours are restated there rather than inherited from the dark media query.
///
/// The three pages behind the admin token, and nothing else. A signup page is
/// read once by somebody deciding whether to hand over their mail; this is read
/// every day by one person who already decided.
fn console(status: StatusCode, title: &str, body: &str) -> Response {
    render(status, title, body, r#" class="console""#)
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

fn render(status: StatusCode, title: &str, body: &str, body_class: &str) -> Response {
    let title = escape_html(title);
    let masthead =
        format!(r#"<header class="brand">{MARK}<span class="wordmark">Passband</span></header>"#);
    let is_console = body_class == r#" class="console""#;
    let opening = if body_class == r#" class="auth""# {
        format!("{masthead}<main>")
    } else if is_console {
        format!("{CONSOLE_CHROME}<main>{masthead}")
    } else {
        format!("<main>{masthead}")
    };
    let console_css = if is_console { CONSOLE_CSS } else { "" };
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
{body}</main></body>
</html>
"#,
        title = title,
        opening = opening,
        font_path = FONT_PATH,
        theme = crate::appearance::current().name(),
        body = body,
        body_class = body_class,
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
/// permissions, because the consent screen that follows is Google's and says it
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
<p class="lead">Pick a username, then connect Gmail.</p>
{error_html}
<form class="rig" method="post" action="/signup">
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
<button type="submit">Continue with Google <span class="arrow" aria-hidden="true">→</span></button>
</form>
<details>
<summary>Google will ask to read, change, and send your Gmail.</summary>
<ul>
<li><strong>Read</strong> (<code>gmail.readonly</code>): mail triage.</li>
<li><strong>Change</strong> (<code>gmail.modify</code>): archiving and labeling, never deletion.</li>
<li><strong>Send</strong> (<code>gmail.send</code>): replying from the app.</li>
</ul>
<p>Passband needs all three, so keep those boxes checked. It also asks for your name and profile photo, to show them in the app; that one is optional. Your Google refresh token is stored encrypted so mail keeps syncing; self-host to hold it yourself.</p>
</details>
{links_html}"#,
            error_html = error_html,
            links_html = links_html,
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
<p class="lead">{host} is live. Get the app, then sign in with Gmail to finish.</p>
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
<p>In Passband, choose Sign in with Gmail and enter:</p>
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
    admin_door(status, &stop_note(error))
}

/// The door after a DELIBERATE sign out, which is not a refusal and must not be
/// dressed as one.
///
/// A 200 and a quiet note, where [`admin_login`] would answer 401 with a red
/// banner. The rule that function states still holds and this is the other half
/// of it: a 200 carrying "not accepted" is a lie to everything that reads status
/// codes, and so is a 401 for a request that did exactly what was asked.
pub fn admin_signed_out() -> Response {
    admin_door(StatusCode::OK, r#"<p class="hint">Signed out.</p>"#)
}

/// The form both doors show, with whatever note belongs above it.
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
/// A 403 with no words is the right shape for a page on some other origin that
/// pressed these buttons, and the wrong shape for the operator, who meets the
/// same refusal when an extension, a proxy, or a sandboxed frame rewrites those
/// headers under an address bar that reads correctly. Both are told the same
/// thing, because the attacker's half of it is a request they wrote themselves.
///
/// `report` arrives already filtered by [`crate::admin`] and is escaped here
/// too: the rule is that nothing is interpolated raw.
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

/// The dashboard: who is waiting, who has been approved, and the two buttons.
///
/// ALWAYS 200, even carrying an `error`. The list under it is correct either
/// way, and the banner says what the button did NOT do (a row already approved,
/// an invite already spent); that is a finished page describing a real state,
/// not a failed request.
///
/// THE CODE IS NOT HERE, and cannot be. Only its hash was kept, so the one
/// remedy for a lost invite is a fresh one, which is what the send buttons are.
pub fn admin_page(pending: &[UserRow], approved: &[UserRow], error: Option<&str>) -> Response {
    let waiting: String = pending
        .iter()
        .map(|r| {
            format!(
                r#"<tr><td class="who">{who}</td><td class="when">{joined}</td><td class="act">{action}</td></tr>
"#,
                who = who_cell(r),
                joined = day(r.created_at),
                // "Approve", not "Approve and email invite". The long label was
                // the only disclosure that pressing it sends mail, and it paid
                // for that by repeating three words down every row of the board
                // in the loudest thing on the page. The sentence above the table
                // says it once, and the button is a word.
                action = action_form("/admin/approve", r.id, "Approve", true),
            )
        })
        .collect();

    let history: String = approved
        .iter()
        .map(|r| {
            // Two shapes for one row. A stamped `notified_at` is the quiet
            // case (the mail went out; the button is there for the person who
            // lost it), and a missing one is the loud case: this row is
            // approved and nobody was told.
            // THREE STATES, AND THE LAST ONE IS THE POINT OF THE COLUMN.
            //
            // Redemption was always in the store (`invite_codes.used_at`, joined
            // on in `list_users`) and the re-send handler already refused a
            // spent code with INVITE_SPENT. What was missing was SAYING so: the
            // only way to learn somebody had signed up was to press a button and
            // read the refusal. Now the board says it, and offers NO button on
            // that row, because the one it used to offer could only ever fail.
            //
            // No button is the honest answer rather than a tidy one. A spent
            // code means a mailbox exists, and minting a second for the same
            // person is a tenant nobody approved, which is what the handler's
            // refusal is there to stop.
            //
            // THE TOP RUNG IS ACTIVATION, and it sits ABOVE "signed up" because
            // the ladder is read as PRECEDENCE and not as history: somebody
            // active signed up too, the row has space for one answer, and the
            // answer worth having is the furthest they got. Same `.done` family
            // as the rung below and the same no-button rule, for the same
            // reason: there is nothing left to mail somebody who is using the
            // product.
            let outcome = if let Some(at) = r.first_paired_at {
                let label = tenant_code(r);
                format!(
                    r#"<span class="done">active</span><span class="muted">{}</span>{label}"#,
                    day(at),
                )
            } else if let Some(at) = r.signed_up_at {
                let label = tenant_code(r);
                format!(
                    r#"<span class="done">signed up</span><span class="muted">{}</span>{label}"#,
                    day(at),
                )
            } else if let Some(at) = r.notified_at {
                format!(
                    r#"<span class="muted">Invited {}</span> {}"#,
                    day(at),
                    action_form("/admin/send", r.id, "Re-send", true),
                )
            } else {
                // A PILL, not the `.stop` rule. `.stop` is a left border meant
                // for a whole paragraph, and inline beside a button it drew a
                // stray red tick that ran into the label next to it.
                format!(
                    r#"<span class="flag">email not sent</span>{}"#,
                    action_form("/admin/send", r.id, "Send invite", false),
                )
            };
            let approved_on = r
                .approved_at
                .map(|at| format!("<br>approved {}", day(at)))
                .unwrap_or_default();
            // For the stylesheet only: an active row's pill is green.
            let rung = if r.first_paired_at.is_some() {
                r#" class="active""#
            } else {
                ""
            };
            format!(
                r#"<tr{rung}><td class="who">{who}</td><td class="when">joined {joined}{approved_on}</td><td class="act">{outcome}</td></tr>
"#,
                who = who_cell(r),
                joined = day(r.created_at),
            )
        })
        .collect();

    // "OF THOSE" NESTS, so an active person is counted in BOTH numbers:
    // everybody active signed up. Partitioning them instead would make
    // "3 signed up, 2 active" read as five people.
    let accepted_count = approved.iter().filter(|r| r.signed_up_at.is_some()).count();
    let active_count = approved
        .iter()
        .filter(|r| r.first_paired_at.is_some())
        .count();
    let rungs = [
        Rung {
            label: "waiting",
            count: pending.len(),
            colour: "var(--warn)",
            of: None,
        },
        Rung {
            label: "approved",
            count: approved.len(),
            colour: "var(--accent)",
            of: None,
        },
        Rung {
            label: "signed up",
            count: accepted_count,
            colour: "var(--accent-ink)",
            of: Some((approved.len(), "approved")),
        },
        Rung {
            label: "active",
            count: active_count,
            colour: "var(--positive)",
            of: Some((accepted_count, "signed up")),
        },
    ];

    console(
        StatusCode::OK,
        "Waitlist",
        &format!(
            r#"<section class="hero">
{tabs}
<h1 data-text="Waitlist">Waitlist</h1>
<p class="sr">{waiting_count} waiting, {approved_count} approved recently,
{accepted_count} of those signed up, {active_count} of those active</p>
</section>
{scope}
{error_html}
<div class="racks">
<section class="rack wait">
<div class="rack-head"><span class="led"></span><h2>Waiting</h2><span class="count">{waiting_count}</span>
<p class="hint">Approving mints one invite code and emails it. The code works
once and expires in {ttl} days.</p></div>
{waiting_table}
</section>
<section class="rack done-list">
<div class="rack-head"><span class="led"></span><h2>Approved recently</h2><span class="count">{approved_count}</span></div>
{history_table}
</section>
<section class="rack send">
<div class="rack-head"><span class="led"></span><h2>Invite someone directly</h2></div>
<form class="invite" method="post" action="/admin/invite">
<label class="sr" for="email">Email address</label>
<input type="email" id="email" name="email" placeholder="them@example.com"
  autocomplete="off" autocapitalize="off" spellcheck="false" required>
<button type="submit">Mint and email an invite</button>
</form>
<p class="hint">They do not have to be on the list first.</p>
</section>
</div>
<div class="footer-row">
<form class="signout" method="post" action="/admin/logout">
<button type="submit" class="quiet">Sign out</button>
</form>
<p class="hint">This session lasts {session_days} days.</p>
</div>"#,
            tabs = admin_tabs(AdminTab::Waitlist),
            scope = scope(&rungs),
            error_html = stop_note(error),
            waiting_count = pending.len(),
            approved_count = approved.len(),
            session_days = crate::cookie::ADMIN_COOKIE_TTL_SECS / (24 * 60 * 60),
            waiting_table = table(
                r#"<th>Who</th><th>Joined</th><th></th>"#,
                &waiting,
                "Nobody is waiting.",
            ),
            history_table = table(
                r#"<th>Who</th><th>Dates</th><th>Outcome</th>"#,
                &history,
                "Nobody has been approved yet.",
            ),
            ttl = crate::invites::DEFAULT_TTL_DAYS,
        ),
    )
}

/// The two pages behind the admin token, as the strip above each one's title.
#[derive(Clone, Copy, PartialEq, Eq)]
enum AdminTab {
    Waitlist,
    Features,
}

/// The way between the operator's pages. Plain links: no JavaScript here, and
/// each page is a whole document.
fn admin_tabs(current: AdminTab) -> String {
    let tab = |tab: AdminTab, href: &str, label: &str| {
        let here = if tab == current {
            r#" aria-current="page""#
        } else {
            ""
        };
        format!(r#"<a href="{href}"{here}>{label}</a>"#)
    };
    format!(
        r#"<nav class="tabs" aria-label="Admin pages">{}{}</nav>"#,
        tab(AdminTab::Waitlist, "/admin", "Waitlist"),
        tab(AdminTab::Features, "/admin/features", "Features"),
    )
}

/// The form field names on the features page. Here so the page that writes
/// them and the handler that reads them cannot drift apart.
///
/// `FEATURE_TENANT_FIELD` rides once per row as a hidden field and says "this
/// tenant was on the page", which is what lets an UNTICKED box mean "off": a
/// checkbox that is not ticked sends nothing at all, so without it a tenant
/// with every box cleared would be indistinguishable from one who was never
/// listed.
pub const FEATURE_TENANT_FIELD: &str = "t";
/// One COLUMN the page drew, as a hidden field, for the same reason the tenant
/// field exists turned ninety degrees: a save may only untick boxes in columns
/// this page actually had. A tab rendered before a deploy added a feature has
/// no boxes for it, and must not read their absence as "off for everyone".
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
///
/// ALWAYS 200, for the reason [`admin_page`] is: a banner over a real state is
/// a finished page.
///
/// EVERYTHING FROM THE STORE IS ESCAPED, and so are the registry's own strings,
/// which are compiled in and would be safe raw. The rule on this page is that
/// nothing is interpolated raw, and it has no exceptions for good behaviour.
pub fn admin_features(
    registry: &[Feature],
    board: Option<&FeatureBoard>,
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
        (true, _) => r#"<section class="rack"><div class="rack-head"><span class="led"></span><h2>Tenants</h2></div>
<p class="muted">This build hides nothing: every feature it has is on for everyone. A hidden feature
shows up here once it is listed in <code>squelch-core/src/features.rs</code> and that build is deployed.</p>
</section>"#
            .to_string(),
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
                        r#"<br><span class="flag">no control token</span><span class="muted">waits for <code>share mint</code></span>"#
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
                r#"<p class="muted">No active tenants yet.</p>"#
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
<section class="rack">
<div class="rack-head"><span class="led"></span><h2>Tenants</h2><span class="count">{count}</span>
<p class="hint">A ticked box is on. A mailbox's daemon hears about a change within {refresh_minutes} minutes, with no restart.</p></div>
<table class="matrix"><tr><th>Who</th>{head}</tr>
{everyone_row}{tenant_rows}</table>
{nobody}
</section>
<div class="footer-row"><button type="submit">Save</button>
<p class="hint">Saves the whole page at once.</p></div>
</form>"#,
                count = board.tenants.len(),
            )
        }
    };

    let saved_note = if saved && error.is_none() {
        r#"<p class="hint saved" role="status">Saved.</p>"#
    } else {
        ""
    };
    let legend = if legend.is_empty() {
        String::new()
    } else {
        format!(
            r#"<section class="rack"><div class="rack-head"><span class="led"></span><h2>Hidden features</h2><span class="count">{}</span></div>
<dl class="legend">
{legend}</dl>
</section>"#,
            registry.len(),
        )
    };

    console(
        StatusCode::OK,
        "Features",
        &format!(
            r#"<section class="hero">
{tabs}
<h1 data-text="Features">Features</h1>
</section>
{error_html}{saved_note}
<div class="racks">
{legend}
{body}
</div>
<div class="footer-row">
<form class="signout" method="post" action="/admin/logout">
<button type="submit" class="quiet">Sign out</button>
</form>
</div>"#,
            tabs = admin_tabs(AdminTab::Features),
            error_html = stop_note(error),
        ),
    )
}

/// One rung of the funnel, as the scope draws it: a carrier with a marker.
struct Rung {
    label: &'static str,
    count: usize,
    /// A CSS colour, always one of the stylesheet's own variables. Never data.
    colour: &'static str,
    /// The rung this one is a fraction of, and its name, for the rate under it.
    of: Option<(usize, &'static str)>,
}

/// The funnel as the landing page's analyzer would draw it: a noise floor with
/// one carrier per rung standing out of it, and a marker on each peak.
///
/// THE PEAKS ARE THE DATA. Each carrier's height is its count on a log scale
/// against the largest rung, so a board of 200 waiting and 3 active still
/// shows three as a real signal rather than a flat line. A rung of zero is a
/// carrier that is not there: just the floor, which is the honest picture.
///
/// THE NOISE IS NOT. It comes from a fixed seed, so the same counts draw the
/// same trace on every refresh and the only thing that moves the picture is
/// the funnel moving. Decoration that changed on every load would read as the
/// data changing.
///
/// Everything here is numbers the server computed and labels it owns; no
/// string from the store reaches the SVG, so there is nothing to escape.
fn scope(rungs: &[Rung; 4]) -> String {
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
    let trace = |seed: u64, grass: f64| -> String {
        let mut rnd = noise(seed);
        let mut d = String::new();
        let mut x = 0.0;
        while x <= W {
            let y = (FLOOR - grass * rnd() - signal(x)).max(8.0);
            d.push_str(&format!(
                "{}{x:.0} {y:.1}",
                if x == 0.0 { "M" } else { "L" }
            ));
            x += 4.0;
        }
        d
    };
    let live = trace(7, 12.0);

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
            // A rate only where there is one to state. The top two rungs are
            // just their counts.
            let sub = match r.of {
                Some((of, name)) if of > 0 => format!(
                    r#"<div class="sub">{}% of {name}</div>"#,
                    (r.count as f64 / of as f64 * 100.0).round()
                ),
                _ => String::new(),
            };
            format!(
                r#"<div class="readout" style="--c:{c}"><div class="lbl">{label}</div><span class="num">{count}</span>{sub}</div>"#,
                c = r.colour,
                label = r.label,
                count = r.count,
            )
        })
        .collect();

    format!(
        r#"<figure class="scope" aria-hidden="true"><div class="scope-screen"><svg viewBox="0 0 {W} {H}" preserveAspectRatio="none"><path class="fill" d="{live} L{W} {H} L0 {H} Z"/><path class="trace" vector-effect="non-scaling-stroke" pathLength="4000" d="{live}"/></svg>{markers}</div><div class="readouts">{readouts}</div></figure>"#,
    )
}

/// The one question the operator answers before any invite leaves the building.
///
/// A DOCUMENT AND NOT A `confirm()`, because the CSP at the top of this file is
/// `default-src 'none'` with no `script-src` at all, and every page here is
/// tested for having no `<script>` in it. An interruption that cannot be script
/// has to be a page, so this is one: the address it concerns, the question, and
/// the two ways out.
///
/// WHY THE QUESTION IS WORTH A WHOLE PAGE. The OAuth client is not verified yet
/// (`docs/VERIFICATION.md`), so Google refuses consent to any account that is
/// not on the project's test-user list. An invite mailed to somebody who is not
/// on that list is an invite to a wall: the code is spent, the row reads
/// "invited", and the first anyone hears of it is the person writing back to say
/// Google would not let them in. Asking costs a click. Not asking costs a
/// signup and a re-mint, and the board cannot tell the two apart afterwards.
///
/// NOTHING HAS HAPPENED YET when this renders. Every caller checks for the
/// confirmation BEFORE it approves, mints, revokes, or mails, so cancelling here
/// is genuinely a no-op rather than a half-finished approval.
pub fn admin_confirm(
    action: &str,
    hidden: &[(&str, &str)],
    who: &str,
    confirm_label: &str,
) -> Response {
    let fields: String = hidden
        .iter()
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
<p class="hint"><a href="/admin">Cancel and go back.</a> Nothing has been
approved, minted, or mailed.</p>"#,
            who = escape_html(who),
            action = escape_html(action),
            confirm_label = escape_html(confirm_label),
            fields = fields,
            CONFIRM_FIELD = CONFIRM_FIELD,
        ),
    )
}

/// The hidden field that says the operator answered the question on
/// [`admin_confirm`]. Lives here so the page that writes it and the handlers
/// that read it cannot drift to two different spellings.
pub const CONFIRM_FIELD: &str = "confirmed";

/// The "who" cell: the address this person is known by, — when the Google
/// account that actually signed up is a DIFFERENT address — a muted line naming
/// that too, and last, the name they typed into the form.
///
/// THE ADDRESS STAYS ON TOP even though the name is the more human of the two.
/// Every button on this page acts on an address, every refusal names one, and
/// the operator's own question before approving anybody ("is this account on
/// the Google test-user list?") is asked about an address. The name is context
/// for that decision and is rendered as context: beneath, and absent entirely
/// on the rows that have none rather than padded out with a dash.
///
/// THE LINES ARE ORDERED BY AUTHORITY, AND THE NAME IS QUOTED. Both of those
/// are one fix for one hole. The name arrives here as free text on a public,
/// unauthenticated form, and it used to render in the same muted line, with the
/// same wrapper, as the `signed up as` provenance below — so a stranger who
/// submitted the name `signed up as ada@example.com` produced a row that was
/// byte-for-byte identical to a genuine Google-verified one. That inverts the
/// trust ordering the whole page rests on: the provenance line is the ONE thing
/// on it that claims to come from Google rather than from a form, and on the
/// Waiting table it cannot legitimately appear at all, because `account_email`
/// is only ever written beside `signed_up_at`.
///
/// So the page asserts first and quotes last. The curly quotes are the frame,
/// and `store::normalize_name` strips both of those characters out of every
/// stored name, which is what makes the frame something free text cannot spell:
/// a quoted line is what somebody typed, an unquoted one is what this page
/// knows.
///
/// AN INVITE IS A BEARER CODE, so the two are not promised to match: a person
/// can be invited at work and sign up with a personal mailbox, or forward the
/// mail to somebody else entirely. Before this line the board silently showed
/// the invited address for a mailbox belonging to another address, which is the
/// one thing an operator reading this page cannot afford to guess at.
///
/// Shown only on a MISMATCH. Repeating the same address twice on every signed-up
/// row would be noise that trains the eye to skip the line that matters.
///
/// ALL THREE are escaped, like everything else on this page. `account_email`
/// comes from Google and the address had to pass `is_email` to be stored at
/// all, so the rule is not carried by either of them: the name is free text a
/// stranger typed into a public form, and it is the one this page would break
/// on. Nothing here is interpolated raw regardless of how well-behaved its
/// source is believed to be.
fn who_cell(r: &UserRow) -> String {
    let mut cell = escape_html(&r.email);
    if let Some(account) = r.account_email.as_deref()
        && account != r.email
    {
        cell.push_str(&format!(
            r#"<br><span class="muted">signed up as {}</span>"#,
            escape_html(account)
        ));
    }
    // Escaped like everything else here, and this one is the reason the rule
    // exists: it is the only string on this page that a stranger typed into a
    // public form. The quotes are the page's own and cannot be part of the
    // name; see above.
    if let Some(name) = r.name.as_deref() {
        cell.push_str(&format!(
            "<br><span class=\"given\">\u{201c}{}\u{201d}</span>",
            escape_html(name)
        ));
    }
    cell
}

/// The mailbox a row became, as a `<code>` label, or nothing at all.
///
/// Shared by the two `.done` rungs so the label renders identically on both: an
/// active row is a signed-up row that got further, and a difference in how they
/// spell the same tenant would read as a difference in what happened.
fn tenant_code(r: &UserRow) -> String {
    r.tenant_label
        .as_deref()
        .map(|l| format!(" <code>{}</code>", escape_html(l)))
        .unwrap_or_default()
}

/// One row's button, as its own form. No JavaScript on this page, so a button
/// that acts is a form that posts, and the row it acts on rides in a hidden
/// field.
fn action_form(action: &str, id: i64, label: &str, quiet: bool) -> String {
    let class = if quiet { r#" class="quiet""# } else { "" };
    format!(
        r#"<form method="post" action="{action}"><input type="hidden" name="id" value="{id}"><button type="submit"{class}>{label}</button></form>"#,
        action = escape_html(action),
        label = escape_html(label),
    )
}

/// A table, or a sentence saying there is nothing to put in one.
fn table(head: &str, rows: &str, empty: &str) -> String {
    if rows.is_empty() {
        format!(r#"<p class="muted">{}</p>"#, escape_html(empty))
    } else {
        format!("<table><tr>{head}</tr>\n{rows}</table>")
    }
}

/// The error banner every page above renders the same way.
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

/// A date as the operator reads it. Escaped like everything else: the rule is
/// that nothing is interpolated raw, and an exception for "this one is only
/// ever digits" is how the rule stops being one.
fn day(ts: DateTime<Utc>) -> String {
    escape_html(&ts.format("%Y-%m-%d").to_string())
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
        assert!(html.contains("gmail.readonly"));
        assert!(html.contains("gmail.modify"));
        assert!(html.contains("gmail.send"));
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
        assert!(folded.contains("needs all three"), "{html}");
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

    /// A user row as the store hands one over.
    fn row(id: i64, email: &str, notified: bool) -> UserRow {
        // 2026-01-01T00:00:00Z.
        let at = DateTime::from_timestamp(1_767_225_600, 0).unwrap();
        UserRow {
            id,
            email: email.to_string(),
            name: None,
            created_at: at,
            status: crate::store::USER_APPROVED.to_string(),
            approved_at: Some(at),
            invite_id: Some(7),
            notified_at: notified.then_some(at),
            signed_up_at: None,
            tenant_label: None,
            account_email: None,
            first_paired_at: None,
        }
    }

    /// The same row after they spent the code: a mailbox exists.
    fn accepted_row(id: i64, email: &str, label: &str) -> UserRow {
        let at = DateTime::from_timestamp(1_767_225_600, 0).unwrap();
        UserRow {
            signed_up_at: Some(at),
            tenant_label: Some(label.to_string()),
            account_email: Some(email.to_string()),
            ..row(id, email, true)
        }
    }

    /// ...and after a device that is not the console paired with it: the top of
    /// the ladder.
    fn active_row(id: i64, email: &str, label: &str) -> UserRow {
        let at = DateTime::from_timestamp(1_767_225_600, 0).unwrap();
        UserRow {
            first_paired_at: Some(at),
            ..accepted_row(id, email, label)
        }
    }

    /// The dashboard is the one page that renders a string a stranger typed
    /// into a public form, so it is the one page where escaping is the whole
    /// defense. `<`, `>`, and `"` all survive the address shape check.
    #[tokio::test]
    async fn the_dashboard_escapes_an_address_that_is_an_attack() {
        let hostile = r#""><script>alert(1)</script>@evil.test"#;
        let html = body_of(admin_page(
            &[row(1, hostile, false)],
            &[row(2, hostile, true)],
            Some("<b>nope</b>"),
        ))
        .await;
        assert!(!html.contains("<script>alert(1)"), "{html}");
        assert!(!html.contains("<b>nope</b>"), "{html}");
        assert!(html.contains("&lt;script&gt;"), "{html}");
        assert!(html.contains("&quot;&gt;"), "{html}");
    }

    /// The direct-invite form is ON the dashboard, and it is styled: the shared
    /// stylesheet named two input types by hand, so an `email` input rendered as
    /// an unstyled browser default next to the ones that are not.
    #[tokio::test]
    async fn the_dashboard_carries_a_styled_form_for_inviting_directly() {
        let page = body_of(admin_page(&[], &[], None)).await;
        assert!(page.contains(r#"action="/admin/invite""#), "{page}");
        assert!(page.contains(r#"name="email""#), "{page}");
        assert!(page.contains("Mint and email an invite"), "{page}");
        assert!(page.contains("input[type=email]"), "{page}");
        // Both palettes, or it is an unstyled box in one of them, plus the
        // console surface, which restates every colour rather than inheriting
        // the dark media query (see the `body.console` block).
        assert_eq!(page.matches("input[type=email]").count(), 3, "{page}");
        assert!(!page.contains("<script"), "{page}");
    }

    /// Which button a row gets is the whole state machine the operator sees: a
    /// stamped row is quiet, an unstamped one is loud and asks to be pressed.
    #[tokio::test]
    async fn an_unsent_invite_says_so_and_offers_the_button() {
        let sent = body_of(admin_page(&[], &[row(1, "ada@example.com", true)], None)).await;
        assert!(sent.contains("Invited 2026-01-01"), "{sent}");
        // Quiet: this row is fine, and the button is only there for somebody
        // who lost the mail. Matched WITH its label, because the board carries
        // a quiet sign-out button of its own at the foot.
        assert!(
            sent.contains(r#"<button type="submit" class="quiet">Re-send</button>"#),
            "{sent}"
        );
        assert!(!sent.contains("email not sent"), "{sent}");

        let failed = body_of(admin_page(&[], &[row(1, "ada@example.com", false)], None)).await;
        assert!(
            failed.contains(r#"<span class="flag">email not sent</span>"#),
            "{failed}"
        );
        // Loud, and the only row action on the board that is: this one is asking
        // to be pressed. No `class="quiet"` on THIS button, which is what the
        // label in the match is for.
        assert!(
            failed.contains(r#"<button type="submit">Send invite</button>"#),
            "{failed}"
        );

        let waiting = body_of(admin_page(&[row(1, "ada@example.com", false)], &[], None)).await;
        assert!(waiting.contains(">Approve</button>"), "{waiting}");
        assert!(waiting.contains(r#"name="id" value="1""#), "{waiting}");
        assert!(waiting.contains(r#"action="/admin/approve""#), "{waiting}");
        // No JavaScript on this page either: a button that acts is a form.
        assert!(!waiting.contains("<script"), "{waiting}");
    }

    /// THE FAR END OF THE FUNNEL, which the board could not show at all before
    /// the redemption join: an invite that was spent means a mailbox, and the
    /// row says which one.
    ///
    /// NO BUTTON ON THAT ROW. `admin::send` refuses a spent code (INVITE_SPENT)
    /// because a second code for somebody who already has a mailbox is a tenant
    /// nobody approved, so offering the press was offering a guaranteed
    /// refusal, and pressing it was the only way to discover they had signed up.
    #[tokio::test]
    async fn a_spent_invite_says_which_mailbox_it_became_and_offers_no_button() {
        let html = document_of(admin_page(
            &[],
            &[accepted_row(1, "ada@example.com", "ada")],
            None,
        ))
        .await;
        assert!(
            html.contains(r#"<span class="done">signed up</span>"#),
            "{html}"
        );
        assert!(html.contains("<code>ada</code>"), "{html}");
        assert!(html.contains("1 of those signed up"), "{html}");
        // The whole point: no press, and nothing that could mint a second code.
        assert!(!html.contains("Re-send"), "{html}");
        assert!(!html.contains("/admin/send"), "{html}");

        // An outstanding invite is unchanged: still offered, still quiet.
        let waiting_on_them =
            document_of(admin_page(&[], &[row(1, "ada@example.com", true)], None)).await;
        assert!(waiting_on_them.contains("Re-send"), "{waiting_on_them}");
        assert!(
            !waiting_on_them.contains("signed up</span>"),
            "{waiting_on_them}"
        );
        assert!(
            waiting_on_them.contains("0 of those signed up"),
            "{waiting_on_them}"
        );
    }

    /// THE TOP OF THE LADDER (#89): somebody who signed up AND opened the app.
    /// The whole point of the column was that "did anyone we invited ever run
    /// it" had no answer anywhere; this is the answer, on the row.
    ///
    /// Same `.done` family and same no-button rule as the rung below it, and the
    /// tally nests: an active person is counted as signed up too.
    #[tokio::test]
    async fn an_active_row_says_so_and_offers_no_button() {
        let html = document_of(admin_page(
            &[],
            &[active_row(1, "ada@example.com", "ada")],
            None,
        ))
        .await;
        assert!(
            html.contains(r#"<span class="done">active</span>"#),
            "{html}"
        );
        assert!(html.contains("<code>ada</code>"), "{html}");
        // Precedence, not history: the further rung wins the cell outright.
        assert!(!html.contains("signed up</span>"), "{html}");
        assert!(
            html.contains("1 of those signed up, 1 of those active"),
            "{html}"
        );
        // Nothing left to mail somebody who is using the product.
        assert!(!html.contains("Re-send"), "{html}");
        assert!(!html.contains("/admin/send"), "{html}");

        // A signed-up row that never paired is the other half of the count.
        let not_yet = document_of(admin_page(
            &[],
            &[accepted_row(1, "ada@example.com", "ada")],
            None,
        ))
        .await;
        assert!(
            not_yet.contains("1 of those signed up, 0 of those active"),
            "{not_yet}"
        );
        assert!(!not_yet.contains(r#">active</span>"#), "{not_yet}");
    }

    /// The name is context under the address, on both tables, and only when
    /// there is one: a row without a name is the row that has always been
    /// there and renders exactly as it did.
    #[tokio::test]
    async fn a_name_sits_under_the_address_when_there_is_one() {
        let named = UserRow {
            name: Some("Ada Lovelace".to_string()),
            status: crate::store::USER_PENDING.to_string(),
            approved_at: None,
            invite_id: None,
            notified_at: None,
            ..row(1, "ada@example.com", false)
        };
        let html = document_of(admin_page(&[named], &[], None)).await;
        assert!(html.contains("ada@example.com"), "{html}");
        assert!(
            html.contains("<span class=\"given\">\u{201c}Ada Lovelace\u{201d}</span>"),
            "{html}"
        );
        // The address is what every button here acts on, so it stays first.
        let who = html.find("ada@example.com").unwrap();
        assert!(who < html.find("Ada Lovelace").unwrap(), "{html}");

        // The one string on this page a stranger typed into a public form.
        let hostile = UserRow {
            name: Some(r#"<script>alert(1)</script>"#.to_string()),
            ..row(2, "eve@evil.test", true)
        };
        let html = document_of(admin_page(&[], &[hostile], None)).await;
        assert!(html.contains("&lt;script&gt;alert(1)"), "{html}");

        // No name, no extra line and no empty one.
        let html = document_of(admin_page(&[], &[row(3, "grace@example.com", true)], None)).await;
        assert!(!html.contains(r#"class="given""#), "{html}");
        assert!(html.contains("grace@example.com"), "{html}");
    }

    /// A NAME CANNOT SPELL THE ONE LINE ON THIS PAGE THAT COMES FROM GOOGLE.
    ///
    /// The provenance line is the page's most authoritative field and the name
    /// beneath it is free text off a public form. They shared a wrapper once,
    /// and a stranger submitting the name `signed up as ada@example.com`
    /// rendered a row byte-for-byte identical to a genuine one — an operator
    /// reading "this person already signed up as somebody I recognize" and
    /// approving an invite for an address that had done no such thing.
    ///
    /// Two things stop it and this test holds both: the frame (curly quotes
    /// that `normalize_name` strips out of every stored name, so nothing inside
    /// one can be one) and the order (what the page knows first, what somebody
    /// typed last).
    #[tokio::test]
    async fn a_name_cannot_forge_the_line_that_came_from_google() {
        let forged = UserRow {
            name: Some("signed up as ada@example.com".to_string()),
            status: crate::store::USER_PENDING.to_string(),
            approved_at: None,
            invite_id: None,
            notified_at: None,
            ..row(1, "eve@evil.test", false)
        };
        let forged = document_of(admin_page(&[forged], &[], None)).await;
        let genuine = UserRow {
            account_email: Some("ada@example.com".to_string()),
            ..accepted_row(1, "eve@evil.test", "eve")
        };
        let genuine = document_of(admin_page(&[], &[genuine], None)).await;

        let cell = |html: &str| {
            let start = html.find("eve@evil.test").unwrap();
            html[start..start + 120].to_string()
        };
        assert_ne!(
            cell(&forged),
            cell(&genuine),
            "the forgery is indistinguishable"
        );
        // The genuine line is unquoted and unmarked; the forged one is neither.
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
        let html = document_of(admin_page(&[], &[both], None)).await;
        assert!(
            html.find("eve-real@evil.test").unwrap() < html.find("ada@bigco.com").unwrap(),
            "{html}"
        );
    }

    /// AN INVITE IS A BEARER CODE, so the address that was invited and the
    /// Google account that signed up are two different facts. When they differ
    /// the board says so, and when they agree it says nothing rather than
    /// printing one address twice.
    ///
    /// The second line is ESCAPED like everything else here: it arrives from
    /// Google rather than from the public form, and the rule on this page does
    /// not care where a string came from.
    #[tokio::test]
    async fn a_signup_under_another_address_says_which() {
        let mismatch = UserRow {
            account_email: Some(r#"<script>alert(1)</script>@evil.test"#.to_string()),
            ..accepted_row(1, "ada@example.com", "ada")
        };
        let html = document_of(admin_page(&[], &[mismatch], None)).await;
        assert!(html.contains("signed up as"), "{html}");
        assert!(html.contains("ada@example.com"), "{html}");
        assert!(!html.contains("<script>alert(1)"), "{html}");
        assert!(html.contains("&lt;script&gt;"), "{html}");
        assert!(
            html.contains(r#"<span class="muted">signed up as"#),
            "{html}"
        );

        // Same address on both sides: one line, no second address, no noise.
        let agreed = document_of(admin_page(
            &[],
            &[accepted_row(1, "ada@example.com", "ada")],
            None,
        ))
        .await;
        assert!(!agreed.contains("signed up as"), "{agreed}");
    }

    /// A DELIBERATE SIGN OUT IS NOT A REFUSAL. The session runs for a month
    /// now, so ending one on a borrowed machine had to become a press rather
    /// than a trip to Railway to rotate the token; and the page that press lands
    /// on answers 200, because the request did exactly what was asked. The
    /// expired-session door is the 401, and still says so.
    #[tokio::test]
    async fn signing_out_is_a_200_and_being_thrown_out_is_a_401() {
        let out = admin_signed_out();
        assert_eq!(out.status(), StatusCode::OK);
        let html = document_of(out).await;
        assert!(html.contains("Signed out."), "{html}");
        // The door, not a dead end: the way back in is on it.
        assert!(html.contains(r#"action="/admin/login""#), "{html}");
        // Not dressed as a refusal.
        assert!(!html.contains(r#"class="stop""#), "{html}");

        assert_eq!(
            admin_login(Some("Your admin session has ended.")).status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(admin_login(None).status(), StatusCode::OK);
    }

    /// The exit is ON the board, and it is a form, because a link cannot carry
    /// the same-origin POST every other admin action on this page is held to.
    #[tokio::test]
    async fn the_board_carries_the_way_out() {
        let html = document_of(admin_page(&[], &[], None)).await;
        assert!(html.contains(r#"action="/admin/logout""#), "{html}");
        assert!(html.contains("Sign out"), "{html}");
        // The page states its own lifetime rather than leaving the operator to
        // discover it by being signed out.
        assert!(html.contains("This session lasts 30 days"), "{html}");
    }

    /// The door names nothing that is behind it, and a refusal is a 401 rather
    /// than a 200 with bad news in it.
    #[tokio::test]
    async fn the_admin_door_says_nothing_and_refuses_with_a_status() {
        let r = admin_login(None);
        assert_eq!(r.status(), StatusCode::OK);
        let html = body_of(r).await;
        assert!(html.contains(r#"type="password""#), "{html}");
        assert!(html.contains(r#"action="/admin/login""#), "{html}");
        assert!(!html.to_lowercase().contains("waitlist"), "{html}");

        let refused = admin_login(Some("That token was not accepted."));
        assert_eq!(refused.status(), StatusCode::UNAUTHORIZED);
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
            body_of(admin_page(
                &[row(1, "ada@example.com", false)],
                &[row(2, "bob@example.com", true)],
                Some("no"),
            ))
            .await,
            body_of(admin_page(&[], &[], None)).await,
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
            body_of(admin_page(&[row(1, "ada@example.com", false)], &[], None)).await,
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
            admin_page(&[row(1, "ada@example.com", false)], &[], None),
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
