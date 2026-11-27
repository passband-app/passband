// The Passband demo film. Every frame is a pure function of the clock in
// timeline.ts, except the analyzer, which is the site scope's look drawn from
// an FFT of the soundtrack (analyzer.ts): the trace is what you hear.
import { createAnalyzer, decodeWav, type Layout } from "./analyzer";
import { DURATION, FLICKS, FPS, T, RULE_TEXT, typeTimes } from "./timeline";

const W = 1920;
const H = 1080;

// ---------------------------------------------------------------- easing
const clamp = (x: number, a = 0, b = 1) => Math.min(b, Math.max(a, x));
const lin = (t: number, a: number, b: number) => clamp((t - a) / (b - a));
const smooth = (x: number) => x * x * (3 - 2 * x);
const outCubic = (x: number) => 1 - Math.pow(1 - x, 3);
const outExpo = (x: number) => (x >= 1 ? 1 : 1 - Math.pow(2, -10 * x));
const inOut = (x: number) => (x < 0.5 ? 4 * x * x * x : 1 - Math.pow(-2 * x + 2, 3) / 2);
const mix = (a: number, b: number, k: number) => a + (b - a) * k;
/// 0 before `a`, eased up to 1 over `dur`, and back down over `dur` from `b`.
const window_ = (t: number, a: number, b = Infinity, dur = 0.7) =>
  outCubic(lin(t, a, a + dur)) * (1 - smooth(lin(t, b, b + dur)));

