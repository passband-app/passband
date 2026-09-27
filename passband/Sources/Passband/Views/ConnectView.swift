// The connect screen. Two ways in, one destination: a bearer token in the OS
// keychain, proved against /client/stats before it is stored.
//
// TWO PLACES SHOW IT, and `purpose` is the whole difference. As the GATE it is
// the window: this install has no identity, so it also teaches what a daemon is
// and, on success, becomes the identity. As the ADD ACCOUNT sheet it sits over
// a working app and adds a SECOND daemon beside the one already live — same
// form, same proof, but it must not touch the connection state the shell behind
// it is standing on (see AppStore.addAccount).
//
// PAIRING is the default. `squelchd pair` prints a code, this screen trades it
// for a token minted for THIS Mac alone — one that shows up by name in
// `squelchd token list` and dies on `squelchd token revoke`. Pasting a raw
// token stays available as the advanced path, because a self-hosted daemon's
// SQUELCH_API_TOKEN is still a first-class way in and always will be.
//
// Neither the code nor the token is ever logged, echoed into an error, or held
// anywhere but this view's own state on its way to the keychain.

import SwiftUI

/// The hosted control plane's one door from the app.
///
/// SIGN IN is the only hosted action, for newcomers too: Google names them, the
/// control plane looks for the daemon that mailbox owns, and the browser comes
/// back with a `passband://pair` link. Somebody with no mailbox yet is told so
/// on that same page and handed the signup form from there, so the app never
/// has to ask "do you have an account?" before it knows the answer.
private enum Hosted {
    // Send the rendered appearance, including a resolved system preference.
    // The account site preserves this choice through OAuth and pairing (#214).
    static func signIn(theme: ColorScheme) -> String {
        "https://signup.passband.app/app/auth?theme=\(theme == .dark ? "dark" : "light")"
    }
}

/// Which way in the screen is showing.
private enum ConnectMode: Hashable { case pair, token }

/// The first decision a fresh install makes. It changes the explanation and
/// defaults around the credential form, never the credential protocol itself:
/// hosted and self-hosted daemons expose the same human door.
private enum HostingChoice: Hashable { case hosted, selfHosted }

/// Progressive disclosure for the first account. Add Account deliberately
/// skips this — someone adding a second daemon already knows what Passband is.
private enum ConnectGateStep: Hashable {
    case introduction
    case welcome
    case selfHostGuide
    /// nil means a complete pair link brought us here. Its URL is authoritative,
    /// but it does not reliably reveal whether the daemon is hosted.
    case credentials(HostingChoice?)
}

/// Why this screen is up. See the file header — it decides what a success
/// MEANS, and nothing else about the form.
enum ConnectPurpose: Hashable {
    /// The app has no identity; connecting becomes it.
    case gate
    /// The app is connected; connecting adds another account beside it.
    case addAccount
}

/// Field focus, so Enter walks the form instead of dead-ending. `submit` is the
/// button itself: a deep link fills the form and lands here, because pairing is
/// never something a link does on its own.
private enum ConnectField: Hashable { case url, code, device, token, name, submit }

struct ConnectView: View {
    @Environment(AppStore.self) private var store
    @Environment(\.dismiss) private var dismiss
    @Environment(\.colorScheme) private var colorScheme

    /// Defaults to the gate, so the two shells that mount it as the whole
    /// window say nothing about it.
    var purpose: ConnectPurpose = .gate

    @State private var mode: ConnectMode = .pair
    @State private var gateStep: ConnectGateStep = .introduction
    @State private var url = "http://127.0.0.1:8848"
    @State private var code = ""
    @State private var deviceName = Pairing.defaultDeviceName()
    @State private var token = ""
    /// The human's name for this account. Optional everywhere: an empty label
    /// leaves the switcher showing the daemon's host:port, which is a real
    /// answer rather than a placeholder.
    @State private var accountLabel = ""
    /// An add-account attempt is in flight. The gate reads `store.connStatus`
    /// for this, but adding deliberately never moves it — the shell behind the
    /// sheet is standing on it.
    @State private var adding = false
    @State private var retryingSavedConnection = false
    /// The failure from an add. Kept out of `store.connError`, which belongs to
    /// the gate and would still be sitting there next time one opened.
    @State private var addError: String?
    /// A claim is in flight. Separate from `connStatus`, which only starts
    /// moving once pairing has produced a token to test.
    @State private var claiming = false
    /// The pairing failure, which is this view's own — the store never sees the
    /// code or its rejection.
    @State private var pairError: String?
    /// A token a claim already minted that `connect` has not accepted yet. Held
    /// because a retry must re-run the PROBE, not the claim: claiming again
    /// spends another of the code's attempts and mints a second token nobody
    /// holds, which only the operator can clear with `squelchd token revoke`.
    /// Dropped once connect succeeds, and whenever the user edits the url or
    /// the code, since a token one daemon minted is nothing to another.
    @State private var heldToken: String?
    /// A deep link filled the form and stopped. Rings the button that is
    /// waiting for the press the link will never make for the user.
    @State private var linkArmed = false
    @State private var arrivedViaPairLink = false
    @State private var showingPairingDetails = false
    /// The gate was mounted because a saved connection could not be restored,
    /// at launch or after the practice inbox. Fixed at mount: the retry it offers is about
    /// the keychain, not about whatever error the form shows later, so it
    /// must outlive the form's own habit of clearing `store.connError`.
    @State private var savedConnectionFailed = false
    /// The analytics id a hosted deep link carried, held until the pairing it
    /// belongs to actually LANDS. It is a claim about who this person is, and
    /// the only proof of that claim is a successful connect to the daemon the
    /// same link named — so it waits here rather than being adopted on arrival.
    /// Cleared alongside `heldToken`/`linkArmed` for the same reason they are:
    /// once a human has hand-edited the url or the code, the form is no longer
    /// that link's claim about anything.
    @State private var linkAid: String?
    @State private var pairingHelp = false
    @FocusState private var focus: ConnectField?

