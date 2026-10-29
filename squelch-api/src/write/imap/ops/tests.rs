use super::*;

use crate::write::imap::apply_plan;

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| x.to_string()).collect()
}

fn loc(folder: &str, uid: u32) -> MessageLocation {
    MessageLocation {
        folder: folder.into(),
        uidvalidity: 7,
        uid,
        message_id: Some("m@x".into()),
    }
}

fn roles() -> FolderRoles {
    FolderRoles {
        sent: Some("Sent".into()),
        junk: Some("Junk".into()),
        trash: Some("Trash".into()),
        drafts: None,
        archive: Some("Archive".into()),
        all: None,
        selectable: s(&["INBOX", "Sent", "Junk", "Trash", "Archive"]),
        delimiter: None,
    }
}

// ---- the label table -----------------------------------------------------------

#[test]
fn the_label_table_maps_each_row() {
    let p = |a: &[&str], r: &[&str]| plan_labels(&s(a), &s(r)).unwrap();
    assert_eq!(p(&["UNREAD"], &[]).remove_flags, vec![SEEN]);
    assert_eq!(p(&[], &["UNREAD"]).add_flags, vec![SEEN]);
    assert_eq!(p(&["STARRED"], &[]).add_flags, vec![FLAGGED]);
    assert_eq!(p(&[], &["STARRED"]).remove_flags, vec![FLAGGED]);
    assert_eq!(p(&[], &["INBOX"]).mv, Some(Move::Archive));
    assert_eq!(p(&["INBOX"], &[]).mv, Some(Move::ToInbox));
    assert_eq!(p(&["SPAM"], &[]).mv, Some(Move::ToJunk));
    assert_eq!(p(&[], &["SPAM"]).mv, Some(Move::NotSpam));
    assert_eq!(p(&["TRASH"], &[]).mv, Some(Move::Trash));
    // The pairs Gmail's clients send together are one move.
    assert_eq!(p(&["SPAM"], &["INBOX"]).mv, Some(Move::ToJunk));
    assert_eq!(p(&["TRASH"], &["INBOX"]).mv, Some(Move::Trash));
    assert_eq!(p(&["INBOX"], &["SPAM"]).mv, Some(Move::NotSpam));
    // Flags and a move together; duplicates collapse.
    let both = p(&["STARRED", "STARRED"], &["UNREAD", "INBOX"]);
    assert_eq!(both.add_flags, vec![FLAGGED, SEEN]);
    assert!(both.remove_flags.is_empty());
    assert_eq!(both.mv, Some(Move::Archive));
}

#[test]
fn any_other_label_is_a_400_that_names_it() {
    for (add, remove, name) in [
        (s(&["IMPORTANT"]), s(&[]), "IMPORTANT"),
        (s(&[]), s(&["CATEGORY_PROMOTIONS"]), "CATEGORY_PROMOTIONS"),
        (s(&[]), s(&["TRASH"]), "TRASH"),
        (s(&["Label_12"]), s(&[]), "Label_12"),
        (s(&["inbox"]), s(&[]), "inbox"),
    ] {
        match plan_labels(&add, &remove) {
            Err(WriteError::Invalid(m)) => assert!(m.contains(name), "{m}"),
            other => panic!("{name}: {other:?}"),
        }
    }
    // A control character in a label never reaches the message.
    match plan_labels(&s(&["A\r\nB"]), &[]) {
        Err(WriteError::Invalid(m)) => assert!(!m.contains('\r') && !m.contains('\n'), "{m}"),
        other => panic!("{other:?}"),
    }
}

/// VALIDATED WHOLE: a good label next to a bad one is refused as a whole,
/// so nothing (here the UNREAD flag) is applied.
#[test]
fn one_bad_label_refuses_the_whole_request() {
    assert!(matches!(
        plan_labels(&s(&["UNREAD", "IMPORTANT"]), &[]),
        Err(WriteError::Invalid(_))
    ));
    assert!(matches!(
        plan_labels(&s(&["STARRED"]), &s(&["STARRED"])),
        Err(WriteError::Invalid(_))
    ));
    for (a, r) in [
        (s(&["TRASH", "SPAM"]), s(&[])),
        (s(&["TRASH", "INBOX"]), s(&[])),
        (s(&["SPAM", "INBOX"]), s(&[])),
        (s(&[]), s(&["SPAM", "INBOX"])),
    ] {
        assert!(
            matches!(plan_labels(&a, &r), Err(WriteError::Invalid(_))),
            "{a:?} {r:?}"
        );
    }
}