// ---------------------------------------------------------------- styles
const CSS = `
:root {
  --bg: #090D16; --canvas: #0E141D; --ink: #E9EEF5; --dim: #A8B4C4; --faint: #808E9F; --faintest: #62707F;
  --hair: rgba(233, 238, 245, 0.08); --hair-strong: rgba(233, 238, 245, 0.14);
  --accent: #4E9BEA; --accent-ink: #82BAF5; --warn: #E8AC4C; --positive: #4FC08A; --danger: #FF7A68;
  --serif: "Newsreader", ui-serif, Georgia, serif;
  --sans: -apple-system, BlinkMacSystemFont, "SF Pro Text", system-ui, sans-serif;
  --mono: ui-monospace, "SF Mono", Menlo, monospace;
}
html, body { margin: 0; width: ${W}px; height: ${H}px; overflow: hidden; background: var(--bg); color: var(--ink);
  font-family: var(--sans); -webkit-font-smoothing: antialiased; }
.abs { position: absolute; }
canvas.scene { position: absolute; inset: 0; width: 100%; height: 100%; display: block; }
.grat { position: absolute; inset: 0;
  background-image: repeating-linear-gradient(90deg, rgba(130,186,245,.07) 0 1px, transparent 1px 10%),
    repeating-linear-gradient(180deg, rgba(130,186,245,.07) 0 1px, transparent 1px 12.5%);
  -webkit-mask-image: radial-gradient(ellipse 75% 70% at 62% 55%, #000 30%, transparent 85%); }
.scrim { position: absolute; inset: 0;
  background: linear-gradient(90deg, rgba(9,13,22,.95) 0%, rgba(9,13,22,.82) 30%, rgba(9,13,22,.4) 46%, rgba(9,13,22,0) 62%),
    linear-gradient(180deg, rgba(9,13,22,0) 74%, rgba(9,13,22,.85) 100%); }
.veil { position: absolute; inset: 0; background: var(--bg); }

.brand { left: 76px; top: 58px; display: flex; align-items: center; gap: 14px; }
.brand img { width: 52px; }
.brand span { font-family: var(--serif); font-size: 32px; font-weight: 500; letter-spacing: -0.005em; }
.read { bottom: 40px; font: 500 15px/1 var(--mono); letter-spacing: .08em; color: var(--faintest); }
.read b { color: var(--accent-ink); font-weight: 600; display: inline-block; min-width: 4ch; text-align: right; }

.copy { left: 150px; width: 900px; }
.lede { margin: 0; font-family: var(--serif); font-size: 56px; line-height: 1.12; color: var(--dim); }
.hero { margin: 0; font-family: var(--serif); font-weight: 500; font-size: 112px; line-height: 1.0; letter-spacing: -0.014em; }
.hero em, .h2 em { font-style: normal; color: var(--accent-ink); }
.sub { margin: 0; font-size: 26px; line-height: 1.55; color: var(--dim); }

.mkr { transform: translate(-50%, calc(-100% - 16px)); font: 600 16px/1 var(--mono); color: var(--accent-ink);
  display: flex; flex-direction: column; align-items: center; gap: 5px; }
.mkr::after { content: ""; border: 7px solid transparent; border-top: 8px solid var(--accent); border-bottom: 0; }
.mkrs { right: 120px; top: 150px; width: 470px; font: 500 19px/1.2 var(--mono); color: var(--dim);
  border: 1px solid rgba(130,186,245,.22); border-radius: 13px; background: rgba(9,13,22,.74); }
.mkrs div { display: grid; grid-template-columns: 2.2rem 1fr auto; gap: .8rem; padding: 1rem 1.3rem;
  border-top: 1px solid rgba(130,186,245,.1); }
.mkrs div:first-child { border-top: 0; font-size: 13px; letter-spacing: .1em; color: var(--faintest); }
.mkrs b { color: var(--accent-ink); font-weight: 600; }
.late { color: var(--danger); } .soon { color: var(--warn); }

/* THE WALL: the before panel's doomscroll, blown up and turned in space. */
.wallport { left: 760px; top: -40px; width: 1260px; height: 1160px; perspective: 1700px; }
.wall { position: absolute; left: 0; top: 0; width: 1180px; height: 1160px; overflow: hidden;
  transform-origin: 0% 50%; border-left: 1px solid var(--hair-strong); background: #0c1017;
  -webkit-mask-image: linear-gradient(transparent, #000 16%, #000 78%, transparent); }
.rows { position: absolute; left: 0; right: 0; top: 0; }
.row { position: relative; display: flex; align-items: baseline; gap: 22px; height: 64px; box-sizing: border-box;
  padding: 20px 30px 0; border-bottom: 1px solid var(--hair); white-space: nowrap; font-size: 21px; }
.row .s { width: 250px; flex: none; font-weight: 600; color: #b9c2cd; overflow: hidden; text-overflow: ellipsis; }
.row .x { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; color: var(--faintest); }
.row .x span { color: #9aa5b3; }
.row .tm { flex: none; font-size: 17px; color: var(--faintest); }
.row .bar { position: absolute; left: 0; top: 10px; bottom: 10px; width: 4px; border-radius: 2px; background: var(--accent); }
.unread { display: inline-block; font: 600 88px/1 var(--sans); font-variant-numeric: tabular-nums; color: #fff;
  background: #c7433a; padding: 14px 34px 16px; border-radius: 999px; box-shadow: 0 0 60px -10px rgba(199,67,58,.7); }
.kicker { font: 600 15px/1 var(--mono); letter-spacing: .16em; text-transform: uppercase; color: var(--faintest); }

/* THE APP: the landing page's sitrep mock, laid out in the app's own points. */
.stage3d { inset: 0; perspective: 2200px; }
.win { position: absolute; left: 0; top: 0; width: ${1480}px; border-radius: 22px; overflow: hidden;
  background: #262c35; transform-origin: 0 0;
  border: 1px solid rgba(78,155,234,.45);
  box-shadow: 0 0 0 1px rgba(78,155,234,.12), 0 0 120px -20px rgba(78,155,234,.45), 0 60px 140px -30px rgba(0,0,0,.8); }
.app { --pt: ${1480 / 1040}px; display: grid; grid-template-columns: 58pt minmax(0, 1fr); grid-template-rows: 40pt auto;
  font-size: 12pt; color: var(--ink); line-height: 1.25; }
.app-bar { grid-column: 1 / -1; display: flex; align-items: center; gap: 14pt; padding: 0 18pt 0 16pt; }
.lights { display: flex; gap: 8pt; }
.lights i { width: 12pt; height: 12pt; border-radius: 50%; background: #ec6a5e; }
.lights i:nth-child(2) { background: #f4bf4f; } .lights i:nth-child(3) { background: #61c554; }
.app-title { display: flex; align-items: baseline; gap: 8pt; margin-left: 12pt; }
.app-title b { font-family: var(--serif); font-weight: 500; font-size: 19pt; }
.app-title small { font-size: 10pt; font-weight: 500; letter-spacing: .06em; color: var(--faintest); }
.app-tools { margin-left: auto; display: flex; align-items: center; gap: 12pt; font-size: 10pt; font-weight: 500; color: var(--faint); }
.app-tools svg { width: 10pt; height: 10pt; vertical-align: -1pt; margin-right: 3pt; }
.need { display: inline-flex; align-items: center; gap: 5pt; color: var(--c); font-size: 11pt; font-weight: 500;
  padding: 3pt 9pt; border-radius: 99pt; background: color-mix(in srgb, var(--c) 14%, transparent);
  box-shadow: inset 0 0 0 .75pt color-mix(in srgb, var(--c) 45%, transparent); }
.need::before { content: ""; width: 5pt; height: 5pt; border-radius: 50%; background: var(--c); }
.side { background: #2c323f; border-right: 1px solid #363b48; border-top-right-radius: 10pt;
  display: flex; flex-direction: column; align-items: center; gap: 13pt; padding: 12pt 0 16pt; }
.side span { width: 36pt; height: 30pt; display: grid; place-items: center; border-radius: 8pt; color: #9aa6b6; }
.side span.on { background: rgba(78,155,234,.2); color: var(--accent); }
.side svg { width: 18pt; height: 18pt; }
.side .gap { flex: 1; }
.side .me { width: 20pt; height: 20pt; border-radius: 50%; background: #24384F; color: #A9CBF0; font-size: 10pt; font-weight: 600; }
.page { padding: 6pt 22pt 22pt 30pt; display: grid; grid-template-columns: minmax(0, 1fr) 252pt; column-gap: 14pt; align-items: start; }
.dash-hero { grid-column: 1 / -1; margin-bottom: 16pt; position: relative; }
.dash-hero small { display: block; font-size: 10pt; font-weight: 500; letter-spacing: .12em; text-transform: uppercase; color: var(--accent); margin-bottom: 2pt; }
.dash-hero .lines { position: relative; height: 39pt; }
.dash-hero b { position: absolute; left: 0; top: 0; white-space: nowrap; font-family: var(--serif); font-weight: 500; font-size: 36pt; letter-spacing: -.01em; line-height: 1.08; }
.col { display: flex; flex-direction: column; gap: 14pt; min-width: 0; }
.zone { border-radius: 16pt; padding: 12pt 12pt 10pt; background: #1f2c3a; box-shadow: inset 0 0 0 .75pt #2f4459; }
.zone-h { display: flex; align-items: center; gap: 7pt; padding: 0 4pt 7pt; font-size: 13pt; font-weight: 600; }
.zone-h svg { width: 14pt; height: 14pt; color: var(--zone, var(--accent)); flex: none; }
.zone-h em { font-style: normal; font-size: 10pt; font-weight: 500; color: var(--faint); background: rgba(233,238,245,.08); border-radius: 99pt; padding: 1pt 6pt; }
.zone-h small { font-size: 10pt; font-weight: 500; color: var(--faintest); margin-left: 2pt; }
.eye-wrap { overflow: hidden; }
.eye { position: relative; display: flex; align-items: center; gap: 9pt; padding: 7pt 10pt; border-radius: 8pt; white-space: nowrap; }
.eye .hl { position: absolute; inset: 0; border-radius: 8pt; background: rgba(78,155,234,.1); }
.eye .done { position: absolute; inset: 0; border-radius: 8pt; background: rgba(79,192,138,.16); box-shadow: inset 0 0 0 .75pt rgba(79,192,138,.45); }
.eye.overdue::before { content: ""; position: absolute; left: 0; top: 5pt; bottom: 5pt; width: 2pt; border-radius: 1pt; background: var(--danger); z-index: 1; }
.avatar { position: relative; width: 22pt; height: 22pt; border-radius: 50%; flex: none; display: grid; place-items: center; font-size: 9pt; font-weight: 600; }
.avatar .ck { position: absolute; inset: 0; border-radius: 50%; background: var(--positive); color: #0b2418; display: grid; place-items: center; }
.avatar .ck svg { width: 12pt; height: 12pt; }
.eye b { position: relative; font-weight: 500; flex: none; }
.eye span.line { position: relative; color: var(--dim); overflow: hidden; text-overflow: ellipsis; flex: 1; min-width: 0; }
.chip { position: relative; flex: none; font-size: 10pt; font-weight: 500; padding: 2.5pt 7pt; border-radius: 99pt; color: var(--c);
  box-shadow: inset 0 0 0 .75pt color-mix(in srgb, var(--c) 30%, transparent); }
.chip.filled { background: color-mix(in srgb, var(--c) 16%, transparent); }
.reading { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: 8pt; }
.rd { display: flex; gap: 10pt; padding: 8pt; border-radius: 12pt; background: #283647; min-width: 0; }
.logo { width: 46pt; height: 46pt; border-radius: 8pt; flex: none; display: grid; place-items: center; font-weight: 700; font-size: 17pt; }
.rd-text { min-width: 0; flex: 1; display: flex; flex-direction: column; gap: 3pt; }
.rd-top { display: flex; justify-content: space-between; gap: 6pt; }
.rd-top b { font-weight: 600; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.rd-top span { font-size: 10pt; color: var(--faintest); white-space: nowrap; }
.rd p { margin: 0; font-size: 10.5pt; line-height: 1.3; color: var(--faint); display: -webkit-box; -webkit-line-clamp: 2; -webkit-box-orient: vertical; overflow: hidden; }
.rail { display: flex; flex-direction: column; gap: 12pt; min-width: 0; }
.rec { border-radius: 16pt; padding: 11pt 11pt 9pt; background: var(--bg-c); box-shadow: inset 0 0 0 .75pt var(--edge); }
.rec .zone-h { padding: 0 3pt 5pt; }
.rec-cal { --bg-c: #253341; --edge: #3b5673; --zone: var(--accent); }
.rec-ship { --bg-c: #35332d; --edge: #6a5a3e; --zone: var(--warn); }
.rec-bill { --bg-c: #2a3540; --edge: #3f566d; --zone: var(--accent); }
.rec-rcpt { --bg-c: #233635; --edge: #2f5a49; --zone: var(--positive); }
.rec-row { display: flex; justify-content: space-between; align-items: center; gap: 8pt; padding: 4.5pt 3pt; font-size: 11.5pt; white-space: nowrap; }
.rec-row b { font-weight: 400; color: #cdd5df; overflow: hidden; text-overflow: ellipsis; }
.rec-row span { font-size: 10pt; color: var(--faintest); font-variant-numeric: tabular-nums; }
.rec-row .chip { --c: var(--faint); }
.rec-rcpt .rec-row span { font-size: 11.5pt; font-weight: 600; color: var(--positive); }
.ship { background: #3f3d38; border-radius: 10pt; padding: 7pt 8pt; margin-bottom: 6pt; display: flex; flex-direction: column; gap: 6pt; }
.ship-top { display: flex; align-items: center; gap: 7pt; white-space: nowrap; }
.ship-top i { width: 14pt; height: 14pt; border-radius: 3pt; flex: none; background: #5a4631; display: grid; place-items: center; font-style: normal; font-size: 8pt; font-weight: 700; color: #f4c47a; }
.ship-top b { font-weight: 600; flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; }
.ship .chip.eta { --c: var(--faint); align-self: flex-start; }

.title { left: 0; right: 0; text-align: center; top: 72px; }
.h2 { margin: 0; font-family: var(--serif); font-weight: 500; font-size: 80px; line-height: 1.04; letter-spacing: -0.012em; }
.foot-scrim { left: 0; right: 0; bottom: 0; height: 420px;
  background: linear-gradient(180deg, rgba(9,13,22,0), rgba(9,13,22,.88) 55%, rgba(9,13,22,.96)); }
.cap { left: 120px; bottom: 96px; width: 1200px; display: flex; flex-direction: column; gap: 16px; }
.cap .kicker { color: var(--accent); }
.cap p { margin: 0; font-family: var(--serif); font-weight: 500; font-size: 64px; line-height: 1.04; letter-spacing: -0.01em; }

/* A keycap, machined out of the same material as the site's rig. */
.key { right: 150px; bottom: 96px; width: 128px; height: 128px; border-radius: 26px; display: grid; place-items: center;
  font: 500 58px/1 var(--mono); color: #dfe8f3;
  background: linear-gradient(180deg, #1a2638, #0c121c); border: 1px solid rgba(78,155,234,.3); }
.key::before { content: ""; position: absolute; inset: 0 0 auto; height: 1px; opacity: .6;
  background: linear-gradient(90deg, transparent, rgba(78,155,234,.9), transparent); }
.key small { position: absolute; bottom: -44px; left: 50%; transform: translateX(-50%); white-space: nowrap;
  font: 500 16px/1 var(--mono); letter-spacing: .1em; color: var(--faint); }

.rig { left: 50%; top: 330px; width: 980px; margin-left: -490px; border-radius: 22px; overflow: hidden;
  background: linear-gradient(180deg, #152032, #0c121c); border: 1px solid rgba(78,155,234,.6);
  box-shadow: inset 0 1px 0 rgba(255,255,255,.12), 0 0 90px -10px rgba(78,155,234,.5), 0 40px 90px rgba(0,0,0,.6); }
.rig::before { content: ""; position: absolute; inset: 0 0 auto; height: 1px; background: linear-gradient(90deg, transparent, rgba(78,155,234,.9), transparent); }
.rig-h { display: flex; align-items: center; gap: 14px; padding: 26px 32px 22px; border-bottom: 1px solid rgba(78,155,234,.3); }
.rig-h .avatar { width: 40px; height: 40px; font-size: 16px; }
.rig-h b { font-size: 24px; font-weight: 600; }
.rig-h .kicker { margin-left: auto; color: var(--accent); }
.rig-in { position: relative; padding: 34px 32px 38px; font-size: 38px; line-height: 1.2; color: var(--ink); min-height: 46px; }
.rig-in .ph { color: var(--faintest); }
.caret { display: inline-block; width: 3px; height: 42px; vertical-align: -7px; margin-left: 2px; background: var(--accent-ink); }
.rig-f { display: flex; align-items: center; gap: 12px; padding: 0 32px 28px; font-size: 20px; color: var(--faint); }
.rig-f .ok { color: var(--positive); font-weight: 600; }
.rig-f svg { width: 22px; height: 22px; }

.facts-h { left: 0; right: 0; top: 190px; text-align: center; }
.facts { left: 170px; right: 170px; top: 430px; display: grid; grid-template-columns: repeat(3, 1fr); gap: 90px;
  border-top: 1px solid var(--hair-strong); padding-top: 56px; }
.fact .kicker { color: var(--accent); margin-bottom: 22px; }
.fact h3 { margin: 0 0 16px; font-family: var(--serif); font-weight: 500; font-size: 46px; letter-spacing: -0.01em; }
.fact p { margin: 0; color: var(--dim); font-size: 24px; line-height: 1.55; }

.end { left: 0; right: 0; top: 150px; display: flex; flex-direction: column; align-items: center; text-align: center; }
.end .mark { width: 190px; margin-bottom: 26px; }
.end .word { font-family: var(--serif); font-size: 120px; font-weight: 500; letter-spacing: -0.012em; line-height: 1; }
.end .tag { margin-top: 30px; font-family: var(--serif); font-size: 50px; color: var(--dim); }
.cta { margin-top: 52px; position: relative; display: inline-flex; align-items: center; gap: 14px; padding: 22px 50px 40px;
  border-radius: 20px; font-size: 27px; font-weight: 600; letter-spacing: .015em; color: #f4f8fd; overflow: hidden;
  background: linear-gradient(180deg, #152032, #0c121c); border: 1px solid rgba(78,155,234,.62);
  box-shadow: inset 0 1px 0 rgba(255,255,255,.14), 0 0 60px -8px rgba(78,155,234,.5), 0 20px 50px rgba(0,0,0,.55); }
.cta::before { content: ""; position: absolute; inset: 0 0 auto; height: 1px; background: linear-gradient(90deg, transparent, rgba(78,155,234,.9), transparent); }
.cta canvas { position: absolute; inset: 0; width: 100%; height: 100%; }
.cta span, .cta svg { position: relative; }
.url { margin-top: 38px; font: 500 22px/1 var(--mono); letter-spacing: .08em; color: var(--accent-ink); }
.fine { margin-top: 16px; font-size: 20px; color: var(--faintest); }
`;

