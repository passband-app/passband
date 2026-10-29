//! THE IMAP WRITER'S FOLDER AND FLAG WORK (issue #575 phase 3 step 2): the
//! label table, the move strategy, and the commands, over a
//! [`WriteMailbox`] so every decision can be driven by a script.
//!
//! THE EXPUNGE RULE (contract §6). Nothing here can remove a message the
//! writer did not just copy:
//! - a move is `UID MOVE` (RFC 6851) when the server has MOVE;
//! - else `UID COPY`, `UID STORE <uid> +FLAGS.SILENT (\Deleted)`, then
//!   `UID EXPUNGE <uid>` (RFC 4315), ONLY with UIDPLUS: `UID EXPUNGE` removes
//!   just the UIDs it names, so a `\Deleted` message the user left in the
//!   folder stays;
//! - else the move is refused before anything is sent.
//!
//! There is no plain `EXPUNGE` and no `CLOSE` anywhere in this file (CLOSE
//! expunges a folder opened read-write): a folder is left with `UNSELECT`,
//! or, on a server without it, by `EXAMINE`-ing it read-only, which deselects
//! without expunging (RFC 3501 §6.3.1). A folder is `SELECT`ed only for the
//! command that writes to it.

use squelch_core::mail::imap::{FolderRoles, MessageLocation};

use super::super::WriteError;

pub(crate) const SEEN: &str = "\\Seen";
pub(crate) const FLAGGED: &str = "\\Flagged";
pub(crate) const DELETED: &str = "\\Deleted";

/// The name a created archive folder gets (contract §3).
pub(crate) const ARCHIVE_NAME: &str = "Archive";

/// A folder move the label table asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Move {
    /// Out of INBOX into the archive folder.
    Archive,
    /// From Junk or the archive folder back into INBOX.
    ToInbox,
    /// Into Junk.
    ToJunk,
    /// Out of Junk into INBOX.
    NotSpam,
    /// Into Trash.
    Trash,
}

/// What one `modify(add, remove)` comes to on IMAP: flags to set and clear
/// on every verified copy, then at most one move.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct LabelPlan {
    pub add_flags: Vec<&'static str>,
    pub remove_flags: Vec<&'static str>,
    pub mv: Option<Move>,
}

/// A label id as it may appear in an error: short, printable.
fn shown(label: &str) -> String {
    let clean: String = label.chars().filter(|c| !c.is_control()).take(64).collect();
    format!("{clean:?}")
}

/// THE LABEL TABLE (contract §4), validated whole before anything is applied:
///
/// | change | IMAP |
/// |---|---|
/// | +UNREAD / -UNREAD | -\Seen / +\Seen |
/// | +STARRED / -STARRED | +\Flagged / -\Flagged |
/// | -INBOX | archive |
/// | +INBOX | Junk or archive folder -> INBOX |
/// | +SPAM | -> Junk |
/// | -SPAM | not spam (Junk -> INBOX) |
/// | +TRASH | trash |
///
/// The pairs Gmail's own clients send together are one move: +SPAM with
/// -INBOX is to Junk, +TRASH with -INBOX is trash, -SPAM with +INBOX is not
/// spam. Any other label, a label both added and removed, or two moves that
/// disagree is [`WriteError::Invalid`] (the door's 400) naming it.
pub(crate) fn plan_labels(add: &[String], remove: &[String]) -> Result<LabelPlan, WriteError> {
    let mut plan = LabelPlan::default();
    let (mut add_inbox, mut rm_inbox, mut add_spam, mut rm_spam, mut add_trash) =
        (false, false, false, false, false);
    for label in add {
        if remove.contains(label) {
            return Err(WriteError::Invalid(format!(
                "label {} is both added and removed",
                shown(label)
            )));
        }
    }
    for (label, adding) in add
        .iter()
        .map(|l| (l, true))
        .chain(remove.iter().map(|l| (l, false)))
    {
        match (label.as_str(), adding) {
            ("UNREAD", true) => push_once(&mut plan.remove_flags, SEEN),
            ("UNREAD", false) => push_once(&mut plan.add_flags, SEEN),
            ("STARRED", true) => push_once(&mut plan.add_flags, FLAGGED),
            ("STARRED", false) => push_once(&mut plan.remove_flags, FLAGGED),
            ("INBOX", true) => add_inbox = true,
            ("INBOX", false) => rm_inbox = true,
            ("SPAM", true) => add_spam = true,
            ("SPAM", false) => rm_spam = true,
            ("TRASH", true) => add_trash = true,
            _ => {
                return Err(WriteError::Invalid(format!(
                    "label {} cannot be {} on an IMAP account",
                    shown(label),
                    if adding { "added" } else { "removed" }
                )));
            }
        }
    }
    let conflict = || {
        Err(WriteError::Invalid(
            "these label changes ask for two different moves at once".into(),
        ))
    };
    plan.mv = if add_trash {
        if add_inbox || add_spam || rm_spam {
            return conflict();
        }
        Some(Move::Trash)
    } else if add_spam {
        if add_inbox {
            return conflict();
        }
        Some(Move::ToJunk)
    } else if rm_spam {
        if rm_inbox {
            return conflict();
        }
        Some(Move::NotSpam)
    } else if rm_inbox {
        Some(Move::Archive)
    } else if add_inbox {
        Some(Move::ToInbox)
    } else {
        None
    };
    Ok(plan)
}