// ---- the move strategy ---------------------------------------------------------

#[test]
fn move_then_uidplus_then_refuse() {
    let c = |mv, uidplus| WriteCaps {
        mv,
        uidplus,
        ..WriteCaps::default()
    };
    assert_eq!(move_strategy(c(true, true)).unwrap(), MoveStrategy::Move);
    assert_eq!(move_strategy(c(true, false)).unwrap(), MoveStrategy::Move);
    assert_eq!(
        move_strategy(c(false, true)).unwrap(),
        MoveStrategy::CopyThenUidExpunge
    );
    assert!(matches!(
        move_strategy(c(false, false)),
        Err(WriteError::Invalid(m)) if m == NO_SAFE_MOVE
    ));
}

// ---- a scripted mailbox --------------------------------------------------------

/// A mailbox that answers from a script and records every command it was
/// sent, in the words the live one sends them.
struct Scripted {
    caps: WriteCaps,
    /// UIDVALIDITY by folder; a folder not here is refused at SELECT.
    validity: std::collections::HashMap<String, u32>,
    /// The roles a re-LIST gives.
    relisted: FolderRoles,
    /// The next UID each COPY/MOVE gets, reported as COPYUID.
    next_uid: u32,
    /// A command (its first words) the server answers NO to.
    fail: Option<&'static str>,
    /// UIDs no longer in their folder (`UID SEARCH UID` finds nothing).
    gone: Vec<u32>,
    log: Vec<String>,
}

impl Scripted {
    fn new(caps: WriteCaps) -> Self {
        Self {
            caps,
            validity: ["INBOX", "Sent", "Junk", "Trash", "Archive", "All Mail"]
                .into_iter()
                .map(|f| (f.to_string(), 7))
                .collect(),
            relisted: roles(),
            next_uid: 100,
            fail: None,
            gone: Vec::new(),
            log: Vec::new(),
        }
    }
    fn say(&mut self, c: String) -> Result<(), WriteError> {
        let failed = self.fail.is_some_and(|f| c.starts_with(f));
        self.log.push(c);
        if failed {
            Err(imap_failed("scripted"))
        } else {
            Ok(())
        }
    }
    /// No command anywhere expunges a folder wholesale or CLOSEs one.
    fn assert_never_destructive(&self) {
        for c in &self.log {
            assert_ne!(c, "EXPUNGE", "{:?}", self.log);
            assert_ne!(c, "CLOSE", "{:?}", self.log);
            assert!(!c.starts_with("DELETE"), "{:?}", self.log);
        }
    }
}

const ALL: WriteCaps = WriteCaps {
    mv: true,
    uidplus: true,
    unselect: true,
    create_special_use: true,
};

const COPY_ONLY: WriteCaps = WriteCaps {
    mv: false,
    uidplus: true,
    unselect: true,
    create_special_use: false,
};

#[async_trait::async_trait]
impl WriteMailbox for Scripted {
    fn caps(&self) -> WriteCaps {
        self.caps
    }
    async fn select(&mut self, folder: &str) -> Result<Option<u32>, WriteError> {
        self.say(format!("SELECT {folder}"))?;
        Ok(self.validity.get(folder).copied())
    }
    async fn leave(&mut self, folder: &str) -> Result<(), WriteError> {
        if self.caps.unselect {
            self.say("UNSELECT".into())
        } else {
            self.say(format!("EXAMINE {folder}"))
        }
    }
    async fn store(&mut self, uid: u32, add: bool, flags: &[&str]) -> Result<(), WriteError> {
        let sign = if add { '+' } else { '-' };
        self.say(format!(
            "UID STORE {uid} {sign}FLAGS.SILENT ({})",
            flags.join(" ")
        ))
    }
    async fn uid_move(&mut self, uid: u32, target: &str) -> Result<NewPlace, WriteError> {
        self.say(format!("UID MOVE {uid} {target}"))?;
        self.next_uid += 1;
        Ok(Some((9, self.next_uid)))
    }
    async fn uid_copy(&mut self, uid: u32, target: &str) -> Result<NewPlace, WriteError> {
        self.say(format!("UID COPY {uid} {target}"))?;
        self.next_uid += 1;
        Ok(Some((9, self.next_uid)))
    }
    async fn uid_expunge(&mut self, uid: u32) -> Result<(), WriteError> {
        self.say(format!("UID EXPUNGE {uid}"))
    }
    async fn uid_present(&mut self, uid: u32) -> Result<bool, WriteError> {
        self.say(format!("UID SEARCH UID {uid}"))?;
        Ok(!self.gone.contains(&uid))
    }
    async fn create_archive(&mut self, name: &str, special_use: bool) -> Result<(), WriteError> {
        self.say(if special_use {
            format!("CREATE {name} (USE (\\Archive))")
        } else {
            format!("CREATE {name}")
        })
    }
    async fn list_roles(&mut self) -> Result<FolderRoles, WriteError> {
        self.say("LIST".into())?;
        Ok(self.relisted.clone())
    }
}

