// Downsample-at-decode + PNG-encode, shared by every thumbnail cache here.
// Free of actor isolation on purpose: both callers run this inside a detached
// task, and `CGImage` is not Sendable — it must never cross back out.

import CoreGraphics
import Foundation
import ImageIO
import UniformTypeIdentifiers

enum Raster {
    /// ImageIO decodes DIRECTLY to thumbnail size, so a 1200px hero (or a 2MB
    /// photo) is never fully rasterized to fill a small square.
    static func thumbnail(_ data: Data, maxPixel: Int) -> CGImage? {
        guard let source = CGImageSourceCreateWithData(data as CFData, nil) else { return nil }
        let options: [CFString: Any] = [
            kCGImageSourceCreateThumbnailFromImageAlways: true,
            kCGImageSourceCreateThumbnailWithTransform: true,
            kCGImageSourceThumbnailMaxPixelSize: maxPixel,
        ]
        return CGImageSourceCreateThumbnailAtIndex(source, 0, options as CFDictionary)
    }

    /// An already-decoded image brought down to `maxPixel` on its long side —
    /// for a CROP, which ImageIO cannot thumbnail because there are no encoded
    /// bytes behind it. Never scales up: a small mark is handed back as it is
    /// and the view that draws it does the enlarging.
    static func fitted(_ image: CGImage, maxPixel: Int) -> CGImage? {
        let long = max(image.width, image.height)
        guard long > maxPixel else { return image }
        let scale = Double(maxPixel) / Double(long)
        let w = max(1, Int((Double(image.width) * scale).rounded()))
        let h = max(1, Int((Double(image.height) * scale).rounded()))
        guard let space = CGColorSpace(name: CGColorSpace.sRGB),
            let ctx = CGContext(
                data: nil, width: w, height: h, bitsPerComponent: 8, bytesPerRow: 0,
                space: space, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)
        else { return nil }
        ctx.interpolationQuality = .high
        ctx.draw(image, in: CGRect(x: 0, y: 0, width: w, height: h))
        return ctx.makeImage()
    }

    /// PNG on the way out because `CGImage` is not Sendable and `Data` is — that
    /// re-encode is what lets the render stay off the main actor. PNG rather than
    /// JPEG because a transparent logo needs its alpha to survive.
    static func png(_ image: CGImage) -> Data? {
        let out = NSMutableData()
        guard
            let dest = CGImageDestinationCreateWithData(
                out as CFMutableData, UTType.png.identifier as CFString, 1, nil)
        else { return nil }
        CGImageDestinationAddImage(dest, image, nil)
        guard CGImageDestinationFinalize(dest) else { return nil }
        return out as Data
    }
}
