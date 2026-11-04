// Which release notes a given install is owed. The selection rule is the whole
// feature: everything visible (the card, the Settings button, the generated
// changelog) is a rendering of what these functions return, and the failure
// modes are all silent ones. Showing a returning user four years of backlog,
// showing a brand new user anything, or swallowing a release nobody read are
// each a bug that ships looking exactly like correct behaviour.
//
// The table itself is asserted too, because it is hand-authored prose and the
// order it is written in is load-bearing.

import Foundation

@main
@MainActor
struct ReleaseNotesTests {
    static var failures = 0
    static var checks = 0

    static func main() {
        versionOrdering()
        selection()
        stamping()
        theTable()
        digest()
        thePhonesTable()
        thePhonesArrival()
        theMacsNewcomer()
        quietReleases()

        if failures > 0 {
            print("FAILED: \(failures) of \(checks) checks")
            exit(1)
        }
        print("ok: \(checks) checks passed")
    }

    // MARK: - versions

    static func versionOrdering() {
        expect(v("0.0.4") > v("0.0.3"), "patch ordering")
        expect(v("0.1.0") > v("0.0.99"), "minor outranks patch")
        expect(v("1.0.0") > v("0.9.9"), "major outranks minor")
        // Component-wise, not lexical: the string compare would put 10 first.
        expect(v("0.0.10") > v("0.0.9"), "components compare as numbers")
        // A stamp written by one build has to satisfy a build that spells its
        // own version with a different number of components.
        expect(v("0.0.4") == v("0.0.4.0"), "missing trailing components are zero")
        expect(!(v("0.0.4") < v("0.0.4.0")), "equal versions do not order")

        expect(ReleaseVersion("") == nil, "empty string is not a version")
        expect(ReleaseVersion("0.0.x") == nil, "a non-numeric component is not a version")
        expect(ReleaseVersion("v0.0.4") == nil, "a tag prefix is not a version")
        expect(ReleaseVersion("0.0.") == nil, "a trailing dot is not a version")
    }

    // MARK: - what gets shown

    static let table: [ReleaseNote] = ReleaseNotes.all
    /// The rows the card can show: the table without its quiet releases.
    static let announced: [ReleaseNote] = table.filter { !$0.quiet }

    static func selection() {
        // The ordinary case: one release taken, one release announced.
        let oneUp = ReleaseNotes.unseen(lastSeen: "0.0.3", running: "0.0.4")
        expect(oneUp.map(\.version) == ["0.0.4"], "one version behind shows one release")

        // Two skipped, both owed, newest first.
        let twoUp = ReleaseNotes.unseen(lastSeen: "0.0.2", running: "0.0.4")
        expect(twoUp.map(\.version) == ["0.0.4", "0.0.3"], "skipped versions all show, newest first")

        // Already read: nothing at all. This is the "never see it again" half
        // of the feature and the one that gets exercised on every launch.
        expect(
            ReleaseNotes.unseen(lastSeen: "0.0.4", running: "0.0.4").isEmpty,
            "a current stamp shows nothing")

        // NO STAMP is the install that predates this feature. It is owed the
        // release it just took, and NOT the back catalogue.
        let seeded = ReleaseNotes.unseen(lastSeen: nil, running: "0.0.4")
        expect(seeded.map(\.version) == ["0.0.4"], "no stamp shows only the running release")

        // The table can describe a version this build is not yet: notes get
        // written before the tag lands. Announcing an unshipped feature is
        // worse than announcing it late.
        expect(
            ReleaseNotes.unseen(lastSeen: "0.0.2", running: "0.0.3").map(\.version) == ["0.0.3"],
            "a newer note than the running build is withheld")

        // A stamp AHEAD of the build (downgrade, or a shared prefs file) must
        // not replay everything below it.
        expect(
            ReleaseNotes.unseen(lastSeen: "9.9.9", running: "0.0.4").isEmpty,
            "a stamp ahead of the build shows nothing")

        // Garbage in the stamp reads as no stamp, which lands on the seed rule
        // rather than on the whole history.
        expect(
            ReleaseNotes.unseen(lastSeen: "not-a-version", running: "0.0.4").map(\.version)
                == ["0.0.4"],
            "an unparseable stamp falls back to the seed, not to everything")

        // A build with no version at all is a context that cannot honestly
        // answer, so it shows nothing rather than guessing.
        expect(
            ReleaseNotes.unseen(lastSeen: "0.0.1", running: "").isEmpty,
            "an unparseable running version shows nothing")

        // The oldest release on record has no predecessor to floor against, so
        // an unstamped install running it still sees it.
        expect(
            ReleaseNotes.unseen(lastSeen: nil, running: "0.0.1").map(\.version) == ["0.0.1"],
            "the first release seeds against nothing and still shows")
    }