async fn run(
    mb: &mut Scripted,
    roles: &mut FolderRoles,
    locs: &[MessageLocation],
    plan: LabelPlan,
) -> (
    Vec<(MessageLocation, Option<MessageLocation>)>,
    Result<(), WriteError>,
) {
    let out = apply_plan(mb, roles, locs, &plan).await;
    mb.assert_never_destructive();
    out
}

#[tokio::test]
async fn a_server_with_move_moves_the_inbox_copy_and_says_where() {
    let mut mb = Scripted::new(ALL);
    let (moved, r) = run(
        &mut mb,
        &mut roles(),
        &[loc("INBOX", 5), loc("Sent", 3)],
        LabelPlan::moving(Move::Archive),
    )
    .await;
    r.unwrap();
    assert_eq!(
        mb.log,
        s(&["SELECT INBOX", "UID MOVE 5 Archive", "UNSELECT"])
    );
    assert_eq!(moved.len(), 1);
    assert_eq!(moved[0].0, loc("INBOX", 5));
    let to = moved[0].1.clone().unwrap();
    assert_eq!(
        (to.folder.as_str(), to.uidvalidity, to.uid),
        ("Archive", 9, 101)
    );
    assert_eq!(to.message_id.as_deref(), Some("m@x"));
}

/// No MOVE, UIDPLUS: COPY, flag THAT uid, expunge THAT uid. Leaving is
/// EXAMINE on a server without UNSELECT.
#[tokio::test]
async fn without_move_it_is_copy_store_and_uid_expunge_of_one_uid() {
    let mut mb = Scripted::new(WriteCaps {
        unselect: false,
        ..COPY_ONLY
    });
    let (_, r) = run(
        &mut mb,
        &mut roles(),
        &[loc("INBOX", 5)],
        LabelPlan::moving(Move::Trash),
    )
    .await;
    r.unwrap();
    assert_eq!(
        mb.log,
        s(&[
            "SELECT INBOX",
            "UID COPY 5 Trash",
            "UID STORE 5 +FLAGS.SILENT (\\Deleted)",
            "UID EXPUNGE 5",
            "EXAMINE INBOX",
        ])
    );
}

/// A COPY that fails stops there: nothing is flagged \Deleted.
#[tokio::test]
async fn a_failed_copy_flags_nothing_deleted() {
    let mut mb = Scripted::new(COPY_ONLY);
    mb.fail = Some("UID COPY");
    let (moved, r) = run(
        &mut mb,
        &mut roles(),
        &[loc("INBOX", 5)],
        LabelPlan::moving(Move::Archive),
    )
    .await;
    assert!(r.is_err());
    assert!(moved.is_empty());
    assert!(
        !mb.log
            .iter()
            .any(|c| c.contains("Deleted") || c.contains("EXPUNGE"))
    );
    assert_eq!(mb.log.last().unwrap(), "UNSELECT", "left the folder");
}

/// Neither MOVE nor UIDPLUS: refused before any command, flags included.
#[tokio::test]
async fn neither_move_nor_uidplus_is_refused_before_any_command() {
    let mut mb = Scripted::new(WriteCaps::default());
    let plan = LabelPlan {
        add_flags: vec![FLAGGED],
        remove_flags: vec![],
        mv: Some(Move::Archive),
    };
    let (_, r) = run(&mut mb, &mut roles(), &[loc("INBOX", 5)], plan).await;
    assert!(matches!(r, Err(WriteError::Invalid(m)) if m == NO_SAFE_MOVE));
    assert!(mb.log.is_empty(), "{:?}", mb.log);
    // Flags alone need no move, and go through.
    let (_, r) = run(
        &mut mb,
        &mut roles(),
        &[loc("INBOX", 5)],
        LabelPlan {
            add_flags: vec![FLAGGED],
            ..LabelPlan::default()
        },
    )
    .await;
    r.unwrap();
}

