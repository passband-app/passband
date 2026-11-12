"""Append N synthetic notification events to a spike daemon's SQLite store.

Each event needs its own message (events.message_id is UNIQUE), so this clones
message 1 under a fresh gmail id and thread, then appends an events row for it.
Synthetic data only (.example addresses). Run with python3 -I.

    python3 -I add_events.py <db> [count]
Prints the new event ids, one per line.
"""
import sqlite3, sys, time, datetime

db, n = sys.argv[1], int(sys.argv[2]) if len(sys.argv) > 2 else 1
c = sqlite3.connect(db, timeout=10)
cols = [r[1] for r in c.execute("pragma table_info(messages)") if r[1] not in ("id", "gmail_msg_id", "thread_id", "subject")]
src = c.execute("select id, account_id from messages order by id limit 1").fetchone()
ids = []
for i in range(n):
    tag = f"ev-{time.time_ns()}-{i}"
    cur = c.execute(
        f"insert into messages (gmail_msg_id, thread_id, subject, {','.join(cols)}) "
        f"select ?, ?, ?, {','.join(cols)} from messages where id=?",
        (tag, "t-" + tag, f"Synthetic event {tag}", src[0]),
    )
    mid = cur.lastrowid
    now = datetime.datetime.now(datetime.timezone.utc).isoformat().replace("+00:00", "Z")
    cur = c.execute(
        "insert into events (account_id, message_id, thread_id, kind, tier, importance, sender, one_line, created_at)"
        " values (?, ?, ?, 'urgent', 'signal', 80, 'alice@example.com', ?, ?)",
        (src[1], mid, "t-" + tag, f"Synthetic event {tag}", now),
    )
    ids.append(cur.lastrowid)
c.commit()
print("\n".join(map(str, ids)))
