import {
  type ReactNode,
  type RefObject,
  useEffect,
  useMemo,
  useRef,
  useState,
  type FormEvent,
  type MouseEvent,
  type PointerEvent,
} from "react";
import { CARRIERS, createScope, markerPoint, type ScopeLayout } from "./scope";
import { SF_SYMBOLS } from "./sf-symbols";

// The "before" panel is a procedurally repeated fake inbox: the drudgery
// Passband exists to kill. Snippets are written long so rows run the full
// width of the panel.
const FAKE_EMAILS: Array<[sender: string, subject: string, snippet: string]> = [
  ["LinkedIn", "You appeared in 9 searches this week", "See who's looking for someone like you. Recruiters from companies you've never heard of are searching for profiles matching yours..."],
  ["Cats Weekly", "Why your cat stares at the wall", "Nine boxes, ranked by sit quality. Plus: a reader writes in about a cat who has learned to open the fridge and what it..."],
  ["no-reply@accounts", "Security alert for your account", "A new sign-in was detected on a device you may or may not recognize. If this was you, no action is needed. If this wasn't..."],
  ["DoorDash", "Your Friday deserves 40% off", "Hungry? Use code FRIYAY at checkout before midnight and get 40% off orders over $35, up to a maximum discount of $8..."],
  ["Confluence", "Weekly digest: 47 updates in spaces you follow", "Q3 Planning Doc was edited by 6 people. Retro Notes (DRAFT) (COPY) was moved. A page you commented on in 2024 was..."],
  ["Marriott Bonvoy", "Points update: you have 312 points", "Your points balance summary for the month. You are 87,688 points away from your next free night at participating..."],
  ["Haightssion", "This month's programme", "Three talks, a walk and a pottery class. Doors at seven, and the walk leaves from the corner whether or not you have..."],
  ["billing@saas.io", "Your receipt from Acme SaaS #48211", "Amount paid: $12.00. Thank you for your continued subscription to a service you forgot you signed up for. Manage your..."],
  ["Skyward Miles", "Your miles are expiring soon", "Don't lose your 1,204 miles. Book by the end of the month or transfer them to a partner at an exchange rate that will..."],
  ["HR Announcements", "REMINDER: Open enrollment closes Friday", "This is your final reminder to complete your benefits elections. If you do not act, your current elections will roll..."],
  ["The Federal Overstatement", "Markets react calmly to everything", "The Case Against Breakfast, and other essays. Plus: a 4,000-word post about someone's move to Lisbon and what it..."],
  ["Twitter", "You have 4 new notifications", "@someguy and 3 others liked a post you were mentioned in. See what you're missing. Your network has been busy while..."],
  ["Zoom", "Your cloud recording is now available", "Meeting recording: Weekly Sync. Duration: 58 minutes. This recording will be automatically deleted in 30 days and..."],
  ["Chase", "Your statement is ready", "Your February statement for account ending in 4482 is now available. Your minimum payment is due in 21 days. Log in..."],
  ["GitHub", "[repo] 23 new notifications", "dependabot opened 14 pull requests in repositories you have not touched since 2023. Bump lodash from 4.17.20 to..."],
  ["Eventbrite", "Events near you this weekend", "Networking mixers, fun runs, and a pottery class. Based on your interests: 12 events happening within 25 miles of..."],
  ["Google Calendar", "Daily agenda for Tuesday", "You have 7 events scheduled today starting with Standup at 9:00 AM, followed by a meeting that could have been an..."],
  ["Brightly", "48 hours only: 40% off everything", "Everything you looked at once is on sale. So is everything you did not. Ends Thursday at midnight, or when we send the..."],
  ["Rainforest", "Deals picked for you", "Based on one search in 2023, we think you will love these 14 extension cords. Customers who bought nothing also bought..."],
  ["The Team", "We've updated our Privacy Policy", "We're writing to let you know about some updates to our Privacy Policy and Terms of Service, effective in 30 days..."],
  ["Slack", "You have unread messages in #general", "While you were away, 312 messages were posted in channels you follow, including a heated thread about the office..."],
];
// The same inbox's mail that mattered, buried in the scroll exactly as it would
// be. These are the rows the "after" panel pulls forward, so the two panels are
// honestly one mailbox before and after, not two different ones.
const BURIED: Array<[sender: string, subject: string, snippet: string]> = [
  ["Jamie Chen", "A quick decision before Friday", "Hey! Two options for the offsite venue, I need your pick by Friday so we can hold the date..."],
  ["Parkline Properties", "Lease renewal: signature needed", "Your renewal packet is ready. Please review and sign by September 23 to lock in your current rate..."],
  ["ExFed", "Your package is out for delivery", "Keychron Q1 Max. Scheduled delivery: today by 8:00 PM. Track your package for live updates..."],
  ["Dr. Ortiz's Office", "Please confirm Thursday's appointment", "Reply C to confirm your appointment on Thursday at 2:30 PM, or call us to reschedule..."],
];