impl LabelPlan {
    /// A plan that is one move and no flags.
    pub(crate) fn moving(mv: Move) -> Self {
        Self {
            mv: Some(mv),
            ..Self::default()
        }
    }
}

fn push_once(v: &mut Vec<&'static str>, flag: &'static str) {
    if !v.contains(&flag) {
        v.push(flag);
    }
}

/// What the writer's session said it can do, read once per login.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct WriteCaps {
    pub mv: bool,
    pub uidplus: bool,
    pub unselect: bool,
    pub create_special_use: bool,
}

/// How a message moves between folders on this server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MoveStrategy {
    /// `UID MOVE`.
    Move,
    /// `UID COPY`, `UID STORE +FLAGS.SILENT (\Deleted)`, `UID EXPUNGE <uid>`.
    CopyThenUidExpunge,
}

/// What a server with neither MOVE nor UIDPLUS is told.
pub(crate) const NO_SAFE_MOVE: &str = "this mail server lacks MOVE and UIDPLUS, so Passband cannot move mail on it without \
     risking other deleted messages";

/// THE STRATEGY (contract §6): MOVE, else COPY + UID EXPUNGE under UIDPLUS,
/// else refused. Never a plain EXPUNGE.
pub(crate) fn move_strategy(caps: WriteCaps) -> Result<MoveStrategy, WriteError> {
    if caps.mv {
        Ok(MoveStrategy::Move)
    } else if caps.uidplus {
        Ok(MoveStrategy::CopyThenUidExpunge)
    } else {
        Err(WriteError::Invalid(NO_SAFE_MOVE.into()))
    }
}

/// A location's UID in the folder a `COPYUID` named: (UIDVALIDITY, UID).
pub(crate) type NewPlace = Option<(u32, u32)>;

/// The commands a write needs of an IMAP session. The live one is the
/// writer's own session; the tests' is a script that records every command.
#[async_trait::async_trait]
pub(crate) trait WriteMailbox: Send {
    fn caps(&self) -> WriteCaps;
    /// `SELECT` (read-write) `folder`; its UIDVALIDITY, or `None` when the
    /// server refuses the folder.
    async fn select(&mut self, folder: &str) -> Result<Option<u32>, WriteError>;
    /// Leave the selected folder WITHOUT expunging: `UNSELECT`, or `EXAMINE`
    /// of the same folder on a server without it. Never `CLOSE`.
    async fn leave(&mut self, folder: &str) -> Result<(), WriteError>;
    /// `UID STORE uid {+|-}FLAGS.SILENT (flags)`.
    async fn store(&mut self, uid: u32, add: bool, flags: &[&str]) -> Result<(), WriteError>;
    /// `UID MOVE uid target`, with the `COPYUID` the server gave.
    async fn uid_move(&mut self, uid: u32, target: &str) -> Result<NewPlace, WriteError>;
    /// `UID COPY uid target`, with the `COPYUID` the server gave.
    async fn uid_copy(&mut self, uid: u32, target: &str) -> Result<NewPlace, WriteError>;
    /// `UID EXPUNGE uid`: that one UID, and only when it is `\Deleted`.
    async fn uid_expunge(&mut self, uid: u32) -> Result<(), WriteError>;
    /// Is `uid` still in the selected folder (`UID SEARCH UID uid`)?
    async fn uid_present(&mut self, uid: u32) -> Result<bool, WriteError>;
    /// `CREATE name`, with `USE (\Archive)` when `special_use`.
    async fn create_archive(&mut self, name: &str, special_use: bool) -> Result<(), WriteError>;
    /// The folder roles, listed afresh.
    async fn list_roles(&mut self) -> Result<FolderRoles, WriteError>;
}