// ---------------------------------------------------------------- content
// The wall's filler and its buried mail are the site's own FAKE_EMAILS and
// BURIED, so the film and the page tell one story about one mailbox.
const FAKE: Array<[string, string, string]> = [
  ["LinkedIn", "You appeared in 9 searches this week", "See who's looking for someone like you. Recruiters from companies you've never heard of are searching..."],
  ["Medium Daily Digest", "Stories for you", "10 Habits of Highly Effective Engineers | 12 min read. Why I Quit My Job to Farm Mushrooms..."],
  ["no-reply@accounts", "Security alert for your account", "A new sign-in was detected on a device you may or may not recognize..."],
  ["DoorDash", "Your Friday deserves 40% off", "Hungry? Use code FRIYAY at checkout before midnight and get 40% off orders over $35..."],
  ["Confluence", "Weekly digest: 47 updates in spaces you follow", "Q3 Planning Doc was edited by 6 people. Retro Notes (DRAFT) (COPY) was moved..."],
  ["Marriott Bonvoy", "Points update: you have 312 points", "You are 87,688 points away from your next free night at participating..."],
  ["Product Hunt Daily", "The 10 best new products today", "An AI notetaker for your AI notetaker, a smart water bottle that syncs to your calendar..."],
  ["billing@saas.io", "Your receipt from Acme SaaS #48211", "Amount paid: $12.00. Thank you for your continued subscription to a service you forgot..."],
  ["United Airlines", "MileagePlus: miles expiring soon", "Don't lose your 1,204 miles. Book by the end of the month or transfer them..."],
  ["HR Announcements", "REMINDER: Open enrollment closes Friday", "This is your final reminder to complete your benefits elections..."],
  ["Substack", "3 new posts from writers you follow", "The Case Against Breakfast, and other essays. Plus: a 4,000-word post about Lisbon..."],
  ["Zoom", "Your cloud recording is now available", "Meeting recording: Weekly Sync. Duration: 58 minutes. This recording will be deleted..."],
  ["GitHub", "[repo] 23 new notifications", "dependabot opened 14 pull requests in repositories you have not touched since 2023..."],
  ["Eventbrite", "Events near you this weekend", "Networking mixers, fun runs, and a pottery class. Based on your interests..."],
  ["Sephora", "We miss you! Here's 15% off", "It's been a while. Your Beauty Insider points are waiting, and so is 15% off..."],
  ["The Team", "We've updated our Privacy Policy", "We're writing to let you know about some updates to our Privacy Policy and Terms..."],
  ["Slack", "You have unread messages in #general", "While you were away, 312 messages were posted in channels you follow..."],
];
const BURIED: Record<number, [string, string, string]> = {
  17: ["Parkline Properties", "Lease renewal: signature needed", "Your renewal packet is ready. Please review and sign by September 23..."],
  21: ["Dr. Ortiz's Office", "Please confirm Thursday's appointment", "Reply C to confirm your appointment on Thursday at 2:30 PM..."],
  26: ["Jamie Chen", "A quick decision before Friday", "Hey! Two options for the offsite venue, I need your pick by Friday..."],
};
const ROW_H = 64;