// Every colour on the page, lifted from the Swift client's dark palette
// (Design/Palette.swift) so the site and the app are one product. The page is
// always night: the scene is the brand, and its glow version is the one worth
// leading with.
const PAGE_CSS = `
:root {
  --bg: #090D16;
  --canvas: #0E141D;
  --card: #121A26;
  --ink: #E9EEF5;
  --dim: #A8B4C4;
  --faint: #808E9F;
  --faintest: #62707F;
  --hair: rgba(233, 238, 245, 0.08);
  --hair-strong: rgba(233, 238, 245, 0.14);
  --accent: #4E9BEA;
  --accent-ink: #82BAF5;
  --warn: #E8AC4C;
  --positive: #4FC08A;
  --danger: #FF7A68;
  --serif: "Newsreader", ui-serif, Georgia, serif;
  --mono: ui-monospace, "SF Mono", Menlo, monospace;
  --sans: -apple-system, BlinkMacSystemFont, "SF Pro Text", system-ui, sans-serif;
  --gutter: clamp(1rem, 4vw, 2.75rem);
}
html { background: var(--bg); color-scheme: dark; }
body { margin: 0; background: var(--bg); color: var(--ink); font-family: var(--sans);
  -webkit-font-smoothing: antialiased; }
.pb a { color: inherit; }

/* THE STORY. A tall section with a sticky stage inside it: the scroll through
   the section is the squelch closing. The stage is only the BACKGROUND: the
   words are ordinary page laid over it (.pb-track) and leave at the speed of
   the wheel, so nothing ever holds the scroll. The first leaf of copy goes up
   and away, the second comes up from under the fold, and the scope closes in
   the time between them. */
.pb-story { position: relative; height: 182vh; height: 182svh; }
.pb-track { position: absolute; inset: 0; z-index: 2; }
.pb-leaf { position: relative; height: 100vh; height: 100svh; }
/* Pulled well up under the first, so the second headline is already on its
   way in as the first one leaves: the gap between them is a short scroll, not
   a screen of empty scope. As far as it can go without showing at rest. */
.pb-leaf-1 { margin-top: -38vh; margin-top: -38svh; }
.pb-stage { position: sticky; top: 0; height: 100vh; height: 100svh; overflow: hidden; }
.pb-scene { position: absolute; inset: 0; width: 100%; height: 100%; display: block; }
/* The analyzer's graticule: ten divisions across, eight down, faded out at the
   edges so the screen has no hard border to sit inside. */
.pb-grat { position: absolute; inset: 0; pointer-events: none;
  background-image:
    repeating-linear-gradient(90deg, rgba(130, 186, 245, 0.07) 0 1px, transparent 1px 10%),
    repeating-linear-gradient(180deg, rgba(130, 186, 245, 0.07) 0 1px, transparent 1px 12.5%);
  -webkit-mask-image: radial-gradient(ellipse 75% 70% at 62% 55%, #000 30%, transparent 85%);
          mask-image: radial-gradient(ellipse 75% 70% at 62% 55%, #000 30%, transparent 85%); }
/* Markers: numbered like an analyzer's, riding the carriers' peaks. They come
   up only once the squelch has closed, which is when there is something to
   point at. */
.pb-mkr { position: absolute; z-index: 1; transform: translate(-50%, calc(-100% - 12px)); pointer-events: none;
  font: 600 11px/1 ui-monospace, "SF Mono", Menlo, monospace; color: var(--accent-ink);
  display: flex; flex-direction: column; align-items: center; gap: 3px;
  opacity: 0; transition: opacity 0.5s ease; }
.pb-mkr::after { content: ""; border: 5px solid transparent; border-top: 6px solid var(--accent); border-bottom: 0; }
.pb-stage[data-locked] .pb-mkr { opacity: 1; }
.pb-mkr:nth-of-type(2) { transition-delay: 0.08s; } .pb-mkr:nth-of-type(3) { transition-delay: 0.16s; }
.pb-mkrs { position: absolute; z-index: 2; right: var(--gutter); top: 17%; width: 19rem;
  font: 500 12px/1.2 ui-monospace, "SF Mono", Menlo, monospace; color: var(--dim);
  border: 1px solid rgba(130, 186, 245, 0.2); border-radius: 10px; background: rgba(9, 13, 22, 0.7);
  -webkit-backdrop-filter: blur(6px); backdrop-filter: blur(6px);
  opacity: 0; transform: translateY(6px); transition: opacity 0.6s ease 0.2s, transform 0.6s ease 0.2s; }
.pb-stage[data-locked] .pb-mkrs { opacity: 1; transform: none; }
.pb-mkrs div { display: grid; grid-template-columns: 1.6rem 1fr auto; gap: 0.6rem; padding: 0.55rem 0.8rem;
  border-top: 1px solid rgba(130, 186, 245, 0.1); }
.pb-mkrs div:first-child { border-top: 0; font-size: 10px; letter-spacing: 0.1em; color: var(--faintest); }
.pb-mkrs b { color: var(--accent-ink); font-weight: 600; }
.pb-mkrs span:last-child { text-align: right; }
.pb-mkrs .late { color: var(--danger); } .pb-mkrs .soon { color: var(--warn); }
/* The readouts along the screen's foot. SQUELCH is live: it is the scroll. */
.pb-read-l, .pb-read-r { position: absolute; z-index: 2; bottom: 1.4rem;
  font: 500 11px/1 ui-monospace, "SF Mono", Menlo, monospace; letter-spacing: 0.08em; color: var(--faintest); }
.pb-read-l { left: var(--gutter); } .pb-read-r { right: var(--gutter); }
.pb-read-r b { color: var(--accent-ink); font-weight: 600; display: inline-block; min-width: 4ch; text-align: right; }
/* The scrim, the web twin of the intro's: the backdrop's own colour, so it
   melts into the scene rather than tinting it. From the leading edge on wide
   screens, from the top on narrow ones where the copy sits above the waves. */
.pb-scrim { position: absolute; inset: 0; pointer-events: none;
  background:
    linear-gradient(90deg, rgba(9,13,22,0.95) 0%, rgba(9,13,22,0.82) 30%, rgba(9,13,22,0.4) 44%, rgba(9,13,22,0) 60%),
    linear-gradient(180deg, rgba(9,13,22,0) 72%, rgba(9,13,22,0.85) 100%); }
.pb-top { position: absolute; inset: 0 0 auto; display: flex; align-items: center;
  justify-content: space-between; padding: 1.4rem var(--gutter); z-index: 2; }
.pb-brand { display: flex; align-items: center; gap: 0.6rem; text-decoration: none; }
.pb-brand img { width: 42px; height: auto; }
.pb-brand span { font-family: var(--serif); font-size: 1.5rem; font-weight: 500; letter-spacing: -0.005em; }
.pb-nav { display: flex; gap: 1.4rem; font-size: 0.9rem; }
.pb-nav a, .pb-foot a { color: var(--faint); text-decoration: none; transition: color 0.2s; }
.pb-nav a:hover, .pb-foot a:hover { color: var(--ink); }

.pb-copy { position: absolute; z-index: 2; left: var(--gutter); top: 50%;
  transform: translateY(-50%); width: min(30rem, calc(100% - 2 * var(--gutter)));
  display: flex; flex-direction: column; gap: 1.6rem; }
.pb-beat { display: flex; flex-direction: column; gap: 1rem; }
.pb-lede { margin: 0; font-size: clamp(1.05rem, 1.5vw, 1.2rem); line-height: 1.4; color: var(--dim); }
.pb-hero { margin: 0; font-family: var(--serif); font-weight: 500;
  font-size: clamp(2.4rem, 4.6vw, 3.4rem); line-height: 1.02; letter-spacing: -0.012em; }
.pb-sub { margin: 0; font-size: 1rem; line-height: 1.6; color: var(--dim); max-width: 27rem; }
/* Reserves the open rig's height, so the vertically centred copy above never
   jumps when the button opens. The fine print rides under whichever control
   is showing and the unused room sits below it, where it reads as margin. */
.pb-slot { min-height: 8.2rem; display: flex; flex-direction: column; align-items: flex-start; gap: 0.65rem; }
.pb-slot .pb-cta { margin-top: 0; }
/* The rig arrives where the button was, rising out of the slot rather than
   popping in, so the press reads as the button opening up. */
.pb-slot .pb-rig { animation: pb-rig-in 0.45s cubic-bezier(0.2, 0.8, 0.2, 1); }
@keyframes pb-rig-in { from { opacity: 0; transform: translateY(6px); } }
.pb-fine { margin: 0; font-size: 0.8rem; color: var(--faintest); }
.pb-copy .pb-rig { margin-top: 0; }
.pb-confirm { margin: 0; color: var(--ink); font-size: 1rem; overflow-wrap: anywhere; }
.pb-status { margin: 0; color: var(--faint); font-size: 0.9rem; line-height: 1.5; max-width: 26rem; }
.pb-status-error { color: var(--danger); }
.pb-cue { position: absolute; z-index: 2; left: 50%; bottom: 1.4rem; transform: translateX(-50%);
  font-size: 0.72rem; letter-spacing: 0.14em; text-transform: uppercase; color: var(--faintest);
  transition: opacity 0.5s ease; display: flex; flex-direction: column; align-items: center; gap: 0.5rem; }
.pb-cue::after { content: ""; width: 1px; height: 26px;
  background: linear-gradient(var(--faintest), transparent); animation: pb-cue 2.2s ease-in-out infinite; }
@keyframes pb-cue { 0%, 100% { transform: scaleY(0.4); transform-origin: top; } 50% { transform: scaleY(1); transform-origin: top; } }
.pb-stage[data-scrolled] .pb-cue { opacity: 0; }

/* PHONES: the words at the top, the scope in the middle, the waitlist at the
   bottom under the thumb. The copy column stretches the stage's height and
   splits: the beats hold the top, the slot the foot, and the scope draws in
   the gap between them (scopeLayout measures it). The scrim darkens only
   behind the words and the form, never the trace. */
@media (max-width: 820px) {
  .pb-scrim { background:
    linear-gradient(180deg, rgba(9,13,22,0.92) 0%, rgba(9,13,22,0.55) 26%, rgba(9,13,22,0) 36%),
    linear-gradient(180deg, rgba(9,13,22,0) 70%, rgba(9,13,22,0.85) 82%); }
  .pb-copy { top: 5.2rem; bottom: 3rem; transform: none; gap: 1.25rem; justify-content: space-between; }
  .pb-nav a:not(.pb-keep) { display: none; }
  .pb-cue, .pb-mkrs, .pb-read-l { display: none; }
  /* The words cross the trace on their way out here, so they carry their own
     ground with them. */
  .pb-beat { text-shadow: 0 0 6px var(--bg), 0 0 18px var(--bg), 0 0 30px var(--bg); }
  /* The second leaf's words sit at its top here, so it can come up less. */
  .pb-story { height: 200vh; height: 200svh; }
  .pb-leaf-1 { margin-top: -8vh; margin-top: -8svh; }
}

/* BEFORE / AFTER. */
.pb-section { padding: clamp(4.5rem, 10vw, 8rem) var(--gutter); max-width: 84rem; margin: 0 auto; }
.pb-h2 { margin: 0 0 0.9rem; font-family: var(--serif); font-weight: 500;
  font-size: clamp(2rem, 3.8vw, 2.9rem); line-height: 1.05; letter-spacing: -0.01em; }
.pb-h2 em { font-style: normal; color: var(--accent-ink); }
.pb-intro { margin: 0 0 clamp(2rem, 5vw, 3.25rem); color: var(--dim); font-size: 1.05rem;
  line-height: 1.6; max-width: 34rem; }
/* THE PAIR, AND THE TOUR THAT GROWS OUT OF IT. The left column is ordinary
   page: the "before" inbox, then one caption per feature, all scrolling at
   the speed of the wheel. The window on the right holds its place beside them
   and changes as each caption comes level with it. Nothing is captured: the
   page never stops moving, only the window keeps you company.

   The window's size is worked out here, once, from the pair's own width and
   the viewport's height, so it always fits on screen whole and the "before"
   panel can be cut to the same height. */
.pb-pair { --g: clamp(0.75rem, 1.6vw, 1.25rem);
  --shot-w: min((100cqw - 3.25rem - 2 * var(--g)) * 0.71, (100svh - 7rem) * 1.4444);
  --shot-h: calc(var(--shot-w) * 0.6923);
  container-type: inline-size; display: grid; grid-template-columns: minmax(0, 1fr) auto;
  column-gap: var(--g); align-items: start; }
.pb-before { grid-column: 1; grid-row: 1; }
.pb-hold { grid-column: 2; grid-row: 1 / span 2; position: sticky;
  top: calc((100svh - var(--shot-h) - 2rem) / 2); display: flex; gap: var(--g); align-items: stretch; }
/* The room under the last caption is what lets it reach the middle of the
   screen before the window lets go. */
.pb-steps { grid-column: 1; grid-row: 2; list-style: none; margin: 0; padding: 0 0 16svh; }
.pb-step { min-height: 56svh; display: flex; flex-direction: column; justify-content: center; gap: 0.6rem;
  padding-right: 1rem; opacity: 0.26; transition: opacity 0.4s ease; }
.pb-step[data-on] { opacity: 1; }
.pb-step i { font: 600 11px/1 var(--mono); font-style: normal; letter-spacing: 0.1em; color: var(--accent-ink); }
.pb-step h3 { margin: 0; font-family: var(--serif); font-weight: 500; font-size: clamp(1.5rem, 2.3vw, 2rem);
  line-height: 1.1; letter-spacing: -0.008em; }
.pb-step p { margin: 0; color: var(--dim); font-size: 1rem; line-height: 1.6; max-width: 22rem; }
/* THE CLOUDS behind the tours (see Clouds). Wide screens only: a few large,
   soft passband-coloured glows on a stage pinned while the section is on
   screen, faded at the screen's top and bottom so a section's edge never cuts
   one. Clipped sideways only. */
.pb { overflow-x: clip; }
.pb-clouded { position: relative; isolation: isolate; }
.pb-clouds { display: none; }
@media (min-width: 901px) {
  .pb-clouds { display: block; position: absolute; z-index: -1; pointer-events: none;
    top: 0; bottom: 0; left: calc(50% - 50vw); width: 100vw; overflow: clip; }
  .pb-clouds-stage { position: sticky; top: 0; height: 100vh; height: 100svh; overflow: hidden;
    -webkit-mask-image: linear-gradient(transparent, #000 18%, #000 82%, transparent);
            mask-image: linear-gradient(transparent, #000 18%, #000 82%, transparent); }
  .pb-cloud { position: absolute; will-change: transform; filter: blur(70px); }
}
/* THE GATE between the two panels: a lit filament with the squelch sitting on
   it, the same passband blue the scope's filter curve draws in. The before is
   everything upstream of it and the after is what comes out, so the divider is
   the product rather than a gap. Horizontal once the panels stack. */
.pb-gate { position: relative; width: 3.25rem; display: grid; place-items: center;
  margin-top: 2rem; /* level with the windows, below the captions */ }
.pb-gate::before { content: ""; position: absolute; top: 0; bottom: 0; left: 50%; width: 1px;
  background: linear-gradient(transparent, rgba(78, 155, 234, 0.75) 25%, rgba(78, 155, 234, 0.75) 75%, transparent);
  box-shadow: 0 0 14px 1px rgba(78, 155, 234, 0.45); }
.pb-gate span { position: relative; width: 2.6rem; height: 2.6rem; border-radius: 50%; display: grid;
  place-items: center; color: var(--accent-ink); background: #0d1624;
  box-shadow: inset 0 0 0 1px rgba(78, 155, 234, 0.55), 0 0 26px -2px rgba(78, 155, 234, 0.55); }
.pb-gate svg { width: 1.05rem; height: 1.05rem; }
/* PHONES: one column. The window sticks to the top of the screen and the
   captions pass under it, on a ground that fades them out before they reach it. */
@media (max-width: 900px) {
  .pb-pair { display: block; --shot-w: 100cqw; }
  .pb-hold { display: contents; }
  .pb-after { position: sticky; top: 0; z-index: 2; padding: 0.7rem 0 1.6rem;
    background: linear-gradient(var(--bg) 86%, rgba(9, 13, 22, 0)); }
  .pb-step { min-height: 40svh; justify-content: flex-start; padding-top: 1rem; }
  .pb-steps { padding-bottom: 0; }
  .pb-gate { width: auto; height: 3.25rem; margin: 0.25rem 0; }
  .pb-gate::before { top: 50%; bottom: auto; left: 0; right: 0; width: auto; height: 1px;
    background: linear-gradient(90deg, transparent, rgba(78, 155, 234, 0.75) 25%, rgba(78, 155, 234, 0.75) 75%, transparent); }
  .pb-gate svg { transform: rotate(90deg); }
}
.pb-panel { margin: 0; display: flex; flex-direction: column; gap: 0.8rem; min-width: 0; }
.pb-panel figcaption { display: flex; align-items: baseline; gap: 0.6rem; font-size: 0.82rem; color: var(--faint); }
.pb-panel figcaption b { font-size: 0.7rem; letter-spacing: 0.14em; text-transform: uppercase;
  font-weight: 600; color: var(--faintest); }
.pb-panel figcaption b { font-size: 0.78rem; }
.pb-after figcaption b { color: var(--accent); }
.pb-after figcaption { color: var(--dim); }
/* Before is drained: grey, dim, a world with no signal in it. Only its unread
   count keeps its colour, which is the point of it. After is lit: edged in
   passband blue with the glow of the gate it came through. */
.pb-before .pb-window { filter: saturate(0.25) brightness(0.82); background: #0c1017; }
.pb-after .pb-window { border-color: rgba(78, 155, 234, 0.45);
  box-shadow: 0 0 0 1px rgba(78, 155, 234, 0.12), 0 0 60px -12px rgba(78, 155, 234, 0.35),
    0 30px 80px -20px rgba(0, 0, 0, 0.7); }
.pb-window { position: relative; border-radius: 14px; overflow: hidden;
  border: 1px solid var(--hair-strong); background: var(--canvas);
  box-shadow: 0 30px 80px -20px rgba(0, 0, 0, 0.7), inset 0 1px 0 rgba(255, 255, 255, 0.04); }
.pb-before .pb-window { height: var(--shot-h); }
@media (max-width: 900px) { .pb-before .pb-window { height: 18rem; } }

/* The doomscroll. Dimmed and faded at both ends: it is the "before", so it
   should read as a texture you are drowning in, not as something to read. */
.pb-inbox { position: absolute; inset: 0; overflow: hidden;
  -webkit-mask-image: linear-gradient(transparent, #000 14%, #000 80%, transparent);
          mask-image: linear-gradient(transparent, #000 14%, #000 80%, transparent); }
.pb-row { display: flex; align-items: baseline; gap: 0.75rem; padding: 0.6rem 1rem;
  border-bottom: 1px solid var(--hair); white-space: nowrap; font-size: 0.8rem; }
.pb-row-sender { width: 7.5rem; flex-shrink: 0; font-weight: 600; color: #b9c2cd;
  overflow: hidden; text-overflow: ellipsis; }
.pb-row-text { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; color: var(--faintest); }
.pb-row-text span { color: #9aa5b3; }
.pb-row-time { flex-shrink: 0; font-size: 0.7rem; color: var(--faintest); }
.pb-unread { margin-left: auto; font-size: 0.72rem; font-weight: 600;
  padding: 0.2rem 0.55rem; border-radius: 99px; color: #fff; background: #c7433a;
  font-variant-numeric: tabular-nums; }

/* THE AFTER: a fake screenshot of the sitrep, built in HTML rather than shipped
   as a PNG so it stays sharp at every size and every word in it is real text.

   Measured off the real app (the practice board, shot at 1440pt) and written
   in the app's own POINTS: every "Npt" below is rewritten to a multiple of
   --pt, which is the window's width over the width the mock is laid out at (1040pt, about the app's smallest window).
   So the mock is the app at one fixed layout, scaled as a picture would be,
   rather than a web page that reflows into something the app never draws.
   Colours are sampled from the same shot. */
.pb-after .pb-window { background: #101826; }
/* THE SHOT: the window plus the two things that float over it. It is the
   container the points are measured against, so the login code panel can hang
   off the window's bottom edge and still be drawn in the same points. */
.pb-shot { position: relative; container-type: inline-size; width: var(--shot-w); }
.pb-app { --pt: calc(100cqw / 1040); }
.pb-app { position: relative; display: grid; grid-template-columns: 60pt minmax(0, 1fr);
  grid-template-rows: 40pt minmax(0, 1fr); height: 720pt; font-size: 12pt; color: var(--ink); line-height: 1.25; }
.pb-app-bar { grid-column: 1 / -1; display: flex; align-items: center; gap: 14pt; padding: 0 18pt 0 16pt; }
.pb-lights { display: flex; gap: 8pt; }
.pb-lights i { width: 12pt; height: 12pt; border-radius: 50%; background: #ec6a5e; }
.pb-lights i:nth-child(2) { background: #f4bf4f; } .pb-lights i:nth-child(3) { background: #61c554; }
.pb-app-title { display: flex; align-items: baseline; gap: 7pt; margin-left: 12pt; }
.pb-app-title b { font-family: var(--serif); font-weight: 500; font-size: 19pt; }
.pb-app-title small { font-size: 10pt; font-weight: 500; letter-spacing: 0.06em; color: var(--faintest); }
/* Compose and search, at the bar's right end. */
.pb-app-tools { margin-left: auto; display: flex; align-items: center; gap: 2pt; color: var(--faint); }
.pb-app-tools span { width: 30pt; height: 30pt; display: grid; place-items: center; }
.pb-app-tools svg { width: 17pt; height: 17pt; }
/* The rail: three destinations on the digit keys, then the discovery ring,
   a hairline, and the account's own group. Rules and Groups live in Settings. */
.pb-side { background: #0c131e; border-right: 1px solid #1b2636; border-top-right-radius: 12pt;
  display: flex; flex-direction: column; align-items: center; gap: 6pt; padding: 10pt 0; }
.pb-side span { width: 44pt; height: 36pt; display: grid; place-items: center; border-radius: 11pt; color: #9aa6b6; }
.pb-side span.on { background: rgba(78, 155, 234, 0.26); color: var(--accent); }
.pb-side span.up { color: var(--accent); }
.pb-side svg { width: 17pt; height: 17pt; }
.pb-side .pb-side-gap { flex: 1; }
.pb-side hr { width: 22pt; height: 0; margin: 4pt 0; border: 0; border-top: 1px solid var(--hair-strong); }
.pb-side .pb-me { width: 26pt; height: 26pt; margin-top: 5pt; border-radius: 50%; overflow: hidden;
  display: block; box-shadow: inset 0 0 0 0.5pt rgba(255, 255, 255, 0.12); }
.pb-side .pb-me { object-fit: cover; }
.pb-page { padding: 0 24pt 0; display: grid; grid-template-columns: minmax(0, 1fr) 306pt;
  column-gap: 18pt; align-items: start; align-content: start; }
.pb-dash-hero { grid-column: 1 / -1; padding: 20pt 4pt 18pt; }
.pb-dash-hero small { display: block; font-size: 10pt; font-weight: 500; letter-spacing: 0.12em;
  text-transform: uppercase; color: var(--accent); margin-bottom: 7pt; }
.pb-dash-hero b { position: relative; display: inline-block; font-family: var(--serif); font-weight: 500;
  font-size: 38pt; letter-spacing: -0.01em; line-height: 1.08; }
.pb-col { display: flex; flex-direction: column; gap: 16pt; min-width: 0; }
.pb-zone { position: relative; border-radius: 18pt; padding: 12pt 12pt 10pt; background: #152234;
  box-shadow: inset 0 0 0 0.75pt #24374f; }
.pb-zone-h { display: flex; align-items: center; gap: 8pt; padding: 0 4pt 10pt; font-size: 13pt; font-weight: 600; }
.pb-zone-h svg { width: 14pt; height: 14pt; color: var(--zone, var(--accent)); flex: none; }
.pb-zone-h em { font-style: normal; font-size: 11pt; font-weight: 600; color: var(--faint);
  background: rgba(233, 238, 245, 0.08); border-radius: 99pt; padding: 1pt 6pt; }
.pb-zone-h small { font-size: 10pt; font-weight: 500; color: var(--faintest); }
/* FLOW MODE, in For your eyes' corner: a pill that says what it is. */
.pb-flow { margin-left: auto; display: flex; align-items: center; gap: 5pt;
  padding: 3pt 9pt 3pt 8pt; border-radius: 99pt; background: rgba(78, 155, 234, 0.1);
  font-size: 10pt; font-weight: 500; color: var(--dim); }
.pb-flow svg { width: 10pt; height: 10pt; color: var(--accent); }
.pb-eye { position: relative; display: flex; align-items: center; gap: 9pt; padding: 7pt 10pt; border-radius: 9pt;
  margin-top: 1pt; white-space: nowrap; }
.pb-eye.cursor { background: rgba(78, 155, 234, 0.1); }
.pb-eye.overdue.cursor { background: rgba(255, 122, 104, 0.09); }
.pb-eye.overdue::before { content: ""; position: absolute; left: 0; top: 5pt; bottom: 5pt; width: 2pt;
  border-radius: 1pt; background: var(--danger); }
.pb-avatar { width: 22pt; height: 22pt; border-radius: 50%; flex: none; display: grid; place-items: center;
  font-size: 9pt; font-weight: 600; }
.pb-eye b { font-weight: 500; flex: none; }
.pb-eye svg { width: 10pt; height: 10pt; flex: none; color: var(--faintest); margin-left: -3pt; }
.pb-eye span.line { color: var(--dim); overflow: hidden; text-overflow: ellipsis; flex: 1; min-width: 0; }
.pb-chip { flex: none; font-size: 10pt; font-weight: 500; padding: 2.5pt 7pt; border-radius: 99pt; color: var(--c);
  box-shadow: inset 0 0 0 0.75pt color-mix(in srgb, var(--c) 30%, transparent); }
.pb-chip.filled { background: color-mix(in srgb, var(--c) 16%, transparent); }
/* Reading: articles first on warm paper, promotions after them on plain
   ground. One card per sender, however many times they wrote. */
.pb-reading { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: 10pt; }
.pb-read { display: flex; gap: 9pt; padding: 9pt; border-radius: 12pt; background: rgba(233, 238, 245, 0.05); min-width: 0; }
.pb-read.content { background: rgba(232, 207, 158, 0.16); }
.pb-logo { width: 46pt; height: 46pt; border-radius: 15%; flex: none; display: block; }
.pb-read-text { min-width: 0; flex: 1; display: flex; flex-direction: column; gap: 6pt; }
.pb-read-top { display: flex; justify-content: space-between; align-items: baseline; gap: 6pt; }
.pb-read-top b { font-weight: 600; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.pb-read-top span { font-size: 10pt; font-weight: 500; color: var(--faintest); white-space: nowrap; }
.pb-read p { margin: 0; font-size: 10pt; font-weight: 500; line-height: 1.3; color: var(--faint);
  display: -webkit-box; -webkit-line-clamp: 2; -webkit-box-orient: vertical; overflow: hidden; }
/* The records rail: one TINTED card per zone, the zone's own hue washed into
   its ground and its border, which is the single biggest thing that makes the
   sitrep read as the sitrep. */
.pb-rail { display: flex; flex-direction: column; gap: 14pt; min-width: 0; }
.pb-rec { position: relative; border-radius: 18pt; padding: 12pt 11pt 9pt; background: var(--bg-c);
  box-shadow: inset 0 0 0 0.75pt var(--edge); }
.pb-rec .pb-zone-h { padding: 0 3pt 8pt; }
.pb-rec-cal { --bg-c: #17283e; --edge: #2f5580; --zone: var(--accent); }
.pb-rec-ship { --bg-c: #282620; --edge: #5c4b30; --zone: var(--warn); }
.pb-rec-bill { --bg-c: #1e2b3f; --edge: #34507a; --zone: var(--accent-ink); }
.pb-rec-rcpt { --bg-c: #172b2c; --edge: #275446; --zone: var(--positive); }
.pb-rec-row { display: flex; align-items: center; gap: 7pt; padding: 5pt 7pt; white-space: nowrap; }
.pb-rec-row b { flex: 1; min-width: 0; font-weight: 400; color: var(--dim); overflow: hidden; text-overflow: ellipsis; }
.pb-rec-row span { font-size: 10pt; color: var(--faintest); font-variant-numeric: tabular-nums; }
.pb-rec-row .pb-chip { --c: var(--faint); }
.pb-rec-row span.amt { font-size: 11pt; font-weight: 500; color: var(--accent-ink); }
.pb-rec-rcpt .pb-rec-row span { font-size: 11pt; font-weight: 500; color: var(--positive); }
/* A package: what is in the box and who sold it, with the status on the
   title's line and the arrival under it only when there is one. */
.pb-ship { background: rgba(233, 238, 245, 0.07); border-radius: 11pt; padding: 9pt; margin-bottom: 6pt;
  display: flex; flex-direction: column; align-items: flex-start; gap: 7pt; }
.pb-ship:last-child { margin-bottom: 2pt; }
.pb-ship-top { align-self: stretch; display: flex; align-items: center; gap: 7pt; white-space: nowrap; }
.pb-ship-top .pb-logo { width: 18pt; height: 18pt; }
.pb-ship-top div { flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 2pt; }
.pb-ship-top b { font-weight: 500; overflow: hidden; text-overflow: ellipsis; }
.pb-ship-top small { font-size: 10pt; font-weight: 500; color: var(--faint); overflow: hidden; text-overflow: ellipsis; }
.pb-ship .pb-chip.eta { --c: var(--faint); }

/* MAIL ARRIVING. The tour no longer lights features: it follows mail in.
   Each email that arrives is a card that drops in over the window, is read,
   and flies to where it belongs, where its row opens up (a grid row going
   from 0fr to 1fr, so the rows below make room) or its count ticks. Scroll
   drives every card; nothing runs on a timer. */
.pb-land { height: 0; overflow: hidden; opacity: 0; }
.pb-land > div { display: flow-root; }
.pb-land[data-arrived] > div > *, .pb-read[data-arrived] { animation: pb-landed 1.4s ease-out; }
@keyframes pb-landed { from { background-color: rgba(78, 155, 234, 0.3); } }
.pb-side span[data-arrived] { animation: pb-filed 1.2s ease-out; }
@keyframes pb-filed { from { color: var(--accent); background: rgba(78, 155, 234, 0.3); } }
[data-count] { transition: color 0.4s ease; }

/* The cards live on the page, not in the app, at a fixed point size so they
   read at the caption's scale whatever the window's size. */
.pb-inmail { --pt: 1.15px; }
@media (max-width: 900px) { .pb-inmail { --pt: 1px; } }
.pb-in { position: absolute; left: 0; top: 0; width: min(290pt, 100%); box-sizing: border-box; padding: 11pt 13pt; border-radius: 14pt;
  font-size: 12pt; line-height: 1.3; color: var(--dim); overflow: hidden;
  background: color-mix(in srgb, rgba(52, 96, 150, 0.95) calc(var(--m, 0) * 100%), rgba(24, 34, 50, 0.97));
  box-shadow: inset 0 0 0 0.75pt rgba(130, 186, 245, 0.4), 0 18pt 44pt -10pt rgba(0, 0, 0, 0.75),
    0 0 40pt -12pt rgba(78, 155, 234, 0.55);
  opacity: 0; visibility: hidden; transform-origin: 50% 50%; will-change: transform, opacity; }
/* In flight the shell becomes the row it is about to form (its box, its
   corners, its padding, tinting toward the blue the row flashes as it
   lands). The avatar and sender stay, which is what a row is; the rest of
   the card fades out of it. */
.pb-in-body { display: flex; flex-direction: column; gap: 6pt; }
.pb-in-body > :not(.pb-in-top), .pb-in-top small { opacity: var(--fade, 1); }
.pb-in-body > .pb-in-read { opacity: calc(var(--r, 0) * var(--fade, 1)); }
.pb-in-body > .pb-in-to { opacity: calc(var(--r, 0) * var(--fade, 1)); }
.pb-in-top { display: flex; align-items: center; gap: 8pt; }
.pb-in-top .pb-logo { width: 22pt; height: 22pt; }
.pb-in-top b { flex: 1; min-width: 0; color: var(--ink); font-weight: 600; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.pb-in-top small { font-size: 10pt; color: var(--faintest); }
.pb-in-subj { color: var(--ink); font-weight: 500; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.pb-in-to { align-self: flex-start; display: flex; align-items: center; gap: 5pt; padding: 2pt 9pt 2pt 7pt;
  border-radius: 99pt; font-size: 10pt; font-weight: 500; color: var(--accent-ink);
  box-shadow: inset 0 0 0 0.75pt rgba(130, 186, 245, 0.55);
  opacity: var(--r, 0); transform: translateY(calc((1 - var(--r, 0)) * 4pt));
  max-height: calc(var(--r, 0) * 30pt); margin-top: calc((var(--r, 0) - 1) * 6pt); overflow: hidden; }
.pb-in-to svg { width: 9pt; height: 9pt; }
/* The first email is read in full: what it asks, by when, what it is about,
   and the level it reaches against the squelch. */
.pb-in-read { max-height: calc(var(--r, 0) * 140pt); opacity: var(--r, 0); overflow: hidden;
  margin-top: calc((var(--r, 0) - 1) * 6pt);
  display: flex; flex-direction: column; gap: 9pt; }
.pb-in-read dl { display: grid; grid-template-columns: auto minmax(0, 1fr); gap: 4pt 12pt; margin: 6pt 0 0;
  padding-top: 9pt; border-top: 0.75pt solid var(--hair-strong); }
.pb-in-read dt { font: 500 9.5pt/1.5 var(--mono); letter-spacing: 0.08em; text-transform: uppercase; color: var(--faintest); }
.pb-in-read dd { margin: 0; color: var(--ink); }
.pb-in-meter { position: relative; height: 5pt; margin-top: 12pt; border-radius: 99pt; background: rgba(233, 238, 245, 0.08); }
.pb-in-meter > i { position: absolute; inset: 0 auto 0 0; width: calc(var(--r, 0) * 84%); border-radius: 99pt;
  background: linear-gradient(90deg, rgba(78, 155, 234, 0.25), var(--accent)); box-shadow: 0 0 12pt -2pt rgba(78, 155, 234, 0.7); }
.pb-in-meter > em { position: absolute; left: 62%; top: -12pt; bottom: -3pt; width: 1px; background: var(--ink); opacity: 0.55; }
.pb-in-meter > em::before { content: "squelch"; position: absolute; bottom: 100%; left: 50%; transform: translateX(-50%);
  padding-bottom: 2pt; font: 500 8.5pt/1 var(--mono); font-style: normal; letter-spacing: 0.1em; text-transform: uppercase; color: var(--faint); }
/* THE MAIL IN THE CAPTIONS. Each caption carries its email in the page,
   under the words, so it scrolls with everything else; the run under the
   last caption is a short stack. Only a card in flight rises above the
   window (the words and the waiting cards still pass under it on a phone),
   so a step dims its words, never
   itself: an opacity on the step would trap its cards beneath the window.
   The room under the list lets the last card reach the middle of the screen
   and land before the window lets go. */
.pb-mailpair .pb-steps { position: relative; padding-bottom: 12svh; }
.pb-mailpair .pb-step { min-height: 64svh; opacity: 1; justify-content: flex-start; padding-top: 10svh; }
.pb-mailpair .pb-step > :not(.pb-inmail) { opacity: 0.26; transition: opacity 0.4s ease; }
.pb-mailpair .pb-step[data-on] > * { opacity: 1; }
.pb-mailpair .pb-step:last-child { min-height: 0; }
.pb-inmail { margin-top: 1.4rem; }
.pb-inrun { display: flex; flex-direction: column; gap: 14px; }
.pb-inslot { position: relative; }
/* THE TALLY. A readout in the trust row's instrument voice: one line per
   zone, dim until its email lands, the one that needed you in blue. */
.pb-tally { margin: 0; max-width: 22rem; border: 1px solid rgba(130, 186, 245, 0.2); border-radius: 12px;
  overflow: hidden; font-size: 0.9rem; }
.pb-tally > div { display: flex; align-items: baseline; justify-content: space-between; gap: 1rem;
  padding: 0.6rem 0.9rem; border-top: 1px solid rgba(130, 186, 245, 0.1); transition: background 0.4s ease; }
.pb-tally > div:first-child { border-top: 0; }
.pb-tally dt { font: 500 10.5px/1.6 var(--mono); letter-spacing: 0.12em; text-transform: uppercase; color: var(--faintest);
  transition: color 0.4s ease; }
.pb-tally dd { margin: 0; color: var(--faintest); transition: color 0.4s ease; }
.pb-tally .pb-tally-h dd { font: 500 10.5px/1.6 var(--mono); letter-spacing: 0.08em; color: var(--accent-ink); }
.pb-tally > div[data-arrived] dt { color: var(--faint); }
.pb-tally > div[data-arrived] dd { color: var(--ink); }
.pb-tally > div.you[data-arrived] { background: rgba(78, 155, 234, 0.12); }
.pb-tally > div.you[data-arrived] dt { color: var(--accent-ink); }
.pb-steps .pb-in { position: relative; left: auto; top: auto; visibility: visible; }
.pb-steps .pb-in[data-flying] { z-index: 5; }
@media (max-width: 900px) {
  .pb-mailpair .pb-step { min-height: 56svh; }
}
@media (prefers-reduced-motion: reduce) {
  .pb-in { display: none; }
  .pb-land[data-arrived] > div > *, .pb-read[data-arrived], .pb-side span[data-arrived] { animation: none; }
}

/* No phone layout, on purpose: a phone gets the same window at the same
   aspect ratio, just smaller. An accurate picture of the app beats a legible
   one of a layout the app never draws. */

/* THE TRUST ROW. Three readouts on one instrument strip, the same shape as
   /trust's spec strip: a channel label, the value in serif, one line under
   it. No icons: the values carry themselves. Even borders all round, and the
   analyzer's graticule faint behind, so it reads as part of the scope. */
.pb-facts { position: relative; display: grid; grid-template-columns: repeat(3, minmax(0, 1fr));
  border: 1px solid rgba(130, 186, 245, 0.2); border-radius: 14px; overflow: hidden;
  background: rgba(9, 13, 22, 0.7); }
.pb-facts::before { content: ""; position: absolute; inset: 0; pointer-events: none;
  background-image:
    repeating-linear-gradient(90deg, rgba(130, 186, 245, 0.06) 0 1px, transparent 1px 5%),
    repeating-linear-gradient(180deg, rgba(130, 186, 245, 0.06) 0 1px, transparent 1px 25%);
  -webkit-mask-image: radial-gradient(ellipse 60% 120% at 50% 0%, #000 10%, transparent 75%);
          mask-image: radial-gradient(ellipse 60% 120% at 50% 0%, #000 10%, transparent 75%); }
.pb-fact { position: relative; padding: clamp(1.4rem, 3vw, 2rem) clamp(1.2rem, 2.6vw, 1.8rem);
  border-left: 1px solid rgba(130, 186, 245, 0.1); }
.pb-fact:first-child { border-left: 0; }
.pb-fact dt { display: flex; align-items: center; gap: 0.55rem; font: 500 10.5px/1 var(--mono);
  letter-spacing: 0.12em; text-transform: uppercase; color: var(--faintest); }
.pb-fact dt b { color: var(--accent-ink); font-weight: 600; }
.pb-fact dd { margin: 0; }
.pb-fact-val { margin: 1rem 0 0.7rem; font-family: var(--serif); font-weight: 500;
  font-size: clamp(1.9rem, 3.2vw, 2.5rem); line-height: 1; letter-spacing: -0.01em; color: var(--ink); }
.pb-fact p { margin: 0; color: var(--dim); font-size: 0.93rem; line-height: 1.6; max-width: 22rem; }
.pb-fact a { color: var(--accent-ink); text-decoration: none; }
.pb-fact a:hover { text-decoration: underline; }
@media (max-width: 760px) {
  .pb-facts { grid-template-columns: minmax(0, 1fr); }
  .pb-fact { border-left: 0; border-top: 1px solid rgba(130, 186, 245, 0.1); }
  .pb-fact:first-child { border-top: 0; }
}

/* THE FORM AGAIN, straight after the tour, for whoever is convinced by now. */
.pb-join { display: flex; flex-direction: column; align-items: center; text-align: center; gap: 1.1rem;
  padding-top: clamp(2rem, 6vw, 4rem); }
.pb-join .pb-h2, .pb-join .pb-intro { margin: 0; }
.pb-join .pb-rig { text-align: left; margin-top: 0.4rem; }
.pb-join .pb-confirm, .pb-join .pb-status { text-align: center; }
/* THE ARGUMENT: a claim on the left, the reasoning on the right. */
.pb-layer { display: grid; grid-template-columns: minmax(0, 0.9fr) minmax(0, 1.1fr);
  gap: clamp(1.5rem, 5vw, 4.5rem); align-items: start; }
.pb-layer .pb-h2 { margin: 0; }
.pb-layer-body { display: flex; flex-direction: column; gap: 1.1rem; }
.pb-layer-body p { margin: 0; color: var(--dim); font-size: 1.05rem; line-height: 1.65; }
.pb-layer-body p:first-child { color: var(--ink); }
@media (max-width: 820px) { .pb-layer { grid-template-columns: minmax(0, 1fr); } }

.pb-close { text-align: center; display: flex; flex-direction: column; align-items: center; gap: 1.4rem; }
.pb-close .pb-h2 { margin: 0; }
/* No meter behind this one, so no clearance for it under the label. */
.pb-close .pb-cta { padding: 0.9rem 1.9rem; }
/* FOOTER: a faded mark, then three link columns, then the copyright line. */
.pb-foot { padding: clamp(3rem, 7vw, 4.5rem) var(--gutter) 2.25rem; border-top: 1px solid var(--hair); }
.pb-foot-grid, .pb-foot-legal { max-width: 84rem; margin-inline: auto; }
.pb-foot-grid { display: grid; grid-template-columns: repeat(4, minmax(0, 1fr)); gap: 3rem; }
.pb-foot-mark { width: fit-content; align-self: start; opacity: 0.35; filter: grayscale(1); transition: opacity 0.2s; }
.pb-foot-mark:hover { opacity: 0.6; }
.pb-foot-mark img { display: block; width: 3.5rem; height: auto; }
.pb-foot nav { display: flex; flex-direction: column; align-items: flex-start; gap: 0.75rem; }
.pb-foot nav p { margin: 0 0 0.75rem; color: var(--faintest); font-size: 0.95rem; }
.pb-foot nav a { color: var(--dim); font-size: 0.95rem; }
.pb-foot-legal { margin-top: clamp(3rem, 7vw, 4.5rem); margin-bottom: 0; color: var(--faintest); font-size: 0.85rem; }
@media (max-width: 820px) { .pb-foot-grid { grid-template-columns: repeat(2, minmax(0, 1fr)); } }
@media (max-width: 420px) { .pb-foot-grid { grid-template-columns: minmax(0, 1fr); gap: 2.25rem; } }

@media (prefers-reduced-motion: reduce) {
  .pb-cue { transition-duration: 0.01ms; }
  .pb-step, .pb-dash-hero, .pb-zone, .pb-rec { transition: none; }
  .pb-slot .pb-rig { animation: none; }
  .pb-cue::after { animation: none; }
}
`;