/// The message changed or went between the check and the write.
fn moved_under_us() -> WriteError {
    WriteError::Api {
        status: 409,
        message: "the message moved on the server; try again".into(),
    }
}

/// Is `folder` INBOX (case-insensitively, RFC 3501 §5.1)?
pub(crate) fn is_inbox(folder: &str) -> bool {
    folder.eq_ignore_ascii_case("INBOX")
}

/// FLAGS on EVERY verified copy (contract §5): one `SELECT` per folder, one
/// `UID STORE` per flag direction, then out without expunging.
pub(crate) async fn apply_flags(
    mb: &mut dyn WriteMailbox,
    locs: &[MessageLocation],
    add: &[&str],
    remove: &[&str],
) -> Result<(), WriteError> {
    if add.is_empty() && remove.is_empty() {
        return Ok(());
    }
    for loc in locs {
        if mb.select(&loc.folder).await? != Some(loc.uidvalidity) {
            // A folder the server refused is not open: leaving is best effort.
            let _ = mb.leave(&loc.folder).await;
            return Err(moved_under_us());
        }
        let stored = async {
            if !add.is_empty() {
                mb.store(loc.uid, true, add).await?;
            }
            if !remove.is_empty() {
                mb.store(loc.uid, false, remove).await?;
            }
            Ok::<_, WriteError>(())
        }
        .await;
        let left = mb.leave(&loc.folder).await;
        stored?;
        left?;
    }
    Ok(())
}

/// Why a [`move_one`] did not go through.
#[derive(Debug)]
pub(crate) enum MoveFail {
    /// The server answered the MOVE (or the COPY) itself `NO`/`BAD`: nothing
    /// was done, so another target may be tried.
    Refused(WriteError),
    /// Anything else (the SELECT, the flagging after a COPY, a broken
    /// stream): what happened is not known, so nothing more is tried.
    Other(WriteError),
}

impl From<MoveFail> for WriteError {
    fn from(f: MoveFail) -> Self {
        match f {
            MoveFail::Refused(e) | MoveFail::Other(e) => e,
        }
    }
}

/// The server's `NO`/`BAD` to the move command itself, from what the
/// mailbox calls it ([`imap_failed`]'s 502).
fn refused_or_other(e: WriteError) -> MoveFail {
    match e {
        WriteError::Api { status: 502, .. } => MoveFail::Refused(e),
        e => MoveFail::Other(e),
    }
}

