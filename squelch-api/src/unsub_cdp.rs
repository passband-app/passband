//! The unsubscribe browser agent's HANDS: a small Chrome DevTools Protocol
//! client and the handful of page operations the agent is allowed.
//!
//! The browser is not ours and not here. It is a remote Chromium
//! (`crate::unsub_browser` rents one per job) reached over one WebSocket, which
//! is why this is a few hundred lines of protocol and not a browser library:
//! the daemon needs to navigate, read, click, tick, choose and type one known
//! string, and nothing a library would add beyond that is something a
//! stranger's page should be able to reach for.
//!
//! WHERE OUR SCRIPT RUNS. The page is read and acted on from an ISOLATED WORLD:
//! a JavaScript context that shares the DOM with the page and nothing else. The
//! page's own script cannot see the table of controls we keep there, replace
//! the functions we call, or hand us a doctored `value` setter. What it can do
//! is lie in the DOM, which is the same lie it could tell a person.
//!
//! NOTHING THE MODEL SAYS IS EVER SCRIPT. Every call into the page is a fixed
//! function with numbers for arguments; the one string that is ever typed goes
//! in through the browser's input pipeline, never through source text.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;

/// One protocol call. Generous next to what a healthy browser needs, and short
/// next to the job's budget: a browser that stops answering is a job that ends.
const CALL_TIMEOUT: Duration = Duration::from_secs(15);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a page gets to finish loading after a navigation or a click.
const LOAD_WAIT: Duration = Duration::from_secs(10);
/// How long the page's own requests get to finish before it is read. A page
/// built by script is often an empty shell until a fetch comes back, and a
/// document that has merely stopped changing may only be waiting on one.
const NETWORK_WAIT: Duration = Duration::from_secs(6);
/// The network counts as calm once nothing has been in flight for this long.
const NETWORK_CALM: Duration = Duration::from_millis(500);
/// A request still open after this long is a stream or a long poll, not
/// something the page is waiting on to draw itself.
const REQUEST_IS_A_STREAM_AFTER: Duration = Duration::from_secs(4);
/// A dialog's text, as kept for the next reading of the page.
const MAX_DIALOG_CHARS: usize = 200;

#[derive(Debug, thiserror::Error)]
pub(crate) enum PageError {
    /// The page moved on under us (a navigation destroyed the context, the
    /// control is no longer in the document). Read the page again.
    #[error("stale")]
    Stale,
    /// The browser is gone, or said something this client cannot use.
    #[error("browser: {0}")]
    Browser(&'static str),
}

/// What kind of control a ref names. The agent's vocabulary is checked against
/// this, so it is deliberately coarse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Kind {
    Link,
    Button,
    Checkbox,
    Radio,
    Switch,
    Select,
    Email,
    Text,
    Textarea,
    Password,
    File,
    Other,
}

/// One control on the page, as found by [`SNAPSHOT_JS`]. Its ref is its index.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Control {
    pub kind: Kind,
    pub label: String,
    /// How many options a select has; 0 for everything else.
    #[serde(default)]
    pub options: usize,
    /// A plain text field that its own markup says is for an email address.
    #[serde(default)]
    pub email_like: bool,
    #[serde(default)]
    pub disabled: bool,
}

/// The page, as the agent reads it.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct Snapshot {
    pub url: String,
    pub title: String,
    /// Visible text with the controls inline, in the notation the agent's
    /// prompt describes (`squelch_core::triage::unsub_agent`).
    pub text: String,
    pub controls: Vec<Control>,
}

/// What the agent loop needs from a browser tab. A trait so the loop can be
/// tested against a scripted page; production has one implementation,
/// [`CdpPage`].
pub(crate) trait BrowserPage {
    async fn open(&mut self, url: &str) -> Result<(), PageError>;
    async fn snapshot(&mut self) -> Result<Snapshot, PageError>;
    async fn click(&mut self, r: usize) -> Result<(), PageError>;
    async fn set_checked(&mut self, r: usize, on: bool) -> Result<(), PageError>;
    async fn select(&mut self, r: usize, option: usize) -> Result<(), PageError>;
    /// Type `text` into field `r`, replacing what is there.
    async fn fill(&mut self, r: usize, text: &str) -> Result<(), PageError>;
}

// ---------------------------------------------------------------------------
// The wire.
// ---------------------------------------------------------------------------

struct Command {
    method: &'static str,
    params: Value,
    session: Option<String>,
    reply: oneshot::Sender<Result<Value, PageError>>,
}

/// One connection to a browser. Cheap to clone; the socket closes when the
/// last clone is dropped.
#[derive(Clone)]
pub(crate) struct Cdp {
    tx: mpsc::Sender<Command>,
    /// Text of JavaScript dialogs the pump accepted since the last reading.
    dialogs: Arc<Mutex<Vec<String>>>,
    /// Requests the page has in flight, by id, with when each started.
    in_flight: InFlight,
}

type InFlight = Arc<Mutex<HashMap<String, tokio::time::Instant>>>;

impl Cdp {
    /// Connect to a browser-level DevTools endpoint. `ws_url` IS A CREDENTIAL
    /// (a rented browser's URL carries its key) and is never logged, here or
    /// in an error.
    pub async fn connect(ws_url: &str) -> Result<Self, PageError> {
        let (ws, _) =
            tokio::time::timeout(CONNECT_TIMEOUT, tokio_tungstenite::connect_async(ws_url))
                .await
                .map_err(|_| PageError::Browser("connect timeout"))?
                .map_err(|_| PageError::Browser("connect"))?;
        let (tx, rx) = mpsc::channel(16);
        let dialogs = Arc::new(Mutex::new(Vec::new()));
        let in_flight: InFlight = Arc::default();
        tokio::spawn(pump(ws, rx, dialogs.clone(), in_flight.clone()));
        Ok(Self {
            tx,
            dialogs,
            in_flight,
        })
    }