    private var busy: Bool { claiming || adding || retryingSavedConnection || store.connStatus == .connecting }

    /// One error line, whichever half produced it. A pairing failure wins: it is
    /// the more recent thing the user did.
    private var errorText: String? { pairError ?? addError ?? store.connError }

    private var canSubmit: Bool {
        guard !busy, !url.trimmed.isEmpty else { return false }
        switch mode {
        // A held token is a claim already paid for, and the code that bought it
        // is spent and cleared, so the code field is not what gates the retry.
        case .pair:
            return heldToken != nil
                || (Pairing.looksComplete(code) && !deviceName.trimmed.isEmpty)
        case .token: return !token.trimmed.isEmpty
        }
    }

    var body: some View {
        Group {
            switch purpose {
            case .gate: gateBody
            // A sheet is sized by its content, so the card's own measure is the
            // sheet's — no filling the screen, and no getting-started pane: an
            // install adding a SECOND daemon plainly knows what the first one
            // was.
            case .addAccount: connectCard.padding(24)
            }
        }
        // A link can arrive before this view mounts (the app was launched by
        // one, or the sheet is being opened BY one) or while it is up, so both
        // entry points are covered.
        .task {
            // Commit the initial route before the form can edit/clear an
            // error. Later error changes must never send it back to welcome.
            if purpose == .gate {
                if case .introduction = gateStep, isReturning, store.connError != nil {
                    // The saved account's own host goes in the form, so a
                    // hosted person is not handed a localhost self-host form
                    // with a pairing-link instruction for a link that never came.
                    savedConnectionFailed = true
                    if let host = Self.savedHost {
                        url = (Self.isHostedHost(host) ? "https://" : "http://") + host
                    } else {
                        url = ""
                    }
                }
                gateStep = effectiveGateStep
            }
            applyPairLink(store.pairLink)
        }
        .onChange(of: store.pairLink) { _, link in applyPairLink(link) }
        // A link that arrived while the form was busy stayed parked on the
        // store; it is applied the moment the form can take it.
        .onChange(of: busy) { _, nowBusy in
            if !nowBusy { applyPairLink(store.pairLink) }
        }
    }

    /// Somebody the intro is not for: they left the practice inbox this
    /// session, or they have a saved account that failed to restore at launch.
    /// `hasLeftPractice` alone is session-only, so without the second arm every
    /// launch whose restore failed (a revoked token, a keychain panel denied
    /// after an update, a daemon too old for this build) showed a long-time
    /// user the first-run cold open and a welcome screen with no error on it.
    /// A fresh install has no active account and loadSettings clears
    /// `connError` for it, so the intro stays theirs.
    private var isReturning: Bool {
        store.tour.hasLeftPractice
            || (AccountManager.shared.active != nil && store.connError != nil)
    }

    /// A practice exit or a failed restore may need credentials, but never the
    /// intro again.
    private var effectiveGateStep: ConnectGateStep {
        if case .introduction = gateStep, isReturning {
            // A saved connection that failed to restore needs the form and
            // its existing error, not the first-time hosting introduction —
            // and the form for the KIND of daemon that account was.
            guard store.connError != nil else { return .welcome }
            guard let host = Self.savedHost else { return .credentials(nil) }
            return .credentials(Self.isHostedHost(host) ? .hosted : .selfHosted)
        }
        return gateStep
    }

    /// The live account's host as the index remembers it, for a gate that
    /// exists because that account's credentials could not be read.
    private static var savedHost: String? {
        let host = AccountManager.shared.active?.displayHost ?? ""
        return host.isEmpty ? nil : host
    }

    private static func isHostedHost(_ host: String) -> Bool {
        host.hasSuffix("passband.email")
    }

    /// The welcome screen's retry, only when there is a saved connection to
    /// retry. Built here rather than inline: an optional closure conjured by
    /// a ternary inside the view builder is more than the type checker will
    /// resolve in one expression.
    private var savedRetry: (() -> Void)? {
        guard savedConnectionFailed else { return nil }
        return { retrySavedConnection() }
    }

    private var gateBody: some View {
        ZStack {
            switch effectiveGateStep {
            case .introduction:
                OnboardingIntroView { gateStep = .welcome }
            case .welcome:
                WelcomeGate(
                    retrySaved: savedRetry,
                    retrying: retryingSavedConnection,
                    login: {
                        url = ""
                        mode = .pair
                        arrivedViaPairLink = false
                        gateStep = .credentials(.hosted)
                        Opener.open(Hosted.signIn(theme: colorScheme))
                    },
                    selfHostedLogin: {
                        url = "http://127.0.0.1:8848"
                        mode = .pair
                        gateStep = .credentials(.selfHosted)
                    })
            case .selfHostGuide:
                SelfHostGuide(
                    back: { gateStep = .welcome },
                    continueToForm: {
                        mode = .pair
                        gateStep = .credentials(.selfHosted)
                    })
            case .credentials:
                IntroStage { wide in formColumn(wide: wide) }
            }
        }
        // The phone's screen margin. On the Mac the card is a fixed measure
        // floating in a large window and needs none.
        #if !os(macOS)
            .padding(.horizontal, 16)
        #endif
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .overlay(alignment: .bottomTrailing) {
            SetupThemeToggle()
                .padding(20)
        }
    }

    /// The Add Account sheet's form: the same column as the gate's, worn as a
    /// glass card over the working app rather than set on the intro's stage.
    private var connectCard: some View {
        formColumn(wide: false)
            .padding(30)
            // A window can always be 440pt wide; a phone cannot. Same intended
            // measure either way — the Mac states it, the phone treats it as a
            // ceiling and takes whatever the screen leaves (see body's margin).
            #if os(macOS)
                .frame(width: 440)
            #else
                .frame(maxWidth: 440)
            #endif
            .passbandGlass(.pane, cornerRadius: 24, tint: Palette.glassTintStrong)
            .shadow(color: .black.opacity(0.3), radius: 50, y: 24)
    }

