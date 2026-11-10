//! The operator's pager: one Slack incoming webhook, one line per signup that
//! ended somewhere a human has to look, and one per new waitlist entry.
//!
//! WHY THIS EXISTS. On 2026-09-28 the `tenants` quota was full, a signup's pod
//! was refused at admission, and the only record was a WARN and two ERRORs in
//! this service's log. The operator found out because the person
//! signing up said so. Every state this module announces is one that nothing
//! else in the system will heal: an orphaned half-signup, a running tenant with
//! no control row. Those should reach a phone, not wait in a log.
//!
//! FIRE AND FORGET. [`AlertClient::notify`] spawns the post and returns: a
//! signup never waits on Slack, and a Slack outage never fails a signup. A post
//! that does not land is a WARN in this service's log, which is exactly where
//! the event would have been without this module.
//!
//! WHAT A MESSAGE MAY CARRY: the label and the virtual-key IDS (never the
//! keys), the error's `Display` (every variant in [`crate::warden`] is written
//! to be safe in a log line), and a command to run. NEVER the account address:
//! a webhook is a third party, and the label is already enough to find
//! everything else from a shell. The builders below take no argument an
//! address could arrive through, which is the whole enforcement.
//!
//! The URL is the secret (a Slack webhook needs no other credential), so
//! [`AlertClient`] derives nothing and [`AlertError`] names nothing, the way
//! [`crate::resend`] handles its API key.
//!
//! COPY RULES (house style): no em dashes, here or anywhere a person reads.

use std::time::Duration;

use serde::Serialize;

/// Budget for one post. Short, because nothing waits on it and a webhook that
/// takes longer than this is not going to answer.
pub const ALERT_TIMEOUT: Duration = Duration::from_secs(5);

/// Ceiling on a response body. Slack answers `ok`; anything bigger is read up
/// to this and thrown away.
const MAX_RESPONSE_BODY: usize = 16 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum AlertError {
    /// Not reached, timed out, or answered something unreadable.
    #[error("the alert webhook could not be reached")]
    Unreachable,
    /// Reached, and it said no: a revoked webhook, a deleted channel, a bad
    /// payload. Retrying the same post changes nothing.
    #[error("the alert webhook refused the message")]
    Refused,
}

/// Slack's incoming-webhook body. `text` is rendered as mrkdwn.
#[derive(Serialize)]
struct Payload<'a> {
    text: &'a str,
}

/// The webhook client. DELIBERATELY DERIVES NOTHING: `url` is the credential.
pub struct AlertClient {
    url: String,
    http: reqwest::Client,
}

impl AlertClient {
    /// `url` is validated by [`crate::config`] (https, a host, no userinfo).
    pub fn new(url: String, timeout: Duration) -> Result<Self, AlertError> {
        let http = reqwest::Client::builder()
            // Redirects refused: the URL is the secret, and following one would
            // hand the message to a host nobody configured.
            .redirect(reqwest::redirect::Policy::none())
            .timeout(timeout)
            .build()
            .map_err(|_| AlertError::Unreachable)?;
        Ok(Self { url, http })
    }

    /// Post one message and wait for the answer. [`Self::notify`] is what a
    /// handler calls; this is the part a test can await.
    pub async fn post(&self, text: &str) -> Result<(), AlertError> {
        let resp = self
            .http
            .post(&self.url)
            .json(&Payload { text })
            .send()
            .await
            .map_err(|_| AlertError::Unreachable)?;
        let ok = resp.status().is_success();
        read_capped(resp).await?;
        if ok { Ok(()) } else { Err(AlertError::Refused) }
    }

    /// Post in the background. Returns at once; a failure is logged, without
    /// the URL, and goes nowhere else.
    pub fn notify(self: &std::sync::Arc<Self>, text: String) {
        let this = self.clone();
        tokio::spawn(async move {
            if let Err(e) = this.post(&text).await {
                tracing::warn!(error = %e, "operator alert not delivered");
            }
        });
    }
}

