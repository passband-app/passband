//! The engine's incremental walk on an OUTLOOK account, against a scripted
//! Graph: what Graph's delta changes about the walk (it names changed mail as
//! well as new mail) is asserted on the wire.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use async_trait::async_trait;
use axum::Router;
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde_json::json;

use super::*;
use crate::mail::graph::PREFER_IMMUTABLE_IDS;
use crate::store::SqliteStore;

#[derive(Default)]
struct St {
    base: String,
    seen: Vec<String>,
    unpreferred: usize,
    delta: HashMap<String, VecDeque<Vec<&'static str>>>,
    n: usize,
}

#[derive(Clone, Default)]
struct Mock(Arc<Mutex<St>>);

impl Mock {
    fn note(&self, h: &HeaderMap, what: String) {
        let mut st = self.0.lock().unwrap();
        if h.get("prefer").and_then(|v| v.to_str().ok()) != Some(PREFER_IMMUTABLE_IDS) {
            st.unpreferred += 1;
        }
        st.seen.push(what);
    }
    fn calls(&self, prefix: &str) -> usize {
        self.0
            .lock()
            .unwrap()
            .seen
            .iter()
            .filter(|s| s.starts_with(prefix))
            .count()
    }
}

async fn folder(State(m): State<Mock>, h: HeaderMap, AxumPath(name): AxumPath<String>) -> Response {
    m.note(&h, format!("folder:{name}"));
    axum::Json(json!({"id": format!("F-{name}"), "unreadItemCount": 0})).into_response()
}

async fn delta(
    State(m): State<Mock>,
    h: HeaderMap,
    AxumPath(name): AxumPath<String>,
    Query(_q): Query<HashMap<String, String>>,
) -> Response {
    m.note(&h, format!("delta:{name}"));
    let mut st = m.0.lock().unwrap();
    let ids = st
        .delta
        .get_mut(&name)
        .and_then(|d| d.pop_front())
        .unwrap_or_default();
    st.n += 1;
    axum::Json(json!({
        "value": ids.iter().map(|id| json!({"id": id})).collect::<Vec<_>>(),
        "@odata.deltaLink": format!("{}/me/mailFolders/{name}/messages/delta?$deltatoken=d{}", st.base, st.n),
    }))
    .into_response()
}

async fn meta(State(m): State<Mock>, h: HeaderMap, AxumPath(id): AxumPath<String>) -> Response {
    m.note(&h, format!("meta:{id}"));
    axum::Json(json!({
        "id": id,
        "conversationId": format!("conv-{id}"),
        "receivedDateTime": "2026-10-01T12:00:00Z",
        "parentFolderId": "F-inbox",
    }))
    .into_response()
}

async fn value(State(m): State<Mock>, h: HeaderMap, AxumPath(id): AxumPath<String>) -> Response {
    m.note(&h, format!("value:{id}"));
    format!(
        "From: Ann <ann@example.com>\r\nTo: me@outlook.com\r\nSubject: hello {id}\r\n\
         Message-ID: <{id}@example.com>\r\nDate: Thu, 01 Oct 2026 12:00:00 +0000\r\n\r\nhi\r\n"
    )
    .into_response()
}

async fn serve(m: Mock) -> String {
    let app = Router::new()
        .route("/me/mailFolders/{name}", get(folder))
        .route("/me/mailFolders/{name}/messages/delta", get(delta))
        .route("/me/messages/{id}", get(meta))
        .route("/me/messages/{id}/$value", get(value))
        .fallback(|| async { StatusCode::IM_A_TEAPOT })
        .with_state(m.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let base = format!("http://{addr}");
    m.0.lock().unwrap().base = base.clone();
    base
}

struct FixedToken;

#[async_trait]
impl CredentialStore for FixedToken {
    async fn token(&self, _: AccountId) -> Result<crate::credentials::OAuthToken> {
        Ok(crate::credentials::OAuthToken {
            access_token: "t".into(),
            refresh_token: None,
            expires_at: None,
        })
    }
}

#[tokio::test]
async fn an_outlook_poll_fetches_only_new_mail_and_an_idle_one_says_so() {
    let m = Mock::default();
    let base = serve(m.clone()).await;
    let store = Arc::new(SqliteStore::open_in_memory().unwrap());
    let acct = store.ensure_account("me@outlook.com").unwrap();
    store
        .bind_account_provider(acct, MailProvider::Outlook)
        .unwrap();
    let link = |f: &str| format!("{base}/me/mailFolders/{f}/messages/delta?$deltatoken=start");
    store
        .set_sync_cursor(
            acct,
            &SyncCursor::new(
                json!({"v": 1, "inbox": link("inbox"), "sent": link("sentitems")}).to_string(),
            ),
        )
        .unwrap();
    {
        let mut st = m.0.lock().unwrap();
        st.delta.insert(
            "inbox".into(),
            VecDeque::from([vec!["new-1"], vec!["new-1"], vec![]]),
        );
    }
    let engine = SyncEngine::new(
        store.clone(),
        Arc::new(FixedToken),
        acct,
        "me@outlook.com".into(),
        Config::default(),
    )
    .with_api_base(base.clone());

    // 1. A new message: fetched and stored.
    assert!(engine.poll_once().await.unwrap());
    assert!(store.has_message(acct, "new-1").unwrap());
    assert_eq!(m.calls("value:new-1"), 1);
    let stored = store.sync_cursor(acct).unwrap().unwrap();
    assert!(
        !stored.as_str().contains("deltatoken=start"),
        "both links moved on: {}",
        stored.as_str()
    );

    // 2. The same message again (somebody read it in Outlook): no refetch,
    // and the walk still reports a change, because read state is one.
    assert!(engine.poll_once().await.unwrap());
    assert_eq!(
        m.calls("value:new-1"),
        1,
        "a read-state flip re-downloaded the body"
    );

    // 3. Both rounds empty: idle, which is what lets the lane back off.
    assert!(!engine.poll_once().await.unwrap());

    assert_eq!(m.calls("delta:inbox"), 3);
    assert_eq!(m.calls("delta:sentitems"), 3);
    assert_eq!(
        m.0.lock().unwrap().unpreferred,
        0,
        "a request without ImmutableId"
    );
    assert_eq!(m.calls("folder:"), 5, "folder ids are looked up once");
}