    /// Everything the form says and asks, with no chrome of its own: the gate
    /// sets it on the intro's stage, the sheet wraps it in a card.
    private func formColumn(wide: Bool) -> some View {
            VStack(spacing: 0) {
                header(wide: wide)
                    .modifier(IntroEntrance(delay: purpose == .gate ? 0.04 : 0))
                credentialContent

                if let errorText {
                    Label(errorText, systemImage: "exclamationmark.triangle.fill")
                        .font(.system(size: 12))
                        .foregroundStyle(Palette.danger)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(.top, 12)
                }

                if purpose == .gate, savedConnectionFailed {
                    Button(retryingSavedConnection ? "Trying saved connection…" : "Try saved connection again") {
                        retrySavedConnection()
                    }
                    .disabled(busy || store.accountActionsBlocked)
                    .padding(.top, 12)
                }

                bottomActions.padding(.top, 20)
            }
    }

    @ViewBuilder private func header(wide: Bool) -> some View {
        if purpose == .gate {
            // The intro's voice: a hero line and a readable paragraph under it.
            VStack(alignment: .leading, spacing: 16) {
                Text(headerTitle)
                    .font(Typo.hero(wide ? 40 : 32))
                    .foregroundStyle(Palette.ink)
                    .accessibilityAddTraits(.isHeader)
                Text(headerSubtitle)
                    .font(.system(size: 15))
                    .foregroundStyle(Palette.inkDim)
                    .lineSpacing(5)
            }
            .fixedSize(horizontal: false, vertical: true)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.bottom, 28)
        } else {
            VStack(alignment: .leading, spacing: 4) {
                Text(headerTitle)
                    .font(Typo.serif(30, weight: .medium))
                    .foregroundStyle(Palette.ink)
                Text("one daemon per mailbox. this one joins the accounts you already have.")
                    .font(.system(size: 13))
                    .foregroundStyle(Palette.inkFaint)
                    .fixedSize(horizontal: false, vertical: true)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.bottom, 22)
        }
    }

    private var hostingChoice: HostingChoice? {
        guard purpose == .gate, case .credentials(let choice) = gateStep else { return nil }
        return choice
    }

    /// Re-read the index and the keychain the way boot does. The gate is up
    /// because that read failed once (a denied access panel, most often), and
    /// a second answer is a click away without re-pairing anything.
    private func retrySavedConnection() {
        guard !busy, !store.accountActionsBlocked else { return }
        retryingSavedConnection = true
        Task { @MainActor in
            await store.loadSettings()
            retryingSavedConnection = false
        }
    }

    private var headerTitle: String {
        guard purpose == .gate else { return "add account" }
        if arrivedViaPairLink { return "Make yourself at home." }
        switch hostingChoice {
        case .hosted: return "Finish in your browser."
        case .selfHosted: return "Connect your server."
        case nil: return "Pair this device."
        }
    }

    private var headerSubtitle: String {
        if purpose == .gate, arrivedViaPairLink {
            return "Give this device and account names that feel familiar."
        }
        switch hostingChoice {
        case .hosted:
            return "Sign in with Google in the tab that just opened. If Passband doesn’t come back on its own, enter the server URL and pairing code from that page."
        case .selfHosted:
            return "Run squelchd pair on your server, then enter what it prints."
        case nil:
            return "Check the server below, then confirm the pairing link."
        }
    }

    @ViewBuilder private var credentialContent: some View {
        if purpose == .gate {
            gateCredentials
        } else if arrivedViaPairLink && mode == .pair {
            linkedPairForm
        } else {
            if purpose == .addAccount || hostingChoice == .selfHosted {
                GlassSegmented(
                    options: [(ConnectMode.pair, "pair with code"), (.token, "api token")],
                    selection: modeBinding)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.bottom, 16)
            }
            switch mode {
            case .pair: pairForm
            case .token: tokenForm
            }
            nameField.padding(.top, 14)
        }
    }

    // MARK: - the gate's rig

    /// The gate's form, built the way the website's waitlist is: one machined
    /// piece with slots cut in it and the submit set into its last row, rather
    /// than a stack of captioned wells. A slot names itself on its trailing
    /// edge instead of above it, because several arrive already filled (the
    /// device name, a linked server) and a placeholder is gone by then.
    @ViewBuilder private var gateCredentials: some View {
        let linked = arrivedViaPairLink && mode == .pair
        VStack(alignment: .leading, spacing: 0) {
            if hostingChoice == .selfHosted, !linked {
                GlassSegmented(
                    options: [(ConnectMode.pair, "pair with code"), (.token, "api token")],
                    selection: modeBinding)
                    .padding(.bottom, 14)
            }
            ConnectRig(lit: focus != nil) {
                if linked {
                    RigSlot(label: "device", focused: focus == .device) {
                        TextField("This Mac", text: $deviceName)
                            .autocorrectionDisabled()
                            .focused($focus, equals: .device)
                            .onSubmit { focus = .name }
                    }
                } else {
                    RigSlot(label: "server", focused: focus == .url) {
                        TextField(serverPlaceholder, text: urlBinding)
                            .textContentType(.URL)
                            .autocorrectionDisabled()
                            .focused($focus, equals: .url)
                            .onSubmit { focus = mode == .pair ? .code : .token }
                    }
                    RigDivider()
                    switch mode {
                    case .pair:
                        RigSlot(label: "pairing code", focused: focus == .code) {
                            HStack(spacing: 6) {
                                TextField("XXXX-XXXX", text: codeBinding)
                                    // Monospaced so a code read off a terminal
                                    // lines up with it character for character.
                                    .font(Typo.mono(14))
                                    .autocorrectionDisabled()
                                    .focused($focus, equals: .code)
                                    .onSubmit { focus = .device }
                                if hostingChoice == .selfHosted { pairingHelpButton }
                            }
                        }
                        RigDivider()
                        RigSlot(label: "device", focused: focus == .device) {
                            TextField("This Mac", text: $deviceName)
                                .autocorrectionDisabled()
                                .focused($focus, equals: .device)
                                .onSubmit { focus = .name }
                        }
                    case .token:
                        RigSlot(label: "api token", focused: focus == .token) {
                            SecureField("SQUELCH_API_TOKEN", text: $token)
                                .focused($focus, equals: .token)
                                .onSubmit { focus = .name }
                        }
                    }
                }
                RigDivider()
                HStack(spacing: 0) {
                    RigSlot(label: "account", focused: focus == .name) {
                        TextField("Name it (optional)", text: $accountLabel)
                            .autocorrectionDisabled()
                            .focused($focus, equals: .name)
                            .onSubmit { submit() }
                    }
                    rigSubmit
                }
                .fixedSize(horizontal: false, vertical: true)
            }
            if linked {
                pairingDetails.padding(.top, 14)
            }
        }
    }

    /// The submit, set into the rig's last row: no ground or shell of its own,
    /// divided off by one hairline, lit in the accent when it can be pressed.
    private var rigSubmit: some View {
        Button { submit() } label: {
            HStack(spacing: 8) {
                Text(buttonLabel)
                Image(systemName: "arrow.right")
            }
            .font(.system(size: 13, weight: .semibold))
            .foregroundStyle(canSubmit ? Palette.accent : Palette.inkFaint)
            .padding(.horizontal, 18)
            .frame(maxHeight: .infinity)
            .background(Palette.accent.opacity(canSubmit ? 0.12 : 0.04))
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .overlay(alignment: .leading) {
            Rectangle().fill(Palette.accent.opacity(0.18)).frame(width: 1)
        }
        // A pair link filled the form and stopped: ring the one press left.
        .overlay {
            if linkArmed {
                Rectangle()
                    .strokeBorder(Palette.accent, lineWidth: 2)
                    .allowsHitTesting(false)
            }
        }
        .disabled(!canSubmit)
        .focused($focus, equals: .submit)
        .keyboardShortcut(.defaultAction)
    }

    private var pairingHelpButton: some View {
        Button { pairingHelp = true } label: {
            Image(systemName: "questionmark.circle")
                .font(.system(size: 12))
        }
        .buttonStyle(.plain)
        .foregroundStyle(Palette.inkFaint)
        .help("Where the pairing code comes from")
        .popover(isPresented: $pairingHelp) {
            VStack(alignment: .leading, spacing: 10) {
                Text("Run this on the machine hosting squelchd:")
                    .font(.system(size: 12))
                Text("squelchd pair")
                    .font(Typo.mono(13))
                    .padding(.horizontal, 10)
                    .padding(.vertical, 7)
                    .background(RoundedRectangle(cornerRadius: 8).fill(Palette.canvas))
            }
            .padding(16)
        }
    }

    /// The gate's quiet way out, beside whatever second chance this route has.
    @ViewBuilder private var gateSecondaryAction: some View {
        if hostingChoice == .hosted {
            Button("Open sign in again") { Opener.open(Hosted.signIn(theme: colorScheme)) }
                .buttonStyle(.plain)
                .font(.system(size: 12))
                .foregroundStyle(Palette.inkDim)
        } else if hostingChoice == .selfHosted {
            Button("Need help setting up a server?") { gateStep = .selfHostGuide }
                .buttonStyle(.plain)
                .font(.system(size: 12))
                .foregroundStyle(Palette.inkDim)
        }
    }

    @ViewBuilder private var bottomActions: some View {
        if purpose == .gate {
            HStack(spacing: 12) {
                Button("Back") { goBack() }
                    .buttonStyle(.plain)
                    .foregroundStyle(Palette.inkDim)
                    .disabled(busy)
                Spacer()
                gateSecondaryAction
            }
        } else {
            sheetActions
        }
    }

    private var sheetActions: some View {
        HStack(spacing: 12) {
            Button("cancel") { goBack() }
                .disabled(busy)
            Spacer()
            Button(buttonLabel) { submit() }
                .buttonStyle(.borderedProminent)
                .tint(Palette.accent)
                .disabled(!canSubmit)
                .focused($focus, equals: .submit)
                .overlay {
                    if linkArmed {
                        RoundedRectangle(cornerRadius: 7, style: .continuous)
                            .strokeBorder(Palette.accent, lineWidth: 2)
                            .padding(-4)
                            .allowsHitTesting(false)
                    }
                }
        }
    }

    private func goBack() {
        guard purpose == .gate else { dismiss(); return }
        // The whole of a link's claim goes with the screen it filled: a code
        // and a held token belong to the daemon the link named, and the
        // analytics id it carried is a claim about a pairing that did not
        // happen. Leaving any of them behind would let the self-hosted form
        // present a hosted link's code, ringed and ready, against localhost.
        arrivedViaPairLink = false
        showingPairingDetails = false
        code = ""
        heldToken = nil
        linkArmed = false
        linkAid = nil
        pairError = nil
        addError = nil
        store.connError = nil
        gateStep = .welcome
    }

    /// The account's name, optional in both purposes. It matters most in the
    /// sheet — a second row in the switcher wants a word a human picked — but
    /// the first account is allowed one too, since it is about to have company.
    private var nameField: some View {
        Field(label: "account name") {
            TextField("optional, defaults to the server host", text: $accountLabel)
                .textFieldStyle(.plain)
                .autocorrectionDisabled()
                .focused($focus, equals: .name)
                .onSubmit { submit() }
        }
    }

    // MARK: - forms

    private var linkedPairForm: some View {
        VStack(alignment: .leading, spacing: 18) {
            Field(label: "Device name") {
                TextField("This Mac", text: $deviceName)
                    .textFieldStyle(.plain)
                    .font(.system(size: 16, weight: .medium))
                    .autocorrectionDisabled()
                    .focused($focus, equals: .device)
                    .onSubmit { focus = .name }
            }
            Field(label: "Account name") {
                TextField("Personal, work, or something else", text: $accountLabel)
                    .textFieldStyle(.plain)
                    .font(.system(size: 16, weight: .medium))
                    .autocorrectionDisabled()
                    .focused($focus, equals: .name)
                    .onSubmit { submit() }
            }
            Text("Account name is optional. You can rename it later.")
                .font(.system(size: 11))
                .foregroundStyle(Palette.inkFaint)
                .padding(.top, -10)

            pairingDetails
        }
    }

    /// The server a pair link named, with its URL and code folded away for
    /// the one case that needs them: a rejected code, corrected by hand.
    private var pairingDetails: some View {
        VStack(alignment: .leading, spacing: 10) {
            Label(URL(string: url)?.host ?? url, systemImage: "link")
                .font(.system(size: 12))
                .foregroundStyle(Palette.inkDim)
                .lineLimit(2)
                .textSelection(.enabled)
            DisclosureGroup("Pairing details", isExpanded: $showingPairingDetails) {
                VStack(alignment: .leading, spacing: 12) {
                    Field(label: "Server URL") {
                        TextField("Server URL", text: urlBinding)
                            .textFieldStyle(.plain)
                            .autocorrectionDisabled()
                            .focused($focus, equals: .url)
                    }
                    Field(label: "Pairing code") {
                        TextField("XXXX-XXXX", text: codeBinding)
                            .textFieldStyle(.plain)
                            .font(Typo.mono(12))
                            .autocorrectionDisabled()
                            .focused($focus, equals: .code)
                            .onSubmit { submit() }
                    }
                }
                .padding(.top, 10)
                // A rejected code is corrected HERE, so the fields it
                // lives in open the moment there is something to correct.
                .onChange(of: pairError) { _, error in
                    if error != nil { showingPairingDetails = true }
                }
            }
            .font(.system(size: 11))
            .foregroundStyle(Palette.inkFaint)
        }
        .padding(14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(RoundedRectangle(cornerRadius: 12).fill(Palette.canvas.opacity(0.3)))
    }

    private var pairForm: some View {
        VStack(alignment: .leading, spacing: 14) {
            Field(label: "server url") {
                TextField(serverPlaceholder, text: urlBinding)
                    .textFieldStyle(.plain)
                    .textContentType(.URL)
                    .autocorrectionDisabled()
                    .focused($focus, equals: .url)
                    .onSubmit { focus = .code }
            }
            VStack(alignment: .leading, spacing: 5) {
                HStack(spacing: 5) {
                    FieldLabel("pairing code")
                    if hostingChoice == .selfHosted {
                        Button { pairingHelp = true } label: {
                            Image(systemName: "questionmark.circle")
                                .font(.system(size: 11))
                        }
                        .buttonStyle(.plain)
                        .foregroundStyle(Palette.inkFaint)
                        .popover(isPresented: $pairingHelp) {
                            VStack(alignment: .leading, spacing: 10) {
                                Text("Run this on the machine hosting squelchd:")
                                    .font(.system(size: 12))
                                Text("squelchd pair")
                                    .font(Typo.mono(13))
                                    .padding(.horizontal, 10)
                                    .padding(.vertical, 7)
                                    .background(RoundedRectangle(cornerRadius: 8).fill(Palette.canvas))
                            }
                            .padding(16)
                        }
                    }
                }
                TextField("XXXX-XXXX", text: codeBinding)
                    .textFieldStyle(.plain)
                    // Monospaced so a code read off a terminal lines up with
                    // what the terminal showed, character for character.
                    .font(Typo.mono(13))
                    .autocorrectionDisabled()
                    .focused($focus, equals: .code)
                    .onSubmit { submit() }
                    .fieldWell()
            }
            Field(label: "device name") {
                TextField("this Mac", text: $deviceName)
                    .textFieldStyle(.plain)
                    .autocorrectionDisabled()
                    .focused($focus, equals: .device)
                    .onSubmit { submit() }
            }
        }
    }

    private var serverPlaceholder: String {
        hostingChoice == .hosted
            ? "https://username.passband.email"
            : "http://127.0.0.1:8848"
    }

    private var tokenForm: some View {
        VStack(alignment: .leading, spacing: 14) {
            Field(label: "server url") {
                TextField("http://127.0.0.1:8848", text: urlBinding)
                    .textFieldStyle(.plain)
                    .textContentType(.URL)
                    .autocorrectionDisabled()
                    .focused($focus, equals: .url)
                    .onSubmit { focus = .token }
            }
            Field(label: "api token") {
                SecureField("SQUELCH_API_TOKEN", text: $token)
                    .textFieldStyle(.plain)
                    .focused($focus, equals: .token)
                    .onSubmit { submit() }
            }
        }
    }

    /// Sentence case at the gate, where it sits under the intro's type; the
    /// sheet keeps the app's lowercase chrome.
    private var buttonLabel: String {
        let label = rawButtonLabel
        guard purpose == .gate, let first = label.first else { return label }
        return first.uppercased() + label.dropFirst()
    }

    private var rawButtonLabel: String {
        if claiming { return "pairing…" }
        if adding || store.connStatus == .connecting { return "testing…" }
        // The claim is done and only its probe is outstanding, so the button
        // offers the step that is actually left.
        let done = purpose == .gate ? "connect" : "add account"
        if mode == .pair && heldToken != nil { return done }
        return mode == .pair ? "pair" : done
    }

    /// The mode switch, written by hand so flipping it also clears the stale
    /// error from the other path. A raw `$mode` would leave "token rejected"
    /// sitting over the pairing form.
    private var modeBinding: Binding<ConnectMode> {
        Binding(
            get: { mode },
            set: { next in
                guard next != mode else { return }
                mode = next
                pairError = nil
                addError = nil
                store.connError = nil
                focus = next == .pair ? .code : .token
            })
    }

    /// The server-url field, hand-written because a typed edit has to drop a
    /// token held from an earlier claim: it belongs to the daemon that minted
    /// it, not to whatever host the field now names.
    private var urlBinding: Binding<String> {
        Binding(
            get: { url },
            set: { next in
                guard next != url else { return }
                url = next
                heldToken = nil
                linkArmed = false
                linkAid = nil
            })
    }

    /// The pairing-code field. A different code means a different claim, so the
    /// token the last one bought is no longer the thing a press should retry.
    private var codeBinding: Binding<String> {
        Binding(
            get: { code },
            set: { next in
                guard next != code else { return }
                code = next
                heldToken = nil
                linkArmed = false
                linkAid = nil
            })
    }

    // MARK: - actions

    private func submit() {
        guard canSubmit else { return }
        switch mode {
        case .pair: Task { await claim() }
        case .token: Task { await finish(serverURL: url.trimmed, apiToken: token.trimmed) }
        }
    }

    /// THE destination both forms share, and the one place `purpose` changes
    /// what happens: the gate's credentials become this install's identity, the
    /// sheet's become one more account beside it. Returns whether the
    /// credentials were accepted, which is what tells `claim` its held token is
    /// spent.
    @discardableResult
    private func finish(serverURL: String, apiToken: String) async -> Bool {
        addError = nil
        switch purpose {
        case .gate:
            return await store.connect(
                serverURL: serverURL, apiToken: apiToken, label: accountLabel.trimmed)
        case .addAccount:
            adding = true
            let outcome = await store.addAccount(
                serverURL: serverURL, apiToken: apiToken, label: accountLabel.trimmed)
            adding = false
            addError = outcome.error
            // The sheet's whole job is done, and the app behind it has already
            // switched to the account that was just added.
            if outcome.ok { dismiss() }
            return outcome.ok
        }
    }

    /// Claim the code, then hand the issued token to the SAME path a pasted one
    /// takes: `connect` proves it against /client/stats and stores it. Pairing
    /// adds a step in front of that flow, it does not replace it.
    ///
    /// A press AFTER the probe failed does not claim again. The token the first
    /// claim minted is held and re-probed, because a second claim would spend
    /// another of the code's five attempts and leave the first token orphaned
    /// server-side: minted, held by nobody, and only removable by hand.
    private func claim() async {
        let base = url.trimmed
        linkArmed = false
        pairError = nil
        addError = nil
        store.connError = nil

        // Already paid for. Retry the connection, not the claim.
        if let held = heldToken {
            if await finish(serverURL: base, apiToken: held) {
                heldToken = nil
                adoptLinkAid()
            }
            return
        }

        // A duplicate daemon must be refused BEFORE the claim, not after:
        // claiming mints a device token on the daemon and spends one of the
        // code's five attempts, and `addAccount`'s own check would only see
        // the duplicate once both are already gone.
        if purpose == .addAccount, store.isKnownDaemon(base) {
            addError = "that daemon is already one of your accounts"
            return
        }

        let typedCode = code
        let name = Pairing.clampDeviceName(deviceName)
        claiming = true
        do {
            let issued = try await Pairing.claim(baseURL: base, code: typedCode, deviceName: name)
            claiming = false
            // Spent the moment the daemon answers: a code is one-shot, and
            // leaving it on screen invites a second press that can only fail.
            // Assigned to the state directly rather than through `codeBinding`,
            // which is for USER edits and would drop the token we just held.
            code = ""
            heldToken = issued.token
            if await finish(serverURL: base, apiToken: issued.token) {
                heldToken = nil
                adoptLinkAid()
            }
        } catch {
            claiming = false
            pairError = Pairing.message(for: error)
        }
    }

    /// Adopt the deep link's analytics id, now that the pairing it came with has
    /// actually landed. Called from BOTH success paths in `claim`, because a
    /// connect that only succeeded on the second press is no less a pairing —
    /// the retry re-probes a token the same link's code already bought.
    ///
    /// Deliberately not reached from anywhere else. A token-mode connect and a
    /// hand-typed code carry no id, so there is nothing to adopt and nothing
    /// invented: that install stays anonymous until the next `/app/auth` sign-in
    /// link brings the id over, which heals it without a guess. Runs for the
    /// add-account sheet as well as the gate — `Analytics.adopt` decides what a
    /// second account means, and that decision belongs there rather than split
    /// across a view that only knows about one form.
    private func adoptLinkAid() {
        guard let aid = linkAid else { return }
        Analytics.adopt(analyticsId: aid)
        linkAid = nil
    }

    /// Fill the form from a deep link. It NEVER claims, whatever host it names:
    /// a `passband://` URL is openable by any web page, every claim spends one
    /// of the live code's five attempts, and a 200 from whatever answers the
    /// named port is a token this app would then store. So a link gets the user
    /// a filled form with the button ringed, and no further.
    private func applyPairLink(_ link: PairLink?) {
        guard let link else { return }
        // Busy means a claim, an add or a saved-connection retry is mid-
        // flight, and a form edited under it would be claimed against the
        // wrong host. The link stays PARKED on the store rather than consumed:
        // `onChange(of: busy)` applies it the moment the form is free.
        guard !busy else { return }
        store.pairLink = nil
        if purpose == .gate { gateStep = .credentials(nil) }
        mode = .pair
        url = link.serverURL
        code = Pairing.formatted(link.code)
        // Copied, not merged: a new link replaces the previous link's claim
        // whole, and a self-hosted one (which carries no id) correctly clears
        // whatever a hosted link left sitting here.
        linkAid = link.analyticsId
        heldToken = nil
        pairError = nil
        addError = nil
        store.connError = nil
        linkArmed = true
        arrivedViaPairLink = true
        showingPairingDetails = false
        // At the gate the form is the whole screen and Return is the natural
        // next act. The Add Account sheet is raised OVER whatever the human
        // was doing, and focusing submit there would let a Return already in
        // flight claim against the link's chosen host — pairing from a link
        // while connected takes an explicit click.
        focus = purpose == .gate ? .submit : nil
    }
}

/// The sign-in gate, set on the intro's own stage: the same scene behind it,
/// the same masthead and type, so finishing the intro changes the words and
/// nothing else. One hosted door, sign in with Gmail, with self-hosting as the
/// quieter way in beneath it.
private struct WelcomeGate: View {
    /// Present only when the gate is up because a saved connection could
    /// not be restored: the retry belongs on this screen too, since back
    /// from the form lands here.
    var retrySaved: (() -> Void)? = nil
    var retrying = false
    let login: () -> Void
    let selfHostedLogin: () -> Void

    var body: some View {
        IntroStage { wide in content(wide: wide) }
    }

    private func content(wide: Bool) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            VStack(alignment: .leading, spacing: 18) {
                Text("One last step.")
                    .font(Typo.serif(wide ? 33 : 27))
                    .foregroundStyle(Palette.inkDim)
                    .modifier(IntroEntrance(delay: 0.04))
                Text("Bring your inbox.")
                    .font(Typo.hero(wide ? 49 : 36))
                    .foregroundStyle(Palette.ink)
                    .modifier(IntroEntrance(delay: 0.14))
            }
            .fixedSize(horizontal: false, vertical: true)
            .accessibilityElement(children: .combine)
            .accessibilityAddTraits(.isHeader)

            Text("Sign in with the Google account whose mail you want Passband to sort. New here? The same button gets you set up.")
                .font(.system(size: 15))
                .foregroundStyle(Palette.inkDim)
                .lineSpacing(5)
                .fixedSize(horizontal: false, vertical: true)
                .padding(.top, 22)
                .modifier(IntroEntrance(delay: 0.24))

            VStack(alignment: .leading, spacing: 12) {
                Button(action: login) {
                    HStack(spacing: 9) {
                        Image(systemName: "envelope.fill")
                        Text("Sign in with Gmail")
                        Spacer(minLength: 0)
                        Image(systemName: "arrow.right")
                    }
                    .font(.system(size: 14, weight: .semibold))
                    .padding(.vertical, 9)
                    .padding(.horizontal, 8)
                    .frame(maxWidth: .infinity)
                }
                .buttonStyle(.borderedProminent)
                .tint(Palette.accent)
                .keyboardShortcut(.defaultAction)

                Button(action: selfHostedLogin) {
                    HStack(spacing: 9) {
                        Image(systemName: "server.rack")
                        Text("Use a self-hosted server")
                        Spacer(minLength: 0)
                    }
                    .font(.system(size: 13, weight: .medium))
                    .foregroundStyle(Palette.inkDim)
                    .padding(.vertical, 10)
                    .padding(.horizontal, 16)
                    .frame(maxWidth: .infinity)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .background(RoundedRectangle(cornerRadius: 10).fill(Palette.canvas.opacity(0.3)))
                .overlay(RoundedRectangle(cornerRadius: 10).strokeBorder(Palette.hairlineStrong, lineWidth: 0.75))

                if let retrySaved {
                    Button(retrying ? "Trying saved connection…" : "Try saved connection again", action: retrySaved)
                        .buttonStyle(.plain)
                        .font(.system(size: 12))
                        .foregroundStyle(Palette.inkDim)
                        .disabled(retrying)
                        .padding(.top, 6)
                }
            }
            .frame(maxWidth: 340, alignment: .leading)
            .padding(.top, 32)
            .modifier(IntroEntrance(delay: 0.32))
        }
    }
}

