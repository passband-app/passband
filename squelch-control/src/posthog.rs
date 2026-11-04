//! Names a hosted person's PostHog profile: one `$set` carrying their email and
//! name, keyed by their `analytics_id`.
//!
//! The apps report under that id once a hosted pairing hands it over (see
//! `Analytics.adopt` in the Mac/iOS client), so this is what turns an opaque
//! id in PostHog's person list and session replays into a person the operator
//! can recognize. HOSTED ONLY: this crate knows nobody else, and the apps never
//! send an address themselves. Their closed-vocabulary guarantee is untouched.
//!
//! The project key is PostHog's PUBLIC ingest key, the same one baked into the
//! apps: it can write events and read nothing, so it is a constant here, not a
//! secret and not an environment variable. The origin is pinned in
//! [`crate::config`] for the reason every other outbound origin is.
//!
//! Same outbound discipline as the rest of the crate: redirects refused, one
//! timeout, response bodies capped, and errors whose `Display` names nothing.
//! NOTHING HERE LOGS the address or the name. Callers log the row id.
//!
//! Deleting an account deletes this person in PostHog by hand; see
//! `deploy/hosted/PRODUCTION.md`, "Erasing a tenant".

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Map, Value, json};

/// PostHog's US ingest origin. `from_env` pins it; tests point at a mock.
pub const POSTHOG_URL: &str = "https://us.i.posthog.com";

/// The project's public ingest key (`phc_`), identical to the apps' own.
pub const POSTHOG_PROJECT_KEY: &str = "phc_uuYkpwXQYxrz33f4bQsi5ozAQ6LL5kPsnrnU7vVvshni";

pub const POSTHOG_TIMEOUT: Duration = Duration::from_secs(5);

/// PostHog answers a batch with `{"status": 1}`. Anything bigger is not the
/// API we know.
const MAX_RESPONSE_BODY: usize = 16 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum PostHogError {
    #[error("posthog could not be reached")]
    Unreachable,
    #[error("posthog refused the identify")]
    Refused,
}

/// Who a PostHog person is. No `Debug`: it holds an address and a name.
#[derive(Clone)]
pub struct HostedIdentity {
    pub analytics_id: String,
    pub email: String,
    pub name: Option<String>,
}

/// No `Debug` derive, matching every client in this crate.
pub struct PostHogClient {
    url: String,
    key: String,
    http: reqwest::Client,
}

impl PostHogClient {
    pub fn new(url: String, key: String, timeout: Duration) -> Result<Self, PostHogError> {
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(timeout)
            .build()
            .map_err(|_| PostHogError::Unreachable)?;
        Ok(Self { url, key, http })
    }

    /// Set `email` and `name` on the person PostHog knows by `analytics_id`.
    /// Last write wins, so a changed mailbox or name simply replaces the old.
    pub async fn identify(&self, who: &HostedIdentity) -> Result<(), PostHogError> {
        let resp = self
            .http
            .post(format!("{}/batch/", self.url))
            .json(&body(&self.key, who))
            .send()
            .await
            .map_err(|_| PostHogError::Unreachable)?;
        let ok = resp.status().is_success();
        read_capped(resp).await?;
        if ok {
            Ok(())
        } else {
            Err(PostHogError::Refused)
        }
    }

    /// Fire and forget, for the signup path: analytics never makes a person
    /// wait, and a failure costs a name on a dashboard, which the backfill
    /// command (`squelch-control analytics identify`) repairs.
    pub fn identify_detached(self: &Arc<Self>, user_id: i64, who: HostedIdentity) {
        let this = self.clone();
        tokio::spawn(async move {
            if let Err(e) = this.identify(&who).await {
                tracing::warn!(error = %e, user_id, "posthog identify not delivered");
            }
        });
    }
}

fn body(key: &str, who: &HostedIdentity) -> Value {
    let mut set = Map::new();
    set.insert("email".into(), Value::String(who.email.clone()));
    if let Some(name) = who.name.as_deref().filter(|n| !n.is_empty()) {
        set.insert("name".into(), Value::String(name.to_owned()));
    }
    set.insert("hosted".into(), Value::Bool(true));
    json!({
        "api_key": key,
        "batch": [{
            "event": "$set",
            "distinct_id": who.analytics_id,
            "properties": {
                "$set": set,
                "$lib": "squelch-control",
            },
        }],
    })
}

async fn read_capped(mut resp: reqwest::Response) -> Result<(), PostHogError> {
    let mut seen = 0usize;
    while let Some(chunk) = resp.chunk().await.map_err(|_| PostHogError::Unreachable)? {
        seen += chunk.len();
        if seen > MAX_RESPONSE_BODY {
            return Err(PostHogError::Unreachable);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ada(name: Option<&str>) -> HostedIdentity {
        HostedIdentity {
            analytics_id: "0e51e11e-89ff-4c8f-a566-95a8f9b169b0".into(),
            email: "ada@example.com".into(),
            name: name.map(str::to_owned),
        }
    }

    #[test]
    fn the_body_sets_email_and_name_on_the_analytics_id() {
        let b = body("phc_test", &ada(Some("Ada Lovelace")));
        assert_eq!(b["api_key"], "phc_test");
        let e = &b["batch"][0];
        assert_eq!(e["event"], "$set");
        assert_eq!(e["distinct_id"], "0e51e11e-89ff-4c8f-a566-95a8f9b169b0");
        assert_eq!(e["properties"]["$set"]["email"], "ada@example.com");
        assert_eq!(e["properties"]["$set"]["name"], "Ada Lovelace");
        assert_eq!(e["properties"]["$set"]["hosted"], true);
    }

    #[test]
    fn a_missing_or_empty_name_is_left_off_rather_than_blanking_one() {
        for name in [None, Some("")] {
            let b = body("phc_test", &ada(name));
            assert!(
                b["batch"][0]["properties"]["$set"].get("name").is_none(),
                "{b}"
            );
        }
    }
}
