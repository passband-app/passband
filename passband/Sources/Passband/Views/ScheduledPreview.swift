// A SCHEDULED MAIL, READ-ONLY, AS SCHEDULED (user decision 2026-09-29;
// docs/SCHEDULED-SEND.md §6 "Sent"). Enter / click on an outbox row opens THIS
// in the reader's slot, where a sent row's thread would open: what it says,
// who it goes to, what it carries and when it goes. Looking at it changes
// nothing. Esc closes it and the mail STAYS scheduled; only `edit` pulls it
// back into the composer (`AppStore.pullBackScheduled`), and `send now` and
// `cancel` are the row's own two verbs. Opening a row never moves it to
// drafts, and never opens a composer.
//
// In the reader's visual language: its backdrop, its top bar metrics and
// `esc back` chip, its column. The header block is the composer's labelled
// lines, because this IS a composition rather than a message. The body is the
// daemon's own send-path render (`/client/markdown/preview`, the call the
// signature preview makes), so what shows here is byte for byte the HTML half
// the send will carry, in the same light-pinned frame every mail renders in.
//
// Every word it prints is `ScheduledPreviewCopy` (Lib/ScheduledSend.swift),
// kept pure so test.sh holds the status line, the header and the key map.

import SwiftUI

struct ScheduledPreview: View {
    let row: ScheduledSendView

    @Environment(AppStore.self) private var store

    /// The daemon's render of `row.body`. nil until it lands, and for good if
    /// it fails: the raw markdown stays on screen then, which is the mail's
    /// words all the same.
    @State private var html: String?
    /// The render came back with an error (not merely "not yet"): the words
    /// as typed stay on screen, and a line under them says why they look plain.
    @State private var renderFailed = false

    /// The words the preview shows: inline pictures as `[name]` placeholders,
    /// since there are no bytes here to draw (`placeholdingInlineImages`).
    private var source: String {
        ScheduledPreviewCopy.placeholdingInlineImages(row.body, attachments: row.attachments)
    }

    private var actionable: Bool { ScheduledPreviewCopy.actionable(row) }