const GLYPHS: Record<string, string> = {
  sitrep: "M8 14.2A6.2 6.2 0 1 0 8 1.8a6.2 6.2 0 0 0 0 12.4Zm0-3.6 2.8-5M3.8 8h1M8 3.8v1m4.2 3.2h-1",
  mail: "M2 3.8h12v8.4H2V3.8Zm0 0 6 4.6 6-4.6",
  key: "M8 6.6a2.6 2.6 0 1 0 0-5.2 2.6 2.6 0 0 0 0 5.2Zm0 0v7.8l1.6-1.3M8 10.4h1.6",
  sliders: "M2 4.5h12M2 8h12M2 11.5h12M5 3.3v2.4M10.5 6.8v2.4M6.5 10.3v2.4",
  people: "M6 7.2a2.4 2.4 0 1 0 0-4.8 2.4 2.4 0 0 0 0 4.8Zm-4.2 6.4c.3-2.6 2-4 4.2-4s3.9 1.4 4.2 4M11 7.2a2 2 0 1 0 0-4m1.6 6.6c1 .5 1.6 1.7 1.8 3.8",
  pulse: "M1.5 8.5h3l1.6-4.4 3 8.4 1.8-4h3.6",
  gear: "M8 10.3a2.3 2.3 0 1 0 0-4.6 2.3 2.3 0 0 0 0 4.6ZM8 2v2m0 8v2M2 8h2m8 0h2M3.8 3.8l1.4 1.4m5.6 5.6 1.4 1.4m0-8.4-1.4 1.4m-5.6 5.6-1.4 1.4",
  eye: "M1.5 8S4 3.5 8 3.5 14.5 8 14.5 8 12 12.5 8 12.5 1.5 8 1.5 8Zm6.5 2a2 2 0 1 0 0-4 2 2 0 0 0 0 4Z",
  reading: "M2 6.5 8 2.5l6 4v7H2v-7Zm0 0 6 4 6-4",
  calendar: "M2.5 3.5h11v10h-11v-10Zm0 3h11M5 2v2.5M11 2v2.5",
  box: "M8 1.8 14 4.6v6.8L8 14.2 2 11.4V4.6L8 1.8ZM2 4.6 8 7.4l6-2.8M8 7.4v6.8",
  bank: "M2 6 8 2.5 14 6H2Zm1.2 0v6m3.2-6v6m3.2-6v6M12.8 6v6M2 13.5h12",
  receipt: "M3.5 1.8h9v12.4l-1.5-1-1.5 1-1.5-1-1.5 1-1.5-1-1.5 1V1.8Zm2.5 4h4m-4 3h4",
  retriage: "M13.5 8a5.5 5.5 0 1 1-1.6-3.9M12.5 1.8v2.6H9.9",
  check: "M3.5 8.4 6.6 11.4 12.6 4.8",
};
const g = (name: string, sw = 1.25) =>
  `<svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="${sw}" stroke-linecap="round" stroke-linejoin="round"><path d="${GLYPHS[name]}"/></svg>`;
const zoneHead = (glyph: string, title: string, count: number, sub = "") =>
  `<div class="zone-h">${g(glyph)}${title} <em>${count}</em>${sub ? `<small>${sub}</small>` : ""}</div>`;

const AVATAR: Record<string, [string, string]> = {
  blue: ["#24384F", "#A9CBF0"], green: ["#22392E", "#9AD3B6"], rust: ["#442C26", "#EBA894"],
  violet: ["#2F2A4A", "#B8ACF2"], ochre: ["#3E2E1B", "#E2C078"], rose: ["#3E2839", "#E0A6CD"],
};
const EYES = [
  { who: "Parkline Properties", initials: "PP", tone: "ochre", line: "Your lease renewal still needs a signature before the new rate lapses.", chip: "3d PAST DUE", overdue: true },
  { who: "Dr. Ortiz's Office", initials: "DO", tone: "green", line: "Confirm Thursday's 2:30 appointment or call to reschedule.", chip: "due today" },
  { who: "Jamie Chen", initials: "JC", tone: "blue", line: "Needs your pick between two offsite venues so they can hold the date.", chip: "due Fri" },
  { who: "Alex Rivera", initials: "AR", tone: "violet", line: "Replied to your offsite dates: the 14th works for everyone." },
  { who: "Mom", initials: "M", tone: "rose", line: "Sent photos from the weekend." },
];
const READING: Array<[string, number, string, [string, string, string]]> = [
  ["Medium Daily Digest", 7, "10 Habits of Highly Effective Engineers, and six more stories picked for you.", ["#f2f2ee", "#111", "M"]],
  ["Product Hunt Daily", 5, "An AI notetaker for your AI notetaker, and eight more launches.", ["#3a2a22", "#f08a5d", "P"]],
  ["Substack", 3, "Three new posts from writers you follow.", ["#f3eee6", "#c2531f", "S"]],
];

