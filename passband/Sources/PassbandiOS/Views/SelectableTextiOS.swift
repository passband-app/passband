// SELECTABLE PLAIN TEXT ON A PHONE (#522). The reader draws a plain-text body
// as native text, and on the Mac `.textSelection(.enabled)` on a SwiftUI `Text`
// is real selection. On iOS it is not: that modifier gives a long press that
// copies the WHOLE block and nothing else, with no handles and no way to take
// one sentence. HTML mail never had the problem because WebKit selects on its
// own. So the phone draws plain text with the one thing UIKit selects
// properly, a read-only UITextView, and the Mac keeps its `Text`.
//
// It never scrolls (the thread column is the one scroll surface, the same rule
// as the mail frames) and it is as tall as its text at the width it is given.
// No data detectors: a tapped link would leave through UIKit rather than
// through `Opener`, and a phone number turning blue is a separate decision.

import SwiftUI
import UIKit

struct SelectableText: UIViewRepresentable {
    let text: String
    /// Find matches to paint, in `text`'s own indices.
    var hits: [Range<String.Index>] = []
    /// Which of `hits` the cursor is on.
    var active: Int?
    /// Point size, already scaled for the density.
    let size: CGFloat
    let color: Color
    /// Take the whole width offered (a card) or only what the text needs (a
    /// bubble). Same switch as PlainBody's `fills`.
    var fills = true

    func makeUIView(context: Context) -> UITextView {
        let view = UITextView()
        view.isEditable = false
        view.isSelectable = true
        view.isScrollEnabled = false
        view.backgroundColor = .clear
        view.tintColor = PlatformColor(Palette.accent)
        view.dataDetectorTypes = []
        // No inset and no line padding, so the text sits exactly where a
        // SwiftUI `Text` would and a bubble's padding means the same thing.
        view.textContainerInset = .zero
        view.textContainer.lineFragmentPadding = 0
        view.adjustsFontForContentSizeCategory = false
        view.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        view.setContentHuggingPriority(.defaultLow, for: .horizontal)
        apply(to: view, context: context)
        return view
    }

    func updateUIView(_ view: UITextView, context: Context) {
        apply(to: view, context: context)
    }

    /// Re-set the text only when something drawn changed. Assigning
    /// `attributedText` drops the selection, and SwiftUI re-renders the reader
    /// for plenty of reasons that are not this body.
    private func apply(to view: UITextView, context: Context) {
        let key = Key(text: text, hits: hits, active: active, size: size, color: color)
        guard context.coordinator.drawn != key else { return }
        context.coordinator.drawn = key
        view.attributedText = attributed()
        view.invalidateIntrinsicContentSize()
    }

    private func attributed() -> NSAttributedString {
        let out = NSMutableAttributedString(
            string: text,
            attributes: [
                .font: UIFont.systemFont(ofSize: size),
                .foregroundColor: PlatformColor(color),
            ])
        for (i, hit) in hits.enumerated() {
            let range = NSRange(hit, in: text)
            guard range.location != NSNotFound, NSMaxRange(range) <= out.length else { continue }
            out.addAttributes(
                [
                    .backgroundColor: PlatformColor(i == active ? FindPaint.active : FindPaint.match),
                    .foregroundColor: PlatformColor(FindPaint.ink),
                ], range: range)
        }
        return out
    }

    /// THE HEIGHT IS THE TEXT'S, at the width offered. A UITextView that does
    /// not scroll reports an intrinsic size of one unwrapped line, which
    /// SwiftUI would believe. A bubble also narrows to its longest line.
    func sizeThatFits(_ proposal: ProposedViewSize, uiView: UITextView, context: Context)
        -> CGSize?
    {
        let offered = proposal.width.flatMap { $0.isFinite && $0 > 0 ? $0 : nil }
        let limit = offered ?? .greatestFiniteMagnitude
        let natural = uiView.attributedText.boundingRect(
            with: CGSize(width: limit, height: .greatestFiniteMagnitude),
            options: [.usesLineFragmentOrigin, .usesFontLeading], context: nil)
        let width = fills ? (offered ?? ceil(natural.width)) : min(limit, ceil(natural.width))
        let fitted = uiView.sizeThatFits(CGSize(width: width, height: .greatestFiniteMagnitude))
        return CGSize(width: width, height: ceil(fitted.height))
    }

    func makeCoordinator() -> Coordinator { Coordinator() }

    final class Coordinator {
        var drawn: Key?
    }

    struct Key: Equatable {
        let text: String
        let hits: [Range<String.Index>]
        let active: Int?
        let size: CGFloat
        let color: Color
    }
}