// The mock's "Npt" lengths are the app's points: rewritten to multiples of
// --pt, which scales the whole mock with its window (see THE AFTER above).
// The sign rides inside the calc: "-7pt" has to become calc(-7 * ...), since a
// minus in front of a calc() is not a length.
const PAGE_CSS_RESOLVED = PAGE_CSS.replace(
  /(-?\d*\.?\d+)pt\b/g,
  "calc($1 * var(--pt))",
);

function FakeInbox() {
  // One randomized batch of rows, rendered twice so the scroll can wrap
  // seamlessly: when the offset passes one copy's height it resets mod that
  // height and the second copy is pixel-identical to where the first began.
  // The mail that mattered is dealt in every dozen rows or so, unmarked.
  const rows = useMemo(
    () =>
      Array.from({ length: 60 }, (_, i) => {
        const [sender, subject, snippet] =
          i % 13 === 5
            ? BURIED[Math.floor(i / 13) % BURIED.length]
            : FAKE_EMAILS[Math.floor(Math.random() * FAKE_EMAILS.length)];
        const h = Math.floor(Math.random() * 12) + 1;
        const m = String(Math.floor(Math.random() * 60)).padStart(2, "0");
        return { sender, subject, snippet, time: `${h}:${m}` };
      }),
    [],
  );

  const scrollRef = useRef<HTMLDivElement>(null);

  // Fake doomscroll: a flick of random distance and speed, a random pause,
  // repeat forever. rAF drives each flick; timeouts space them out. It only
  // runs while the panel is on screen.
  useEffect(() => {
    if (matchMedia("(prefers-reduced-motion: reduce)").matches) return;
    const el = scrollRef.current;
    if (!el) return;
    let offset = 0;
    let raf = 0;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let running = false;
    const easeOut = (t: number) => 1 - Math.pow(1 - t, 3);

    const flick = () => {
      if (!running) return;
      const distance = 40 + Math.random() * 280;
      const duration = 800 + Math.random() * 1400;
      const from = offset;
      const start = performance.now();
      const frame = (now: number) => {
        if (!running) return;
        const t = Math.min(1, (now - start) / duration);
        offset = from + distance * easeOut(t);
        const wrap = el.scrollHeight / 2 || 1;
        el.style.transform = `translateY(-${offset % wrap}px)`;
        if (t < 1) raf = requestAnimationFrame(frame);
        else timer = setTimeout(flick, 150 + Math.random() * 1100);
      };
      raf = requestAnimationFrame(frame);
    };

    const io = new IntersectionObserver(([entry]) => {
      if (entry.isIntersecting && !running) {
        running = true;
        timer = setTimeout(flick, 300);
      } else if (!entry.isIntersecting && running) {
        running = false;
        cancelAnimationFrame(raf);
        clearTimeout(timer);
      }
    });
    io.observe(el.parentElement!);
    return () => {
      running = false;
      io.disconnect();
      cancelAnimationFrame(raf);
      clearTimeout(timer);
    };
  }, []);

  return (
    <div className="pb-inbox" aria-hidden="true">
      <div ref={scrollRef} style={{ willChange: "transform" }}>
        {[...rows, ...rows].map(({ sender, subject, snippet, time }, i) => (
          <div key={i} className="pb-row">
            <span className="pb-row-sender">{sender}</span>
            <span className="pb-row-text">
              <span>{subject}</span> {snippet}
            </span>
            <span className="pb-row-time">{time}</span>
          </div>
        ))}
      </div>
    </div>
  );
}

// Passband blue, the app's own dark-mode accent (Palette.accent, 4E9BEA).
// The rig is the one lit instrument on a page that is otherwise the scene.
const ACCENT = "78, 155, 234";

// The passband's half-width when the filter is fully open. Everything narrower
// is this times the opening, which is what keeps the hump one shape instead of
// a stretching blob. Module scope because the pointer handler sizes its travel
// limit from it and the draw loop sizes the curve from it.
const FULL_W = 0.19;

const CTA_CSS = `
.pb-cta {
  position: relative;
  isolation: isolate;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  gap: 0.6rem;
  margin-top: 0.65rem;
  padding: 0.9rem 1.9rem 1.3rem;
  border-radius: 0.9rem;
  overflow: hidden;
  text-decoration: none;
  font-family: inherit;
  font-size: 0.95rem;
  font-weight: 600;
  letter-spacing: 0.015em;
  cursor: pointer;
  appearance: none;
  color: #dfe8f3;
  background: linear-gradient(180deg, #152032, #0c121c);
  border: 1px solid rgba(${ACCENT}, 0.22);
  box-shadow:
    inset 0 1px 0 rgba(255, 255, 255, 0.07),
    0 12px 30px rgba(0, 0, 0, 0.5);
  transition:
    transform 0.4s cubic-bezier(0.2, 0.8, 0.2, 1),
    border-color 0.4s ease,
    box-shadow 0.4s ease,
    color 0.4s ease;
}
.pb-cta:hover,
.pb-cta:focus-visible {
  color: #f4f8fd;
  transform: translateY(-2px);
  border-color: rgba(${ACCENT}, 0.62);
  box-shadow:
    inset 0 1px 0 rgba(255, 255, 255, 0.14),
    0 0 34px -6px rgba(${ACCENT}, 0.45),
    0 16px 38px rgba(0, 0, 0, 0.55);
}
.pb-cta:active { transform: translateY(0) scale(0.995); }
.pb-cta:focus-visible {
  outline: 2px solid rgba(${ACCENT}, 0.75);
  outline-offset: 3px;
}
/* Machined top edge: a filament that comes up with the rest of the hardware. */
.pb-cta::before {
  content: "";
  position: absolute;
  inset: 0 0 auto;
  height: 1px;
  z-index: 2;
  opacity: 0.45;
  background: linear-gradient(90deg, transparent, rgba(${ACCENT}, 0.8), transparent);
  transition: opacity 0.4s ease;
}
.pb-cta:hover::before,
.pb-cta:focus-visible::before { opacity: 1; }
.pb-cta-meter {
  position: absolute;
  inset: 0;
  z-index: 0;
  width: 100%;
  height: 100%;
  /* Ground, never a target. Inside the rig it lies across the address field,
     and a canvas that swallowed the click would leave the field unfocusable. */
  pointer-events: none;
}
.pb-cta-arrow,
.pb-cta-label { position: relative; z-index: 1; }
.pb-cta-arrow { transition: transform 0.4s cubic-bezier(0.2, 0.8, 0.2, 1); }
.pb-cta:hover .pb-cta-arrow,
.pb-cta:focus-visible .pb-cta-arrow { transform: translateX(3px); }
/* THE RIG: the email field and the join button as ONE piece of machined
   hardware rather than a form on a card.
   The card that used to be here was a translucent rounded rectangle with a
   hairline white border, which is the single most templated component on the
   web and belonged to no part of this page. The page has exactly one material
   for controls: a dark machined ground edged in passband blue. So the
   waitlist is built out of THAT, with slots cut in it. The rig owns the material and the enclosure; the button inside it
   keeps only its meter and its label. */
.pb-rig {
  position: relative;
  isolation: isolate;
  /* A COLUMN, because there are two things to give now and three controls will
     not fit across 28rem: at that width the address slot ends up too narrow to
     show a whole address, and on a phone it clips mid-domain. So the rig
     grows downward instead of squeezing sideways, and stays ONE piece of
     hardware with two slots cut in it rather than becoming two controls. */
  display: flex;
  flex-direction: column;
  width: min(28rem, 100%);
  margin-top: 0.65rem;
  border-radius: 0.9rem;
  overflow: hidden;
  background: linear-gradient(180deg, #152032, #0c121c);
  border: 1px solid rgba(${ACCENT}, 0.22);
  box-shadow:
    inset 0 1px 0 rgba(255, 255, 255, 0.07),
    0 12px 30px rgba(0, 0, 0, 0.5);
  transition: border-color 0.4s ease, box-shadow 0.4s ease;
}
/* One lit state for the whole instrument, driven by the field inside it:
   focusing the input is the same event as arming the button, so lighting only
   the half under the cursor would say the two are separate things. */
.pb-rig:hover,
.pb-rig:focus-within {
  border-color: rgba(${ACCENT}, 0.62);
  box-shadow:
    inset 0 1px 0 rgba(255, 255, 255, 0.14),
    0 0 34px -6px rgba(${ACCENT}, 0.45),
    0 16px 38px rgba(0, 0, 0, 0.55);
}
/* The same machined top edge the standalone button wears. */
.pb-rig::before {
  content: "";
  position: absolute;
  inset: 0 0 auto;
  height: 1px;
  z-index: 2;
  opacity: 0.45;
  background: linear-gradient(90deg, transparent, rgba(${ACCENT}, 0.8), transparent);
  transition: opacity 0.4s ease;
}
.pb-rig:hover::before,
.pb-rig:focus-within::before { opacity: 1; }
/* The bottom slot's row. It carries the meter, so the instrument's floor stays
   exactly the height it was rather than growing with the rig: the bars are
   drawn behind the address and the button, which is the half where the action
   is, and the name sits above the whole display. */
.pb-rig-row {
  position: relative;
  display: flex;
  align-items: stretch;
}
/* The slot. No border and no ground of its own: it is an opening in the rig,
   not a control sitting on one. Its padding matches the button's exactly
   (the in-rig button is moved down to the caption's room), so the address
   and the label sit on the same optical line. */
.pb-rig-field {
  position: relative;
  z-index: 1;
  flex: 1 1 auto;
  min-width: 0;
  width: 100%;
  box-sizing: border-box;
  padding: 1.25rem 1.1rem 1.3rem;
  border: 0;
  outline: none;
  background: none;
  color: #E9EEF5;
  font-family: inherit;
  font-size: 0.95rem;
  line-height: 1.2;
}
.pb-rig-field::placeholder { color: #62707F; }
.pb-rig-field:disabled { color: #808E9F; }
/* THE SLOT NAMES ITSELF, and keeps naming itself once it is in use. The name
   sits where the text will go while the slot is empty and nobody is in it,
   then rides up into the slot's top edge, small caps in the accent, the
   moment the slot is focused or holds anything, so a half-filled form still
   says which opening is which. The placeholder is the example, shown only
   while typing, because the cap is in its place the rest of the time. */
.pb-field {
  position: relative;
  z-index: 1;
  display: flex;
  flex: 1 1 auto;
  min-width: 0;
  cursor: text;
}
.pb-field-cap {
  position: absolute;
  left: 1.1rem;
  top: 1.25rem;
  z-index: 2;
  pointer-events: none;
  color: #62707F;
  font-size: 0.95rem;
  line-height: 1.2;
  transition: top 0.25s cubic-bezier(0.2, 0.8, 0.2, 1), font-size 0.25s cubic-bezier(0.2, 0.8, 0.2, 1),
    letter-spacing 0.25s ease, color 0.25s ease;
}
.pb-field-name { flex: none; }
.pb-field-name .pb-field-cap { top: 1.2rem; }
.pb-rig-field:not(:focus)::placeholder { color: transparent; }
.pb-rig-field:is(:focus, :not(:placeholder-shown), :-webkit-autofill) + .pb-field-cap {
  top: 0.5rem;
  font-size: 0.62rem;
  font-weight: 600;
  letter-spacing: 0.14em;
  text-transform: uppercase;
}
/* Brighter once small: the placeholder grey is fine at full size and too
   faint at ten-pixel caps. */
.pb-rig-field:is(:not(:placeholder-shown), :-webkit-autofill) + .pb-field-cap { color: #8592A3; }
.pb-rig-field:focus + .pb-field-cap { color: rgba(${ACCENT}, 0.95); }
@media (prefers-reduced-motion: reduce) {
  .pb-field-cap { transition: none; }
}
/* Chrome recognises name beside email as an address profile and paints its
   own opaque ground into both slots, which on a rig whose whole premise is that
   the slots have no ground of their own is the one thing that breaks the
   material. The inset shadow is the documented way to overrule it. */
.pb-rig-field:-webkit-autofill,
.pb-rig-field:-webkit-autofill:hover,
.pb-rig-field:-webkit-autofill:focus {
  -webkit-text-fill-color: #E9EEF5;
  caret-color: #E9EEF5;
  box-shadow: inset 0 0 0 100vw #101826;
  transition: background-color 9999s;
}
/* The name slot. Divided off from the row below by a hairline, so the two
   openings read as machined out of one face rather than as one box with two
   bits of text floating in it. Its own padding, because the address slot's
   asymmetric bottom is clearance for the meter and there is no meter behind
   this one. */
.pb-rig-name {
  flex: none;
  padding: 1.2rem 1.1rem 0.8rem;
  /* Brighter than the button's own divider, and it has to be: that one runs
     down the lit half of the instrument, with the meter's glow behind it,
     while this one crosses dark ground where the same 0.18 vanishes. */
  border-bottom: 1px solid rgba(${ACCENT}, 0.3);
}
/* The button, once the rig owns the material: no ground, no shell, no lift.
   What is left of it is the half that lights up, divided off by one hairline.
   The lift is dropped deliberately, because a button that rises out of the bar
   it is set into reads as a part coming loose. */
.pb-cta-in-rig {
  flex: none;
  margin-top: 0;
  /* Down by what the address slot gave its caption, so "join" stays on the
     address's line. */
  padding-top: 1.25rem;
  border: 0;
  border-left: 1px solid rgba(${ACCENT}, 0.18);
  border-radius: 0;
  background: none;
  box-shadow: none;
}
.pb-cta-in-rig::before { display: none; }
.pb-cta-in-rig:hover,
.pb-cta-in-rig:focus-visible {
  transform: none;
  box-shadow: none;
  border-color: rgba(${ACCENT}, 0.35);
}
/* Drawn inside, because the rig clips anything outside it. */
.pb-cta-in-rig:focus-visible { outline-offset: -3px; }
/* In flight. The meter keeps running (the request is the thing being waited
   on) but the hardware stops answering the pointer. */
.pb-cta[disabled] { cursor: progress; color: #808E9F; }
.pb-cta[disabled]:hover,
.pb-cta[disabled]:hover .pb-cta-arrow { transform: none; }
.pb-cta[disabled]:hover { border-color: rgba(${ACCENT}, 0.22); box-shadow:
  inset 0 1px 0 rgba(255, 255, 255, 0.07), 0 12px 30px rgba(0, 0, 0, 0.5); }
.pb-cta-in-rig[disabled]:hover { box-shadow: none; border-color: rgba(${ACCENT}, 0.18); }
@media (prefers-reduced-motion: reduce) {
  .pb-cta,
  .pb-cta-arrow { transition-duration: 0.01ms; }
  .pb-cta:hover,
  .pb-cta:focus-visible,
  .pb-cta:hover .pb-cta-arrow,
  .pb-cta:focus-visible .pb-cta-arrow { transform: none; }
}
`;

