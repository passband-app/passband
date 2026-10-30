// The sender's face on a pushed banner, the way Messages, Gmail and Superhuman
// show it: a large round avatar with Passband's icon as the small badge.
//
// iOS draws that layout only for a COMMUNICATION notification: the content is
// re-derived from an `INSendMessageIntent` whose sender is an `INPerson` with
// an image. The app target carries the matching entitlement
// (`com.apple.developer.usernotifications.communication`) and lists the intent
// in NSUserActivityTypes; without both, `updating(from:)` throws and the banner
// goes out exactly as before, with the app icon.
//
// THE PRIVACY RULE IS THE APP'S (see SenderIdentity and the Mac's
// NotificationIcon): a service sender's tile is its domain logo, fetched from
// the icon service; a human correspondent's is initials drawn here, and their
// domain never leaves the phone. `eligibleFaviconDomain` answers nil for a
// person, and that nil is the whole rule.
//
// EVERYTHING FAILS OPEN to the plain banner. No logo in time: initials. No
// image at all: the intent still names the sender. The intent refused: the
// content the caller already built.

import Foundation
import Intents
import UIKit
import UserNotifications

enum SenderAvatar {
    /// Tile edge in pixels. iOS shows the avatar at about 40pt on a lock-screen
    /// banner, so 3x of that with room for the expanded view.
    private static let edge: CGFloat = 180

    /// A short leash for the logo: the banner is already late by one daemon
    /// round trip, and a logo is a nicety. Its own session so the ceiling is
    /// unambiguous, as the badge count's is in NotificationService.
    private nonisolated(unsafe) static let session = Sessions.ephemeral(
        timeout: 1, resource: 1.5, emptyHeaders: true)

    /// `content` re-derived as a communication notification from `sender`, or
    /// `content` unchanged when iOS refuses. `name` is the readable title the
    /// banner already chose, so the bold line does not change.
    static func communication(
        _ content: UNNotificationContent, sender: String, name: String
    ) async -> UNNotificationContent {
        let address = SenderID.address(sender)
        guard !address.isEmpty else { return content }
        let image = await tile(for: sender).map { INImage(imageData: $0) }
        let person = INPerson(
            personHandle: INPersonHandle(value: address, type: .emailAddress),
            nameComponents: nil,
            displayName: name,
            image: image,
            contactIdentifier: nil,
            customIdentifier: address)
        let intent = INSendMessageIntent(
            recipients: nil,
            outgoingMessageType: .outgoingMessageText,
            content: content.body,
            speakableGroupName: nil,
            conversationIdentifier: content.threadIdentifier,
            serviceName: nil,
            sender: person,
            attachments: nil)
        intent.setImage(image, forParameterNamed: \.sender)
        // NOT donated: the banner needs only `updating(from:)`, and a donation
        // would hand every sender, people included, to Siri suggestions.
        return (try? content.updating(from: intent)) ?? content
    }

    /// PNG bytes for the sender's tile: the brand's logo on white when the
    /// sender is a service and the logo arrives in time, otherwise initials on
    /// the sender's palette colour (the light pair, as the Mac's tile is: the
    /// image is baked once and outlives the appearance it was posted under).
    static func tile(for sender: String) async -> Data? {
        let resolved = SenderID.resolve(sender)
        if let domain = resolved.faviconDomain, let logo = await logo(domain) {
            return render { rect in
                UIColor.white.setFill()
                UIRectFill(rect)
                logo.draw(in: rect.insetBy(dx: edge * 0.2, dy: edge * 0.2))
            }
        }
        let pair = avatarHex[resolved.slot % avatarHex.count]
        let initials = resolved.initials
        return render { rect in
            color(pair.bg.light).setFill()
            UIRectFill(rect)
            let font = UIFont.systemFont(ofSize: edge * 0.42, weight: .semibold)
            let attrs: [NSAttributedString.Key: Any] = [
                .font: font, .foregroundColor: color(pair.fg.light),
            ]
            let text = NSAttributedString(string: initials, attributes: attrs)
            let size = text.size()
            text.draw(at: CGPoint(x: (edge - size.width) / 2, y: (edge - size.height) / 2))
        }
    }

    /// Square, opaque, sRGB. iOS crops the avatar to a circle itself, so a
    /// full-bleed square is what it wants.
    private static func render(_ draw: (CGRect) -> Void) -> Data? {
        let format = UIGraphicsImageRendererFormat()
        format.scale = 1
        format.opaque = true
        let rect = CGRect(x: 0, y: 0, width: edge, height: edge)
        return UIGraphicsImageRenderer(bounds: rect, format: format).pngData { _ in draw(rect) }
    }

    /// The domain's logo, the same request the app's FaviconLoader makes, with
    /// the same filter for the service's blank fallback.
    private static func logo(_ domain: String) async -> UIImage? {
        guard let url = SenderID.faviconURL(domain) else { return nil }
        var request = URLRequest(url: url)
        request.setValue("", forHTTPHeaderField: "Referer")
        guard let (data, response) = try? await session.data(for: request),
            (response as? HTTPURLResponse)?.statusCode == 200,
            let image = UIImage(data: data),
            image.size.width > 1, image.size.height > 1
        else { return nil }
        return image
    }

    private static func color(_ hex: UInt32) -> UIColor {
        UIColor(
            red: CGFloat((hex >> 16) & 0xFF) / 255,
            green: CGFloat((hex >> 8) & 0xFF) / 255,
            blue: CGFloat(hex & 0xFF) / 255,
            alpha: 1)
    }
}
