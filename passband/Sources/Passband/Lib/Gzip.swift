// gzip, by hand: Foundation's `.zlib` compression is raw DEFLATE (RFC 1951),
// and PostHog's ingestion wants a gzip member (RFC 1952) when a request says
// `Content-Encoding: gzip`. The difference is a 10-byte header and an 8-byte
// trailer (CRC-32 and length), which is cheaper to write than a dependency.
//
// Used for session replay uploads, where a minute of frames is a few
// hundred kilobytes of very repetitive JSON and shrinks about tenfold.

import Foundation

enum Gzip {
    static func compress(_ data: Data) -> Data? {
        guard let deflated = try? (data as NSData).compressed(using: .zlib) as Data else {
            return nil
        }
        var out = Data([0x1F, 0x8B, 0x08, 0, 0, 0, 0, 0, 0, 0xFF])
        out.append(deflated)
        var crc = crc32(data).littleEndian
        var size = UInt32(truncatingIfNeeded: data.count).littleEndian
        withUnsafeBytes(of: &crc) { out.append(contentsOf: $0) }
        withUnsafeBytes(of: &size) { out.append(contentsOf: $0) }
        return out
    }

    private static let table: [UInt32] = (0..<256).map { n in
        var c = UInt32(n)
        for _ in 0..<8 { c = (c & 1) != 0 ? 0xEDB8_8320 ^ (c >> 1) : c >> 1 }
        return c
    }

    static func crc32(_ data: Data) -> UInt32 {
        var c: UInt32 = 0xFFFF_FFFF
        for byte in data { c = table[Int((c ^ UInt32(byte)) & 0xFF)] ^ (c >> 8) }
        return c ^ 0xFFFF_FFFF
    }
}