// The one piece of hardware on the page: the meter behind the waitlist rig. At
// rest it shows a noise floor; on hover the filter closes and only the
// passband survives, lit in passband blue. It is the scene behind it, shrunk to
// the size of a button. The animation is the product's own metaphor, which is the price
// of putting motion here at all.
//
// A HOOK RATHER THAN A COMPONENT because the pointer handlers have to sit on
// the button, not on the canvas inside it: the meter is ground beneath a label,
// and the geometry it reads is the button's own box. So the hook hands back
// both halves, the caller spreads the handlers onto whatever element it is
// building, and the two pages share one meter rather than growing two that
// drift apart.
function useMeter() {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  // Hover lives in a ref, not state: the rAF loop reads it every frame and CSS
  // already owns the chrome, so a re-render would buy nothing.
  const hovered = useRef(false);
  // Where the passband is tuned to and how far it is opened, both 0..1 and both
  // driven by the pointer: x slides the band along the button, y opens it up.
  // Same reasoning as above: a ref, read per frame, never re-rendering.
  const tuned = useRef({ x: 0.5, lift: 1 });

  useEffect(() => {
    const canvas = canvasRef.current;
    const ctx = canvas?.getContext("2d");
    if (!canvas || !ctx) return;
    const reduce = matchMedia("(prefers-reduced-motion: reduce)").matches;

    // Each bar drifts on its own beat so the floor shimmers rather than
    // marching in step. Sized to the widest bar count the button can ask for.
    const POOL = 96;
    const phase = Array.from({ length: POOL }, () => Math.random() * Math.PI * 2);
    const speed = Array.from({ length: POOL }, () => 0.7 + Math.random() * 1.9);

    // The filter's frequency response: flat across the top, steep skirts either
    // side. This curve is the passband the product is named for. `c` is where it
    // is tuned to and `w` how wide it is opened.
    const band = (x: number, c: number, w: number) =>
      Math.exp(-Math.pow(Math.abs(x - c) / w, 4));

    let width = 0;
    let height = 0;
    let gate = 0; // 0 = wide open (noise), 1 = filtered down to the passband
    // The drawn centre and opening, chasing `tuned`. Eased rather than assigned
    // so a fast cursor pulls the band along instead of teleporting it, and so
    // leaving the button glides it home rather than snapping.
    let centre = 0.5;
    // Named `aperture`, not `open`: the bar loop below already has its own
    // local `open` for a bar's unfiltered height, and reading an outer `open`
    // earlier in that same block lands in the temporal dead zone and throws
    // on every frame. Syntax checks do not catch it; the canvas just stays
    // blank.
    let aperture = 1;

    const render = (t: number) => {
      ctx.clearRect(0, 0, width, height);
      // Width tracks height, so raising the cursor grows the hump rather than
      // stretching it: the skirts spread at the same rate the peak climbs and
      // the silhouette stays the same shape at every size.
      const bandW = FULL_W * aperture;
      const bars = Math.max(16, Math.min(POOL, Math.round(width / 5.5)));
      const step = width / bars;
      const barW = Math.max(1.5, step - 2);
      const maxH = height * 0.44;

      // Trace the response curve only once the filter is closing, so it reads
      // as the cause of the collapse rather than as decoration.
      if (gate > 0.01) {
        const top = (x: number) =>
          height - (0.06 + 0.94 * aperture * band(x / width, centre, bandW)) * maxH;
        ctx.beginPath();
        ctx.moveTo(0, height);
        for (let x = 0; x <= width; x += 2) ctx.lineTo(x, top(x));
        ctx.lineTo(width, height);
        const fill = ctx.createLinearGradient(0, height - maxH, 0, height);
        fill.addColorStop(0, `rgba(${ACCENT}, ${0.16 * gate})`);
        fill.addColorStop(1, `rgba(${ACCENT}, 0)`);
        ctx.fillStyle = fill;
        ctx.fill();

        ctx.beginPath();
        ctx.moveTo(0, top(0));
        for (let x = 2; x <= width; x += 2) ctx.lineTo(x, top(x));
        ctx.strokeStyle = `rgba(${ACCENT}, ${0.45 * gate})`;
        ctx.lineWidth = 1;
        ctx.stroke();
      }

      for (let i = 0; i < bars; i++) {
        const x = bars > 1 ? i / (bars - 1) : 0.5;
        const response = aperture * band(x, centre, bandW);
        // Two incommensurate beats per bar: busy, but never repeating.
        const noise =
          0.5 +
          0.5 *
            Math.sin(t * speed[i] + phase[i]) *
            Math.cos(t * speed[i] * 0.61 + phase[i] * 1.7);
        const open = 0.12 + 0.3 * noise;
        const filtered = 0.04 + 0.96 * response * (0.55 + 0.45 * noise);
        const barH = Math.max(1, (open * (1 - gate) + filtered * gate) * maxH);

        // Warmth is gated response: only bars the filter passes light up.
        const warm = gate * response;
        const mix = (cold: number, hot: number) =>
          Math.round(cold + (hot - cold) * warm);
        ctx.fillStyle = `rgba(${mix(128, 130)}, ${mix(142, 186)}, ${mix(159, 245)}, ${0.4 + 0.55 * warm})`;
        ctx.shadowBlur = warm > 0.25 ? 10 * warm : 0;
        ctx.shadowColor = `rgba(${ACCENT}, ${0.7 * warm})`;
        const bx = i * step + (step - barW) / 2;
        ctx.beginPath();
        ctx.roundRect(bx, height - barH, barW, barH, barW / 2);
        ctx.fill();
      }
      ctx.shadowBlur = 0;
    };

    const resize = () => {
      const dpr = Math.min(2, window.devicePixelRatio || 1);
      const rect = canvas.getBoundingClientRect();
      width = rect.width;
      height = rect.height;
      canvas.width = Math.round(width * dpr);
      canvas.height = Math.round(height * dpr);
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
      // Resizing clears the bitmap, and with no loop running nothing would
      // repaint it.
      if (reduce) render(0);
    };

    let raf = 0;
    let last = 0;
    const loop = (now: number) => {
      const dt = last ? Math.min(0.05, (now - last) / 1000) : 0;
      last = now;
      // Exponential approach, so the ease is the same at 60Hz and 120Hz.
      gate += ((hovered.current ? 1 : 0) - gate) * (1 - Math.exp(-dt * 9));
      // Tuning tracks faster than the filter opens: the band should feel
      // attached to the cursor, while the collapse into it stays a beat behind.
      centre += (tuned.current.x - centre) * (1 - Math.exp(-dt * 16));
      aperture += (tuned.current.lift - aperture) * (1 - Math.exp(-dt * 16));
      render(now / 1000);
      raf = requestAnimationFrame(loop);
    };

    resize();
    const observer = new ResizeObserver(resize);
    observer.observe(canvas);
    // Reduced motion still gets the meter, just held on a single frame.
    if (reduce) render(0);
    else raf = requestAnimationFrame(loop);

    return () => {
      cancelAnimationFrame(raf);
      observer.disconnect();
    };
  }, []);

  return {
    // Everything that goes INSIDE the button: the stylesheet, and the canvas
    // the loop above draws on.
    chrome: (
      <>
        {/* href + precedence is React 19's hoist path: the rules land in <head>
            and dedupe by href instead of sitting loose in the body, which is
            also what lets both buttons render this without shipping it twice. */}
        <style href="pb-cta" precedence="default">
          {CTA_CSS}
        </style>
        <canvas ref={canvasRef} className="pb-cta-meter" aria-hidden="true" />
      </>
    ),
    // Everything that goes ON it.
    handlers: {
      onPointerEnter: () => {
        hovered.current = true;
      },
      onPointerMove: (event: PointerEvent<HTMLElement>) => {
        // THE CANVAS'S BOX, NOT THE HANDLER'S. They were the same element's
        // box until the rig grew a second slot above the meter: the handlers
        // stay on the whole instrument (so hovering anywhere tunes it, which is
        // the design) while the canvas covers only the bottom row, and reading
        // the outer box spent half the vertical travel over a slot with no
        // meter behind it — the filter could never open fully anywhere you
        // could actually see it. On the standalone button the two boxes are
        // still the same rectangle, so nothing changes there.
        const box = (canvasRef.current ?? event.currentTarget).getBoundingClientRect();
        const x = (event.clientX - box.left) / (box.width || 1);
        // Screen y grows downward and the hump grows upward, so invert: the
        // top of the button is the filter wide open.
        const lift = 1 - (event.clientY - box.top) / (box.height || 1);
        // Never fully shut: at zero the hump has no height and no width, so
        // the bars all die and the button looks broken rather than tuned. The
        // top of the travel goes past 1, which is the meter's nominal full
        // height, so the peak reaches up behind the label instead of stopping
        // politely beneath it.
        const aperture = 0.38 + 0.8 * Math.min(1, Math.max(0, lift));
        // Clamped by the skirts' real width, not a fixed margin: a wide hump
        // needs more room to keep both shoulders on the button than a narrow
        // one, so the travel opens up exactly as the filter closes down. The
        // quartic is down to a percent of peak by 1.5 half-widths, so 0.72 is
        // where the shoulder has visually landed.
        const edge = 0.72 * FULL_W * aperture;
        tuned.current = {
          x: Math.min(1 - edge, Math.max(edge, x)),
          lift: aperture,
        };
      },
      onPointerLeave: () => {
        hovered.current = false;
        // Home, so the next hover starts centred and open rather than
        // wherever the last one happened to end.
        tuned.current = { x: 0.5, lift: 1 };
      },
      // Keyboard focus has no cursor to follow, so it gets the centred band.
      onFocus: () => {
        hovered.current = true;
      },
      onBlur: () => {
        hovered.current = false;
      },
    },
  };
}

// The arrow the buttons wear. It points the way the press goes. Label first,
// arrow second, so the two read left to right in the order they happen.
function Arrow() {
  return (
    <svg
      className="pb-cta-arrow"
      width="15"
      height="14"
      viewBox="0 0 15 14"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.6"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      <path d="M1.4 7h11.2m0 0L9.3 3.7M12.6 7 9.3 10.3" />
    </svg>
  );
}

// THE HOMEPAGE'S ONE ACTION. It used to be the download, and the client is not
// a thing worth holding before there is a mailbox behind it: the door here is
// the list, and the download waits at the end of the invite flow, where it is
// the next thing somebody actually needs.
//
// STILL AN ANCHOR with a real `href`, even though the click is handled: it is a
// link to a URL that exists, so cmd-click, middle-click, and "copy link" all
// have to keep meaning what they mean.
function SubmitButton({ busy }: { busy: boolean }) {
  return (
    <button className="pb-cta pb-cta-in-rig" type="submit" disabled={busy}>
      <span className="pb-cta-label">{busy ? "sending" : "join"}</span>
      {!busy && <Arrow />}
    </button>
  );
}

// The control plane answers 200 for a fresh address and for one already on the
// list, so this page can never become a membership oracle.
const WAITLIST_URL = "https://signup.passband.app/waitlist";

// The path the waitlist state answers to. A real URL, deep-linkable and
// shareable, even though reaching it from the button never loads a document.
const WAITLIST_PATH = "/waitlist";

// THE HERO'S ONE ACTION, until it is pressed; then the rig takes its slot.
//
// STILL AN ANCHOR with a real `href`, even though the click is handled: it is a
// link to a URL that exists, so cmd-click, middle-click, and "copy link" all
// have to keep meaning what they mean.
function JoinButton({
  onClick,
}: {
  onClick: (event: MouseEvent<HTMLAnchorElement>) => void;
}) {
  const { chrome, handlers } = useMeter();
  return (
    <a className="pb-cta" href={WAITLIST_PATH} onClick={onClick} {...handlers}>
      {chrome}
      <span className="pb-cta-label">join the waitlist</span>
      <Arrow />
    </a>
  );
}

// The waitlist rig, in the slot the button held, under the first headline.
function Waitlist({ focus = true }: { focus?: boolean }) {
  const { chrome, handlers } = useMeter();
  const nameRef = useRef<HTMLInputElement>(null);
  // The button that opened this is gone from under the cursor, so the first
  // field takes the focus it left behind: press join, start typing. Not
  // autoFocus: that scrolls the field into view, which from the closing
  // button would cut the smooth scroll back up short.
  // The copy of the form further down the page was never opened by a press,
  // so it leaves the focus where it is.
  useEffect(() => {
    if (focus) nameRef.current?.focus({ preventScroll: true });
  }, [focus]);
  const [name, setName] = useState("");
  const [email, setEmail] = useState("");
  const [state, setState] = useState<"idle" | "busy" | "done" | "error">("idle");

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (state === "busy") return;
    setState("busy");
    try {
      const res = await fetch(WAITLIST_URL, {
        method: "POST",
        // urlencoded keeps this a CORS simple request: no preflight round trip.
        headers: { "Content-Type": "application/x-www-form-urlencoded" },
        // The name rides along beside the address. The control plane treats it
        // as optional, so an older cached copy of this bundle posting an
        // address alone still joins the list.
        body: new URLSearchParams({ name, email }),
        credentials: "omit",
      });
      setState(res.ok ? "done" : "error");
    } catch {
      setState("error");
    }
  };

  // WHAT ARRIVES AND WHAT IS AT THE END OF IT, which is the only thing still
  // unanswered once somebody is on the list, and where the client lives now.
  if (state === "done") {
    // BY NAME WHEN THERE IS ONE. It is the only thing the form now knows that
    // it did not before, and answering with it is what makes the field read as
    // having been asked rather than collected. First word only: a full name
    // read back at somebody is a receipt, not a greeting.
    //
    // AND ONLY WHEN IT IS THE SIZE OF A NAME. The field takes 128 characters
    // and people put addresses in the wrong box; a greeting is not the place to
    // find out. Anything longer falls back to the line that needs no name,
    // which also keeps this state inside the slot above.
    const word = name.trim().split(/\s+/)[0];
    const first = word.length <= 20 ? word : "";
    return (
      <>
        <p className="pb-confirm">
          {first ? `you're on the list, ${first}.` : "you're on the list."}
        </p>
        <p className="pb-status">
          you will receive an email when a spot opens. it walks you through
          setup, and the app is waiting at the end of it.
        </p>
      </>
    );
  }

  return (
    <>
      <form className="pb-rig" onSubmit={submit} {...handlers}>
        <label className="pb-field pb-field-name">
          <input
            ref={nameRef}
            className="pb-rig-field pb-rig-name"
            type="text"
            name="name"
            required
            autoComplete="name"
            placeholder="Margaret Hamilton"
            aria-label="your name"
            // The control plane's own ceiling is 128 CHARACTERS and this counts
            // UTF-16 units, so the two agree on every name and the browser is
            // the stricter of them on astral ones. Deliberately the stricter
            // side: a name stopped at the field beats one cut on the way in.
            maxLength={128}
            value={name}
            disabled={state === "busy"}
            onChange={(event) => setName(event.target.value)}
          />
          <span className="pb-field-cap" aria-hidden="true">your name</span>
        </label>
        <div className="pb-rig-row">
          {chrome}
          <label className="pb-field">
            <input
              className="pb-rig-field"
              type="email"
              name="email"
              required
              autoComplete="email"
              placeholder="you@example.com"
              aria-label="email address"
              value={email}
              disabled={state === "busy"}
              onChange={(event) => setEmail(event.target.value)}
            />
            <span className="pb-field-cap" aria-hidden="true">email address</span>
          </label>
          <SubmitButton busy={state === "busy"} />
        </div>
      </form>
      {state === "error" && (
        <p className="pb-status pb-status-error">
          that didn't go through. give it a second and try again.
        </p>
      )}
    </>
  );
}


// MARK: - the "after": a fake sitrep

// Stand-ins for the SF Symbols the app draws: gauge, envelope, key, the
// discovery ring, the update arrow, waveform and gear in the side rail; eye, envelope.open, calendar,
// shippingbox, building.columns and receipt on the zones.
// The app's icons are SF Symbols, so the mocks draw the same ones: each is a
// white mask rendered from the system by tools/sf-symbols.swift and filled
// with currentColor, which keeps every colour rule below in charge. Only the
// discovery ring is the app's own drawing, so it stays a path here.
type GlyphName = keyof typeof SF_SYMBOLS | "ring";
const RING = "M8 2.2a5.8 5.8 0 1 1-5 2.9M5.2 2.9l.1 0M3.4 4.3l.1 0";

