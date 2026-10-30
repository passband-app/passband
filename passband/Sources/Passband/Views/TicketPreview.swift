// WHERE A TICKET OPENS: mounted once at the root of each shell, drawing
// nothing, opening whatever `AppStore.ticketPreview` names (the card's Tickets
// button, a reminder's tap or its "Show tickets"). See
// Model/CalendarReminders.swift for why the store only names the file.
//
// THE SAME VIEWERS AS AN ATTACHMENT IN THE READER. On the Mac that is the
// system's Quick Look panel, through the same host view the attachment strip
// plants (Views/QuickLookPanel.swift); on the phone, Quick Look in a sheet
// (Views/AttachmentPreviewiOS.swift), and for a wallet pass the system's Add
// to Wallet sheet, which falls back to Quick Look when the pass will not read.

import SwiftUI

#if os(iOS)
    import PassKit
#endif

struct TicketPreviewHost: View {
    @Environment(AppStore.self) private var store

    #if os(macOS)
        @State private var launcher = QuickLookLauncher()
        /// What the host view holds. Not cleared when the request is: the
        /// panel keeps reading its items while it is up, and an empty list
        /// closes it.
        @State private var shown: [Attachment] = []
    #else
        @State private var staged: StagedAttachment?
        @State private var pass: WalletPass?
    #endif

    var body: some View {
        Color.clear
            .frame(width: 0, height: 0)
            .allowsHitTesting(false)
            .accessibilityHidden(true)
            #if os(macOS)
                .background {
                    QuickLookHost(
                        attachments: shown, sourceFrames: [:], launcher: launcher,
                        onError: { store.pushToast($0, .error) })
                }
            #else
                .sheet(item: $staged) { file in
                    QuickLookPreview(url: file.url).ignoresSafeArea()
                }
                .background {
                    Color.clear.sheet(item: $pass) { wallet in
                        AddPassSheet(controller: wallet.controller) { pass = nil }
                            .ignoresSafeArea()
                    }
                }
            #endif
            .onChange(of: store.ticketPreview?.id, initial: true) { _, _ in
                guard let request = store.ticketPreview else { return }
                store.ticketPreview = nil
                open(request)
            }
    }

    #if os(macOS)
        /// The host view takes the list on its next update, which lands after
        /// this change; the open waits for it rather than racing it.
        private func open(_ request: TicketPreviewRequest) {
            shown = [request.attachment]
            let attachment = request.attachment
            Task { @MainActor in
                for _ in 0..<20 {
                    if launcher.holds(attachment) {
                        launcher.open(attachment)
                        return
                    }
                    try? await Task.sleep(for: .milliseconds(25))
                }
            }
        }
    #else
        private func open(_ request: TicketPreviewRequest) {
            let opened = store.epoch
            Task {
                do {
                    let file = try await AttachmentFiles.shared.file(for: request.attachment)
                    guard store.isCurrent(opened) else { return }
                    if request.isWalletPass, PKAddPassesViewController.canAddPasses(),
                        let data = try? Data(contentsOf: file.url),
                        let parsed = try? PKPass(data: data),
                        let controller = PKAddPassesViewController(pass: parsed)
                    {
                        pass = WalletPass(controller: controller)
                    } else {
                        staged = file
                    }
                } catch is CancellationError {
                    // The account changed under the fetch. Nothing to say.
                } catch {
                    guard store.isCurrent(opened) else { return }
                    store.pushToast(errText(error, "Could not open \(request.attachment.filename)"), .error)
                }
            }
        }
    #endif
}

#if os(iOS)
    /// The system's Add to Wallet sheet for one pass.
    struct WalletPass: Identifiable {
        let id = UUID()
        let controller: PKAddPassesViewController
    }

    /// `PKAddPassesViewController` in a SwiftUI sheet. It asks its delegate
    /// to be dismissed, whether the pass was added or not.
    private struct AddPassSheet: UIViewControllerRepresentable {
        let controller: PKAddPassesViewController
        let onFinish: () -> Void

        func makeCoordinator() -> Coordinator { Coordinator(onFinish: onFinish) }

        func makeUIViewController(context: Context) -> PKAddPassesViewController {
            controller.delegate = context.coordinator
            return controller
        }

        func updateUIViewController(_ controller: PKAddPassesViewController, context: Context) {}

        final class Coordinator: NSObject, PKAddPassesViewControllerDelegate {
            let onFinish: () -> Void
            init(onFinish: @escaping () -> Void) { self.onFinish = onFinish }

            func addPassesViewControllerDidFinish(_ controller: PKAddPassesViewController) {
                onFinish()
            }
        }
    }
#endif