    // MARK: - what gets written back

    static func stamping() {
        let shown = ReleaseNotes.unseen(lastSeen: "0.0.2", running: "0.0.4")
        expect(
            ReleaseNotes.newestShown(shown) == "0.0.4", "the stamp is the newest note shown")
        expect(ReleaseNotes.newestShown([]) == nil, "nothing shown stamps nothing")

        // The stamp follows what was SHOWN, so a build carrying no note for the
        // release it is running leaves that release unstamped and a later build
        // can still announce it. Both ends are written against the table rather
        // than as literals: spelled as the next version up, this assertion goes
        // red the day that version is written down, which it already did once.
        expect(
            ReleaseNotes.unseen(lastSeen: announced.first?.version, running: "999.0.0").isEmpty,
            "a build past the table shows nothing and so stamps nothing")

        // Settings' button, which ignores the stamp entirely.
        expect(
            ReleaseNotes.newest(atOrBelow: "0.0.4")?.version == "0.0.4",
            "the current release answers for itself")
        expect(
            ReleaseNotes.newest(atOrBelow: "0.0.3")?.version == "0.0.3",
            "an older build answers with its own release")
        // A hotfix or an early bump answers with the last thing that shipped
        // rather than with nothing.
        expect(
            ReleaseNotes.newest(atOrBelow: "99.0.0")?.version == announced.first?.version,
            "a build past the table answers with the newest note")
        expect(
            ReleaseNotes.newest(atOrBelow: "0.0.0") == nil,
            "a build below the whole table answers with nothing")
    }

    // MARK: - the table itself

    static func theTable() {
        expect(!table.isEmpty, "there is a changelog")

        var seen = Set<String>()
        var previous: ReleaseVersion?
        for note in table {
            guard let version = ReleaseVersion(note.version) else {
                fail("version \(note.version) does not parse")
                continue
            }
            expect(seen.insert(note.version).inserted, "\(note.version) appears once")
            if let previous {
                expect(version < previous, "\(note.version) is older than the note above it")
            }
            previous = version

            expect(!note.headline.isEmpty, "\(note.version) has a headline")
            expect(!note.items.isEmpty, "\(note.version) says what it brought")
            // A quiet release is never walked, so a page written for it is a
            // page nobody sees.
            if note.quiet {
                expect(
                    note.items.allSatisfy { $0.symbol == nil },
                    "quiet \(note.version) has no headliners")
            }
            // ISO, because the generated changelog and the card both print it
            // raw rather than reformatting it.
            expect(
                note.date.count == 10 && note.date.filter { $0 == "-" }.count == 2,
                "\(note.version) carries an ISO date")

            for item in note.items {
                expect(!item.text.isEmpty, "\(note.version) has no empty bullets")
                // HOUSE RULE, asserted rather than remembered: no em dashes in
                // user-facing copy. This is the only file where release prose
                // is written, so this is the only place it can be caught.
                expect(
                    !item.text.contains("\u{2014}"),
                    "\(note.version) bullet avoids em dashes: \(item.text.prefix(40))")
                expect(
                    item.detail?.contains("\u{2014}") != true,
                    "\(note.version) page line avoids em dashes: \(item.text.prefix(40))")
                // A page of the walk is a headliner's: a line or a vignette
                // on an item with no symbol would be written and never shown.
                if item.symbol == nil {
                    expect(
                        item.detail == nil && item.vignette == nil,
                        "\(note.version) only a headliner has a page: \(item.text.prefix(40))")
                }
                // A vignette shows a gesture and says nothing; the line under
                // it is what names the key.
                if item.vignette != nil {
                    expect(
                        item.detail?.isEmpty == false,
                        "\(note.version) a vignette comes with its line: \(item.text.prefix(40))")
                }
                // Backticks set a key as a key, in pairs.
                expect(
                    (item.detail ?? "").filter { $0 == "`" }.count % 2 == 0,
                    "\(note.version) page line closes its backticks: \(item.text.prefix(40))")
            }

            // The surface split is what the card draws its headings from, so a
            // note with items must land in at least one of them.
            let grouped = ReleaseSurface.allCases.flatMap { note.items(on: $0) }
            expect(
                grouped.count == note.items.count,
                "\(note.version) items all carry a known surface")

            // Everything the table says, the card says somewhere: a tile, a
            // section bullet, or the footnote. Folding must never drop a line.
            let d = note.digest
            let said = Set(
                (d.headliners.map(\.text) + d.sections.flatMap { $0.items + $0.extras })
                    .map(ReleaseDigest.key))
            for item in note.items {
                expect(
                    said.contains(ReleaseDigest.key(item.text)),
                    "\(note.version) card still says: \(item.text.prefix(40))")
            }
            // A wall of tiles is the wall of bullets again, with icons.
            expect(d.headliners.count <= 4, "\(note.version) has at most four headliners")
        }
    }