#[tokio::test]
async fn no_trash_folder_is_refused_and_never_created() {
    let mut mb = Scripted::new(ALL);
    let mut r0 = roles();
    r0.trash = None;
    let (_, r) = run(
        &mut mb,
        &mut r0,
        &[loc("INBOX", 5)],
        LabelPlan::moving(Move::Trash),
    )
    .await;
    assert!(matches!(r, Err(WriteError::Invalid(m)) if m.contains("no Trash")));
    assert!(mb.log.is_empty(), "{:?}", mb.log);
}

#[tokio::test]
async fn no_junk_folder_refuses_spam() {
    let mut mb = Scripted::new(ALL);
    let mut r0 = roles();
    r0.junk = None;
    let (_, r) = run(
        &mut mb,
        &mut r0,
        &[loc("INBOX", 5)],
        LabelPlan::moving(Move::ToJunk),
    )
    .await;
    assert!(matches!(r, Err(WriteError::Invalid(_))));
    assert!(mb.log.is_empty());
}

/// No \Archive and no \All: CREATE "Archive" with USE where the server takes
/// it, list again, use it. A second archive finds it and creates nothing.
#[tokio::test]
async fn no_archive_creates_one_once_then_uses_it() {
    let mut mb = Scripted::new(ALL);
    let mut r0 = roles();
    r0.archive = None;
    let (_, r) = run(
        &mut mb,
        &mut r0,
        &[loc("INBOX", 5)],
        LabelPlan::moving(Move::Archive),
    )
    .await;
    r.unwrap();
    assert_eq!(
        mb.log,
        s(&[
            "CREATE Archive (USE (\\Archive))",
            "LIST",
            "SELECT INBOX",
            "UID MOVE 5 Archive",
            "UNSELECT",
        ])
    );
    assert_eq!(r0.archive.as_deref(), Some("Archive"), "roles re-resolved");
    mb.log.clear();
    let (_, r) = run(
        &mut mb,
        &mut r0,
        &[loc("INBOX", 6)],
        LabelPlan::moving(Move::Archive),
    )
    .await;
    r.unwrap();
    assert!(
        !mb.log.iter().any(|c| c.starts_with("CREATE")),
        "{:?}",
        mb.log
    );

    // Without CREATE-SPECIAL-USE: a plain CREATE (found by name after).
    let mut mb = Scripted::new(COPY_ONLY);
    let mut r0 = roles();
    r0.archive = None;
    let (_, r) = run(
        &mut mb,
        &mut r0,
        &[loc("INBOX", 5)],
        LabelPlan::moving(Move::Archive),
    )
    .await;
    r.unwrap();
    assert_eq!(mb.log[0], "CREATE Archive");
}

/// A CREATE the server refuses (another client made it first) still lists
/// again and uses the folder that is there; one that is still not listed
/// is the CREATE's error, and nothing moves.
#[tokio::test]
async fn a_refused_create_still_uses_an_archive_that_exists() {
    let mut mb = Scripted::new(ALL);
    mb.fail = Some("CREATE");
    let mut r0 = roles();
    r0.archive = None;
    let (_, r) = run(
        &mut mb,
        &mut r0,
        &[loc("INBOX", 5)],
        LabelPlan::moving(Move::Archive),
    )
    .await;
    r.unwrap();
    assert!(mb.log.contains(&"UID MOVE 5 Archive".to_string()));

    let mut mb = Scripted::new(ALL);
    mb.fail = Some("CREATE");
    mb.relisted.archive = None;
    let mut r0 = roles();
    r0.archive = None;
    let (_, r) = run(
        &mut mb,
        &mut r0,
        &[loc("INBOX", 5)],
        LabelPlan::moving(Move::Archive),
    )
    .await;
    assert!(r.is_err());
    assert!(!mb.log.iter().any(|c| c.contains("MOVE")));
}