/// MOVE ONE COPY to `target` by `strategy`, and say where it landed when the
/// server told (`COPYUID`). The UIDVALIDITY is checked at `SELECT`: the UID
/// that was verified is the UID that moves.
///
/// `gone_ok` (trash): a refused move of a UID that is no longer in its
/// folder is the move already done, by someone else or by the server (a
/// Gmail-style `\All` copy goes when its INBOX copy is trashed), not a 502.
pub(crate) async fn move_one(
    mb: &mut dyn WriteMailbox,
    loc: &MessageLocation,
    target: &str,
    strategy: MoveStrategy,
    gone_ok: bool,
) -> Result<Option<MessageLocation>, MoveFail> {
    if mb.select(&loc.folder).await.map_err(MoveFail::Other)? != Some(loc.uidvalidity) {
        let _ = mb.leave(&loc.folder).await;
        return Err(MoveFail::Other(moved_under_us()));
    }
    let moved = async {
        match strategy {
            MoveStrategy::Move => mb.uid_move(loc.uid, target).await.map_err(refused_or_other),
            MoveStrategy::CopyThenUidExpunge => {
                let place = mb
                    .uid_copy(loc.uid, target)
                    .await
                    .map_err(refused_or_other)?;
                // The copy is made: from here a failure leaves the message in
                // both folders, never in neither.
                mb.store(loc.uid, true, &[DELETED])
                    .await
                    .map_err(MoveFail::Other)?;
                mb.uid_expunge(loc.uid).await.map_err(MoveFail::Other)?;
                Ok(place)
            }
        }
    }
    .await;
    let moved = match moved {
        Err(MoveFail::Refused(e)) if gone_ok => match mb.uid_present(loc.uid).await {
            Ok(false) => Ok(None),
            _ => Err(MoveFail::Refused(e)),
        },
        other => other,
    };
    // Out of the folder whatever happened. A failure to leave after the move
    // went through does not undo it: the session is dropped afterwards.
    let left = mb.leave(&loc.folder).await;
    let place = moved?;
    if left.is_err() {
        eprintln!("squelch-api: imap: could not leave a folder after a move");
    }
    Ok(place.map(|(uidvalidity, uid)| MessageLocation {
        folder: target.to_string(),
        uidvalidity,
        uid,
        message_id: loc.message_id.clone(),
    }))
}

/// THE ARCHIVE FOLDER (contract §3): `\Archive`, else `\All`, else one
/// created ([`create_archive_folder`]). A `\All` that refuses the move (a
/// virtual all-mail view, which Dovecot offers for search and never takes
/// a save into) is not Gmail's: [`run_move`] then creates the folder and
/// moves there. That is remembered by the server, not by Passband: the next
/// write's LIST has an archive folder, which wins over `\All`.
pub(crate) async fn archive_folder(
    mb: &mut dyn WriteMailbox,
    roles: &mut FolderRoles,
) -> Result<String, WriteError> {
    if let Some(a) = roles.archive.clone().or_else(|| roles.all.clone()) {
        return Ok(a);
    }
    create_archive_folder(mb, roles).await
}

/// A folder named `Archive` CREATEd once (with `USE (\Archive)` where the
/// server takes it), under the personal namespace's prefix where every
/// folder is under INBOX (`INBOX.Archive`; [`FolderRoles::create_prefix`]),
/// and the roles listed again after. A top-level CREATE refused on a server
/// with a delimiter is tried once more under INBOX (an INBOX.-namespace
/// server with nothing but INBOX yet). A CREATE the server refuses (it raced
/// another client, say) still re-lists: an `Archive` that exists is used.
pub(crate) async fn create_archive_folder(
    mb: &mut dyn WriteMailbox,
    roles: &mut FolderRoles,
) -> Result<String, WriteError> {
    let special_use = mb.caps().create_special_use;
    let prefix = roles.create_prefix();
    let first = format!("{}{ARCHIVE_NAME}", prefix.as_deref().unwrap_or(""));
    let mut created = mb.create_archive(&first, special_use).await;
    if created.is_err()
        && prefix.is_none()
        && let Some(d) = roles.delimiter.clone().filter(|d| !d.is_empty())
    {
        created = mb
            .create_archive(&format!("INBOX{d}{ARCHIVE_NAME}"), special_use)
            .await;
    }
    *roles = mb.list_roles().await?;
    match roles.archive.clone() {
        Some(a) => Ok(a),
        None => Err(created.err().unwrap_or_else(|| WriteError::Api {
            status: 502,
            message: "the server did not list the Archive folder it created".into(),
        })),
    }
}

/// What a move resolved to: which copies go where.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct MovePlan {
    pub sources: Vec<MessageLocation>,
    /// `None` until the archive folder is known (it may need creating).
    pub target: Target,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Target {
    Folder(String),
    /// The archive folder, created if the server has none.
    Archive,
}

