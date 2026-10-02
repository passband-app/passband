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
            ReleaseNotes.unseen(lastSeen: table.first?.version, running: "999.0.0").isEmpty,
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
            ReleaseNotes.newest(atOrBelow: "99.0.0")?.version == table.first?.version,
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