/// Self-hosting stays inside Passband: the shortest complete path from no
/// daemon to a pairing code, with copyable commands and an explicit handoff to
/// the form once the server is running.
private struct SelfHostGuide: View {
    let back: () -> Void
    let continueToForm: () -> Void

    var body: some View {
        IntroStage(columnWidth: 520) { wide in
            VStack(alignment: .leading, spacing: 0) {
                VStack(alignment: .leading, spacing: 16) {
                    Text("Set up your server.")
                        .font(Typo.hero(wide ? 44 : 34))
                        .foregroundStyle(Palette.ink)
                        .accessibilityAddTraits(.isHeader)
                    Text("Run squelchd on this Mac, a NAS, or a server you control. Three commands and you’re in.")
                        .font(.system(size: 15))
                        .foregroundStyle(Palette.inkDim)
                        .lineSpacing(5)
                }
                .fixedSize(horizontal: false, vertical: true)
                .modifier(IntroEntrance(delay: 0.04))

                VStack(spacing: 16) {
                    SetupStep(
                        number: 1,
                        title: "Install squelchd",
                        detail: "Pull the public image for amd64 or arm64.",
                        command: "docker pull ghcr.io/braelyn-ai/squelchd")
                    SetupStep(
                        number: 2,
                        title: "Authorize Gmail",
                        detail: "Complete Google's one-time consent on the server.",
                        command: "squelchd auth")
                    SetupStep(
                        number: 3,
                        title: "Pair this device",
                        detail: "Mint the short code the next screen accepts.",
                        command: "squelchd pair")
                }
                .padding(.top, 28)
                .modifier(IntroEntrance(delay: 0.16))

                Button("View detailed instructions on GitHub") {
                    Opener.open(
                        "https://github.com/braelyn-ai/squelch/blob/main/docs/GETTING-STARTED.md")
                }
                .buttonStyle(.textAction)
                .padding(.top, 18)

                HStack(spacing: 12) {
                    Button("Back", action: back)
                        .buttonStyle(.plain)
                        .foregroundStyle(Palette.inkDim)
                    Spacer()
                    Button(action: continueToForm) {
                        HStack(spacing: 9) {
                            Text("My server is running")
                            Image(systemName: "arrow.right")
                        }
                        .font(.system(size: 13, weight: .semibold))
                        .padding(.vertical, 7)
                        .padding(.horizontal, 8)
                    }
                    .buttonStyle(.borderedProminent)
                    .tint(Palette.accent)
                    .keyboardShortcut(.defaultAction)
                }
                .padding(.top, 28)
            }
        }
    }
}