/// WHICH COPY MOVES (contract §5): the one in the folder being changed.
/// Archive takes the INBOX copy, not spam the Junk copy, +INBOX the Junk or
/// archive copy, +SPAM the INBOX (else archive) copy. Trash takes every
/// verified copy not already there, so a self-sent message leaves Sent too,
/// as Gmail's trash takes every label. `Ok(None)` is nothing to do (the
/// message is already where the move puts it). A role the move needs and the
/// server lacks (Junk, Trash) is refused: never created.
pub(crate) fn plan_move(
    mv: Move,
    roles: &FolderRoles,
    locs: &[MessageLocation],
) -> Result<Option<MovePlan>, WriteError> {
    let in_folder = |f: Option<&str>| -> Vec<MessageLocation> {
        locs.iter()
            .filter(|l| f.is_some_and(|f| l.folder == f))
            .cloned()
            .collect()
    };
    let inbox: Vec<MessageLocation> = locs
        .iter()
        .filter(|l| is_inbox(&l.folder))
        .cloned()
        .collect();
    let archive_name = roles.archive.as_deref().or(roles.all.as_deref());
    let archived = in_folder(archive_name);
    let junk_role = || {
        roles.junk.clone().ok_or_else(|| {
            WriteError::Invalid("this mail server has no Junk folder to move it to".into())
        })
    };
    let plan = |sources: Vec<MessageLocation>, target: Target| {
        Ok((!sources.is_empty()).then_some(MovePlan { sources, target }))
    };
    match mv {
        Move::Archive => plan(inbox, Target::Archive),
        Move::NotSpam => plan(
            in_folder(roles.junk.as_deref()),
            Target::Folder("INBOX".into()),
        ),
        Move::ToInbox => {
            if !inbox.is_empty() {
                return Ok(None);
            }
            let mut from = in_folder(roles.junk.as_deref());
            from.extend(archived);
            if from.is_empty() {
                return Err(WriteError::Invalid(
                    "only mail in Junk or the archive can be moved back to the inbox".into(),
                ));
            }
            from.truncate(1);
            plan(from, Target::Folder("INBOX".into()))
        }
        Move::ToJunk => {
            let junk = junk_role()?;
            if locs.iter().any(|l| l.folder == junk) {
                return Ok(None);
            }
            let from = if inbox.is_empty() { archived } else { inbox };
            if from.is_empty() {
                return Err(WriteError::Invalid(
                    "only mail in the inbox or the archive can be moved to Junk".into(),
                ));
            }
            plan(from, Target::Folder(junk))
        }
        Move::Trash => {
            let trash = roles.trash.clone().ok_or_else(|| {
                WriteError::Invalid(
                    "this mail server has no Trash folder, and Passband never deletes mail \
                     outright"
                        .into(),
                )
            })?;
            let from: Vec<MessageLocation> =
                locs.iter().filter(|l| l.folder != trash).cloned().collect();
            plan(from, Target::Folder(trash))
        }
    }
}

/// CARRY OUT A MOVE: the archive folder found (or made), every source moved.
/// Each `(from, to)` that went through is handed back for the location
/// table EVEN WHEN a later one failed, with that failure.
pub(crate) async fn run_move(
    mb: &mut dyn WriteMailbox,
    roles: &mut FolderRoles,
    plan: MovePlan,
    strategy: MoveStrategy,
) -> (
    Vec<(MessageLocation, Option<MessageLocation>)>,
    Result<(), WriteError>,
) {
    let mut done = Vec::new();
    // `into_all`: the archive is the all-mail view, which may refuse it.
    let (mut target, mut into_all) = match plan.target {
        Target::Folder(f) => (f, false),
        Target::Archive => match archive_folder(mb, roles).await {
            Ok(f) => {
                let into_all = roles.archive.as_deref() != Some(f.as_str());
                (f, into_all)
            }
            Err(e) => return (done, Err(e)),
        },
    };
    let gone_ok = roles.trash.as_deref() == Some(target.as_str());
    for loc in plan.sources {
        if loc.folder == target {
            continue;
        }
        let mut moved = move_one(mb, &loc, &target, strategy, gone_ok).await;
        if into_all && matches!(moved, Err(MoveFail::Refused(_))) {
            // NOT GMAIL'S \All (correctness review F3): it took nothing,
            // so the archive is a folder of its own, made now.
            into_all = false;
            match create_archive_folder(mb, roles).await {
                Ok(f) if roles.all.as_deref() != Some(f.as_str()) => {
                    target = f;
                    moved = move_one(mb, &loc, &target, strategy, gone_ok).await;
                }
                Ok(_) => {}
                Err(e) => return (done, Err(e)),
            }
        }
        match moved {
            Ok(to) => done.push((loc, to)),
            Err(e) => return (done, Err(e.into())),
        }
    }
    (done, Ok(()))
}

