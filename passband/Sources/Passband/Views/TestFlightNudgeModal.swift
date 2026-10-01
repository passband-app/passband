// THE IPHONE ASK. Two days into the Mac app, once, for somebody whose mailbox
// has never seen the phone app: a small card with the TestFlight link.
//
// A QR CODE, NOT JUST A LINK. The link is for a phone, and opening it on the
// Mac lands on a page that tells you to go open it on your phone. Pointing the
// phone's camera at the screen is the shortest way there; "Copy link" is the
// fallback for somebody who would rather AirDrop or message it to themselves.
//
// Same modal rules as `ShareNudgeModal`, including the scarred one: NO
// `.defaultAction`, because an unbidden card does not get to own Return.

import AppKit
import CoreImage.CIFilterBuiltins
import SwiftUI

struct TestFlightNudgeModal: View {
    private var nudge: TestFlightNudge { TestFlightNudge.shared }
    @State private var copied = false

    var body: some View {
        if nudge.showingNudge {
            ZStack {
                Rectangle()
                    .fill(.black.opacity(0.28))
                    .ignoresSafeArea()
                    .onTapGesture { dismiss() }

                card
            }
            .transition(.opacity)
            .animation(Motion.toast, value: nudge.showingNudge)
        }
    }

    private var card: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack(alignment: .top, spacing: 16) {
                VStack(alignment: .leading, spacing: 8) {
                    Text("Passband is on iPhone too")
                        .font(.system(size: 14, weight: .semibold))
                        .foregroundStyle(Palette.ink)
                        .fixedSize(horizontal: false, vertical: true)

                    Text(
                        "The same triaged inbox, with notifications only for the mail that "
                            + "matters. It's in beta on TestFlight. Scan the code with your "
                            + "phone's camera to join."
                    )
                    .font(Typo.rowSub)
                    .foregroundStyle(Palette.inkDim)
                    .fixedSize(horizontal: false, vertical: true)
                }

                if let qr = Self.qrCode {
                    Image(nsImage: qr)
                        .interpolation(.none)
                        .resizable()
                        .frame(width: 108, height: 108)
                        .padding(6)
                        // Pinned white in both themes: a phone camera reads
                        // dark-on-light, and an inverted code is a coin flip.
                        .background(.white, in: RoundedRectangle(cornerRadius: 8))
                        .accessibilityLabel("QR code for the TestFlight invite")
                }
            }

            HStack(spacing: 10) {
                Spacer()
                Button("Not now") { dismiss() }
                    .buttonStyle(.glass)
                    .font(.system(size: 12, weight: .medium))
                Button(copied ? "Copied" : "Copy link") { copy() }
                    .buttonStyle(.glassProminent)
                    .font(.system(size: 12, weight: .semibold))
            }
        }
        .padding(18)
        .frame(maxWidth: 420)
        .passbandGlass(.pane, cornerRadius: 14, tint: Palette.glassTintStrong)
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Get Passband for iPhone")
    }

    private func dismiss() {
        nudge.showingNudge = false
        Analytics.capture("ios_nudge_closed", ["copied": copied])
    }

    /// Copy, say so, and leave the card up: the person may still want the code,
    /// and "Not now" or the scrim closes it.
    private func copy() {
        let board = NSPasteboard.general
        board.clearContents()
        board.setString(TestFlightNudge.joinURL.absoluteString, forType: .string)
        copied = true
    }

    /// Rendered once per process. Nil only if CoreImage cannot make a QR code
    /// of a 40-character URL, in which case the card is copy-only.
    private static let qrCode: NSImage? = {
        let filter = CIFilter.qrCodeGenerator()
        filter.message = Data(TestFlightNudge.joinURL.absoluteString.utf8)
        filter.correctionLevel = "M"
        guard let output = filter.outputImage,
            let cg = CIContext().createCGImage(output, from: output.extent)
        else { return nil }
        return NSImage(cgImage: cg, size: output.extent.size)
    }()
}