async fn read_capped(mut resp: reqwest::Response) -> Result<(), AlertError> {
    let mut seen = 0usize;
    while let Some(chunk) = resp.chunk().await.map_err(|_| AlertError::Unreachable)? {
        seen += chunk.len();
        if seen > MAX_RESPONSE_BODY {
            return Err(AlertError::Unreachable);
        }
    }
    Ok(())
}

/// The VK ids a failed signup minted, as one line, or nothing when it minted
/// none (the gateway is off, or the mint itself failed).
fn orphan_line(vk_id: Option<&str>, assistant_vk_id: Option<&str>) -> String {
    let ids: Vec<String> = [("triage", vk_id), ("assistant", assistant_vk_id)]
        .into_iter()
        .filter_map(|(kind, id)| id.map(|id| format!("{kind} `{id}`")))
        .collect();
    if ids.is_empty() {
        String::new()
    } else {
        format!("\nOrphaned VKs, revoke in Bifrost: {}", ids.join(", "))
    }
}

/// Phase two failed: the tenant is pending (or further along than control
/// knows) and nobody will finish it. Tonight's shape.
///
/// BOTH PLACES TO LOOK, events first, because on 2026-09-28 the warden's log
/// had nothing and the answer (`exceeded quota`) was an event on the
/// ReplicaSet.
pub fn signup_incomplete(
    label: &str,
    error: &str,
    vk_id: Option<&str>,
    assistant_vk_id: Option<&str>,
) -> String {
    format!(
        ":warning: *Signup half-finished:* `{label}`\n\
         {error}\n\
         Check: `kubectl -n tenants get events | grep {label}` and \
         `kubectl -n warden logs deploy/squelch-warden | grep -E 'tenant={label}($| )'`\n\
         If its pod comes up Ready, the mailbox exists with no control row: insert the \
         `tenants` row, then `squelch-control llm mint {label}` and `share mint {label}`.{}",
        orphan_line(vk_id, assistant_vk_id)
    )
}

/// Phase two answered 409: a second tab or a retry raced the first. Usually
/// harmless, and the orphaned keys are the part worth a human.
pub fn signup_already_provisioned(
    label: &str,
    vk_id: Option<&str>,
    assistant_vk_id: Option<&str>,
) -> String {
    format!(
        ":information_source: *Signup hit an already-set-up mailbox:* `{label}`\n\
         Two tabs or a retry. Check the person can sign in from Passband.{}",
        orphan_line(vk_id, assistant_vk_id)
    )
}

/// The daemon is running and control has no row for it. The worst of the
/// three: the person has a mailbox they cannot sign in to.
pub fn signup_not_recorded(
    label: &str,
    vk_id: Option<&str>,
    assistant_vk_id: Option<&str>,
) -> String {
    format!(
        ":rotating_light: *Provisioned but not recorded:* `{label}`\n\
         The tenant is running with no control row, so Passband sign-in cannot find it.\n\
         Fix: insert the `tenants` row, then `squelch-control llm mint {label}` and \
         `squelch-control share mint {label}`.{}",
        orphan_line(vk_id, assistant_vk_id)
    )
}

/// A signup that went all the way through.
pub fn signup_provisioned(label: &str) -> String {
    format!(":tada: New tenant provisioned: `{label}`")
}

