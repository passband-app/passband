#!/usr/bin/env bun
// Publishes docs/SECURITY.md as passband.app/security.
//
//   cd passband-site && bun tools/security-page.ts
//
// Rerun it whenever docs/SECURITY.md changes, then look at the page and commit
// security.html. The doc's text is the source of truth and is kept whole; this
// script only adds what makes it readable on the web:
//   - a plain-English summary under each section heading (SUMMARIES below),
//   - Invariant / Enforcement / Do not break as labeled blocks,
//   - the densest audit detail folded behind a "Show" toggle,
//   - one diagram per section (VISUALS below), and the contents sidebar.
// A section with no summary or diagram still publishes; the script warns so a
// new section gets one. Page chrome (head, styles, hero, footer) lives in
// security.html itself and is left alone between the markers it rewrites.
import { marked } from "marked";
import { join } from "path";

const SITE = join(import.meta.dir, "..");
const DOC = join(SITE, "../docs/SECURITY.md");
const PAGE = join(SITE, "security.html");

const warn = (msg: string) => console.warn(`warning: ${msg}`);

function slug(s: string): string {
  return s
    .replace(/<[^>]+>/g, "")
    .replace(/&[a-z#0-9]+;/g, "")
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-|-$/g, "");
}

// ---------------------------------------------------------------- copy

// Keyed by section number. One or two sentences a non-engineer can follow.
// No em dashes (site copy rule).
const SUMMARIES: Record<string, string> = {
  "1": "Every incoming email is scrubbed once, the moment it arrives, so nothing that could run code is ever stored.",
  "2": "Even scrubbed mail opens inside a locked-down viewer. There are five separate locks, and any one of them would stop scripts, network calls and navigation on its own.",
  "3": "Images can load, but only through Passband&#39;s own fetcher: no cookies, no referrer. Tracking pixels are removed unless the sender is someone you write to and Gmail vouches for them.",
  "4": "You can read all of your mail. AI agents, the built-in assistant included, cannot read login codes or reset links, and cannot read anything that has not been checked yet.",
  "4b": "Spam is kept for you to look at, and away from everything else: no model reads it, no agent sees it, and it never notifies you.",
  "4c": "Agents can leave drafts by default and can send only if you turn that on. Everything an agent writes goes through the same pipeline and checks as your own mail, with fewer options.",
  "4d": "On hosted, your mail text leaves your own instance for exactly two of our services: the AI gateway that triages it, and the search embedder, which never sees spam or login codes.",
  "5": "Before mail leaves, Passband looks for anything shaped like a secret. If it finds one it stops and asks, and it never copies the secret anywhere.",
  "6": "The assistant sees only what the app is allowed to show it, and anything that would change your Gmail waits for your tap.",
  "7": "Passband follows an unsubscribe link only after you ask, only for authenticated mail, only out to the public internet, and presses at most one button.",
  "8": "On hosted, only you can use your mailbox&#39;s agent door, with a sign-in that opens nothing else. If the setup is incomplete the door stays shut rather than falling open.",
  "9": "Your calendar is shown to you and nobody else: no agents, no logs, no triage. Two narrow, documented exceptions let a small model shorten a place name and plan reminders.",
};

// Each diagram is placed before the first match of `before` inside its section
// (or at the section's end when that is not found, with a warning).
type Visual = { before: string; html: string };

const a = (n: string, text: string) => `<a href="#s${n}">${text}</a>`;
const arr = '<i class="arr" aria-hidden="true"></i>';

const MAP = `<figure class="viz map" aria-label="How mail moves through Passband">
<p class="viz-title">How mail moves, and where each rule sits</p>
<div class="map-in">
  <div class="mnode src"><b>Gmail</b><span>Mail arrives</span></div>
  ${arr}
  <div class="mnode"><span class="tag">Ingest</span><b>Checked once, on arrival</b>
    <ul><li>${a("1", "&sect;1 Scrub the HTML")}</li><li>${a("3", "&sect;3 Record Gmail&#39;s auth verdict")}</li><li>${a("4b", "&sect;4b Hold spam back")}</li></ul></div>
  ${arr}
  <div class="mnode"><span class="tag">Store</span><b>squelchd</b><span>Stored already scrubbed</span>
    <ul><li>${a("4d", "&sect;4d Hosted: two services see text")}</li></ul></div>
</div>
<div class="map-split" aria-hidden="true"><i></i></div>
<div class="map-doors">
  <div class="mnode door"><span class="tag">Human door</span><b>You, in the Passband app</b>
    <ul><li>${a("2", "&sect;2 Locked-down viewer")}</li><li>${a("3", "&sect;3 Images through our proxy")}</li><li>${a("9", "&sect;9 Your calendar")}</li></ul></div>
  <div class="mnode door agent"><span class="tag">Agent door</span><b>The assistant and MCP agents</b>
    <ul><li>${a("4", "&sect;4 No codes, nothing unchecked")}</li><li>${a("4c", "&sect;4c Drafts by default, send if allowed")}</li><li>${a("6", "&sect;6 Gmail changes wait for your tap")}</li><li>${a("8", "&sect;8 Only the owner gets in")}</li></ul></div>
</div>
<div class="map-out">
  <span class="tag">Outgoing</span>
  <span class="chip">Your reply or forward</span>${arr}
  <span class="chip hl">${a("5", "&sect;5 Secret guard")}</span>${arr}
  <span class="chip">Gmail</span>
  <span class="chip">${a("7", "&sect;7 Unsubscribe, when you ask")}</span>
</div>
</figure>`;

const VISUALS: Record<string, Visual> = {
  "2": {
    before: '<p class="lbl">Enforcement</p>',
    html: `<figure class="viz layers" aria-label="Five rendering layers">
<p class="viz-title">Five locks around every email body, each enough on its own</p>
<div class="ly"><p><b>5</b> Throwaway storage <span>No cookies, nothing on disk, an opaque origin</span></p>
<div class="ly"><p><b>4</b> Navigation refused <span>Only loads we started; links open outside; no popups</span></p>
<div class="ly"><p><b>3</b> Content Security Policy <span>No script source, no http or https; images only via the proxy</span></p>
<div class="ly"><p><b>2</b> JavaScript off <span>Page script cannot run at all</span></p>
<div class="ly"><p><b>1</b> Scrubbed at ingest <span>&sect;1: no scripts, handlers, forms or frames stored</span></p>
<div class="core">The email body</div>
</div></div></div></div></div>
</figure>`,
  },
  "3": {
    before: "<p>Outbound read tracking is the mirror image",
    html: `<figure class="viz gate" aria-label="When tracking pixels survive">
<p class="viz-title">When tracking pixels survive: both halves, or nothing</p>
<div class="gate-grid" role="table">
<div class="gh" role="columnheader"></div>
<div class="gh" role="columnheader">Gmail auth <b>pass</b><code>Some(true)</code></div>
<div class="gh" role="columnheader">Gmail auth <b>fail</b><code>Some(false)</code></div>
<div class="gh" role="columnheader">Never checked<code>NULL</code></div>
<div class="rh" role="rowheader">Sender you have written to</div>
<div class="cell keep">Pixels kept</div><div class="cell">Stripped</div><div class="cell">Stripped</div>
<div class="rh" role="rowheader">Anyone else</div>
<div class="cell">Stripped</div><div class="cell">Stripped</div><div class="cell">Stripped</div>
</div>
<figcaption>Kept pixels still need remote images switched on, and still load only through the proxy.</figcaption>
</figure>
<figure class="viz pipe" aria-label="Image rewrite order">
<p class="viz-title">What happens to an image, in this exact order</p>
<ol class="steps-row">
<li><b>Strip trackers</b><span>Tiny, hidden or known pixels</span></li>
<li><b>Dedupe repeats</b><span>Same image, once</span></li>
<li><b>Read links</b><span>Before anything is rewritten</span></li>
<li><b>Resolve cid:</b><span>Only to this message&#39;s own parts</span></li>
<li class="hl"><b>Rewrite to proxy</b><span>Always last; signed per launch</span></li>
</ol>
<div class="pipe-out"><span class="chip">passband-img: handler checks the signature</span>${arr}<span class="chip">Fetch with no cookies, no referrer, image/* only</span></div>
</figure>`,
  },
  "4": {
    before: "<p><strong>Assessment.</strong>",
    html: `<figure class="viz access" aria-label="Who can read what">
<p class="viz-title">Who can read what</p>
<div class="table-wrap"><table>
<thead><tr><th></th><th>Everyday mail</th><th>Login and security alerts</th><th>Codes and reset links</th><th>Not checked yet</th></tr></thead>
<tbody>
<tr><td>You, in the app</td><td class="y">Yes</td><td class="y">Yes</td><td class="y">Yes</td><td class="y">Yes</td></tr>
<tr><td>Internal triage models</td><td class="y">Yes</td><td class="y">Yes</td><td class="y">Yes</td><td class="y">Yes</td></tr>
<tr><td>The assistant and MCP agents</td><td class="y">Yes</td><td class="m">Normally</td><td class="n">Never</td><td class="n">No, until checked</td></tr>
</tbody></table></div>
<figcaption>Old archive mail counts as not checked yet: to an agent it does not exist until a check clears it.</figcaption>
</figure>`,
  },
  "4b": {
    before: "<p><strong>Why it is a security rule",
    html: `<figure class="viz spam" aria-label="Where spam can go">
<p class="viz-title">Where provider spam can go</p>
<ul class="dest">
<li class="ok"><b>The spam page</b><span>Fetched only when you open it</span></li>
<li><b>Triage models</b><span>Never scored</span></li>
<li><b>Embeddings</b><span>Never embedded</span></li>
<li><b>Agent door</b><span>Not found, by design</span></li>
<li><b>Notifications</b><span>Never fires</span></li>
<li><b>Lists, counts, search</b><span>Filtered out in SQL</span></li>
</ul>
</figure>`,
  },
  "4c": {
    before: "<p><strong>The setting.</strong>",
    html: `<figure class="viz ladder" aria-label="Agent write levels">
<p class="viz-title">What an agent may write, set per mailbox by you</p>
<ol class="rungs">
<li><span class="tag">none</span><b>Read only</b><span>No drafts, no sending</span></li>
<li class="def"><span class="tag">drafts &middot; the default</span><b>Leave drafts for you</b><span>Marked &ldquo;from your agent&rdquo;; mail leaves only when you press send</span></li>
<li class="stop"><span class="tag">send &middot; opt-in</span><b>Send with your approval</b><span>Same pipeline as yours, plus a secret check with no override</span></li>
</ol>
<div class="kinds"><span>Never for agents</span><code>attachments</code><code>forwards</code><code>send groups</code><code>read tracking</code><code>scheduling</code></div>
<figcaption>An agent cannot change this setting, and a value the daemon cannot read counts as none.</figcaption>
</figure>`,
  },
  "4d": {
    before: '<p><strong>The sealed gate, at the store side.</strong>',
    html: `<figure class="viz hosted" aria-label="Where hosted mail text goes">
<p class="viz-title">Hosted: where mail text goes outside your instance</p>
<div class="h-flow">
<div class="mnode src"><span class="tag">Your instance</span><b>Your squelchd</b><span>One per account</span></div>
<div class="h-lanes">
<div class="h-row">${arr}<div class="mnode"><span class="tag">LLM gateway</span><b>Triage, translation, assistant</b><span>Every non-spam message, login codes included, because judging them is triage&#39;s job</span></div></div>
<div class="h-row">${arr}<div class="mnode"><span class="tag">Embedding service</span><b>Search vectors</b><span>Only mail known not to be sealed; never spam</span></div></div>
<div class="h-row">${arr}<div class="mnode none"><b>Anything else of ours</b><span>Never handed mail text</span></div></div>
</div>
</div>
<div class="kinds"><span>The embedder</span><code>logs no text</code><code>stores nothing</code><code>one request per pass</code><code>no cache</code><code>no tenant name</code><code>no egress</code></div>
<figcaption>Self-host uses neither service: it embeds in-process and calls no process of ours.</figcaption>
</figure>`,
  },
  "5": {
    before: '<p class="lbl">Enforcement</p>',
    html: `<figure class="viz guard" aria-label="Outbound secret guard">
<p class="viz-title">The outbound check, on agent sends</p>
<div class="g-flow">
<div class="mnode"><span class="tag">Scanned</span><b>Everything an agent is about to send</b><span>Recipients, subject and body; links skipped for the long-run checks</span></div>
${arr}
<div class="g-branch">
<div class="g-row"><span class="chip">Nothing found</span>${arr}<span class="chip">Sent</span></div>
<div class="g-row"><span class="chip warn">Looks like a secret</span>${arr}<span class="chip">Refused, no override; told only the kind</span></div>
</div>
</div>
<div class="kinds"><span>Detects</span><code>pem_block</code><code>api_key</code><code>otp_code</code><code>long_hex</code><code>long_base64</code></div>
<figcaption>The matched text itself never leaves the process: not in the response, a log, or the audit row.</figcaption>
</figure>`,
  },
  "6": {
    before: '<p class="lbl">Enforcement</p>',
    html: `<figure class="viz tiers3" aria-label="Assistant tool tiers">
<p class="viz-title">The assistant&#39;s fourteen tools, in three tiers</p>
<div class="t3">
<div class="t"><span class="tag">Reads</span><b>Never gated</b><span>Through the same door the app uses</span></div>
<div class="t"><span class="tag">Safe writes</span><b>This database only</b><span>Status, sender rules, drafts; reversible in the app</span></div>
<div class="t stop"><span class="tag">Gmail writes</span><b>Wait for your tap</b><span>Archive, label, send, unsubscribe</span></div>
</div>
</figure>`,
  },
  "7": {
    before: '<p class="lbl">Enforcement</p>',
    html: `<figure class="viz pipe" aria-label="Automatic unsubscribe checks">
<p class="viz-title">Every check an automatic unsubscribe has to pass</p>
<ol class="steps-row">
<li><b>You asked</b><span>And confirmed; nothing prefetches a link</span></li>
<li><b>Mail we trust</b><span>Not spam, and Gmail&#39;s auth passed</span></li>
<li><b>Sender&#39;s own link</b><span>A footer link must match the From domain</span></li>
<li><b>Public internet</b><span>Never a private or local address</span></li>
<li class="hl"><b>One button</b><span>At most the one a person would press</span></li>
</ol>
<div class="pipe-out"><span class="chip">Any check fails</span>${arr}<span class="chip">You get the link to open in your own browser</span></div>
</figure>`,
  },
  "8": {
    before: "<p><strong>Three modes</strong>",
    html: `<figure class="viz tiers3" aria-label="Agent door modes">
<p class="viz-title">The agent door runs in one of three modes, fixed at startup</p>
<div class="t3">
<div class="t"><span class="tag">Open &middot; self-host</span><b>No sign-in</b><span>Only for a listener whose reach you control</span></div>
<div class="t ok"><span class="tag">OAuth &middot; hosted</span><b>A token on every request</b><span>Bound to your mailbox; cannot open the human door</span></div>
<div class="t stop"><span class="tag">Misconfigured</span><b>Shut</b><span>Answers 503 to everything; never falls back to Open</span></div>
</div>
</figure>`,
  },
  "9": {
    before: '<p class="lbl">Enforcement</p>',
    html: `<figure class="viz spam" aria-label="Where calendar data can go">
<p class="viz-title">Where calendar data can go</p>
<ul class="dest">
<li class="ok"><b>Your timeline in the app</b><span>The human door only</span></li>
<li class="ex"><b>Small model: the place</b><span>The location text alone, cut to the place</span></li>
<li class="ex"><b>Small model: reminders</b><span>The event and its linked mail&#39;s subjects</span></li>
<li><b>Agent door and MCP</b><span>Nothing calendar-related</span></li>
<li><b>Logs</b><span>Never a title, place or address</span></li>
<li><b>Triage prompts</b><span>Never</span></li>
</ul>
<figcaption>Guests are emailed only when your own edit says to notify them.</figcaption>
</figure>`,
  },
};

// ---------------------------------------------------------------- render

const md = (await Bun.file(DOC).text())
  // An em dash that opens a line would otherwise start a list.
  .replace(/\n— /g, " - ")
  // Site copy uses no em dashes.
  .replaceAll("—", "-");

let html = marked.parse(md, { async: false }) as string;
html = html.replace(/^<h1>.*?<\/h1>\n/, "");
// Links to other repo docs 404 on the web: keep the name, drop the link.
html = html.replace(/<a href="(?!https?:|mailto:|#)[^"]*">(.*?)<\/a>/g, "$1");

type Section = { num: string; title: string; id: string; body: string };
const parts = html.split(/<h2>/);
const intro = parts[0];
const sections: Section[] = parts.slice(1).map((p) => {
  const [head, ...rest] = p.split("</h2>");
  const m = head.match(/^(\d+[a-z]?)\. (.*)$/);
  if (!m) throw new Error(`section heading without a number: ${head}`);
  // Issue numbers point into the private repo; drop them from the heading.
  const title = m[2].replace(/\s*\(issue #\d+\)\s*$/, "");
  return { num: m[1], title, id: `s${m[1]}`, body: rest.join("</h2>") };
});

// Folds from a bold-led paragraph up to the next bold-led paragraph, warning
// box or section end. The bold lead (capture group 1) becomes the summary.
function fold(body: string, startRe: RegExp): string {
  const m = startRe.exec(body);
  if (!m) return body;
  const summary = m[1];
  const start = m.index;
  const tail = body.slice(start + m[0].length);
  const next = tail.search(/<p><strong>|<div class="dnb">|$/);
  const inner = (m[0].replace(/<strong>.*?<\/strong>\s*/, "") + tail.slice(0, next)).trim();
  return body.slice(0, start) + `<details><summary>${summary}</summary>\n${inner}\n</details>\n` + tail.slice(next);
}

function shape(s: Section): string {
  let b = s.body;
  b = b.replace(/<p><strong>Invariant\.<\/strong> ([\s\S]*?)<\/p>/,
    '<div class="inv"><p class="lbl">Invariant</p><p>$1</p></div>');
  b = b.replace(/<p><strong>(Enforcement(?:, in order)?)\.<\/strong><\/p>/, '<p class="lbl">$1</p>');
  b = b.replace(/<p><strong>Enforcement\.<\/strong> /, '<p class="lbl">Enforcement</p>\n<p>');
  b = b.replace(/<p><strong>(?:Do not break|What a maintainer must not break)\.<\/strong><\/p>\n(<ul>[\s\S]*?<\/ul>)/,
    '<div class="dnb"><p class="lbl">Do not break</p>\n$1</div>');
  b = b.replace(/<p><strong>Do not break\.<\/strong> ([\s\S]*?)<\/p>/,
    '<div class="dnb"><p class="lbl">Do not break</p><p>$1</p></div>');
  // Fold the audit contracts and the accepted-residual lists.
  while (/<p><strong>[^<]*\(audit contract\)\.<\/strong> /.test(b)) {
    b = fold(b, /<p><strong>([^<]*\(audit contract\))\.<\/strong> /);
  }
  b = b.replace(/<strong>Residual risks, accepted:<\/strong>(<ul>[\s\S]*?<\/ul>)/,
    "<details><summary>Residual risks, accepted</summary>$1</details>");
  b = b.replace(/(<li>Accepted residuals\.[^<]*?)(<ol>[\s\S]*?<\/ol>)/,
    "$1<details><summary>The four residuals</summary>$2</details>");
  b = b.replace(/<table>/g, '<div class="table-wrap"><table>').replace(/<\/table>/g, "</table></div>");

  const summary = SUMMARIES[s.num];
  if (!summary) warn(`§${s.num} has no summary; add one to SUMMARIES`);
  const v = VISUALS[s.num];
  if (v) {
    const at = b.indexOf(v.before);
    if (at < 0) {
      warn(`§${s.num}: diagram anchor not found, placed at the section's end`);
      b = b + v.html;
    } else {
      b = b.slice(0, at) + v.html + "\n" + b.slice(at);
    }
  } else if (s.num !== "1") {
    warn(`§${s.num} has no diagram`);
  }
  return `<p class="sec-num">&sect;${s.num}</p>
<h2 id="${s.id}">${s.title}</h2>
${summary ? `<p class="sec-lede">${summary}</p>\n` : ""}${b.trim()}`;
}

const REPORTING = `<h2 id="reporting-a-vulnerability">Reporting a vulnerability</h2>
<p>
  Email <a href="mailto:braelyn@passband.app">braelyn@passband.app</a>. We
  will acknowledge within 72 hours.
</p>`;

const article = [intro.trim(), MAP, ...sections.map(shape), REPORTING].join("\n");
const toc = [
  ...sections.map((s) => `          <li><a href="#${s.id}">${s.num}. ${s.title}</a></li>`),
  '          <li><a href="#reporting-a-vulnerability">Reporting a vulnerability</a></li>',
].join("\n");

// Every section link the copy above uses must land on a section.
for (const m of article.matchAll(/href="#(s\d[^"]*)"/g)) {
  if (!sections.some((s) => s.id === m[1])) warn(`link to missing section ${m[1]}`);
}

let page = await Bun.file(PAGE).text();
const replaceBetween = (src: string, open: string, close: string, inner: string) => {
  const i = src.indexOf(open);
  const j = src.indexOf(close, i + open.length);
  if (i < 0 || j < 0) throw new Error(`marker not found: ${open}`);
  return src.slice(0, i + open.length) + "\n" + inner + "\n" + src.slice(j);
};
page = replaceBetween(page, '<nav class="toc" aria-label="On this page">\n          <p>On this page</p>\n          <ol>', "          </ol>", toc);
page = replaceBetween(page, '<article class="prose">', "        </article>", article);
await Bun.write(PAGE, page);
console.log(`security.html: ${sections.length} sections from docs/SECURITY.md`);