    // MARK: - the card's arrangement

    static func digest() {
        let note = ReleaseNote(
            version: "9.9.9", date: "2026-01-01", headline: "test",
            items: [
                ReleaseItem(.app, "Schedule an email for later", symbol: "clock"),
                ReleaseItem(.app, "Wide emails fit the window"),
                ReleaseItem(.app, "UI improvements"),
                ReleaseItem(.app, "Bug fixes"),
                ReleaseItem(.ios, "Schedule an email for later."),
                ReleaseItem(.daemon, "Bug fixes"),
            ])
        let d = note.digest

        expect(d.headliners.count == 1, "one feature on two surfaces is one tile")
        expect(
            d.headliners.first?.detail == nil && d.headliners.first?.vignette == nil,
            "a headliner written with only a symbol has no page line and no vignette")
        expect(d.headliners.first?.surfaces == [.app, .ios], "the tile names both surfaces")
        expect(
            d.sections.map(\.surface) == [.app, .daemon],
            "a surface whose only item became a tile draws no section")
        expect(d.sections.first?.items == ["Wide emails fit the window"], "catch-alls leave the list")
        expect(
            d.sections.first?.extras == ["UI improvements", "Bug fixes"],
            "catch-alls are kept for the footnote, in order")
        expect(d.sections.last?.items.isEmpty == true, "a section can be footnote only")

        expect(
            ReleaseDigest.footnote(["UI improvements", "Bug fixes"]) == "Plus UI improvements and bug fixes.",
            "footnote keeps acronyms and lowers the rest")
        expect(ReleaseDigest.footnote(["Bug Fixes"]) == "Plus bug fixes.", "one extra")
        expect(ReleaseDigest.footnote([]) == nil, "no extras, no footnote")

        let paged = ReleaseNote(
            version: "9.9.7", date: "2026-01-01", headline: "test",
            items: [
                ReleaseItem(.app, "Flow mode", detail: "Press `Space`", symbol: "wind", vignette: .flow)
            ])
        expect(paged.digest.headliners.first?.detail == "Press `Space`", "the page line reaches the card")
        expect(paged.digest.headliners.first?.vignette == .flow, "the vignette reaches the card")

        let plain = ReleaseNote(
            version: "9.9.8", date: "2026-01-01", headline: "test",
            items: [ReleaseItem(.app, "One thing")])
        expect(plain.digest.headliners.isEmpty, "a release with no symbols has no tiles")

        skippedReleases()
    }

    // MARK: - several releases as one card