    var body: some View {
        ZStack {
            ReaderBackdrop()
                .ignoresSafeArea()
            VStack(spacing: 0) {
                #if os(macOS)
                    topBar
                #endif
                ScrollView {
                    content
                        .padding(.horizontal, 22)
                        .padding(.vertical, 18)
                        .frame(maxWidth: ThreadViewer.columnWidth)
                        .frame(maxWidth: .infinity)
                }
                #if os(macOS)
                    if actionable { verbBar }
                #endif
            }
        }
        // The reader's layer, the reader's way: the list's j/k/e/d underneath
        // must not fire while this covers the page.
        .keyContext(.thread)
        .keyBindings(.thread, bindings)
        #if !os(macOS)
            .safeAreaInset(edge: .bottom, spacing: 0) {
                if actionable { phoneBar }
            }
        #endif
        // Keyed on the words: a poll that only moved the state (a retry, a
        // failure) must not re-render the body.
        .task(id: row.body) {
            html = nil
            renderFailed = false
            let source = source
            guard !source.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return }
            do {
                let rendered = try await APIClient.shared.markdownPreview(source)
                if !Task.isCancelled { html = rendered }
            } catch {
                if !Task.isCancelled { renderFailed = true }
            }
        }
    }

    // MARK: - the top bar

    #if os(macOS)
        /// The reader's bar: same height, same rule, `esc back` in the far
        /// corner. The status sits beside it, where the reader keeps its verbs.
        private var topBar: some View {
            HStack(alignment: .firstTextBaseline, spacing: 12) {
                HStack(spacing: 5) {
                    Image(systemName: "clock")
                        .font(.system(size: 10, weight: .semibold))
                    Text("scheduled mail").font(Typo.micro)
                }
                .foregroundStyle(Palette.inkFaint)
                Spacer(minLength: 12)
                status
                Button { store.closeScheduledPreview() } label: {
                    HStack(spacing: 4) {
                        Kbd("esc")
                        Text("back").font(Typo.micro)
                    }
                }
                .buttonStyle(.textAction)
                .help("close; it stays scheduled")
            }
            .padding(.horizontal, 24)
            .frame(height: TopBar.height)
            .overlay(alignment: .bottom) { Hairline() }
        }
    #endif

    /// A clock, not a stamp: "in 3h" counts down while the preview sits open,
    /// exactly as the row's cell does.
    private var status: some View {
        TimelineView(.periodic(from: .now, by: 30)) { context in
            let s = ScheduledPreviewCopy.status(row, now: context.date)
            Text(s.text)
                .font(s.tone == .quiet ? Typo.num(11) : Typo.micro)
                .foregroundStyle(color(s.tone))
                .lineLimit(1)
                .truncationMode(.tail)
        }
    }

    // MARK: - the mail

    private var content: some View {
        VStack(alignment: .leading, spacing: 16) {
            #if !os(macOS)
                // The phone has no top bar of ours (the navigation bar holds
                // the way back), so the status leads the page instead.
                status
            #endif
            header
            mail
            files
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    /// The composer's labelled lines, ruled apart, on the reader's ground.
    private var header: some View {
        let lines = ScheduledPreviewCopy.header(
            row, groupName: store.scheduledGroupNames[row.id])
        return VStack(alignment: .leading, spacing: 0) {
            ForEach(Array(lines.enumerated()), id: \.offset) { i, line in
                if i > 0 { Hairline() }
                InlineFieldRow(label: line.label) {
                    Text(line.value)
                        .textSelection(.enabled)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
        }
        .overlay(alignment: .bottom) { Hairline() }
    }

    /// The daemon's render in the reader's frame, or, until it lands (and if
    /// it never does), the words as typed.
    @ViewBuilder private var mail: some View {
        if let html, !html.isEmpty {
            EmailWebView(html: html)
        } else if row.body.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            Text("no message")
                .font(Typo.micro)
                .foregroundStyle(Palette.inkFaintest)
        } else {
            VStack(alignment: .leading, spacing: 8) {
                PlainBody(content: source)
                if renderFailed {
                    Text(ScheduledPreviewCopy.renderFailed)
                        .font(Typo.micro)
                        .foregroundStyle(Palette.inkFaintest)
                }
            }
        }
    }

    /// The files it carries, as metadata: name, size, kind. Nothing to open or
    /// save, since the bytes are staged on the daemon for the send and are not
    /// a message part the reader's strip could fetch.
    @ViewBuilder private var files: some View {
        if !row.attachments.isEmpty {
            FlowLayout(spacing: 6) {
                ForEach(row.attachments) { file in
                    ScheduledFileChip(file: file)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    // MARK: - verbs

    #if os(macOS)
        /// The three verbs, keys printed beside them as the composer's footer
        /// prints its own.
        private var verbBar: some View {
            HStack(spacing: 8) {
                Spacer()
                ForEach(ScheduledPreviewCopy.verbs(row), id: \.key) { key in
                    Button { run(key.verb) } label: { keyed(key.verb.label, key.hint) }
                        .buttonStyle(.glass)
                        .tint(key.verb == .cancel ? Palette.danger : Palette.accent)
                }
            }
            .padding(.horizontal, 18)
            .padding(.vertical, 12)
            .overlay(alignment: .top) { Hairline() }
        }

        private func keyed(_ verb: String, _ key: String) -> some View {
            HStack(spacing: 6) {
                Text(verb)
                Text(key)
                    .font(Typo.mono(10, weight: .medium))
                    .opacity(0.6)
            }
        }
    #else
        /// The phone's action bar: the same three verbs, where a thumb is.
        private var phoneBar: some View {
            HStack(spacing: 10) {
                ForEach(ScheduledPreviewCopy.verbs(row), id: \.key) { key in
                    Button {
                        Haptics.commit()
                        run(key.verb)
                    } label: {
                        Text(key.verb.label).frame(maxWidth: .infinity)
                    }
                    .buttonStyle(.glass)
                    .tint(key.verb == .cancel ? Palette.danger : Palette.accent)
                }
            }
            .padding(.horizontal, 18)
            .padding(.vertical, 10)
            .background(Palette.readerBackground)
            .overlay(alignment: .top) { Hairline() }
        }
    #endif

    /// Every verb the store would take closes the preview FIRST: `edit` hands
    /// the mail to a composer (a reply's opens its thread in this very slot),
    /// `cancel` takes the row off the page behind its undo toast, and `send
    /// now` hands it to the worker. None of them leaves anything here to look
    /// at. One the store would REFUSE (a `sending` row, or one whose last verb
    /// is still on the wire) is asked about first and says so, and the
    /// preview stays open: closing it for a verb that then did nothing read
    /// as the verb having worked.
    private func run(_ verb: ScheduledPreviewCopy.Verb) {
        let row = row
        if verb != .back, store.refusesScheduledVerb(row) { return }
        switch verb {
        case .back:
            store.closeScheduledPreview()
        case .edit:
            store.closeScheduledPreview()
            store.pullBackScheduled(row)
        case .sendNow:
            store.closeScheduledPreview()
            store.sendScheduledNow(row)
        case .cancel:
            store.closeScheduledPreview()
            store.cancelScheduledRow(row)
        }
    }

    /// The key map, straight off `ScheduledPreviewCopy.keys`. EXACT CASE on the
    /// letters, as the Sent page binds them: a shifted S or X is not the verb.
    private var bindings: [KeyBinding] {
        ScheduledPreviewCopy.keys(row).map { key in
            let exact = key.key.count == 1
            return KeyBinding(declining: key.key, key.verb.label, exactCase: exact) {
                run(key.verb)
                return true
            }
        }
    }

    private func color(_ tone: ScheduledRowCopy.Tone) -> Color {
        switch tone {
        case .quiet: Palette.inkFaint
        case .busy: Palette.accent
        case .danger: Palette.danger
        }
    }
}

/// One staged file, read-only: the tray's chrome (a glyph tile, the name, the
/// size) without its remove button or its inline switch.
private struct ScheduledFileChip: View {
    let file: OutboundAttachment

    private var glyph: String {
        if AttachmentKinds.isRenderableImage(file.mime) { return "photo" }
        return AttachmentKinds.isPDF(file.mime) ? "doc.richtext" : "doc"
    }

    var body: some View {
        HStack(spacing: 9) {
            Image(systemName: glyph)
                .font(.system(size: 16, weight: .light))
                .foregroundStyle(Palette.inkFaintest)
                .frame(width: 32, height: 32)
                .background(
                    RoundedRectangle(cornerRadius: 7, style: .continuous)
                        .fill(Palette.hairline.opacity(0.6)))
            VStack(alignment: .leading, spacing: 1) {
                Text(file.filename)
                    .font(.system(size: 11, weight: .medium))
                    .foregroundStyle(Palette.ink)
                    .lineLimit(1)
                    .truncationMode(.middle)
                Text(Fmt.humanSize(file.size))
                    .font(Typo.micro)
                    .foregroundStyle(Palette.inkFaintest)
            }
            .frame(maxWidth: 170, alignment: .leading)
        }
        .padding(6)
        .padding(.trailing, 4)
        .background(
            RoundedRectangle(cornerRadius: 11, style: .continuous)
                .fill(Palette.hairline.opacity(0.4)))
        .accessibilityElement(children: .combine)
    }
}