private struct SetupStep: View {
    let number: Int
    let title: String
    let detail: String
    let command: String
    @State private var copied = false

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            Text("\(number)")
                .font(.system(size: 12, weight: .semibold))
                .foregroundStyle(Palette.accentInk)
                .frame(width: 23, height: 23)
                .background(Circle().fill(Palette.accent))
            VStack(alignment: .leading, spacing: 4) {
                Text(title)
                    .font(.system(size: 14, weight: .semibold))
                    .foregroundStyle(Palette.ink)
                Text(detail)
                    .font(.system(size: 12))
                    .foregroundStyle(Palette.inkDim)
                HStack(spacing: 8) {
                    Text(command)
                        .font(Typo.mono(11))
                        .lineLimit(1)
                        .truncationMode(.middle)
                    Spacer(minLength: 4)
                    Button {
                        Clip.copy(command, flashing: $copied)
                    } label: {
                        Image(systemName: copied ? "checkmark" : "doc.on.doc")
                    }
                    .buttonStyle(.plain)
                }
                .padding(.horizontal, 9)
                .padding(.vertical, 6)
                .background(RoundedRectangle(cornerRadius: 8).fill(Palette.canvas.opacity(0.9)))
                .overlay(RoundedRectangle(cornerRadius: 8).strokeBorder(Palette.hairline, lineWidth: 0.75))
                .padding(.top, 2)
            }
        }
    }
}