    async fn call(
        &self,
        session: Option<&str>,
        method: &'static str,
        params: Value,
    ) -> Result<Value, PageError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Command {
                method,
                params,
                session: session.map(str::to_string),
                reply,
            })
            .await
            .map_err(|_| PageError::Browser("closed"))?;
        match tokio::time::timeout(CALL_TIMEOUT, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(PageError::Browser("closed")),
            Err(_) => Err(PageError::Browser("call timeout")),
        }
    }

    /// How many requests the page is plausibly still waiting on.
    fn busy(&self) -> usize {
        let now = tokio::time::Instant::now();
        self.in_flight
            .lock()
            .map(|m| {
                m.values()
                    .filter(|started| now.duration_since(**started) < REQUEST_IS_A_STREAM_AFTER)
                    .count()
            })
            .unwrap_or(0)
    }

    fn take_dialogs(&self) -> Vec<String> {
        self.dialogs
            .lock()
            .map(|mut d| std::mem::take(&mut *d))
            .unwrap_or_default()
    }
}

/// Owns the socket: writes commands, routes replies by id, answers the one
/// event that would otherwise freeze the page, and keeps count of the page's
/// requests so a reader can wait for them.
async fn pump<S>(
    ws: tokio_tungstenite::WebSocketStream<S>,
    mut rx: mpsc::Receiver<Command>,
    dialogs: Arc<Mutex<Vec<String>>>,
    in_flight: InFlight,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let (mut sink, mut stream) = ws.split();
    let mut pending: HashMap<u64, oneshot::Sender<Result<Value, PageError>>> = HashMap::new();
    let mut next_id: u64 = 0;
    loop {
        tokio::select! {
            cmd = rx.recv() => {
                // Every handle dropped: the job is over.
                let Some(cmd) = cmd else { break };
                next_id += 1;
                let mut msg = json!({ "id": next_id, "method": cmd.method, "params": cmd.params });
                if let Some(s) = cmd.session {
                    msg["sessionId"] = Value::String(s);
                }
                if sink.send(Message::Text(msg.to_string().into())).await.is_err() {
                    let _ = cmd.reply.send(Err(PageError::Browser("closed")));
                    break;
                }
                pending.insert(next_id, cmd.reply);
            }
            frame = stream.next() => {
                let text = match frame {
                    Some(Ok(Message::Text(t))) => t,
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                    Some(Ok(_)) => continue,
                };
                let Ok(msg) = serde_json::from_str::<Value>(text.as_str()) else { continue };
                if let Some(id) = msg.get("id").and_then(Value::as_u64) {
                    let Some(reply) = pending.remove(&id) else { continue };
                    let result = match msg.get("error") {
                        Some(err) => Err(classify_protocol_error(err)),
                        None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
                    };
                    let _ = reply.send(result);
                    continue;
                }
                let method = msg.get("method").and_then(Value::as_str);
                if let Some(id) = msg["params"]["requestId"].as_str()
                    && let Ok(mut flying) = in_flight.lock()
                {
                    match method {
                        Some("Network.requestWillBeSent") => {
                            // Bounded: a page cannot grow this without end.
                            if flying.len() < 2000 {
                                flying.insert(id.to_string(), tokio::time::Instant::now());
                            }
                        }
                        Some("Network.loadingFinished" | "Network.loadingFailed") => {
                            flying.remove(id);
                        }
                        _ => {}
                    }
                }
                // A JavaScript dialog (`confirm("Are you sure?")`) blocks the
                // page until it is answered, and the only person here is us.
                // Accepting is what pressing the page's own button meant; the
                // text is kept so the agent reads what was agreed to.
                if method == Some("Page.javascriptDialogOpening") {
                    let said = msg["params"]["message"].as_str().unwrap_or("");
                    if let Ok(mut d) = dialogs.lock() {
                        d.push(said.chars().take(MAX_DIALOG_CHARS).collect());
                    }
                    next_id += 1;
                    let mut answer = json!({
                        "id": next_id,
                        "method": "Page.handleJavaScriptDialog",
                        "params": { "accept": true },
                    });
                    if let Some(s) = msg.get("sessionId") {
                        answer["sessionId"] = s.clone();
                    }
                    if sink.send(Message::Text(answer.to_string().into())).await.is_err() {
                        break;
                    }
                }
            }
        }
    }
    // Dropping `pending` wakes every waiter with "closed".
    let _ = sink.close().await;
}

/// A protocol error, reduced to what the caller can act on. The browser's own
/// message is NOT carried: it can quote the page's URL.
fn classify_protocol_error(err: &Value) -> PageError {
    let message = err["message"].as_str().unwrap_or("").to_ascii_lowercase();
    if message.contains("context")
        || message.contains("navigat")
        || message.contains("detached")
        || message.contains("no node")
        || message.contains("not found")
    {
        PageError::Stale
    } else {
        PageError::Browser("protocol")
    }
}

// ---------------------------------------------------------------------------
// The page.
// ---------------------------------------------------------------------------