function appHTML() {
  const eyes = EYES.map(({ who, initials, tone, line, chip, overdue }, i) => {
    const [bg, fg] = AVATAR[tone];
    const c = overdue ? "var(--danger)" : "var(--warn)";
    return `<div class="eye-wrap" data-eye="${i}"><div class="eye${overdue ? " overdue" : ""}">
      <i class="hl"></i><i class="done"></i>
      <span class="avatar" style="background:${bg};color:${fg}">${initials}<i class="ck">${g("check", 2)}</i></span>
      <b>${who}</b><span class="line">${line}</span>
      ${chip ? `<span class="chip${overdue ? " filled" : ""}" style="--c:${c}">${chip}</span>` : ""}
    </div></div>`;
  }).join("");
  const reading = READING.map(([s, n, blurb, [bg, fg, m]]) =>
    `<div class="rd"><span class="logo" style="background:${bg};color:${fg}">${m}</span><div class="rd-text">
      <div class="rd-top"><b>${s}</b><span>${n} emails</span></div><p>${blurb}</p></div></div>`).join("");
  return `<div class="app">
    <div class="app-bar">
      <div class="lights"><i></i><i></i><i></i></div>
      <div class="app-title"><b>passband</b><small>SITREP</small></div>
      <div class="app-tools"><span>${g("retriage")}re-triage 7d</span><span class="need" id="need" style="--c:var(--danger)">2 need you now</span></div>
    </div>
    <nav class="side">
      <span class="on">${g("sitrep")}</span><span>${g("mail")}</span><span>${g("key")}</span><span>${g("sliders")}</span>
      <span>${g("people")}</span><i class="gap"></i><span>${g("pulse")}</span><span>${g("gear")}</span><span class="me">Y</span>
    </nav>
    <div class="page">
      <div class="dash-hero"><small>Good morning</small><div class="lines">
        <b data-hero="0">Two items for your eyes.</b><b data-hero="1">One item for your eyes.</b><b data-hero="2">You’re clear for today.</b>
      </div></div>
      <div class="col">
        <section class="zone" id="fye">${zoneHead("eye", "For your eyes", EYES.length)}${eyes}</section>
        <section class="zone" id="reading">${zoneHead("reading", "Reading", 3, "newsletters, announcements, and offers")}<div class="reading">${reading}</div></section>
      </div>
      <aside class="rail" id="rail">
        <section class="rec rec-cal">${zoneHead("calendar", "Calendar", 2)}
          <div class="rec-row"><b>Dr. Ortiz · checkup</b><span>Sep 26</span></div>
          <div class="rec-row"><b>Team offsite</b><span>Oct 14</span></div></section>
        <section class="rec rec-ship">${zoneHead("box", "Shipments", 2)}
          <div class="ship"><div class="ship-top"><i>UPS</i><b>Keychron Q1</b><span class="chip filled" style="--c:var(--warn)">out for delivery</span></div></div>
          <div class="ship"><div class="ship-top"><i>a</i><b>Linen notebooks</b><span class="chip filled" style="--c:var(--accent)">shipped</span></div><span class="chip eta">arrives Sat</span></div></section>
        <section class="rec rec-bill">${zoneHead("bank", "Billing", 2)}
          <div class="rec-row"><b>Chase ··4417</b><span class="chip">statement</span></div>
          <div class="rec-row"><b>Con Edison</b><span class="chip">update</span></div></section>
        <section class="rec rec-rcpt">${zoneHead("receipt", "Receipts", 3)}
          <div class="rec-row"><b>Blue Bottle</b><span>$6.50</span></div>
          <div class="rec-row"><b>DoorDash</b><span>$23.18</span></div>
          <div class="rec-row"><b>Berghain Klubnacht</b><span>€25.00</span></div></section>
      </aside>
    </div>
  </div>`;
}

const ARROW = `<svg width="22" height="20" viewBox="0 0 15 14" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"><path d="M1.4 7h11.2m0 0L9.3 3.7M12.6 7 9.3 10.3"/></svg>`;

function wallHTML() {
  // Deterministic, so the render and a preview agree row for row.
  let s = 7;
  const r = () => ((s = (s * 16807) % 2147483647) / 2147483647);
  let out = "";
  let prev = "";
  for (let i = 0; i < 72; i++) {
    let pick = FAKE[Math.floor(r() * FAKE.length)];
    while (pick[0] === prev) pick = FAKE[Math.floor(r() * FAKE.length)];
    const [sender, subject, snippet] = BURIED[i] ?? pick;
    prev = sender;
    const tm = `${1 + Math.floor(r() * 12)}:${String(Math.floor(r() * 60)).padStart(2, "0")}`;
    out += `<div class="row"${BURIED[i] ? ` data-buried` : ""}><i class="bar" style="opacity:0"></i><span class="s">${sender}</span><span class="x"><span>${subject}</span> ${snippet}</span><span class="tm">${tm}</span></div>`;
  }
  return out;
}

// ---------------------------------------------------------------- DOM
document.head.insertAdjacentHTML("beforeend", `<style>${CSS}</style>`);
document.body.insertAdjacentHTML("beforeend", `
  <canvas class="scene"></canvas>
  <div class="grat" id="grat"></div>
  <span class="mkr abs">1</span><span class="mkr abs">2</span><span class="mkr abs">3</span>
  <div class="scrim" id="scrim"></div>
  <div class="veil" id="dim" style="opacity:0"></div>

  <div class="mkrs abs" id="mkrs">
    <div><span>MKR</span><span>SENDER</span><span>STATUS</span></div>
    <div><b>1</b><span>Parkline Properties</span><span class="late">past due</span></div>
    <div><b>2</b><span>Dr. Ortiz's Office</span><span class="soon">today</span></div>
    <div><b>3</b><span>Jamie Chen</span><span class="soon">due Fri</span></div>
  </div>

  <div class="wallport abs" id="wallport"><div class="wall" id="wall"><div class="rows" id="rows">${wallHTML()}</div></div></div>

  <div class="copy abs" id="c1" style="top:340px; width:1200px">
    <p class="lede" id="c1a">Inbox zero every day was never realistic.</p>
    <h1 class="hero" id="c1b" style="margin-top:26px">You’re only human.</h1>
  </div>
  <div class="copy abs" id="c2" style="top:300px; width:620px">
    <div class="kicker" id="c2k">Unread</div>
    <div style="margin-top:22px"><span class="unread" id="count">0</span></div>
    <h2 class="hero" id="c2a" style="font-size:92px; margin-top:54px">4,312 emails.</h2>
    <p class="sub" id="c2b" style="margin-top:18px">Everything, in the order it arrived.</p>
  </div>
  <div class="copy abs" id="c3" style="top:360px">
    <p class="lede" id="c3a">4,312 emails.</p>
    <h1 class="hero" id="c3b" style="margin-top:20px"><em>Three</em> need you.</h1>
  </div>

  <div class="brand abs" id="brand"><img src="mark.svg"><span>passband</span></div>
  <span class="read abs" id="readL" style="left:76px">SPAN&nbsp;&nbsp;your inbox&nbsp;&nbsp;·&nbsp;&nbsp;4,312 msgs</span>
  <span class="read abs" id="readR" style="right:76px">SQUELCH&nbsp;<b id="squelch">0%</b></span>

  <div class="title abs" id="t4"><h2 class="h2">Know what needs you.</h2></div>
  <div class="stage3d abs"><div class="win" id="win">${appHTML()}</div></div>
  <div class="foot-scrim abs" id="fscrim"></div>
  <div class="cap abs" id="capA"><span class="kicker">For your eyes</span><p>The few that need you. First.</p></div>
  <div class="cap abs" id="capB"><span class="kicker">Keyboard first</span><p>Done in a keystroke.</p></div>
  <div class="cap abs" id="capC"><span class="kicker">Smart rules</span><p>Tell it what you care about.<br>In plain English.</p></div>
  <div class="key abs" id="keyE">e<small>mark done</small></div>
  <div class="key abs" id="keyT">t<small>write a rule</small></div>

  <div class="rig abs" id="rig">
    <div class="rig-h"><span class="avatar" style="background:#3E2839;color:#E0A6CD">S</span><b>Sephora</b><span class="kicker">Smart rule</span></div>
    <div class="rig-in"><span id="typed"></span><i class="caret" id="caret"></i><span class="ph" id="ph">Describe what you want from this sender</span></div>
    <div class="rig-f" id="rigHint">Try “Always show me product updates.”</div>
    <div class="rig-f" id="rigOk" style="display:none"><span class="ok">${g("check", 2)}</span><span><span class="ok">Saved.</span> Sephora only gets through at 25% off or more.</span></div>
  </div>

  <div class="facts-h abs" id="fh"><h2 class="h2">Your mail stays <em>yours.</em></h2></div>
  <div class="facts abs" id="facts">
    <div class="fact"><div class="kicker">01</div><h3>Open source</h3><p>MIT. Every line on GitHub.</p></div>
    <div class="fact"><div class="kicker">02</div><h3>Run it where you like</h3><p>Your machine, or ours.</p></div>
    <div class="fact"><div class="kicker">03</div><h3>Reading can’t send</h3><p>Sync is read-only.</p></div>
  </div>

  <div class="end abs" id="end">
    <img class="mark" src="mark.svg">
    <div class="word">passband</div>
    <div class="tag">Find a little breathing room.</div>
    <div class="cta" id="cta"><canvas id="meter"></canvas><span>join the waitlist</span>${ARROW}</div>
    <div class="url">passband.app</div>
    <div class="fine">For Gmail, on Mac and iPhone.</div>
  </div>

  <div class="veil" id="fade"></div>
`);