function Glyph({ name }: { name: GlyphName }) {
  if (name === "ring") {
    return (
      <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.25"
        strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
        <path d={RING} />
      </svg>
    );
  }
  const id = `sf-${name}`;
  return (
    <svg viewBox="0 0 96 96" aria-hidden="true">
      <mask id={id} maskUnits="userSpaceOnUse" x="0" y="0" width="96" height="96">
        <image href={SF_SYMBOLS[name]} width="96" height="96" />
      </mask>
      <rect width="96" height="96" fill="currentColor" mask={`url(#${id})`} />
    </svg>
  );
}

function ZoneHead({ glyph, title, count, sub, land }: {
  glyph: GlyphName; title: string; count: number; sub?: string; land?: string;
}) {
  return (
    <div className="pb-zone-h">
      <Glyph name={glyph} />
      {title} <em data-count={land} data-from={land ? count - 1 : undefined}>{count}</em>
      {sub && <small>{sub}</small>}
    </div>
  );
}

// A row that is not on the board yet: it opens when its email lands in the
// tour (see Showcase), so the board the tour ends on is the board as drawn.
function Land({ id, children }: { id: string; children: ReactNode }) {
  return <div className="pb-land" data-land={id}><div>{children}</div></div>;
}

// The app's rail: Sitrep, Calendar, Mail and Login codes on the digit keys,
// then the discovery ring, a hairline, and the account's own group.
function Rail({ on }: { on: "sitrep" | "calendar" }) {
  return (
    <nav className="pb-side" aria-hidden="true">
      <span className={on === "sitrep" ? "on" : undefined}><Glyph name="sitrep" /></span>
      <span className={on === "calendar" ? "on" : undefined}><Glyph name="calendar" /></span>
      <span data-ping="filed"><Glyph name="mail" /></span>
      <span><Glyph name="key" /></span>
      <i className="pb-side-gap" />
      <span><Glyph name="ring" /></span>
      <hr />
      <span className="up"><Glyph name="update" /></span>
      <span><Glyph name="pulse" /></span>
      <span><Glyph name="gear" /></span>
      <img className="pb-me" src="/sam.jpg" alt="" />
    </nav>
  );
}

// The app's avatar palette, dark halves (Palette.avatarPalette).
const AVATAR = {
  blue: ["#24384F", "#A9CBF0"], green: ["#22392E", "#9AD3B6"], rust: ["#442C26", "#EBA894"],
  violet: ["#2F2A4A", "#B8ACF2"], ochre: ["#3E2E1B", "#E2C078"], rose: ["#3E2839", "#E0A6CD"],
} as const;

// The mail from the "before" panel's scroll, as the sitrep has it: a name, the
// model's one-line abstraction of what the thread wants, and a chip only where
// there is a date. Overdue is the filled red chip plus the bar at the row's
// edge, exactly as SitrepView draws it; upcoming is an amber outline.
const EYES: Array<{
  who: string; initials: string; tone: keyof typeof AVATAR; line: string;
  chip?: string; overdue?: boolean; clip?: boolean; land?: string;
}> = [
  { who: "Parkline Properties", initials: "PP", tone: "ochre", line: "Your lease renewal still needs a signature before the new rate lapses.", chip: "3d PAST DUE", overdue: true, clip: true },
  { who: "Dr. Ortiz's Office", initials: "DO", tone: "green", line: "Confirm Thursday's 2:30 appointment or call to reschedule.", chip: "due today" },
  { who: "Jamie Chen", initials: "JC", tone: "blue", line: "Needs your pick between two offsite venues so they can hold the date.", chip: "due Fri", land: "jamie" },
  { who: "Alex Rivera", initials: "AR", tone: "violet", line: "Replied to your offsite dates: the 14th works for everyone." },
  { who: "Mom", initials: "M", tone: "rose", line: "Sent photos from the weekend." },
];

// Recurring senders from the scroll, one card per sender: the ones worth
// sitting down with first (the app tints those like paper), the offers after.
const READING: Array<[brand: Brand, sender: string, count: number, blurb: string, content?: boolean]> = [
  ["cats", "Cats Weekly", 4, "Why your cat stares at the wall, and nine boxes ranked by sit quality.", true],
  ["federal", "The Federal Overstatement", 3, "Markets react calmly to everything. Plus the case against breakfast.", true],
  ["haight", "Haightssion", 2, "This month's programme: three talks, a walk and a pottery class.", true],
  ["brightly", "Brightly", 9, "48 hours only: 40% off everything you looked at once."],
  ["rainforest", "Rainforest", 5, "Deals picked for you, based on one search in 2023."],
  ["skyward", "Skyward Miles", 2, "1,204 miles expire at the end of the month."],
];

// MARK: - fake brands

// The practice inbox's own invented senders (RehearsalNewsletterLogo.swift),
// drawn from the same geometry, plus one airline that exists only here.
// Nobody's real logo is on this page.
type Brand = "cats" | "federal" | "haight" | "brightly" | "rainforest" | "exfed" | "skyward";

const poly = (points: Array<[number, number]>) =>
  points.map(([x, y]) => `${12 + x * 0.76},${12 + y * 0.76}`).join(" ");

const BRIGHTLY_STAR = poly([
  [50, 0], [58, 20], [75, 7], [73, 29], [93, 25], [80, 43], [100, 50], [80, 58], [93, 75], [71, 73], [75, 93], [57, 80],
  [50, 100], [42, 80], [25, 93], [27, 71], [7, 75], [20, 57], [0, 50], [20, 42], [7, 25], [29, 27], [25, 7], [43, 20],
]);
const HAIGHT_PIECES = [
  [[8, 8], [62, 8], [42, 28], [30, 28], [30, 70], [8, 92]],
  [[92, 8], [92, 92], [38, 92], [58, 72], [70, 72], [70, 30]],
  [[30, 49], [49, 30], [70, 30], [70, 51], [51, 70], [30, 70]],
].map((piece) => poly(piece as Array<[number, number]>));

function Logo({ brand }: { brand: Brand }) {
  const marks: Record<Brand, ReactNode> = {
    cats: (
      <>
        <rect width="100" height="100" fill="#F5EFDF" />
        <path fill="#293B30" d="M32 20l9 13q9-4 18 0l9-13 2 25q0 17-20 17T30 45Z" />
        <text x="51.5" y="87" textAnchor="middle" fill="#293B30" fontFamily="Newsreader, Georgia, serif"
          fontWeight="700" fontSize="21" letterSpacing="3">CW</text>
      </>
    ),
    federal: (
      <>
        <rect width="100" height="100" fill="#FFFDF6" />
        <path fill="#181818" d="M13 16h74v2.5H13zM13 78h74v2.5H13zM13 83h74v1.5H13z" />
        <text x="50" y="65" textAnchor="middle" fill="#181818" fontFamily="Newsreader, Georgia, serif"
          fontWeight="700" fontSize="46" letterSpacing="-2">FO</text>
      </>
    ),
    haight: (
      <>
        <rect width="100" height="100" fill="#171717" />
        {HAIGHT_PIECES.map((points) => <polygon key={points} points={points} fill="#D9FF43" />)}
      </>
    ),
    brightly: (
      <>
        <rect width="100" height="100" fill="#171717" />
        <polygon points={BRIGHTLY_STAR} fill="#FFE500" />
      </>
    ),
    rainforest: (
      <>
        <rect width="100" height="100" fill="#232F3E" />
        <text x="50" y="64" textAnchor="middle" fill="#fff" fontFamily="ui-rounded, system-ui, sans-serif"
          fontWeight="700" fontSize="78">r</text>
        <path d="M20 73Q50 95 80 69M65 69l16-2-4 15" fill="none" stroke="#FF9900" strokeWidth="6.5"
          strokeLinecap="round" strokeLinejoin="round" />
      </>
    ),
    exfed: (
      <>
        <rect width="100" height="100" fill="#fff" />
        <text x="50" y="47" textAnchor="middle" fill="#51258A" fontFamily="system-ui, sans-serif"
          fontWeight="900" fontSize="42" letterSpacing="-3.5">Ex</text>
        <text x="50" y="84" textAnchor="middle" fill="#F46A20" fontFamily="system-ui, sans-serif"
          fontWeight="900" fontSize="42" letterSpacing="-3.5">Fed</text>
      </>
    ),
    skyward: (
      <>
        <rect width="100" height="100" fill="#0F2747" />
        <path fill="#8FC1FF" d="M14 66Q40 30 88 24Q62 40 52 70Q36 58 14 66Z" />
        <path fill="#4E9BEA" d="M30 80Q52 62 80 60Q64 70 58 84Q46 76 30 80Z" />
      </>
    ),
  };
  return (
    <svg className="pb-logo" viewBox="0 0 100 100" aria-hidden="true">{marks[brand]}</svg>
  );
}

// MARK: - the tour

// What the window shows off as the page scrolls past it, in the order the
// captions arrive. `key` is what the stylesheet switches the picture on.
function AppMock() {
  return (
    <div
      className="pb-app"
      role="img"
      aria-label="The Passband sitrep. A headline reads: the offsite has a date and your keyboard lands today. Five items are for your eyes, led by a lease renewal three days past due and an appointment to confirm today. Newsletters wait on a reading shelf, articles ahead of promotions. A rail beside it holds the calendar, packages by name, billing and receipts."
    >
      <div className="pb-app-bar">
        <div className="pb-lights" aria-hidden="true"><i /><i /><i /></div>
        <div className="pb-app-title">
          <b>passband</b>
          <small>SITREP</small>
        </div>
        <div className="pb-app-tools">
          <span><Glyph name="compose" /></span>
          <span><Glyph name="search" /></span>
        </div>
      </div>

      <Rail on="sitrep" />

      <div className="pb-page">
        <div className="pb-dash-hero">
          <small>Good morning, Sam</small>
          <b>The offsite has a date and your keyboard lands today.</b>
        </div>

        <div className="pb-col">
          <section className="pb-zone pb-zone-eyes">
            <div className="pb-zone-h">
              <Glyph name="eye" />
              For your eyes <em data-count="jamie" data-from={EYES.length - 1}>{EYES.length}</em>
              <span className="pb-flow">
                <Glyph name="wind" />
                flow mode
              </span>
            </div>
            {EYES.map(({ who, initials, tone, line, chip, overdue, clip, land }, i) => {
              const [bg, fg] = AVATAR[tone];
              const row = (
                <div key={who} className={`pb-eye${overdue ? " overdue" : ""}${i === 0 ? " cursor" : ""}`}
                  style={{ ["--i" as string]: i }}>
                  <span className="pb-avatar" style={{ background: bg, color: fg }}>{initials}</span>
                  <b>{who}</b>
                  {clip && <Glyph name="clip" />}
                  <span className="line">{line}</span>
                  {chip && (
                    <span
                      className={`pb-chip${overdue ? " filled" : ""}`}
                      style={{ ["--c" as string]: overdue ? "var(--danger)" : "var(--warn)" }}
                    >
                      {chip}
                    </span>
                  )}
                </div>
              );
              return land ? <Land key={who} id={land}>{row}</Land> : row;
            })}
          </section>

          <section className="pb-zone pb-zone-reading">
            <div className="pb-zone-h">
              <Glyph name="reading" />
              Reading <em>{READING.length}</em>
              <small>newsletters, announcements, and offers</small>
            </div>
            <div className="pb-reading">
              {READING.map(([brand, sender, count, blurb, content]) => (
                <div key={sender} className={`pb-read${content ? " content" : ""}`}
                  data-ping={sender === "Cats Weekly" ? "cats" : undefined}>
                  <Logo brand={brand} />
                  <div className="pb-read-text">
                    <div className="pb-read-top">
                      <b>{sender}</b>
                      {sender === "Cats Weekly"
                        ? <span data-count="cats" data-from={count - 1} data-suffix=" emails">{count} emails</span>
                        : <span>{count} emails</span>}
                    </div>
                    <p>{blurb}</p>
                  </div>
                </div>
              ))}
            </div>
          </section>
        </div>

        <aside className="pb-rail">
          <section className="pb-rec pb-rec-cal">
            <ZoneHead glyph="calendar" title="Calendar" count={2} land="cal" />
            <div className="pb-rec-row"><b>Dr. Ortiz · checkup</b><span>Thu 2:30 PM</span></div>
            <Land id="cal">
              <div className="pb-rec-row"><b>Team offsite</b><span className="pb-chip">updated</span><span>Oct 14</span></div>
            </Land>
          </section>
          <section className="pb-rec pb-rec-ship">
            <ZoneHead glyph="box" title="Shipments" count={2} land="ship" />
            <div className="pb-ship">
              <div className="pb-ship-top">
                <Logo brand="exfed" />
                <div><b>Keychron Q1 Max</b><small>Keychron · order KC-48211</small></div>
                <span className="pb-chip filled" style={{ ["--c" as string]: "var(--warn)" }}>out for delivery</span>
              </div>
            </div>
            <Land id="ship">
              <div className="pb-ship">
                <div className="pb-ship-top">
                  <Logo brand="rainforest" />
                  <div><b>Linen notebooks, set of 3</b><small>Rainforest · 2 items</small></div>
                  <span className="pb-chip" style={{ ["--c" as string]: "var(--accent)" }}>shipped</span>
                </div>
                <span className="pb-chip eta">arrives Sat</span>
              </div>
            </Land>
          </section>
          <section className="pb-rec pb-rec-bill">
            <ZoneHead glyph="bank" title="Billing" count={2} />
            <div className="pb-rec-row"><b>Con Edison</b><span className="pb-chip">autopay</span><span className="amt">$142.08</span></div>
            <div className="pb-rec-row"><b>Chase ··4417</b><span className="pb-chip">statement</span></div>
          </section>
          <section className="pb-rec pb-rec-rcpt">
            <ZoneHead glyph="receipt" title="Receipts" count={3} land="rcpt" />
            <Land id="rcpt">
              <div className="pb-rec-row"><b>Blue Bottle</b><span>$6.50</span></div>
            </Land>
            <div className="pb-rec-row"><b>DoorDash</b><span>$23.18</span></div>
            <div className="pb-rec-row"><b>Berghain Klubnacht</b><span>€25.00</span></div>
          </section>
        </aside>
      </div>

    </div>
  );
}

// THE BEFORE, THE AFTER, AND THE TOUR. The step whose caption is level with
// the reading line (mid-screen, or lower on a phone where the window holds
// the top) is the one the window shows. Before the first and after the last
// the window is just the sitrep.
function useTourFocus() {
  const stepsRef = useRef<HTMLOListElement>(null);
  const [focus, setFocus] = useState(-1);

  useEffect(() => {
    const list = stepsRef.current;
    if (!list) return;
    let raf = 0;
    const measure = () => {
      raf = 0;
      const line = innerHeight * (innerWidth > 900 ? 0.5 : 0.54);
      let at = -1;
      Array.from(list.children).forEach((step, i) => {
        const rect = step.getBoundingClientRect();
        if (rect.top <= line && rect.bottom > line) at = i;
      });
      setFocus(at);
    };
    const queue = () => {
      if (!raf) raf = requestAnimationFrame(measure);
    };
    measure();
    addEventListener("scroll", queue, { passive: true });
    addEventListener("resize", queue);
    return () => {
      cancelAnimationFrame(raf);
      removeEventListener("scroll", queue);
      removeEventListener("resize", queue);
    };
  }, []);

  return { stepsRef, focus };
}

// THE CLOUDS. Behind the two tours, on wide screens: three large, soft glows
// in the passband's blues, each wandering on its own slow path as the page
// scrolls. Atmosphere only, no shapes to read. Positions and sizes are in
// viewport units; a, the glow's strength at its heart.
type Cloud = { w: number; h: number; l: number; t: number; rgb: string; a: number;
  ax: number; ay: number; px: number; py: number; ph: number };

const CLOUDS: Record<"mail" | "events", Cloud[]> = {
  mail: [
    { w: 62, h: 44, l: -14, t: 4, rgb: "78, 155, 234", a: 0.16, ax: 16, ay: 14, px: 2600, py: 3400, ph: 0 },
    { w: 50, h: 36, l: 34, t: 52, rgb: "104, 112, 232", a: 0.13, ax: 20, ay: 12, px: 3100, py: 2400, ph: 2.1 },
    { w: 54, h: 38, l: 66, t: -4, rgb: "70, 172, 206", a: 0.1, ax: 14, ay: 16, px: 2200, py: 3800, ph: 4.2 },
  ],
  events: [
    { w: 58, h: 40, l: -10, t: 46, rgb: "104, 112, 232", a: 0.14, ax: 18, ay: 12, px: 2900, py: 2600, ph: 1 },
    { w: 56, h: 40, l: 28, t: -2, rgb: "78, 155, 234", a: 0.15, ax: 16, ay: 14, px: 2400, py: 3600, ph: 3 },
    { w: 48, h: 34, l: 68, t: 50, rgb: "70, 172, 206", a: 0.1, ax: 14, ay: 16, px: 3300, py: 2300, ph: 5 },
  ],
};

// How strong the clouds are overall. Kept barely there on purpose: felt more
// than seen.
const CLOUD_STRENGTH = 0.22;
// How far each wanders from its place, as a share of its own ax and ay.
const CLOUD_WANDER = 0.45;
// The fastest the clouds will follow the scroll, in page pixels a second.
// A fast scroll outruns it, and the clouds keep drifting slowly after.
const CLOUD_MAX_RATE = 420;

// Two overlapping falloffs make a softer, less perfectly round glow.
const cloudFill = ({ rgb, a: a0 }: Cloud) => {
  const a = +(a0 * CLOUD_STRENGTH).toFixed(3);
  return `radial-gradient(closest-side, rgba(${rgb}, ${a}), rgba(${rgb}, ${(a * 0.4).toFixed(3)}) 55%, rgba(${rgb}, 0)), ` +
    `radial-gradient(closest-side at 68% 62%, rgba(${rgb}, ${(a * 0.7).toFixed(3)}), rgba(${rgb}, 0))`;
};

function Clouds({ kind }: { kind: "mail" | "events" }) {
  const clouds = CLOUDS[kind];
  const layerRef = useRef<HTMLDivElement>(null);
  const stageRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const layer = layerRef.current;
    const stage = stageRef.current;
    const section = layer?.parentElement;
    if (!layer || !stage || !section) return;
    if (matchMedia("(prefers-reduced-motion: reduce)").matches) return;
    const wide = matchMedia("(min-width: 901px)");
    let raf = 0;
    // The clouds follow the scroll, but never faster than a slow drift: a
    // hard flick of the wheel leaves them easing on behind it, gently, until
    // they arrive. Off screen they simply jump to where they belong.
    let shown: number | null = null;
    let last = 0;
    const draw = (p: number) => {
      const vw = innerWidth / 100;
      const vh = innerHeight / 100;
      clouds.forEach((c, i) => {
        const x = c.ax * CLOUD_WANDER * vw * Math.sin((p / c.px) * Math.PI * 2 + c.ph);
        const y = c.ay * CLOUD_WANDER * vh * Math.sin((p / c.py) * Math.PI * 2 + c.ph + 1.3);
        (stage.children[i] as HTMLElement).style.transform = `translate3d(${x.toFixed(1)}px, ${y.toFixed(1)}px, 0)`;
      });
    };
    const move = (now: number) => {
      raf = 0;
      const rect = section.getBoundingClientRect();
      const target = -rect.top;
      const dt = last ? Math.min(0.1, (now - last) / 1000) : 0;
      last = now;
      const offscreen = rect.bottom < 0 || rect.top > innerHeight;
      if (shown === null || offscreen) {
        shown = target;
      } else {
        // Ease toward the scroll, capped at CLOUD_MAX_RATE scroll-pixels a second.
        const want = (target - shown) * (1 - Math.exp(-dt / 0.6));
        const cap = CLOUD_MAX_RATE * dt;
        shown += Math.max(-cap, Math.min(cap, want));
      }
      draw(shown);
      if (!offscreen && Math.abs(target - shown) > 0.5) raf = requestAnimationFrame(move);
      else last = 0;
    };
    const queue = () => {
      if (!raf && wide.matches) raf = requestAnimationFrame(move);
    };
    queue();
    addEventListener("scroll", queue, { passive: true });
    addEventListener("resize", queue);
    return () => {
      cancelAnimationFrame(raf);
      removeEventListener("scroll", queue);
      removeEventListener("resize", queue);
    };
  }, [clouds]);

  return (
    <div className="pb-clouds" ref={layerRef} aria-hidden="true">
      <div className="pb-clouds-stage" ref={stageRef}>
        {clouds.map((c, i) => (
          <div key={i} className="pb-cloud" style={{
            width: `${c.w}vw`, height: `${c.h}vw`, left: `${c.l}vw`, top: `${c.t}vh`, background: cloudFill(c),
          }} />
        ))}
      </div>
    </div>
  );
}

