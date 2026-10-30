//! ONE IMAP SEND PER COMPOSITION (#575 phase 3 final review N4).
//!
//! An IMAP send outlives its request (`handlers::detached_send`), so a
//! client that gives up (Passband's 15 s idle) and retries does not replace
//! the first send, it adds a second. The daemon answers the retry instead:
//! while a send with the same [`send_key`] is in flight, a second request
//! awaits the first and gets its answer; for [`WINDOW`] after it went (or
//! after it ended in doubt), a repeat gets the same answer, marked
//! `"deduplicated": true`, and nothing is sent.
//!
//! The key is the request itself, normalized: Passband's send carries no
//! per-compose id (`draft_id` only when a draft exists), and the staged
//! files are named by id, which are unique rows. Two identical messages a
//! user really means to send twice within two minutes are one send; that is
//! the trade, and the second can be sent after the window.
//!
//! In memory only: a restart forgets, which is no worse than before this.
//! Gmail never comes here.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::http::StatusCode;
use sha2::{Digest, Sha256};
use tokio::sync::watch;

use crate::error::ApiError;
use crate::handlers::SendBody;

/// How long a settled send answers its repeats.
pub(crate) const WINDOW: Duration = Duration::from_secs(120);

/// What a send task that died (a panic) is answered with: the mail may have
/// gone, so it is never "try again", and its repeats are not sent.
pub(crate) const SEND_TASK_LOST: &str = "the send stopped unexpectedly and the message may have \
     been sent; check Sent before sending it again";

pub(crate) type Key = [u8; 32];

/// A send's answer as its repeats get it.
#[derive(Debug, Clone)]
pub(crate) enum Settled {
    Sent(serde_json::Value),
    Failed {
        status: StatusCode,
        message: String,
        code: Option<&'static str>,
    },
}

impl Settled {
    fn failed(e: &ApiError) -> Self {
        Settled::Failed {
            status: e.status,
            message: e.message.clone(),
            code: e.code,
        }
    }

    fn lost() -> Self {
        Settled::Failed {
            status: StatusCode::BAD_GATEWAY,
            message: SEND_TASK_LOST.into(),
            code: None,
        }
    }

    /// The answer, as the first request had it or (`repeat`) as a repeat
    /// does: the same JSON with `"deduplicated": true`.
    fn answer(self, repeat: bool) -> Result<serde_json::Value, ApiError> {
        match self {
            Settled::Sent(mut v) => {
                if repeat && let Some(o) = v.as_object_mut() {
                    o.insert("deduplicated".into(), serde_json::Value::Bool(true));
                }
                Ok(v)
            }
            Settled::Failed {
                status,
                message,
                code,
            } => {
                let mut e = ApiError::new(status, message);
                e.code = code;
                Err(e)
            }
        }
    }
}

enum Entry {
    InFlight(watch::Receiver<Option<Settled>>),
    Done(Instant, Settled),
}

/// The sends in flight and the recently settled, by key.
#[derive(Default)]
pub(crate) struct SendDedupe {
    map: Mutex<HashMap<Key, Entry>>,
}

enum Claim {
    First(Holder),
    Wait(watch::Receiver<Option<Settled>>),
    Done(Settled),
}

/// The first request's hold on its key. Settled once; dropped unsettled (the
/// task panicked) it settles as [`SEND_TASK_LOST`], kept for the window.
struct Holder {
    dedupe: Arc<SendDedupe>,
    key: Key,
    tx: Option<watch::Sender<Option<Settled>>>,
}

impl Holder {
    /// `keep`: the answer stands for the window (sent, or in doubt). A plain
    /// refusal is forgotten, so the user can fix it and send again.
    fn settle(&mut self, settled: Settled, keep: bool) {
        let Some(tx) = self.tx.take() else { return };
        if let Ok(mut m) = self.dedupe.map.lock() {
            if keep {
                m.insert(self.key, Entry::Done(Instant::now(), settled.clone()));
            } else {
                m.remove(&self.key);
            }
        }
        let _ = tx.send(Some(settled));
    }
}

