// THE END OF THE CHAIN, and answering from a card in it. Phone only.
//
// A reader who has got to the bottom of a conversation has read enough to
// answer it, so that is where the answer starts: two buttons under the last
// message, reply and reply all. Press one and the composer takes their place,
// in the chain's own scroll rather than pinned over it (see InlineReply for why
// it stopped being a bar). Dismiss it and the buttons are back.
//
// The other way in is a CARD. Dragging one message of the chain to the left
// pulls out a rail of three verbs about THAT message: reply, reply all,
// forward. The Mac says the same three with `r`, Enter and `f`; the rail is
// those keys for a thumb, and every one of them ends in the store call the key
// makes. Nothing here decides anything.
//
// The foot lives OUTSIDE the lazy stack the messages are in, on purpose. A lazy
// stack lets go of what scrolls far enough away, and a composer that was let go
// while its reader scrolled up to check a date would come back as a new text
// view: the keyboard down, then up again the moment it re-mounted, the
// recipient fields shut. The words would survive (they are in the store); the
// place would not.

import SwiftUI
import UIKit

struct ChainFoot: View {
    /// The thread's messages and subject, handed straight to the composer.
    let messages: [ClientMessage]
    let threadSubject: String
    let proxy: ScrollViewProxy
    /// `r` and Enter, as the reader itself defines them: the newest message.
    let onReply: () -> Void
    let onReplyAll: () -> Void

    @Environment(AppStore.self) private var store

    /// What the scroll is asked to bring into view. One id for the buttons and
    /// the composer alike: they are the same place.
    static let anchor = "chain-foot"

    /// Whether the composer is showing, asked the way the composer asks it: a
    /// draft whose parent is in THIS thread. Read here and not in the reader,
    /// for the reason the composer is mounted unconditionally: the draft
    /// changes on every keystroke, and only this foot should hear about it.
    private var replying: Bool {
        guard let parent = store.inlineReply?.replyToMessageId else { return false }
        return messages.contains { $0.id == parent }
    }

    var body: some View {
        VStack(spacing: 0) {
            // Mounted either way; it draws nothing without a draft.
            InlineReply(messages: messages, threadSubject: threadSubject)
            if !replying { doors }
        }
        .padding(.top, Space.s12)
        .padding(.bottom, Space.s16)
        .id(Self.anchor)
        // A REPLY OPENED, from anywhere: these buttons, a card's rail, a list
        // row's Reply that arrived with the thread. The composer is the last
        // thing in the chain and the reader may be anywhere in it.
        .onChange(of: store.inlineReply?.id) { _, now in
            if now != nil, replying { Self.reveal(proxy) }
        }
        .onAppear { if replying { Self.reveal(proxy) } }
        // AND AGAIN ONCE THE KEYBOARD IS UP. It takes half the window a beat
        // after the composer opens, from the bottom, which is exactly where
        // the composer was just put.
        //
        // Only for a keyboard the BODY raised. With the recipient fields open
        // the card can be taller than what the keyboard leaves, and parking
        // its foot at the bottom would push the To field somebody just tapped
        // off the top of the window.
        .onReceive(
            NotificationCenter.default.publisher(for: UIResponder.keyboardDidShowNotification)
        ) { _ in
            if replying, MarkdownFocus.shared.isEditing { Self.reveal(proxy, after: .zero) }
        }
    }

    /// Bring the foot's bottom edge to the bottom of what is left of the
    /// window, so the send button rests just above the keyboard. A beat first:
    /// the composer is laid out by the same update that reports it open, and a
    /// scroll aimed before that lands on the two buttons it replaced.
    static func reveal(_ proxy: ScrollViewProxy, after delay: Duration = .milliseconds(80)) {
        Task { @MainActor in
            try? await Task.sleep(for: delay)
            withAnimation(Motion.disclose) { proxy.scrollTo(anchor, anchor: .bottom) }
        }
    }

    /// Two doors of equal weight, because which one is right depends on the
    /// mail and not on us. Lowercase, like every other verb in this reader.
    private var doors: some View {
        HStack(spacing: Space.s10) {
            door("reply", symbol: "arrowshape.turn.up.left", action: onReply)
            door("reply all", symbol: "arrowshape.turn.up.left.2", action: onReplyAll)
        }
    }

    private func door(_ title: String, symbol: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            HStack(spacing: Space.s8) {
                Image(systemName: symbol)
                    .font(Typo.ui(12, weight: .semibold))
                    .foregroundStyle(Palette.accent)
                Text(title)
                    .font(Typo.ui(13, weight: .medium))
                    .foregroundStyle(Palette.ink)
            }
            .frame(maxWidth: .infinity)
            .frame(height: 44)
            .contentShape(Capsule())
        }
        .buttonStyle(.plain)
        .glassCapsule(tint: Palette.glassTint)
    }
}

// MARK: - the card's rail

/// One message of the chain, wearing the dashboard's swipe rail on its trailing
/// edge. A modifier rather than a wrapper at the call site so the reader's
/// `ForEach` stays one expression on both platforms and the Mac's simply does
/// not have this line.
struct CardRail: ViewModifier {
    let verbs: [SwipeVerb]
    /// Mount with the rail already out. Only a debug build ever honours it.
    var staged = false

    func body(content: Content) -> some View {
        // Square, because a card is not a tile: it runs the width of the
        // column and is divided from its neighbours by a hairline, and a
        // rounded clip would bite the ends off that line. The leading edge is
        // yielded outright: this is a pushed screen, and a drag to the right
        // is the way back.
        #if DEBUG
            SwipeRow(
                trailing: verbs, cornerRadius: 0, buttonWidth: Self.buttonWidth,
                ground: Palette.readerBackground, followsTouch: true, floating: true,
                yieldsEmptyEdges: true,
                stagedOpen: staged
            ) { content }
        #else
            SwipeRow(
                trailing: verbs, cornerRadius: 0, buttonWidth: Self.buttonWidth,
                ground: Palette.readerBackground, followsTouch: true, floating: true,
                yieldsEmptyEdges: true
            ) { content }
        #endif
    }

    /// Narrower than the dashboard's 74: three of those is more than half a
    /// phone, and the mail being answered should still be most of what is on
    /// screen with its rail out.
    private static var buttonWidth: CGFloat { 68 }
}

// MARK: - the chain's end

/// The lazy stack of messages, then the foot, then the room that lets the
/// newest message rest at the top of the window (ThreadViewer.tailSpace). A
/// plain stack around a lazy one: the messages stay lazy, the foot stays
/// mounted.
struct ChainEnd<Foot: View>: ViewModifier {
    let tail: CGFloat
    @ViewBuilder var foot: Foot

    func body(content: Content) -> some View {
        VStack(spacing: 0) {
            content
            foot
            Color.clear.frame(height: tail)
        }
    }
}
