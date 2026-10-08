// Central error type for the human-door client. Every non-2xx response becomes
// an APIError; callers switch on `kind` rather than sniffing status codes.

import Foundation

enum APIErrorKind: String, Sendable {
    case unauthorized   // 401: bad/absent bearer token
    case forbidden      // 403: no write credential (run `squelchd auth --write`)
    case notFound       // 404: missing OR sealed-hidden (indistinguishable by design)
    case badRequest     // 400: validation
    case guardBlocked   // 422: outbound send guard matched; see `guardKinds`
    case server         // 5xx
    case network        // transport failure / timeout
    case unknown

    static func forStatus(_ status: Int) -> APIErrorKind {
        switch status {
        case 400: .badRequest
        case 401: .unauthorized
        case 403: .forbidden
        case 404: .notFound
        case 422: .guardBlocked
        default: status >= 500 ? .server : .unknown
        }
    }
}

struct APIError: Error, Sendable {
    var kind: APIErrorKind
    var status: Int
    var message: String
    /// Redacted guard kinds parsed from a 422 send response, if any.
    var guardKinds: [String]?
    /// A transport failure after which the server may still have received
    /// the request and acted on it: the wait timed out, the connection
    /// dropped mid-request, or an answer came back that could not be read
    /// (a bad or empty response: the server got the request). False for one
    /// that never got there (refused, no route, bad address). Only ever true
    /// with `kind == .network`.
    var mayHaveArrived = false

    init(_ kind: APIErrorKind, _ status: Int, _ message: String, guardKinds: [String]? = nil) {
        self.kind = kind
        self.status = status
        self.message = message
        self.guardKinds = guardKinds
    }

    /// A transport failure as the client reports it: never the URL or the
    /// token, and whether the request may have reached the server.
    static func transport(_ error: Error) -> APIError {
        var out = APIError(.network, 0, "cannot reach squelch server")
        let code = (error as? URLError)?.code
        out.mayHaveArrived = code.map(arrivedCodes.contains) ?? false
        return out
    }
}

extension APIError {
    /// The transport failures that may have followed the request reaching
    /// the server (`mayHaveArrived`).
    static let arrivedCodes: Set<URLError.Code> = [
        .timedOut, .networkConnectionLost, .badServerResponse, .cannotParseResponse, .zeroByteResource,
    ]
}

extension APIError: LocalizedError {
    var errorDescription: String? { message }
}

/// The 422 send-guard message is a sentence carrying the redacted kinds:
///   "outbound guard blocked send; matched (redacted) kinds: aws_key, jwt. …"
/// Parse the comma list between "kinds:" and the trailing period.
func parseGuardKinds(_ message: String) -> [String] {
    guard let range = message.range(of: "kinds:", options: .caseInsensitive) else { return [] }
    let tail = message[range.upperBound...]
    let sentence = tail.split(separator: ".", maxSplits: 1, omittingEmptySubsequences: false)[0]
    return sentence
        .split(separator: ",")
        .map { $0.trimmingCharacters(in: .whitespaces) }
        .filter { !$0.isEmpty && !$0.contains(" ") }
}

/// Human text for any thrown error, without ever echoing a token or URL.
func errText(_ error: Error, _ fallback: String) -> String {
    if let api = error as? APIError { return api.message }
    return fallback
}

/// A temporary outage is not evidence of an incompatible daemon. Keep retries
/// bounded so a switch cannot hold the account picker indefinitely.
struct CapabilityProbeRetry {
    private var failures = 0
    private static let delays: [TimeInterval] = [1, 3, 10]

    static func isTransient(_ error: Error) -> Bool {
        guard let error = error as? APIError else { return false }
        return error.kind == .network || error.kind == .server
    }

    mutating func delay(after error: Error) -> TimeInterval? {
        guard Self.isTransient(error), failures < Self.delays.count else { return nil }
        defer { failures += 1 }
        return Self.delays[failures]
    }
}