// THE MAIL THAT ARRIVES DURING THE TOUR, told in the left column at the
// speed of the wheel: nothing pins, nothing waits for you. The window is the
// only thing that holds still. The first caption carries Jamie's email, which
// lives its whole life as it scrolls up the screen: it rises in, is read, and
// leaves for the window once it passes the middle. The second carries the
// run, a short stack of emails that each leave the same way, so two or three
// are in the air at once. `to` is the zone each lands in; the mock has
// a matching data-land row (or data-ping) and a data-count that ticks.
type Inbound = {
  id: string; who: string; subject: string; to: string;
  face: { brand: Brand } | { initials: string; tone: keyof typeof AVATAR };
};
const JAMIE: Inbound = { id: "jamie", who: "Jamie Chen", subject: "Offsite venue: Lisbon or Porto?",
  to: "For your eyes", face: { initials: "JC", tone: "blue" } };
const RUN: Inbound[] = [
  { id: "ship", who: "Rainforest", subject: "Your linen notebooks have shipped", to: "Shipments",
    face: { brand: "rainforest" } },
  { id: "cats", who: "Cats Weekly", subject: "Nine boxes, ranked by sit quality", to: "Reading",
    face: { brand: "cats" } },
  { id: "filed", who: "Marriott Bonvoy", subject: "Your points summary is ready", to: "Filed",
    face: { initials: "M", tone: "rust" } },
  { id: "rcpt", who: "Blue Bottle", subject: "Your receipt from Blue Bottle", to: "Receipts",
    face: { initials: "BB", tone: "ochre" } },
  { id: "cal", who: "Alex Rivera", subject: "Offsite: the 14th it is", to: "Calendar",
    face: { initials: "AR", tone: "violet" } },
];

const STEPS: Array<{ title: string; line: ReactNode }> = [
  { title: "New mail comes in",
    line: "Passband reads it the way you would: what it asks, by when, and what it is about. Jamie's clears the squelch, so it goes to the top of your day." },
  { title: "The rest sorts itself",
    line: "Shipping updates, receipts, newsletters and plan changes come in all day and go where they belong. The noise never reaches you." },
  { title: "What's left is yours",
    line: "Six emails came in. One needed you, and it is waiting at the top. The rest is where you would look for it." },
];

// The closing tally: where this morning's six went, each line lighting as
// its email lands.
const TALLY: Array<[id: string, where: string, what: string]> = [
  ["jamie", "For your eyes", "Jamie Chen"],
  ["ship", "Shipments", "Rainforest"],
  ["cats", "Reading", "Cats Weekly"],
  ["filed", "Filed", "Marriott Bonvoy"],
  ["rcpt", "Receipts", "Blue Bottle"],
  ["cal", "Calendar", "Alex Rivera"],
];

const clamp01 = (v: number) => Math.min(1, Math.max(0, v));
const ease = (t: number) => (t < 0.5 ? 4 * t * t * t : 1 - (-2 * t + 2) ** 3 / 2);

// A card rises in, is read, and leaves for its zone. `full` shows the whole
// read (Jamie's). It opens downward into the step's spare room (steps sit at
// their top, and are taller than what they hold), so nothing visible moves.
function InCard({ m, full }: { m: Inbound; full?: boolean }) {
  return (
    <div className="pb-in" data-in={m.id} data-full={full || undefined}>
      <div className="pb-in-body">
      <div className="pb-in-top">
        {"brand" in m.face
          ? <Logo brand={m.face.brand} />
          : <span className="pb-avatar" style={{ background: AVATAR[m.face.tone][0], color: AVATAR[m.face.tone][1] }}>{m.face.initials}</span>}
        <b>{m.who}</b>
        <small>now</small>
      </div>
      <div className="pb-in-subj">{m.subject}</div>
      {full && (
        <div className="pb-in-read">
          <dl>
            <dt>Asks</dt><dd>Pick a venue</dd>
            <dt>By</dt><dd>Friday</dd>
            <dt>About</dt><dd>Team offsite</dd>
          </dl>
          <div className="pb-in-meter"><i /><em /></div>
        </div>
      )}
      <span className="pb-in-to">
        <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
          <path d="M2.5 8h11m0 0L9.5 4m4 4-4 4" />
        </svg>
        {m.to}
      </span>
      </div>
    </div>
  );
}

// Drives the cards off where each one is on screen. Every card sits in the
// page and scrolls with it; only a card in flight is moved, by a transform
// from its own place to its zone in the window. d is how far past the
// reading line the card's middle is, in screen heights.
function useArrivals(pairRef: RefObject<HTMLDivElement | null>) {
  useEffect(() => {
    const pair = pairRef.current;
    const shot = pair?.querySelector<HTMLElement>(".pb-shot");
    if (!pair || !shot) return;
    const reduce = matchMedia("(prefers-reduced-motion: reduce)").matches;
    const cards = Array.from(pair.querySelectorAll<HTMLElement>(".pb-steps .pb-in"));
    let raf = 0;

    const frame = () => {
      raf = 0;
      const vh = innerHeight;
      const line = vh * (innerWidth > 900 ? 0.5 : 0.62);
      cards.forEach((card) => {
        const id = card.dataset.in!;
        // Measured without the flight's transform, so the page position is
        // the card's own.
        const home = (card.parentElement as HTMLElement).getBoundingClientRect();
        const d = (line - (home.top + home.height / 2)) / vh;
        // Jamie's takes its time over the read, low on the screen, before it
        // goes; the run's cards only flash where they are going.
        const full = card.hasAttribute("data-full");
        const a = clamp01((d + (full ? 0.48 : 0.42)) / 0.18);
        const r = full ? clamp01((d + 0.16) / 0.18) : clamp01((d + 0.2) / 0.14);
        const f = full ? clamp01((d - 0.08) / 0.26) : clamp01((d + 0.04) / 0.24);
        card.style.setProperty("--r", String(r));

        const target = shot.querySelector<HTMLElement>(`[data-land="${id}"], [data-ping="${id}"]`);
        if (!target) return;
        const arrived = f >= 1;
        if (arrived !== target.hasAttribute("data-arrived")) target.toggleAttribute("data-arrived", arrived);
        shot.querySelectorAll<HTMLElement>(`[data-count="${id}"]`).forEach((el) => {
          const n = Number(el.dataset.from) + (arrived ? 1 : 0);
          const text = `${n}${el.dataset.suffix ?? ""}`;
          if (el.textContent !== text) el.textContent = text;
          el.style.color = arrived ? "var(--accent-ink)" : "";
        });
        pair.querySelector(`[data-tally="${id}"]`)?.toggleAttribute("data-arrived", arrived);

        // The row opens through the second half of the flight, so the card
        // and the row it becomes finish together.
        const land = target.classList.contains("pb-land");
        const inner = land ? (target.firstElementChild as HTMLElement) : null;
        const rowH = inner ? inner.offsetHeight : 0;
        const open = reduce ? (arrived ? 1 : 0) : clamp01((f - 0.45) / 0.55);
        if (land) {
          target.style.height = open >= 1 ? "auto" : `${open * rowH}px`;
          target.style.opacity = String(open);
        }

        const slot = card.parentElement as HTMLElement;
        const flying = !reduce && f > 0;
        card.toggleAttribute("data-flying", flying);
        if (!flying) {
          slot.style.height = "";
          card.style.width = card.style.height = card.style.borderRadius = card.style.padding = "";
          card.style.removeProperty("--pt");
          card.style.setProperty("--fade", "1");
          card.style.setProperty("--m", "0");
          card.dataset.w = String(card.offsetWidth);
          card.dataset.h = String(card.offsetHeight);
          const cs = getComputedStyle(card);
          card.dataset.rad = cs.borderTopLeftRadius;
          card.dataset.pv = cs.paddingTop;
          card.dataset.pt = String(parseFloat(getComputedStyle(slot).getPropertyValue("--pt")) || 1.15);
          card.dataset.ph = cs.paddingLeft;
          card.style.transform = reduce ? "" : `translateY(${(1 - ease(a)) * 24}px)`;
          card.style.opacity = String(reduce ? (arrived ? 0 : 1) : a);
          return;
        }
        // Hold the card's place in the page while it changes shape.
        if (!slot.style.height) slot.style.height = `${slot.offsetHeight}px`;
        const w0 = Number(card.dataset.w), h0 = Number(card.dataset.h), r0 = parseFloat(card.dataset.rad ?? "16");
        const t = target.getBoundingClientRect();
        const row = (inner?.firstElementChild as HTMLElement | null) ?? target;
        const r1 = parseFloat(getComputedStyle(row).borderTopLeftRadius) || 0;
        const end = { l: t.left, t: t.top, w: t.width, h: land ? rowH : t.height };
        const e = ease(f);
        const lerp = (x: number, y: number) => x + (y - x) * e;
        const x = lerp(home.left, end.l);
        const y = lerp(home.top, end.t) - Math.sin(Math.PI * e) * 40;
        card.style.width = `${lerp(w0, end.w)}px`;
        card.style.height = `${lerp(h0, end.h)}px`;
        card.style.borderRadius = `${lerp(r0, r1)}px`;
        // The type shrinks to the window's scale (its points are 1/1040 of
        // its width), so the sender line lands at the row's size.
        card.style.setProperty("--pt", `${lerp(Number(card.dataset.pt), shot.offsetWidth / 1040)}px`);
        // Padding closes in so the sender line sits centred in the row.
        const top = card.querySelector<HTMLElement>(".pb-in-top")!.offsetHeight;
        const pv1 = Math.max(0, (end.h - top) / 2);
        card.style.padding = `${lerp(parseFloat(card.dataset.pv!), pv1)}px ${lerp(parseFloat(card.dataset.ph!), 10)}px`;
        card.style.transform = `translate(${x - home.left}px, ${y - home.top}px)`;
        card.style.setProperty("--fade", String(1 - clamp01((f - 0.05) / 0.4)));
        card.style.setProperty("--m", String(clamp01((f - 0.4) / 0.6)));
        card.style.opacity = String(1 - clamp01((f - 0.86) / 0.14));
      });
    };
    const queue = () => {
      if (!raf) raf = requestAnimationFrame(frame);
    };
    frame();
    addEventListener("scroll", queue, { passive: true });
    addEventListener("resize", queue);
    return () => {
      cancelAnimationFrame(raf);
      removeEventListener("scroll", queue);
      removeEventListener("resize", queue);
    };
  }, [pairRef]);
}

function Showcase() {
  const { stepsRef, focus } = useTourFocus();
  const pairRef = useRef<HTMLDivElement>(null);
  useArrivals(pairRef);

  // What sits under each caption.
  const mail = [
    <div className="pb-inslot"><InCard m={JAMIE} full /></div>,
    <div className="pb-inrun">
      {RUN.map((m) => <div key={m.id} className="pb-inslot"><InCard m={m} /></div>)}
    </div>,
    <dl className="pb-tally">
      <div className="pb-tally-h"><dt>This morning</dt><dd>6 in · 1 for you</dd></div>
      {TALLY.map(([id, where, what]) => (
        <div key={id} data-tally={id} className={id === "jamie" ? "you" : undefined}>
          <dt>{where}</dt><dd>{what}</dd>
        </div>
      ))}
    </dl>,
  ];

  return (
    <div className="pb-pair pb-mailpair" ref={pairRef}>
      <figure className="pb-panel pb-before">
        <figcaption>
          <b>Before</b> everything, in the order it arrived
          <span className="pb-unread" aria-label="4,312 unread">4,312</span>
        </figcaption>
        <div className="pb-window">
          <FakeInbox />
        </div>
      </figure>
      <div className="pb-hold">
        <div className="pb-gate" aria-hidden="true">
          <span>
            <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.6"
              strokeLinecap="round" strokeLinejoin="round">
              <path d="M2.5 8h11m0 0L9.5 4m4 4-4 4" />
            </svg>
          </span>
        </div>
        <figure className="pb-panel pb-after">
          <figcaption><b>After</b> what Passband shows you</figcaption>
          <div className="pb-shot">
            <div className="pb-window">
              <AppMock />
            </div>
          </div>
        </figure>
      </div>
      <ol className="pb-steps" ref={stepsRef}>
        {STEPS.map(({ title, line }, i) => (
          <li key={title} className="pb-step" data-on={i === focus || undefined}>
            <i aria-hidden="true">{String(i + 1).padStart(2, "0")}</i>
            <h3>{title}</h3>
            <p>{line}</p>
            <div className="pb-inmail" aria-hidden="true">{mail[i]}</div>
          </li>
        ))}
      </ol>
    </div>
  );
}

// MARK: - the calendar tour

// THE CALENDAR PAGE, as a second tour. Same frame as the sitrep's: captions
// scroll on the left, the window holds still on the right and picks out what
// each caption is about. Measured off the Mac's Calendar page (Month, with the
// event pane open) and drawn in the same points as the sitrep mock. Chip
// colours are sampled from the app; a ghost from mail is an outline (maybe),
// hatched (likely) or folded into "1 unlikely" until opened, as in
// CalendarMarks.swift. Every name in it is invented.
const CAL_CSS = `
/* Mirrored: the window holds the left and the notes pass on its right, so
   the second tour does not read as a repeat of the first. */
.pb-pair.pb-cal { grid-template-columns: auto minmax(0, 1fr); }
.pb-pair.pb-cal .pb-steps { grid-column: 2; grid-row: 1; padding-top: 8svh; }
.pb-pair.pb-cal .pb-step { padding: 0 0 0 clamp(1.5rem, 3vw, 3rem); }
.pb-pair.pb-cal .pb-hold { grid-column: 1; grid-row: 1; }
.pb-calshot .pb-app-title b { font-family: var(--sans); font-weight: 700; font-size: 17pt; }
.pb-seg { margin-left: auto; display: flex; padding: 3pt; border-radius: 99pt; background: #1a2535;
  font-size: 12pt; font-weight: 600; color: var(--ink); }
.pb-seg span { padding: 4pt 13pt; border-radius: 99pt; }
.pb-seg span.on { background: #296798; }
.pb-calpage { display: grid; grid-template-columns: minmax(0, 1fr) 300pt; column-gap: 18pt;
  padding: 0 18pt 0 20pt; min-height: 0; }
.pb-caltool { grid-column: 1 / -1; display: flex; align-items: center; gap: 10pt; height: 46pt; }
.pb-caltool .nav { width: 34pt; height: 26pt; border-radius: 7pt; background: #172131;
  display: grid; place-items: center; color: var(--dim); }
.pb-caltool .nav svg { width: 13pt; height: 13pt; }
.pb-caltool .new { display: flex; align-items: center; gap: 5pt; }
.pb-caltool .new svg { width: 11pt; height: 11pt; }
.pb-evpane > header svg { width: 13pt; height: 13pt; margin-left: -3pt; }
.pb-caltool b { font-size: 17pt; font-weight: 700; margin: 0 4pt; }
.pb-caltool .today { padding: 5pt 12pt; border-radius: 7pt; background: #172131; font-size: 11pt; color: var(--dim); }
.pb-caltool .new { margin-left: auto; padding: 6pt 13pt; border-radius: 7pt; background: #172131;
  font-size: 11pt; font-weight: 600; }
.pb-month { display: grid; grid-template-columns: repeat(7, minmax(0, 1fr)); grid-auto-rows: auto; align-content: start; }
.pb-wd { padding: 0 6pt 6pt; font-size: 10.5pt; font-weight: 500; color: #4d5865; border-bottom: 1px solid #1e2b3c; }
.pb-day { position: relative; height: 104pt; padding: 6pt 3pt 0; border-bottom: 1px solid #1e2b3c;
  display: flex; flex-direction: column; gap: 3pt; min-width: 0; }
.pb-day > header { display: flex; align-items: baseline; justify-content: space-between; padding: 0 4pt 2pt;
  font-size: 11.5pt; font-weight: 500; color: #818c9a; white-space: nowrap; gap: 4pt; }
.pb-day.out > header { color: #46515d; }
.pb-day.out .pb-ev { filter: brightness(0.7) saturate(0.6); }
.pb-day > header small { font-size: 9.5pt; color: var(--faintest); transition: color 0.4s ease; }
.pb-day::after { content: ""; position: absolute; inset: 1pt; border-radius: 9pt; pointer-events: none;
  box-shadow: inset 0 0 0 1pt rgba(155, 178, 205, 0); transition: box-shadow 0.4s ease; }
.pb-day.sel::after { box-shadow: inset 0 0 0 1pt rgba(155, 178, 205, 0.85); }
.pb-ev { --fill: #263244; --ink: #a9b6c8; --edge: #4b5d76;
  display: block; height: 19pt; line-height: 19pt; padding: 0 5pt; border-radius: 5pt;
  font-size: 10.5pt; font-weight: 600; white-space: nowrap; overflow: hidden; text-overflow: ellipsis;
  background: var(--fill); color: #eef1f6; transition: opacity 0.45s ease, box-shadow 0.45s ease; }
.pb-ev span { font-weight: 500; color: var(--ink); margin-right: 4pt; }
.pb-ev.maybe { background: none; box-shadow: inset 0 0 0 1pt var(--edge); }
.pb-ev.likely { background: repeating-linear-gradient(135deg, color-mix(in srgb, var(--fill) 70%, transparent) 0 3pt,
  transparent 3pt 6pt); box-shadow: inset 0 0 0 1pt var(--edge); }
.pb-ev.thin { background: none; box-shadow: inset 0 0 0 0.75pt color-mix(in srgb, var(--edge) 55%, transparent);
  color: #8a8f9f; }
.pb-ev.thin span { color: #6e7584; }
.pb-ev.picked { box-shadow: inset 0 0 0 1.25pt #c9d6e6; }
.pb-ev.fold { max-height: 0; opacity: 0; margin-top: -3pt; transition: max-height 0.45s ease, opacity 0.45s ease,
  margin 0.45s ease, box-shadow 0.45s ease; }
.pb-ev.red { --fill: #552d3b; --ink: #e79aad; --edge: #b5607a; }
.pb-ev.purple { --fill: #36375b; --ink: #caccfe; --edge: #8a8de0; }
.pb-ev.teal { --fill: #0d3b44; --ink: #7fc8ca; --edge: #3f9a9c; }
.pb-ev.brown { --fill: #513514; --ink: #e6b980; --edge: #b5844a; }
.pb-ev.green { --fill: #284425; --ink: #a5d69e; --edge: #5c9a55; }
.pb-ev.gray { --fill: #263244; --ink: #a9b6c8; --edge: #5a6d88; }

/* THE EVENT PANE, beside the month. */
.pb-evpane { min-width: 0; padding-top: 2pt; }
.pb-evpane > header { display: flex; align-items: center; gap: 6pt; padding: 0 2pt 10pt; font-size: 14pt;
  font-weight: 600; color: var(--dim); }
.pb-evcard { border-radius: 16pt; padding: 18pt; background: #152234;
  box-shadow: inset 0 0 0 0.75pt #24374f; animation: pb-cal-in 0.45s ease both; }
@keyframes pb-cal-in { from { opacity: 0; transform: translateY(5pt); } }
.pb-evsec { padding: 11pt; margin: -11pt; transition: opacity 0.45s ease, box-shadow 0.45s ease; border-radius: 10pt; }
.pb-evkind { display: flex; align-items: center; gap: 6pt; font-size: 10.5pt; color: var(--dim); padding-bottom: 11pt; }
.pb-evkind i { width: 9pt; height: 9pt; border-radius: 2.5pt; box-shadow: inset 0 0 0 1pt var(--accent); }
.pb-evkind i.on { background: var(--positive); box-shadow: none; }
.pb-evkind small { margin-left: auto; font-size: 10pt; color: var(--faintest); }
.pb-flyer { position: relative; border-radius: 9pt; overflow: hidden; margin-bottom: 12pt; }
.pb-flyer img { display: block; width: 100%; height: auto; }
.pb-flyer em { position: absolute; left: 7pt; bottom: 7pt; font-style: normal; font-size: 9.5pt; font-weight: 600;
  padding: 2pt 7pt; border-radius: 99pt; background: rgba(0, 0, 0, 0.45); color: #fff; }
.pb-evtitle { font-size: 16pt; font-weight: 700; line-height: 1.15; }
.pb-evtime { font-size: 11.5pt; color: var(--dim); margin-top: 2pt; }
.pb-tickets { display: inline-flex; align-items: center; gap: 5pt; margin-top: 9pt; padding: 5pt 11pt;
  border-radius: 8pt; background: var(--accent); color: #fff; font-size: 11pt; font-weight: 600;
  transition: box-shadow 0.45s ease; }
.pb-tickets svg { width: 12pt; height: 12pt; }
.pb-evfacts { display: grid; grid-template-columns: 62pt minmax(0, 1fr); gap: 8pt 8pt; margin-top: 13pt; font-size: 11pt; }
.pb-evfacts dt { color: var(--faint); }
.pb-evfacts dd { margin: 0; color: var(--ink); }
.pb-likely { display: flex; align-items: center; gap: 5pt; font-weight: 600; }
.pb-likely i { width: 9pt; height: 9pt; border-radius: 2.5pt; box-shadow: inset 0 0 0 1pt var(--accent); }
.pb-likely i.thin { box-shadow: inset 0 0 0 0.75pt var(--faintest); }
.pb-likely i.on { background: var(--positive); box-shadow: none; }
.pb-reason { margin-top: 3pt; font-weight: 400; color: var(--dim); line-height: 1.35; }
.pb-reason small { display: block; font-size: 9.5pt; color: var(--faintest); margin-top: 2pt; }
.pb-evsec.mail { margin-top: 3pt; }
.pb-behind { margin-top: 0; padding: 9pt 10pt 4pt; border-radius: 11pt; background: #101b2a; }
.pb-behind > b { display: block; font-size: 10.5pt; font-weight: 500; color: var(--dim); padding-bottom: 6pt; }
.pb-brow { display: grid; grid-template-columns: minmax(0, 1fr) auto; gap: 0 8pt; padding: 5pt 4pt; border-radius: 7pt;
  transition: background 0.45s ease; }
.pb-brow b { font-size: 11pt; font-weight: 600; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.pb-brow small { grid-column: 1; font-size: 9.5pt; color: var(--faint); }
.pb-brow em { grid-column: 2; grid-row: 1; font-style: normal; font-size: 9.5pt; color: var(--faintest); }
.pb-evkeys { display: flex; gap: 12pt; margin-top: 10pt; font-size: 10pt; color: var(--dim); }
.pb-evkeys kbd { font: 600 9pt/1 var(--mono); padding: 2pt 5pt; margin-right: 4pt; border-radius: 4pt;
  background: #1e2b3d; color: var(--dim); }

/* WHAT THE TOUR DOES TO THE PICTURE. As on the sitrep: the rest steps back
   and the part the caption is about keeps its light, ringed and glowing. */
.pb-calshot[data-focus] .pb-ev { opacity: 0.3; }
.pb-calshot[data-focus] .pb-evsec { opacity: 0.3; }
.pb-calshot[data-focus="plans"] .pb-ev.night,
.pb-calshot[data-focus="unlikely"] .pb-ev.summit { opacity: 1; }
.pb-calshot:is([data-focus="plans"], [data-focus="unlikely"]) .pb-evsec.top { opacity: 1; }
.pb-calshot[data-focus="plans"] .pb-ev.night,
.pb-calshot[data-focus="unlikely"] .pb-ev.summit {
  box-shadow: inset 0 0 0 1pt rgba(130, 186, 245, 0.9), 0 0 22pt -4pt rgba(78, 155, 234, 0.8); }
.pb-calshot[data-focus="plans"] .pb-evsec.top {
  box-shadow: inset 0 0 0 1pt rgba(130, 186, 245, 0.75), 0 0 44pt -8pt rgba(78, 155, 234, 0.65); }
.pb-calshot[data-focus="unlikely"] .pb-ev.fold { max-height: 19pt; opacity: 1; margin-top: 0; }
.pb-calshot[data-focus="unlikely"] .pb-day.summitday > header small { color: var(--accent-ink); }

@media (prefers-reduced-motion: reduce) {
  .pb-ev, .pb-evsec, .pb-day::after, .pb-tickets, .pb-brow { transition: none; }
  .pb-evcard { animation: none; }
}
`;
const CAL_CSS_RESOLVED = CAL_CSS.replace(/(-?\d*\.?\d+)pt\b/g, "calc($1 * var(--pt))");