/// A new address landed on the waitlist. Carries no address and no name, for
/// the reason the module header gives: the admin page is where those live.
/// Sent only for a NEW row, so resubmitting the form cannot page anybody.
pub fn waitlist_joined(public_url: &str) -> String {
    format!(
        ":wave: Someone joined the waitlist. <{public_url}/admin/users|Review on the admin page>"
    )
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::{
        Router, extract::State as AxumState, http::StatusCode, response::IntoResponse,
        routing::post,
    };
    use serde_json::Value;

    use super::*;

    #[derive(Default)]
    struct Recorder {
        bodies: Vec<Value>,
        status: Option<u16>,
    }

    type Shared = Arc<Mutex<Recorder>>;

    async fn spawn_webhook(rec: Shared) -> String {
        let app = Router::new()
            .route(
                "/services/T0/B0/secret",
                post(
                    |AxumState(rec): AxumState<Shared>, body: String| async move {
                        let mut r = rec.lock().unwrap();
                        r.bodies
                            .push(serde_json::from_str(&body).unwrap_or(Value::Null));
                        let status = r.status.unwrap_or(200);
                        (StatusCode::from_u16(status).unwrap(), "ok").into_response()
                    },
                ),
            )
            .route(
                "/redirect",
                post(|| async {
                    (
                        StatusCode::TEMPORARY_REDIRECT,
                        [("location", "/services/T0/B0/secret")],
                    )
                        .into_response()
                }),
            )
            .with_state(rec);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn posts_slack_text_and_maps_the_answer() {
        let rec: Shared = Arc::default();
        let base = spawn_webhook(rec.clone()).await;
        let client =
            AlertClient::new(format!("{base}/services/T0/B0/secret"), ALERT_TIMEOUT).unwrap();

        client.post("hello `ethan`").await.unwrap();
        assert_eq!(rec.lock().unwrap().bodies[0]["text"], "hello `ethan`");

        rec.lock().unwrap().status = Some(404);
        assert!(matches!(client.post("x").await, Err(AlertError::Refused)));
    }

    /// A redirect is a refusal, not a second hop: the message goes to the one
    /// URL that was configured or nowhere.
    #[tokio::test]
    async fn does_not_follow_a_redirect() {
        let rec: Shared = Arc::default();
        let base = spawn_webhook(rec.clone()).await;
        let client = AlertClient::new(format!("{base}/redirect"), ALERT_TIMEOUT).unwrap();
        assert!(matches!(client.post("x").await, Err(AlertError::Refused)));
        assert!(rec.lock().unwrap().bodies.is_empty());
    }

    #[tokio::test]
    async fn an_unreachable_webhook_is_unreachable() {
        let client = AlertClient::new("http://127.0.0.1:1/hook".into(), ALERT_TIMEOUT).unwrap();
        assert!(matches!(
            client.post("x").await,
            Err(AlertError::Unreachable)
        ));
    }

    /// The error names nothing: no URL, which is the credential.
    #[test]
    fn errors_name_nothing() {
        for e in [AlertError::Unreachable, AlertError::Refused] {
            let s = e.to_string();
            assert!(!s.contains("http"), "{s}");
            assert!(!s.contains("hooks.slack.com"), "{s}");
        }
    }

    /// Every message a handler can send, with the real warden error text in
    /// it: the label and the ids are there, an address never is, and neither
    /// is an em dash.
    #[test]
    fn messages_carry_the_label_and_ids_and_no_address() {
        let err = crate::warden::WardenError::TimedOut {
            label: "ethan".into(),
        }
        .to_string();
        let msgs = [
            signup_incomplete("ethan", &err, Some("vk-1"), Some("vk-2")),
            signup_already_provisioned("ethan", Some("vk-1"), None),
            signup_not_recorded("ethan", None, Some("vk-2")),
            signup_provisioned("ethan"),
        ];
        for m in &msgs {
            assert!(m.contains("`ethan`"), "{m}");
            assert!(!m.contains('@'), "an address-shaped token in: {m}");
            assert!(!m.contains('\u{2014}'), "em dash in: {m}");
        }
        assert!(msgs[0].contains("get events") && msgs[0].contains("`vk-1`"));
        assert!(msgs[0].contains("`vk-2`"));
        assert!(!msgs[2].contains("`vk-1`") && msgs[2].contains("`vk-2`"));
        // No keys minted, no orphan line.
        assert!(!signup_incomplete("ethan", &err, None, None).contains("Orphaned"));

        let w = waitlist_joined("https://signup.passband.app");
        assert!(w.contains("https://signup.passband.app/admin/users"), "{w}");
        assert!(!w.contains('@') && !w.contains('\u{2014}'), "{w}");
    }
}
