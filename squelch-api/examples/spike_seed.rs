//! Seeds a SQLite store with a handful of synthetic messages for the web-client
//! spikes, so `squelch-api` (the loopback dev binary) can serve a real
//! `/client/*` tree with no mailbox behind it.
//!
//!   cargo run -p squelch-api --example spike_seed -- /path/to/spike.db
//!   SQUELCH_DB_PATH=/path/to/spike.db SQUELCH_ACCOUNT_EMAIL=me@example.com \
//!     SQUELCH_API_TOKEN=spike-token cargo run -p squelch-api --bin squelch-api
//!
//! Every person, address and body here is invented (`.example` domains only).

use squelch_core::config::Stage1Config;
use squelch_core::store::{SqliteStore, Store};
use squelch_core::sync::ingest::{RawFetched, ingest};

/// A 1x1 transparent PNG.
const PNG_B64: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=";

fn raw(from: &str, subject: &str, body: &str) -> Vec<u8> {
    format!(
        "From: {from}\r\nTo: me@example.com\r\nSubject: {subject}\r\nMessage-ID: <{}@example.com>\r\nMIME-Version: 1.0\r\n{body}",
        subject.replace(' ', "-").to_lowercase()
    )
    .into_bytes()
}

fn messages() -> Vec<(&'static str, Vec<u8>)> {
    let html_with_images = format!(
        "Content-Type: multipart/mixed; boundary=\"outer\"\r\n\r\n\
--outer\r\nContent-Type: multipart/related; boundary=\"rel\"\r\n\r\n\
--rel\r\nContent-Type: text/html; charset=utf-8\r\n\r\n\
<html><body><h1>Your order shipped</h1>\
<p>Logo: <img src=\"cid:logo1\" alt=\"logo\" width=\"40\" height=\"40\"></p>\
<p>Remote: <img src=\"https://images.shop.example/banner.png\" alt=\"banner\" width=\"300\" height=\"80\"></p>\
<p><a href=\"https://shop.example/track/123\">Track it</a></p></body></html>\r\n\
--rel\r\nContent-Type: image/png\r\nContent-ID: <logo1>\r\nContent-Transfer-Encoding: base64\r\n\r\n{PNG_B64}\r\n\
--rel--\r\n\
--outer\r\nContent-Type: image/png; name=\"receipt.png\"\r\nContent-Disposition: attachment; filename=\"receipt.png\"\r\nContent-Transfer-Encoding: base64\r\n\r\n{PNG_B64}\r\n\
--outer--\r\n"
    );
    vec![
        (
            "t-plain",
            raw(
                "Alice Example <alice@example.com>",
                "Lunch on Thursday",
                "Content-Type: text/plain; charset=utf-8\r\n\r\nAre we still on for Thursday?\r\n",
            ),
        ),
        (
            "t-shop",
            raw(
                "Shop <orders@shop.example>",
                "Your order shipped",
                &html_with_images,
            ),
        ),
        (
            "t-reply",
            raw(
                "Bob Example <bob@example.com>",
                "Re: Plans",
                "Content-Type: text/html; charset=utf-8\r\n\r\n<div>Sounds good.</div>\
<div class=\"gmail_quote\"><div>On Mon, Bob wrote:</div><blockquote>Earlier text</blockquote></div>\r\n",
            ),
        ),
    ]
}

fn main() -> anyhow::Result<()> {
    let path = std::env::args()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("usage: spike_seed <db path> [--pair-code]"))?;
    let store = SqliteStore::open(&path)?;
    let acct = store.ensure_account("me@example.com")?;
    // `--pair-code`: mint one pairing code, print only it, seed nothing.
    if std::env::args().nth(2).as_deref() == Some("--pair-code") {
        let minted = store.mint_pairing_code(
            acct,
            chrono::Duration::seconds(squelch_core::store::sqlite::device_tokens::PAIRING_TTL_SECS),
        )?;
        println!("{}", minted.code);
        return Ok(());
    }
    for (i, (thread, bytes)) in messages().into_iter().enumerate() {
        let fetched = RawFetched {
            account_id: acct,
            gmail_msg_id: format!("spike-{i}"),
            gmail_thread_id: Some(thread.to_string()),
            raw: bytes,
            internal_date: Some(chrono::Utc::now() - chrono::Duration::minutes(i as i64 * 7)),
            is_sent: false,
            is_spam: false,
            account_addr: "me@example.com".to_string(),
        };
        let triaged = ingest(
            &fetched,
            &Stage1Config::default(),
            chrono::Utc::now(),
            |_| false,
        );
        let id = store.ingest_message(&triaged)?;
        println!("seeded message {id} in thread {thread}");
    }
    Ok(())
}