    static func skippedReleases() {
        let newer = ReleaseNote(
            version: "9.9.9", date: "2026-01-02", headline: "newer",
            items: [
                ReleaseItem(.app, "Flow mode", symbol: "wind"),
                ReleaseItem(.ios, "Attach files to emails", symbol: "paperclip"),
                ReleaseItem(.app, "Name an account"),
                ReleaseItem(.app, "Bug fixes"),
                ReleaseItem(.daemon, "Bug fixes"),
            ])
        let older = ReleaseNote(
            version: "9.9.8", date: "2026-01-01", headline: "older",
            items: [
                ReleaseItem(.app, "Attach files to emails", symbol: "paperclip"),
                ReleaseItem(.app, "Schedule an email for later", symbol: "clock"),
                ReleaseItem(.app, "Wide emails fit the window"),
                ReleaseItem(.app, "Name an account."),
                ReleaseItem(.app, "Bug fixes"),
                ReleaseItem(.daemon, "Triage is faster"),
            ])
        let d = ReleaseDigest([newer, older])

        expect(
            d.headliners.map(\.text) == ["Flow mode", "Attach files to emails", "Schedule an email for later"],
            "every skipped release's headliners are walked, newest release first")
        expect(
            d.headliners.map(\.version) == ["9.9.9", "9.9.9", "9.9.8"],
            "a page knows its release, the newest when two releases said it")
        expect(
            d.headliners[1].surfaces == [.app, .ios],
            "one feature across two releases is one page naming both surfaces")
        expect(d.sections.map(\.surface) == [.app, .daemon], "one section per surface, not per release")
        expect(
            d.sections.first?.items == ["Name an account", "Wide emails fit the window"],
            "the bullets are one list, newest first, the same words said once")
        expect(d.sections.first?.extras == ["Bug fixes"], "three releases of bug fixes are one footnote")
        expect(d.sections.last?.items == ["Triage is faster"], "an older release's surface still lists")

        // Whatever is skipped, nothing the table says goes missing.
        let all = ReleaseDigest(table)
        let said = Set((all.headliners.map(\.text) + all.sections.flatMap { $0.items + $0.extras }).map(ReleaseDigest.key))
        expect(
            table.flatMap(\.items).allSatisfy { said.contains(ReleaseDigest.key($0.text)) },
            "the whole table merged still says every line")

        // The greeting is for a jump of more than one release, and names both ends.
        let two = ReleaseNotes.unseen(lastSeen: "0.0.2", running: "0.0.4")
        let greeting = ReleaseNotes.catchUp(lastSeen: "0.0.2", running: "0.0.4", shown: two)
        expect(greeting == ReleaseCatchUp(from: "0.0.2", to: "0.0.4"), "two releases at once are greeted")
        expect(greeting?.title == "Thank you for updating to 0.0.4!", "the greeting names the new version")
        expect(greeting?.line.hasSuffix("since you were on 0.0.2.") == true, "and the one they came from")
        expect(greeting?.line.contains("\u{2014}") == false, "the greeting avoids em dashes")
        let one = ReleaseNotes.unseen(lastSeen: "0.0.3", running: "0.0.4")
        expect(
            ReleaseNotes.catchUp(lastSeen: "0.0.3", running: "0.0.4", shown: one) == nil,
            "the ordinary update is not greeted")
        expect(
            ReleaseNotes.catchUp(lastSeen: nil, running: "0.0.4", shown: two) == nil,
            "no stamp, no version to say they came from")
    }

    // MARK: - the iPhone's own table

    static let phone: [PhoneReleaseNote] = PhoneReleaseNotes.all

    static func thePhonesTable() {
        expect(!phone.isEmpty, "the phone has release notes")

        var seen = Set<String>()
        var previous: ReleaseVersion?
        for note in phone {
            guard let version = ReleaseVersion(note.version) else {
                fail("phone version \(note.version) does not parse")
                continue
            }
            expect(seen.insert(note.version).inserted, "phone \(note.version) appears once")
            if let previous {
                expect(version < previous, "phone \(note.version) is older than the note above it")
            }
            previous = version

            expect(!note.headline.isEmpty, "phone \(note.version) has a headline")
            expect(
                note.date.count == 10 && note.date.filter { $0 == "-" }.count == 2,
                "phone \(note.version) carries an ISO date")
            // A walk: fewer than three pages is not one, and more than five is
            // the list again with pictures. A quiet release is not walked.
            expect(
                note.quiet ? note.features.isEmpty : (3...5).contains(note.features.count),
                "phone \(note.version) walks three to five features, or none when quiet")
            expect(
                Set(note.features.map(\.title)).count == note.features.count,
                "phone \(note.version) names each feature once")

            for feature in note.features {
                let what = "phone \(note.version) \(feature.title.prefix(30))"
                expect(!feature.title.isEmpty && !feature.detail.isEmpty, "\(what): a page has a title and a line")
                expect(!feature.symbol.isEmpty, "\(what): a page has a symbol to fall back on")
                // A title is a phrase set in the serif; the line under it is a
                // sentence.
                expect(!feature.title.hasSuffix("."), "\(what): the title is not a sentence")
                expect(feature.detail.hasSuffix("."), "\(what): the line is one")
                // HOUSE RULES. No em dashes, and no keys: a backtick is how
                // the Mac's table sets a key, and a phone has none to press.
                for text in [feature.title, feature.detail] {
                    expect(!text.contains("\u{2014}"), "\(what): no em dashes")
                    expect(!text.contains("`"), "\(what): no keys on a phone")
                }
            }
            for aside in note.also {
                expect(!aside.text.isEmpty, "phone \(note.version) has no empty lines")
                expect(!aside.text.contains("\u{2014}"), "phone \(note.version) line avoids em dashes: \(aside.text.prefix(30))")
                expect(!aside.text.contains("`"), "phone \(note.version) line names no key: \(aside.text.prefix(30))")
            }
            // Everything the table says, the closing page says: in its list or
            // in its footnote. Folding must never drop a line.
            let folded = note.also.count - note.asides.count
            expect(
                (folded == 0) == (note.footnote == nil),
                "phone \(note.version) says its catch-alls in the footnote, and only those")
        }

        let folding = PhoneReleaseNote(
            version: "9.9.9", date: "2026-01-01", headline: "test", features: [],
            also: [PhoneAside("Rules have a page", symbol: "list.bullet"), PhoneAside("Bug fixes"), PhoneAside("UI improvements")])
        expect(folding.asides.map(\.text) == ["Rules have a page"], "catch-alls leave the phone's list")
        expect(folding.footnote == "Plus bug fixes and UI improvements.", "and are said once, after it")
    }