const $ = <E extends HTMLElement = HTMLElement>(id: string) => document.getElementById(id) as E;
const set = (el: HTMLElement, opacity: number, transform = "") => {
  el.style.opacity = String(opacity);
  el.style.transform = transform;
  el.style.visibility = opacity <= 0.001 ? "hidden" : "visible";
};

// ---------------------------------------------------------------- the analyzer
// Wide on the right for the hero beats (the site's desktop layout), then low
// and centred for the close (the social card's), drifting between the two
// while the app covers it.
const HERO: Layout = { center: 0.66, base: 0.8, scale: 0.5, spread: 1 };
const CLOSE: Layout = { center: 0.5, base: 0.96, scale: 0.22, spread: 1.4 };
const layoutAt = (t: number): Layout => {
  const k = inOut(lin(t, T.appIn + 3, T.factsIn - 1));
  return {
    center: mix(HERO.center, CLOSE.center, k), base: mix(HERO.base, CLOSE.base, k),
    scale: mix(HERO.scale, CLOSE.scale, k), spread: mix(HERO.spread, CLOSE.spread, k),
  };
};
let analyzer: ReturnType<typeof createAnalyzer>;

// The CTA's meter, the site's useMeter closed on its passband. Pure in t.
const meter = $<HTMLCanvasElement>("meter");
const mctx = meter.getContext("2d")!;
const mPhase = Array.from({ length: 96 }, (_, i) => (i * 2.399) % (Math.PI * 2));
const mSpeed = Array.from({ length: 96 }, (_, i) => 0.7 + ((i * 0.618) % 1) * 1.9);
function drawMeter(t: number, gate: number) {
  const w = meter.clientWidth, h = meter.clientHeight;
  if (meter.width !== w * 2) { meter.width = w * 2; meter.height = h * 2; }
  mctx.setTransform(2, 0, 0, 2, 0, 0);
  mctx.clearRect(0, 0, w, h);
  const bars = Math.round(w / 6), step = w / bars, bw = Math.max(1.5, step - 2.2), maxH = h * 0.3;
  const band = (x: number) => Math.exp(-Math.pow(Math.abs(x - 0.5) / 0.19, 4));
  for (let i = 0; i < bars; i++) {
    const x = i / (bars - 1);
    const n = 0.5 + 0.5 * Math.sin(t * mSpeed[i] + mPhase[i]) * Math.cos(t * mSpeed[i] * 0.61 + mPhase[i] * 1.7);
    const open = 0.12 + 0.3 * n, filt = 0.04 + 0.96 * band(x) * (0.55 + 0.45 * n);
    const bh = Math.max(1, (open * (1 - gate) + filt * gate) * maxH);
    const warm = gate * band(x);
    const m = (c: number, hot: number) => Math.round(c + (hot - c) * warm);
    mctx.fillStyle = `rgba(${m(128, 130)}, ${m(142, 186)}, ${m(159, 245)}, ${0.4 + 0.55 * warm})`;
    mctx.shadowBlur = warm > 0.25 ? 10 * warm : 0;
    mctx.shadowColor = `rgba(78,155,234,${0.7 * warm})`;
    mctx.beginPath();
    mctx.roundRect(i * step + (step - bw) / 2, h - bh, bw, bh, bw / 2);
    mctx.fill();
  }
}

// ---------------------------------------------------------------- measure
// Taken once, with the window untransformed, in the window's own pixels.
const win = $("win");
const eyeWraps = [...document.querySelectorAll<HTMLElement>(".eye-wrap")];
let eyeH: number[] = [];
let rects: Record<string, { x: number; y: number; w: number; h: number }> = {};
function measure() {
  win.style.transform = "none";
  const base = win.getBoundingClientRect();
  const rel = (el: Element) => {
    const r = el.getBoundingClientRect();
    return { x: r.left - base.left, y: r.top - base.top, w: r.width, h: r.height };
  };
  rects = { win: rel(win), fye: rel($("fye")), reading: rel($("reading")), rail: rel($("rail")) };
  eyeH = eyeWraps.map((e) => e.getBoundingClientRect().height);
}

// A camera over the window: window point (fx, fy) lands on screen (sx, sy),
// at scale s.
type Cam = { s: number; fx: number; fy: number; sx: number; sy: number };
const camFull = (): Cam => ({ s: 1, fx: rects.win.w / 2, fy: 0, sx: W / 2, sy: 238 });
const camFye = (): Cam => ({ s: 1.29, fx: rects.win.w / 2, fy: 0, sx: W / 2, sy: 16 });
const camRule = (): Cam => ({ s: 1.12, fx: rects.win.w / 2, fy: rects.reading.y, sx: W / 2, sy: 560 });
const lerpCam = (a: Cam, b: Cam, k: number): Cam => ({
  s: mix(a.s, b.s, k), fx: mix(a.fx, b.fx, k), fy: mix(a.fy, b.fy, k), sx: mix(a.sx, b.sx, k), sy: mix(a.sy, b.sy, k),
});

// ---------------------------------------------------------------- render
const TYPE_AT = typeTimes();
const els = {
  c1: $("c1"), c1a: $("c1a"), c1b: $("c1b"), c2: $("c2"), c2k: $("c2k"), c2a: $("c2a"), c2b: $("c2b"), count: $("count"),
  c3: $("c3"), c3a: $("c3a"), c3b: $("c3b"), brand: $("brand"), readL: $("readL"), readR: $("readR"), squelch: $("squelch"),
  wallport: $("wallport"), wall: $("wall"), rows: $("rows"), mkrs: $("mkrs"), scrim: $("scrim"), dim: $("dim"), grat: $("grat"),
  t4: $("t4"), fscrim: $("fscrim"), capA: $("capA"), capB: $("capB"), capC: $("capC"), keyE: $("keyE"), keyT: $("keyT"),
  rig: $("rig"), typed: $("typed"), caret: $("caret"), ph: $("ph"), rigHint: $("rigHint"), rigOk: $("rigOk"),
  fh: $("fh"), facts: $("facts"), end: $("end"), cta: $("cta"), fade: $("fade"), need: $("need"),
};
const markers = [...document.querySelectorAll<HTMLElement>(".mkr")];
const buried = [...document.querySelectorAll<HTMLElement>(".row[data-buried]")];
const allRows = [...document.querySelectorAll<HTMLElement>(".row")];
const fyeCount = document.querySelector("#fye .zone-h em") as HTMLElement;
const heroLines = [...document.querySelectorAll<HTMLElement>("[data-hero]")];
const facts = [...document.querySelectorAll<HTMLElement>(".fact")];
const rise = (k: number, d = 16) => `translateY(${(1 - k) * d}px)`;