// ---- the live mailbox ----------------------------------------------------------

/// The bound on one write command.
const STEP: std::time::Duration = std::time::Duration::from_secs(60);

/// A command the server answered `NO`/`BAD`, or a broken stream: the
/// server's text is not passed on (it may quote the command).
fn imap_failed(what: &str) -> WriteError {
    WriteError::Api {
        status: 502,
        message: format!("the mail server refused {what}"),
    }
}

/// The connection broke while a command's answer was read.
fn imap_broken(what: &str) -> WriteError {
    WriteError::Transport(format!(
        "the connection to the mail server broke during {what}"
    ))
}

fn timed_out(what: &str) -> WriteError {
    WriteError::Transport(format!("imap {what} timed out"))
}

/// A mailbox name as an IMAP quoted string, or `None` for one that cannot be
/// one (a quote, backslash or line break, which the names LIST gave never
/// hold on a sane server; 8-bit names go as they came, the way LIST sent
/// them).
fn quoted(name: &str) -> Option<String> {
    (!name.contains(['"', '\\', '\r', '\n'])).then(|| format!("\"{name}\""))
}

/// The writer's own session as a [`WriteMailbox`].
pub(crate) struct LiveMailbox {
    pub session: squelch_core::mail::imap::ImapSession,
    pub caps: WriteCaps,
}

impl LiveMailbox {
    /// The capabilities that matter to a write, from the logged-in session.
    pub async fn read_caps(
        session: &mut squelch_core::mail::imap::ImapSession,
    ) -> Result<WriteCaps, WriteError> {
        let caps = tokio::time::timeout(STEP, session.capabilities())
            .await
            .map_err(|_| timed_out("CAPABILITY"))?
            .map_err(|_| imap_failed("CAPABILITY"))?;
        Ok(WriteCaps {
            mv: caps.has_str("MOVE"),
            uidplus: caps.has_str("UIDPLUS"),
            unselect: caps.has_str("UNSELECT"),
            create_special_use: caps.has_str("CREATE-SPECIAL-USE"),
        })
    }

    async fn ok(&mut self, what: &str, command: String) -> Result<(), WriteError> {
        tokio::time::timeout(STEP, self.session.run_command_and_check_ok(command))
            .await
            .map_err(|_| timed_out(what))?
            .map_err(|_| imap_failed(what))
    }

    /// Run a COPY or MOVE and read its `COPYUID` (RFC 4315), wherever the
    /// server put it: the tagged OK (COPY) or an untagged OK before the
    /// expunges (MOVE, RFC 6851 §4.3). Only a `COPYUID` naming exactly
    /// `uid` as its source is believed.
    async fn copyuid(
        &mut self,
        what: &str,
        uid: u32,
        command: String,
    ) -> Result<NewPlace, WriteError> {
        use async_imap::imap_proto::{Response, ResponseCode, Status};
        let session = &mut self.session;
        tokio::time::timeout(STEP, async move {
            let tag = session
                .run_command(command)
                .await
                .map_err(|_| imap_failed(what))?;
            let mut place: NewPlace = None;
            loop {
                // A stream that broke mid-answer is not a refusal: what the
                // server did is not known (see [`MoveFail`]).
                let resp = session
                    .read_response()
                    .await
                    .map_err(|_| imap_broken(what))?
                    .ok_or_else(|| imap_broken(what))?;
                let (status, outcome, done) = match resp.parsed() {
                    Response::Done {
                        tag: t,
                        status,
                        outcome,
                    } if *t == tag => (status, outcome, true),
                    Response::Data { status, outcome } => (status, outcome, false),
                    _ => continue,
                };
                if let (Status::Ok, Some(ResponseCode::CopyUid(validity, src, dst))) =
                    (status, &outcome.code)
                    && let ([s], [d]) = (src.as_slice(), dst.as_slice())
                    && single_uid(s) == Some(uid)
                    && let Some(new) = single_uid(d)
                {
                    place = Some((*validity, new));
                }
                if done {
                    return match status {
                        Status::Ok => Ok(place),
                        _ => Err(imap_failed(what)),
                    };
                }
            }
        })
        .await
        .map_err(|_| timed_out(what))?
    }
}