/// Reads the page into a [`Snapshot`] and leaves the table of controls in the
/// isolated world as `globalThis.__pb`, one `{el, target}` per ref.
///
/// WHAT COUNTS AS VISIBLE is the part worth care, because the agent's "done" is
/// taken from this text: a success message sitting in the markup, hidden until
/// a script reveals it, must not be read. So a subtree is skipped when the
/// browser itself says it is not rendered (`checkVisibility`: display, visibility,
/// opacity, content-visibility, closed `<details>`), when it is collapsed
/// behind a clip (`overflow: hidden` on a box under two pixels: closed
/// accordions and screen-reader-only text), or when it sits entirely off the
/// top or left of the document. An element with `display: contents` is the
/// one thing the browser calls invisible that is not: it has no box, and its
/// children are on screen.
///
/// Square brackets in the page's own text become parentheses, so the only
/// bracketed controls the agent ever sees are the ones written here.
const SNAPSHOT_JS: &str = r#"function() {
  const MAX_NODES = 8000, MAX_CONTROLS = 150, MAX_TEXT = 12000, MAX_LABEL = 140, MAX_OPTIONS = 15;
  const refs = [], controls = [];
  let out = '', nodes = 0;
  const clean = (s) => String(s || '').replace(/\[/g, '(').replace(/\]/g, ')').replace(/\s+/g, ' ').trim();
  const cut = (s, n) => (s.length > n ? s.slice(0, n) + '...' : s);
  const q = (s) => '"' + cut(clean(s), MAX_LABEL).replace(/"/g, "'") + '"';
  const SKIP = new Set(['SCRIPT', 'STYLE', 'NOSCRIPT', 'TEMPLATE', 'HEAD', 'META', 'LINK', 'OBJECT',
    'EMBED', 'CANVAS', 'VIDEO', 'AUDIO', 'OPTION', 'OPTGROUP', 'svg', 'SVG']);
  const BLOCK = /^(block|flex|grid|list-item|table|table-row|table-caption|flow-root)$/;
  const rendered = (el) => {
    // `display: contents` has no box of its own, so the browser calls it not
    // visible, while everything inside it is drawn as usual. Whole sites are
    // wrapped in these; each child answers for itself.
    if (getComputedStyle(el).display === 'contents') return true;
    if (el.checkVisibility) {
      return el.checkVisibility({ opacityProperty: true, visibilityProperty: true });
    }
    const s = getComputedStyle(el);
    return s.display !== 'none' && s.visibility === 'visible' && parseFloat(s.opacity || '1') > 0;
  };
  const clippedAway = (el, style) => {
    const r = el.getBoundingClientRect();
    if (r.right + scrollX < 0 || r.bottom + scrollY < 0) return true;
    const hides = (v) => v === 'hidden' || v === 'clip';
    return (hides(style.overflowX) || hides(style.overflowY)) && (r.width < 2 || r.height < 2);
  };
  const emailLike = (el) => /e-?mail/i.test([el.name, el.id, el.getAttribute('placeholder'),
    el.getAttribute('autocomplete'), el.getAttribute('aria-label'), labelOf(el)].join(' '));
  function labelOf(el) {
    const aria = el.getAttribute('aria-label');
    if (aria && clean(aria)) return aria;
    const by = el.getAttribute('aria-labelledby');
    if (by) {
      const root = el.getRootNode();
      const t = by.split(/\s+/).map((id) => {
        const n = root.getElementById ? root.getElementById(id) : null;
        return n ? n.innerText || n.textContent : '';
      }).join(' ');
      if (clean(t)) return t;
    }
    if (el.labels && el.labels.length) {
      const t = Array.from(el.labels).map((l) => l.innerText || l.textContent).join(' ');
      if (clean(t)) return t;
    }
    const tag = el.tagName;
    if (tag === 'INPUT' || tag === 'SELECT' || tag === 'TEXTAREA') {
      const wrap = el.closest('label');
      if (wrap && clean(wrap.innerText)) return wrap.innerText;
      if (tag === 'INPUT' && /^(submit|button|reset|image)$/i.test(el.type)) {
        return el.value || el.alt || el.type;
      }
      return el.getAttribute('placeholder') || el.getAttribute('title') || el.name || el.id || '';
    }
    const t = el.innerText || el.textContent;
    if (clean(t)) return t;
    const img = el.querySelector('img[alt]');
    return el.getAttribute('title') || (img ? img.alt : '') || el.getAttribute('value') || '';
  }
  function kindOf(el, style) {
    const tag = el.tagName;
    if (tag === 'INPUT') {
      const t = (el.type || 'text').toLowerCase();
      if (t === 'hidden') return null;
      if (t === 'checkbox') return el.getAttribute('role') === 'switch' ? 'switch' : 'checkbox';
      if (t === 'radio') return 'radio';
      if (/^(submit|button|reset|image)$/.test(t)) return 'button';
      if (t === 'email') return 'email';
      if (t === 'password') return 'password';
      if (t === 'file') return 'file';
      if (/^(text|search|tel|url|number)$/.test(t)) return 'text';
      return 'other';
    }
    if (tag === 'BUTTON' || tag === 'SUMMARY') return 'button';
    if (tag === 'SELECT') return 'select';
    if (tag === 'TEXTAREA') return 'textarea';
    if (tag === 'A' && el.hasAttribute('href')) return 'link';
    const role = (el.getAttribute('role') || '').toLowerCase();
    if (role === 'link') return 'link';
    if (role === 'checkbox' || role === 'menuitemcheckbox') return 'checkbox';
    if (role === 'radio' || role === 'menuitemradio') return 'radio';
    if (role === 'switch') return 'switch';
    if (/^(button|tab|menuitem|option|combobox)$/.test(role)) return 'button';
    // A div somebody put a click handler on and never told anyone about: the
    // outermost thing the pointer turns into a hand over, with no real control
    // inside it.
    if (tag !== 'LABEL' && style.cursor === 'pointer' && el.parentElement
        && getComputedStyle(el.parentElement).cursor !== 'pointer'
        && !el.querySelector('a[href],button,input,select,textarea,[role]')) {
      return 'button';
    }
    return null;
  }
  const checkedOf = (el) => (el.tagName === 'INPUT' ? !!el.checked : el.getAttribute('aria-checked') === 'true');
  function emit(el, kind) {
    const i = refs.length;
    const label = labelOf(el);
    const disabled = !!el.disabled || el.getAttribute('aria-disabled') === 'true';
    const c = { kind, label: cut(clean(label), MAX_LABEL), options: 0, email_like: false, disabled };
    let s = '[' + i + ' ' + kind;
    let target = el;
    if (kind === 'checkbox' || kind === 'radio' || kind === 'switch') {
      s += (checkedOf(el) ? ' on ' : ' off ') + q(label);
    } else if (kind === 'select') {
      const opts = Array.from(el.options || []);
      c.options = opts.length;
      const cur = el.selectedIndex >= 0 && opts[el.selectedIndex] ? opts[el.selectedIndex].text : '';
      s += ' ' + q(label) + ' = ' + q(cur) + ' options: '
        + opts.slice(0, MAX_OPTIONS).map((o, k) => k + ' ' + q(o.text)).join(' | ');
      if (opts.length > MAX_OPTIONS) s += ' (and ' + (opts.length - MAX_OPTIONS) + ' more)';
    } else if (kind === 'link' || kind === 'button') {
      s += ' ' + q(label);
    } else {
      if (kind === 'text') c.email_like = emailLike(el);
      s += ' ' + q(label) + (el.value ? ' filled' : ' empty') + (el.required ? ' required' : '');
    }
    if (disabled) s += ' disabled';
    out += ' ' + s + '] ';
    refs.push({ el, target });
    controls.push(c);
  }
  const brk = () => { if (out && !out.endsWith('\n')) out += '\n'; };
  function kids(el) {
    if (el.shadowRoot) return el.shadowRoot.childNodes;
    if (el.tagName === 'SLOT') {
      const a = el.assignedNodes();
      if (a.length) return a;
    }
    return el.childNodes;
  }
  function walk(node, quiet) {
    if (nodes++ > MAX_NODES || out.length > MAX_TEXT) return;
    if (node.nodeType === 3) {
      if (!quiet) {
        const t = clean(node.nodeValue);
        if (t) out += t + ' ';
      }
      return;
    }
    if (node.nodeType !== 1) return;
    const el = node;
    const tag = el.tagName;
    if (SKIP.has(tag)) return;
    if (tag === 'BR') { out += '\n'; return; }
    if (tag === 'IFRAME' || tag === 'FRAME') {
      if (rendered(el)) {
        let host = '';
        try { host = new URL(el.src, location.href).host; } catch (e) {}
        out += ' (embedded frame from ' + clean(host || 'unknown') + ', contents not shown) ';
      }
      return;
    }
    // A native checkbox or radio is often hidden so a styled label can stand
    // in for it. It is still the control; the path down to it was visible.
    const nativeToggle = tag === 'INPUT' && /^(checkbox|radio)$/i.test(el.type);
    if (!nativeToggle && !rendered(el)) return;
    const style = getComputedStyle(el);
    if (!nativeToggle && clippedAway(el, style)) return;
    const kind = controls.length < MAX_CONTROLS ? kindOf(el, style) : null;
    const block = BLOCK.test(style.display);
    if (block) brk();
    if (kind) {
      emit(el, kind);
      // A link's or button's text IS its label; anything else has no children
      // worth reading.
    } else if (tag === 'INPUT') {
      // type=hidden, or past the control cap.
    } else {
      // A label's words are already quoted on its control.
      const mute = quiet || (tag === 'LABEL' && !!el.control);
      for (const k of kids(el)) walk(k, mute);
    }
    if (block) brk();
    else if (style.display === 'table-cell') out += ' ';
  }
  if (document.body) walk(document.body, false);
  globalThis.__pb = refs;
  const text = out.replace(/[ \t]+/g, ' ').replace(/ ?\n ?/g, '\n').replace(/\n{3,}/g, '\n\n').trim();
  return { url: location.href, title: clean(document.title), text, controls };
}"#;