/// Always reachable during first-run setup, before Settings exists as a
/// destination. It uses the same persisted preference and flip semantics as
/// the app-wide keyboard shortcut.
private struct SetupThemeToggle: View {
    @Environment(Prefs.self) private var prefs

    private var isDark: Bool {
        switch prefs.theme {
        case .dark: true
        case .light: false
        case .system: Platform.isDarkAppearance
        }
    }

    var body: some View {
        Button { prefs.flipTheme() } label: {
            Image(systemName: isDark ? "sun.max.fill" : "moon.fill")
                .font(.system(size: 13, weight: .semibold))
                .foregroundStyle(Palette.ink)
                .frame(width: 30, height: 30)
        }
        .buttonStyle(.plain)
        .glassCapsule(tint: Palette.glassTint.opacity(0.35))
        .help(isDark ? "use light mode" : "use dark mode")
        .accessibilityLabel(isDark ? "Use light mode" : "Use dark mode")
    }
}

/// The gate's machined form: one dark piece edged in the accent, with slots
/// cut in it. Lights as ONE instrument while any slot holds focus, because
/// typing into it and arming its submit are the same act.
private struct ConnectRig<Content: View>: View {
    let lit: Bool
    @ViewBuilder var content: Content
    @State private var hovering = false

    var body: some View {
        let active = lit || hovering
        VStack(spacing: 0) { content }
            .background(
                LinearGradient(
                    colors: [Palette.canvas.opacity(0.94), Palette.canvas.opacity(0.98)],
                    startPoint: .top, endPoint: .bottom))
            .clipShape(RoundedRectangle(cornerRadius: 14, style: .continuous))
            .overlay(
                RoundedRectangle(cornerRadius: 14, style: .continuous)
                    .strokeBorder(Palette.accent.opacity(active ? 0.62 : 0.22), lineWidth: 1))
            .shadow(color: Palette.accent.opacity(active ? 0.3 : 0), radius: 18)
            .shadow(color: .black.opacity(0.35), radius: 16, y: 10)
            .animation(.easeOut(duration: 0.3), value: active)
            .onHover { hovering = $0 }
    }
}

