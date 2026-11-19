// Renders the waitlist email's feature icons into passband-site/email-*.png:
// the same SF Symbols the app draws, tinted the email's accent blue on a
// transparent ground. No mail client renders SVG and Gmail strips data URIs,
// so these are served from passband.app like mark-light.png. 72px for a 24px
// slot, because mail reaches retina screens. Run on a Mac from passband-site/:
//
//   swift tools/email-icons.swift
import AppKit

// File name -> the SF Symbol. squelch-control/src/resend.rs names these.
let symbols: [(String, String)] = [
    ("email-sitrep", "gauge.with.dots.needle.33percent"),
    ("email-box", "shippingbox"),
    ("email-wind", "wind"),
    ("email-key", "key"),
]

// #82BAF5, the email's link and button colour.
let accent = NSColor(srgbRed: 0x82 / 255.0, green: 0xBA / 255.0, blue: 0xF5 / 255.0, alpha: 1)
let side = 72
let config = NSImage.SymbolConfiguration(pointSize: 52, weight: .regular)
    .applying(NSImage.SymbolConfiguration(paletteColors: [accent]))

for (name, symbol) in symbols {
    guard let base = NSImage(systemSymbolName: symbol, accessibilityDescription: nil),
          let img = base.withSymbolConfiguration(config) else {
        FileHandle.standardError.write("missing symbol \(symbol)\n".data(using: .utf8)!)
        exit(1)
    }
    let rep = NSBitmapImageRep(
        bitmapDataPlanes: nil, pixelsWide: side, pixelsHigh: side, bitsPerSample: 8,
        samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB,
        bytesPerRow: 0, bitsPerPixel: 0)!
    rep.size = NSSize(width: side, height: side)
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
    let s = img.size
    img.draw(in: NSRect(x: (CGFloat(side) - s.width) / 2, y: (CGFloat(side) - s.height) / 2,
                        width: s.width, height: s.height))
    NSGraphicsContext.restoreGraphicsState()
    try! rep.representation(using: .png, properties: [:])!
        .write(to: URL(fileURLWithPath: "\(name).png"))
}