/// Scroll a control into view and say where its middle is, and whether a click
/// there would actually land on it (an overlay may be in the way).
const AIM_JS: &str = r#"function(i) {
  const e = (globalThis.__pb || [])[i];
  if (!e || !e.target.isConnected) return { gone: true };
  const t = e.target;
  t.scrollIntoView({ block: 'center', inline: 'center' });
  const r = t.getBoundingClientRect();
  const x = r.left + r.width / 2, y = r.top + r.height / 2;
  const root = t.getRootNode();
  const hit = (root.elementFromPoint ? root : document).elementFromPoint(x, y);
  return { x, y, clear: !!hit && (hit === t || t.contains(hit)) && r.width >= 1 && r.height >= 1 };
}"#;

/// The fallback click, for a control a pointer cannot reach.
const CLICK_JS: &str = r#"function(i) {
  const e = (globalThis.__pb || [])[i];
  if (!e || !e.target.isConnected) return false;
  e.target.click();
  return true;
}"#;

/// Is this toggle on? `null` when the control is gone.
const CHECKED_JS: &str = r#"function(i) {
  const e = (globalThis.__pb || [])[i];
  if (!e || !e.el.isConnected) return null;
  const el = e.el;
  return { native: el.tagName === 'INPUT', on: el.tagName === 'INPUT' ? !!el.checked : el.getAttribute('aria-checked') === 'true' };
}"#;

/// Flip a NATIVE checkbox or radio by its own `click()`: it toggles, fires the
/// events a framework listens for, and cannot land on a link inside the label.
const TOGGLE_JS: &str = r#"function(i) {
  const e = (globalThis.__pb || [])[i];
  if (!e || !e.el.isConnected) return false;
  e.el.click();
  return true;
}"#;

