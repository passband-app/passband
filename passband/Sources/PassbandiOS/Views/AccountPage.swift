// THE ACCOUNT PAGE — the phone's you-space, behind the person glyph at the top
// left of the sitrep. Everything about the person using the app and the install
// they are using it on lives here: which mailboxes this install knows first,
// then what that mailbox has been told (its rules, its groups), and then the
// settings panes under that.
//
// WHY THE SETTINGS LIVE UNDER THE PERSON AND NOT BESIDE THE MAIL. A phone settles
// settings behind whoever is signed in — it is where every other app on the
// device has trained a thumb to look, and it is honest about the relationship:
// the switches are the account's, not the inbox's. Spending one of a phone's
// three tabs on a screen you visit twice a month would have been the trade going
// the other way.
//
// ACCOUNT SWITCHING LANDED HERE, above the panes — which is why the Account row
// already sat first rather than last in the Mac's order. The Mac switches worlds
// with ⌘1..⌘9 and a menu on the rail; a phone has neither a chord nor a rail, so
// the list of mailboxes IS the control, and it sits at the top of the one screen
// entered by tapping a person.
//
// The panes here hold THE SAME STRUCTS the Mac's Views/SettingsView.swift lays
// out: AppearanceSection, SignatureSection, AccountSection and the rest are
// one declaration each, composed by whichever shell is on screen. That is the
// whole design. A settings screen is the surface most likely to be reimplemented
// "just for the phone", and the moment it is, the two copies start disagreeing
// about what a toggle writes, which cap is which, and what a hint promises. They
// cannot disagree here, because there is only one of each.
//
// WHAT DIFFERS IS THE NAVIGATION, AND ONLY THAT. A window can put six panes in a
// column beside their contents and let you scan the whole map at once; a phone
// has room for the map or the territory, never both. So the six become a list you
// tap into and back out of, which is also the shape every other settings screen
// on the device has — the one place in this app where matching the platform's
// habit beats having a house style.
//
// RULES AND GROUPS ARE HERE TOO, above the settings. On the Mac they lead the
// Settings sub-nav, with the accounts; a phone's bar has three tabs and
// neither of these is somewhere you live. But a rule made from a swipe has to
// be findable afterwards, and the place a thumb goes looking for "what have I
// told this thing" is the screen about you. So they get a pane of their own,
// ABOVE the settings and directly under the switcher: they belong to the
// mailbox the switcher just named (a second account has different rules),
// where everything below them belongs to the install. Their rows wear the same
// shape as the settings rows so the screen still reads as one index. They ARE
// `SettingsSection` cases (the Mac's sub-nav lists them, and the label and
// icon come from there), but not in `order`: here they are pushed as pages.
//
// A LIST AND NOT A `List`. The app paints its own canvas and floats glass on it,
// and `insetGrouped` brings a grouped system background plus separator insets
// that fight both. This is the same ScrollView-of-cards MobileRecordsView is, and
// the index reads as inset-grouped because the rows share one glass pane.

import SwiftUI

