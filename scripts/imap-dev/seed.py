"""APPEND the IMAP test bed's fixture mail. Run by `scripts/imap-dev.sh seed`.

Usage: seed.py <port> <user> <password>   (always 127.0.0.1, plaintext)

The fixtures are what the IMAP source's tests lean on:
  INBOX  1  a plain message
  INBOX  2  a reply chain's parent ...
  INBOX  3  ... and its reply (In-Reply-To + References to 2)
  INBOX  4  a message with NO Message-ID header
  INBOX  5  a multipart/alternative message
  Sent   1  the account's own reply to message 1
Nothing is flagged \\Seen, so a test can prove a fetch left it unset.

Refuses to run twice: a second seed would duplicate every fixture.
"""

import imaplib
import sys
import time

port, user, password = int(sys.argv[1]), sys.argv[2], sys.argv[3]
ME = user


def msg(headers, body):
    lines = [f"{k}: {v}" for k, v in headers]
    return ("\r\n".join(lines) + "\r\n\r\n" + body.replace("\n", "\r\n")).encode()


INBOX = [
    msg(
        [
            ("From", "Ada Lovelace <ada@example.org>"),
            ("To", ME),
            ("Subject", "Plain fixture"),
            ("Date", "Mon, 26 Oct 2026 09:00:00 +0000"),
            ("Message-ID", "<plain-1@Example.ORG>"),
            ("MIME-Version", "1.0"),
            ("Content-Type", "text/plain; charset=utf-8"),
        ],
        "Hello from the plain fixture.\n",
    ),
    msg(
        [
            ("From", "Grace Hopper <grace@example.net>"),
            ("To", ME),
            ("Subject", "Thread parent"),
            ("Date", "Mon, 26 Oct 2026 10:00:00 +0000"),
            ("Message-ID", "<thread-parent@example.net>"),
            ("MIME-Version", "1.0"),
            ("Content-Type", "text/plain; charset=utf-8"),
        ],
        "Parent of a two-message chain.\n",
    ),
    msg(
        [
            ("From", "Grace Hopper <grace@example.net>"),
            ("To", ME),
            ("Subject", "Re: Thread parent"),
            ("Date", "Mon, 26 Oct 2026 11:00:00 +0000"),
            ("Message-ID", "<thread-reply@example.net>"),
            ("In-Reply-To", "<thread-parent@example.net>"),
            ("References", "<thread-parent@example.net>"),
            ("MIME-Version", "1.0"),
            ("Content-Type", "text/plain; charset=utf-8"),
        ],
        "Reply in the two-message chain.\n",
    ),
    msg(
        [
            ("From", "No Id <noid@example.com>"),
            ("To", ME),
            ("Subject", "No Message-ID here"),
            ("Date", "Tue, 27 Oct 2026 08:00:00 +0000"),
            ("MIME-Version", "1.0"),
            ("Content-Type", "text/plain; charset=utf-8"),
        ],
        "This fixture carries no Message-ID header.\n",
    ),
    msg(
        [
            ("From", "Shop <news@shop.example>"),
            ("To", ME),
            ("Subject", "Multipart fixture"),
            ("Date", "Tue, 27 Oct 2026 12:00:00 +0000"),
            ("Message-ID", "<multi-5@shop.example>"),
            ("MIME-Version", "1.0"),
            ("Content-Type", 'multipart/alternative; boundary="b1"'),
        ],
        "--b1\nContent-Type: text/plain; charset=utf-8\n\nPlain part.\n"
        "--b1\nContent-Type: text/html; charset=utf-8\n\n<p>HTML part.</p>\n--b1--\n",
    ),
]

SENT = [
    msg(
        [
            ("From", ME),
            ("To", "Ada Lovelace <ada@example.org>"),
            ("Subject", "Re: Plain fixture"),
            ("Date", "Mon, 26 Oct 2026 13:00:00 +0000"),
            ("Message-ID", "<sent-1@dev.test>"),
            ("In-Reply-To", "<plain-1@Example.ORG>"),
            ("References", "<plain-1@Example.ORG>"),
            ("MIME-Version", "1.0"),
            ("Content-Type", "text/plain; charset=utf-8"),
        ],
        "My reply to the plain fixture.\n",
    ),
]

imap = imaplib.IMAP4("127.0.0.1", port)
imap.login(user, password)
typ, data = imap.select("INBOX", readonly=True)
if int(data[0]) > 0:
    sys.exit(f"INBOX already has {int(data[0])} messages; stop + start for a clean seed")
for folder, batch in (("INBOX", INBOX), ("Sent", SENT)):
    for raw in batch:
        typ, resp = imap.append(folder, "", imaplib.Time2Internaldate(time.time()), raw)
        if typ != "OK":
            sys.exit(f"APPEND to {folder} failed: {resp}")
print(f"seeded {len(INBOX)} INBOX + {len(SENT)} Sent messages")
imap.logout()