const SELECT_JS: &str = r#"function(i, k) {
  const e = (globalThis.__pb || [])[i];
  if (!e || !e.el.isConnected) return null;
  const el = e.el;
  if (el.tagName !== 'SELECT' || k < 0 || k >= el.options.length) return false;
  el.selectedIndex = k;
  el.dispatchEvent(new Event('input', { bubbles: true }));
  el.dispatchEvent(new Event('change', { bubbles: true }));
  return true;
}"#;

/// Put the caret in a field with its contents selected, ready to be typed over.
const FOCUS_JS: &str = r#"function(i) {
  const e = (globalThis.__pb || [])[i];
  if (!e || !e.el.isConnected) return null;
  const el = e.el;
  el.scrollIntoView({ block: 'center', inline: 'center' });
  el.focus();
  if (el.select) el.select();
  return true;
}"#;

/// After typing: does the field hold exactly `text`? If the page ate the
/// keystrokes, set it the way a framework's own input handling would see.
const ENSURE_VALUE_JS: &str = r#"function(i, text) {
  const e = (globalThis.__pb || [])[i];
  if (!e || !e.el.isConnected) return null;
  const el = e.el;
  if (el.value === text) return true;
  const d = Object.getOwnPropertyDescriptor(Object.getPrototypeOf(el), 'value');
  if (d && d.set) d.set.call(el, text); else el.value = text;
  el.dispatchEvent(new Event('input', { bubbles: true }));
  el.dispatchEvent(new Event('change', { bubbles: true }));
  return el.value === text;
}"#;

/// Resolves once the document has gone half a second without changing, or
/// after three seconds regardless: long enough for a script to draw the page
/// it was fetched to draw, short enough for a page that never sits still.
const QUIET_JS: &str = r#"new Promise((resolve) => {
  let idle = setTimeout(done, 500);
  const cap = setTimeout(done, 3000);
  const obs = new MutationObserver(() => { clearTimeout(idle); idle = setTimeout(done, 500); });
  obs.observe(document, { subtree: true, childList: true, attributes: true, characterData: true });
  function done() { obs.disconnect(); clearTimeout(idle); clearTimeout(cap); resolve(true); }
})"#;

/// One tab of a remote browser.
pub(crate) struct CdpPage {
    cdp: Cdp,
    session: String,
    /// The isolated world holding the current snapshot's control table.
    /// `None` after anything that may have navigated.
    world: Option<i64>,
    /// Tabs we have seen, so a new one (a link that opened a window) is
    /// noticed and followed.
    known: HashSet<String>,
}

impl CdpPage {
    /// Take the browser's existing tab, or open one.
    pub async fn attach(cdp: Cdp) -> Result<Self, PageError> {
        let targets = cdp.call(None, "Target.getTargets", json!({})).await?;
        let pages = page_targets(&targets);
        let target = match pages.first() {
            Some(t) => t.clone(),
            None => cdp
                .call(None, "Target.createTarget", json!({ "url": "about:blank" }))
                .await?["targetId"]
                .as_str()
                .ok_or(PageError::Browser("no target"))?
                .to_string(),
        };
        let mut known: HashSet<String> = pages.into_iter().collect();
        known.insert(target.clone());
        let session = attach_to(&cdp, &target).await?;
        Ok(Self {
            cdp,
            session,
            world: None,
            known,
        })
    }

    async fn call(&self, method: &'static str, params: Value) -> Result<Value, PageError> {
        self.cdp.call(Some(&self.session), method, params).await
    }

    /// Run one of the fixed functions above in the isolated world, by value.
    async fn run(&self, function: &'static str, args: &[Value]) -> Result<Value, PageError> {
        let world = self.world.ok_or(PageError::Stale)?;
        let arguments: Vec<Value> = args.iter().map(|v| json!({ "value": v })).collect();
        let out = self
            .call(
                "Runtime.callFunctionOn",
                json!({
                    "functionDeclaration": function,
                    "executionContextId": world,
                    "arguments": arguments,
                    "returnByValue": true,
                }),
            )
            .await?;
        if out.get("exceptionDetails").is_some() {
            return Err(PageError::Stale);
        }
        Ok(out["result"]["value"].clone())
    }