/// A Gmail-style server (\All, no \Archive) archives into All Mail.
#[tokio::test]
async fn all_mail_is_the_archive_where_there_is_no_archive() {
    let mut mb = Scripted::new(ALL);
    let mut r0 = roles();
    r0.archive = None;
    r0.all = Some("All Mail".into());
    let (_, r) = run(
        &mut mb,
        &mut r0,
        &[loc("INBOX", 5)],
        LabelPlan::moving(Move::Archive),
    )
    .await;
    r.unwrap();
    assert_eq!(
        mb.log,
        s(&["SELECT INBOX", "UID MOVE 5 All Mail", "UNSELECT"])
    );
}

/// A VIRTUAL \All (Dovecot's, for search) refuses the save: not Gmail's,
/// so Archive is CREATEd and the INBOX copy moves there instead (correctness
/// review F3). The next archive's roles have the folder, which wins over
/// \All: \All is not tried again.
#[tokio::test]
async fn an_all_mail_that_refuses_the_move_falls_back_to_a_created_archive() {
    let mut mb = Scripted::new(ALL);
    mb.fail = Some("UID MOVE 5 All Mail");
    mb.relisted.all = Some("All Mail".into());
    let mut r0 = roles();
    r0.archive = None;
    r0.all = Some("All Mail".into());
    let (moved, r) = run(
        &mut mb,
        &mut r0,
        &[loc("INBOX", 5)],
        LabelPlan::moving(Move::Archive),
    )
    .await;
    r.unwrap();
    assert_eq!(
        mb.log,
        s(&[
            "SELECT INBOX",
            "UID MOVE 5 All Mail",
            "UNSELECT",
            "CREATE Archive (USE (\\Archive))",
            "LIST",
            "SELECT INBOX",
            "UID MOVE 5 Archive",
            "UNSELECT",
        ])
    );
    assert_eq!(moved.len(), 1);
    assert_eq!(moved[0].1.as_ref().unwrap().folder, "Archive");
    assert_eq!(r0.archive.as_deref(), Some("Archive"));
    // The next one goes straight to Archive.
    mb.log.clear();
    mb.fail = None;
    let (_, r) = run(
        &mut mb,
        &mut r0,
        &[loc("INBOX", 6)],
        LabelPlan::moving(Move::Archive),
    )
    .await;
    r.unwrap();
    assert_eq!(
        mb.log,
        s(&["SELECT INBOX", "UID MOVE 6 Archive", "UNSELECT"])
    );

    // COPY + UID EXPUNGE servers the same: a refused COPY flags nothing.
    let mut mb = Scripted::new(COPY_ONLY);
    mb.fail = Some("UID COPY 5 All Mail");
    let mut r0 = roles();
    r0.archive = None;
    r0.all = Some("All Mail".into());
    let (_, r) = run(
        &mut mb,
        &mut r0,
        &[loc("INBOX", 5)],
        LabelPlan::moving(Move::Archive),
    )
    .await;
    r.unwrap();
    let deleted_at = mb
        .log
        .iter()
        .position(|c| c.contains("Deleted"))
        .expect("flagged after the copy that worked");
    let copy_at = mb
        .log
        .iter()
        .position(|c| c == "UID COPY 5 Archive")
        .expect("copied to Archive");
    assert!(copy_at < deleted_at, "{:?}", mb.log);
    assert_eq!(
        mb.log.iter().filter(|c| c.contains("Deleted")).count(),
        1,
        "{:?}",
        mb.log
    );
}

/// A move into \All that fails any other way (the SELECT, a broken
/// stream) is not a refusal: nothing is created, the error stands.
#[tokio::test]
async fn an_all_mail_failure_that_is_not_a_refusal_creates_nothing() {
    let mut mb = Scripted::new(ALL);
    mb.fail = Some("SELECT INBOX");
    let mut r0 = roles();
    r0.archive = None;
    r0.all = Some("All Mail".into());
    let (_, r) = run(
        &mut mb,
        &mut r0,
        &[loc("INBOX", 5)],
        LabelPlan::moving(Move::Archive),
    )
    .await;
    assert!(r.is_err());
    assert!(
        !mb.log.iter().any(|c| c.starts_with("CREATE")),
        "{:?}",
        mb.log
    );
}