type Chip = { t?: string; name: string; hue: string; kind?: "maybe" | "likely" | "thin" | "fold"; id?: string };
// November 2026, Sunday first, as the app lays it out: five weeks, the last
// running into December. Index 0 is Sunday November 1.
// November 2026, Sunday first, as the app lays it out: five weeks, the last
// running into December. Index 0 is Sunday November 1. A working month, with
// one night out.
const CAL_DAYS: Array<{ label: string; out?: boolean; unlikely?: number; chips: Chip[] }> = [
  { label: "Nov 1", chips: [{ t: "10am", name: "Long run", hue: "teal" }] },
  { label: "2", chips: [{ t: "9:30", name: "Standup", hue: "purple" }, { t: "2pm", name: "1:1 Jamie", hue: "purple" }] },
  { label: "3", chips: [{ name: "Election Day", hue: "brown" }, { t: "11am", name: "Pipeline", hue: "red" }] },
  { label: "4", chips: [{ t: "10am", name: "Design crit", hue: "purple" }, { t: "3pm", name: "Interview", hue: "green" }] },
  { label: "5", chips: [{ t: "9:30", name: "Standup", hue: "purple" }, { t: "1pm", name: "Priya", hue: "red" }, { t: "4pm", name: "Security", hue: "gray" }] },
  { label: "6", chips: [{ t: "4pm", name: "Demo day", hue: "teal" }] },
  { label: "7", chips: [] },
  { label: "8", chips: [] },
  { label: "9", chips: [{ t: "9:30", name: "Standup", hue: "purple" }, { t: "1pm", name: "Board prep", hue: "red" }] },
  { label: "10", chips: [{ t: "11am", name: "Pipeline", hue: "red" }, { t: "4pm", name: "Investors", hue: "red" }] },
  { label: "11", unlikely: 1, chips: [{ name: "Veterans Day", hue: "brown" }, { t: "9am", name: "Cost Summit", hue: "gray", kind: "fold", id: "summit" }, { t: "10am", name: "Design crit", hue: "purple" }] },
  { label: "12", chips: [{ t: "9:30", name: "Standup", hue: "purple" }, { t: "2pm", name: "Acme call", hue: "teal" }] },
  { label: "13", chips: [{ t: "12pm", name: "Team lunch", hue: "green" }] },
  { label: "14", chips: [] },
  { label: "15", chips: [] },
  { label: "16", chips: [{ t: "9:30", name: "Standup", hue: "purple" }, { t: "2pm", name: "1:1 Jamie", hue: "purple" }] },
  { label: "17", chips: [{ t: "10am", name: "Board", hue: "red" }, { t: "3pm", name: "Hiring", hue: "green" }] },
  { label: "18", chips: [{ t: "10am", name: "Design crit", hue: "purple" }, { t: "5pm", name: "Demo", hue: "teal" }] },
  { label: "19", chips: [{ t: "9:30", name: "Standup", hue: "purple" }, { t: "2:30pm", name: "Dr. Ortiz", hue: "teal" }] },
  { label: "20", chips: [{ t: "10am", name: "Roadmap", hue: "purple" }, { t: "10pm", name: "Endzeit x Non Series", hue: "purple", kind: "maybe", id: "night" }] },
  { label: "21", chips: [] },
  { label: "22", chips: [] },
  { label: "23", chips: [{ t: "9:30", name: "Standup", hue: "purple" }, { t: "3pm", name: "Offsite prep", hue: "green" }] },
  { label: "24", chips: [{ t: "11am", name: "Pipeline", hue: "red" }] },
  { label: "25", chips: [{ t: "8:05am", name: "SFO to JFK", hue: "gray", kind: "likely" }] },
  { label: "26", chips: [{ name: "Thanksgiving", hue: "brown" }, { t: "4pm", name: "Mom's", hue: "green" }] },
  { label: "27", chips: [] },
  { label: "28", chips: [] },
  { label: "29", chips: [{ t: "6:40pm", name: "JFK to SFO", hue: "gray", kind: "likely" }] },
  { label: "30", chips: [{ t: "9:30", name: "Standup", hue: "purple" }, { t: "2pm", name: "1:1 Jamie", hue: "purple" }] },
  { label: "Dec 1", out: true, chips: [{ t: "11am", name: "Pipeline", hue: "red" }] },
  { label: "Dec 2", out: true, chips: [{ t: "10am", name: "Design crit", hue: "purple" }] },
  { label: "Dec 3", out: true, chips: [{ t: "9:30", name: "Standup", hue: "purple" }] },
  { label: "Dec 4", out: true, unlikely: 1, chips: [] },
  { label: "Dec 5", out: true, chips: [] },
];

const CAL_TOUR: Array<{ key: string; title: string; line: ReactNode }> = [
  { key: "plans", title: "Your inbox already knows your plans",
    line: "Invitations, tickets and bookings show up on your calendar as they arrive, with the flyer, the place, and a line on how likely you are to go." },
  { key: "unlikely", title: "Not every invite is a plan",
    line: "The summit a vendor sent to everyone on its list folds away as unlikely. It is there if you want it, and out of the way if you don't." },
];


// What the pane shows for each step. Before the first and through the second
// it is the invitation as a maybe; the third opens the unlikely one; the last
// is the same invitation after you said you are going and bought tickets.
function EventPane({ focus }: { focus: string | undefined }) {
  if (focus === "unlikely") {
    return (
      <div className="pb-evpane">
        <header><Glyph name="back" />Wed Nov 11</header>
        <div className="pb-evcard" key="summit">
          <div className="pb-evsec top">
            <div className="pb-evkind"><i />Invitation, from your mail<small>not on your calendar</small></div>
            <div className="pb-evtitle">Cloud Cost Summit</div>
            <div className="pb-evtime">Wed Nov 11, 9am to 5pm</div>
            <dl className="pb-evfacts">
              <dt>Likelihood</dt>
              <dd>
                <span className="pb-likely"><i className="thin" />Unlikely</span>
                <div className="pb-reason">
                  “a vendor's mass mailing, and the user has never registered for one of their events”
                  <small>the triage model's words</small>
                </div>
              </dd>
              <dt>Place</dt><dd>Online</dd>
            </dl>
          </div>
          <div className="pb-evsec mail">
            <div className="pb-behind">
              <b>Mail behind it</b>
              <div className="pb-brow"><b>Last call: Cloud Cost Summit seats</b><small>Stackwise, Oct 19</small><em>first</em></div>
            </div>
          </div>
          <div className="pb-evkeys"><span><kbd>↵</kbd>open mail</span><span><kbd>y</kbd>going</span><span><kbd>x</kbd>dismiss</span></div>
        </div>
      </div>
    );
  }
  const going = focus === "remind";
  return (
    <div className="pb-evpane">
      <header><Glyph name="back" />Fri Nov 20</header>
      <div className="pb-evcard" key={going ? "going" : "night"}>
        <div className="pb-evsec top">
          <div className="pb-evkind">
            <i className={going ? "on" : undefined} />
            {going ? "Going, from your mail" : "Invitation, from your mail"}
            <small>{going ? "on your calendar" : "not on your calendar"}</small>
          </div>
          <div className="pb-flyer"><img src="/endzeit-flyer.jpg" alt="" /></div>
          <div className="pb-evtitle">Endzeit x Non Series</div>
          <div className="pb-evtime">Fri Nov 20, 10pm to Sat 6am</div>
          {going && (
            <span className="pb-tickets">
              <Glyph name="ticket" />
              Tickets
            </span>
          )}
          <dl className="pb-evfacts">
            <dt>Likelihood</dt>
            <dd>
              {going ? (
                <span className="pb-likely"><i className="on" />Going</span>
              ) : (
                <>
                  <span className="pb-likely"><i />Maybe</span>
                  <div className="pb-reason">
                    “mass mailing-list invite, though the user has a history with Endzeit events”
                    <small>the triage model's words</small>
                  </div>
                </>
              )}
            </dd>
            <dt>Place</dt><dd>Downtown San Francisco</dd>
          </dl>
        </div>
        <div className="pb-evsec mail">
          <div className="pb-behind">
            <b>Mail behind it</b>
            <div className="pb-brow"><b>Endzeit x Non Series, November 20</b><small>Endzeit, Oct 23</small><em>first</em></div>
            <div className="pb-brow update"><b>A correction on the Non Series lineup</b><small>Endzeit, Oct 23</small><em>update</em></div>
            {going && <div className="pb-brow"><b>Your tickets: Endzeit x Non Series</b><small>Endzeit, Oct 26</small><em>tickets</em></div>}
          </div>
        </div>
        {!going && <div className="pb-evkeys"><span><kbd>↵</kbd>open mail</span><span><kbd>y</kbd>going</span><span><kbd>x</kbd>dismiss</span></div>}
      </div>
    </div>
  );
}

function CalMock({ focus }: { focus: string | undefined }) {
  const selected = focus === "unlikely" ? 10 : 19;
  return (
    <div
      className="pb-app"
      role="img"
      aria-label="The Passband calendar, November. A working month: standups, reviews, a board meeting, two flights. An Endzeit invitation found in the mail sits on Friday the 20th as a maybe, with its flyer, the reason it is a maybe, and the two emails behind it, one of them a lineup correction. Wednesday the 11th holds one unlikely event, folded away."
    >
      <div className="pb-app-bar">
        <div className="pb-lights" aria-hidden="true"><i /><i /><i /></div>
        <div className="pb-app-title"><b>Calendar</b><small>MONTH</small></div>
        <div className="pb-seg" aria-hidden="true"><span className="on">Month</span><span>Week</span><span>Agenda</span></div>
      </div>
      <Rail on="calendar" />
      <div className="pb-calpage">
        <div className="pb-caltool">
          <span className="nav"><Glyph name="back" /></span><span className="nav"><Glyph name="forward" /></span>
          <b>November 2026</b>
          <span className="today">Today</span>
          <span className="new"><Glyph name="plus" />New event</span>
        </div>
        <div className="pb-month">
          {["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"].map((d) => <div key={d} className="pb-wd">{d}</div>)}
          {CAL_DAYS.map((day, i) => (
            <div key={day.label}
              className={`pb-day${day.out ? " out" : ""}${i === selected ? " sel" : ""}${i === 10 ? " summitday" : ""}`}>
              <header>{day.label}{day.unlikely ? <small>{day.unlikely} unlikely</small> : null}</header>
              {day.chips.map((c) => (
                <div key={c.name}
                  className={`pb-ev ${c.hue}${c.kind === "fold" ? " fold thin" : c.kind ? ` ${c.kind}` : ""}${c.id ? ` ${c.id}` : ""}${c.id === "night" && i === selected && focus !== "unlikely" ? " picked" : ""}`}>
                  {c.t && <span>{c.t}</span>}{c.name}
                </div>
              ))}
            </div>
          ))}
        </div>
        <EventPane focus={focus} />
      </div>
    </div>
  );
}

// The reminder an hour before doors. The line is the kind the
// reminder model writes: short, no links, no codes.
function CalShowcase() {
  const { stepsRef, focus } = useTourFocus();
  const key = focus < 0 ? undefined : CAL_TOUR[focus].key;
  return (
    <div className="pb-pair pb-cal">
      <div className="pb-hold">
        <figure className="pb-panel pb-after">
          <figcaption><b>Calendar</b> built from your mail</figcaption>
          <div className="pb-shot pb-calshot" data-focus={key}>
            <div className="pb-window">
              <CalMock focus={key} />
            </div>
          </div>
        </figure>
      </div>
      <ol className="pb-steps" ref={stepsRef}>
        {CAL_TOUR.map(({ key, title, line }, i) => (
          <li key={key} className="pb-step" data-on={i === focus || undefined}>
            <i aria-hidden="true">{String(i + 1).padStart(2, "0")}</i>
            <h3>{title}</h3>
            <p>{line}</p>
          </li>
        ))}
      </ol>
    </div>
  );
}


// THE LIGHT PAGE. Warm paper, ink, and one friendly blue: the same page with
// the instrument taken off it. The analyzer's chrome (graticule, markers,
// readouts, machined controls) goes; the trace stays as a calm ink line. The
// app's own window stays dark, because that is the app, and a dark product
// shot on paper reads as a thing you could have. ?theme=dark shows the night
// page.
export const THEME: "light" | "dark" =
  typeof location !== "undefined" && new URLSearchParams(location.search).get("theme") === "dark" ? "dark" : "light";

const NIGHT_TOKENS = `
  --bg: #090D16; --canvas: #0E141D; --card: #121A26;
  --ink: #E9EEF5; --dim: #A8B4C4; --faint: #808E9F; --faintest: #62707F;
  --hair: rgba(233, 238, 245, 0.08); --hair-strong: rgba(233, 238, 245, 0.14);
  --accent: #4E9BEA; --accent-ink: #82BAF5;
  --warn: #E8AC4C; --positive: #4FC08A; --danger: #FF7A68;`;