    /// Wait for whatever the last action started: the load, then the page's
    /// own requests, then its scripts going quiet. Then follow a newly opened
    /// tab, if there is one.
    /// Never fails: a page that will not settle is read as it stands.
    async fn settle(&mut self) {
        self.world = None;
        tokio::time::sleep(Duration::from_millis(250)).await;
        let deadline = tokio::time::Instant::now() + LOAD_WAIT;
        loop {
            let state = self
                .call(
                    "Runtime.evaluate",
                    json!({ "expression": "document.readyState", "returnByValue": true }),
                )
                .await;
            let complete = matches!(&state, Ok(v) if v["result"]["value"] == "complete");
            if complete || tokio::time::Instant::now() >= deadline {
                break;
            }
            if matches!(state, Err(PageError::Browser(_))) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        self.calm().await;
        let _ = self
            .call(
                "Runtime.evaluate",
                json!({ "expression": QUIET_JS, "awaitPromise": true, "returnByValue": true }),
            )
            .await;
        self.follow_new_tab().await;
    }

    /// Wait until the page has had nothing in flight for [`NETWORK_CALM`], or
    /// [`NETWORK_WAIT`] has passed. "The document stopped changing" is not
    /// "the page is drawn": an app shell sits perfectly still while the fetch
    /// that fills it is on the wire.
    async fn calm(&self) {
        let deadline = tokio::time::Instant::now() + NETWORK_WAIT;
        let mut calm_since: Option<tokio::time::Instant> = None;
        loop {
            let now = tokio::time::Instant::now();
            if self.cdp.busy() == 0 {
                let since = *calm_since.get_or_insert(now);
                if now.duration_since(since) >= NETWORK_CALM {
                    return;
                }
            } else {
                calm_since = None;
            }
            if now >= deadline {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// A link with `target=_blank`, or a script's `window.open`, puts the next
    /// page in a tab we are not attached to. The newest tab is the one a
    /// person would be looking at.
    async fn follow_new_tab(&mut self) {
        let Ok(targets) = self.cdp.call(None, "Target.getTargets", json!({})).await else {
            return;
        };
        let fresh: Vec<String> = page_targets(&targets)
            .into_iter()
            .filter(|t| !self.known.contains(t))
            .collect();
        let Some(newest) = fresh.last().cloned() else {
            return;
        };
        self.known.extend(fresh);
        if let Ok(session) = attach_to(&self.cdp, &newest).await {
            self.session = session;
            // The new tab may still be loading; one more wait, no recursion
            // into tab-following.
            tokio::time::sleep(Duration::from_millis(500)).await;
            self.calm().await;
            let _ = self
                .call(
                    "Runtime.evaluate",
                    json!({ "expression": QUIET_JS, "awaitPromise": true, "returnByValue": true }),
                )
                .await;
        }
    }

    async fn fresh_world(&mut self) -> Result<i64, PageError> {
        let tree = self.call("Page.getFrameTree", json!({})).await?;
        let frame = tree["frameTree"]["frame"]["id"]
            .as_str()
            .ok_or(PageError::Browser("no frame"))?;
        let world = self
            .call(
                "Page.createIsolatedWorld",
                json!({ "frameId": frame, "worldName": "passband", "grantUniveralAccess": false }),
            )
            .await?["executionContextId"]
            .as_i64()
            .ok_or(PageError::Browser("no world"))?;
        self.world = Some(world);
        Ok(world)
    }

    async fn read(&mut self) -> Result<Snapshot, PageError> {
        self.fresh_world().await?;
        let value = self.run(SNAPSHOT_JS, &[]).await?;
        let mut snap: Snapshot = serde_json::from_value(value).map_err(|_| PageError::Stale)?;
        for said in self.cdp.take_dialogs() {
            // Brackets out, as for the rest of the page's words.
            let said = said.replace('[', "(").replace(']', ")");
            snap.text = format!(
                "(the page showed a dialog, which was accepted: {said})\n{}",
                snap.text
            );
        }
        Ok(snap)
    }

    async fn mouse(&self, kind: &str, x: f64, y: f64) -> Result<(), PageError> {
        let mut params = json!({ "type": kind, "x": x, "y": y });
        if kind != "mouseMoved" {
            params["button"] = json!("left");
            params["clickCount"] = json!(1);
        }
        self.call("Input.dispatchMouseEvent", params)
            .await
            .map(drop)
    }

    /// A click a page cannot tell from a person's when the control is
    /// reachable by pointer, and the control's own `click()` when it is not.
    async fn press(&mut self, r: usize) -> Result<(), PageError> {
        let aim = self.run(AIM_JS, &[json!(r)]).await?;
        if aim["gone"] == true {
            return Err(PageError::Stale);
        }
        match (aim["clear"] == true, aim["x"].as_f64(), aim["y"].as_f64()) {
            (true, Some(x), Some(y)) => {
                self.mouse("mouseMoved", x, y).await?;
                self.mouse("mousePressed", x, y).await?;
                self.mouse("mouseReleased", x, y).await?;
            }
            _ => {
                if self.run(CLICK_JS, &[json!(r)]).await? != true {
                    return Err(PageError::Stale);
                }
            }
        }
        Ok(())
    }
}

impl BrowserPage for CdpPage {
    async fn open(&mut self, url: &str) -> Result<(), PageError> {
        let out = self.call("Page.navigate", json!({ "url": url })).await?;
        // `errorText` is a navigation that never produced a page: DNS, TLS,
        // refused. A download reports one too, and is no more readable.
        if out
            .get("errorText")
            .and_then(Value::as_str)
            .is_some_and(|e| !e.is_empty())
        {
            return Err(PageError::Browser("navigation"));
        }
        self.settle().await;
        Ok(())
    }

    async fn snapshot(&mut self) -> Result<Snapshot, PageError> {
        match self.read().await {
            // A page still redirecting when we looked: wait it out, once.
            Err(PageError::Stale) => {
                self.settle().await;
                self.read().await
            }
            other => other,
        }
    }

    async fn click(&mut self, r: usize) -> Result<(), PageError> {
        self.press(r).await?;
        self.settle().await;
        Ok(())
    }

    async fn set_checked(&mut self, r: usize, on: bool) -> Result<(), PageError> {
        let state = self.run(CHECKED_JS, &[json!(r)]).await?;
        if state.is_null() {
            return Err(PageError::Stale);
        }
        if (state["on"] == true) == on {
            return Ok(());
        }
        if state["native"] == true {
            if self.run(TOGGLE_JS, &[json!(r)]).await? != true {
                return Err(PageError::Stale);
            }
        } else {
            self.press(r).await?;
        }
        // A toggle does not navigate, so the control table stays good for the
        // rest of the step's actions; a page that DOES navigate on a tick
        // makes the next action stale, which is the right answer.
        tokio::time::sleep(Duration::from_millis(150)).await;
        Ok(())
    }

    async fn select(&mut self, r: usize, option: usize) -> Result<(), PageError> {
        match self.run(SELECT_JS, &[json!(r), json!(option)]).await? {
            Value::Bool(true) => {
                tokio::time::sleep(Duration::from_millis(150)).await;
                Ok(())
            }
            _ => Err(PageError::Stale),
        }
    }

    async fn fill(&mut self, r: usize, text: &str) -> Result<(), PageError> {
        if self.run(FOCUS_JS, &[json!(r)]).await?.is_null() {
            return Err(PageError::Stale);
        }
        // Through the input pipeline, as typing: the page's own handlers see
        // real input events. The text never becomes script.
        self.call("Input.insertText", json!({ "text": text }))
            .await?;
        match self.run(ENSURE_VALUE_JS, &[json!(r), json!(text)]).await? {
            Value::Bool(true) => Ok(()),
            _ => Err(PageError::Stale),
        }
    }
}

fn page_targets(targets: &Value) -> Vec<String> {
    targets["targetInfos"]
        .as_array()
        .map(|infos| {
            infos
                .iter()
                .filter(|t| t["type"] == "page")
                .filter_map(|t| t["targetId"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Attach to a tab and turn on the two domains whose events we use.
async fn attach_to(cdp: &Cdp, target: &str) -> Result<String, PageError> {
    let session = cdp
        .call(
            None,
            "Target.attachToTarget",
            json!({ "targetId": target, "flatten": true }),
        )
        .await?["sessionId"]
        .as_str()
        .ok_or(PageError::Browser("no session"))?
        .to_string();
    // Dialog events arrive only with the Page domain enabled, and the count
    // of requests in flight only with Network.
    cdp.call(Some(&session), "Page.enable", json!({})).await?;
    cdp.call(Some(&session), "Network.enable", json!({}))
        .await?;
    Ok(session)
}

/// THE REAL THING: these drive an actual headless Chrome against pages served
/// from loopback, because the snapshot script and the input calls are only
/// true or false in a browser. Ignored by default (CI has no Chrome to count
/// on); run them with
/// `cargo test -p squelch-api unsub_cdp -- --ignored --test-threads=1`.
#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use axum::extract::State;
    use axum::response::Html;
    use axum::routing::{get, post};
    use tokio::io::{AsyncBufReadExt, BufReader};

    pub(crate) type Hits = Arc<Mutex<Vec<String>>>;

    /// A preference center the way they are actually built: nothing in the
    /// markup but a script, a native checkbox hidden behind a styled label, a
    /// success message present-but-hidden from the start, a `confirm()` on
    /// save, and a save that is a `fetch`, not a form post.
    pub(crate) const PREFS: &str = r#"<!doctype html><html><head><title>Email [preferences]</title></head><body>
<div style="display:contents"><div id="app"></div></div>
<script>
setTimeout(() => {
  document.getElementById('app').innerHTML = `
    <h1>Manage your emails</h1>
    <div id="ok" style="display:none">You have been unsubscribed.</div>
    <div style="height:0;overflow:hidden">Success! You are removed.</div>
    <p>Signed in as [nobody]</p>
    <label class="fancy"><input type="checkbox" id="deals" checked style="display:none"><span>Weekly deals</span></label>
    <div><input type="checkbox" id="news" checked><label for="news">Product news</label></div>
    <div role="switch" id="partners" aria-checked="true" tabindex="0" aria-label="Partner offers"
         onclick="this.setAttribute('aria-checked', this.getAttribute('aria-checked') === 'true' ? 'false' : 'true')"
         style="width:40px;height:20px;background:#ccc"></div>
    <input type="text" name="email_address" id="em" placeholder="Your email">
    <input type="text" name="fullname" placeholder="Full name">
    <input type="password" name="pw">
    <select id="freq" aria-label="How often"><option>Daily</option><option>Weekly</option><option>Never</option></select>
    <button id="re" onclick="fetch('/hit/resubscribe', {method:'POST'})">Resubscribe</button>
    <div id="save" style="cursor:pointer;width:120px;height:30px">Save preferences</div>`;
  document.getElementById('save').addEventListener('click', async () => {
    if (!confirm('Really stop these emails?')) return;
    const body = JSON.stringify({
      deals: document.getElementById('deals').checked,
      news: document.getElementById('news').checked,
      partners: document.getElementById('partners').getAttribute('aria-checked'),
      email: document.getElementById('em').value,
      freq: document.getElementById('freq').value,
    });
    await fetch('/hit/save', { method: 'POST', body });
    document.getElementById('app').innerHTML = '<h1>Your preferences have been saved.</h1>';
  });
}, 300);
</script></body></html>"#;

    const POPUP: &str = r#"<!doctype html><html><body>
<a href="/done" target="_blank">Unsubscribe from all</a></body></html>"#;

    pub(crate) async fn site() -> (String, Hits) {
        let hits: Hits = Arc::new(Mutex::new(Vec::new()));
        let app = axum::Router::new()
            .route("/prefs", get(|| async { Html(PREFS) }))
            .route("/popup", get(|| async { Html(POPUP) }))
            .route(
                "/done",
                get(|| async { Html("<h1>You are unsubscribed.</h1>") }),
            )
            .route(
                "/hit/{what}",
                post(
                    |State(h): State<Hits>,
                     axum::extract::Path(what): axum::extract::Path<String>,
                     body: String| async move {
                        h.lock().unwrap().push(format!("{what} {body}"));
                        "ok"
                    },
                ),
            )
            .with_state(hits.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}"), hits)
    }

    /// A local headless Chrome, killed and cleaned up on drop.
    pub(crate) struct Chrome {
        child: tokio::process::Child,
        dir: std::path::PathBuf,
        pub(crate) ws: String,
    }

    impl Drop for Chrome {
        fn drop(&mut self) {
            let _ = self.child.start_kill();
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    pub(crate) async fn chrome() -> Chrome {
        let bin = std::env::var("CHROME_BIN").unwrap_or_else(|_| {
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome".to_string()
        });
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("passband-cdp-test-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut child = tokio::process::Command::new(bin)
            .args([
                "--headless=new",
                "--remote-debugging-port=0",
                "--no-first-run",
                "--no-default-browser-check",
                "--window-size=1200,900",
            ])
            .arg(format!("--user-data-dir={}", dir.display()))
            .arg("about:blank")
            .stderr(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .expect("a local Chrome (set CHROME_BIN)");
        let mut lines = BufReader::new(child.stderr.take().unwrap()).lines();
        let ws = tokio::time::timeout(Duration::from_secs(20), async {
            while let Ok(Some(line)) = lines.next_line().await {
                if let Some(ws) = line.strip_prefix("DevTools listening on ") {
                    return ws.trim().to_string();
                }
            }
            panic!("Chrome exited without a DevTools endpoint");
        })
        .await
        .expect("Chrome started");
        Chrome { child, dir, ws }
    }

    fn find(snap: &Snapshot, kind: Kind, label: &str) -> usize {
        snap.controls
            .iter()
            .position(|c| c.kind == kind && c.label.contains(label))
            .unwrap_or_else(|| panic!("no {kind:?} labelled {label:?} in:\n{}", snap.text))
    }

    #[tokio::test]
    #[ignore = "needs a local Chrome"]
    async fn a_script_built_preference_center_is_read_and_worked_like_a_person_would() {
        let (base, hits) = site().await;
        let chrome = chrome().await;
        let mut page = CdpPage::attach(Cdp::connect(&chrome.ws).await.unwrap())
            .await
            .unwrap();
        page.open(&format!("{base}/prefs")).await.unwrap();
        let snap = page.snapshot().await.unwrap();
        eprintln!("--- snapshot ---\n{}\n---", snap.text);

        // Read: what a person sees, and only that.
        assert!(
            snap.text.contains("Manage your emails"),
            "the script ran, and a `display: contents` wrapper hides nothing"
        );
        assert!(
            !snap.text.contains("unsubscribed") && !snap.text.contains("removed"),
            "a success message that is in the markup but not on screen is not read"
        );
        assert!(
            snap.text.contains("(nobody)"),
            "the page's own brackets are defanged"
        );
        assert_eq!(snap.title, "Email (preferences)");
        let deals = find(&snap, Kind::Checkbox, "Weekly deals");
        let news = find(&snap, Kind::Checkbox, "Product news");
        let partners = find(&snap, Kind::Switch, "Partner offers");
        let email = find(&snap, Kind::Text, "Your email");
        let name = find(&snap, Kind::Text, "Full name");
        let freq = find(&snap, Kind::Select, "How often");
        let save = find(&snap, Kind::Button, "Save preferences");
        find(&snap, Kind::Password, "pw");
        assert!(snap.controls[email].email_like);
        assert!(!snap.controls[name].email_like);
        assert_eq!(snap.controls[freq].options, 3);
        assert!(
            snap.text
                .contains(&format!("[{deals} checkbox on \"Weekly deals\"]"))
        );
        assert!(
            snap.text
                .contains(&format!("[{partners} switch on \"Partner offers\"]"))
        );
        assert!(
            snap.text
                .contains("options: 0 \"Daily\" | 1 \"Weekly\" | 2 \"Never\"")
        );
        assert_eq!(
            snap.text.matches("Weekly deals").count(),
            1,
            "a label is quoted on its control and not said twice"
        );

        // Act.
        page.set_checked(deals, false).await.unwrap();
        page.set_checked(news, false).await.unwrap();
        page.set_checked(news, false).await.unwrap(); // already off: a no-op, not a flip back
        page.set_checked(partners, false).await.unwrap();
        page.fill(email, "me+shop@example.com").await.unwrap();
        page.select(freq, 2).await.unwrap();
        page.click(save).await.unwrap();

        let after = page.snapshot().await.unwrap();
        eprintln!("--- after ---\n{}\n---", after.text);
        assert!(after.text.contains("Your preferences have been saved."));
        assert!(
            after.text.contains("Really stop these emails?"),
            "the confirm() was accepted, and the agent is told what it said"
        );
        let hits = hits.lock().unwrap().clone();
        assert_eq!(hits.len(), 1, "{hits:?}");
        let sent: Value = serde_json::from_str(hits[0].strip_prefix("save ").unwrap()).unwrap();
        assert_eq!(
            sent,
            json!({
                "deals": false, "news": false, "partners": "false",
                "email": "me+shop@example.com", "freq": "Never",
            })
        );

        // The old page's numbers mean nothing on the new one.
        assert!(matches!(
            page.set_checked(deals, true).await,
            Err(PageError::Stale)
        ));
    }

    #[tokio::test]
    #[ignore = "needs a local Chrome"]
    async fn a_link_that_opens_a_new_tab_is_followed_into_it() {
        let (base, _hits) = site().await;
        let chrome = chrome().await;
        let mut page = CdpPage::attach(Cdp::connect(&chrome.ws).await.unwrap())
            .await
            .unwrap();
        page.open(&format!("{base}/popup")).await.unwrap();
        let snap = page.snapshot().await.unwrap();
        let link = find(&snap, Kind::Link, "Unsubscribe from all");
        page.click(link).await.unwrap();
        let after = page.snapshot().await.unwrap();
        assert!(
            after.text.contains("You are unsubscribed."),
            "{}",
            after.text
        );
        assert!(after.url.ends_with("/done"));
    }

    #[tokio::test]
    #[ignore = "needs a local Chrome"]
    async fn a_page_that_does_not_exist_is_an_error_not_an_empty_page() {
        let chrome = chrome().await;
        let mut page = CdpPage::attach(Cdp::connect(&chrome.ws).await.unwrap())
            .await
            .unwrap();
        // A port nothing listens on.
        assert!(page.open("http://127.0.0.1:9/").await.is_err());
    }
}