function keyPress(t: number, at: number) {
  // Down fast, back up slower: a real keystroke.
  const down = lin(t, at - 0.06, at) * (1 - lin(t, at + 0.08, at + 0.26));
  const glow = Math.exp(-Math.max(0, t - at) * 3) * (t >= at - 0.06 ? 1 : 0);
  return { down, glow };
}

function render(t: number) {
  // --- the analyzer, and everything laid over it. The squelch is the same
  // curve the score closes its filter on.
  const engage = smooth(lin(t, T.squelchFrom, T.squelchTo));
  const L = layoutAt(t);
  analyzer.step(t, L, engage, 1 / FPS);
  els.squelch.textContent = `${Math.round(engage * 100)}%`;
  markers.forEach((el, i) => {
    const { x, y } = analyzer.peak(i, L);
    el.style.left = `${x * 100}%`;
    el.style.top = `${y * 100}%`;
    const k = window_(t, T.markers + i * 0.14, T.scopeOut - 0.4, 0.35);
    set(el, k, `translate(-50%, calc(-100% - 16px - ${(1 - k) * 10}px))`);
  });
  const tbl = window_(t, T.markers + 0.45, T.scopeOut - 0.3, 0.6);
  set(els.mkrs, tbl, rise(tbl, 10));

  // The scene recedes while the app is up and comes back for the close.
  const appUp = smooth(lin(t, T.scopeOut - 0.4, T.appIn + 0.8)) * (1 - smooth(lin(t, T.appOut, T.factsIn + 0.6)));
  els.dim.style.opacity = String(0.82 * appUp + 0.3 * smooth(lin(t, T.appOut, T.factsIn + 0.6)));
  els.scrim.style.opacity = String(1 - smooth(lin(t, T.appOut, T.factsIn)));
  els.grat.style.opacity = String(1 - 0.5 * smooth(lin(t, T.appOut, T.factsIn)));

  // Brand and readouts ride the analyzer beats only.
  set(els.brand, window_(t, 0.5, T.scopeOut - 0.4, 1.0));
  const rd = window_(t, 1.0, T.scopeOut - 0.4, 1.0);
  set(els.readL, rd);
  set(els.readR, rd * (1 - window_(t, T.wallIn - 0.3, T.wallOut, 0.5)));

  // --- 1. cold open
  const out1 = 1 - smooth(lin(t, T.openOut, T.openOut + 0.5));
  const a = outCubic(lin(t, T.lede, T.lede + 1.0)), b = outCubic(lin(t, T.human, T.human + 1.0));
  set(els.c1, out1, `translateY(${-smooth(lin(t, T.openOut, T.openOut + 0.5)) * 20}px)`);
  set(els.c1a, a, rise(a)); set(els.c1b, b, rise(b, 22));

  // --- 2. the wall
  const inW = outCubic(lin(t, T.wallIn, T.wallIn + 1.1));
  const outW = smooth(lin(t, T.wallOut, T.wallOut + 0.8));
  set(els.wallport, inW * (1 - outW));
  els.wall.style.transform =
    `rotateY(${mix(-34, -22, inW) - 8 * outW}deg) rotateX(4deg) translateX(${(1 - inW) * 260 + outW * 120}px) translateZ(${-outW * 300}px)`;
  const s = t - T.wallIn;
  let off = 0;
  for (const [st, dur, dist] of FLICKS) off += dist * outCubic(lin(s, st, st + dur));
  els.rows.style.transform = `translateY(${-off}px)`;
  const glint = smooth(lin(t, T.glint, T.glint + 0.5));
  for (const row of allRows) row.style.opacity = String(1 - 0.72 * glint);
  for (const row of buried) {
    row.style.opacity = "1";
    row.style.background = `rgba(78,155,234,${0.12 * glint})`;
    (row.querySelector(".bar") as HTMLElement).style.opacity = String(glint);
    (row.querySelector(".s") as HTMLElement).style.color = glint > 0.01 ? `color-mix(in srgb, #82BAF5 ${glint * 100}%, #b9c2cd)` : "";
  }
  els.wall.style.filter = `saturate(${0.25 + 0.75 * glint}) brightness(${0.82 + 0.18 * glint})`;

  const out2 = 1 - smooth(lin(t, T.wallOut, T.wallOut + 0.6));
  set(els.c2, out2);
  const kc = outCubic(lin(t, T.wallIn + 0.2, T.wallIn + 1.0));
  set(els.c2k, kc, rise(kc)); set(els.count.parentElement!, kc, rise(kc));
  const n = Math.round(4312 * outExpo(lin(t, T.countFrom, T.countTo)));
  els.count.textContent = n.toLocaleString("en-US");
  const ca = outCubic(lin(t, T.wallCaption, T.wallCaption + 1.0)), cb = outCubic(lin(t, T.wallCaption + 0.5, T.wallCaption + 1.5));
  set(els.c2a, ca, rise(ca, 22)); set(els.c2b, cb, rise(cb));

  // --- 3. the squelch
  const out3 = 1 - smooth(lin(t, T.scopeOut - 0.5, T.scopeOut + 0.1));
  set(els.c3, out3);
  const l3 = outCubic(lin(t, T.squelchFrom + 0.3, T.squelchFrom + 1.3)), h3 = outCubic(lin(t, T.threeNeed, T.threeNeed + 1.0));
  set(els.c3a, l3, rise(l3)); set(els.c3b, h3, rise(h3, 22));

  // --- 4-6. the app
  const winIn = outCubic(lin(t, T.appIn, T.appIn + 1.6));
  const winOut = inOut(lin(t, T.appOut, T.appOut + 1.0));
  let cam = camFull();
  cam = lerpCam(cam, camFye(), inOut(lin(t, T.zoomFye, T.zoomFye + 1.6)));
  cam = lerpCam(cam, camRule(), inOut(lin(t, T.ruleKey - 0.2, T.ruleOpen + 0.8)));
  // Scaled about the window's origin, with the camera's focus folded into the
  // translation.
  const sc = cam.s * (0.94 + 0.06 * winIn) * (1 - 0.06 * winOut);
  win.style.transform =
    `translate(${cam.sx - cam.fx * sc}px, ${cam.sy - cam.fy * sc + (1 - winIn) * 300 + winOut * 120}px) scale(${sc}) rotateX(${(1 - winIn) * 24 + winOut * 10}deg)`;
  win.style.opacity = String(winIn * (1 - winOut));
  win.style.visibility = winIn * (1 - winOut) > 0 ? "visible" : "hidden";
  // Dim the app under the rule palette, never the palette.
  const ruleUp = window_(t, T.ruleOpen, T.appOut - 0.6, 0.5);
  win.style.filter = ruleUp > 0.001 ? `brightness(${1 - 0.5 * ruleUp})` : "";

  const t4 = window_(t, T.appTitle, T.zoomFye - 0.2, 0.9);
  set(els.t4, t4, rise(t4, 14));
  const capBase = smooth(lin(t, T.zoomFye + 0.4, T.zoomFye + 1.2)) * (1 - smooth(lin(t, T.appOut - 0.5, T.appOut)));
  set(els.fscrim, capBase);
  const cA = window_(t, T.zoomFye + 0.8, T.key1 - 0.9, 0.6);
  const cB = window_(t, T.key1 - 0.4, T.ruleKey - 0.7, 0.6);
  const cC = window_(t, T.ruleOpen + 0.1, T.appOut - 0.6, 0.6);
  set(els.capA, cA, rise(cA)); set(els.capB, cB, rise(cB)); set(els.capC, cC, rise(cC));

  const kE = window_(t, T.key1 - 0.7, T.key2 + 0.7, 0.4);
  const p1 = keyPress(t, T.key1), p2 = keyPress(t, T.key2);
  const eDown = Math.max(p1.down, p2.down), eGlow = Math.max(p1.glow, p2.glow);
  pressKey(els.keyE, kE, eDown, eGlow);
  const kT = window_(t, T.ruleKey - 0.6, T.ruleOpen + 0.5, 0.4);
  const pt = keyPress(t, T.ruleKey);
  pressKey(els.keyT, kT, pt.down, pt.glow);

  // Resolving rows: flash done, then fold away.
  const done = [T.key1, T.key2];
  eyeWraps.forEach((wrap, i) => {
    const at = done[i];
    const eye = wrap.firstElementChild as HTMLElement;
    const hl = eye.querySelector(".hl") as HTMLElement, dn = eye.querySelector(".done") as HTMLElement;
    const ck = eye.querySelector(".ck") as HTMLElement;
    const flash = at === undefined ? 0 : smooth(lin(t, at, at + 0.12));
    const fold = at === undefined ? 0 : inOut(lin(t, at + 0.42, at + 0.9));
    dn.style.opacity = String(flash);
    ck.style.opacity = String(flash);
    ck.style.transform = `scale(${0.6 + 0.4 * outCubic(lin(t, at ?? 0, (at ?? 0) + 0.2))})`;
    wrap.style.height = `${eyeH[i] * (1 - fold)}px`;
    eye.style.opacity = String(1 - smooth(lin(t, (at ?? 1e9) + 0.3, (at ?? 1e9) + 0.7)));
    // The cursor sits on the top live row.
    const cursorRow = t < T.key1 + 0.6 ? 0 : t < T.key2 + 0.6 ? 1 : 2;
    const cursorOn = smooth(lin(t, T.zoomFye + 1.2, T.zoomFye + 1.6));
    hl.style.opacity = String(i === cursorRow ? cursorOn : 0);
  });
  // Header count and the hero line follow the resolutions.
  const stage = t < T.key1 + 0.35 ? 0 : t < T.key2 + 0.35 ? 1 : 2;
  fyeCount.textContent = String(5 - stage);
  els.need.textContent = ["2 need you now", "1 needs you now", "all clear"][stage];
  els.need.style.setProperty("--c", stage === 2 ? "var(--positive)" : "var(--danger)");
  heroLines.forEach((el, i) => {
    const at = i === 0 ? -1 : done[i - 1] + 0.35;
    const next = i < 2 ? done[i] + 0.35 : 1e9;
    const k = (i === 0 ? 1 : smooth(lin(t, at, at + 0.4))) * (1 - smooth(lin(t, next, next + 0.3)));
    set(el, k, `translateY(${(1 - k) * (t < next ? 10 : -10)}px)`);
  });

  // The rule palette.
  const rig = window_(t, T.ruleOpen, T.appOut - 0.7, 0.45);
  set(els.rig, rig, `translateY(${(1 - rig) * 24}px) scale(${0.97 + 0.03 * rig})`);
  const typedN = TYPE_AT.filter((at) => t >= at).length;
  els.typed.textContent = RULE_TEXT.slice(0, typedN);
  els.ph.style.display = typedN ? "none" : "";
  const blinkOn = t < T.typeTo + 0.2 && t > T.typeFrom - 0.1 ? true : Math.floor(t * 2.2) % 2 === 0;
  els.caret.style.opacity = t < T.ruleSave ? (blinkOn ? "1" : "0") : "0";
  const saved = t >= T.ruleSave;
  els.rigHint.style.display = saved ? "none" : "";
  els.rigOk.style.display = saved ? "" : "none";
  els.rigOk.style.opacity = String(smooth(lin(t, T.ruleSave, T.ruleSave + 0.3)));

  // --- 7. facts
  const fh = window_(t, T.factsIn, T.factsOut, 0.9);
  set(els.fh, fh, rise(fh, 18));
  const fo = 1 - smooth(lin(t, T.factsOut, T.factsOut + 0.7));
  set(els.facts, fo * smooth(lin(t, T.factsIn + 0.3, T.factsIn + 0.9)));
  facts.forEach((el, i) => {
    const k = outCubic(lin(t, T.factsIn + 0.6 + i * 0.35, T.factsIn + 1.5 + i * 0.35));
    set(el, k, rise(k, 20));
  });

  // --- 8. close
  const e = outCubic(lin(t, T.endIn, T.endIn + 1.4));
  set(els.end, e, rise(e, 18));
  const c = outCubic(lin(t, T.endCta, T.endCta + 0.9));
  els.cta.style.opacity = String(c);
  els.cta.style.transform = rise(c, 12);
  if (t > T.endIn) drawMeter(t, smooth(lin(t, T.endCta + 0.6, T.endCta + 2.0)));

  // --- in from and out to night
  els.fade.style.opacity = String(Math.max(1 - smooth(lin(t, 0, 1.4)), smooth(lin(t, T.fadeOut, DURATION - 0.2))));
}