const LIGHT_CSS = `
:root[data-theme="light"] {
  --bg: #FAF7F2; --canvas: #FFFFFF; --card: #FFFFFF;
  --ink: #1D2430; --dim: #4F5867; --faint: #6F7886; --faintest: #959CA7;
  --hair: rgba(29, 36, 48, 0.08); --hair-strong: rgba(29, 36, 48, 0.13);
  --accent: #2E6BD6; --accent-ink: #2A62C4;
  --warn: #B7791F; --positive: #2F8F5B; --danger: #D24B3A;
  --shadow: 0 1px 2px rgba(29, 36, 48, 0.06), 0 12px 32px -12px rgba(29, 36, 48, 0.18);
}
:root[data-theme="light"] html, html[data-theme="light"] { color-scheme: light; }

/* The app keeps its own night inside its window. */
[data-theme="light"] .pb-shot { ${NIGHT_TOKENS} color: var(--ink); }

/* HERO: the instrument's furniture goes, the line stays. */
[data-theme="light"] :is(.pb-grat, .pb-mkr, .pb-mkrs, .pb-read-l, .pb-read-r) { display: none; }
[data-theme="light"] .pb-scrim { background:
  linear-gradient(90deg, rgba(250,247,242,0.96) 0%, rgba(250,247,242,0.85) 30%, rgba(250,247,242,0.35) 46%, rgba(250,247,242,0) 62%),
  linear-gradient(180deg, rgba(250,247,242,0) 74%, rgba(250,247,242,0.9) 100%); }
@media (max-width: 820px) {
  [data-theme="light"] .pb-scrim { background:
    linear-gradient(180deg, rgba(250,247,242,0.94) 0%, rgba(250,247,242,0.6) 26%, rgba(250,247,242,0) 36%),
    linear-gradient(180deg, rgba(250,247,242,0) 70%, rgba(250,247,242,0.88) 82%); }
}
[data-theme="light"] .pb-brand img { filter: none; }

/* THE BUTTON: one solid, friendly pill. No meter, no filament. */
[data-theme="light"] .pb-cta { color: #fff; background: var(--ink); border: 0; border-radius: 999px;
  padding: 0.95rem 1.7rem; box-shadow: 0 1px 2px rgba(29,36,48,0.12), 0 10px 24px -10px rgba(29,36,48,0.45); }
[data-theme="light"] .pb-cta:hover, [data-theme="light"] .pb-cta:focus-visible { color: #fff; background: #2B3444;
  box-shadow: 0 1px 2px rgba(29,36,48,0.12), 0 14px 30px -10px rgba(29,36,48,0.5); }
[data-theme="light"] :is(.pb-cta, .pb-rig)::before, [data-theme="light"] .pb-cta-meter { display: none; }
[data-theme="light"] .pb-cta[disabled] { color: rgba(255,255,255,0.7); }
[data-theme="light"] .pb-cta[disabled]:hover { box-shadow: none; }
/* The form: a white card with two fields and the same button. */
[data-theme="light"] .pb-rig { background: #fff; border: 1px solid var(--hair-strong); border-radius: 16px;
  box-shadow: var(--shadow); }
[data-theme="light"] .pb-rig:hover, [data-theme="light"] .pb-rig:focus-within { border-color: rgba(46,107,214,0.45);
  box-shadow: 0 0 0 4px rgba(46,107,214,0.1), var(--shadow); }
[data-theme="light"] .pb-rig-name { border-bottom-color: var(--hair-strong); }
[data-theme="light"] .pb-rig-field { color: var(--ink); }
[data-theme="light"] .pb-rig-field::placeholder, [data-theme="light"] .pb-field-cap { color: var(--faintest); }
[data-theme="light"] .pb-rig-field:is(:not(:placeholder-shown), :-webkit-autofill) + .pb-field-cap { color: var(--faint); }
[data-theme="light"] .pb-rig-field:focus + .pb-field-cap { color: var(--accent); }
[data-theme="light"] .pb-rig-field:-webkit-autofill { -webkit-text-fill-color: var(--ink); caret-color: var(--ink);
  box-shadow: inset 0 0 0 100vw #fff; }
[data-theme="light"] .pb-cta-in-rig { margin: 0.45rem; padding: 0.8rem 1.3rem; border-radius: 999px; border: 0; }
[data-theme="light"] .pb-cta-in-rig:hover, [data-theme="light"] .pb-cta-in-rig:focus-visible { box-shadow: none; }

/* THE TOUR. No step numbers; the windows sit on paper like photographs. */
[data-theme="light"] .pb-step > i { display: none; }
[data-theme="light"] .pb-window { border-color: rgba(29,36,48,0.1);
  box-shadow: 0 2px 4px rgba(29,36,48,0.05), 0 30px 70px -28px rgba(29,36,48,0.4); }
[data-theme="light"] .pb-after .pb-window { border-color: rgba(29,36,48,0.14);
  box-shadow: 0 2px 4px rgba(29,36,48,0.06), 0 40px 90px -30px rgba(29,36,48,0.5); }
[data-theme="light"] .pb-before .pb-window { filter: none; background: #fff; }
[data-theme="light"] .pb-row { border-bottom-color: var(--hair); }
[data-theme="light"] .pb-row-sender { color: var(--dim); }
[data-theme="light"] .pb-unread { background: rgba(210, 75, 58, 0.1); color: var(--danger); }
[data-theme="light"] .pb-panel figcaption b { color: var(--faint); }
[data-theme="light"] .pb-after figcaption b { color: var(--accent); }
[data-theme="light"] .pb-gate::before { background: linear-gradient(transparent, rgba(29,36,48,0.18) 25%, rgba(29,36,48,0.18) 75%, transparent);
  box-shadow: none; }
[data-theme="light"] .pb-gate span { background: #fff; color: var(--accent);
  box-shadow: inset 0 0 0 1px var(--hair-strong), var(--shadow); }
@media (max-width: 900px) {
  [data-theme="light"] .pb-after { background: linear-gradient(var(--bg) 86%, rgba(250,247,242,0)); }
  [data-theme="light"] .pb-gate::before { background: linear-gradient(90deg, transparent, rgba(29,36,48,0.18) 25%, rgba(29,36,48,0.18) 75%, transparent); }
}
/* The arriving mail: white cards, like a notification, darkening into the
   app's row as they land. */
[data-theme="light"] .pb-in { color: var(--dim);
  background: color-mix(in srgb, rgba(52, 96, 150, 0.95) calc(var(--m, 0) * 100%), #ffffff);
  box-shadow: inset 0 0 0 1px rgba(29,36,48,0.08), 0 1px 2px rgba(29,36,48,0.06), 0 16px 36px -14px rgba(29,36,48,0.28); }
/* The name stays ink until the ground has darkened enough to carry it, then
   flips light over the last stretch, so it never goes pale on pale. */
[data-theme="light"] .pb-in-top b { color: color-mix(in srgb, #F4F7FB calc(clamp(0, (var(--m, 0) - 0.55) * 4, 1) * 100%), var(--ink)); }
[data-theme="light"] .pb-in-subj, [data-theme="light"] .pb-in-read dd { color: var(--ink); }
[data-theme="light"] .pb-in-to { box-shadow: inset 0 0 0 1px rgba(46,107,214,0.35); color: var(--accent); }
[data-theme="light"] .pb-in-meter { background: rgba(29,36,48,0.08); }
[data-theme="light"] .pb-in-meter > i { box-shadow: none; background: linear-gradient(90deg, rgba(46,107,214,0.35), var(--accent)); }
[data-theme="light"] :is(.pb-in-read dt, .pb-in-meter > em::before) { font-family: var(--sans); letter-spacing: 0.02em; }
/* The tally and the trust row: plain cards, sans labels. */
[data-theme="light"] :is(.pb-tally, .pb-facts) { background: #fff; border-color: var(--hair-strong); box-shadow: var(--shadow); }
[data-theme="light"] .pb-facts::before { display: none; }
[data-theme="light"] :is(.pb-tally > div, .pb-fact) { border-color: var(--hair); }
[data-theme="light"] :is(.pb-tally dt, .pb-tally .pb-tally-h dd, .pb-fact dt) { font-family: var(--sans); font-size: 0.78rem;
  letter-spacing: 0.01em; text-transform: none; }
[data-theme="light"] .pb-fact dt b { display: none; }
[data-theme="light"] .pb-tally > div.you[data-arrived] { background: rgba(46,107,214,0.07); }
[data-theme="light"] .pb-fact a { color: var(--accent); }

/* The footer mark keeps its colour on paper. */
[data-theme="light"] .pb-foot-mark { filter: none; opacity: 0.55; }
`;

// MARK: - the page

// Where the analyzer draws, as fractions of the stage. Wide screens tune the
// band to the right of the copy; narrow ones centre it under the copy, low.
//
// On a phone the scope sits BETWEEN the copy and the waitlist, so its band is
// measured off them rather than guessed: the trace's baseline just above the
// slot, full scale reaching just under the beats. Phone heights vary too much
// for fixed fractions to clear both on every one.
function scopeLayout(): ScopeLayout {
  if (innerWidth > 820) return { center: 0.7, base: 0.82, scale: 0.5, spread: 1 };
  // The first leaf, not the stage: the leaf is where the copy sits before any
  // scrolling, and measuring against it gives the same answer at any scroll.
  const stage = document.querySelector(".pb-leaf")?.getBoundingClientRect();
  const beats = document.querySelector(".pb-beats")?.getBoundingClientRect();
  const slot = document.querySelector(".pb-slot")?.getBoundingClientRect();
  if (!stage || !beats || !slot || stage.height < 1) {
    return { center: 0.5, base: 0.7, scale: 0.3, spread: 1.9 };
  }
  const top = beats.bottom - stage.top + 12;
  const bottom = slot.top - stage.top - 18;
  return {
    center: 0.5,
    base: bottom / stage.height,
    scale: Math.max(0.08, (bottom - top) / stage.height),
    spread: 1.9,
  };
}

// The carriers' names in the marker table, in the scope's order.
const MARKERS: Array<[who: string, status: string, tone: string]> = [
  ["Parkline Properties", "past due", "late"],
  ["Dr. Ortiz's Office", "today", "soon"],
  ["Jamie Chen", "due Fri", "soon"],
];

export function App() {
  const storyRef = useRef<HTMLElement>(null);
  const stageRef = useRef<HTMLDivElement>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const [scrolled, setScrolled] = useState(false);
  const [locked, setLocked] = useState(false);
  const readoutRef = useRef<HTMLElement>(null);
  const [layout, setLayout] = useState<ScopeLayout>(() => scopeLayout());

  useEffect(() => {
    const canvas = canvasRef.current;
    const story = storyRef.current;
    if (!canvas || !story) return;
    const reduceMotion = matchMedia("(prefers-reduced-motion: reduce)").matches;
    const scene = createScope(canvas, {
      reduceMotion,
      light: THEME === "light",
      layout: scopeLayout,
      // Written straight to the DOM: it changes every frame, and a re-render
      // per frame would buy nothing.
      onReadout: (squelch) => {
        if (readoutRef.current) readoutRef.current.textContent = `${Math.round(squelch * 100)}%`;
      },
    });

    // THE SQUELCH IS KEYED TO THE SECOND HEADLINE, not to the section's length:
    // it closes as that headline comes up the screen and is fully shut at the
    // moment the headline is level with the middle, never before. `SHUT` is
    // the progress at which the scope has finished closing (scope.ts's
    // squelchAt), and the markers come up just short of it.
    const second = story.querySelector<HTMLElement>(".pb-leaf-1 .pb-beat");
    const SHUT = 0.52;
    const onScroll = () => {
      const rect = story.getBoundingClientRect();
      const scrolled = -rect.top;
      let arrival = Math.max(1, rect.height - innerHeight);
      if (second) {
        const at = second.getBoundingClientRect();
        // How far the page has to scroll for the headline to sit mid-screen.
        arrival = Math.max(1, scrolled + at.top + at.height / 2 - innerHeight / 2);
      }
      const t = Math.max(0, scrolled / arrival);
      const p = Math.min(1, t <= 1 ? t * SHUT : SHUT + (t - 1) * 0.3);
      scene?.setProgress(p);
      setLocked(p > 0.46);
      setScrolled(scrolled > innerHeight * 0.03);
    };
    const onResize = () => {
      setLayout(scopeLayout());
      onScroll();
    };
    // The phone layout is measured off the copy, which only has its real
    // height once mounted and again once the serif has loaded.
    onResize();
    document.fonts?.ready.then(onResize);
    addEventListener("scroll", onScroll, { passive: true });
    addEventListener("resize", onResize);

    // Nothing to draw once the stage has scrolled away.
    const io = new IntersectionObserver(([entry]) => scene?.setVisible(entry.isIntersecting));
    io.observe(stageRef.current!);

    return () => {
      removeEventListener("scroll", onScroll);
      removeEventListener("resize", onResize);
      io.disconnect();
      scene?.destroy();
    };
  }, []);

  // THE WAITLIST IS A STATE OF THIS PAGE, not a document of its own: the URL
  // changes, so the link stays real and back still goes back, but nothing
  // remounts and the scope behind it never restarts.
  const [joining, setJoining] = useState(() => location.pathname === WAITLIST_PATH);
  useEffect(() => {
    const sync = () => setJoining(location.pathname === WAITLIST_PATH);
    addEventListener("popstate", sync);
    return () => removeEventListener("popstate", sync);
  }, []);
  const open = (next: boolean) => {
    if (next === joining) return;
    history.pushState(null, "", next ? WAITLIST_PATH : "/");
    setJoining(next);
  };
  const go = (next: boolean) => (event: MouseEvent<HTMLAnchorElement>) => {
    // Anything but a plain left click is asking for a new document: a new
    // tab, a new window, a saved link. Let the browser have those.
    if (event.metaKey || event.ctrlKey || event.shiftKey || event.altKey || event.button !== 0) return;
    event.preventDefault();
    open(next);
  };

  // The closing button goes back up to the hero and opens the rig there, so
  // there is only ever one form on the page.
  const backToTop = () => {
    const smooth = !matchMedia("(prefers-reduced-motion: reduce)").matches;
    scrollTo({ top: 0, behavior: smooth ? "smooth" : "auto" });
    open(true);
  };

  return (
    <main className="pb">
      <style href="pb-page" precedence="default">{PAGE_CSS_RESOLVED}</style>
      <style href="pb-cal" precedence="default">{CAL_CSS_RESOLVED}</style>
      <style href="pb-light" precedence="default">{LIGHT_CSS}</style>

      <section ref={storyRef} className="pb-story">
        <div
          ref={stageRef}
          className="pb-stage"
          data-locked={locked || undefined}
          data-scrolled={scrolled || undefined}
        >
          <canvas ref={canvasRef} className="pb-scene" aria-hidden="true" />
          <div className="pb-grat" />
          {CARRIERS.map((_, i) => {
            const { x, y } = markerPoint(i, layout);
            return (
              <span key={i} className="pb-mkr" style={{ left: `${x * 100}%`, top: `${y * 100}%` }}
                aria-hidden="true">{i + 1}</span>
            );
          })}
          <div className="pb-scrim" />
          <div className="pb-mkrs" aria-label="What made it through">
            <div><span>MKR</span><span>SENDER</span><span>STATUS</span></div>
            {MARKERS.map(([who, status, tone], i) => (
              <div key={who}>
                <b>{i + 1}</b>
                <span>{who}</span>
                <span className={tone}>{status}</span>
              </div>
            ))}
          </div>
          <span className="pb-read-l" aria-hidden="true">SPAN&nbsp;&nbsp;your inbox&nbsp;&nbsp;·&nbsp;&nbsp;4,312 msgs</span>
          <span className="pb-read-r" aria-hidden="true">SQUELCH&nbsp;<b ref={readoutRef}>0%</b></span>

          <div className="pb-cue" aria-hidden="true">scroll</div>
        </div>

        <div className="pb-track">
          <div className="pb-leaf">
            <header className="pb-top">
              <a className="pb-brand" href="/" onClick={go(false)}>
                <img src="/mark.svg" alt="" width={42} height={23} />
                <span>passband</span>
              </a>
              <nav className="pb-nav">
                <a href="/self-host">Self-host</a>
                <a href="/trust">Trust</a>
                <a className="pb-keep" href="https://github.com/passband-app/passband">GitHub</a>
              </nav>
            </header>
            <div className="pb-copy">
              <div className="pb-beat pb-beats">
                <p className="pb-lede">Inbox zero every day was never realistic.</p>
                <h1 className="pb-hero">You’re only human.</h1>
                <p className="pb-sub">
                  Your attention is valuable. You deserve an inbox that treats it that way.
                </p>
              </div>
              <div className="pb-slot">
                {joining ? <Waitlist /> : <JoinButton onClick={go(true)} />}
                <p className="pb-fine">For Mac and iPhone.</p>
              </div>
            </div>
          </div>
          <div className="pb-leaf pb-leaf-1">
            <div className="pb-copy">
              <div className="pb-beat">
                <h2 className="pb-hero">Know what needs you.</h2>
                <p className="pb-sub">
                  Passband brings the important things forward, so you can give them your
                  attention and get on with your day.
                </p>
              </div>
            </div>
          </div>
        </div>
      </section>

      <section className="pb-section pb-clouded">
        <Clouds kind="mail" />
        <h2 className="pb-h2">
          Same inbox. <em>Squelched.</em>
        </h2>
        <p className="pb-intro">
          Every email still arrives. Passband reads each one, pulls forward the few that
          need you, and files the rest where you can find them when you want to.
        </p>
        <Showcase />
      </section>

      <section className="pb-section pb-clouded">
        <Clouds kind="events" />
        <h2 className="pb-h2">
          Plans arrive by email. <em>So does your calendar.</em>
        </h2>
        <p className="pb-intro">
          Passband lays out the invitations, tickets and bookings in your mail and says how
          likely you are to go.
        </p>
        <CalShowcase />
      </section>

      <section className="pb-section pb-join">
        <h2 className="pb-h2">That’s an inbox built in the <em>modern era.</em></h2>
        <p className="pb-intro">Join the waitlist and we will email you when a spot opens.</p>
        <Waitlist focus={false} />
        <p className="pb-fine">For Mac and iPhone.</p>
      </section>

      <section className="pb-section pb-layer">
        <h2 className="pb-h2">An intelligence layer, <em>not a wrapper.</em></h2>
        <div className="pb-layer-body">
          <p>
            Most AI email is a chat box bolted onto the same inbox. You still do the reading.
            It just answers questions about the pile.
          </p>
          <p>
            Passband puts a capable agentic layer underneath instead. Every message is read
            as it arrives, revisited as things change, and turned into what it means for you:
            a deadline, a package, a receipt, a person waiting on a reply.
          </p>
          <p>
            Everything else draws on that one understanding. The sitrep, the notifications
            on your phone and the agent you already use all start from mail that has been
            read, not mail that is waiting to be.
          </p>
        </div>
      </section>

      <section className="pb-section" style={{ paddingTop: 0 }}>
        <dl className="pb-facts">
          <div className="pb-fact">
            <dt><b>01</b> License</dt>
            <dd>
              <div className="pb-fact-val">Open source</div>
              <p>
                MIT licensed. Every line is <a href="https://github.com/passband-app/passband">on GitHub</a>,
                the threat model included.
              </p>
            </dd>
          </div>
          <div className="pb-fact">
            <dt><b>02</b> Where it runs</dt>
            <dd>
              <div className="pb-fact-val">Yours or ours</div>
              <p>
                <a href="/self-host">Self-host</a> the daemon on your own machine, or let us run an
                isolated instance for you.
              </p>
            </dd>
          </div>
          <div className="pb-fact">
            <dt><b>03</b> Training on your mail</dt>
            <dd>
              <div className="pb-fact-val">Never</div>
              <p>
                Not for us, not for our model provider. <a href="/trust">Where your mail goes</a>,
                every wire of it.
              </p>
            </dd>
          </div>
        </dl>
      </section>

      <section className="pb-section pb-close">
        <h2 className="pb-h2">Find a little breathing room.</h2>
        <button className="pb-cta" type="button" onClick={backToTop}>
          <span className="pb-cta-label">join the waitlist</span>
          <Arrow />
        </button>
      </section>

      <footer className="pb-foot">
        <div className="pb-foot-grid">
          <a className="pb-foot-mark" href="/" aria-label="Passband home"><img src="/mark.svg" alt="" width={42} height={23} /></a>
          <nav aria-label="Legal">
            <p>Legal</p>
            <a href="/terms">Terms of Service</a>
            <a href="/privacy">Privacy Policy</a>
          </nav>
          <nav aria-label="Security">
            <p>Security</p>
            <a href="/trust">Trust</a>
            <a href="/security">Threat model</a>
            <a href="mailto:braelyn@passband.app">Report a vulnerability</a>
          </nav>
          <nav aria-label="Connect">
            <p>Connect</p>
            <a href="/about">About</a>
            <a href="/self-host">Self-host</a>
            <a href="https://github.com/passband-app/passband/releases" target="_blank" rel="noopener noreferrer">Changelog</a>
            <a href="https://github.com/passband-app/passband" target="_blank" rel="noopener noreferrer">GitHub</a>
            <a href="mailto:braelyn@passband.app">Email</a>
          </nav>
        </div>
        <p className="pb-foot-legal">&copy; 2026 Passband. All rights reserved.</p>
      </footer>
    </main>
  );
}