/// AN INBOX.-NAMESPACE SERVER (Courier, Cyrus): the archive is created
/// under `INBOX.`, the prefix LIST shows every folder has.
#[tokio::test]
async fn an_inbox_namespace_server_creates_inbox_dot_archive() {
    let ns = FolderRoles {
        sent: Some("INBOX.Sent".into()),
        trash: Some("INBOX.Trash".into()),
        selectable: s(&["INBOX", "INBOX.Sent", "INBOX.Trash"]),
        delimiter: Some(".".into()),
        ..FolderRoles::default()
    };
    let mut mb = Scripted::new(COPY_ONLY);
    mb.validity.insert("INBOX.Archive".into(), 7);
    mb.relisted = FolderRoles {
        archive: Some("INBOX.Archive".into()),
        ..ns.clone()
    };
    let mut r0 = ns.clone();
    let (_, r) = run(
        &mut mb,
        &mut r0,
        &[loc("INBOX", 5)],
        LabelPlan::moving(Move::Archive),
    )
    .await;
    r.unwrap();
    assert_eq!(mb.log[0], "CREATE INBOX.Archive");
    assert!(mb.log.contains(&"UID COPY 5 INBOX.Archive".to_string()));

    // Nothing but INBOX to infer from: the top-level CREATE is refused, and
    // the one under INBOX (by LIST's delimiter) is made.
    let mut mb = Scripted::new(COPY_ONLY);
    mb.fail = Some("CREATE Archive");
    mb.relisted = FolderRoles {
        archive: Some("INBOX.Archive".into()),
        ..ns.clone()
    };
    let mut r0 = FolderRoles {
        selectable: s(&["INBOX"]),
        delimiter: Some(".".into()),
        ..FolderRoles::default()
    };
    let (_, r) = run(
        &mut mb,
        &mut r0,
        &[loc("INBOX", 5)],
        LabelPlan::moving(Move::Archive),
    )
    .await;
    r.unwrap();
    assert_eq!(
        &mb.log[..2],
        &s(&["CREATE Archive", "CREATE INBOX.Archive"])[..]
    );
}

/// TRASHING A COPY THAT IS ALREADY GONE is done, not a 502: on a \All
/// server the All Mail copy goes with the INBOX one, and a server may
/// answer NO to a MOVE of the UID it no longer has. A refusal of a UID that
/// is still there is still the error.
#[tokio::test]
async fn trashing_a_copy_that_is_already_gone_succeeds() {
    let mut mb = Scripted::new(ALL);
    mb.fail = Some("UID MOVE 6 Trash");
    mb.gone = vec![6];
    let mut r0 = roles();
    r0.all = Some("All Mail".into());
    let (moved, r) = run(
        &mut mb,
        &mut r0,
        &[loc("INBOX", 5), loc("All Mail", 6)],
        LabelPlan::moving(Move::Trash),
    )
    .await;
    r.unwrap();
    assert_eq!(moved.len(), 2, "both copies' old places are forgotten");
    assert!(moved[1].1.is_none());
    assert!(mb.log.contains(&"UID SEARCH UID 6".to_string()));

    let mut mb = Scripted::new(ALL);
    mb.fail = Some("UID MOVE 6 Trash");
    let (_, r) = run(
        &mut mb,
        &mut r0,
        &[loc("All Mail", 6)],
        LabelPlan::moving(Move::Trash),
    )
    .await;
    assert!(r.is_err(), "still there: the refusal stands");
    // An archive never reads a vanished copy as done.
    let mut mb = Scripted::new(ALL);
    mb.fail = Some("UID MOVE 5 Archive");
    mb.gone = vec![5];
    let (_, r) = run(
        &mut mb,
        &mut roles(),
        &[loc("INBOX", 5)],
        LabelPlan::moving(Move::Archive),
    )
    .await;
    assert!(r.is_err());
    assert!(!mb.log.iter().any(|c| c.starts_with("UID SEARCH")));
}

/// FLAGS ON EVERY VERIFIED COPY, then the move, which takes only the copy
/// in the folder being changed.
#[tokio::test]
async fn flags_go_on_every_copy_and_the_move_on_the_inbox_one() {
    let mut mb = Scripted::new(ALL);
    let plan = plan_labels(&s(&["STARRED"]), &s(&["UNREAD", "INBOX"])).unwrap();
    let (moved, r) = run(
        &mut mb,
        &mut roles(),
        &[loc("INBOX", 5), loc("Sent", 3)],
        plan,
    )
    .await;
    r.unwrap();
    assert_eq!(
        mb.log,
        s(&[
            "SELECT INBOX",
            "UID STORE 5 +FLAGS.SILENT (\\Flagged \\Seen)",
            "UNSELECT",
            "SELECT Sent",
            "UID STORE 3 +FLAGS.SILENT (\\Flagged \\Seen)",
            "UNSELECT",
            "SELECT INBOX",
            "UID MOVE 5 Archive",
            "UNSELECT",
        ])
    );
    assert_eq!(moved.len(), 1);
}

