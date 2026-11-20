use super::*;

use std::sync::atomic::{AtomicUsize, Ordering};

fn body(json: serde_json::Value) -> SendBody {
    serde_json::from_value(json).unwrap()
}

/// A send that counts itself and takes `ms` to go.
fn counted(
    runs: &Arc<AtomicUsize>,
    ms: u64,
) -> impl std::future::Future<Output = Result<serde_json::Value, (ApiError, bool)>> + Send + 'static
{
    let runs = runs.clone();
    async move {
        let n = runs.fetch_add(1, Ordering::SeqCst) + 1;
        tokio::time::sleep(Duration::from_millis(ms)).await;
        Ok(serde_json::json!({ "sent": true, "echo_message_id": n }))
    }
}

fn key(n: u8) -> Key {
    [n; 32]
}

/// TWO IDENTICAL REQUESTS AT ONCE ARE ONE SEND: the second awaits the
/// first and gets its answer, marked deduplicated.
#[tokio::test]
async fn a_concurrent_duplicate_awaits_the_first_and_sends_nothing() {
    let d = Arc::new(SendDedupe::default());
    let runs = Arc::new(AtomicUsize::new(0));
    let (a, b) = tokio::join!(
        d.run_once(key(1), counted(&runs, 200)),
        d.run_once(key(1), counted(&runs, 200)),
    );
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_eq!(a["echo_message_id"], b["echo_message_id"]);
    assert!(a.get("deduplicated").is_none(), "{a}");
    assert_eq!(b["deduplicated"], true);
}

/// A REPEAT WITHIN THE WINDOW gets the earlier answer at once; another
/// message (another key) sends.
#[tokio::test]
async fn a_repeat_within_the_window_is_answered_and_another_message_sends() {
    let d = Arc::new(SendDedupe::default());
    let runs = Arc::new(AtomicUsize::new(0));
    let first = d.run_once(key(2), counted(&runs, 0)).await.unwrap();
    let again = d.run_once(key(2), counted(&runs, 0)).await.unwrap();
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    assert_eq!(again["echo_message_id"], first["echo_message_id"]);
    assert_eq!(again["deduplicated"], true);

    d.run_once(key(3), counted(&runs, 0)).await.unwrap();
    assert_eq!(runs.load(Ordering::SeqCst), 2, "a different message sends");

    // Past the window it is a new send.
    if let Some(Entry::Done(at, _)) = d.map.lock().unwrap().get_mut(&key(2)) {
        *at -= WINDOW + Duration::from_secs(1);
    }
    d.run_once(key(2), counted(&runs, 0)).await.unwrap();
    assert_eq!(runs.load(Ordering::SeqCst), 3);
}

/// A REFUSAL IS FORGOTTEN (fix it and send again); a send in doubt STANDS:
/// its repeat gets the same error and nothing is sent.
#[tokio::test]
async fn a_refusal_can_be_retried_and_a_maybe_sent_cannot() {
    let d = Arc::new(SendDedupe::default());
    let runs = Arc::new(AtomicUsize::new(0));
    let fail = |keep: bool, runs: &Arc<AtomicUsize>| {
        let runs = runs.clone();
        async move {
            runs.fetch_add(1, Ordering::SeqCst);
            Err((ApiError::new(StatusCode::BAD_GATEWAY, "nope"), keep))
        }
    };
    assert!(d.run_once(key(4), fail(false, &runs)).await.is_err());
    assert!(d.run_once(key(4), fail(false, &runs)).await.is_err());
    assert_eq!(
        runs.load(Ordering::SeqCst),
        2,
        "a refusal is not remembered"
    );

    let e = d.run_once(key(5), fail(true, &runs)).await.unwrap_err();
    let again = d.run_once(key(5), counted(&runs, 0)).await.unwrap_err();
    assert_eq!(runs.load(Ordering::SeqCst), 3, "the repeat sent nothing");
    assert_eq!((again.status, again.message), (e.status, e.message));
}

/// A SEND TASK THAT PANICS is "may have been sent", never a plain 500, for
/// the request and every repeat; nothing is sent again.
#[tokio::test]
async fn a_panicked_send_is_maybe_sent_and_never_resent() {
    let d = Arc::new(SendDedupe::default());
    let runs = Arc::new(AtomicUsize::new(0));
    let r = runs.clone();
    let e = d
        .run_once(key(6), async move {
            r.fetch_add(1, Ordering::SeqCst);
            if r.load(Ordering::SeqCst) > 0 {
                panic!("boom");
            }
            Ok(serde_json::Value::Null)
        })
        .await
        .unwrap_err();
    assert_eq!(e.status, StatusCode::BAD_GATEWAY);
    assert_eq!(e.message, SEND_TASK_LOST);
    let again = d.run_once(key(6), counted(&runs, 0)).await.unwrap_err();
    assert_eq!(again.message, SEND_TASK_LOST);
    assert_eq!(runs.load(Ordering::SeqCst), 1);
}

/// A CLIENT THAT GIVES UP DOES NOT CANCEL THE SEND (correctness review F2):
/// the handler's future is dropped 50 ms in, the send still finishes, and
/// the client's retry, sent while it runs, gets its answer.
#[tokio::test]
async fn dropping_the_request_midway_lets_the_send_finish_for_the_retry() {
    let d = Arc::new(SendDedupe::default());
    let runs = Arc::new(AtomicUsize::new(0));
    assert!(
        tokio::time::timeout(
            Duration::from_millis(50),
            d.run_once(key(7), counted(&runs, 300))
        )
        .await
        .is_err()
    );
    let retry = d.run_once(key(7), counted(&runs, 300)).await.unwrap();
    assert_eq!(retry["deduplicated"], true);
    assert_eq!(runs.load(Ordering::SeqCst), 1, "the send was not cancelled");
}

/// THE KEY: recipients are sets (order, case and spacing do not matter);
/// the body, the subject, the files and the account do.
#[test]
fn the_key_reads_recipients_as_sets_and_everything_else_as_sent() {
    let base = serde_json::json!({
        "to": "Bob@x.test, carol@x.test", "cc": "dan@x.test", "bcc": "eve@x.test",
        "subject": "hi", "body": "hello", "attachment_ids": [3, 4]
    });
    let k = send_key(1, &body(base.clone()));
    let mut same = base.clone();
    same["to"] = "carol@x.test,bob@x.test".into();
    assert_eq!(send_key(1, &body(same)), k);
    for (field, v) in [
        ("body", serde_json::json!("hello!")),
        ("subject", serde_json::json!("hi again")),
        ("bcc", serde_json::json!("eve@x.test, fay@x.test")),
        ("attachment_ids", serde_json::json!([3])),
        ("reply_to_message_id", serde_json::json!(9)),
    ] {
        let mut other = base.clone();
        other[field] = v;
        assert_ne!(send_key(1, &body(other)), k, "{field}");
    }
    assert_ne!(send_key(2, &body(base)), k, "another account");
}