impl Drop for Holder {
    fn drop(&mut self) {
        self.settle(Settled::lost(), true);
    }
}

impl SendDedupe {
    fn claim(self: &Arc<Self>, key: Key) -> Claim {
        let now = Instant::now();
        let mut m = match self.map.lock() {
            Ok(m) => m,
            Err(p) => p.into_inner(),
        };
        m.retain(|_, e| !matches!(e, Entry::Done(at, _) if now.duration_since(*at) >= WINDOW));
        match m.get(&key) {
            Some(Entry::Done(_, s)) => Claim::Done(s.clone()),
            Some(Entry::InFlight(rx)) => Claim::Wait(rx.clone()),
            None => {
                let (tx, rx) = watch::channel(None);
                m.insert(key, Entry::InFlight(rx));
                Claim::First(Holder {
                    dedupe: self.clone(),
                    key,
                    tx: Some(tx),
                })
            }
        }
    }

    /// RUN `work` ONCE PER KEY: on its own task (dropping the returned future
    /// stops the waiting, never the work), unless the same key is in flight
    /// (await it) or settled within [`WINDOW`] (its answer, at once).
    ///
    /// `work` answers the response JSON, or the error and whether it stands
    /// for the window (`true` for "may have been sent").
    pub(crate) async fn run_once<F>(
        self: &Arc<Self>,
        key: Key,
        work: F,
    ) -> Result<serde_json::Value, ApiError>
    where
        F: std::future::Future<Output = Result<serde_json::Value, (ApiError, bool)>>
            + Send
            + 'static,
    {
        match self.claim(key) {
            Claim::Done(s) => s.answer(true),
            Claim::Wait(mut rx) => match rx.wait_for(|v| v.is_some()).await {
                Ok(v) => v.clone().unwrap_or_else(Settled::lost).answer(true),
                Err(_) => Settled::lost().answer(true),
            },
            Claim::First(mut holder) => {
                let task = tokio::spawn(async move {
                    let r = work.await;
                    match &r {
                        Ok(v) => holder.settle(Settled::Sent(v.clone()), true),
                        Err((e, keep)) => holder.settle(Settled::failed(e), *keep),
                    }
                    r
                });
                match task.await {
                    Ok(r) => r.map_err(|(e, _)| e),
                    Err(_) => {
                        eprintln!("squelch-api: a detached send task failed");
                        Settled::lost().answer(false)
                    }
                }
            }
        }
    }
}

/// An address list as a set: split on commas, trimmed, lowercased, sorted,
/// empties dropped. `"B@x, a@x"` and `"a@x,b@x"` are one audience.
fn normalized_list(list: &Option<String>) -> Option<String> {
    list.as_ref().map(|l| {
        let mut v: Vec<String> = l
            .split(',')
            .map(|a| a.trim().to_ascii_lowercase())
            .filter(|a| !a.is_empty())
            .collect();
        v.sort();
        v.dedup();
        v.join(",")
    })
}

/// THE DEDUPE KEY: sha256 of the account and the whole request, with its
/// To, Cc and Bcc read as sets. Everything else (subject, body and its
/// format, the parent or original, the staged files' ids, the draft, the
/// group, the tracker, the guard override) is as sent: any change is
/// another message.
pub(crate) fn send_key(account_id: i64, body: &SendBody) -> Key {
    let mut b = body.clone();
    b.to = normalized_list(&body.to);
    b.cc = normalized_list(&body.cc);
    b.bcc = normalized_list(&body.bcc);
    let json = serde_json::to_vec(&b).unwrap_or_default();
    let mut h = Sha256::new();
    h.update(account_id.to_be_bytes());
    h.update([0u8]);
    h.update(&json);
    h.finalize().into()
}

#[cfg(test)]
mod tests;