fn single_uid(m: &async_imap::imap_proto::rfc4315::UidSetMember) -> Option<u32> {
    use async_imap::imap_proto::rfc4315::UidSetMember;
    match m {
        UidSetMember::Uid(u) => Some(*u),
        UidSetMember::UidRange(r) if r.start() == r.end() => Some(*r.start()),
        UidSetMember::UidRange(_) => None,
    }
}

#[async_trait::async_trait]
impl WriteMailbox for LiveMailbox {
    fn caps(&self) -> WriteCaps {
        self.caps
    }

    async fn select(&mut self, folder: &str) -> Result<Option<u32>, WriteError> {
        match tokio::time::timeout(STEP, self.session.select(folder))
            .await
            .map_err(|_| timed_out("SELECT"))?
        {
            Ok(mbox) => Ok(mbox.uid_validity),
            Err(async_imap::error::Error::No(_)) => Ok(None),
            Err(_) => Err(imap_failed("SELECT")),
        }
    }

    async fn leave(&mut self, folder: &str) -> Result<(), WriteError> {
        if self.caps.unselect {
            return self.ok("UNSELECT", "UNSELECT".into()).await;
        }
        // RFC 3501 §6.3.1: opening another view deselects WITHOUT the
        // expunge CLOSE would do. Read-only, so nothing more can change.
        tokio::time::timeout(STEP, self.session.examine(folder))
            .await
            .map_err(|_| timed_out("EXAMINE"))?
            .map(|_| ())
            .map_err(|_| imap_failed("EXAMINE"))
    }

    async fn store(&mut self, uid: u32, add: bool, flags: &[&str]) -> Result<(), WriteError> {
        let sign = if add { '+' } else { '-' };
        self.ok(
            "STORE",
            format!("UID STORE {uid} {sign}FLAGS.SILENT ({})", flags.join(" ")),
        )
        .await
    }

    async fn uid_move(&mut self, uid: u32, target: &str) -> Result<NewPlace, WriteError> {
        let t = quoted(target).ok_or_else(|| imap_failed("MOVE"))?;
        self.copyuid("MOVE", uid, format!("UID MOVE {uid} {t}"))
            .await
    }

    async fn uid_copy(&mut self, uid: u32, target: &str) -> Result<NewPlace, WriteError> {
        let t = quoted(target).ok_or_else(|| imap_failed("COPY"))?;
        self.copyuid("COPY", uid, format!("UID COPY {uid} {t}"))
            .await
    }

    async fn uid_expunge(&mut self, uid: u32) -> Result<(), WriteError> {
        if !self.caps.uidplus {
            // Belt and braces: the strategy never gets here without it.
            return Err(WriteError::Invalid(NO_SAFE_MOVE.into()));
        }
        self.ok("UID EXPUNGE", format!("UID EXPUNGE {uid}")).await
    }

    async fn uid_present(&mut self, uid: u32) -> Result<bool, WriteError> {
        let hits = tokio::time::timeout(STEP, self.session.uid_search(format!("UID {uid}")))
            .await
            .map_err(|_| timed_out("SEARCH"))?
            .map_err(|_| imap_failed("SEARCH"))?;
        Ok(hits.contains(&uid))
    }

    async fn create_archive(&mut self, name: &str, special_use: bool) -> Result<(), WriteError> {
        let n = quoted(name).ok_or_else(|| imap_failed("CREATE"))?;
        let command = if special_use {
            format!("CREATE {n} (USE (\\Archive))")
        } else {
            format!("CREATE {n}")
        };
        self.ok("CREATE", command).await
    }

    async fn list_roles(&mut self) -> Result<FolderRoles, WriteError> {
        tokio::time::timeout(
            STEP,
            squelch_core::mail::imap::list_folder_roles(&mut self.session),
        )
        .await
        .map_err(|_| timed_out("LIST"))?
        .map_err(|_| imap_failed("LIST"))
    }
}

#[cfg(test)]
mod tests;