function pressKey(el: HTMLElement, vis: number, down: number, glow: number) {
  set(el, vis, `translateY(${(1 - vis) * 18 + down * 7}px) scale(${1 - down * 0.03})`);
  el.style.borderColor = `rgba(78,155,234,${0.3 + 0.55 * glow})`;
  el.style.boxShadow =
    `inset 0 1px 0 rgba(255,255,255,${0.08 + 0.1 * glow}), 0 0 ${30 + 50 * glow}px -6px rgba(78,155,234,${0.2 + 0.5 * glow}), ` +
    `0 ${14 - down * 10}px 30px rgba(0,0,0,.55), 0 ${8 - down * 6}px 0 #070b12`;
}

// ---------------------------------------------------------------- clock
const w = window as any;
let lastFrame = -1;
w.seek = (frame: number) => {
  // The analyzer's persistence carries frame to frame, so order matters.
  if (frame !== lastFrame + 1) throw new Error(`frames must run in order: ${lastFrame} -> ${frame}`);
  lastFrame = frame;
  render(frame / FPS);
};
w.boot = async () => {
  await document.fonts.ready;
  await Promise.all([...document.images].map((i) => i.decode().catch(() => {})));
  const { samples, sr } = decodeWav(await (await fetch("score.wav")).arrayBuffer());
  analyzer = createAnalyzer(document.querySelector("canvas.scene")!, samples, sr);
  measure();
  return true;
};