    // MARK: - what the phone is owed

    /// The versions an arrival would show, or a word for what it did instead.
    static func shown(_ arrival: PhoneReleaseNotes.Arrival) -> [String] {
        switch arrival {
        case .show(let notes): notes.map(\.version)
        case .stamp(let version): ["stamp " + version]
        case .nothing: []
        }
    }

    /// A Mac that just finished the tour is stamped just below the latest
    /// release, so its first time on the Sitrep shows that release alone.
    static func theMacsNewcomer() {
        guard announced.count > 1 else { return }
        let newest = announced[0].version, below = announced[1].version
        expect(
            ReleaseLedger.newcomerStamp(table, running: newest) == below,
            "a Mac newcomer is stamped just below the latest release")
        expect(
            ReleaseNotes.unseen(lastSeen: below, running: newest).map(\.version) == [newest],
            "and is shown the latest release alone")
        expect(
            ReleaseLedger.newcomerStamp(table, running: "0.0.0") == nil,
            "a build below the whole table has nothing to stamp")
    }

    static func thePhonesArrival() {
        // What the walk can raise: the table without its quiet releases.
        let phone = Self.phone.filter { !$0.quiet }
        guard let first = phone.last?.version, let newest = phone.first?.version else { return }
        let arrival = PhoneReleaseNotes.arrival

        // THE UPDATE THIS FEATURE SHIPS IN. An install that already had a
        // mailbox and has no stamp predates the walk: it is owed the release
        // it just took.
        expect(
            shown(arrival(nil, first, false)) == [first],
            "an established phone with no stamp is shown the release it took")

        // A NEWCOMER IS SHOWN THE LATEST RELEASE and nothing older: stamped
        // with the release just below it, so their first sync raises one.
        let below = phone[1].version
        expect(
            shown(arrival(nil, newest, true)) == ["stamp " + below],
            "a newcomer is stamped just below the latest release")
        expect(
            shown(arrival(below, newest, false)) == [newest],
            "and their first sync shows the latest release alone")
        // A build past the table still shows the newest note there is.
        expect(
            shown(arrival(nil, "999.0.0", true)) == ["stamp " + below],
            "a newcomer on a build past the table meets the newest note")
        // A build older than the first phone note, as a newcomer and not.
        expect(shown(arrival(nil, "0.0.0", true)) == ["stamp 0.0.0"], "a newcomer below the table is still stamped")
        expect(shown(arrival(nil, "0.0.0", false)).isEmpty, "a phone below the whole table is shown nothing")
        expect(
            shown(arrival("0.0.0", first, false)) == [first],
            "and that newcomer is shown the first note when they update to it")

        // Somebody at the gate who HAS a stamp has been here before (signed
        // out and back in): the stamp stands and nothing is rewritten.
        expect(shown(arrival(newest, newest, true)).isEmpty, "a stamped phone at the gate is left alone")
        // Garbage in the stamp is no stamp.
        expect(
            shown(arrival("not-a-version", newest, true)) == ["stamp " + below],
            "an unparseable stamp on a newcomer is replaced")

        // The same bounds as the Mac's table, asked of the phone's.
        expect(
            PhoneReleaseNotes.unseen(lastSeen: "9.9.9", running: newest).isEmpty,
            "a phone stamp ahead of the build shows nothing")
        expect(shown(arrival(nil, "", false)).isEmpty, "a phone build with no version shows nothing")
        expect(shown(arrival(nil, "", true)).isEmpty, "and stamps nothing")
        expect(
            PhoneReleaseNotes.newest(atOrBelow: "0.0.0") == nil,
            "Settings has nothing to show a phone build below the table")
        expect(
            PhoneReleaseNotes.newest(atOrBelow: "99.0.0")?.version == newest,
            "and the newest note for one past it")
        expect(
            PhoneReleaseNotes.newestShown(PhoneReleaseNotes.unseen(lastSeen: nil, running: newest)) == newest,
            "closing the walk stamps the newest note shown")

        // THE TWO TABLES ARE TWO VERSION LINES. The phone's first note shares
        // a number with a Mac release from weeks earlier; nothing here may
        // read one for the other.
        if let twin = ReleaseNotes.all.first(where: { $0.version == first }) {
            expect(
                twin.headline != phone.last?.headline,
                "the phone's \(first) is its own release, not the Mac's of the same number")
        }
    }