struct AccountPage: View {
    /// The panes, in the order a PHONE wants them: the account first, because
    /// this page is entered through a person icon and the row that answers
    /// "who am I signed in as" should not be the sixth one down. The Mac's
    /// sub-nav keeps its own order — `SettingsSection.allCases` — since a rail
    /// beside the pane is a map, and a map you can read at a glance has no
    /// reason to lead with anything.
    ///
    /// Rules and Groups are NOT here: the Mac files them as settings panes, and
    /// the phone gives them rows of their own above this list (`pages`).
    private static let order: [SettingsSection] = [
        .account, .general, .mail, .assistant, .agents, .privacy, .audit,
    ]

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                masthead
                // Which mailbox the panes below are about. Adding is NOT here:
                // it lives in the Account pane, one row down, where removing
                // and renaming already live.
                //
                // A DIFFERENT mailbox closes the page: choosing one is asking
                // to see it, and its board is behind this pane. The one already
                // live stays put, since a tap on it is a look, not a move.
                AccountSwitcher { changed in if changed { AccountDoor.shared.close() } }
                pages
                index
            }
            .padding(.horizontal, 16)
            .padding(.bottom, 28)
        }
        .background(Palette.canvas)
        .navigationTitle("Account")
        .navigationBarTitleDisplayMode(.inline)
        // THE WAY BACK OUT, on the side it came in from. This page is the
        // first screen of a pane that slid in from the left (AccountDoor), so
        // it has no stack to pop: the chevron sends the pane back, and so does
        // dragging it that way. Both are on THIS screen only; a page pushed
        // from here has the stack's own back.
        .toolbar {
            ToolbarItem(placement: .topBarLeading) {
                Button { AccountDoor.shared.close() } label: {
                    Image(systemName: "chevron.left")
                }
                .accessibilityLabel("Close")
            }
        }
        .simultaneousGesture(
            LeadingPaneClose.drag { close in if close { AccountDoor.shared.close() } })
        #if DEBUG
            // A screenshot's way in; see FixtureRoute.
            .navigationDestination(item: stagedPage) { page in
                switch page {
                case .rules: RulesView()
                case .groups: GroupsView()
                case .general: pane(.general)
                case .agents: pane(.agents)
                }
            }
        #endif
    }

    #if DEBUG
        private var stagedPage: Binding<FixtureRoute.Page?> {
            Binding(
                get: { FixtureRoute.shared.page },
                set: { FixtureRoute.shared.page = $0 })
        }
    #endif

    /// The one serif line on the index, same as Quick Look's: says what the
    /// rows have in common so the screen reads as a place, not a menu.
    private var masthead: some View {
        VStack(alignment: .leading, spacing: 3) {
            Text("account")
                .font(Typo.micro)
                .foregroundStyle(Palette.accent)
                .textCase(.uppercase)
                .tracking(0.6)
            Text("You, and how this one behaves.")
                .font(Typo.hero(26))
                .foregroundStyle(Palette.ink)
                .fixedSize(horizontal: false, vertical: true)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.top, 4)
    }

    /// WHAT THIS MAILBOX HAS BEEN TOLD: the two pages the Mac keeps at the top
    /// of its Settings sub-nav. The views are the Mac's own (`RulesView`, `GroupsView`), each
    /// with a phone layout inside it, so a rule edited here is edited by the
    /// same code that edits it there.
    ///
    /// Rules first. It is the page a swipe's Tune has been writing to all
    /// along, and the one a person comes back for.
    private var pages: some View {
        VStack(spacing: 0) {
            NavigationLink { RulesView() } label: {
                row(
                    symbol: SettingsSection.rules.symbol, title: SettingsSection.rules.label,
                    blurb: "what you asked for from each sender")
            }
            .buttonStyle(.plain)
            Hairline()
            NavigationLink { GroupsView() } label: {
                row(
                    symbol: SettingsSection.groups.symbol, title: SettingsSection.groups.label,
                    blurb: "people you write to together")
            }
            .buttonStyle(.plain)
        }
        .padding(.vertical, 4)
        .frame(maxWidth: .infinity, alignment: .leading)
        .passbandGlass(.pane, cornerRadius: 18, tint: Palette.glassTint)
    }

    /// One glass pane, a row per settings pane, hairlines between: the
    /// inset-grouped shape, built out of the app's own material rather than
    /// the system's.
    private var index: some View {
        VStack(spacing: 0) {
            ForEach(Self.order, id: \.self) { section in
                NavigationLink { pane(section) } label: { row(section) }
                    .buttonStyle(.plain)
                if section != Self.order.last { Hairline() }
            }
        }
        .padding(.vertical, 4)
        .frame(maxWidth: .infinity, alignment: .leading)
        .passbandGlass(.pane, cornerRadius: 18, tint: Palette.glassTint)
    }

    private func row(_ section: SettingsSection) -> some View {
        row(symbol: section.symbol, title: section.label, blurb: blurb(section))
    }

    /// The one row shape on this screen, for a settings pane or a page.
    private func row(symbol: String, title: String, blurb: String) -> some View {
        HStack(spacing: 12) {
            Image(systemName: symbol)
                .font(.system(size: 14, weight: .medium))
                .foregroundStyle(Palette.accent)
                .frame(width: 22)
            VStack(alignment: .leading, spacing: 2) {
                Text(title)
                    .font(Typo.rowSub)
                    .foregroundStyle(Palette.ink)
                Text(blurb)
                    .font(Typo.micro)
                    .foregroundStyle(Palette.inkFaintest)
                    .lineLimit(1)
            }
            Spacer(minLength: 8)
            Image(systemName: "chevron.right")
                .font(.system(size: 11, weight: .semibold))
                .foregroundStyle(Palette.inkFaintest)
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 11)
        // The whole row is the tap target, not just the glyphs on it.
        .contentShape(Rectangle())
    }

    /// THE PANES, and the one place the mapping lives. Same grouping as the
    /// Mac's sub-nav, because a user who has seen both should find the
    /// read-tracking switch under Mail either way.
    @ViewBuilder private func pane(_ section: SettingsSection) -> some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                switch section {
                case .general:
                    YouSection()
                    AppearanceSection()
                    NotificationsSection()
                    // HelpSection is here for its What's new row, which the
                    // shell can now answer (WhatsNewWalk, mounted in
                    // MobileShell). Its Practice inbox row stays on the Mac:
                    // the tour is a lesson over the Mac's shell and the phone
                    // has none. The section leaves both out by itself, and
                    // draws nothing when there is no note to show.
                    HelpSection()
                    DeveloperSection()
                case .mail:
                    MailSection()
                    SearchSection()
                    SignatureSection()
                    ReadTrackingSection()
                // Not panes here: the phone pushes these two from `pages`, as
                // pages of their own, so nothing asks this switch for either.
                case .rules, .groups:
                    EmptyView()
                case .assistant:
                    AssistantSection()
                case .agents:
                    AgentsSection()
                case .privacy:
                    PrivacySection()
                case .audit:
                    AuditSection()
                case .account:
                    AccountSection()
                }
            }
            .padding(.horizontal, 16)
            .padding(.bottom, 28)
        }
        .background(Palette.canvas)
        .navigationTitle(section.label)
        .navigationBarTitleDisplayMode(.inline)
    }

    /// The one-line blurbs live HERE rather than on `SettingsSection`, which is
    /// shared with the Mac and does not need them: its pane is already on
    /// screen beside the row. A list row has to say what is behind it before
    /// you commit a tap to finding out. (The icons used to live here too; the
    /// Mac's sub-nav shows them now, so they are `SettingsSection.symbol`.)

    private func blurb(_ section: SettingsSection) -> String {
        switch section {
        case .general: "your name, theme, chime"
        case .mail: "images, search order, signature, read tracking"
        case .rules: "how each sender's mail is handled"
        case .groups: "people you write to together"
        case .assistant: "your own api key, and which model"
        case .agents: "connect Claude, OpenClaw, Hermes Agent and others"
        case .privacy: "what telemetry leaves the app"
        case .audit: "what the agent and this app have done"
        case .account: "the mailboxes this install knows"
        }
    }
}

