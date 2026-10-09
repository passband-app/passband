// What an image IS, read from its first bytes, for the hosts whose
// Content-Type will not say. Loops' image CDN and any S3 bucket uploaded
// without a type serve every PNG as `application/octet-stream`; a browser never
// consults the header for an <img> and decodes the bytes, so those mails render
// everywhere except behind a fetcher that believes the header.
//
// RASTER ONLY, and that is the security rule rather than an omission: svg has
// no magic number, only markup, and a body promoted to `image/svg+xml` on the
// strength of looking like one is a document we chose to call an image.
// Anything not recognised here stays refused. See docs/SECURITY.md §3.

import Foundation

enum ImageSniff {
    /// How many leading bytes `mime(of:)` reads at most. A fetch that has this
    /// many and still gets nil can drop the connection: more bytes cannot
    /// change the answer.
    static let prefixLength = 12

    /// The image type these bytes open with, or nil. Takes any byte collection
    /// so the fetcher can ask about its buffer mid-stream without copying it.
    static func mime(of bytes: some Collection<UInt8>) -> String? {
        let head = Array(bytes.prefix(prefixLength))

        func opens(with magic: [UInt8], at offset: Int = 0) -> Bool {
            head.count >= offset + magic.count
                && head[offset..<offset + magic.count].elementsEqual(magic)
        }

        if opens(with: [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]) { return "image/png" }
        if opens(with: [0xFF, 0xD8, 0xFF]) { return "image/jpeg" }
        // "GIF87a" / "GIF89a"
        if opens(with: [0x47, 0x49, 0x46, 0x38, 0x37, 0x61])
            || opens(with: [0x47, 0x49, 0x46, 0x38, 0x39, 0x61])
        {
            return "image/gif"
        }
        // "RIFF" <size> "WEBP" — RIFF alone is also wav and avi.
        if opens(with: [0x52, 0x49, 0x46, 0x46]), opens(with: [0x57, 0x45, 0x42, 0x50], at: 8) {
            return "image/webp"
        }
        // ISO base media: <size> "ftyp" <brand>. The box is shared with mp4 and
        // mov, so the brand decides, and only the still-image ones count.
        if opens(with: [0x66, 0x74, 0x79, 0x70], at: 4), head.count >= 12 {
            switch String(decoding: head[8..<12], as: UTF8.self) {
            case "avif", "avis": return "image/avif"
            case "heic", "heix", "mif1", "msf1": return "image/heic"
            default: return nil
            }
        }
        return nil
    }
}