/// The hairline between two of the rig's slots.
private struct RigDivider: View {
    var body: some View {
        Rectangle().fill(Palette.accent.opacity(0.3)).frame(height: 1)
    }
}

/// One slot in the rig: the field, and its name on the trailing edge. The
/// live slot gets a lit edge down its leading side, so tabbing between them
/// shows which one is taking the typing.
private struct RigSlot<Field: View>: View {
    let label: String
    let focused: Bool
    @ViewBuilder var field: Field

    var body: some View {
        HStack(spacing: 10) {
            field
                .textFieldStyle(.plain)
                .font(.system(size: 14))
                .foregroundStyle(Palette.ink)
            Text(label)
                .font(Typo.micro)
                .foregroundStyle(Palette.inkFaint)
                .textCase(.lowercase)
                .accessibilityHidden(true)
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 13)
        .overlay(alignment: .leading) {
            Rectangle()
                .fill(Palette.accent.opacity(focused ? 0.7 : 0))
                .frame(width: 2)
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel(label)
    }
}

/// A labelled input well. Near-opaque on purpose: a translucent field over a
/// busy wallpaper, or over the first-run squelch scene, leaves typed text
/// unreadable.
struct Field<Content: View>: View {
    let label: String
    @ViewBuilder var content: Content

    var body: some View {
        VStack(alignment: .leading, spacing: 5) {
            FieldLabel(label)
            content.fieldWell()
        }
    }
}

/// The caption above a well. Split out so a field needing its own row under the
/// label still labels itself identically.
struct FieldLabel: View {
    let text: String
    init(_ text: String) { self.text = text }

    var body: some View {
        Text(text)
            .font(Typo.micro)
            .foregroundStyle(Palette.inkFaint)
            .textCase(.lowercase)
    }
}

extension View {
    func fieldWell() -> some View {
        self
            .font(.system(size: 13))
            .foregroundStyle(Palette.ink)
            .padding(.horizontal, 10)
            .padding(.vertical, 8)
            .background(
                RoundedRectangle(cornerRadius: 9, style: .continuous)
                    .fill(Palette.canvas.opacity(0.9))
            )
            .overlay(
                RoundedRectangle(cornerRadius: 9, style: .continuous)
                    .strokeBorder(Palette.hairlineStrong, lineWidth: 0.75)
            )
    }
}

extension String {
    var trimmed: String { trimmingCharacters(in: .whitespacesAndNewlines) }
}