#[tokio::test]
async fn unread_clears_seen_on_every_copy() {
    let mut mb = Scripted::new(ALL);
    let plan = plan_labels(&s(&["UNREAD"]), &s(&["STARRED"])).unwrap();
    let (_, r) = run(
        &mut mb,
        &mut roles(),
        &[loc("INBOX", 5), loc("Sent", 3)],
        plan,
    )
    .await;
    r.unwrap();
    assert_eq!(
        mb.log,
        s(&[
            "SELECT INBOX",
            "UID STORE 5 -FLAGS.SILENT (\\Seen \\Flagged)",
            "UNSELECT",
            "SELECT Sent",
            "UID STORE 3 -FLAGS.SILENT (\\Seen \\Flagged)",
            "UNSELECT",
        ])
    );
}

/// The folder's UIDVALIDITY changed since the copy was verified: the UID
/// may name another message now, so nothing is written to it.
#[tokio::test]
async fn a_changed_uidvalidity_writes_nothing() {
    let mut mb = Scripted::new(ALL);
    mb.validity.insert("INBOX".into(), 8);
    let (_, r) = run(
        &mut mb,
        &mut roles(),
        &[loc("INBOX", 5)],
        LabelPlan {
            add_flags: vec![SEEN],
            ..LabelPlan::default()
        },
    )
    .await;
    assert!(matches!(r, Err(WriteError::Api { status: 409, .. })));
    assert_eq!(mb.log, s(&["SELECT INBOX", "UNSELECT"]));
    let (_, r) = run(
        &mut mb,
        &mut roles(),
        &[loc("INBOX", 5)],
        LabelPlan::moving(Move::Archive),
    )
    .await;
    assert!(r.is_err());
    assert!(!mb.log.iter().any(|c| c.contains("MOVE")));
}

#[tokio::test]
async fn not_spam_moves_the_junk_copy_to_inbox() {
    let mut mb = Scripted::new(ALL);
    let (_, r) = run(
        &mut mb,
        &mut roles(),
        &[loc("Junk", 4)],
        LabelPlan::moving(Move::NotSpam),
    )
    .await;
    r.unwrap();
    assert_eq!(mb.log, s(&["SELECT Junk", "UID MOVE 4 INBOX", "UNSELECT"]));
}

#[test]
fn which_copy_moves() {
    let r = roles();
    let src = |mv, locs: &[MessageLocation]| {
        plan_move(mv, &r, locs)
            .unwrap()
            .map(|p| (p.sources, p.target))
    };
    // Nothing to do: already where the move puts it.
    assert_eq!(src(Move::Archive, &[loc("Archive", 1)]), None);
    assert_eq!(src(Move::NotSpam, &[loc("INBOX", 1)]), None);
    assert_eq!(src(Move::ToInbox, &[loc("INBOX", 1), loc("Junk", 2)]), None);
    assert_eq!(src(Move::ToJunk, &[loc("Junk", 1)]), None);
    assert_eq!(src(Move::Trash, &[loc("Trash", 1)]), None);
    // +INBOX from Junk or the archive.
    assert_eq!(
        src(Move::ToInbox, &[loc("Archive", 2)]),
        Some((vec![loc("Archive", 2)], Target::Folder("INBOX".into())))
    );
    assert!(matches!(
        plan_move(Move::ToInbox, &r, &[loc("Sent", 2)]),
        Err(WriteError::Invalid(_))
    ));
    // +SPAM from INBOX, else the archive.
    assert_eq!(
        src(Move::ToJunk, &[loc("Archive", 2), loc("INBOX", 3)]),
        Some((vec![loc("INBOX", 3)], Target::Folder("Junk".into())))
    );
    // Trash takes every copy, Sent's too.
    assert_eq!(
        src(
            Move::Trash,
            &[loc("INBOX", 1), loc("Sent", 2), loc("Trash", 3)]
        ),
        Some((
            vec![loc("INBOX", 1), loc("Sent", 2)],
            Target::Folder("Trash".into())
        ))
    );
}