    // MARK: - quiet releases

    /// A quiet release is recorded and never announced. Over a made-up table
    /// (9.9.9 quiet, on top of two loud ones) so the rules hold whether or not
    /// the real table has one right now.
    static func quietReleases() {
        let loud = ReleaseNote(
            version: "9.9.8", date: "2026-01-02", headline: "loud",
            items: [ReleaseItem(.app, "Flow mode", symbol: "wind")])
        let older = ReleaseNote(
            version: "9.9.7", date: "2026-01-01", headline: "older",
            items: [ReleaseItem(.app, "Bug fixes")])
        let quiet = ReleaseNote(
            version: "9.9.9", date: "2026-01-03", headline: "quiet",
            items: [ReleaseItem(.app, "Bug fixes")], quiet: true)
        let made = [quiet, loud, older]
        let versions = { (notes: [ReleaseNote]) in notes.map(\.version) }

        expect(
            ReleaseLedger.unseen(made, lastSeen: "9.9.8", running: "9.9.9").isEmpty,
            "taking a quiet release shows nothing")
        expect(
            versions(ReleaseLedger.unseen(made, lastSeen: "9.9.7", running: "9.9.9")) == ["9.9.8"],
            "somebody who skipped a loud release is still shown it, without the quiet one")
        expect(
            ReleaseLedger.unseen(made, lastSeen: nil, running: "9.9.9").isEmpty,
            "an unstamped install on a quiet release is shown nothing")
        expect(
            ReleaseLedger.newest(made, atOrBelow: "9.9.9")?.version == "9.9.8",
            "Settings replays the newest announced release")
        expect(
            ReleaseLedger.newcomerStamp(made, running: "9.9.9") == "9.9.7",
            "a newcomer is stamped below the newest announced release")
        expect(
            versions(ReleaseLedger.unseen(made, lastSeen: "9.9.7", running: "9.9.9")) == ["9.9.8"],
            "so they meet that one")

        // The phone reads the same ledger.
        let phoneQuiet = PhoneReleaseNote(
            version: "9.9.9", date: "2026-01-03", headline: "quiet", features: [],
            also: [PhoneAside("Bug fixes")], quiet: true)
        let phoneLoud = PhoneReleaseNote(
            version: "9.9.8", date: "2026-01-02", headline: "loud", features: [], also: [])
        expect(
            ReleaseLedger.unseen([phoneQuiet, phoneLoud], lastSeen: "9.9.8", running: "9.9.9").isEmpty,
            "a quiet phone release raises no walk")
    }

    // MARK: - harness

    static func v(_ text: String) -> ReleaseVersion {
        guard let version = ReleaseVersion(text) else {
            fatalError("test bug: \(text) is not a version")
        }
        return version
    }

    static func expect(_ condition: Bool, _ what: String) {
        checks += 1
        if !condition {
            failures += 1
            print("  ✗ \(what)")
        }
    }

    static func fail(_ what: String) {
        checks += 1
        failures += 1
        print("  ✗ \(what)")
    }
}
