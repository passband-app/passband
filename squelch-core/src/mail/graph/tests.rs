//! `GraphSource` against a scripted Graph on loopback: the wire is asserted,
//! not a struct, because the properties that matter (immutable ids on every
//! request, never following a foreign link, the expired cursor) only exist on
//! the wire.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::Router;
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde_json::{Value, json};

use super::*;
use crate::credentials::OAuthToken;

/// One scripted delta reply for a folder.
#[derive(Clone)]
enum Delta {
    /// A page: these ids, these removed ids, and whether another page follows.
    Page {
        ids: Vec<&'static str>,
        removed: Vec<&'static str>,
        more: bool,
    },
    Status(u16),
}

#[derive(Default)]
struct St {
    base: String,
    /// Every request's path and query, in order.
    seen: Vec<String>,
    /// Requests that arrived without the immutable-id preference.
    without_prefer: Vec<String>,
    delta: HashMap<String, VecDeque<Delta>>,
    /// Folder listing pages, per folder name.
    lists: HashMap<String, Vec<Value>>,
    /// The all-mail listing's entries.
    all: Vec<Value>,
    raw: HashMap<String, Vec<u8>>,
    meta: HashMap<String, Value>,
    /// Statuses to answer the next request to a path with, before any body.
    errors: HashMap<String, VecDeque<(u16, Option<&'static str>)>>,
    token_n: usize,
}

#[derive(Clone, Default)]
struct Mock(Arc<Mutex<St>>);

fn note(st: &mut St, headers: &HeaderMap, what: String) {
    if headers.get("prefer").and_then(|v| v.to_str().ok())
        != Some(&*format!("{PREFER_IMMUTABLE_IDS}, odata.maxpagesize=100"))
    {
        st.without_prefer.push(what.clone());
    }
    st.seen.push(what);
}

fn scripted_error(st: &mut St, key: &str) -> Option<Response> {
    let (status, retry_after) = st.errors.get_mut(key)?.pop_front()?;
    let mut resp = (
        StatusCode::from_u16(status).unwrap(),
        axum::Json(json!({"error": {"code": "TooManyRequests", "message": "secret detail"}})),
    )
        .into_response();
    if let Some(ra) = retry_after {
        resp.headers_mut()
            .insert("retry-after", ra.parse().unwrap());
    }
    Some(resp)
}

async fn folder(
    State(m): State<Mock>,
    headers: HeaderMap,
    AxumPath(name): AxumPath<String>,
) -> Response {
    let mut st = m.0.lock().unwrap();
    note(&mut st, &headers, format!("folder:{name}"));
    if let Some(r) = scripted_error(&mut st, &format!("folder:{name}")) {
        return r;
    }
    axum::Json(json!({"id": format!("F-{name}"), "unreadItemCount": 7})).into_response()
}

async fn folder_messages(
    State(m): State<Mock>,
    headers: HeaderMap,
    AxumPath(name): AxumPath<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let mut st = m.0.lock().unwrap();
    note(
        &mut st,
        &headers,
        format!(
            "list:{name}:{}",
            q.get("$filter").cloned().unwrap_or_default()
        ),
    );
    let value = st.lists.get(&name).cloned().unwrap_or_default();
    axum::Json(json!({ "value": value })).into_response()
}

async fn delta(
    State(m): State<Mock>,
    headers: HeaderMap,
    AxumPath(name): AxumPath<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let mut st = m.0.lock().unwrap();
    let token = q
        .get("$deltatoken")
        .or(q.get("$skiptoken"))
        .cloned()
        .unwrap_or_else(|| format!("initial:{}", q.get("$filter").cloned().unwrap_or_default()));
    note(&mut st, &headers, format!("delta:{name}:{token}"));
    let reply = st
        .delta
        .get_mut(&name)
        .and_then(|d| d.pop_front())
        .unwrap_or(Delta::Page {
            ids: vec![],
            removed: vec![],
            more: false,
        });
    match reply {
        Delta::Status(s) => StatusCode::from_u16(s).unwrap().into_response(),
        Delta::Page { ids, removed, more } => {
            st.token_n += 1;
            let n = st.token_n;
            let mut value: Vec<Value> = ids.iter().map(|id| json!({"id": id})).collect();
            value.extend(
                removed
                    .iter()
                    .map(|id| json!({"id": id, "@removed": {"reason": "deleted"}})),
            );
            let link = if more {
                json!({"@odata.nextLink": format!("{}/me/mailFolders/{name}/messages/delta?$skiptoken=s{n}", st.base)})
            } else {
                json!({"@odata.deltaLink": format!("{}/me/mailFolders/{name}/messages/delta?$deltatoken=d{n}", st.base)})
            };
            let mut body = json!({ "value": value });
            body.as_object_mut()
                .unwrap()
                .extend(link.as_object().unwrap().clone());
            axum::Json(body).into_response()
        }
    }
}

async fn all_messages(
    State(m): State<Mock>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let mut st = m.0.lock().unwrap();
    note(
        &mut st,
        &headers,
        format!("all:{}", q.get("$filter").cloned().unwrap_or_default()),
    );
    axum::Json(json!({ "value": st.all.clone() })).into_response()
}

async fn message(
    State(m): State<Mock>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<String>,
) -> Response {
    let mut st = m.0.lock().unwrap();
    note(&mut st, &headers, format!("meta:{id}"));
    if let Some(r) = scripted_error(&mut st, &format!("meta:{id}")) {
        return r;
    }
    match st.meta.get(&id) {
        Some(v) => axum::Json(v.clone()).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn message_value(
    State(m): State<Mock>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<String>,
) -> Response {
    let mut st = m.0.lock().unwrap();
    note(&mut st, &headers, format!("value:{id}"));
    match st.raw.get(&id) {
        Some(b) => b.clone().into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn serve(m: Mock) -> String {
    let app = Router::new()
        .route("/me/mailFolders/{name}", get(folder))
        .route("/me/mailFolders/{name}/messages", get(folder_messages))
        .route("/me/mailFolders/{name}/messages/delta", get(delta))
        .route("/me/messages", get(all_messages))
        .route("/me/messages/{id}", get(message))
        .route("/me/messages/{id}/$value", get(message_value))
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
    async fn token(&self, _: AccountId) -> Result<OAuthToken> {
        Ok(OAuthToken {
            access_token: "t".into(),
            refresh_token: None,
            expires_at: None,
        })
    }
}

fn source(base: &str) -> GraphSource<FixedToken> {
    GraphSource::new(GraphSourceParts {
        http: reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap(),
        creds: Arc::new(FixedToken),
        account_id: 1,
        account_email: "Me@Outlook.com".into(),
        metrics: SyncMetrics::new(),
        refusals: Arc::new(SourceRefusals::default()),
        api_base: base.to_string(),
        quota_retry_cap: Duration::from_secs(5),
    })
}

async fn setup() -> (Mock, GraphSource<FixedToken>) {
    let m = Mock::default();
    let base = serve(m.clone()).await;
    (m, source(&base))
}

fn seen(m: &Mock) -> Vec<String> {
    m.0.lock().unwrap().seen.clone()
}

fn assert_all_immutable(m: &Mock) {
    let st = m.0.lock().unwrap();
    assert!(!st.seen.is_empty(), "nothing reached the mock");
    assert!(
        st.without_prefer.is_empty(),
        "requests without the ImmutableId preference: {:?}",
        st.without_prefer
    );
}

#[tokio::test]
async fn a_fresh_cursor_costs_no_request_and_reaches_back_the_overlap() {
    let (m, src) = setup().await;
    let before = Utc::now() - CURSOR_OVERLAP;
    let cursor = src.current_cursor().await.unwrap();
    assert!(seen(&m).is_empty());
    assert!(src.cursor_is_usable(&cursor));
    let c = GraphCursor::decode(&cursor).unwrap();
    for (link, folder) in [(&c.inbox, "inbox"), (&c.sent, "sentitems")] {
        assert!(link.contains(&format!("/me/mailFolders/{folder}/messages/delta?")));
        assert!(link.contains("receivedDateTime%20ge%20"), "{link}");
    }
    // The filter's instant is within a second or two of now minus the overlap.
    let first = src.changes(&cursor, Folder::Inbox).await.unwrap();
    assert!(first.ids.is_empty());
    let s = seen(&m);
    let stamp = s[0].rsplit("ge ").next().unwrap();
    let at = DateTime::parse_from_rfc3339(stamp)
        .unwrap()
        .with_timezone(&Utc);
    assert!((at - before).num_seconds().abs() <= 2, "{at} vs {before}");
    assert_all_immutable(&m);
}

#[tokio::test]
async fn a_walk_follows_next_links_skips_removals_and_keeps_the_delta_link() {
    let (m, src) = setup().await;
    m.0.lock().unwrap().delta.insert(
        "inbox".into(),
        VecDeque::from([
            Delta::Page {
                ids: vec!["b", "a"],
                removed: vec!["gone"],
                more: true,
            },
            Delta::Page {
                ids: vec!["a", "c"],
                removed: vec![],
                more: false,
            },
        ]),
    );
    let start = src.current_cursor().await.unwrap();
    let ch = src.changes(&start, Folder::Inbox).await.unwrap();
    assert_eq!(ch.ids, vec!["a", "b", "c"]);
    assert!(
        !ch.mailbox_unchanged,
        "one folder proves nothing about another"
    );
    let next = GraphCursor::decode(&ch.cursor).unwrap();
    let was = GraphCursor::decode(&start).unwrap();
    assert!(next.inbox.ends_with("$deltatoken=d2"), "{}", next.inbox);
    assert_eq!(
        next.sent, was.sent,
        "the sent link is not this walk's to move"
    );

    // The next walk resumes from the stored delta link.
    src.changes(&ch.cursor, Folder::Inbox).await.unwrap();
    assert_eq!(seen(&m).last().unwrap(), "delta:inbox:d2");
    assert_all_immutable(&m);
}

#[tokio::test]
async fn gone_is_an_expired_cursor() {
    let (m, src) = setup().await;
    m.0.lock()
        .unwrap()
        .delta
        .insert("sentitems".into(), VecDeque::from([Delta::Status(410)]));
    let start = src.current_cursor().await.unwrap();
    assert!(matches!(
        src.changes(&start, Folder::Sent).await,
        Err(ChangesError::Expired)
    ));
}

#[tokio::test]
async fn a_foreign_link_in_the_cursor_is_never_followed() {
    let (m, src) = setup().await;
    for link in [
        "https://evil.example/v1.0/me/mailFolders/inbox/messages/delta",
        // A prefix trick: the base followed by something other than `/`.
        &format!("{}.evil.example/x", m.0.lock().unwrap().base),
    ] {
        let forged = GraphCursor {
            v: CURSOR_VERSION,
            inbox: link.to_string(),
            sent: link.to_string(),
        }
        .encode();
        assert!(!src.cursor_is_usable(&forged));
        assert!(matches!(
            src.changes(&forged, Folder::Inbox).await,
            Err(ChangesError::Other(CoreError::InvalidInput(_)))
        ));
    }
    assert!(seen(&m).is_empty(), "a request left for a foreign link");
}

#[test]
fn garbage_and_other_versions_are_not_usable_cursors() {
    let src = source("http://127.0.0.1:1");
    for raw in [
        "",
        "12345",
        "{}",
        r#"{"v":2,"inbox":"http://127.0.0.1:1/a","sent":"http://127.0.0.1:1/b"}"#,
    ] {
        assert!(!src.cursor_is_usable(&SyncCursor::new(raw)), "{raw}");
    }
}

#[test]
fn a_merge_takes_each_folder_from_the_walk_that_moved_it() {
    let src = source("http://h");
    let c = |i: &str, s: &str| {
        GraphCursor {
            v: CURSOR_VERSION,
            inbox: i.into(),
            sent: s.into(),
        }
        .encode()
    };
    let since = c("i0", "s0");
    let merged = src.merge_cursors(&since, &c("i1", "s0"), &c("i0", "s1"));
    assert_eq!(merged, c("i1", "s1"));
    // Neither moved: the start stands.
    assert_eq!(src.merge_cursors(&since, &since, &since), since);
}

#[tokio::test]
async fn fetch_raw_reads_mime_bytes_and_places_by_folder() {
    let (m, src) = setup().await;
    {
        let mut st = m.0.lock().unwrap();
        st.raw
            .insert("m+1/x=".into(), b"Subject: hi\r\n\r\nbody".to_vec());
        st.meta.insert(
            "m+1/x=".into(),
            json!({
                "id": "IMM-1",
                "conversationId": "conv-1",
                "receivedDateTime": "2026-10-01T12:00:00Z",
                "parentFolderId": "F-inbox"
            }),
        );
    }
    let msg = src.fetch_raw("m+1/x=", Urgency::Live).await.unwrap();
    assert_eq!(msg.id, "IMM-1", "the provider's own (immutable) id is kept");
    assert_eq!(msg.thread_id.as_deref(), Some("conv-1"));
    assert!(matches!(msg.raw, RawBody::Bytes(ref b) if b.starts_with(b"Subject: hi")));
    assert_eq!(
        msg.placement,
        Placement {
            in_inbox: true,
            in_sent: false
        }
    );
    assert_eq!(
        msg.received_at.unwrap().to_rfc3339(),
        "2026-10-01T12:00:00+00:00"
    );
    // The id with `/`, `+` and `=` reached the mock as one segment, intact.
    assert!(seen(&m).contains(&"value:m+1/x=".to_string()));
    assert_all_immutable(&m);
}

#[tokio::test]
async fn a_missing_message_is_not_found() {
    let (_m, src) = setup().await;
    assert!(matches!(
        src.fetch_raw("nope", Urgency::Live).await,
        Err(CoreError::NotFound)
    ));
}

#[tokio::test]
async fn the_archive_window_leaves_out_drafts_trash_junk_and_with_filed_only_the_live_folders() {
    let (m, src) = setup().await;
    m.0.lock().unwrap().all = vec![
        json!({"id": "in", "parentFolderId": "F-inbox", "isDraft": false}),
        json!({"id": "sent", "parentFolderId": "F-sentitems", "isDraft": false}),
        json!({"id": "filed", "parentFolderId": "F-custom", "isDraft": false}),
        json!({"id": "draft", "parentFolderId": "F-drafts", "isDraft": true}),
        json!({"id": "trash", "parentFolderId": "F-deleteditems", "isDraft": false}),
        json!({"id": "junk", "parentFolderId": "F-junkemail", "isDraft": false}),
    ];
    let from = Utc::now() - chrono::Duration::days(60);
    let to = Utc::now() - chrono::Duration::days(30);
    let all = src
        .list(
            Listing::Window {
                from,
                to,
                filed_only: false,
            },
            None,
            Urgency::Background,
        )
        .await
        .unwrap();
    assert_eq!(all, vec!["in", "sent", "filed"]);
    let filed = src
        .list(
            Listing::Window {
                from,
                to,
                filed_only: true,
            },
            None,
            Urgency::Background,
        )
        .await
        .unwrap();
    assert_eq!(filed, vec!["filed"]);
    let q = seen(&m)
        .into_iter()
        .find(|s| s.starts_with("all:"))
        .unwrap();
    assert!(q.contains("receivedDateTime ge ") && q.contains(" and receivedDateTime lt "));
    assert_all_immutable(&m);
}

#[tokio::test]
async fn a_folder_listing_filters_by_the_window_and_honours_the_cap() {
    let (m, src) = setup().await;
    m.0.lock().unwrap().lists.insert(
        "junkemail".into(),
        vec![json!({"id": "1"}), json!({"id": "2"}), json!({"id": "3"})],
    );
    let ids = src
        .list(
            Listing::Folder {
                folder: Folder::Spam,
                within_days: Some(30),
            },
            Some(2),
            Urgency::Live,
        )
        .await
        .unwrap();
    assert_eq!(ids, vec!["1", "2"]);
    assert!(seen(&m)[0].starts_with("list:junkemail:receivedDateTime ge "));
}

#[tokio::test]
async fn a_window_request_waits_out_a_throttle_in_place_and_a_live_one_does_not() {
    let (m, src) = setup().await;
    m.0.lock().unwrap().errors.insert(
        "folder:inbox".into(),
        VecDeque::from([(429, Some("0")), (429, Some("0"))]),
    );
    // Live: one attempt, the refusal surfaces, with no Graph text in it.
    let err = src.inbox_unread().await.unwrap_err().to_string();
    assert!(
        err.contains("graph_throttled") && !err.contains("secret detail"),
        "{err}"
    );
    // Window: the remaining refusal is waited out and the same request lands.
    let url = format!("{}/me/mailFolders/inbox?$select=id", src.api_base);
    let f: FolderResp = src.get_json(&url, Urgency::Window).await.unwrap();
    assert_eq!(f.id, "F-inbox");
    assert_eq!(seen(&m).iter().filter(|s| *s == "folder:inbox").count(), 3);
}

#[tokio::test]
async fn background_refusals_are_counted_for_the_archive_walk() {
    let (m, src) = setup().await;
    m.0.lock()
        .unwrap()
        .errors
        .insert("meta:x".into(), VecDeque::from([(429, None)]));
    let _ = src.fetch_raw("x", Urgency::Background).await;
    assert_eq!(src.refusals.quota.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn unread_is_the_inbox_item_count_for_both() {
    let (_m, src) = setup().await;
    assert_eq!(src.inbox_unread().await.unwrap(), (7, 7));
}

#[tokio::test]
async fn headers_are_rendered_from_the_structured_fields() {
    let (m, src) = setup().await;
    m.0.lock().unwrap().meta.insert(
        "h".into(),
        json!({
            "toRecipients": [
                {"emailAddress": {"name": "Ann \"A\" Lee", "address": "ann@example.com"}},
                {"emailAddress": {"name": "bob@example.com", "address": "bob@example.com"}}
            ],
            "ccRecipients": [],
            "bccRecipients": [{"emailAddress": {"address": "c@example.com"}}],
            "receivedDateTime": "2026-10-02T00:00:00Z"
        }),
    );
    let h = src.fetch_headers("h", &["To", "Cc", "Bcc"]).await.unwrap();
    assert_eq!(
        h.headers,
        vec![
            (
                "To".to_string(),
                r#""Ann A Lee" <ann@example.com>, bob@example.com"#.to_string()
            ),
            ("Bcc".to_string(), "c@example.com".to_string()),
        ]
    );
    assert!(h.received_at.is_some());
}

#[test]
fn ids_are_encoded_as_one_path_segment() {
    assert_eq!(path_segment("AAk/ALg+AA=="), "AAk%2FALg%2BAA%3D%3D");
    assert_eq!(path_segment("plain-id_1.2~"), "plain-id_1.2~");
}

#[tokio::test]
async fn the_users_own_mail_is_sent_wherever_it_is_filed() {
    let (m, src) = setup().await;
    {
        let mut st = m.0.lock().unwrap();
        st.raw
            .insert("own".into(), b"Subject: mine\r\n\r\nx".to_vec());
        st.meta.insert(
            "own".into(),
            json!({"id": "own", "parentFolderId": "F-archive",
                   "from": {"emailAddress": {"address": "me@outlook.com"}}}),
        );
    }
    let msg = src.fetch_raw("own", Urgency::Background).await.unwrap();
    assert_eq!(
        msg.placement,
        Placement {
            in_inbox: false,
            in_sent: true
        }
    );
}

#[tokio::test]
async fn a_round_of_only_removals_is_a_change_with_nothing_to_fetch() {
    let (m, src) = setup().await;
    m.0.lock().unwrap().delta.insert(
        "inbox".into(),
        VecDeque::from([Delta::Page {
            ids: vec![],
            removed: vec!["gone"],
            more: false,
        }]),
    );
    let start = src.current_cursor().await.unwrap();
    let ch = src.changes(&start, Folder::Inbox).await.unwrap();
    assert!(ch.ids.is_empty());
    assert!(ch.saw_changes, "a deleted unread message moves the count");
    let ch = src.changes(&ch.cursor, Folder::Inbox).await.unwrap();
    assert!(!ch.saw_changes);
}
